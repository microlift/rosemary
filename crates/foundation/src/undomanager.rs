//! `NSUndoManager`: a stack of undo groups and a stack of redo groups, each
//! group a list of actions recorded by `-registerUndoWithTarget:...`.
//!
//! Behaviour follows Apple's documentation for the class and its members
//! (`https://developer.apple.com/documentation/foundation/nsundomanager`);
//! see the `FIXME (ACCURACY)` comments below for where this falls short of
//! it.

use std::sync::{Mutex, OnceLock};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::block;
use crate::boxed;
use crate::define::methods;

/// The notification names this class posts, exported the same way every
/// other Foundation notification name is - see `crate::NOTIFICATION_NAMES`.
pub const CHECKPOINT: &str = "NSUndoManagerCheckpointNotification";
pub const WILL_UNDO_CHANGE: &str = "NSUndoManagerWillUndoChangeNotification";
pub const DID_UNDO_CHANGE: &str = "NSUndoManagerDidUndoChangeNotification";
pub const WILL_REDO_CHANGE: &str = "NSUndoManagerWillRedoChangeNotification";
pub const DID_REDO_CHANGE: &str = "NSUndoManagerDidRedoChangeNotification";
pub const DID_OPEN_GROUP: &str = "NSUndoManagerDidOpenUndoGroupNotification";
pub const WILL_CLOSE_GROUP: &str = "NSUndoManagerWillCloseUndoGroupNotification";
pub const DID_CLOSE_GROUP: &str = "NSUndoManagerDidCloseUndoGroupNotification";

/// One recorded undo (or redo) operation.
///
/// The target is never retained - Apple's own rule, and the reason
/// `-removeAllActionsWithTarget:` exists: a manager that retained every
/// target it was given would keep each of them alive for as long as the
/// action sits on a stack.
enum Action {
    Selector {
        target: Id,
        selector: Sel,
        object: Id,
    },
    Handler {
        target: Id,
        block: usize,
    },
}

impl Action {
    fn targets(&self, who: Id) -> bool {
        match self {
            Action::Selector { target, .. } | Action::Handler { target, .. } => *target == who,
        }
    }

    /// Run the recorded operation.
    ///
    /// # Safety
    ///
    /// The target, if non-null, must still be live and must still answer to
    /// the selector - or accept the block - the registration validated.
    unsafe fn run(&self) {
        match self {
            Action::Selector {
                target,
                selector,
                object,
            } => {
                let (target, selector, object) = (*target, *selector, *object);
                if target.is_null() {
                    return;
                }
                // SAFETY: a live target.
                let Some(method) =
                    (unsafe { objc::object::class_of(target) }).and_then(|c| c.method(selector))
                else {
                    // Apple raises here; a named abort instead, for the reason
                    // `notification::send_notification` gives the same choice.
                    objc::fail!(
                        "-[NSUndoManager undo]: the target does not respond to the selector it \
                         registered"
                    );
                };
                // SAFETY: the method found above, sent the one argument
                // `-registerUndoWithTarget:selector:object:` declares.
                unsafe {
                    let f: unsafe extern "C" fn(Id, Sel, Id) =
                        std::mem::transmute(method.implementation());
                    f(target, selector, object);
                }
            }
            Action::Handler { target, block } => {
                let (target, block) = (*target, *block);
                if block == 0 {
                    return;
                }
                // SAFETY: a `void (^)(id)` this registration copied.
                unsafe { block::call_with(block as Id, target) };
            }
        }
    }
}

impl Drop for Action {
    fn drop(&mut self) {
        match self {
            Action::Selector { object, .. } => objc::arc::release(*object),
            Action::Handler { block, .. } => objc::block::release(*block as Id),
        }
    }
}

/// One undo grouping: the actions registered inside it, in the order they
/// were registered, and the name a menu item built from it shows.
#[derive(Default)]
struct Group {
    actions: Vec<Action>,
    action_name: String,
}

struct Inner {
    /// Closed groups, oldest first - the last one is the next `-undo` runs.
    undo_stack: Vec<Group>,
    /// As `undo_stack`, for `-redo`.
    redo_stack: Vec<Group>,
    /// Groups still being accumulated, outermost first. Empty when nothing
    /// is open.
    open: Vec<Group>,
    /// Zero means no limit.
    levels_of_undo: usize,
    group_by_event: bool,
    /// Whether the outermost open group was opened by a bare registration
    /// rather than an explicit `-beginUndoGrouping`.
    auto_opened: bool,
    /// Whether [`schedule_auto_close`] already has a run-loop observer
    /// pending for this manager.
    close_scheduled: bool,
    is_undoing: bool,
    is_redoing: bool,
    /// Incremented by `-disableUndoRegistration`, decremented by
    /// `-enableUndoRegistration`; registration is enabled at zero.
    disable_count: u32,
    /// `-runLoopModes`, lazily built; null until first asked or set.
    run_loop_modes: Id,
}

