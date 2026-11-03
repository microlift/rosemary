use std::collections::HashMap;

use starnix::process::{self as host, Identity};

use crate::credentials::{Change, Credentials, Denied};
use crate::protocol::{Application, Checkin, Launch, Process, Thread, Usage};

pub const LAUNCHD: i32 = 1;

/// How far up the host's parents a check-in looks for launchd. A job may run
/// a host program that runs a guest, but not a deep chain of them.
const ANCESTRY: usize = 16;

struct Record {
    identity: Identity,
    checkin: Checkin,
    credentials: Credentials,
    /// Whether the process is launchd or descends from it, settled when it
    /// checked in: a daemon that double-forks is reparented by the host, and
    /// is launchd's all the same.
    system: bool,
    /// The connection the process checked in on, while it is open.
    connection: Option<u64>,
    /// Names the guest gave its threads, which the host may have truncated.
    thread_names: HashMap<i32, String>,
    /// Which image of the pid this is; see [`Process::image`].
    image: u64,
}

/// A running application, LaunchServices' table of them.
struct Running {
    application: Application,
    identity: Identity,
    /// Whether the image that launched has checked in since. The next fresh
    /// check-in under the pid is an `exec` into something else, which is no
    /// longer the application.
    imaged: bool,
}

#[derive(Default)]
pub struct Registry {
    records: HashMap<i32, Record>,
    /// Oldest first.
    applications: Vec<Running>,
    /// The host pid of the prefix's launchd, which guests see as [`LAUNCHD`].
    launchd: Option<i32>,
    /// The last image handed out; never reused while the server runs.
    images: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the host pid of the prefix's launchd, or that it is gone.
    pub fn set_launchd(&mut self, pid: Option<i32>) {
        self.launchd = pid;
    }

    pub fn launchd(&self) -> Option<i32> {
        self.launchd
    }

    /// A host pid as guests know it.
    pub fn guest_pid(&self, host: i32) -> i32 {
        match self.launchd == Some(host) {
            true => LAUNCHD,
            false => host,
        }
    }

    /// A pid a guest named, as the host knows it. The host's own pid 1 is
    /// never a guest, so the guest's can only mean launchd.
    pub fn host_pid(&self, guest: i32) -> i32 {
        match (guest, self.launchd) {
            (LAUNCHD, Some(host)) => host,
            _ => guest,
        }
    }

    /// Whether a process checking in as `pid` is launchd or one of its
    /// descendants: an image replacing one that was, or a process whose
    /// nearest guest ancestor was.
    fn in_launchd_tree(&mut self, pid: i32) -> bool {
        let Some(launchd) = self.launchd else {
            return false;
        };
        let mut current = pid;
        for _ in 0..ANCESTRY {
            if current == launchd {
                return true;
            }
            if let Some(record) = self.live(current) {
                // The nearest guest decides; the process itself, on an exec.
                return record.system;
            }
            match host::process(current) {
                Some(process) if process.ppid > 1 && process.ppid != current => {
                    current = process.ppid
                }
                _ => return false,
            }
        }
        false
    }

    /// Whether `pid` is launchd or descends from it.
    pub fn is_system(&self, pid: i32) -> bool {
        self.records.get(&pid).is_some_and(|record| record.system)
    }

