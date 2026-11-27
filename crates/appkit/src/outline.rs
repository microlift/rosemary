//! `NSOutlineView`: a table whose rows are the items of a tree, each child
//! shown below its parent while the parent is expanded.
//!
//! The items come from the data source, or from the root tree node a
//! `content` binding gives the view, whose children are tree nodes. The
//! visible rows are worked out again whenever the items or what is expanded
//! change; the table lays its rows out from them.
//!
//! FIXME (ACCURACY): no disclosure triangles are drawn or clicked, and the
//! expanded items are not autosaved.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::tree_controller::{array, elements, send_id, send_id_id, send_word};

/// AppKit's default `-indentationPerLevel`.
const DEFAULT_INDENTATION: f64 = 16.0;

#[derive(Default)]
struct Outline {
    /// Retained items that are expanded.
    expanded: Vec<usize>,
    /// The visible rows: each item, retained, and its level.
    rows: Vec<(usize, usize)>,
    indentation: Option<f64>,
    /// The column the tree is indented in, unretained as the table holds it.
    outline_column: usize,
}

fn outlines() -> &'static Mutex<HashMap<usize, Outline>> {
    static OUTLINES: OnceLock<Mutex<HashMap<usize, Outline>>> = OnceLock::new();
    OUTLINES.get_or_init(Default::default)
}

fn with<R>(receiver: Id, f: impl FnOnce(&mut Outline) -> R) -> R {
    f(outlines()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(receiver as usize)
        .or_default())
}

/// Drop the state of an outline view that is going away.
pub(crate) fn forget(receiver: Id) {
    let gone = outlines()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
    if let Some(outline) = gone {
        let items = outline.expanded.into_iter();
        items
            .chain(outline.rows.into_iter().map(|(item, _)| item))
            .for_each(|item| objc::arc::release(item as Id));
    }
}

pub(crate) fn is_outline(view: Id) -> bool {
    let Some(class) = objc::class::lookup(c"NSOutlineView") else {
        return false;
    };
    // SAFETY: a live object or nil.
    !view.is_null() && unsafe { corefoundation::base::is_kind_of(view.cast(), class) }
}

fn responds(object: Id, sel: &str) -> bool {
    // SAFETY: a live object or nil.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|c| c.method(objc::sel::get(sel)).is_some())
}

/// The children of `item`, nil being the root.
fn children(receiver: Id, item: Id) -> Vec<Id> {
    let root = crate::table::bound_content(receiver);
    if !root.is_null() {
        let node = if item.is_null() { root } else { item };
        return elements(send_id(node, "childNodes"), &|| {
            String::from("-[NSOutlineView reloadData]: a tree node's children")
        });
    }
    let source = crate::table::data_source_of(receiver);
    if !responds(source, "outlineView:numberOfChildrenOfItem:")
        || !responds(source, "outlineView:child:ofItem:")
    {
        return Vec::new();
    }
    // SAFETY: `NSInteger (id, SEL, NSOutlineView *, id)` and
    // `id (id, SEL, NSOutlineView *, NSInteger, id)`, as the protocol declares.
    unsafe {
        let count: unsafe extern "C" fn(Id, Sel, Id, Id) -> isize =
            std::mem::transmute(objc::msg::entry());
        let child: unsafe extern "C" fn(Id, Sel, Id, isize, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        let n = count(
            source,
            objc::sel::get("outlineView:numberOfChildrenOfItem:"),
            receiver,
            item,
        );
        (0..n.max(0))
            .map(|index| {
                child(
                    source,
                    objc::sel::get("outlineView:child:ofItem:"),
                    receiver,
                    index,
                    item,
                )
            })
            .collect()
    }
}

/// Whether `item` has children to show when expanded.
fn expandable(receiver: Id, item: Id) -> bool {
    if item.is_null() {
        return true;
    }
    if !crate::table::bound_content(receiver).is_null() {
        return send_word(item, "isLeaf") as u8 == 0;
    }
    let source = crate::table::data_source_of(receiver);
    if !responds(source, "outlineView:isItemExpandable:") {
        return false;
    }
    // SAFETY: `BOOL (id, SEL, NSOutlineView *, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 =
            std::mem::transmute(objc::msg::entry());
        f(
            source,
            objc::sel::get("outlineView:isItemExpandable:"),
            receiver,
            item,
        ) != 0
    }
}

fn is_expanded(receiver: Id, item: Id) -> bool {
    with(receiver, |o| o.expanded.contains(&(item as usize)))
}