impl Default for Inner {
    fn default() -> Self {
        Inner {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            open: Vec::new(),
            levels_of_undo: 0,
            group_by_event: true,
            auto_opened: false,
            close_scheduled: false,
            is_undoing: false,
            is_redoing: false,
            disable_count: 0,
            run_loop_modes: std::ptr::null_mut(),
        }
    }
}

/// An `NSUndoManager`'s storage, boxed the way every other Foundation class
/// with Rust-side state is - see `crate::boxed`.
#[derive(Default)]
struct Manager {
    inner: Mutex<Inner>,
}

impl Drop for Manager {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Dropping each `Vec<Group>` runs every `Action`'s own `Drop`, which
        // is where the retained halves of a registration are let go.
        inner.undo_stack.clear();
        inner.redo_stack.clear();
        inner.open.clear();
        objc::arc::release(std::mem::replace(
            &mut inner.run_loop_modes,
            std::ptr::null_mut(),
        ));
    }
}

fn manager_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = boxed::register::<Manager>(c"NSUndoManager", None);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// The state behind an `NSUndoManager`, or `None` for anything that is not
/// one.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn state<'a>(object: Id) -> Option<&'a Mutex<Inner>> {
    // SAFETY: the caller's obligation.
    unsafe { boxed::contents::<Manager>(object, manager_class()) }.map(|m| &m.inner)
}

fn lock(state: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drop the oldest groups until `stack` is at most `limit` long. Zero means
/// unlimited.
fn trim_stack(stack: &mut Vec<Group>, limit: usize) {
    if limit == 0 {
        return;
    }
    while stack.len() > limit {
        stack.remove(0);
    }
}

fn post(receiver: Id, name: &str) {
    crate::notification::post(name, receiver, std::ptr::null_mut());
}

/// A fresh, non-autoreleased `NSUndoManager`, for this tree's own use -
/// `AppKit`'s `NSWindow` and `NSDocument` each own one lazily.
pub fn make() -> Id {
    boxed::make(manager_class(), Manager::default())
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of a class registered for `Manager`.
    unsafe { boxed::fill(receiver, Manager::default()) };
    receiver
}

/// Close the pending automatically-opened group if there is one - what a
/// `-canUndo`, `-canRedo`, explicit `-beginUndoGrouping`, or `-undo` does
/// before answering, per the class's documented interaction with
/// `-groupsByEvent`.
fn close_pending_auto_group(receiver: Id, state: &Mutex<Inner>) {
    let pending = {
        let inner = lock(state);
        inner.auto_opened && inner.open.len() == 1
    };
    if pending {
        // SAFETY: a live receiver whose state this function just read.
        unsafe { end_undo_grouping(receiver, objc::sel::get("endUndoGrouping")) };
    }
}

unsafe extern "C" fn begin_undo_grouping(receiver: Id, _sel: Sel) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    // An explicit grouping call starts a group of its own rather than
    // nesting inside one `groupsByEvent` opened on this manager's behalf.
    close_pending_auto_group(receiver, state);
    {
        let mut inner = lock(state);
        inner.open.push(Group::default());
    }
    // FIXME (ACCURACY): Apple skips this checkpoint while a top-level undo
    // is in progress; every open here posts it, which is one extra
    // notification during `-undo` a careful observer could notice.
    post(receiver, CHECKPOINT);
    post(receiver, DID_OPEN_GROUP);
}

unsafe extern "C" fn end_undo_grouping(receiver: Id, _sel: Sel) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    if lock(state).open.is_empty() {
        objc::fail!(
            "-[NSUndoManager endUndoGrouping]: endUndoGrouping called with no matching \
             beginUndoGrouping"
        );
    }
    post(receiver, CHECKPOINT);
    post(receiver, WILL_CLOSE_GROUP);
    {
        let mut inner = lock(state);
        let Some(mut group) = inner.open.pop() else {
            return;
        };
        match inner.open.last_mut() {
            // Still nested: the closing group's actions join its parent's,
            // so only the outermost close ever reaches a stack. Apple's
            // nested groups behave like nested transactions, not like a
            // tree of independently undoable pieces.
            Some(parent) => {
                if parent.action_name.is_empty() {
                    parent.action_name = std::mem::take(&mut group.action_name);
                }
                parent.actions.append(&mut group.actions);
            }
            None => {
                inner.auto_opened = false;
                let is_undoing = inner.is_undoing;
                let limit = inner.levels_of_undo;
                // Closed while undoing, the group just built is this
                // manager's redo of what it undid; otherwise it is a fresh
                // undo (which also covers closing while redoing - the
                // freshly redone action becomes undoable again).
                let destination = match is_undoing {
                    true => &mut inner.redo_stack,
                    false => &mut inner.undo_stack,
                };
                destination.push(group);
                trim_stack(destination, limit);
            }
        }
    }
    post(receiver, DID_CLOSE_GROUP);
}