    /// Every running guest outside launchd's tree, by host pid.
    ///
    /// Running means still connected: a guest keeps its connection for life,
    /// and a record without one is a zombie or a process that has become a
    /// host program, neither of which is the prefix's to end.
    pub fn outsiders(&mut self) -> Vec<i32> {
        self.prune();
        self.records
            .iter()
            .filter(|(_, record)| !record.system && record.connection.is_some())
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// Every running guest, by host pid; see [`Registry::outsiders`].
    pub fn connected(&mut self) -> Vec<i32> {
        self.prune();
        self.records
            .iter()
            .filter(|(_, record)| record.connection.is_some())
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// Drop `pid`'s record, for a check-in that was not accepted after all.
    pub fn forget(&mut self, pid: i32) {
        self.records.remove(&pid);
    }

    pub fn checkin(
        &mut self,
        pid: i32,
        connection: u64,
        checkin: Checkin,
        identity: Identity,
    ) -> Credentials {
        let credentials = match checkin.fresh {
            // launchd is root, as the kernel starts it on a Mac; its jobs
            // inherit that and drop to their `UserName` themselves.
            true if self.launchd == Some(pid) => Credentials::of(0, 0, vec![0]),
            true => self
                .inherited(pid)
                .unwrap_or_else(|| checkin.credentials.clone())
                .exec(checkin.setid),
            false => checkin.credentials.clone(),
        };
        if checkin.fresh {
            self.applications
                .retain(|app| app.application.pid != pid || !app.imaged);
            for app in &mut self.applications {
                if app.application.pid == pid {
                    app.imaged = true;
                }
            }
        }
        let system = self.in_launchd_tree(pid);
        // A fresh image is a new task; checking in again on a new connection
        // continues the one there was. A pid with no live record - a `fork`
        // child, or a reused pid - is a new task either way.
        let image = match self.live(pid) {
            Some(record) if !checkin.fresh => record.image,
            _ => {
                self.images += 1;
                self.images
            }
        };
        self.records.insert(
            pid,
            Record {
                identity,
                checkin,
                credentials: credentials.clone(),
                system,
                connection: Some(connection),
                thread_names: HashMap::new(),
                image,
            },
        );
        credentials
    }

    /// The ids a new image at `pid` starts from: the image it replaced, or
    /// else the guest that started it. `None` when it was started from
    /// outside, by a host process, which is its own user.
    fn inherited(&mut self, pid: i32) -> Option<Credentials> {
        if let Some(previous) = self.live(pid) {
            return Some(previous.credentials.clone());
        }
        let parent = host::process(pid)?.ppid;
        self.live(parent).map(|record| record.credentials.clone())
    }

    /// Change `pid`'s ids, if the rules allow it.
    pub fn change(&mut self, pid: i32, change: &Change) -> Result<Credentials, Denied> {
        let record = self.live(pid).ok_or(Denied::Permission)?;
        let next = record.credentials.changed(change)?;
        record.credentials = next.clone();
        Ok(next)
    }

    /// The connection `connection` closed. Its process may still be alive
    /// (a zombie, or about to check in again from a new image), so the record
    /// stays until the host says otherwise.
    pub fn disconnected(&mut self, connection: u64) {
        for record in self.records.values_mut() {
            if record.connection == Some(connection) {
                record.connection = None;
            }
        }
    }

    pub fn name_thread(&mut self, pid: i32, tid: i32, name: String) {
        if let Some(record) = self.records.get_mut(&pid) {
            record.thread_names.insert(tid, name);
        }
    }

    /// Drop every record whose process is gone or is no longer a guest.
    pub fn prune(&mut self) {
        self.records
            .retain(|pid, record| host::alive(*pid, &record.identity));
    }

    fn live(&mut self, pid: i32) -> Option<&mut Record> {
        let alive = host::alive(pid, &self.records.get(&pid)?.identity);
        if !alive {
            self.records.remove(&pid);
            return None;
        }
        self.records.get_mut(&pid)
    }

    /// Make `application` a running application, unless an instance that
    /// already runs is the application for this `launch` (see
    /// [`Application::answers`]): then that one, which the launch is handed to.
    pub fn launch(
        &mut self,
        application: Application,
        launch: Launch,
        identity: Identity,
    ) -> Option<Application> {
        self.prune_applications();
        let pid = application.pid;
        let running = self
            .applications
            .iter()
            .map(|app| &app.application)
            .find(|other| other.answers(&application, launch));
        if let Some(running) = running {
            return Some(running.clone());
        }
        self.applications.retain(|app| app.application.pid != pid);
        self.applications.push(Running {
            application,
            identity,
            imaged: false,
        });
        None
    }

    /// The running applications, oldest first, with `identifier` if given.
    pub fn applications(&mut self, identifier: Option<&str>) -> Vec<Application> {
        self.prune_applications();
        self.applications
            .iter()
            .map(|app| &app.application)
            .filter(|app| identifier.is_none() || app.identifier.as_deref() == identifier)
            .cloned()
            .collect()
    }

    /// Drop the applications that have quit. A zombie counts as quit here:
    /// it is the same process until collected, but it can never be handed a
    /// launch, and its parent may take a while to collect it.
    fn prune_applications(&mut self) {
        self.applications.retain(|app| {
            let pid = app.application.pid;
            host::alive(pid, &app.identity)
                && host::process(pid).is_none_or(|p| p.state != host::State::Zombie)
        });
    }

    pub fn is_guest(&mut self, pid: i32) -> bool {
        self.live(pid).is_some()
    }

    /// The image `pid` (the host's) is running, while it is a live guest.
    pub fn image(&mut self, pid: i32) -> Option<u64> {
        self.live(pid).map(|record| record.image)
    }

    /// Every connected process, for diagnostics and for tests.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn describe(&self, pid: i32) -> Option<Process> {
        let record = self.records.get(&pid)?;
        let host = host::process(pid)?;
        let ppid = match self.records.contains_key(&host.ppid) {
            true => self.guest_pid(host.ppid),
            // launchd's parent is the kernel's, pid 0.
            false if self.launchd == Some(pid) => 0,
            false => LAUNCHD,
        };
        Some(Process {
            pid: self.guest_pid(pid),
            ppid,
            pgid: host.pgid,
            sid: host.sid,
            tty_major: host.tty_major,
            tty_minor: host.tty_minor,
            tpgid: host.tpgid,
            uid: record.credentials.ruid,
            euid: record.credentials.euid,
            suid: record.credentials.svuid,
            gid: record.credentials.rgid,
            egid: record.credentials.egid,
            sgid: record.credentials.svgid,
            groups: record.credentials.groups.clone(),
            state: host.state,
            nice: host.nice,
            start_sec: host.start_sec,
            start_usec: host.start_usec,
            path: record.checkin.path.clone(),
            argv: record.checkin.argv.clone(),
            cputype: record.checkin.cputype,
            cpusubtype: record.checkin.cpusubtype,
            translated: record.checkin.translated,
            image: record.image,
        })
    }

    /// `pid` here and below is the guest's: launchd is asked for as 1.
    pub fn process(&mut self, pid: i32) -> Option<Process> {
        let pid = self.host_pid(pid);
        self.live(pid)?;
        self.describe(pid)
    }

    /// Every live guest process, in pid order.
    pub fn processes(&mut self) -> Vec<Process> {
        self.prune();
        let pids: Vec<i32> = self.records.keys().copied().collect();
        let mut list: Vec<Process> = pids
            .into_iter()
            .filter_map(|pid| self.describe(pid))
            .collect();
        list.sort_unstable_by_key(|process| process.pid);
        list
    }

    pub fn usage(&mut self, pid: i32) -> Option<Usage> {
        let pid = self.host_pid(pid);
        self.live(pid)?;
        host::usage(pid)
    }

    pub fn threads(&mut self, pid: i32) -> Option<Vec<Thread>> {
        let pid = self.host_pid(pid);
        let record = self.live(pid)?;
        let mut threads = host::threads(pid)?;
        record
            .thread_names
            .retain(|tid, _| threads.iter().any(|thread| thread.tid == *tid));
        for thread in &mut threads {
            if let Some(name) = record.thread_names.get(&thread.tid) {
                thread.name = name.clone();
            }
        }
        Some(threads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::SetId;

    #[test]
    fn an_exec_is_a_new_image_and_a_reconnect_is_not() {
        let mut registry = Registry::new();
        let identity = host::identity(me()).unwrap();
        registry.checkin(me(), 1, fresh("/bin/sh", SetId::default()), identity);
        let first = registry.image(me()).unwrap();
        let again = Checkin {
            fresh: false,
            ..checkin("/bin/sh")
        };
        registry.checkin(me(), 2, again, identity);
        assert_eq!(registry.image(me()), Some(first));
        registry.checkin(me(), 3, fresh("/bin/ls", SetId::default()), identity);
        let exec = registry.image(me()).unwrap();
        assert_ne!(exec, first);
        assert_eq!(registry.process(me()).unwrap().image, exec);
    }

    fn me() -> i32 {
        // SAFETY: `getpid` has no preconditions.
        starnix::process::me()
    }

    fn checkin(path: &str) -> Checkin {
        Checkin {
            path: path.into(),
            argv: vec![path.as_bytes().to_vec()],
            ..Checkin::default()
        }
    }

    const USER: u32 = 501;

    fn fresh(path: &str, setid: SetId) -> Checkin {
        Checkin {
            credentials: Credentials::of(USER, 20, vec![20]),
            fresh: true,
            setid,
            ..checkin(path)
        }
    }

    const SETUID_ROOT: SetId = SetId {
        uid: Some(0),
        gid: None,
    };

    #[test]
    fn a_set_user_id_image_runs_as_its_owner_and_is_listed_so() {
        let mut registry = Registry::new();
        let identity = host::identity(me()).unwrap();
        let creds = registry.checkin(me(), 1, fresh("/usr/bin/sudo", SETUID_ROOT), identity);
        assert_eq!((creds.ruid, creds.euid), (USER, 0));
        let listed = registry.process(me()).unwrap();
        assert_eq!((listed.uid, listed.euid, listed.suid), (USER, 0, 0));
    }

    #[test]
    fn an_exec_inherits_the_ids_of_the_image_it_replaces() {
        let mut registry = Registry::new();
        let identity = host::identity(me()).unwrap();
        registry.checkin(me(), 1, fresh("/usr/bin/sudo", SETUID_ROOT), identity);
        registry
            .change(me(), &Change::SetUid(0))
            .expect("root may become root wholly");
        // The image sudo runs is not set-id, and is root all the same.
        let creds = registry.checkin(me(), 2, fresh("/bin/sh", SetId::default()), identity);
        assert_eq!((creds.ruid, creds.euid, creds.svuid), (0, 0, 0));
        assert!(!creds.tainted);
    }

    #[test]
    fn a_process_that_checks_in_again_keeps_the_ids_it_sends() {
        let mut registry = Registry::new();
        let identity = host::identity(me()).unwrap();
        let mine = Credentials::of(0, 0, vec![0]);
        let again = Checkin {
            credentials: mine.clone(),
            ..checkin("/bin/sh")
        };
        assert_eq!(registry.checkin(me(), 1, again, identity), mine);
    }

    #[test]
    fn an_unprivileged_change_is_refused_and_leaves_the_record_alone() {
        let mut registry = Registry::new();
        let identity = host::identity(me()).unwrap();
        registry.checkin(me(), 1, fresh("/bin/sh", SetId::default()), identity);
        assert_eq!(
            registry.change(me(), &Change::SetUid(0)),
            Err(Denied::Permission)
        );
        assert_eq!(registry.process(me()).unwrap().euid, USER);
    }

    #[test]
    fn a_checked_in_process_is_listed_with_its_guest_path() {
        let mut registry = Registry::new();
        registry.checkin(
            me(),
            1,
            checkin("/usr/bin/top"),
            host::identity(me()).unwrap(),
        );
        let list = registry.processes();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].pid, me());
        assert_eq!(list[0].command(), "top");
        // The test runner's parent did not check in, so it is launchd's child.
        assert_eq!(list[0].ppid, LAUNCHD);
    }

    fn app(pid: i32, bundle: &str, identifier: &str, prohibited: bool) -> Application {
        Application {
            pid,
            bundle: bundle.into(),
            identifier: Some(identifier.into()),
            name: None,
            prohibited,
        }
    }

    /// Two live processes: this one and the one that started it.
    fn two() -> (i32, i32) {
        (me(), std::os::unix::process::parent_id() as i32)
    }

    fn launch(registry: &mut Registry, app: Application, how: Launch) -> Option<i32> {
        let identity = host::identity(app.pid).unwrap();
        registry
            .launch(app, how, identity)
            .map(|running| running.pid)
    }

    #[test]
    fn opening_a_running_bundle_hands_the_launch_to_it() {
        let (first, second) = two();
        let mut registry = Registry::new();
        let a = |pid| app(pid, "/Applications/A.app", "com.example.a", false);
        assert_eq!(launch(&mut registry, a(first), Launch::Open), None);
        assert_eq!(launch(&mut registry, a(second), Launch::Open), Some(first));
        assert_eq!(registry.applications(None).len(), 1);
    }

    #[test]
    fn a_launch_is_not_handed_to_an_instance_that_has_quit() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let quit = child.id() as i32;
        let identity = host::identity(quit).unwrap();
        let mut registry = Registry::new();
        let a = |pid| app(pid, "/Applications/A.app", "com.example.a", false);
        assert_eq!(registry.launch(a(quit), Launch::Open, identity), None);
        // Not collected yet: a zombie until the wait below.
        while host::process(quit).is_some_and(|p| p.state != host::State::Zombie) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(launch(&mut registry, a(me()), Launch::Open), None);
        assert_eq!(registry.applications(None).len(), 1);
        child.wait().unwrap();
    }

