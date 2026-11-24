//! What `rosemary aot` is doing, for a status display to sample.
//!
//! The build writes relaxed atomics as it goes and never waits on a reader, so a
//! build with no display pays only for the counters. A reader takes a
//! [`BoardView`], a plain copy, whenever it wants one; fields read one at a time
//! may disagree by a region or two, which a display does not mind.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::entries::Source;

/// Events kept for display; older ones are dropped.
const EVENTS_KEPT: usize = 8;

/// What the whole build is doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Loading and linking the program, before any image is looked at.
    Loading,
    /// Reading images and choosing their regions.
    Discovering,
    Compiling,
    /// Committing the last parts of every image.
    Finishing,
    Done,
}

impl Phase {
    const ALL: [Phase; 5] = [
        Phase::Loading,
        Phase::Discovering,
        Phase::Compiling,
        Phase::Finishing,
        Phase::Done,
    ];
}

/// Where one image is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageState {
    /// Reading and hashing its file.
    Reading,
    /// Enumerating its entry points.
    Discovering,
    /// Choosing a region for each entry.
    Planning,
    /// Planned; no thread has taken a region of it yet.
    Queued,
    Compiling,
    Done,
    /// The deadline passed before every region was compiled.
    Stopped,
    Skipped,
    Failed,
}

impl ImageState {
    const ALL: [ImageState; 9] = [
        ImageState::Reading,
        ImageState::Discovering,
        ImageState::Planning,
        ImageState::Queued,
        ImageState::Compiling,
        ImageState::Done,
        ImageState::Stopped,
        ImageState::Skipped,
        ImageState::Failed,
    ];

    /// Whether the image is still being read or planned.
    pub fn discovering(self) -> bool {
        matches!(
            self,
            ImageState::Reading | ImageState::Discovering | ImageState::Planning
        )
    }

    pub fn settled(self) -> bool {
        matches!(
            self,
            ImageState::Done | ImageState::Stopped | ImageState::Skipped | ImageState::Failed
        )
    }
}

/// What one compiler thread is doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Lifting,
    /// Holding a lifted large region until the budget has room for it.
    Waiting,
    Compiling,
    Writing,
    /// Committing a finished part, which maps it once so the kernel validates it.
    Committing,
}

impl Activity {
    pub const ALL: [Activity; 6] = [
        Activity::Idle,
        Activity::Lifting,
        Activity::Waiting,
        Activity::Compiling,
        Activity::Writing,
        Activity::Committing,
    ];
    pub const COUNT: usize = Self::ALL.len();
}

/// How important a logged event is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    Info,
    Good,
    Warn,
    Bad,
}

#[derive(Clone, Debug)]
pub struct Event {
    /// Since the board was made.
    pub at: Duration,
    pub kind: EventKind,
    pub text: String,
}

/// The state of one `rosemary aot` build.
pub struct Board {
    started: Instant,
    /// The program being built, as a person knows it.
    program: String,
    /// Whether a reader wants per-region names, which cost a symbol table per image.
    detailed: bool,
    phase: AtomicU8,
    /// What the phase is about: the program being loaded, the image being linked.
    subject: Mutex<String>,
    /// Since `started`, plus one; zero for no deadline.
    deadline: AtomicU64,
    deadline_reached: AtomicBool,
    images: Mutex<Vec<Arc<ImageBoard>>>,
    workers: Mutex<Vec<Arc<WorkerBoard>>>,
    /// CLIF instructions of large regions compiling, and the most there may be.
    budget_used: AtomicUsize,
    budget_limit: AtomicUsize,
    /// Large regions waiting for the budget.
    deferred: AtomicUsize,
    events: Mutex<VecDeque<Event>>,
}