/// Run every action a closed group holds, in the reverse of the order they
/// were registered, with a fresh group open to collect whatever they
/// register in turn - the inverse group this performs pushes onto the
/// opposite stack.
fn replay(state: &Mutex<Inner>, group: Group, redoing: bool) -> Group {
    {
        let mut inner = lock(state);
        match redoing {
            true => inner.is_redoing = true,
            false => inner.is_undoing = true,
        }
        inner.open.push(Group::default());
    }
    for action in group.actions.iter().rev() {
        // SAFETY: built by a registration this module validated when it was
        // made.
        unsafe { action.run() };
    }
    let mut collected = {
        let mut inner = lock(state);
        match redoing {
            true => inner.is_redoing = false,
            false => inner.is_undoing = false,
        }
        inner.open.pop().unwrap_or_default()
    };
    if collected.action_name.is_empty() {
        collected.action_name = group.action_name;
    }
    collected
}

unsafe extern "C" fn undo_nested_group(receiver: Id, _sel: Sel) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    if lock(state).disable_count > 0 {
        objc::fail!(
            "-[NSUndoManager undoNestedGroup]: undo registration is disabled, so the redo it \
             would record cannot be captured"
        );
    }
    post(receiver, CHECKPOINT);
    let group = lock(state).undo_stack.pop();
    let Some(group) = group else {
        return;
    };
    post(receiver, WILL_UNDO_CHANGE);
    let redo_group = replay(state, group, false);
    {
        let mut inner = lock(state);
        inner.redo_stack.push(redo_group);
        let limit = inner.levels_of_undo;
        trim_stack(&mut inner.redo_stack, limit);
    }
    post(receiver, DID_UNDO_CHANGE);
}

unsafe extern "C" fn undo(receiver: Id, _sel: Sel) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    let level = lock(state).open.len();
    if level > 1 {
        objc::fail!(
            "-[NSUndoManager undo]: cannot call undo when more than one undo group is open"
        );
    }
    post(receiver, CHECKPOINT);
    if level == 1 {
        // SAFETY: a live receiver.
        unsafe { end_undo_grouping(receiver, objc::sel::get("endUndoGrouping")) };
    }
    // SAFETY: as this method's own contract.
    unsafe { undo_nested_group(receiver, objc::sel::get("undoNestedGroup")) };
}

unsafe extern "C" fn redo(receiver: Id, _sel: Sel) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    post(receiver, CHECKPOINT);
    let group = lock(state).redo_stack.pop();
    let Some(group) = group else {
        return;
    };
    post(receiver, WILL_REDO_CHANGE);
    let undo_group = replay(state, group, true);
    {
        let mut inner = lock(state);
        inner.undo_stack.push(undo_group);
        let limit = inner.levels_of_undo;
        trim_stack(&mut inner.undo_stack, limit);
    }
    post(receiver, DID_REDO_CHANGE);
}

unsafe extern "C" fn can_undo(receiver: Id, _sel: Sel) -> u8 {
    let Some(state) = (unsafe { state(receiver) }) else {
        return 0;
    };
    close_pending_auto_group(receiver, state);
    u8::from(!lock(state).undo_stack.is_empty())
}

unsafe extern "C" fn can_redo(receiver: Id, _sel: Sel) -> u8 {
    let Some(state) = (unsafe { state(receiver) }) else {
        return 0;
    };
    u8::from(!lock(state).redo_stack.is_empty())
}

unsafe extern "C" fn is_undoing(receiver: Id, _sel: Sel) -> u8 {
    (unsafe { state(receiver) }).map_or(0, |s| u8::from(lock(s).is_undoing))
}

unsafe extern "C" fn is_redoing(receiver: Id, _sel: Sel) -> u8 {
    (unsafe { state(receiver) }).map_or(0, |s| u8::from(lock(s).is_redoing))
}

unsafe extern "C" fn grouping_level(receiver: Id, _sel: Sel) -> isize {
    (unsafe { state(receiver) }).map_or(0, |s| lock(s).open.len() as isize)
}

unsafe extern "C" fn groups_by_event(receiver: Id, _sel: Sel) -> u8 {
    (unsafe { state(receiver) }).map_or(1, |s| u8::from(lock(s).group_by_event))
}

unsafe extern "C" fn set_groups_by_event(receiver: Id, _sel: Sel, value: u8) {
    if let Some(s) = unsafe { state(receiver) } {
        lock(s).group_by_event = value != 0;
    }
}

unsafe extern "C" fn levels_of_undo(receiver: Id, _sel: Sel) -> usize {
    (unsafe { state(receiver) }).map_or(0, |s| lock(s).levels_of_undo)
}