    #[test]
    fn a_new_instance_or_a_direct_run_is_another_application() {
        let (first, second) = two();
        let mut registry = Registry::new();
        let a = |pid| app(pid, "/Applications/A.app", "com.example.a", false);
        assert_eq!(launch(&mut registry, a(first), Launch::Open), None);
        assert_eq!(launch(&mut registry, a(second), Launch::NewInstance), None);
        assert_eq!(registry.applications(Some("com.example.a")).len(), 2);

        let mut registry = Registry::new();
        assert_eq!(launch(&mut registry, a(first), Launch::Open), None);
        assert_eq!(launch(&mut registry, a(second), Launch::Direct), None);
        assert_eq!(registry.applications(None).len(), 2);
    }

    #[test]
    fn copies_of_one_application_run_side_by_side() {
        let (first, second) = two();
        let mut registry = Registry::new();
        let at = |pid, bundle| app(pid, bundle, "com.example.a", false);
        assert_eq!(
            launch(
                &mut registry,
                at(first, "/Applications/A.app"),
                Launch::Open
            ),
            None
        );
        assert_eq!(
            launch(&mut registry, at(second, "/tmp/A.app"), Launch::Open),
            None
        );
    }

    #[test]
    fn a_prohibited_application_is_one_per_identifier_even_with_new_instance() {
        let (first, second) = two();
        let mut registry = Registry::new();
        let at = |pid, bundle| app(pid, bundle, "com.example.a", true);
        assert_eq!(
            launch(
                &mut registry,
                at(first, "/Applications/A.app"),
                Launch::Open
            ),
            None
        );
        assert_eq!(
            launch(&mut registry, at(second, "/tmp/A.app"), Launch::NewInstance),
            Some(first)
        );
        // Run directly, it is not LaunchServices' to refuse.
        assert_eq!(
            launch(&mut registry, at(second, "/tmp/A.app"), Launch::Direct),
            None
        );
    }

