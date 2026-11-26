//! The main menu on the desktop's own menu bar: KDE Plasma's global menu, and
//! the other menu bars that import `com.canonical.dbusmenu`.
//!
//! The menu is exported on the session bus as a dbusmenu object, and each
//! window names that object to the desktop: on Wayland through KWin's
//! `org_kde_kwin_appmenu`, on X11 through `com.canonical.AppMenu.Registrar`.
//! Plasma owns the registrar only while something shows global menus, so its
//! presence is what takes the menus out of the windows' own chrome.
//!
//! The object server answers from a snapshot of the menus on its own thread;
//! everything that runs the program's code (validation, delegates, actions)
//! is queued for the main thread's event pump, as AppKit runs it there.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc::object::Id;
use zbus::zvariant::{OwnedValue, StructureBuilder, Type, Value};

use crate::keymap::HostKey;

const REGISTRAR: &str = "com.canonical.AppMenu.Registrar";
const REGISTRAR_PATH: &str = "/com/canonical/AppMenu/Registrar";
const DBUSMENU: &str = "com.canonical.dbusmenu";
const MENU_PATH: &str = "/MenuBar";
const MENU_PATH_C: &std::ffi::CStr = c"/MenuBar";
const DBUSMENU_VERSION: u32 = 3;
/// The id dbusmenu gives the menu bar itself.
const ROOT: i32 = 0;
/// dbusmenu's `Event` ids.
const EVENT_CLICKED: &str = "clicked";
const EVENT_OPENED: &str = "opened";
const EVENT_CLOSED: &str = "closed";
/// dbusmenu's `toggle-state`: anything but off or on is indeterminate.
const TOGGLE_ON: i32 = 1;
const TOGGLE_MIXED: i32 = -1;
/// How often the snapshot is compared with the menus, which a program may
/// change at any time without telling anyone.
const REBUILD_INTERVAL: Duration = Duration::from_millis(200);
/// Deeper than any real menu; a menu that contains itself stops here.
const MAX_DEPTH: usize = 32;

/// Whether the desktop's menu bar is showing the menus, so the windows do not.
static IN_USE: AtomicBool = AtomicBool::new(false);
/// Whether the registrar has an owner, as the bus last said.
static REGISTRAR_OWNED: AtomicBool = AtomicBool::new(false);

/// Whether the desktop's global menu bar has the menus. Any thread.
pub(crate) fn in_use() -> bool {
    IN_USE.load(Ordering::Relaxed)
}

/// What a dbusmenu item shows.
#[derive(Clone, Debug, Default, PartialEq)]
struct Props {
    label: String,
    enabled: bool,
    separator: bool,
    submenu: bool,
    toggle: Option<i32>,
    /// Modifier names, then the key; empty for none.
    shortcut: Vec<String>,
}

impl Props {
    fn root() -> Props {
        Props {
            enabled: true,
            submenu: true,
            ..Props::default()
        }
    }