unsafe extern "C" fn set_levels_of_undo(receiver: Id, _sel: Sel, value: usize) {
    let Some(s) = (unsafe { state(receiver) }) else {
        return;
    };
    let mut inner = lock(s);
    inner.levels_of_undo = value;
    trim_stack(&mut inner.undo_stack, value);
    trim_stack(&mut inner.redo_stack, value);
}

fn register_action(receiver: Id, action: Action) {
    // SAFETY: `receiver` is this method's own receiver.
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    let opened_now = {
        let mut inner = lock(state);
        if inner.disable_count > 0 {
            return;
        }
        let opened_now = inner.open.is_empty();
        if !inner.is_undoing && !inner.is_redoing {
            inner.redo_stack.clear();
        }
        if opened_now {
            inner.open.push(Group::default());
            inner.auto_opened = true;
        }
        inner
            .open
            .last_mut()
            .expect("just ensured a group is open")
            .actions
            .push(action);
        opened_now
    };
    if !opened_now {
        return;
    }
    // The checkpoint and "did open" notifications for the group this bare
    // registration just opened, posted outside the lock above like every
    // other method here posts - a registration behaves as if it were
    // preceded by an explicit `-beginUndoGrouping`.
    post(receiver, CHECKPOINT);
    post(receiver, DID_OPEN_GROUP);
    let schedule = {
        let mut inner = lock(state);
        let should = inner.group_by_event && !inner.close_scheduled;
        if should {
            inner.close_scheduled = true;
        }
        should
    };
    if schedule {
        schedule_auto_close(receiver);
    }
}

unsafe extern "C" fn register_with_selector(
    receiver: Id,
    _sel: Sel,
    target: Id,
    selector: Sel,
    object: Id,
) {
    register_action(
        receiver,
        Action::Selector {
            target,
            selector,
            object: objc::arc::retain(object),
        },
    );
}

unsafe extern "C" fn register_with_handler(receiver: Id, _sel: Sel, target: Id, handler: Id) {
    let block = objc::block::copy(handler) as usize;
    if block == 0 {
        return;
    }
    register_action(receiver, Action::Handler { target, block });
}

unsafe extern "C" fn remove_all_actions(receiver: Id, _sel: Sel) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    let mut inner = lock(state);
    inner.undo_stack.clear();
    inner.redo_stack.clear();
    inner.open.clear();
    inner.auto_opened = false;
}

/// Doesn't re-enable the manager if it is disabled - Apple's own note on the
/// method, and automatic here since this touches no counter.
unsafe extern "C" fn remove_all_actions_with_target(receiver: Id, _sel: Sel, target: Id) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    let mut inner = lock(state);
    // Destructured so the borrow checker sees three disjoint fields rather
    // than three borrows of `inner` through one chained expression.
    let Inner {
        undo_stack,
        redo_stack,
        open,
        ..
    } = &mut *inner;
    for group in undo_stack
        .iter_mut()
        .chain(redo_stack.iter_mut())
        .chain(open.iter_mut())
    {
        group.actions.retain(|action| !action.targets(target));
    }
}

unsafe extern "C" fn disable_undo_registration(receiver: Id, _sel: Sel) {
    if let Some(s) = unsafe { state(receiver) } {
        lock(s).disable_count += 1;
    }
}

unsafe extern "C" fn enable_undo_registration(receiver: Id, _sel: Sel) {
    if let Some(s) = unsafe { state(receiver) } {
        let mut inner = lock(s);
        inner.disable_count = inner.disable_count.saturating_sub(1);
    }
}

unsafe extern "C" fn is_undo_registration_enabled(receiver: Id, _sel: Sel) -> u8 {
    (unsafe { state(receiver) }).map_or(1, |s| u8::from(lock(s).disable_count == 0))
}

unsafe extern "C" fn set_action_name(receiver: Id, _sel: Sel, name: Id) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    let mut inner = lock(state);
    let Some(top) = inner.open.last_mut() else {
        // Apple's setter has nothing to name when no group is open; a no-op
        // rather than inventing a group to hang the name on.
        return;
    };
    top.action_name = match name.is_null() {
        true => String::new(),
        // SAFETY: a live string.
        false => String::from_utf8_lossy(&unsafe { crate::string::bytes(name) }).into_owned(),
    };
}

fn action_name_of(receiver: Id, undo: bool) -> Id {
    let name = match unsafe { state(receiver) } {
        // SAFETY: the receiver this function was given.
        Some(state) => {
            let inner = lock(state);
            let stack = match undo {
                true => &inner.undo_stack,
                false => &inner.redo_stack,
            };
            stack.last().map(|g| g.action_name.clone())
        }
        None => None,
    };
    objc::arc::autorelease(crate::string::from_str(&name.unwrap_or_default()))
}

unsafe extern "C" fn undo_action_name(receiver: Id, _sel: Sel) -> Id {
    action_name_of(receiver, true)
}