    #[test]
    fn an_exec_into_another_image_is_no_longer_the_application() {
        let (first, second) = two();
        let mut registry = Registry::new();
        let a = |pid| app(pid, "/Applications/A.app", "com.example.a", false);
        assert_eq!(launch(&mut registry, a(first), Launch::Open), None);
        let identity = host::identity(first).unwrap();
        registry.checkin(
            first,
            1,
            fresh("/Applications/A.app/Contents/MacOS/A", SetId::default()),
            identity,
        );
        assert_eq!(registry.applications(None).len(), 1);
        registry.checkin(first, 2, fresh("/bin/sh", SetId::default()), identity);
        assert!(registry.applications(None).is_empty());
        assert_eq!(launch(&mut registry, a(second), Launch::Open), None);
    }

    #[test]
    fn a_closed_connection_does_not_end_a_live_process() {
        let mut registry = Registry::new();
        registry.checkin(me(), 7, checkin("/bin/sh"), host::identity(me()).unwrap());
        registry.disconnected(7);
        assert!(registry.is_guest(me()));
    }

    #[test]
    fn a_process_that_is_no_longer_itself_is_dropped() {
        let mut registry = Registry::new();
        let stale = Identity {
            start: host::identity(me()).unwrap().start + 1,
            ..Identity::default()
        };
        registry.checkin(me(), 1, checkin("/bin/sh"), stale);
        assert!(registry.processes().is_empty());
        assert!(!registry.is_guest(me()));
    }