impl Board {
    pub fn new(program: &str, detailed: bool) -> Self {
        Board {
            started: Instant::now(),
            program: display_name(program).to_owned(),
            detailed,
            phase: AtomicU8::new(Phase::Loading as u8),
            subject: Mutex::new(String::new()),
            deadline: AtomicU64::new(0),
            deadline_reached: AtomicBool::new(false),
            images: Mutex::new(Vec::new()),
            workers: Mutex::new(Vec::new()),
            budget_used: AtomicUsize::new(0),
            budget_limit: AtomicUsize::new(0),
            deferred: AtomicUsize::new(0),
            events: Mutex::new(VecDeque::new()),
        }
    }

    pub fn detailed(&self) -> bool {
        self.detailed
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn set_phase(&self, phase: Phase, subject: &str) {
        self.phase.store(phase as u8, Ordering::Relaxed);
        let mut current = lock(&self.subject);
        current.clear();
        current.push_str(subject);
    }

    pub fn set_deadline(&self, deadline: Option<Instant>) {
        let encoded = deadline.map_or(0, |at| {
            at.saturating_duration_since(self.started).as_nanos() as u64 + 1
        });
        self.deadline.store(encoded, Ordering::Relaxed);
    }

    /// Records, once, that the deadline stopped the build from starting more work.
    pub fn deadline_reached(&self) {
        if !self.deadline_reached.swap(true, Ordering::Relaxed) {
            self.event(
                EventKind::Warn,
                "deadline reached: finishing the regions in flight".into(),
            );
        }
    }

    pub fn event(&self, kind: EventKind, text: String) {
        let mut events = lock(&self.events);
        if events.len() == EVENTS_KEPT {
            events.pop_front();
        }
        events.push_back(Event {
            at: self.now(),
            kind,
            text,
        });
    }

    /// Registers an image about to be read.
    pub fn image(&self, path: &str) -> Arc<ImageBoard> {
        let mut images = lock(&self.images);
        let image = Arc::new(ImageBoard::new(images.len(), path));
        images.push(image.clone());
        image
    }

    /// Makes one board per compiler thread.
    pub fn workers(&self, threads: usize) -> Vec<Arc<WorkerBoard>> {
        let workers: Vec<_> = (0..threads)
            .map(|index| Arc::new(WorkerBoard::new(index, self.started)))
            .collect();
        *lock(&self.workers) = workers.clone();
        workers
    }

    pub fn set_budget_limit(&self, limit: usize) {
        self.budget_limit.store(limit, Ordering::Relaxed);
    }

    pub fn budget_taken(&self, cost: usize) {
        self.budget_used.fetch_add(cost, Ordering::Relaxed);
    }

    pub fn budget_given(&self, cost: usize) {
        self.budget_used.fetch_sub(cost, Ordering::Relaxed);
    }

    pub fn set_deferred(&self, deferred: usize) {
        self.deferred.store(deferred, Ordering::Relaxed);
    }

    pub fn view(&self) -> BoardView {
        let elapsed = self.now();
        let deadline = match self.deadline.load(Ordering::Relaxed) {
            0 => None,
            encoded => Some(Duration::from_nanos(encoded - 1)),
        };
        BoardView {
            program: self.program.clone(),
            elapsed,
            phase: Phase::ALL[usize::from(self.phase.load(Ordering::Relaxed))],
            subject: lock(&self.subject).clone(),
            deadline,
            deadline_reached: self.deadline_reached.load(Ordering::Relaxed),
            images: lock(&self.images).iter().map(|i| i.view()).collect(),
            workers: lock(&self.workers)
                .iter()
                .map(|w| w.view(elapsed))
                .collect(),
            budget_used: self.budget_used.load(Ordering::Relaxed),
            budget_limit: self.budget_limit.load(Ordering::Relaxed),
            deferred: self.deferred.load(Ordering::Relaxed),
            events: lock(&self.events).iter().cloned().collect(),
        }
    }
}

/// One image's progress.
pub struct ImageBoard {
    pub index: usize,
    pub path: String,
    state: AtomicU8,
    /// Why it was skipped or failed.
    note: Mutex<Option<String>>,
    /// Entries found per [`Source`].
    sources: [AtomicUsize; Source::COUNT],
    entries: AtomicUsize,
    /// Entries planned so far, and the address of the last.
    examined: AtomicUsize,
    cursor: AtomicU64,
    planned: AtomicUsize,
    hot: AtomicUsize,
    compiled: AtomicUsize,
    refused: AtomicUsize,
    /// Hot regions compiled or refused.
    hot_finished: AtomicUsize,
    code_bytes: AtomicU64,
    parts: AtomicU32,
    hot_committed: AtomicBool,
}

impl ImageBoard {
    fn new(index: usize, path: &str) -> Self {
        ImageBoard {
            index,
            path: path.to_owned(),
            state: AtomicU8::new(ImageState::Reading as u8),
            note: Mutex::new(None),
            sources: Default::default(),
            entries: AtomicUsize::new(0),
            examined: AtomicUsize::new(0),
            cursor: AtomicU64::new(0),
            planned: AtomicUsize::new(0),
            hot: AtomicUsize::new(0),
            compiled: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            hot_finished: AtomicUsize::new(0),
            code_bytes: AtomicU64::new(0),
            parts: AtomicU32::new(0),
            hot_committed: AtomicBool::new(false),
        }
    }