unsafe extern "C" fn redo_action_name(receiver: Id, _sel: Sel) -> Id {
    action_name_of(receiver, false)
}

/// `-undoMenuItemTitle` / `-redoMenuItemTitle`: "Undo"/"Redo", with the top
/// group's action name appended when it has one, or nil when there is
/// nothing on that stack to name at all.
fn menu_item_title(receiver: Id, undo: bool) -> Id {
    let Some(state) = (unsafe { state(receiver) }) else {
        return std::ptr::null_mut();
    };
    let name = {
        let inner = lock(state);
        let stack = match undo {
            true => &inner.undo_stack,
            false => &inner.redo_stack,
        };
        match stack.last() {
            Some(top) => top.action_name.clone(),
            None => return std::ptr::null_mut(),
        }
    };
    let verb = match undo {
        true => "Undo",
        false => "Redo",
    };
    let title = match name.is_empty() {
        true => verb.to_string(),
        false => format!("{verb} {name}"),
    };
    objc::arc::autorelease(crate::string::from_str(&title))
}

unsafe extern "C" fn undo_menu_item_title(receiver: Id, _sel: Sel) -> Id {
    menu_item_title(receiver, true)
}

unsafe extern "C" fn redo_menu_item_title(receiver: Id, _sel: Sel) -> Id {
    menu_item_title(receiver, false)
}