    #[test]
    fn a_guest_thread_name_outlives_the_hosts_truncation() {
        let mut registry = Registry::new();
        registry.checkin(me(), 1, checkin("/bin/sh"), host::identity(me()).unwrap());
        let long = "com.apple.main-thread.with-a-long-name".to_string();
        let tid = host::threads(me()).unwrap()[0].tid;
        registry.name_thread(me(), tid, long.clone());
        let threads = registry.threads(me()).unwrap();
        let named = threads.iter().find(|thread| thread.tid == tid).unwrap();
        assert_eq!(named.name, long);
    }

    #[test]
    fn launchd_is_pid_one_and_root_and_its_parent_is_the_kernel() {
        let mut registry = Registry::new();
        registry.set_launchd(Some(me()));
        let creds = registry.checkin(
            me(),
            1,
            fresh("/sbin/launchd", SetId::default()),
            host::identity(me()).unwrap(),
        );
        assert_eq!((creds.ruid, creds.euid), (0, 0));
        assert!(registry.is_system(me()));
        let listed = registry.process(LAUNCHD).expect("asked for as pid 1");
        assert_eq!((listed.pid, listed.ppid), (LAUNCHD, 0));
        assert_eq!(registry.processes()[0].pid, LAUNCHD);
        assert!(registry.outsiders().is_empty());
    }

    #[test]
    fn a_descendant_of_launchd_is_its_and_anyone_else_is_an_outsider() {
        let parent = host::process(me()).unwrap().ppid;
        let mut registry = Registry::new();
        registry.set_launchd(Some(parent));
        registry.checkin(
            me(),
            1,
            checkin("/usr/libexec/job"),
            host::identity(me()).unwrap(),
        );
        assert!(registry.is_system(me()));
        assert!(registry.outsiders().is_empty());

        let mut registry = Registry::new();
        registry.set_launchd(Some(i32::MAX));
        registry.checkin(me(), 1, checkin("/bin/sh"), host::identity(me()).unwrap());
        assert!(!registry.is_system(me()));
        assert_eq!(registry.outsiders(), vec![me()]);
        registry.disconnected(1);
        assert!(
            registry.outsiders().is_empty(),
            "only running guests are ended"
        );
    }

    #[test]
    fn only_guests_are_answered() {
        let mut registry = Registry::new();
        assert!(registry.usage(me()).is_none());
        assert!(registry.process(me()).is_none());
    }
}