/// Work out the visible rows again from the items and what is expanded.
pub(crate) fn rebuild_rows(receiver: Id) {
    fn walk(receiver: Id, item: Id, level: usize, rows: &mut Vec<(usize, usize)>) {
        for child in children(receiver, item) {
            rows.push((objc::arc::retain(child) as usize, level));
            if is_expanded(receiver, child) && expandable(receiver, child) {
                walk(receiver, child, level + 1, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(receiver, std::ptr::null_mut(), 0, &mut rows);
    let old = with(receiver, |o| std::mem::replace(&mut o.rows, rows));
    old.into_iter()
        .for_each(|(item, _)| objc::arc::release(item as Id));
}

pub(crate) fn row_count(receiver: Id) -> isize {
    with(receiver, |o| o.rows.len() as isize)
}

/// The item shown in `row`, or nil.
pub(crate) fn item_at(receiver: Id, row: isize) -> Id {
    with(receiver, |o| {
        usize::try_from(row)
            .ok()
            .and_then(|row| o.rows.get(row))
            .map_or(0, |(item, _)| *item)
    }) as Id
}

fn row_of(receiver: Id, item: Id) -> isize {
    with(receiver, |o| {
        o.rows
            .iter()
            .position(|(i, _)| *i == item as usize)
            .map_or(-1, |row| row as isize)
    })
}

fn level_at(receiver: Id, row: isize) -> isize {
    with(receiver, |o| {
        usize::try_from(row)
            .ok()
            .and_then(|row| o.rows.get(row))
            .map_or(-1, |(_, level)| *level as isize)
    })
}

/// What a row's views show in `column`: the represented object of a bound
/// tree node, or what the data source answers.
pub(crate) fn object_value(receiver: Id, column: Id, row: isize) -> Id {
    let item = item_at(receiver, row);
    if item.is_null() {
        return std::ptr::null_mut();
    }
    if !crate::table::bound_content(receiver).is_null() {
        return send_id(item, "representedObject");
    }
    let source = crate::table::data_source_of(receiver);
    if !responds(source, "outlineView:objectValueForTableColumn:byItem:") {
        return std::ptr::null_mut();
    }
    // SAFETY: `id (id, SEL, NSOutlineView *, NSTableColumn *, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            source,
            objc::sel::get("outlineView:objectValueForTableColumn:byItem:"),
            receiver,
            column,
            item,
        )
    }
}

/// The rows of the items the index paths lead to from a bound tree's root,
/// for the `selectionIndexPaths` binding.
pub(crate) fn rows_of_paths(receiver: Id, paths: Id) -> Vec<isize> {
    let root = crate::table::bound_content(receiver);
    if root.is_null() {
        return Vec::new();
    }
    elements(paths, &|| String::from("a selectionIndexPaths binding"))
        .into_iter()
        .map(|path| send_id_id(root, "descendantNodeAtIndexPath:", path))
        .filter(|node| !node.is_null())
        .map(|node| row_of(receiver, node))
        .filter(|row| *row >= 0)
        .collect()
}

/// The index paths of the bound tree nodes in `rows`, for the
/// `selectionIndexPaths` binding to hand back.
pub(crate) fn paths_of_rows(receiver: Id, rows: &[isize]) -> Id {
    let paths: Vec<Id> = rows
        .iter()
        .map(|row| item_at(receiver, *row))
        .filter(|item| !item.is_null())
        .map(|item| send_id(item, "indexPath"))
        .collect();
    array(&paths)
}

/// The left edge of the outline column's cells in `row`, past the levels
/// above it.
pub(crate) fn indentation_at(receiver: Id, column: Id, row: isize) -> f64 {
    let outline_column = with(receiver, |o| o.outline_column) as Id;
    let column_is_outline = match outline_column.is_null() {
        true => crate::table::column_index(receiver, column) == Some(0),
        false => column == outline_column,
    };
    match column_is_outline {
        true => level_at(receiver, row).max(0) as f64 * indentation(receiver),
        false => 0.0,
    }
}

fn indentation(receiver: Id) -> f64 {
    with(receiver, |o| o.indentation).unwrap_or(DEFAULT_INDENTATION)
}

/// Tell the delegate and anyone listening that `item` was expanded or
/// collapsed.
fn announce(receiver: Id, item: Id, expanded: bool) {
    let (name, selector) = match expanded {
        true => (
            "NSOutlineViewItemDidExpandNotification",
            "outlineViewItemDidExpand:",
        ),
        false => (
            "NSOutlineViewItemDidCollapseNotification",
            "outlineViewItemDidCollapse:",
        ),
    };
    let key = foundation::string::from_str("NSObject");
    // SAFETY: `+dictionaryWithObject:forKey:` is `id (Class, SEL, id, id)`.
    let info = unsafe {
        let class = objc::class::lookup(c"NSDictionary").expect("NSDictionary");
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(class) as Id,
            objc::sel::get("dictionaryWithObject:forKey:"),
            item,
            key,
        )
    };
    objc::arc::release(key);
    let delegate = crate::table::delegate_of(receiver);
    if responds(delegate, selector) {
        let notification = foundation::notification::make(name, receiver, info);
        send_id_id(delegate, selector, notification);
    }
    foundation::notification::post(name, receiver, info);
}

fn set_expanded(receiver: Id, item: Id, expand: bool, recursive: bool) {
    let items = match item.is_null() {
        true => children(receiver, item),
        false => vec![item],
    };
    for item in items {
        if expand != is_expanded(receiver, item) && (!expand || expandable(receiver, item)) {
            with(receiver, |o| match expand {
                true => o.expanded.push(objc::arc::retain(item) as usize),
                false => o.expanded.retain(|i| *i != item as usize),
            });
            if !expand {
                objc::arc::release(item);
            }
            announce(receiver, item, expand);
        }
        if recursive {
            for child in children(receiver, item) {
                set_expanded(receiver, child, expand, true);
            }
        }
    }
}

fn changed(receiver: Id) {
    rebuild_rows(receiver);
    crate::table::rows_changed(receiver);
}

unsafe extern "C" fn expand_item_children(receiver: Id, _sel: Sel, item: Id, children: u8) {
    set_expanded(receiver, item, true, children != 0);
    changed(receiver);
}

unsafe extern "C" fn expand_item(receiver: Id, sel: Sel, item: Id) {
    // SAFETY: the same receiver.
    unsafe { expand_item_children(receiver, sel, item, 0) };
}

unsafe extern "C" fn collapse_item_children(receiver: Id, _sel: Sel, item: Id, children: u8) {
    set_expanded(receiver, item, false, children != 0);
    changed(receiver);
}

unsafe extern "C" fn collapse_item(receiver: Id, sel: Sel, item: Id) {
    // SAFETY: the same receiver.
    unsafe { collapse_item_children(receiver, sel, item, 0) };
}

unsafe extern "C" fn is_item_expanded(receiver: Id, _sel: Sel, item: Id) -> u8 {
    (item.is_null() || is_expanded(receiver, item)) as u8
}

unsafe extern "C" fn is_expandable(receiver: Id, _sel: Sel, item: Id) -> u8 {
    expandable(receiver, item) as u8
}

/// `-reloadItem:reloadChildren:`: the rows are worked out again; every row's
/// views are made again, which covers the item's.
unsafe extern "C" fn reload_item_children(receiver: Id, _sel: Sel, _item: Id, _children: u8) {
    changed(receiver);
}

unsafe extern "C" fn reload_item(receiver: Id, sel: Sel, item: Id) {
    // SAFETY: the same receiver.
    unsafe { reload_item_children(receiver, sel, item, 0) };
}

unsafe extern "C" fn item_at_row(receiver: Id, _sel: Sel, row: isize) -> Id {
    item_at(receiver, row)
}

unsafe extern "C" fn row_for_item(receiver: Id, _sel: Sel, item: Id) -> isize {
    row_of(receiver, item)
}

unsafe extern "C" fn level_for_row(receiver: Id, _sel: Sel, row: isize) -> isize {
    level_at(receiver, row)
}

unsafe extern "C" fn level_for_item(receiver: Id, _sel: Sel, item: Id) -> isize {
    level_at(receiver, row_of(receiver, item))
}

/// `-parentForItem:`: the nearest row above at a lower level, or nil for a
/// top-level item.
unsafe extern "C" fn parent_for_item(receiver: Id, _sel: Sel, item: Id) -> Id {
    let row = row_of(receiver, item);
    let level = level_at(receiver, row);
    (0..row.max(0))
        .rev()
        .find(|r| level_at(receiver, *r) < level)
        .map_or(std::ptr::null_mut(), |r| item_at(receiver, r))
}

unsafe extern "C" fn child_of_item(receiver: Id, _sel: Sel, index: isize, item: Id) -> Id {
    usize::try_from(index)
        .ok()
        .and_then(|index| children(receiver, item).get(index).copied())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn number_of_children(receiver: Id, _sel: Sel, item: Id) -> isize {
    children(receiver, item).len() as isize
}

unsafe extern "C" fn child_index_for_item(receiver: Id, sel: Sel, item: Id) -> isize {
    // SAFETY: the same receiver.
    let parent = unsafe { parent_for_item(receiver, sel, item) };
    children(receiver, parent)
        .iter()
        .position(|c| *c == item)
        .map_or(-1, |index| index as isize)
}

unsafe extern "C" fn indentation_per_level(receiver: Id, _sel: Sel) -> f64 {
    indentation(receiver)
}

unsafe extern "C" fn set_indentation_per_level(receiver: Id, _sel: Sel, value: f64) {
    with(receiver, |o| o.indentation = Some(value));
    crate::table::rows_changed(receiver);
}

unsafe extern "C" fn outline_table_column(receiver: Id, _sel: Sel) -> Id {
    match with(receiver, |o| o.outline_column) as Id {
        column if column.is_null() => crate::table::column_at(receiver, 0),
        column => column,
    }
}

unsafe extern "C" fn set_outline_table_column(receiver: Id, _sel: Sel, column: Id) {
    with(receiver, |o| o.outline_column = column as usize);
    crate::table::rows_changed(receiver);
}

/// `-frameOfOutlineCellAtRow:`: no disclosure triangle is shown.
unsafe extern "C" fn frame_of_outline_cell(_receiver: Id, _sel: Sel, _row: isize) -> NSRect {
    NSRect::default()
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSOutlineView").expect("declared in crate::classes");
    methods!(class, {
        "expandItem:" => expand_item, c"v24@0:8@16";
        "expandItem:expandChildren:" => expand_item_children, c"v28@0:8@16c24";
        "collapseItem:" => collapse_item, c"v24@0:8@16";
        "collapseItem:collapseChildren:" => collapse_item_children, c"v28@0:8@16c24";
        "isItemExpanded:" => is_item_expanded, c"c24@0:8@16";
        "isExpandable:" => is_expandable, c"c24@0:8@16";
        "reloadItem:" => reload_item, c"v24@0:8@16";
        "reloadItem:reloadChildren:" => reload_item_children, c"v28@0:8@16c24";
        "itemAtRow:" => item_at_row, c"@24@0:8q16";
        "rowForItem:" => row_for_item, c"q24@0:8@16";
        "levelForRow:" => level_for_row, c"q24@0:8q16";
        "levelForItem:" => level_for_item, c"q24@0:8@16";
        "parentForItem:" => parent_for_item, c"@24@0:8@16";
        "child:ofItem:" => child_of_item, c"@32@0:8q16@24";
        "numberOfChildrenOfItem:" => number_of_children, c"q24@0:8@16";
        "childIndexForItem:" => child_index_for_item, c"q24@0:8@16";
        "indentationPerLevel" => indentation_per_level, c"d16@0:8";
        "setIndentationPerLevel:" => set_indentation_per_level, c"v24@0:8d16";
        "outlineTableColumn" => outline_table_column, c"@16@0:8";
        "setOutlineTableColumn:" => set_outline_table_column, c"v24@0:8@16";
        "frameOfOutlineCellAtRow:" => frame_of_outline_cell, c"{CGRect={CGPoint=dd}{CGSize=dd}}24@0:8q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_controller::class_named;

    fn string(text: &str) -> Id {
        objc::arc::autorelease(foundation::string::from_str(text))
    }

    /// A mutable dictionary named `name` whose `children` are `children`.
    fn item(name: &str, children: &[Id]) -> Id {
        crate::bootstrap();
        let item = send_id(class_named(c"NSMutableDictionary"), "dictionary");
        set_value(item, "name", string(name));
        set_value(item, "children", array(children));
        item
    }

    fn set_value(object: Id, key: &str, value: Id) {
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

    fn name_of(object: Id) -> String {
        let name = send_id_id(object, "valueForKey:", string("name"));
        // SAFETY: a string.
        String::from_utf8_lossy(&unsafe { foundation::string::bytes(name) }).into_owned()
    }

    fn bind(view: Id, binding: &str, controller: Id, key_path: &str) {
        // SAFETY: `void (id, SEL, NSString *, id, NSString *, NSDictionary *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id, Id) =
                std::mem::transmute(objc::msg::entry());
            f(
                view,
                objc::sel::get("bind:toObject:withKeyPath:options:"),
                string(binding),
                controller,
                string(key_path),
                std::ptr::null_mut(),
            );
        }
    }

    fn integer(receiver: Id, sel: &str, argument: isize) -> isize {
        // SAFETY: `NSInteger (id, SEL, NSInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, isize) -> isize =
                std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(sel), argument)
        }
    }

    /// An outline view showing a tree controller's content: `a` with two
    /// children, and `b`.
    fn bound_outline() -> (Id, Id, Id) {
        crate::bootstrap();
        let a = item("a", &[item("a0", &[]), item("a1", &[])]);
        let controller = send_id(send_id(class_named(c"NSTreeController"), "alloc"), "init");
        send_id_id(controller, "setChildrenKeyPath:", string("children"));
        send_id_id(controller, "setContent:", array(&[a, item("b", &[])]));
        let outline = send_id(send_id(class_named(c"NSOutlineView"), "alloc"), "init");
        bind(outline, "content", controller, "arrangedObjects");
        bind(
            outline,
            "selectionIndexPaths",
            controller,
            "selectionIndexPaths",
        );
        (outline, controller, a)
    }

    #[test]
    fn an_expanded_items_children_are_the_rows_below_it() {
        let (outline, _controller, a) = bound_outline();
        assert_eq!(send_word(outline, "numberOfRows"), 2);
        let first = item_at(outline, 0);
        assert_eq!(send_id(first, "representedObject"), a);
        send_id_id(outline, "expandItem:", first);
        assert_eq!(send_word(outline, "numberOfRows"), 4);
        assert_eq!(integer(outline, "levelForRow:", 2), 1);
        assert_eq!(
            name_of(send_id(item_at(outline, 2), "representedObject")),
            "a1"
        );
        assert_eq!(
            name_of(send_id(item_at(outline, 3), "representedObject")),
            "b"
        );
        send_id_id(outline, "collapseItem:", first);
        assert_eq!(send_word(outline, "numberOfRows"), 2);
    }

    #[test]
    fn the_selection_binding_follows_the_controller_and_back() {
        let (outline, controller, _a) = bound_outline();
        // The controller avoids an empty selection, and the view shows it.
        assert_eq!(send_word(outline, "selectedRow") as isize, 0);
        let class = class_named(c"NSIndexSet");
        let set = send_id_id(class, "indexSetWithIndex:", 1 as Id);
        // SAFETY: `void (id, SEL, NSIndexSet *, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, u8) = std::mem::transmute(objc::msg::entry());
            f(
                outline,
                objc::sel::get("selectRowIndexes:byExtendingSelection:"),
                set,
                0,
            );
        }
        let path = send_id(controller, "selectionIndexPath");
        assert_eq!(foundation::indexpath::indexes(path), &[1]);
    }

    unsafe extern "C" fn view_for(
        _delegate: Id,
        _sel: Sel,
        _outline: Id,
        _column: Id,
        _item: Id,
    ) -> Id {
        let cell = send_id(class_named(c"NSTableCellView"), "alloc");
        objc::arc::autorelease(send_id(cell, "init"))
    }

    #[test]
    fn a_view_based_outline_makes_a_row_view_holding_each_rows_cell() {
        let (outline, _controller, a) = bound_outline();
        let delegate_class =
            objc::class::lookup(c"RosemaryTestOutlineDelegate").unwrap_or_else(|| {
                let class = corefoundation::base::register(
                    c"RosemaryTestOutlineDelegate",
                    None,
                    size_of::<usize>(),
                    false,
                );
                methods!(class, {
                    "outlineView:viewForTableColumn:item:" => view_for, c"@40@0:8@16@24@32";
                });
                class
            });
        let delegate = objc::object::create_instance(delegate_class, 0);
        let column = send_id(class_named(c"NSTableColumn"), "alloc");
        send_id_id(outline, "addTableColumn:", send_id(column, "init"));
        send_id_id(outline, "setDelegate:", delegate);
        let rows = crate::view::subviews_of(outline);
        assert_eq!(rows.len(), 2);
        // SAFETY: `id (id, SEL, NSInteger, NSInteger, BOOL)`.
        let cell = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, isize, isize, u8) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                outline,
                objc::sel::get("viewAtColumn:row:makeIfNecessary:"),
                0,
                0,
                0,
            )
        };
        assert_eq!(send_id(cell, "objectValue"), a);
        assert_eq!(send_word(send_id(cell, "superview"), "isSelected") as u8, 1);
        assert_eq!(integer(outline, "rowForView:", cell as isize), 0);
    }
}