unsafe extern "C" fn run_loop_modes(receiver: Id, _sel: Sel) -> Id {
    let Some(state) = (unsafe { state(receiver) }) else {
        return std::ptr::null_mut();
    };
    let mut inner = lock(state);
    if inner.run_loop_modes.is_null() {
        // The default mode, as the one object every other caller of
        // `NSDefaultRunLoopMode` gets - see `corefoundation::base::interned_name`.
        let mode = corefoundation::base::interned_name(corefoundation::runloop::DEFAULT_MODE);
        inner.run_loop_modes = corefoundation::collections::new_array(
            &[mode as usize as corefoundation::base::CFTypeRef],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
    }
    inner.run_loop_modes
}

unsafe extern "C" fn set_run_loop_modes(receiver: Id, _sel: Sel, modes: Id) {
    let Some(state) = (unsafe { state(receiver) }) else {
        return;
    };
    let mut inner = lock(state);
    let old = std::mem::replace(&mut inner.run_loop_modes, objc::arc::retain(modes));
    objc::arc::release(old);
}

/// Close the automatically-opened top-level group at the end of this turn of
/// the run loop - what `groupsByEvent` promises.
///
/// FIXME (ACCURACY): Apple schedules this with
/// `-[NSRunLoop performSelector:target:argument:order:modes:]`, ordered by
/// `NSUndoCloseGroupingRunLoopOrdering` among whatever else that run asked
/// to be performed. This instead adds a one-shot `CFRunLoopObserver` for
/// `kCFRunLoopBeforeWaiting`/`kCFRunLoopExit` on the *calling* thread's run
/// loop (Apple's own is always the main one), which only fires if something
/// actually turns that loop - true of AppKit's own event pump, not of a bare
/// `NSUndoManager` used off the main thread with nothing driving its loop.
fn schedule_auto_close(receiver: Id) {
    // Kept alive until the callback below releases it, so a window or
    // document closed mid-turn cannot leave this pointing at freed memory.
    objc::arc::retain(receiver);
    let info = receiver as usize;
    let observer = corefoundation::runloop::new_observer(
        corefoundation::runloop::activity::BEFORE_WAITING | corefoundation::runloop::activity::EXIT,
        false,
        0,
        Some(close_auto_group),
        info,
    );
    corefoundation::runloop::current()
        .add_observer(&observer, corefoundation::runloop::COMMON_MODES);
}

unsafe extern "C" fn close_auto_group(
    _observer: corefoundation::base::CFTypeRef,
    _activity: u32,
    info: *mut std::ffi::c_void,
) {
    let receiver = info as Id;
    // SAFETY: `receiver` was retained by `schedule_auto_close` and is
    // released at the end of this callback, so it is still live here.
    if let Some(state) = unsafe { state(receiver) } {
        let should_close = {
            let mut inner = lock(state);
            inner.close_scheduled = false;
            inner.auto_opened && inner.open.len() == 1
        };
        if should_close {
            // SAFETY: `receiver` is live, as above.
            unsafe { end_undo_grouping(receiver, objc::sel::get("endUndoGrouping")) };
        }
    }
    objc::arc::release(receiver);
}

pub(crate) fn install() {
    let class = manager_class();
    methods!(class, {
        "init" => init, c"@16@0:8";
        "beginUndoGrouping" => begin_undo_grouping, c"v16@0:8";
        "endUndoGrouping" => end_undo_grouping, c"v16@0:8";
        "undo" => undo, c"v16@0:8";
        "redo" => redo, c"v16@0:8";
        "undoNestedGroup" => undo_nested_group, c"v16@0:8";
        "canUndo" => can_undo, c"B16@0:8";
        "canRedo" => can_redo, c"B16@0:8";
        "isUndoing" => is_undoing, c"B16@0:8";
        "isRedoing" => is_redoing, c"B16@0:8";
        "groupingLevel" => grouping_level, c"q16@0:8";
        "groupsByEvent" => groups_by_event, c"B16@0:8";
        "setGroupsByEvent:" => set_groups_by_event, c"v20@0:8B16";
        "levelsOfUndo" => levels_of_undo, c"Q16@0:8";
        "setLevelsOfUndo:" => set_levels_of_undo, c"v24@0:8Q16";
        "registerUndoWithTarget:selector:object:"
            => register_with_selector, c"v40@0:8@16:24@32";
        "registerUndoWithTarget:handler:" => register_with_handler, c"v32@0:8@16@?24";
        "removeAllActions" => remove_all_actions, c"v16@0:8";
        "removeAllActionsWithTarget:" => remove_all_actions_with_target, c"v24@0:8@16";
        "disableUndoRegistration" => disable_undo_registration, c"v16@0:8";
        "enableUndoRegistration" => enable_undo_registration, c"v16@0:8";
        "isUndoRegistrationEnabled" => is_undo_registration_enabled, c"B16@0:8";
        "setActionName:" => set_action_name, c"v24@0:8@16";
        "undoActionName" => undo_action_name, c"@16@0:8";
        "redoActionName" => redo_action_name, c"@16@0:8";
        "undoMenuItemTitle" => undo_menu_item_title, c"@16@0:8";
        "redoMenuItemTitle" => redo_menu_item_title, c"@16@0:8";
        "runLoopModes" => run_loop_modes, c"@16@0:8";
        "setRunLoopModes:" => set_run_loop_modes, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::block::fake;

    fn new_manager() -> Id {
        crate::bootstrap();
        let receiver = objc::object::create_instance(manager_class(), 0);
        // SAFETY: a fresh instance of this module's own class.
        unsafe { init(receiver, objc::sel::get("init")) };
        receiver
    }

    static UNDO_HITS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn record_hit(_receiver: Id, _sel: Sel, _object: Id) {
        UNDO_HITS.fetch_add(1, Ordering::SeqCst);
    }

    /// A target with one method, `-rosemaryUndo:`, that counts how often it
    /// is sent - for the selector-based registration.
    fn target() -> Id {
        let class = corefoundation::base::register(
            c"RosemaryUndoManagerTarget",
            None,
            size_of::<usize>(),
            false,
        );
        methods!(class, {
            "rosemaryUndo:" => record_hit, c"v24@0:8@16";
        });
        objc::object::create_instance(class, 0)
    }

    #[test]
    fn registering_with_no_open_group_opens_one_that_can_undo_closes() {
        let manager = new_manager();
        let who = target();
        UNDO_HITS.store(0, Ordering::SeqCst);
        unsafe {
            register_with_selector(
                manager,
                objc::sel::get("registerUndoWithTarget:selector:object:"),
                who,
                objc::sel::get("rosemaryUndo:"),
                std::ptr::null_mut(),
            );
        }
        assert_eq!(
            unsafe { grouping_level(manager, objc::sel::get("groupingLevel")) },
            1,
            "a bare registration opens a group"
        );
        assert_eq!(
            unsafe { can_undo(manager, objc::sel::get("canUndo")) },
            1,
            "canUndo closes the pending group first"
        );
        assert_eq!(
            unsafe { grouping_level(manager, objc::sel::get("groupingLevel")) },
            0,
            "canUndo left no group open"
        );
        unsafe { undo(manager, objc::sel::get("undo")) };
        assert_eq!(UNDO_HITS.load(Ordering::SeqCst), 1);
        assert_eq!(unsafe { can_undo(manager, objc::sel::get("canUndo")) }, 0);
        assert_eq!(unsafe { can_redo(manager, objc::sel::get("canRedo")) }, 1);
        objc::arc::release(manager);
        objc::arc::release(who);
    }

    static ORDER: StdMutex<Vec<usize>> = StdMutex::new(Vec::new());

    unsafe extern "C" fn record_order(_block: Id, target: Id) {
        ORDER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(target as usize);
    }

    /// Ending a nested group folds its actions into the parent, so only the
    /// outermost close ever reaches the undo stack - and undoing it runs
    /// every action in the reverse of registration order.
    #[test]
    fn nested_groups_merge_and_undo_runs_in_reverse() {
        let manager = new_manager();
        let block = fake::block(record_order as *const std::ffi::c_void);
        let begin = objc::sel::get("beginUndoGrouping");
        let end = objc::sel::get("endUndoGrouping");
        let register = objc::sel::get("registerUndoWithTarget:handler:");
        unsafe {
            begin_undo_grouping(manager, begin);
            register_with_handler(manager, register, 1usize as Id, block.id());
            begin_undo_grouping(manager, begin);
            register_with_handler(manager, register, 2usize as Id, block.id());
            end_undo_grouping(manager, end);
        }
        assert_eq!(
            unsafe { grouping_level(manager, objc::sel::get("groupingLevel")) },
            1,
            "closing the inner group leaves the outer one open"
        );
        unsafe {
            register_with_handler(manager, register, 3usize as Id, block.id());
            end_undo_grouping(manager, end);
        }
        assert_eq!(
            unsafe { grouping_level(manager, objc::sel::get("groupingLevel")) },
            0
        );
        ORDER.lock().unwrap_or_else(|e| e.into_inner()).clear();
        unsafe { undo(manager, objc::sel::get("undo")) };
        assert_eq!(
            *ORDER.lock().unwrap_or_else(|e| e.into_inner()),
            vec![3, 2, 1],
            "undo runs the most recently registered action first"
        );
        objc::arc::release(manager);
    }

    /// A registration made outside of undoing or redoing clears whatever was
    /// on the redo stack; one made while undoing does not - it is what
    /// builds the very redo stack entry this checks for.
    #[test]
    fn a_fresh_registration_clears_redo_but_undoing_does_not() {
        let manager = new_manager();
        let block = fake::block(record_order as *const std::ffi::c_void);
        let register = objc::sel::get("registerUndoWithTarget:handler:");
        unsafe {
            register_with_handler(manager, register, 1usize as Id, block.id());
            can_undo(manager, objc::sel::get("canUndo")); // close the pending group
            undo(manager, objc::sel::get("undo"));
        }
        assert_eq!(
            unsafe { can_redo(manager, objc::sel::get("canRedo")) },
            1,
            "undoing built a redo entry"
        );
        unsafe {
            register_with_handler(manager, register, 2usize as Id, block.id());
        }
        assert_eq!(
            unsafe { can_redo(manager, objc::sel::get("canRedo")) },
            0,
            "a fresh registration cleared the redo stack"
        );
        objc::arc::release(manager);
    }

    /// `levelsOfUndo` limits the undo stack to that many groups, dropping
    /// the oldest first.
    #[test]
    fn levels_of_undo_drops_the_oldest_group() {
        let manager = new_manager();
        let block = fake::block(record_order as *const std::ffi::c_void);
        let register = objc::sel::get("registerUndoWithTarget:handler:");
        unsafe {
            set_levels_of_undo(manager, objc::sel::get("setLevelsOfUndo:"), 2);
            for tag in [1usize, 2, 3] {
                register_with_handler(manager, register, tag as Id, block.id());
                can_undo(manager, objc::sel::get("canUndo")); // close each group
            }
        }
        let Some(state) = (unsafe { state(manager) }) else {
            panic!("a live manager");
        };
        assert_eq!(
            lock(state).undo_stack.len(),
            2,
            "the oldest group was dropped"
        );
        objc::arc::release(manager);
    }

    /// Registration disabled by `-disableUndoRegistration` neither records
    /// the action nor opens a group for it, and a matching
    /// `-enableUndoRegistration` is what turns it back on.
    #[test]
    fn disabling_registration_drops_it_until_reenabled() {
        let manager = new_manager();
        let block = fake::block(record_order as *const std::ffi::c_void);
        let register = objc::sel::get("registerUndoWithTarget:handler:");
        unsafe {
            disable_undo_registration(manager, objc::sel::get("disableUndoRegistration"));
            assert_eq!(
                is_undo_registration_enabled(manager, objc::sel::get("isUndoRegistrationEnabled")),
                0
            );
            register_with_handler(manager, register, 1usize as Id, block.id());
        }
        assert_eq!(
            unsafe { grouping_level(manager, objc::sel::get("groupingLevel")) },
            0,
            "a disabled registration opens nothing"
        );
        unsafe {
            enable_undo_registration(manager, objc::sel::get("enableUndoRegistration"));
            register_with_handler(manager, register, 2usize as Id, block.id());
        }
        assert_eq!(
            unsafe { grouping_level(manager, objc::sel::get("groupingLevel")) },
            1,
            "registration works again once re-enabled"
        );
        objc::arc::release(manager);
    }

    /// Removing every action for one target leaves another target's actions
    /// alone.
    #[test]
    fn remove_all_actions_with_target_only_removes_that_targets_actions() {
        let manager = new_manager();
        let block = fake::block(record_order as *const std::ffi::c_void);
        let register = objc::sel::get("registerUndoWithTarget:handler:");
        let kept = 9usize as Id;
        let removed = 1usize as Id;
        unsafe {
            begin_undo_grouping(manager, objc::sel::get("beginUndoGrouping"));
            register_with_handler(manager, register, removed, block.id());
            register_with_handler(manager, register, kept, block.id());
            remove_all_actions_with_target(
                manager,
                objc::sel::get("removeAllActionsWithTarget:"),
                removed,
            );
            end_undo_grouping(manager, objc::sel::get("endUndoGrouping"));
        }
        let Some(state) = (unsafe { state(manager) }) else {
            panic!("a live manager");
        };
        let remaining = lock(state).undo_stack.last().map(|g| g.actions.len());
        assert_eq!(remaining, Some(1), "only the other target's action is left");
        objc::arc::release(manager);
    }

    /// `-undoActionName` and `-undoMenuItemTitle` read the top of the undo
    /// stack, falling back to a plain "Undo" for an unnamed action and to
    /// nil when there is nothing to undo at all.
    #[test]
    fn menu_titles_reflect_the_top_of_the_undo_stack() {
        let manager = new_manager();
        assert!(
            unsafe { undo_menu_item_title(manager, objc::sel::get("undoMenuItemTitle")) }.is_null(),
            "nothing to undo yet"
        );
        let block = fake::block(record_order as *const std::ffi::c_void);
        let action_name = crate::string::from_str("Delete");
        unsafe {
            begin_undo_grouping(manager, objc::sel::get("beginUndoGrouping"));
            register_with_handler(
                manager,
                objc::sel::get("registerUndoWithTarget:handler:"),
                1usize as Id,
                block.id(),
            );
            set_action_name(manager, objc::sel::get("setActionName:"), action_name);
            end_undo_grouping(manager, objc::sel::get("endUndoGrouping"));
        }
        objc::arc::release(action_name);
        let title = unsafe { undo_menu_item_title(manager, objc::sel::get("undoMenuItemTitle")) };
        assert_eq!(unsafe { crate::string::bytes(title) }, b"Undo Delete");
        let name = unsafe { undo_action_name(manager, objc::sel::get("undoActionName")) };
        assert_eq!(unsafe { crate::string::bytes(name) }, b"Delete");
        objc::arc::release(manager);
    }

    /// Checkpoints and open/close notifications fire for both an explicit
    /// grouping and the implicit one a bare registration opens.
    #[test]
    fn grouping_posts_its_notifications() {
        crate::bootstrap();
        static HEARD: StdMutex<Vec<String>> = StdMutex::new(Vec::new());
        unsafe extern "C" fn record(_receiver: Id, _sel: Sel, notification: Id) {
            // SAFETY: a notification this crate posted.
            let name = unsafe { crate::notification::name_of(notification) };
            // SAFETY: a notification's name is a string.
            let text = unsafe { crate::string::bytes(name) };
            HEARD
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(String::from_utf8_lossy(&text).into_owned());
        }
        let observer_class = corefoundation::base::register(
            c"RosemaryUndoManagerNotificationObserver",
            None,
            size_of::<usize>(),
            false,
        );
        methods!(observer_class, {
            "heard:" => record, c"v24@0:8@16";
        });
        let who = objc::object::create_instance(observer_class, 0);
        // The manager this test's own notifications are filtered to - the
        // table of observers is one process-wide static, and other tests in
        // this file post the same names concurrently.
        let manager = new_manager();
        let center = crate::notification::default_center_object();
        let add = objc::sel::get("addObserver:selector:name:object:");
        for name in [
            CHECKPOINT,
            DID_OPEN_GROUP,
            WILL_CLOSE_GROUP,
            DID_CLOSE_GROUP,
        ] {
            let name_object = crate::string::from_str(name);
            // SAFETY: `-addObserver:selector:name:object:` is
            // `void (id, SEL, id, SEL, id, id)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id, Sel, Id, Id) =
                    std::mem::transmute(objc::msg::entry());
                f(
                    center,
                    add,
                    who,
                    objc::sel::get("heard:"),
                    name_object,
                    manager,
                );
            }
            objc::arc::release(name_object);
        }
        HEARD.lock().unwrap_or_else(|e| e.into_inner()).clear();

        unsafe {
            begin_undo_grouping(manager, objc::sel::get("beginUndoGrouping"));
            end_undo_grouping(manager, objc::sel::get("endUndoGrouping"));
        }
        assert_eq!(
            *HEARD.lock().unwrap_or_else(|e| e.into_inner()),
            vec![
                CHECKPOINT.to_string(),
                DID_OPEN_GROUP.to_string(),
                CHECKPOINT.to_string(),
                WILL_CLOSE_GROUP.to_string(),
                DID_CLOSE_GROUP.to_string(),
            ]
        );
        // SAFETY: `-removeObserver:` is `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(center, objc::sel::get("removeObserver:"), who);
        }
        objc::arc::release(manager);
        objc::arc::release(who);
    }
}