    /// The properties `names` asks for, or all of them when it is empty.
    fn to_map(&self, names: &[String]) -> HashMap<String, OwnedValue> {
        let mut all: Vec<(&str, Value<'static>)> = Vec::new();
        match self.separator {
            true => all.push(("type", Value::from("separator"))),
            false => {
                all.push(("label", Value::from(mnemonic_free(&self.label))));
                all.push(("enabled", Value::from(self.enabled)));
            }
        }
        if self.submenu {
            all.push(("children-display", Value::from("submenu")));
        }
        if let Some(state) = self.toggle {
            all.push(("toggle-type", Value::from("checkmark")));
            all.push(("toggle-state", Value::from(state)));
        }
        if !self.shortcut.is_empty() {
            all.push(("shortcut", Value::from(vec![self.shortcut.clone()])));
        }
        all.into_iter()
            .filter(|(name, _)| names.is_empty() || names.iter().any(|n| n == name))
            .filter_map(|(name, value)| Some((name.to_string(), OwnedValue::try_from(value).ok()?)))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Node {
    props: Props,
    children: Vec<i32>,
}

#[derive(Default)]
struct Snapshot {
    revision: u32,
    nodes: HashMap<i32, Node>,
}

/// Work for the main thread.
#[derive(Debug)]
enum Request {
    AboutToShow(i32),
    Event { id: i32, kind: String },
}

#[derive(Default)]
struct Shared {
    snapshot: Mutex<Snapshot>,
    requests: Mutex<Vec<Request>>,
}

impl Shared {
    fn queue(&self, request: Request) {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request);
    }
}

/// dbusmenu's `(ia{sv}av)`: an item, its properties and its children, each
/// child the same structure in a variant.
#[derive(serde::Serialize, Type)]
struct Layout {
    id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

impl Layout {
    fn of(nodes: &HashMap<i32, Node>, id: i32, depth: i32, names: &[String]) -> Option<Layout> {
        let node = nodes.get(&id)?;
        let children = match depth {
            0 => Vec::new(),
            _ => node
                .children
                .iter()
                .filter_map(|child| Layout::of(nodes, *child, (depth - 1).max(-1), names))
                .filter_map(Layout::into_variant)
                .collect(),
        };
        Some(Layout {
            id,
            properties: node.props.to_map(names),
            children,
        })
    }

    fn into_variant(self) -> Option<OwnedValue> {
        let structure = StructureBuilder::new()
            .add_field(self.id)
            .add_field(self.properties)
            .add_field(self.children)
            .build()
            .ok()?;
        OwnedValue::try_from(Value::Structure(structure)).ok()
    }
}

/// The exported `com.canonical.dbusmenu` object.
struct MenuBar {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl MenuBar {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let layout = Layout::of(&snapshot.nodes, parent_id, recursion_depth, &property_names)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no item {parent_id}")))?;
        Ok((snapshot.revision, layout))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        ids.into_iter()
            .filter_map(|id| Some((id, snapshot.nodes.get(&id)?.props.to_map(&property_names))))
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        snapshot
            .nodes
            .get(&id)
            .and_then(|node| node.props.to_map(std::slice::from_ref(&name)).remove(&name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no {name} on item {id}")))
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        self.shared.queue(Request::Event { id, kind: event_id });
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let unknown = self.unknown(events.iter().map(|event| event.0));
        for (id, kind, ..) in events {
            self.shared.queue(Request::Event { id, kind });
        }
        unknown
    }

    /// Updates come as `LayoutUpdated` once the main thread has validated
    /// the menu, as Qt's exporter does: the caller is not kept waiting on the
    /// program.
    fn about_to_show(&self, id: i32) -> bool {
        self.shared.queue(Request::AboutToShow(id));
        false
    }

    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let unknown = self.unknown(ids.iter().copied());
        for id in ids {
            self.shared.queue(Request::AboutToShow(id));
        }
        (Vec::new(), unknown)
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        DBUSMENU_VERSION
    }

    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".to_string()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "normal".to_string()
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
}

impl MenuBar {
    fn unknown(&self, ids: impl Iterator<Item = i32>) -> Vec<i32> {
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        ids.filter(|id| !snapshot.nodes.contains_key(id)).collect()
    }
}

/// How windows name the menu to the desktop.
#[derive(Clone, Copy)]
enum Host {
    /// The compositor's `org_kde_kwin_appmenu_manager`.
    Wayland(usize),
    X11,
}

/// A window the desktop has been told about: its appmenu object on Wayland,
/// its X11 window on X11.
#[derive(Clone, Copy)]
enum Registration {
    Wayland(usize),
    X11(u32),
}

struct Exporter {
    connection: zbus::blocking::Connection,
    service: CString,
    shared: Arc<Shared>,
    host: Host,
    /// Every item in the snapshot, retained, by id.
    items: HashMap<i32, usize>,
    ids: HashMap<usize, i32>,
    next_id: i32,
    /// What validation last said of each item, which only a menu about to
    /// open asks the program for.
    validated: HashMap<usize, bool>,
    windows: HashMap<usize, Registration>,
    /// Whether the X11 windows were registered with the registrar there is.
    registered: bool,
    built: Option<Instant>,
}

thread_local! {
    static EXPORTER: RefCell<Option<Exporter>> = const { RefCell::new(None) };
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn install() {
    crate::event::add_pump_hook(pump);
}

/// Export the menus if the desktop can show them, once, before the first
/// window decides how tall its chrome is. Main thread only.
pub(crate) fn prepare() {
    // The unit tests' windows must not depend on the desktop they run on.
    if cfg!(test) || !crate::decor::config().global || STARTED.get() {
        return;
    }
    if !sdl3::video::init_video() || !sdl3::video::is_video_thread() {
        return;
    }
    STARTED.set(true);
    match start() {
        Ok(Some(exporter)) => {
            EXPORTER.with(|e| *e.borrow_mut() = Some(exporter));
            refresh();
            sync_in_use();
        }
        Ok(None) => {}
        Err(error) => {
            crate::window::trace(format_args!("the global menu is not exported: {error}"))
        }
    }
}

fn start() -> zbus::Result<Option<Exporter>> {
    // The virtual desktop's windows are not the host's to name.
    let host = match sdl3::desktop::active() {
        true => None,
        false if sdl3::video::is_x11() => Some(Host::X11),
        false => sdl3::video::wayland_display()
            .and_then(sdl3::wayland::appmenu_manager)
            .map(Host::Wayland),
    };
    let Some(host) = host else {
        return Ok(None);
    };
    let shared = Arc::new(Shared::default());
    let connection = zbus::blocking::connection::Builder::session()?
        .serve_at(
            MENU_PATH,
            MenuBar {
                shared: shared.clone(),
            },
        )?
        .build()?;
    let service = connection
        .unique_name()
        .and_then(|name| CString::new(name.as_str()).ok())
        .ok_or_else(|| zbus::Error::Failure("the bus gave no unique name".to_string()))?;
    // Subscribed before asking, so a change in between is not missed.
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    let changes = bus.receive_name_owner_changed_with_args(&[(0, REGISTRAR)])?;
    let owned = bus.name_has_owner(REGISTRAR.try_into()?)?;
    REGISTRAR_OWNED.store(owned, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("global menu watcher".to_string())
        .spawn(move || {
            for change in changes {
                if let Ok(args) = change.args() {
                    REGISTRAR_OWNED.store(args.new_owner().is_some(), Ordering::Relaxed);
                }
            }
            // The bus went away.
            REGISTRAR_OWNED.store(false, Ordering::Relaxed);
        })
        .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    Ok(Some(Exporter {
        connection,
        service,
        shared,
        host,
        items: HashMap::new(),
        ids: HashMap::new(),
        next_id: ROOT + 1,
        validated: HashMap::new(),
        windows: HashMap::new(),
        registered: false,
        built: None,
    }))
}

/// The event pump's turn: follow the registrar, tell the desktop about new
/// windows, run what the menu bar asked for, and keep the snapshot current.
fn pump() {
    prepare();
    let Some(requests) = with_exporter(|exporter| {
        std::mem::take(
            &mut *exporter
                .shared
                .requests
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }) else {
        return;
    };
    sync_in_use();
    sync_windows();
    let acted = !requests.is_empty();
    for request in requests {
        run(request);
    }
    let due = with_exporter(|exporter| {
        acted
            || exporter
                .built
                .is_none_or(|at| at.elapsed() >= REBUILD_INTERVAL)
    });
    if due == Some(true) {
        refresh();
    }
}

/// Borrow the exporter for work that runs none of the program's code.
fn with_exporter<T>(f: impl FnOnce(&mut Exporter) -> T) -> Option<T> {
    EXPORTER.with(|exporter| {
        let mut exporter = exporter.try_borrow_mut().ok()?;
        exporter.as_mut().map(f)
    })
}

/// Follow the registrar: the windows' menus go when it comes and come back
/// when it goes, and each window keeps its content's size.
fn sync_in_use() {
    let now = with_exporter(|_| REGISTRAR_OWNED.load(Ordering::Relaxed)).unwrap_or(false);
    if IN_USE.swap(now, Ordering::Relaxed) == now {
        return;
    }
    crate::decor::close_menus();
    for window in crate::window::all() {
        crate::window::refit_inset(window);
    }
}

fn sync_windows() {
    let live: Vec<usize> = crate::window::all()
        .into_iter()
        // SAFETY: windows this crate made.
        .map(|window| unsafe { crate::window::sdl_window(window) })
        .filter(|sdl| !sdl.is_null())
        .filter(|sdl| {
            let popup = sdl3::video::flags::POPUP_MENU | sdl3::video::flags::TOOLTIP;
            sdl3::video::window_flags(*sdl) & popup == 0
        })
        .map(|sdl| sdl as usize)
        .collect();
    with_exporter(|exporter| {
        let owned = REGISTRAR_OWNED.load(Ordering::Relaxed);
        // A registrar that is new knows none of the windows.
        let fresh = owned && !exporter.registered;
        exporter.registered = owned;
        for sdl in live {
            let known = exporter.windows.get(&sdl).copied();
            match (exporter.host, known) {
                (Host::X11, Some(Registration::X11(xid))) if fresh => {
                    exporter.register_x11(xid);
                }
                (_, Some(_)) => {}
                (Host::Wayland(manager), None) => {
                    let Some((_, surface)) = sdl3::video::wayland_surface(sdl as sdl3::Window)
                    else {
                        continue;
                    };
                    let appmenu = sdl3::wayland::set_appmenu(
                        manager,
                        surface,
                        &exporter.service,
                        MENU_PATH_C,
                    );
                    if appmenu != 0 {
                        exporter.windows.insert(sdl, Registration::Wayland(appmenu));
                    }
                }
                (Host::X11, None) => {
                    let Some(xid) = sdl3::video::x11_window_id(sdl as sdl3::Window)
                        .and_then(|xid| u32::try_from(xid).ok())
                    else {
                        continue;
                    };
                    if owned {
                        exporter.register_x11(xid);
                    }
                    exporter.windows.insert(sdl, Registration::X11(xid));
                }
            }
        }
    });
}

/// An SDL window is going: the desktop forgets it before its surface goes.
pub(crate) fn destroyed(sdl: sdl3::Window) {
    with_exporter(|exporter| match exporter.windows.remove(&(sdl as usize)) {
        Some(Registration::Wayland(appmenu)) => sdl3::wayland::release_appmenu(appmenu),
        Some(Registration::X11(xid)) if exporter.registered => {
            exporter.tell_registrar("UnregisterWindow", &(xid,));
        }
        _ => {}
    });
}

impl Exporter {
    fn register_x11(&self, xid: u32) {
        let Ok(path) = zbus::zvariant::ObjectPath::try_from(MENU_PATH) else {
            return;
        };
        self.tell_registrar("RegisterWindow", &(xid, path));
    }

    /// Call the registrar without waiting for it: it answers nothing that
    /// matters, and the main thread is the program's.
    fn tell_registrar<B>(&self, method: &str, body: &B)
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let message = zbus::Message::method_call(REGISTRAR_PATH, method)
            .and_then(|m| m.destination(REGISTRAR))
            .and_then(|m| m.interface(REGISTRAR))
            .and_then(|m| m.with_flags(zbus::message::Flags::NoReplyExpected))
            .and_then(|m| m.build(body));
        let sent = message.and_then(|message| self.connection.send(&message));
        if let Err(error) = sent {
            crate::window::trace(format_args!("the menu registrar was not told: {error}"));
        }
    }

    fn id_for(&mut self, item: Id) -> i32 {
        if let Some(id) = self.ids.get(&(item as usize)) {
            return *id;
        }
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(ROOT + 1);
        self.ids.insert(item as usize, id);
        id
    }

    /// The nodes of `menu`'s shown items and their submenus, into `nodes`;
    /// the answer is the items' ids, in order.
    fn add_menu(
        &mut self,
        menu: Id,
        depth: usize,
        nodes: &mut HashMap<i32, Node>,
        live: &mut HashMap<i32, usize>,
    ) -> Vec<i32> {
        if menu.is_null() || depth > MAX_DEPTH {
            return Vec::new();
        }
        let mut children = Vec::new();
        for (at, item) in crate::menu::shown_items(menu).into_iter().enumerate() {
            let id = self.id_for(item);
            // dbusmenu ids are unique; an item shown twice is shown once.
            if live.insert(id, item as usize).is_some() {
                continue;
            }
            let submenu = crate::menu::submenu_of(item);
            let grandchildren = self.add_menu(submenu, depth + 1, nodes, live);
            let props = self.props_of(item, depth == 0 && at == 0);
            nodes.insert(
                id,
                Node {
                    props,
                    children: grandchildren,
                },
            );
            children.push(id);
        }
        children
    }

    fn props_of(&self, item: Id, application_menu: bool) -> Props {
        if crate::menu::is_separator(item) {
            return Props {
                separator: true,
                enabled: true,
                ..Props::default()
            };
        }
        // AppKit titles the application menu with the application's name.
        let label = match application_menu {
            true => crate::metadata::name().to_string_lossy().into_owned(),
            false => crate::menu::display_title(item),
        };
        let enabled = self
            .validated
            .get(&(item as usize))
            .copied()
            .unwrap_or_else(|| crate::menu::is_enabled_flag(item));
        let toggle = match crate::menu::state_of(item) {
            0 => None,
            1 => Some(TOGGLE_ON),
            _ => Some(TOGGLE_MIXED),
        };
        let (key, mask) = crate::menu::key_equivalent_of(item);
        Props {
            label,
            enabled,
            separator: false,
            submenu: !crate::menu::submenu_of(item).is_null(),
            toggle,
            shortcut: shortcut(&key, mask, crate::keymap::command_key().key),
        }
    }
}

/// Read the menus into a new snapshot, and tell the menu bar which menus
/// changed.
fn refresh() {
    let main = crate::menu::main_menu();
    let released = with_exporter(|exporter| {
        let mut nodes = HashMap::new();
        let mut live = HashMap::new();
        let children = exporter.add_menu(main, 0, &mut nodes, &mut live);
        nodes.insert(
            ROOT,
            Node {
                props: Props::root(),
                children,
            },
        );
        for (id, item) in &live {
            if !exporter.items.contains_key(id) {
                exporter
                    .items
                    .insert(*id, objc::arc::retain(*item as Id) as usize);
            }
        }
        let gone: Vec<usize> = exporter
            .items
            .extract_if(|id, _| !live.contains_key(id))
            .map(|(_, item)| item)
            .collect();
        for item in &gone {
            exporter.ids.remove(item);
            exporter.validated.remove(item);
        }
        exporter.built = Some(Instant::now());
        let (revision, changed) = {
            let mut snapshot = exporter
                .shared
                .snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let changed = changed_parents(&snapshot.nodes, &nodes);
            if changed.is_empty() && !snapshot.nodes.is_empty() {
                return gone;
            }
            snapshot.revision = snapshot.revision.wrapping_add(1);
            snapshot.nodes = nodes;
            (snapshot.revision, changed)
        };
        for parent in changed {
            let signal = exporter.connection.emit_signal(
                None::<&str>,
                MENU_PATH,
                DBUSMENU,
                "LayoutUpdated",
                &(revision, parent),
            );
            if let Err(error) = signal {
                crate::window::trace(format_args!("LayoutUpdated was not sent: {error}"));
            }
        }
        gone
    });
    // Outside the borrow: the last release of an item runs its -dealloc.
    for item in released.unwrap_or_default() {
        objc::arc::release(item as Id);
    }
}

/// The menus whose items are not what they were: a different list, or an
/// item that shows differently. A menu new to the snapshot is left to its
/// parent, which lists it.
fn changed_parents(old: &HashMap<i32, Node>, new: &HashMap<i32, Node>) -> Vec<i32> {
    let mut changed: Vec<i32> = new
        .iter()
        .filter(|(id, node)| {
            old.get(id).is_some_and(|before| {
                before.children != node.children
                    || node.children.iter().any(|child| {
                        old.get(child).map(|n| &n.props) != new.get(child).map(|n| &n.props)
                    })
            })
        })
        .map(|(id, _)| *id)
        .collect();
    changed.sort_unstable();
    changed
}

/// The item behind `id`, retained for the caller (nil for the menu bar
/// itself), and the menu it opens.
fn item_and_menu(id: i32) -> Option<(Id, Id)> {
    if id == ROOT {
        let main = crate::menu::main_menu();
        return (!main.is_null()).then_some((std::ptr::null_mut(), main));
    }
    let item = with_exporter(|exporter| exporter.items.get(&id).copied())?? as Id;
    let item = objc::arc::retain(item);
    Some((item, crate::menu::submenu_of(item)))
}

/// Run a request of the menu bar's, on the main thread, as the in-window
/// menus would.
fn run(request: Request) {
    let id = match &request {
        Request::AboutToShow(id) | Request::Event { id, .. } => *id,
    };
    let Some((item, menu)) = item_and_menu(id) else {
        return;
    };
    let submenu = !menu.is_null();
    match request {
        Request::AboutToShow(_) if submenu => {
            let items = crate::menu::prepare(menu);
            let enabled: Vec<(usize, bool)> = items
                .iter()
                .filter(|item| !crate::menu::is_separator(**item))
                .map(|item| (*item as usize, crate::menu::is_item_enabled(*item)))
                .collect();
            crate::menu::release_items(items);
            with_exporter(|exporter| exporter.validated.extend(enabled));
        }
        Request::Event { kind, .. } => match kind.as_str() {
            EVENT_OPENED if submenu => crate::menu::will_open(menu),
            EVENT_CLOSED if submenu => crate::menu::did_close(menu),
            EVENT_CLICKED if !submenu && crate::menu::is_item_enabled(item) => {
                crate::menu::perform(item);
            }
            _ => {}
        },
        Request::AboutToShow(_) => {}
    }
    if !item.is_null() {
        objc::arc::release(item);
    }
}

/// dbusmenu reads `_` as a mnemonic; a Mac title has none.
fn mnemonic_free(label: &str) -> String {
    label.replace('_', "__")
}

/// A key equivalent as dbusmenu spells a shortcut: the host modifiers that
/// type it, then the key, as Qt names keys. `command` is the host key the
/// guest sees as Command, which takes the place of Super.
fn shortcut(key: &str, mask: u64, command: HostKey) -> Vec<String> {
    use crate::keymap::flags;
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Vec::new();
    };
    let name = match c {
        '\r' | '\n' | '\u{3}' => "Return".to_string(),
        '\u{1b}' => "Esc".to_string(),
        '\t' => "Tab".to_string(),
        '\u{8}' | '\u{7f}' => "Backspace".to_string(),
        ' ' => "Space".to_string(),
        '\u{F700}' => "Up".to_string(),
        '\u{F701}' => "Down".to_string(),
        '\u{F702}' => "Left".to_string(),
        '\u{F703}' => "Right".to_string(),
        '\u{F728}' => "Del".to_string(),
        '\u{F729}' => "Home".to_string(),
        '\u{F72B}' => "End".to_string(),
        '\u{F72C}' => "PgUp".to_string(),
        '\u{F72D}' => "PgDown".to_string(),
        // NSF1FunctionKey onwards.
        '\u{F704}'..='\u{F726}' => format!("F{}", c as u32 - 0xF704 + 1),
        other => other.to_uppercase().collect(),
    };
    let host = |key: HostKey| match key {
        HostKey::Ctrl => "Control",
        HostKey::Alt => "Alt",
        HostKey::Super => "Super",
    };
    // The key Command takes the place of is reached through Super.
    let swapped = |guest: HostKey| match guest == command {
        true => "Super",
        false => host(guest),
    };
    let shift = mask & flags::SHIFT != 0 || (c.is_alphabetic() && c.is_uppercase());
    let held = [
        (mask & flags::CONTROL != 0, swapped(HostKey::Ctrl)),
        (mask & flags::OPTION != 0, swapped(HostKey::Alt)),
        (shift, "Shift"),
        (mask & flags::COMMAND != 0, host(command)),
    ];
    // In dbusmenu's order, whichever guest modifier each came from.
    let mut out: Vec<String> = ["Control", "Alt", "Shift", "Super"]
        .into_iter()
        .filter(|name| held.iter().any(|(on, held)| *on && held == name))
        .map(str::to_string)
        .collect();
    out.push(name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::flags;

    #[test]
    fn a_title_keeps_its_underscores() {
        assert_eq!(mnemonic_free("Save_As"), "Save__As");
    }

    #[test]
    fn a_shortcut_names_the_host_keys_that_type_it() {
        let names = |key, mask, command| shortcut(key, mask, command);
        assert_eq!(names("q", flags::COMMAND, HostKey::Super), ["Super", "Q"]);
        assert_eq!(names("q", flags::COMMAND, HostKey::Ctrl), ["Control", "Q"]);
        // Upper case is Shift, as a Mac menu reads it.
        assert_eq!(
            names("S", flags::COMMAND, HostKey::Super),
            ["Shift", "Super", "S"]
        );
        // With Control as Command, the guest's Control is the host's Super.
        assert_eq!(
            names("f", flags::COMMAND | flags::CONTROL, HostKey::Ctrl),
            ["Control", "Super", "F"]
        );
        assert_eq!(
            names("\u{F704}", flags::OPTION, HostKey::Super),
            ["Alt", "F1"]
        );
        assert!(names("", flags::COMMAND, HostKey::Super).is_empty());
    }

    fn node(label: &str, children: &[i32]) -> Node {
        Node {
            props: Props {
                label: label.to_string(),
                enabled: true,
                ..Props::default()
            },
            children: children.to_vec(),
        }
    }

    #[test]
    fn a_menu_is_updated_when_its_items_change() {
        let old: HashMap<i32, Node> = [
            (ROOT, node("", &[1, 2])),
            (1, node("File", &[3])),
            (2, node("Edit", &[4])),
            (3, node("Open", &[])),
            (4, node("Undo", &[])),
        ]
        .into_iter()
        .collect();
        assert!(changed_parents(&old, &old).is_empty());

        let mut retitled = old.clone();
        retitled.insert(4, node("Undo Typing", &[]));
        assert_eq!(changed_parents(&old, &retitled), [2]);

        let mut added = old.clone();
        added.insert(5, node("Close", &[]));
        added.insert(1, node("File", &[3, 5]));
        assert_eq!(changed_parents(&old, &added), [1]);
    }

    #[test]
    fn the_layout_goes_as_deep_as_asked() {
        let nodes: HashMap<i32, Node> = [
            (ROOT, node("", &[1])),
            (1, node("File", &[2])),
            (2, node("Open", &[])),
        ]
        .into_iter()
        .collect();
        let shallow = Layout::of(&nodes, ROOT, 0, &[]).unwrap();
        assert!(shallow.children.is_empty());
        let one = Layout::of(&nodes, ROOT, 1, &[]).unwrap();
        assert_eq!(one.children.len(), 1);
        let all = Layout::of(&nodes, ROOT, -1, &["label".to_string()]).unwrap();
        let Value::Structure(file) = &*all.children[0] else {
            panic!("a child is a structure");
        };
        let Value::Array(grandchildren) = &file.fields()[2] else {
            panic!("children are an array");
        };
        assert_eq!(grandchildren.len(), 1);
        assert!(Layout::of(&nodes, 9, -1, &[]).is_none());
    }
}
