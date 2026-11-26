//! `NSObjectController`, `NSTreeController`, and the `NSTreeNode`s a tree
//! controller arranges its content into.
//!
//! A tree controller's arranged objects are a root node that represents
//! nothing, whose children represent the content's objects. Each node's
//! children are its object's children key path, sorted by the controller's
//! sort descriptors. They are made when first asked for, and made again when
//! the object announces that key changed.

use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

pub(crate) const OBJECT_CONTROLLER_INSTANCE_SIZE: usize = size_of::<ObjectController>();
pub(crate) const TREE_CONTROLLER_INSTANCE_SIZE: usize = size_of::<TreeController>();
pub(crate) const TREE_NODE_INSTANCE_SIZE: usize = size_of::<TreeNode>();

#[repr(C)]
struct ObjectController {
    isa: *const c_void,
    /// Retained.
    content: Id,
    /// A class, which needs no reference.
    object_class: Id,
    editable: u8,
    automatically_prepares_content: u8,
}

#[repr(C)]
struct TreeController {
    base: ObjectController,
    /// Retained strings, or nil.
    children_key_path: Id,
    count_key_path: Id,
    leaf_key_path: Id,
    /// Retained array of `NSSortDescriptor`, or nil.
    sort_descriptors: Id,
    /// Retained array of `NSIndexPath`, or nil for no selection.
    selection: Id,
    /// The arranged objects: retained, made when first asked for.
    root: Id,
    avoids_empty_selection: u8,
    preserves_selection: u8,
    selects_inserted_objects: u8,
}

#[repr(C)]
struct TreeNode {
    isa: *const c_void,
    /// Retained.
    represented: Id,
    /// Unretained: a parent holds its children.
    parent: Id,
    /// Retained array of child nodes, or nil until made.
    children: Id,
    /// Unretained, and nil for a node made on its own: the controller whose
    /// content this node's children come from.
    controller: Id,
    /// Retained key path this node observes its object's children through, or nil.
    observing: Id,
}

/// The context a node observes its object's children with.
static CHILDREN_CHANGED: u8 = 0;

fn children_changed_context() -> usize {
    &CHILDREN_CHANGED as *const u8 as usize
}

fn object(receiver: Id) -> &'static mut ObjectController {
    // SAFETY: an `NSObjectController`, laid out as this module allocates it.
    unsafe { &mut *(receiver as *mut ObjectController) }
}

fn tree(receiver: Id) -> &'static mut TreeController {
    // SAFETY: an `NSTreeController`, laid out as this module allocates it.
    unsafe { &mut *(receiver as *mut TreeController) }
}

fn node(receiver: Id) -> &'static mut TreeNode {
    // SAFETY: an `NSTreeNode`, laid out as this module allocates it.
    unsafe { &mut *(receiver as *mut TreeNode) }
}