    /// The file name a person knows the image by.
    pub fn name(&self) -> &str {
        display_name(&self.path)
    }

    fn state(&self) -> ImageState {
        ImageState::ALL[usize::from(self.state.load(Ordering::Relaxed))]
    }

    pub fn set_state(&self, state: ImageState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }

    /// `entries` were found, `by_source` of each kind; planning starts.
    pub fn discovered(&self, entries: usize, by_source: &[usize; Source::COUNT]) {
        for (count, &found) in self.sources.iter().zip(by_source) {
            count.store(found, Ordering::Relaxed);
        }
        self.entries.store(entries, Ordering::Relaxed);
        self.set_state(ImageState::Planning);
    }

    /// The `examined`th entry, at `address`, was planned, making `regions` so far.
    pub fn examining(&self, examined: usize, address: u64, regions: usize) {
        self.examined.store(examined, Ordering::Relaxed);
        self.cursor.store(address, Ordering::Relaxed);
        self.planned.store(regions, Ordering::Relaxed);
    }

    pub fn planned(&self, regions: usize, hot: usize) {
        self.planned.store(regions, Ordering::Relaxed);
        self.hot.store(hot, Ordering::Relaxed);
        self.set_state(ImageState::Queued);
    }

    /// A thread took one of its regions.
    pub fn started(&self) {
        let _ = self.state.compare_exchange(
            ImageState::Queued as u8,
            ImageState::Compiling as u8,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    pub fn region_finished(&self, hot: bool, written: bool) {
        match written {
            true => self.compiled.fetch_add(1, Ordering::Relaxed),
            false => self.refused.fetch_add(1, Ordering::Relaxed),
        };
        if hot {
            self.hot_finished.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn wrote(&self, bytes: u64) {
        self.code_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn committed(&self, board: &Board, hot: bool) {
        self.parts.fetch_add(1, Ordering::Relaxed);
        if hot && !self.hot_committed.swap(true, Ordering::Relaxed) {
            board.event(
                EventKind::Good,
                format!("{}: start-up part committed", self.name()),
            );
        }
    }

    pub fn finished(&self, board: &Board, complete: bool) {
        let (state, kind, verb) = match complete {
            true => (ImageState::Done, EventKind::Good, "done"),
            false => (
                ImageState::Stopped,
                EventKind::Warn,
                "stopped at the deadline",
            ),
        };
        self.set_state(state);
        let compiled = self.compiled.load(Ordering::Relaxed);
        let refused = self.refused.load(Ordering::Relaxed);
        board.event(
            kind,
            format!(
                "{}: {verb}, {compiled} regions cached, {refused} refused",
                self.name()
            ),
        );
    }

    pub fn skipped(&self, board: &Board, reason: &str) {
        *lock(&self.note) = Some(reason.to_owned());
        self.set_state(ImageState::Skipped);
        board.event(
            EventKind::Info,
            format!("{}: skipped: {reason}", self.name()),
        );
    }

    pub fn failed(&self, board: &Board, error: &str) {
        *lock(&self.note) = Some(error.to_owned());
        self.set_state(ImageState::Failed);
        board.event(EventKind::Bad, format!("{}: {error}", self.name()));
    }

    fn view(&self) -> ImageView {
        let load = |n: &AtomicUsize| n.load(Ordering::Relaxed);
        ImageView {
            name: self.name().to_owned(),
            state: self.state(),
            note: lock(&self.note).clone(),
            sources: std::array::from_fn(|n| load(&self.sources[n])),
            entries: load(&self.entries),
            examined: load(&self.examined),
            cursor: self.cursor.load(Ordering::Relaxed),
            planned: load(&self.planned),
            hot: load(&self.hot),
            compiled: load(&self.compiled),
            refused: load(&self.refused),
            hot_finished: load(&self.hot_finished),
            code_bytes: self.code_bytes.load(Ordering::Relaxed),
            parts: self.parts.load(Ordering::Relaxed),
            hot_committed: self.hot_committed.load(Ordering::Relaxed),
        }
    }
}

/// The region a thread is working on.
#[derive(Clone, Debug, Default)]
pub struct Current {
    /// Index of its image on the board.
    pub image: Option<usize>,
    pub address: u64,
    pub source: Option<Source>,
    /// The symbol it starts in, when the image has one there.
    pub name: String,
    /// CLIF instructions, once lifted.
    pub instructions: usize,
    pub hot: bool,
    /// When the thread took it.
    pub since: Duration,
}

/// One compiler thread's progress.
pub struct WorkerBoard {
    pub index: usize,
    started: Instant,
    activity: AtomicU8,
    /// When `activity` began, since `started`, in nanoseconds.
    since: AtomicU64,
    /// Time spent per [`Activity`] before the current one, in nanoseconds.
    spent: [AtomicU64; Activity::COUNT],
    regions: AtomicUsize,
    current: Mutex<Current>,
}

impl WorkerBoard {
    fn new(index: usize, started: Instant) -> Self {
        WorkerBoard {
            index,
            started,
            activity: AtomicU8::new(Activity::Idle as u8),
            since: AtomicU64::new(started.elapsed().as_nanos() as u64),
            spent: Default::default(),
            regions: AtomicUsize::new(0),
            current: Mutex::new(Current::default()),
        }
    }

    pub fn set(&self, activity: Activity) {
        let now = self.started.elapsed().as_nanos() as u64;
        let since = self.since.swap(now, Ordering::Relaxed);
        let previous = self.activity.swap(activity as u8, Ordering::Relaxed);
        self.spent[usize::from(previous)].fetch_add(now.saturating_sub(since), Ordering::Relaxed);
    }

    /// The thread took a region of image `image` at `address`.
    pub fn begin(
        &self,
        image: usize,
        address: u64,
        source: Source,
        name: Option<(&str, u64)>,
        hot: bool,
    ) {
        use std::fmt::Write;
        let mut current = lock(&self.current);
        current.image = Some(image);
        current.address = address;
        current.source = Some(source);
        current.name.clear();
        match name {
            Some((name, 0)) => current.name.push_str(name),
            Some((name, offset)) => {
                let _ = write!(current.name, "{name}+{offset:#x}");
            }
            None => {}
        }
        current.instructions = 0;
        current.hot = hot;
        current.since = self.started.elapsed();
    }

    pub fn lifted(&self, instructions: usize) {
        lock(&self.current).instructions = instructions;
    }

    pub fn region_done(&self) {
        self.regions.fetch_add(1, Ordering::Relaxed);
    }

    /// Done with every region: nothing is current any more.
    pub fn retire(&self) {
        self.set(Activity::Idle);
        lock(&self.current).image = None;
    }

    fn view(&self, now: Duration) -> WorkerView {
        let activity = Activity::ALL[usize::from(self.activity.load(Ordering::Relaxed))];
        let since = Duration::from_nanos(self.since.load(Ordering::Relaxed));
        let mut spent: [Duration; Activity::COUNT] =
            std::array::from_fn(|n| Duration::from_nanos(self.spent[n].load(Ordering::Relaxed)));
        spent[activity as usize] += now.saturating_sub(since);
        WorkerView {
            index: self.index,
            activity,
            spent,
            regions: self.regions.load(Ordering::Relaxed),
            current: lock(&self.current).clone(),
        }
    }
}

/// A copy of a [`Board`] at one moment.
#[derive(Clone, Debug)]
pub struct BoardView {
    pub program: String,
    pub elapsed: Duration,
    pub phase: Phase,
    pub subject: String,
    /// When the build stops starting work, since it began.
    pub deadline: Option<Duration>,
    pub deadline_reached: bool,
    pub images: Vec<ImageView>,
    pub workers: Vec<WorkerView>,
    pub budget_used: usize,
    pub budget_limit: usize,
    pub deferred: usize,
    pub events: Vec<Event>,
}

impl BoardView {
    /// Regions planned in every image, and how many of them are hot.
    pub fn planned(&self) -> (usize, usize) {
        self.images
            .iter()
            .fold((0, 0), |(all, hot), i| (all + i.planned, hot + i.hot))
    }

    /// Regions compiled or refused in every image, and how many of them are hot.
    pub fn finished(&self) -> (usize, usize) {
        self.images.iter().fold((0, 0), |(all, hot), i| {
            (all + i.compiled + i.refused, hot + i.hot_finished)
        })
    }
}

#[derive(Clone, Debug)]
pub struct ImageView {
    pub name: String,
    pub state: ImageState,
    pub note: Option<String>,
    pub sources: [usize; Source::COUNT],
    pub entries: usize,
    pub examined: usize,
    pub cursor: u64,
    pub planned: usize,
    pub hot: usize,
    pub compiled: usize,
    pub refused: usize,
    pub hot_finished: usize,
    pub code_bytes: u64,
    pub parts: u32,
    pub hot_committed: bool,
}

impl ImageView {
    pub fn finished(&self) -> usize {
        self.compiled + self.refused
    }
}

#[derive(Clone, Debug)]
pub struct WorkerView {
    pub index: usize,
    pub activity: Activity,
    /// Total time per [`Activity`], the current one included.
    pub spent: [Duration; Activity::COUNT],
    pub regions: usize,
    pub current: Current,
}

/// The file name of `path`.
fn display_name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    if file.is_empty() { path } else { file }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_time_includes_the_activity_in_progress() {
        let board = Board::new("test", false);
        let worker = &board.workers(1)[0];
        worker.set(Activity::Compiling);
        std::thread::sleep(Duration::from_millis(5));
        let view = &board.view().workers[0];
        assert_eq!(view.activity, Activity::Compiling);
        assert!(view.spent[Activity::Compiling as usize] >= Duration::from_millis(5));
    }

    #[test]
    fn an_image_compiles_once_a_thread_takes_a_region() {
        let board = Board::new("test", false);
        let image = board.image("/a/libfoo.dylib");
        image.planned(3, 1);
        assert_eq!(board.view().images[0].state, ImageState::Queued);
        image.started();
        image.region_finished(true, true);
        image.region_finished(false, false);
        let view = &board.view().images[0];
        assert_eq!(view.state, ImageState::Compiling);
        assert_eq!((view.compiled, view.refused, view.hot_finished), (1, 1, 1));
        assert_eq!(view.name, "libfoo.dylib");
    }

    #[test]
    fn events_keep_only_the_latest() {
        let board = Board::new("test", false);
        for n in 0..EVENTS_KEPT + 2 {
            board.event(EventKind::Info, n.to_string());
        }
        let events = board.view().events;
        assert_eq!(events.len(), EVENTS_KEPT);
        assert_eq!(events[0].text, "2");
    }
}