fn send_id(receiver: Id, sel: &str) -> Id {
    // SAFETY: `id (id, SEL)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

fn send_id_id(receiver: Id, sel: &str, argument: Id) -> Id {
    // SAFETY: `id (id, SEL, id)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

fn send_word(receiver: Id, sel: &str) -> usize {
    // SAFETY: `NSInteger`, `NSUInteger` or `BOOL (id, SEL)`, as each selector
    // this is used with is; a `BOOL` is read from its low byte.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

/// `-willChangeValueForKey:` or `-didChangeValueForKey:`.
fn announce(receiver: Id, sel: &str, key: &str) {
    let key = foundation::string::from_str(key);
    // SAFETY: `void (id, SEL, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), key);
    }
    objc::arc::release(key);
}

/// Replace a retained slot's object with `value`, retained.
fn replace(slot: &mut Id, value: Id) {
    let old = std::mem::replace(slot, objc::arc::retain(value));
    objc::arc::release(old);
}

/// Replace a retained slot's object with a copy of `value`.
fn replace_with_copy(slot: &mut Id, value: Id) {
    let copy = match value.is_null() {
        true => value,
        false => send_id(value, "copy"),
    };
    objc::arc::release(std::mem::replace(slot, copy));
}

/// A new, autoreleased `NSArray` of `objects`.
fn array(objects: &[Id]) -> Id {
    let values: Vec<_> = objects
        .iter()
        .map(|o| *o as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id;
    objc::arc::autorelease(array)
}

/// The elements of an array, none for nil, or a failure naming `what` for
/// anything else.
fn elements(value: Id, what: &dyn Fn() -> String) -> Vec<Id> {
    if value.is_null() {
        return Vec::new();
    }
    // SAFETY: a live object.
    match unsafe { foundation::elements_of(value) } {
        Some(values) => values.into_iter().map(|v| v as Id).collect(),
        None => {
            // SAFETY: a live object.
            let class = unsafe { objc::object::class_of(value) }.map_or("?", |c| c.name_str());
            objc::fail!("{}: a {class}, which is not an array", what());
        }
    }
}

fn class_named(name: &std::ffi::CStr) -> Id {
    objc::class::ptr(objc::class::lookup(name).expect("registered before AppKit")) as Id
}

// NSObjectController.

unsafe extern "C" fn object_init(receiver: Id, _sel: Sel) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    let controller = object(receiver);
    controller.editable = 1;
    controller.object_class = class_named(c"NSMutableDictionary");
    receiver
}

unsafe extern "C" fn init_with_content(receiver: Id, _sel: Sel, content: Id) -> Id {
    let receiver = send_id(receiver, "init");
    if !receiver.is_null() {
        send_id_id(receiver, "setContent:", content);
    }
    receiver
}

unsafe extern "C" fn object_dealloc(receiver: Id, _sel: Sel) {
    objc::arc::release(object(receiver).content);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn content(receiver: Id, _sel: Sel) -> Id {
    object(receiver).content
}

unsafe extern "C" fn object_set_content(receiver: Id, _sel: Sel, content: Id) {
    replace(&mut object(receiver).content, content);
}

unsafe extern "C" fn object_class(receiver: Id, _sel: Sel) -> Id {
    object(receiver).object_class
}

unsafe extern "C" fn set_object_class(receiver: Id, _sel: Sel, class: Id) {
    object(receiver).object_class = class;
}

unsafe extern "C" fn is_editable(receiver: Id, _sel: Sel) -> u8 {
    object(receiver).editable
}

unsafe extern "C" fn set_editable(receiver: Id, _sel: Sel, flag: u8) {
    object(receiver).editable = (flag != 0) as u8;
}

unsafe extern "C" fn automatically_prepares_content(receiver: Id, _sel: Sel) -> u8 {
    object(receiver).automatically_prepares_content
}

/// FIXME (ACCURACY): recorded only; a controller never makes its own content.
unsafe extern "C" fn set_automatically_prepares_content(receiver: Id, _sel: Sel, flag: u8) {
    object(receiver).automatically_prepares_content = (flag != 0) as u8;
}

/// `-selectedObjects` of an object controller: its content, if it has one.
unsafe extern "C" fn object_selected_objects(receiver: Id, _sel: Sel) -> Id {
    match object(receiver).content {
        content if content.is_null() => array(&[]),
        content => array(&[content]),
    }
}

// NSTreeNode.

/// A new node, retained.
fn new_node(class: &std::ffi::CStr, represented: Id, parent: Id, controller: Id) -> Id {
    let class = objc::class::lookup(class).expect("registered by install");
    let created = objc::object::create_instance(class, 0);
    let state = node(created);
    state.represented = objc::arc::retain(represented);
    state.parent = parent;
    state.controller = controller;
    created
}

unsafe extern "C" fn tree_node_with_represented_object(class: Id, _sel: Sel, object: Id) -> Id {
    let created = send_id(class, "alloc");
    objc::arc::autorelease(send_id_id(created, "initWithRepresentedObject:", object))
}

unsafe extern "C" fn init_with_represented_object(receiver: Id, _sel: Sel, object: Id) -> Id {
    if !receiver.is_null() {
        replace(&mut node(receiver).represented, object);
    }
    receiver
}

/// Drop a node's children and its observation of its object's children, to
/// be made again when next asked for.
fn forget_children(receiver: Id) {
    let state = node(receiver);
    let children = std::mem::replace(&mut state.children, std::ptr::null_mut());
    for child in elements(children, &|| String::from("a node's children")) {
        detach(child);
        node(child).parent = std::ptr::null_mut();
    }
    objc::arc::release(children);
    let observing = std::mem::replace(&mut state.observing, std::ptr::null_mut());
    if !observing.is_null() {
        // SAFETY: `void (id, SEL, NSObject *, NSString *, void *)`, removing
        // the registration `child_nodes_of` made.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, usize) =
                std::mem::transmute(objc::msg::entry());
            f(
                state.represented,
                objc::sel::get("removeObserver:forKeyPath:context:"),
                receiver,
                observing,
                children_changed_context(),
            );
        }
        objc::arc::release(observing);
    }
}

/// Stop a node and everything below it following the controller, whose
/// content it may no longer be part of.
fn detach(receiver: Id) {
    forget_children(receiver);
    node(receiver).controller = std::ptr::null_mut();
}

unsafe extern "C" fn node_dealloc(receiver: Id, _sel: Sel) {
    detach(receiver);
    objc::arc::release(node(receiver).represented);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn represented_object(receiver: Id, _sel: Sel) -> Id {
    node(receiver).represented
}

unsafe extern "C" fn parent_node(receiver: Id, _sel: Sel) -> Id {
    node(receiver).parent
}

/// The key path the controller names for `receiver` through `selector`, so a
/// subclass's per-node override is honoured.
fn key_path_for(controller: Id, selector: &str, receiver: Id) -> Id {
    send_id_id(controller, selector, receiver)
}

fn value_at(object: Id, key_path: Id) -> Id {
    send_id_id(object, "valueForKeyPath:", key_path)
}

fn is_root(receiver: Id) -> bool {
    let state = node(receiver);
    !state.controller.is_null() && state.parent.is_null()
}

/// The objects a controller's node has as children, sorted, and the key path
/// they came through, which the node then observes.
fn model_children(receiver: Id) -> (Vec<Id>, Id) {
    let state = node(receiver);
    let controller = state.controller;
    let (value, key_path) = match is_root(receiver) {
        true => {
            let content = object(controller).content;
            // SAFETY: a live object or nil.
            let is_array = !content.is_null()
                && unsafe {
                    corefoundation::base::is_kind_of(
                        content.cast(),
                        objc::class::lookup(c"NSArray").expect("NSArray"),
                    )
                };
            match content.is_null() || is_array {
                true => (content, std::ptr::null_mut()),
                false => (array(&[content]), std::ptr::null_mut()),
            }
        }
        // A leaf, by the leaf key path, is not asked for children at all.
        false if is_leaf_by_key(receiver) == Some(true) => {
            return (Vec::new(), std::ptr::null_mut());
        }
        false => {
            let key_path = key_path_for(controller, "childrenKeyPathForNode:", receiver);
            if key_path.is_null() {
                return (Vec::new(), std::ptr::null_mut());
            }
            (value_at(state.represented, key_path), key_path)
        }
    };
    let descriptors = tree(controller).sort_descriptors;
    let value =
        match value.is_null() || descriptors.is_null() || send_word(descriptors, "count") == 0 {
            true => value,
            false => send_id_id(value, "sortedArrayUsingDescriptors:", descriptors),
        };
    let what = || {
        // SAFETY: a live object or nil.
        let class =
            unsafe { objc::object::class_of(state.represented) }.map_or("nil", |c| c.name_str());
        format!("-[NSTreeController arrangedObjects]: the children of a {class}")
    };
    (elements(value, &what), key_path)
}

/// A node's children, made from its controller's content the first time.
fn child_nodes_of(receiver: Id) -> Id {
    let state = node(receiver);
    if !state.children.is_null() {
        return state.children;
    }
    if state.controller.is_null() {
        state.children = objc::arc::retain(array(&[]));
        return state.children;
    }
    let (objects, key_path) = model_children(receiver);
    let made: Vec<Id> = objects
        .iter()
        .map(|o| new_node(c"NSTreeControllerTreeNode", *o, receiver, state.controller))
        .collect();
    state.children = objc::arc::retain(array(&made));
    made.into_iter().for_each(objc::arc::release);
    if !key_path.is_null() {
        state.observing = send_id(key_path, "copy");
        // SAFETY: `void (id, SEL, NSObject *, NSString *, NSKeyValueObservingOptions, void *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, usize, usize) =
                std::mem::transmute(objc::msg::entry());
            f(
                state.represented,
                objc::sel::get("addObserver:forKeyPath:options:context:"),
                receiver,
                state.observing,
                0,
                children_changed_context(),
            );
        }
    }
    state.children
}

unsafe extern "C" fn child_nodes(receiver: Id, _sel: Sel) -> Id {
    child_nodes_of(receiver)
}

/// What the leaf or count key path says about a controller's node, if the
/// controller has either.
fn is_leaf_by_key(receiver: Id) -> Option<bool> {
    let state = node(receiver);
    let leaf = key_path_for(state.controller, "leafKeyPathForNode:", receiver);
    if !leaf.is_null() {
        let value = value_at(state.represented, leaf);
        return Some(!value.is_null() && send_word(value, "boolValue") as u8 != 0);
    }
    let count = key_path_for(state.controller, "countKeyPathForNode:", receiver);
    if !count.is_null() {
        let value = value_at(state.represented, count);
        return Some(value.is_null() || send_word(value, "integerValue") == 0);
    }
    None
}

unsafe extern "C" fn is_leaf(receiver: Id, _sel: Sel) -> u8 {
    if node(receiver).controller.is_null() || is_root(receiver) {
        return (send_word(child_nodes_of(receiver), "count") == 0) as u8;
    }
    let leaf = is_leaf_by_key(receiver)
        .unwrap_or_else(|| send_word(child_nodes_of(receiver), "count") == 0);
    leaf as u8
}

/// The indexes from the top of the tree down to `receiver`.
fn path_of(receiver: Id) -> Vec<usize> {
    let mut path = Vec::new();
    let mut current = receiver;
    loop {
        let parent = node(current).parent;
        if parent.is_null() {
            break;
        }
        let siblings = elements(child_nodes_of(parent), &|| {
            String::from("a node's children")
        });
        let Some(index) = siblings.iter().position(|s| *s == current) else {
            break;
        };
        path.push(index);
        current = parent;
    }
    path.reverse();
    path
}

unsafe extern "C" fn index_path(receiver: Id, _sel: Sel) -> Id {
    foundation::indexpath::from_indexes(&path_of(receiver))
}

/// The node `path` leads to from `receiver`, if there is one.
fn descendant(receiver: Id, path: &[usize]) -> Option<Id> {
    path.iter().try_fold(receiver, |current, &index| {
        elements(child_nodes_of(current), &|| {
            String::from("a node's children")
        })
        .get(index)
        .copied()
    })
}

unsafe extern "C" fn descendant_node_at_index_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    descendant(receiver, foundation::indexpath::indexes(path)).unwrap_or(std::ptr::null_mut())
}

/// A controller's node hears that its object's children changed.
unsafe extern "C" fn node_observe(
    receiver: Id,
    _sel: Sel,
    _key_path: Id,
    _object: Id,
    _change: Id,
    context: usize,
) {
    let controller = node(receiver).controller;
    if context != children_changed_context() || controller.is_null() {
        return;
    }
    rearrange(controller, || forget_children(receiver));
}

// NSTreeController.

unsafe extern "C" fn tree_init(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the object controller's `-init`, on the same receiver.
    let receiver = unsafe { object_init(receiver, sel) };
    if receiver.is_null() {
        return receiver;
    }
    let controller = tree(receiver);
    controller.avoids_empty_selection = 1;
    controller.preserves_selection = 1;
    controller.selects_inserted_objects = 1;
    receiver
}

unsafe extern "C" fn tree_dealloc(receiver: Id, sel: Sel) {
    let controller = tree(receiver);
    let root = std::mem::replace(&mut controller.root, std::ptr::null_mut());
    if !root.is_null() {
        detach(root);
        objc::arc::release(root);
    }
    for field in [
        controller.children_key_path,
        controller.count_key_path,
        controller.leaf_key_path,
        controller.sort_descriptors,
        controller.selection,
    ] {
        objc::arc::release(field);
    }
    // SAFETY: the object controller's part, which finishes the chain.
    unsafe { object_dealloc(receiver, sel) };
}

/// The represented objects of the selected nodes, retained.
fn retained_selected_objects(receiver: Id) -> Vec<Id> {
    selected_nodes_of(receiver)
        .into_iter()
        .map(|n| objc::arc::retain(node(n).represented))
        .collect()
}

fn selection_of(receiver: Id) -> Vec<Vec<usize>> {
    elements(tree(receiver).selection, &|| String::from("a selection"))
        .into_iter()
        .map(|path| foundation::indexpath::indexes(path).to_vec())
        .collect()
}

fn selected_nodes_of(receiver: Id) -> Vec<Id> {
    let root = arranged_root(receiver);
    selection_of(receiver)
        .iter()
        .filter_map(|path| descendant(root, path))
        .collect()
}

/// Where `wanted` is in the tree under `from`, looked for depth first.
fn find(from: Id, wanted: Id, prefix: &mut Vec<usize>) -> bool {
    let children = elements(child_nodes_of(from), &|| String::from("a node's children"));
    for (index, child) in children.into_iter().enumerate() {
        prefix.push(index);
        if node(child).represented == wanted || find(child, wanted, prefix) {
            return true;
        }
        prefix.pop();
    }
    false
}

/// Change what the controller arranges, announcing it as a change of
/// `arrangedObjects`, then choose the selection again.
fn rearrange(receiver: Id, change: impl FnOnce()) {
    let previously = retained_selected_objects(receiver);
    let old_paths = selection_of(receiver);
    announce(receiver, "willChangeValueForKey:", "arrangedObjects");
    change();
    announce(receiver, "didChangeValueForKey:", "arrangedObjects");

    let root = arranged_root(receiver);
    let controller = tree(receiver);
    let mut paths: Vec<Vec<usize>> = match controller.preserves_selection != 0 {
        true => previously
            .iter()
            .filter_map(|wanted| {
                let mut path = Vec::new();
                find(root, *wanted, &mut path).then_some(path)
            })
            .collect(),
        // FIXME (ACCURACY): without preserving the selected objects, the paths
        // that still lead somewhere are kept; inferred, not observed.
        false => old_paths
            .iter()
            .filter(|path| descendant(root, path).is_some())
            .cloned()
            .collect(),
    };
    previously.into_iter().for_each(objc::arc::release);
    if paths.is_empty()
        && controller.avoids_empty_selection != 0
        && send_word(child_nodes_of(root), "count") != 0
    {
        paths.push(vec![0]);
    }
    if paths != old_paths {
        set_selection(receiver, &paths);
    }
}

/// `-setSelectionIndexPaths:`, sent so its observers are told.
fn set_selection(receiver: Id, paths: &[Vec<usize>]) {
    let paths: Vec<Id> = paths
        .iter()
        .map(|p| foundation::indexpath::from_indexes(p))
        .collect();
    send_id_id(receiver, "setSelectionIndexPaths:", array(&paths));
}

fn arranged_root(receiver: Id) -> Id {
    let controller = tree(receiver);
    if controller.root.is_null() {
        controller.root = new_node(
            c"NSTreeControllerTreeNode",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            receiver,
        );
    }
    controller.root
}

/// Drop the arranged tree, to be made again from the content when next asked for.
fn drop_root(receiver: Id) {
    let root = std::mem::replace(&mut tree(receiver).root, std::ptr::null_mut());
    if !root.is_null() {
        detach(root);
        objc::arc::release(root);
    }
}

unsafe extern "C" fn tree_set_content(receiver: Id, _sel: Sel, content: Id) {
    rearrange(receiver, || {
        replace(&mut object(receiver).content, content);
        drop_root(receiver);
    });
}

unsafe extern "C" fn rearrange_objects(receiver: Id, _sel: Sel) {
    rearrange(receiver, || drop_root(receiver));
}

unsafe extern "C" fn arranged_objects(receiver: Id, _sel: Sel) -> Id {
    arranged_root(receiver)
}

unsafe extern "C" fn sort_descriptors(receiver: Id, _sel: Sel) -> Id {
    match tree(receiver).sort_descriptors {
        descriptors if descriptors.is_null() => array(&[]),
        descriptors => descriptors,
    }
}

unsafe extern "C" fn set_sort_descriptors(receiver: Id, _sel: Sel, descriptors: Id) {
    rearrange(receiver, || {
        replace_with_copy(&mut tree(receiver).sort_descriptors, descriptors);
        drop_root(receiver);
    });
}

unsafe extern "C" fn selection_index_paths(receiver: Id, _sel: Sel) -> Id {
    match tree(receiver).selection {
        selection if selection.is_null() => array(&[]),
        selection => selection,
    }
}

/// `-setSelectionIndexPaths:`, which answers whether the selection changed
/// to what was asked for; it always does here.
unsafe extern "C" fn set_selection_index_paths(receiver: Id, _sel: Sel, paths: Id) -> u8 {
    replace_with_copy(&mut tree(receiver).selection, paths);
    1
}

unsafe extern "C" fn selection_index_path(receiver: Id, _sel: Sel) -> Id {
    elements(tree(receiver).selection, &|| String::from("a selection"))
        .first()
        .copied()
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn set_selection_index_path(receiver: Id, _sel: Sel, path: Id) -> u8 {
    let paths = match path.is_null() {
        true => array(&[]),
        false => array(&[path]),
    };
    send_word_id(receiver, "setSelectionIndexPaths:", paths)
}

fn send_word_id(receiver: Id, sel: &str, argument: Id) -> u8 {
    // SAFETY: `BOOL (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

/// The current selection with `paths` added or, when `adding` is false, taken out.
fn change_selection(receiver: Id, paths: Id, adding: bool) -> u8 {
    let mut selection = selection_of(receiver);
    let given: Vec<Vec<usize>> = elements(paths, &|| String::from("a selection"))
        .into_iter()
        .map(|p| foundation::indexpath::indexes(p).to_vec())
        .collect();
    match adding {
        true => {
            given.into_iter().for_each(|p| {
                if !selection.contains(&p) {
                    selection.push(p);
                }
            });
            selection.sort();
        }
        false => selection.retain(|p| !given.contains(p)),
    }
    let paths: Vec<Id> = selection
        .iter()
        .map(|p| foundation::indexpath::from_indexes(p))
        .collect();
    send_word_id(receiver, "setSelectionIndexPaths:", array(&paths))
}

unsafe extern "C" fn add_selection_index_paths(receiver: Id, _sel: Sel, paths: Id) -> u8 {
    change_selection(receiver, paths, true)
}

unsafe extern "C" fn remove_selection_index_paths(receiver: Id, _sel: Sel, paths: Id) -> u8 {
    change_selection(receiver, paths, false)
}

unsafe extern "C" fn selected_nodes(receiver: Id, _sel: Sel) -> Id {
    array(&selected_nodes_of(receiver))
}

unsafe extern "C" fn tree_selected_objects(receiver: Id, _sel: Sel) -> Id {
    let objects: Vec<Id> = selected_nodes_of(receiver)
        .into_iter()
        .map(|n| node(n).represented)
        .collect();
    array(&objects)
}

/// `+keyPathsForValuesAffecting...` for what the selection paths decide.
unsafe extern "C" fn affected_by_selection(_class: Id, _sel: Sel) -> Id {
    let key = foundation::string::from_str("selectionIndexPaths");
    let set = send_id_id(class_named(c"NSSet"), "setWithObject:", key);
    objc::arc::release(key);
    set
}

macro_rules! key_path_accessors {
    ($($field:ident, $getter:ident, $setter:ident, $for_node:ident;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> Id {
            tree(receiver).$field
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, key_path: Id) {
            replace_with_copy(&mut tree(receiver).$field, key_path);
        }
        unsafe extern "C" fn $for_node(receiver: Id, _sel: Sel, _node: Id) -> Id {
            tree(receiver).$field
        }
    )*};
}

key_path_accessors! {
    children_key_path, children_key_path, set_children_key_path, children_key_path_for_node;
    count_key_path, count_key_path, set_count_key_path, count_key_path_for_node;
    leaf_key_path, leaf_key_path, set_leaf_key_path, leaf_key_path_for_node;
}

macro_rules! flag_accessors {
    ($($field:ident, $getter:ident, $setter:ident;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> u8 {
            tree(receiver).$field
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, flag: u8) {
            tree(receiver).$field = (flag != 0) as u8;
        }
    )*};
}

flag_accessors! {
    avoids_empty_selection, avoids_empty_selection, set_avoids_empty_selection;
    preserves_selection, preserves_selection, set_preserves_selection;
    selects_inserted_objects, selects_inserted_objects, set_selects_inserted_objects;
}

pub(crate) fn install() {
    let object_controller =
        objc::class::lookup(c"NSObjectController").expect("declared before its methods");
    methods!(object_controller, {
        "init" => object_init, c"@16@0:8";
        "initWithContent:" => init_with_content, c"@24@0:8@16";
        "dealloc" => object_dealloc, c"v16@0:8";
        "content" => content, c"@16@0:8";
        "setContent:" => object_set_content, c"v24@0:8@16";
        "objectClass" => object_class, c"#16@0:8";
        "setObjectClass:" => set_object_class, c"v24@0:8#16";
        "isEditable" => is_editable, c"c16@0:8";
        "setEditable:" => set_editable, c"v20@0:8c16";
        "automaticallyPreparesContent" => automatically_prepares_content, c"c16@0:8";
        "setAutomaticallyPreparesContent:" => set_automatically_prepares_content, c"v20@0:8c16";
        "selectedObjects" => object_selected_objects, c"@16@0:8";
    });

    let node_class = objc::class::lookup(c"NSTreeNode").expect("declared before its methods");
    class_methods!(node_class, {
        "treeNodeWithRepresentedObject:" => tree_node_with_represented_object, c"@24@0:8@16";
    });
    methods!(node_class, {
        "initWithRepresentedObject:" => init_with_represented_object, c"@24@0:8@16";
        "dealloc" => node_dealloc, c"v16@0:8";
        "representedObject" => represented_object, c"@16@0:8";
        "parentNode" => parent_node, c"@16@0:8";
        "childNodes" => child_nodes, c"@16@0:8";
        "isLeaf" => is_leaf, c"c16@0:8";
        "indexPath" => index_path, c"@16@0:8";
        "descendantNodeAtIndexPath:" => descendant_node_at_index_path, c"@24@0:8@16";
    });
    // AppKit's own class for the nodes a tree controller makes.
    let controller_node =
        corefoundation::base::register(c"NSTreeControllerTreeNode", Some(node_class), 0, false);
    methods!(controller_node, {
        "observeValueForKeyPath:ofObject:change:context:" => node_observe,
        c"v48@0:8@16@24@32^v40";
    });

    let tree_controller =
        objc::class::lookup(c"NSTreeController").expect("declared before its methods");
    methods!(tree_controller, {
        "init" => tree_init, c"@16@0:8";
        "dealloc" => tree_dealloc, c"v16@0:8";
        "setContent:" => tree_set_content, c"v24@0:8@16";
        "arrangedObjects" => arranged_objects, c"@16@0:8";
        "rearrangeObjects" => rearrange_objects, c"v16@0:8";
        "sortDescriptors" => sort_descriptors, c"@16@0:8";
        "setSortDescriptors:" => set_sort_descriptors, c"v24@0:8@16";
        "selectionIndexPaths" => selection_index_paths, c"@16@0:8";
        "setSelectionIndexPaths:" => set_selection_index_paths, c"c24@0:8@16";
        "selectionIndexPath" => selection_index_path, c"@16@0:8";
        "setSelectionIndexPath:" => set_selection_index_path, c"c24@0:8@16";
        "addSelectionIndexPaths:" => add_selection_index_paths, c"c24@0:8@16";
        "removeSelectionIndexPaths:" => remove_selection_index_paths, c"c24@0:8@16";
        "selectedNodes" => selected_nodes, c"@16@0:8";
        "selectedObjects" => tree_selected_objects, c"@16@0:8";
        "childrenKeyPath" => children_key_path, c"@16@0:8";
        "setChildrenKeyPath:" => set_children_key_path, c"v24@0:8@16";
        "childrenKeyPathForNode:" => children_key_path_for_node, c"@24@0:8@16";
        "countKeyPath" => count_key_path, c"@16@0:8";
        "setCountKeyPath:" => set_count_key_path, c"v24@0:8@16";
        "countKeyPathForNode:" => count_key_path_for_node, c"@24@0:8@16";
        "leafKeyPath" => leaf_key_path, c"@16@0:8";
        "setLeafKeyPath:" => set_leaf_key_path, c"v24@0:8@16";
        "leafKeyPathForNode:" => leaf_key_path_for_node, c"@24@0:8@16";
        "avoidsEmptySelection" => avoids_empty_selection, c"c16@0:8";
        "setAvoidsEmptySelection:" => set_avoids_empty_selection, c"v20@0:8c16";
        "preservesSelection" => preserves_selection, c"c16@0:8";
        "setPreservesSelection:" => set_preserves_selection, c"v20@0:8c16";
        "selectsInsertedObjects" => selects_inserted_objects, c"c16@0:8";
        "setSelectsInsertedObjects:" => set_selects_inserted_objects, c"v20@0:8c16";
    });
    class_methods!(tree_controller, {
        "keyPathsForValuesAffectingSelectionIndexPath" => affected_by_selection, c"@16@0:8";
        "keyPathsForValuesAffectingSelectedNodes" => affected_by_selection, c"@16@0:8";
        "keyPathsForValuesAffectingSelectedObjects" => affected_by_selection, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(text: &str) -> Id {
        objc::arc::autorelease(foundation::string::from_str(text))
    }

    /// A mutable dictionary named `name` whose `children` are `children`.
    fn item(name: &str, children: &[Id]) -> Id {
        crate::bootstrap();
        let item = send_id(class_named(c"NSMutableDictionary"), "dictionary");
        set(item, "name", string(name));
        set(item, "children", array(children));
        item
    }

    fn set(object: Id, key: &str, value: Id) {
        // SAFETY: `void (id, SEL, id, NSString *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
            f(
                object,
                objc::sel::get("setValue:forKey:"),
                value,
                string(key),
            );
        }
    }

    fn name_at(controller: Id, path: &[usize]) -> String {
        let root = send_id(controller, "arrangedObjects");
        let node = send_id_id(
            root,
            "descendantNodeAtIndexPath:",
            foundation::indexpath::from_indexes(path),
        );
        let name = value_at(send_id(node, "representedObject"), string("name"));
        // SAFETY: a string.
        String::from_utf8_lossy(&unsafe { foundation::string::bytes(name) }).into_owned()
    }

    fn controller_of(content: &[Id]) -> Id {
        crate::bootstrap();
        let controller = send_id(send_id(class_named(c"NSTreeController"), "alloc"), "init");
        send_id_id(controller, "setChildrenKeyPath:", string("children"));
        send_id_id(controller, "setContent:", array(content));
        controller
    }

    #[test]
    fn arranged_objects_follow_the_children_key_path() {
        let content = [
            item("a", &[item("a0", &[]), item("a1", &[])]),
            item("b", &[]),
        ];
        let controller = controller_of(&content);
        let root = send_id(controller, "arrangedObjects");
        assert_eq!(send_word(send_id(root, "childNodes"), "count"), 2);
        assert_eq!(name_at(controller, &[0, 1]), "a1");
        assert_eq!(name_at(controller, &[1]), "b");
        let leaf = send_id_id(
            root,
            "descendantNodeAtIndexPath:",
            foundation::indexpath::from_indexes(&[1]),
        );
        assert_eq!(send_word(leaf, "isLeaf") as u8, 1);
        let path = send_id(leaf, "indexPath");
        assert_eq!(foundation::indexpath::indexes(path), &[1]);
        objc::arc::release(controller);
    }

    #[test]
    fn new_content_selects_the_first_object_to_avoid_an_empty_selection() {
        let content = [item("a", &[]), item("b", &[])];
        let controller = controller_of(&content);
        let selected = send_id(controller, "selectedObjects");
        assert_eq!(send_word(selected, "count"), 1);
        assert_eq!(send_id(selected, "firstObject"), content[0]);
        objc::arc::release(controller);
    }

    #[test]
    fn an_announced_change_of_children_rearranges_the_node() {
        let parent = item("a", &[item("a0", &[])]);
        let controller = controller_of(&[parent]);
        assert_eq!(name_at(controller, &[0, 0]), "a0");
        announce(parent, "willChangeValueForKey:", "children");
        set(
            parent,
            "children",
            array(&[item("z0", &[]), item("z1", &[])]),
        );
        announce(parent, "didChangeValueForKey:", "children");
        assert_eq!(name_at(controller, &[0, 1]), "z1");
        objc::arc::release(controller);
    }

    #[test]
    fn a_selection_follows_its_object_when_content_reorders() {
        let (a, b) = (item("a", &[]), item("b", &[]));
        let controller = controller_of(&[a, b]);
        send_id_id(
            controller,
            "setSelectionIndexPath:",
            foundation::indexpath::from_indexes(&[1]),
        );
        send_id_id(controller, "setContent:", array(&[b, a]));
        let path = send_id(controller, "selectionIndexPath");
        assert_eq!(foundation::indexpath::indexes(path), &[0]);
        objc::arc::release(controller);
    }
}
