//! `NSCollectionView` as a data source fills it, `NSCollectionViewItem`, and
//! the layout that places items: `NSCollectionViewLayout` and the grid that
//! Interface Builder archives, `NSCollectionViewGridLayout`.
//!
//! The data source's counts are read by `-reloadData` (or the first layout)
//! and kept until the next one. A layout pass asks the layout for its content
//! size and each item's attributes, and frames the items' views as subviews.
//!
//! FIXME (ACCURACY): every item is made and kept, not only the visible ones,
//! and none is reused; no drag, keyboard navigation or rubber-band selection.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::controls::NSEdgeInsets;
use crate::define::{class_methods, methods};
use crate::props::{self, properties};

pub(crate) const ATTRIBUTES_INSTANCE_SIZE: usize = size_of::<Attributes>();

/// `NSEventModifierFlags`.
const COMMAND: usize = 1 << 20;
const SHIFT: usize = 1 << 17;

/// `NSCollectionElementCategoryItem`.
const CATEGORY_ITEM: isize = 0;

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

fn send_id_id_id(receiver: Id, sel: &str, first: Id, second: Id) -> Id {
    // SAFETY: `id (id, SEL, id, id)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), first, second)
    }
}

/// A `void`, `BOOL` or integer answer of `(id, SEL)`, read as a word.
fn send_word(receiver: Id, sel: &str) -> usize {
    // SAFETY: as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

fn send_bool(receiver: Id, sel: &str) -> bool {
    send_word(receiver, sel) as u8 != 0
}

fn send_set_bool(receiver: Id, sel: &str, value: bool) {
    // SAFETY: `void (id, SEL, BOOL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), value as u8);
    }
}

fn send_rect(receiver: Id, sel: &str) -> NSRect {
    // SAFETY: `NSRect (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

fn send_set_rect(receiver: Id, sel: &str, rect: NSRect) {
    // SAFETY: `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), rect);
    }
}

fn responds(object: Id, sel: &str) -> bool {
    // SAFETY: a live object or nil.
    !object.is_null()
        && unsafe { objc::object::class_of(object) }
            .is_some_and(|c| c.method(objc::sel::get(sel)).is_some())
}

fn class_named(name: &std::ffi::CStr) -> Id {
    objc::class::ptr(objc::class::lookup(name).expect("registered before collections")) as Id
}

fn array(objects: &[Id]) -> Id {
    let values: Vec<_> = objects
        .iter()
        .map(|o| *o as corefoundation::base::CFTypeRef)
        .collect();
    objc::arc::autorelease(corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id)
}

fn elements(collection: Id) -> Vec<Id> {
    // SAFETY: an array, or nil.
    unsafe { foundation::elements_of(collection) }
        .unwrap_or_default()
        .into_iter()
        .map(|o| o as Id)
        .collect()
}

/// A new, autoreleased `NSSet` of `objects`.
fn set_of(objects: &[Id]) -> Id {
    // SAFETY: `id (Class, SEL, const id *, NSUInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const Id, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            class_named(c"NSSet"),
            objc::sel::get("setWithObjects:count:"),
            objects.as_ptr(),
            objects.len(),
        )
    }
}

/// An `NSIndexPath` of a set or array, as `(section, item)`.
type Path = (usize, usize);

fn path_of(index_path: Id) -> Option<Path> {
    match foundation::indexpath::indexes(index_path) {
        [section, item] => Some((*section, *item)),
        _ => None,
    }
}

fn index_path(path: Path) -> Id {
    foundation::indexpath::from_indexes(&[path.0, path.1])
}

fn paths_in(collection: Id) -> Vec<Path> {
    let objects = match responds(collection, "allObjects") {
        true => send_id(collection, "allObjects"),
        false => collection,
    };
    let mut paths: Vec<Path> = elements(objects).into_iter().filter_map(path_of).collect();
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn set_of_paths(paths: &[Path]) -> Id {
    let objects: Vec<Id> = paths.iter().map(|p| index_path(*p)).collect();
    set_of(&objects)
}

// NSIndexPath's AppKit additions.

unsafe extern "C" fn index_path_for_item(_class: Id, _sel: Sel, item: isize, section: isize) -> Id {
    index_path((section as usize, item as usize))
}

unsafe extern "C" fn index_path_item(receiver: Id, _sel: Sel) -> isize {
    foundation::indexpath::indexes(receiver)
        .get(1)
        .map_or(isize::MAX, |i| *i as isize)
}

unsafe extern "C" fn index_path_section(receiver: Id, _sel: Sel) -> isize {
    foundation::indexpath::indexes(receiver)
        .first()
        .map_or(isize::MAX, |i| *i as isize)
}

// NSCollectionViewLayoutAttributes.

#[repr(C)]
struct Attributes {
    isa: *const c_void,
    frame: NSRect,
    alpha: f64,
    z_index: isize,
    hidden: u8,
    /// Retained.
    index_path: Id,
}

fn attributes(receiver: Id) -> &'static mut Attributes {
    // SAFETY: an `NSCollectionViewLayoutAttributes` this module laid out.
    unsafe { &mut *(receiver as *mut Attributes) }
}

unsafe extern "C" fn attributes_init(receiver: Id, _sel: Sel) -> Id {
    if !receiver.is_null() {
        attributes(receiver).alpha = 1.0;
    }
    receiver
}

unsafe extern "C" fn attributes_for_item(class: Id, _sel: Sel, path: Id) -> Id {
    let made = send_id(send_id(class, "alloc"), "init");
    attributes(made).index_path = send_id(path, "copy");
    objc::arc::autorelease(made)
}

unsafe extern "C" fn attributes_dealloc(receiver: Id, _sel: Sel) {
    objc::arc::release(attributes(receiver).index_path);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn attributes_copy(receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
    // SAFETY: a live object.
    let class = unsafe { objc::object::class_of(receiver) }.expect("a live object");
    let copy = send_id(send_id(objc::class::ptr(class) as Id, "alloc"), "init");
    let (from, to) = (attributes(receiver), attributes(copy));
    to.frame = from.frame;
    to.alpha = from.alpha;
    to.z_index = from.z_index;
    to.hidden = from.hidden;
    to.index_path = objc::arc::retain(from.index_path);
    copy
}

unsafe extern "C" fn attributes_frame(receiver: Id, _sel: Sel) -> NSRect {
    attributes(receiver).frame
}

unsafe extern "C" fn attributes_set_frame(receiver: Id, _sel: Sel, frame: NSRect) {
    attributes(receiver).frame = frame;
}

unsafe extern "C" fn attributes_size(receiver: Id, _sel: Sel) -> NSSize {
    attributes(receiver).frame.size
}

unsafe extern "C" fn attributes_set_size(receiver: Id, _sel: Sel, size: NSSize) {
    attributes(receiver).frame.size = size;
}

unsafe extern "C" fn attributes_alpha(receiver: Id, _sel: Sel) -> f64 {
    attributes(receiver).alpha
}

unsafe extern "C" fn attributes_set_alpha(receiver: Id, _sel: Sel, alpha: f64) {
    attributes(receiver).alpha = alpha;
}

unsafe extern "C" fn attributes_z_index(receiver: Id, _sel: Sel) -> isize {
    attributes(receiver).z_index
}

unsafe extern "C" fn attributes_set_z_index(receiver: Id, _sel: Sel, z: isize) {
    attributes(receiver).z_index = z;
}

unsafe extern "C" fn attributes_is_hidden(receiver: Id, _sel: Sel) -> u8 {
    attributes(receiver).hidden
}

unsafe extern "C" fn attributes_set_hidden(receiver: Id, _sel: Sel, hidden: u8) {
    attributes(receiver).hidden = (hidden != 0) as u8;
}

unsafe extern "C" fn attributes_index_path(receiver: Id, _sel: Sel) -> Id {
    attributes(receiver).index_path
}

unsafe extern "C" fn attributes_set_index_path(receiver: Id, _sel: Sel, path: Id) {
    let attributes = attributes(receiver);
    let copy = match path.is_null() {
        true => path,
        false => send_id(path, "copy"),
    };
    objc::arc::release(std::mem::replace(&mut attributes.index_path, copy));
}

unsafe extern "C" fn attributes_category(_receiver: Id, _sel: Sel) -> isize {
    CATEGORY_ITEM
}

unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

// NSCollectionViewLayout.

unsafe extern "C" fn layout_attributes_class(_class: Id, _sel: Sel) -> Id {
    class_named(c"NSCollectionViewLayoutAttributes")
}

unsafe extern "C" fn nothing(_receiver: Id, _sel: Sel) {}

/// `-invalidateLayout`: the collection view lays its items out again.
unsafe extern "C" fn invalidate_layout(receiver: Id, _sel: Sel) {
    let view = props::object(receiver, "collectionView");
    if !view.is_null() {
        relayout(view);
    }
}

unsafe extern "C" fn no_content_size(_receiver: Id, _sel: Sel) -> NSSize {
    NSSize::default()
}

unsafe extern "C" fn no_attributes_in_rect(_receiver: Id, _sel: Sel, _rect: NSRect) -> Id {
    array(&[])
}

unsafe extern "C" fn no_attributes(_receiver: Id, _sel: Sel, _path: Id) -> Id {
    std::ptr::null_mut()
}

// NSCollectionViewGridLayout.

/// Where a grid layout puts items in its collection view, now.
struct Grid {
    columns: usize,
    /// No limit when zero.
    max_rows: usize,
    item: NSSize,
    margins: NSEdgeInsets,
    column_pitch: f64,
    row_pitch: f64,
}

fn double(receiver: Id, name: &str) -> f64 {
    f64::from_bits(props::integer(receiver, name) as u64)
}

fn set_double(receiver: Id, name: &str, value: f64) {
    props::set_integer_of(receiver, name, value.to_bits() as usize);
}

fn size_named(receiver: Id, name: &str) -> NSSize {
    NSSize {
        width: double(receiver, &format!("{name} width")),
        height: double(receiver, &format!("{name} height")),
    }
}

fn set_size_named(receiver: Id, name: &str, size: NSSize) {
    set_double(receiver, &format!("{name} width"), size.width);
    set_double(receiver, &format!("{name} height"), size.height);
}

fn grid(layout: Id) -> Grid {
    let view = props::object(layout, "collectionView");
    let width = match view.is_null() {
        true => 0.0,
        false => crate::view::bounds_of(view).size.width,
    };
    let margins = NSEdgeInsets {
        top: double(layout, "margins top"),
        left: double(layout, "margins left"),
        bottom: double(layout, "margins bottom"),
        right: double(layout, "margins right"),
    };
    let available = (width - margins.left - margins.right).max(0.0);
    let (min, max) = (
        size_named(layout, "minimumItemSize"),
        size_named(layout, "maximumItemSize"),
    );
    let spacing = double(layout, "minimumInteritemSpacing");
    // FIXME (ACCURACY): with no minimum width, the maximum (or the whole
    // width) is what decides how many columns fit; inferred, not observed.
    let basis = [min.width, max.width, available]
        .into_iter()
        .find(|w| *w > 0.0)
        .unwrap_or(1.0);
    let mut columns = (((available + spacing) / (basis + spacing)).floor() as usize).max(1);
    let max_columns = props::integer(layout, "maximumNumberOfColumns");
    if max_columns > 0 {
        columns = columns.min(max_columns);
    }
    let mut item_width = (available - spacing * (columns - 1) as f64) / columns as f64;
    if max.width > 0.0 {
        item_width = item_width.min(max.width);
    }
    item_width = item_width.max(min.width);
    // FIXME (ACCURACY): items keep their minimum height, and the width a row
    // does not use is left at its end; inferred, not observed.
    let item_height = [min.height, max.height]
        .into_iter()
        .find(|h| *h > 0.0)
        .unwrap_or(0.0);
    Grid {
        columns,
        max_rows: props::integer(layout, "maximumNumberOfRows"),
        item: NSSize {
            width: item_width,
            height: item_height,
        },
        margins,
        column_pitch: item_width + spacing,
        row_pitch: item_height + double(layout, "minimumLineSpacing"),
    }
}

/// The items of every section, in order, as one run of positions.
fn flat_index(view: Id, path: Path) -> Option<usize> {
    let counts = counts_of(view);
    let before: usize = counts.iter().take(path.0).sum();
    (path.1 < *counts.get(path.0)?).then_some(before + path.1)
}

fn grid_rows(grid: &Grid, items: usize) -> usize {
    let rows = items.div_ceil(grid.columns);
    match grid.max_rows {
        0 => rows,
        max => rows.min(max),
    }
}

unsafe extern "C" fn grid_content_size(receiver: Id, _sel: Sel) -> NSSize {
    let view = props::object(receiver, "collectionView");
    if view.is_null() {
        return NSSize::default();
    }
    let grid = grid(receiver);
    let rows = grid_rows(&grid, counts_of(view).iter().sum());
    let height = match rows {
        0 => 0.0,
        rows => rows as f64 * grid.row_pitch - (grid.row_pitch - grid.item.height),
    };
    NSSize {
        width: crate::view::bounds_of(view).size.width,
        height: grid.margins.top + height + grid.margins.bottom,
    }
}

fn grid_frame(grid: &Grid, index: usize) -> Option<NSRect> {
    let (row, column) = (index / grid.columns, index % grid.columns);
    if grid.max_rows != 0 && row >= grid.max_rows {
        return None;
    }
    Some(NSRect {
        origin: NSPoint {
            x: grid.margins.left + column as f64 * grid.column_pitch,
            y: grid.margins.top + row as f64 * grid.row_pitch,
        },
        size: grid.item,
    })
}

fn new_attributes(layout: Id, path: Path, frame: NSRect) -> Id {
    // SAFETY: the layout's class answers `+layoutAttributesClass`.
    let class = unsafe { objc::object::class_of(layout) }.expect("a live layout");
    let attributes_class = send_id(objc::class::ptr(class) as Id, "layoutAttributesClass");
    let made = send_id_id(
        attributes_class,
        "layoutAttributesForItemWithIndexPath:",
        index_path(path),
    );
    send_set_rect(made, "setFrame:", frame);
    made
}

unsafe extern "C" fn grid_attributes_for_item(receiver: Id, _sel: Sel, path: Id) -> Id {
    let view = props::object(receiver, "collectionView");
    let Some(path) = (!view.is_null() && !path.is_null())
        .then(|| path_of(path))
        .flatten()
    else {
        return std::ptr::null_mut();
    };
    let grid = grid(receiver);
    match flat_index(view, path).and_then(|index| grid_frame(&grid, index)) {
        Some(frame) => new_attributes(receiver, path, frame),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn grid_attributes_in_rect(receiver: Id, _sel: Sel, rect: NSRect) -> Id {
    let view = props::object(receiver, "collectionView");
    if view.is_null() {
        return array(&[]);
    }
    let grid = grid(receiver);
    let mut found = Vec::new();
    let mut index = 0;
    for (section, count) in counts_of(view).into_iter().enumerate() {
        for item in 0..count {
            if let Some(frame) = grid_frame(&grid, index)
                // SAFETY: plain geometry.
                && unsafe { crate::geometry::NSIntersectsRect(frame, rect) } != 0
            {
                found.push(new_attributes(receiver, (section, item), frame));
            }
            index += 1;
        }
    }
    array(&found)
}

macro_rules! grid_doubles {
    ($($getter:ident, $setter:ident, $name:literal;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> f64 {
            double(receiver, $name)
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, value: f64) {
            set_double(receiver, $name, value);
            send_word(receiver, "invalidateLayout");
        }
    )*};
}

grid_doubles! {
    minimum_interitem_spacing, set_minimum_interitem_spacing, "minimumInteritemSpacing";
    minimum_line_spacing, set_minimum_line_spacing, "minimumLineSpacing";
}

macro_rules! grid_sizes {
    ($($getter:ident, $setter:ident, $name:literal;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> NSSize {
            size_named(receiver, $name)
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, size: NSSize) {
            set_size_named(receiver, $name, size);
            send_word(receiver, "invalidateLayout");
        }
    )*};
}

grid_sizes! {
    minimum_item_size, set_minimum_item_size, "minimumItemSize";
    maximum_item_size, set_maximum_item_size, "maximumItemSize";
}

macro_rules! grid_integers {
    ($($getter:ident, $setter:ident, $name:literal;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> usize {
            props::integer(receiver, $name)
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, value: usize) {
            props::set_integer_of(receiver, $name, value);
            send_word(receiver, "invalidateLayout");
        }
    )*};
}

grid_integers! {
    maximum_number_of_rows, set_maximum_number_of_rows, "maximumNumberOfRows";
    maximum_number_of_columns, set_maximum_number_of_columns, "maximumNumberOfColumns";
}

unsafe extern "C" fn margins(receiver: Id, _sel: Sel) -> NSEdgeInsets {
    NSEdgeInsets {
        top: double(receiver, "margins top"),
        left: double(receiver, "margins left"),
        bottom: double(receiver, "margins bottom"),
        right: double(receiver, "margins right"),
    }
}

unsafe extern "C" fn set_margins(receiver: Id, _sel: Sel, margins: NSEdgeInsets) {
    set_double(receiver, "margins top", margins.top);
    set_double(receiver, "margins left", margins.left);
    set_double(receiver, "margins bottom", margins.bottom);
    set_double(receiver, "margins right", margins.right);
    send_word(receiver, "invalidateLayout");
}

// NSCollectionView.

/// The data source's counts, per section, as of the last reload.
fn counts() -> &'static Mutex<HashMap<usize, Vec<usize>>> {
    static COUNTS: OnceLock<Mutex<HashMap<usize, Vec<usize>>>> = OnceLock::new();
    COUNTS.get_or_init(Default::default)
}

fn counts_of(view: Id) -> Vec<usize> {
    counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(view as usize))
        .cloned()
        .unwrap_or_default()
}

/// Drop the state of a collection view that is going away.
pub(crate) fn forget(view: Id) {
    counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(view as usize));
}

/// Ask the data source for its counts again.
fn load_counts(view: Id) {
    let source = props::object(view, "dataSource");
    // SAFETY: `NSInteger (id, SEL, NSCollectionView *)` and
    // `NSInteger (id, SEL, NSCollectionView *, NSInteger)`, the data source's.
    let (sections, items): (
        unsafe extern "C" fn(Id, Sel, Id) -> isize,
        unsafe extern "C" fn(Id, Sel, Id, isize) -> isize,
    ) = unsafe {
        (
            std::mem::transmute(objc::msg::entry()),
            std::mem::transmute(objc::msg::entry()),
        )
    };
    let sections = match responds(source, "numberOfSectionsInCollectionView:") {
        // SAFETY: as above.
        true => unsafe {
            sections(
                source,
                objc::sel::get("numberOfSectionsInCollectionView:"),
                view,
            )
        },
        false => 1,
    };
    let loaded: Vec<usize> = match responds(source, "collectionView:numberOfItemsInSection:") {
        false => Vec::new(),
        true => (0..sections.max(0))
            .map(|section| {
                // SAFETY: as above.
                let count = unsafe {
                    items(
                        source,
                        objc::sel::get("collectionView:numberOfItemsInSection:"),
                        view,
                        section,
                    )
                };
                count.max(0) as usize
            })
            .collect(),
    };
    counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(view as usize, loaded);
}

/// A private association: a dictionary the view owns.
fn owned_dictionary(view: Id, name: &str) -> Id {
    let key = objc::sel::get(name) as *const c_void;
    let found = objc::arc::associated(view, key);
    if !found.is_null() {
        return found;
    }
    let made = send_id(class_named(c"NSMutableDictionary"), "dictionary");
    objc::arc::set_associated_with_policy(view, key, made, 1);
    made
}

/// The items made so far, by index path.
fn items(view: Id) -> Id {
    owned_dictionary(view, "_rosemaryItems")
}

fn item_classes(view: Id) -> Id {
    owned_dictionary(view, "_rosemaryItemClasses")
}

thread_local! {
    /// Collection views laying out now, and whether each was asked to again.
    static LAYING_OUT: RefCell<HashMap<usize, bool>> = RefCell::new(HashMap::new());
}

/// Lay the items out again, now; a request during a pass makes another pass
/// once it is done.
fn relayout(view: Id) {
    let busy = LAYING_OUT.with(|l| {
        let mut l = l.borrow_mut();
        match l.get_mut(&(view as usize)) {
            Some(again) => {
                *again = true;
                true
            }
            None => {
                l.insert(view as usize, false);
                false
            }
        }
    });
    if busy {
        return;
    }
    const PASSES: usize = 4;
    for _ in 0..PASSES {
        lay_out(view);
        let again = LAYING_OUT.with(|l| {
            let mut l = l.borrow_mut();
            std::mem::replace(l.get_mut(&(view as usize)).expect("inserted above"), false)
        });
        if !again {
            break;
        }
    }
    LAYING_OUT.with(|l| l.borrow_mut().remove(&(view as usize)));
}

fn take_item(view: Id, key: Id) {
    let items = items(view);
    let item = send_id_id(items, "objectForKey:", key);
    if item.is_null() {
        return;
    }
    if send_bool(item, "isViewLoaded") {
        send_word(send_id(item, "view"), "removeFromSuperview");
    }
    send_id_id(items, "removeObjectForKey:", key);
}

fn lay_out(view: Id) {
    let layout = props::object(view, "collectionViewLayout");
    if layout.is_null() {
        return;
    }
    if !counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&(view as usize))
    {
        load_counts(view);
    }
    send_word(layout, "prepareLayout");
    // SAFETY: `NSSize (id, SEL)`.
    let content = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSSize = std::mem::transmute(objc::msg::entry());
        f(layout, objc::sel::get("collectionViewContentSize"))
    };
    // A document view at least fills the clip view it is in.
    let frame = send_rect(view, "frame");
    let superview = send_id(view, "superview");
    let mut height = content.height;
    if !superview.is_null()
        && let Some(clip) = objc::class::lookup(c"NSClipView")
        // SAFETY: a live view.
        && unsafe { corefoundation::base::is_kind_of(superview.cast(), clip) }
    {
        height = height.max(crate::view::bounds_of(superview).size.height);
    }
    if height != frame.size.height {
        // SAFETY: `void (id, SEL, NSSize)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSSize) = std::mem::transmute(objc::msg::entry());
            f(
                view,
                objc::sel::get("setFrameSize:"),
                NSSize {
                    width: frame.size.width,
                    height,
                },
            );
        }
    }

    let counts = counts_of(view);
    let wanted: HashSet<Path> = counts
        .iter()
        .enumerate()
        .flat_map(|(section, count)| (0..*count).map(move |item| (section, item)))
        .collect();
    for key in elements(send_id(items(view), "allKeys")) {
        if path_of(key).is_none_or(|p| !wanted.contains(&p)) {
            take_item(view, key);
        }
    }
    let selection: HashSet<Path> = paths_in(selection_of(view)).into_iter().collect();
    let source = props::object(view, "dataSource");
    let delegate = props::object(view, "delegate");
    for (section, count) in counts.iter().enumerate() {
        for item_index in 0..*count {
            let path = (section, item_index);
            let key = index_path(path);
            let attributes = send_id_id(layout, "layoutAttributesForItemAtIndexPath:", key);
            let mut item = send_id_id(items(view), "objectForKey:", key);
            let made = item.is_null();
            if made {
                item = send_id_id_id(
                    source,
                    "collectionView:itemForRepresentedObjectAtIndexPath:",
                    view,
                    key,
                );
                if item.is_null() {
                    objc::fail!(
                        "-[NSCollectionView reloadData]: the data source gave no item for {{{section}, {item_index}}}"
                    );
                }
                send_id_id_id(items(view), "setObject:forKey:", item, key);
                objc::arc::set_associated(
                    item,
                    objc::sel::get("collectionView") as *const c_void,
                    view,
                );
            }
            if send_bool(item, "isSelected") != selection.contains(&path) {
                send_set_bool(item, "setSelected:", selection.contains(&path));
            }
            let item_view = send_id(item, "view");
            if attributes.is_null() || item_view.is_null() {
                if !item_view.is_null() {
                    send_set_bool(item_view, "setHidden:", true);
                }
                continue;
            }
            send_set_rect(item_view, "setFrame:", send_rect(attributes, "frame"));
            send_set_bool(item_view, "setHidden:", send_bool(attributes, "isHidden"));
            if made
                && responds(
                    delegate,
                    "collectionView:willDisplayItem:forRepresentedObjectAtIndexPath:",
                )
            {
                // SAFETY: `void (id, SEL, NSCollectionView *, NSCollectionViewItem *, NSIndexPath *)`.
                unsafe {
                    let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) =
                        std::mem::transmute(objc::msg::entry());
                    f(
                        delegate,
                        objc::sel::get(
                            "collectionView:willDisplayItem:forRepresentedObjectAtIndexPath:",
                        ),
                        view,
                        item,
                        key,
                    );
                }
            }
            if send_id(item_view, "superview") != view {
                send_id_id(view, "addSubview:", item_view);
            }
        }
    }
    crate::backing::needs_display(view);
}

unsafe extern "C" fn is_flipped(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn set_collection_view_layout(receiver: Id, sel: Sel, layout: Id) {
    let old = props::object(receiver, "collectionViewLayout");
    if !old.is_null() {
        objc::arc::set_associated(
            old,
            objc::sel::get("collectionView") as *const c_void,
            std::ptr::null_mut(),
        );
    }
    // SAFETY: the strong property setter.
    unsafe { props::set_retained(receiver, sel, layout) };
    if !layout.is_null() {
        objc::arc::set_associated(
            layout,
            objc::sel::get("collectionView") as *const c_void,
            receiver,
        );
        relayout(receiver);
    }
}

unsafe extern "C" fn register_class(receiver: Id, _sel: Sel, class: Id, identifier: Id) {
    let classes = item_classes(receiver);
    match class.is_null() {
        true => send_id_id(classes, "removeObjectForKey:", identifier),
        false => send_id_id_id(classes, "setObject:forKey:", class, identifier),
    };
}

/// `-makeItemWithIdentifier:forIndexPath:`: a new item of the class
/// registered for `identifier`.
unsafe extern "C" fn make_item(receiver: Id, _sel: Sel, identifier: Id, _path: Id) -> Id {
    let class = send_id_id(item_classes(receiver), "objectForKey:", identifier);
    if class.is_null() {
        let name =
            String::from_utf8_lossy(&unsafe { foundation::string::bytes(identifier) }).into_owned();
        objc::fail!(
            "-[NSCollectionView makeItemWithIdentifier:forIndexPath:]: no class is registered for {name:?}, and making items from nibs is not implemented"
        );
    }
    let item = send_id(send_id(class, "alloc"), "init");
    objc::arc::set_associated(
        item,
        objc::sel::get("collectionView") as *const c_void,
        receiver,
    );
    objc::arc::autorelease(item)
}

unsafe extern "C" fn reload_data(receiver: Id, _sel: Sel) {
    for key in elements(send_id(items(receiver), "allKeys")) {
        take_item(receiver, key);
    }
    load_counts(receiver);
    relayout(receiver);
}

unsafe extern "C" fn number_of_sections(receiver: Id, _sel: Sel) -> isize {
    counts_of(receiver).len() as isize
}

unsafe extern "C" fn number_of_items_in_section(receiver: Id, _sel: Sel, section: isize) -> isize {
    counts_of(receiver)
        .get(section as usize)
        .map_or(0, |c| *c as isize)
}

unsafe extern "C" fn item_at_index_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    match path.is_null() {
        true => std::ptr::null_mut(),
        false => send_id_id(items(receiver), "objectForKey:", path),
    }
}

unsafe extern "C" fn index_path_for_item_of_view(receiver: Id, _sel: Sel, item: Id) -> Id {
    let items = items(receiver);
    elements(send_id(items, "allKeys"))
        .into_iter()
        .find(|key| send_id_id(items, "objectForKey:", *key) == item)
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn visible_items(receiver: Id, _sel: Sel) -> Id {
    send_id(items(receiver), "allValues")
}

unsafe extern "C" fn index_paths_for_visible_items(receiver: Id, _sel: Sel) -> Id {
    send_id_id(
        class_named(c"NSSet"),
        "setWithArray:",
        send_id(items(receiver), "allKeys"),
    )
}

unsafe extern "C" fn layout_attributes_for_item(receiver: Id, _sel: Sel, path: Id) -> Id {
    let layout = props::object(receiver, "collectionViewLayout");
    match layout.is_null() {
        true => std::ptr::null_mut(),
        false => send_id_id(layout, "layoutAttributesForItemAtIndexPath:", path),
    }
}

fn selection_of(view: Id) -> Id {
    props::object(view, "selectionIndexPaths")
}

unsafe extern "C" fn selection_index_paths(receiver: Id, _sel: Sel) -> Id {
    match selection_of(receiver) {
        none if none.is_null() => set_of(&[]),
        selection => selection,
    }
}

/// `-setSelectionIndexPaths:`, the one programmatic change of selection its
/// observers are told of.
unsafe extern "C" fn set_selection_index_paths(receiver: Id, _sel: Sel, paths: Id) {
    store_selection(receiver, paths);
}

/// Keep `paths` as the selection and tell each item whether it is selected.
fn store_selection(view: Id, paths: Id) {
    // SAFETY: the `copy` property setter's storage.
    unsafe { props::set_copied(view, objc::sel::get("setSelectionIndexPaths:"), paths) };
    let selected: HashSet<Path> = paths_in(paths).into_iter().collect();
    let items = items(view);
    for key in elements(send_id(items, "allKeys")) {
        let item = send_id_id(items, "objectForKey:", key);
        let wanted = path_of(key).is_some_and(|p| selected.contains(&p));
        if send_bool(item, "isSelected") != wanted {
            send_set_bool(item, "setSelected:", wanted);
        }
    }
    crate::backing::needs_display(view);
}

/// Change the selection through `-setSelectionIndexPaths:`, so its
/// observers are told, as a user's click and `-selectAll:` do.
fn select(view: Id, paths: &[Path]) {
    send_id_id(view, "setSelectionIndexPaths:", set_of_paths(paths));
}

/// Change the selection without telling its observers: AppKit's
/// `-selectItemsAtIndexPaths:scrollPosition:`, `-deselectItemsAtIndexPaths:`
/// and `-deselectAll:` send no change notification for `selectionIndexPaths`,
/// and programs that observe it (CrossOver) count on that.
fn select_quietly(view: Id, paths: &[Path]) {
    store_selection(view, set_of_paths(paths));
}

unsafe extern "C" fn select_items(receiver: Id, _sel: Sel, paths: Id, _position: usize) {
    let mut selection = match send_bool(receiver, "allowsMultipleSelection") {
        true => paths_in(selection_of(receiver)),
        false => Vec::new(),
    };
    selection.extend(paths_in(paths));
    selection.sort_unstable();
    selection.dedup();
    select_quietly(receiver, &selection);
}

unsafe extern "C" fn deselect_items(receiver: Id, _sel: Sel, paths: Id) {
    let gone = paths_in(paths);
    let mut selection = paths_in(selection_of(receiver));
    selection.retain(|p| !gone.contains(p));
    select_quietly(receiver, &selection);
}

unsafe extern "C" fn deselect_all(receiver: Id, _sel: Sel, _sender: Id) {
    select_quietly(receiver, &[]);
}

unsafe extern "C" fn select_all(receiver: Id, _sel: Sel, _sender: Id) {
    if !send_bool(receiver, "allowsMultipleSelection") {
        return;
    }
    let all: Vec<Path> = counts_of(receiver)
        .iter()
        .enumerate()
        .flat_map(|(section, count)| (0..*count).map(move |item| (section, item)))
        .collect();
    select(receiver, &all);
}

/// `-allowsEmptySelection`, kept inverted so a fresh view allows it.
unsafe extern "C" fn allows_empty_selection(receiver: Id, _sel: Sel) -> u8 {
    (props::integer(receiver, "_rosemaryRequiresSelection") == 0) as u8
}

unsafe extern "C" fn set_allows_empty_selection(receiver: Id, _sel: Sel, allows: u8) {
    props::set_integer_of(
        receiver,
        "_rosemaryRequiresSelection",
        (allows == 0) as usize,
    );
}

unsafe extern "C" fn background_colors(receiver: Id, _sel: Sel) -> Id {
    let colors = props::object(receiver, "backgroundColors");
    if !colors.is_null() {
        return colors;
    }
    let color = send_id(class_named(c"NSColor"), "controlBackgroundColor");
    array(&[color])
}

unsafe extern "C" fn set_background_colors(receiver: Id, sel: Sel, colors: Id) {
    // SAFETY: the `copy` property setter.
    unsafe { props::set_copied(receiver, sel, colors) };
    crate::backing::needs_display(receiver);
}

/// FIXME (ACCURACY): kept only; nothing is dragged out of a collection view.
unsafe extern "C" fn set_dragging_source_operation_mask(
    receiver: Id,
    _sel: Sel,
    mask: usize,
    local: u8,
) {
    let name = match local != 0 {
        true => "_rosemaryLocalDragMask",
        false => "_rosemaryRemoteDragMask",
    };
    props::set_integer_of(receiver, name, mask);
}

/// `-drawRect:`: the first background colour behind the items.
unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, dirty: NSRect) {
    let Some(color) = elements(send_id(receiver, "backgroundColors"))
        .first()
        .copied()
    else {
        return;
    };
    send_word(color, "set");
    crate::color::NSRectFill(dirty);
}

/// The index path of the item whose frame holds `point`.
fn path_at(view: Id, point: NSPoint) -> Option<Path> {
    let items = items(view);
    elements(send_id(items, "allKeys"))
        .into_iter()
        .find(|key| {
            let item = send_id_id(items, "objectForKey:", *key);
            let item_view = send_id(item, "view");
            !item_view.is_null()
                && !send_bool(item_view, "isHidden")
                // SAFETY: plain geometry.
                && unsafe {
                    crate::geometry::NSPointInRect(point, send_rect(item_view, "frame"))
                } != 0
        })
        .and_then(path_of)
}

/// Ask the delegate which of `paths` may change, through `selector`.
fn allowed(view: Id, selector: &str, paths: Vec<Path>) -> Vec<Path> {
    let delegate = props::object(view, "delegate");
    if paths.is_empty() || !responds(delegate, selector) {
        return paths;
    }
    paths_in(send_id_id_id(
        delegate,
        selector,
        view,
        set_of_paths(&paths),
    ))
}

fn tell(view: Id, selector: &str, paths: &[Path]) {
    let delegate = props::object(view, "delegate");
    if !paths.is_empty() && responds(delegate, selector) {
        send_id_id_id(delegate, selector, view, set_of_paths(paths));
    }
}

/// `-mouseDown:`: a click selects the item under it, or with Command toggles
/// it, as the delegate allows.
unsafe extern "C" fn mouse_down(receiver: Id, _sel: Sel, event: Id) {
    let window = send_id(receiver, "window");
    if !window.is_null() && send_bool(receiver, "acceptsFirstResponder") {
        send_id_id(window, "makeFirstResponder:", receiver);
    }
    if !send_bool(receiver, "isSelectable") {
        return;
    }
    // SAFETY: `-locationInWindow` answers an `NSPoint`, and
    // `-convertPoint:fromView:` is the view's own.
    let point = unsafe {
        let location: unsafe extern "C" fn(Id, Sel) -> NSPoint =
            std::mem::transmute(objc::msg::entry());
        let convert: unsafe extern "C" fn(Id, Sel, NSPoint, Id) -> NSPoint =
            std::mem::transmute(objc::msg::entry());
        convert(
            receiver,
            objc::sel::get("convertPoint:fromView:"),
            location(event, objc::sel::get("locationInWindow")),
            std::ptr::null_mut(),
        )
    };
    let flags = send_word(event, "modifierFlags");
    let extending =
        flags & (COMMAND | SHIFT) != 0 && send_bool(receiver, "allowsMultipleSelection");
    let old = paths_in(selection_of(receiver));
    let new: Vec<Path> = match (path_at(receiver, point), extending) {
        (Some(path), true) if old.contains(&path) && flags & COMMAND != 0 => {
            old.iter().copied().filter(|p| *p != path).collect()
        }
        (Some(path), true) => old.iter().copied().chain([path]).collect(),
        (Some(path), false) => vec![path],
        (None, true) => old.clone(),
        (None, false) => match send_bool(receiver, "allowsEmptySelection") {
            true => Vec::new(),
            false => old.clone(),
        },
    };
    let deselecting = allowed(
        receiver,
        "collectionView:shouldDeselectItemsAtIndexPaths:",
        old.iter().copied().filter(|p| !new.contains(p)).collect(),
    );
    let selecting = allowed(
        receiver,
        "collectionView:shouldSelectItemsAtIndexPaths:",
        new.iter().copied().filter(|p| !old.contains(p)).collect(),
    );
    if deselecting.is_empty() && selecting.is_empty() {
        return;
    }
    let mut selection: Vec<Path> = old
        .into_iter()
        .filter(|p| !deselecting.contains(p))
        .chain(selecting.iter().copied())
        .collect();
    selection.sort_unstable();
    selection.dedup();
    select(receiver, &selection);
    tell(
        receiver,
        "collectionView:didDeselectItemsAtIndexPaths:",
        &deselecting,
    );
    tell(
        receiver,
        "collectionView:didSelectItemsAtIndexPaths:",
        &selecting,
    );
}

/// NSView's own implementation of `sel`, for an override to run first.
fn view_implementation(sel: &str) -> objc::class::Imp {
    objc::class::lookup(c"NSView")
        .and_then(|c| c.method(objc::sel::get(sel)))
        .expect("NSView's geometry methods are installed first")
        .implementation()
}

/// A change of width lays the items out again.
unsafe extern "C" fn set_frame_size(receiver: Id, sel: Sel, size: NSSize) {
    let before = crate::view::bounds_of(receiver).size.width;
    // SAFETY: NSView's `-setFrameSize:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSSize) =
            std::mem::transmute(view_implementation("setFrameSize:"));
        f(receiver, sel, size);
    }
    if crate::view::bounds_of(receiver).size.width != before {
        relayout(receiver);
    }
}

unsafe extern "C" fn set_frame(receiver: Id, sel: Sel, frame: NSRect) {
    let before = crate::view::bounds_of(receiver).size.width;
    // SAFETY: NSView's `-setFrame:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) =
            std::mem::transmute(view_implementation("setFrame:"));
        f(receiver, sel, frame);
    }
    if crate::view::bounds_of(receiver).size.width != before {
        relayout(receiver);
    }
}

pub(crate) fn install() {
    let index_path = objc::class::lookup(c"NSIndexPath").expect("Foundation's NSIndexPath");
    class_methods!(index_path, {
        "indexPathForItem:inSection:" => index_path_for_item, c"@32@0:8q16q24";
    });
    methods!(index_path, {
        "item" => index_path_item, c"q16@0:8";
        "section" => index_path_section, c"q16@0:8";
    });

    let attributes_class = objc::class::lookup(c"NSCollectionViewLayoutAttributes")
        .expect("declared before its methods");
    class_methods!(attributes_class, {
        "layoutAttributesForItemWithIndexPath:" => attributes_for_item, c"@24@0:8@16";
    });
    methods!(attributes_class, {
        "init" => attributes_init, c"@16@0:8";
        "dealloc" => attributes_dealloc, c"v16@0:8";
        "copyWithZone:" => attributes_copy, c"@24@0:8^v16";
        "frame" => attributes_frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setFrame:" => attributes_set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "size" => attributes_size, c"{CGSize=dd}16@0:8";
        "setSize:" => attributes_set_size, c"v32@0:8{CGSize=dd}16";
        "alpha" => attributes_alpha, c"d16@0:8";
        "setAlpha:" => attributes_set_alpha, c"v24@0:8d16";
        "zIndex" => attributes_z_index, c"q16@0:8";
        "setZIndex:" => attributes_set_z_index, c"v24@0:8q16";
        "isHidden" => attributes_is_hidden, c"c16@0:8";
        "setHidden:" => attributes_set_hidden, c"v20@0:8c16";
        "indexPath" => attributes_index_path, c"@16@0:8";
        "setIndexPath:" => attributes_set_index_path, c"v24@0:8@16";
        "representedElementCategory" => attributes_category, c"q16@0:8";
        "representedElementKind" => nil, c"@16@0:8";
    });

    let layout =
        objc::class::lookup(c"NSCollectionViewLayout").expect("declared before its methods");
    class_methods!(layout, {
        "layoutAttributesClass" => layout_attributes_class, c"#16@0:8";
    });
    methods!(layout, {
        "collectionView" => props::get_object, c"@16@0:8";
        "prepareLayout" => nothing, c"v16@0:8";
        "invalidateLayout" => invalidate_layout, c"v16@0:8";
        "collectionViewContentSize" => no_content_size, c"{CGSize=dd}16@0:8";
        "layoutAttributesForElementsInRect:" => no_attributes_in_rect,
        c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "layoutAttributesForItemAtIndexPath:" => no_attributes, c"@24@0:8@16";
    });

    let grid_class =
        objc::class::lookup(c"NSCollectionViewGridLayout").expect("declared before its methods");
    methods!(grid_class, {
        "collectionViewContentSize" => grid_content_size, c"{CGSize=dd}16@0:8";
        "layoutAttributesForElementsInRect:" => grid_attributes_in_rect,
        c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "layoutAttributesForItemAtIndexPath:" => grid_attributes_for_item, c"@24@0:8@16";
        "minimumInteritemSpacing" => minimum_interitem_spacing, c"d16@0:8";
        "setMinimumInteritemSpacing:" => set_minimum_interitem_spacing, c"v24@0:8d16";
        "minimumLineSpacing" => minimum_line_spacing, c"d16@0:8";
        "setMinimumLineSpacing:" => set_minimum_line_spacing, c"v24@0:8d16";
        "minimumItemSize" => minimum_item_size, c"{CGSize=dd}16@0:8";
        "setMinimumItemSize:" => set_minimum_item_size, c"v32@0:8{CGSize=dd}16";
        "maximumItemSize" => maximum_item_size, c"{CGSize=dd}16@0:8";
        "setMaximumItemSize:" => set_maximum_item_size, c"v32@0:8{CGSize=dd}16";
        "maximumNumberOfRows" => maximum_number_of_rows, c"Q16@0:8";
        "setMaximumNumberOfRows:" => set_maximum_number_of_rows, c"v24@0:8Q16";
        "maximumNumberOfColumns" => maximum_number_of_columns, c"Q16@0:8";
        "setMaximumNumberOfColumns:" => set_maximum_number_of_columns, c"v24@0:8Q16";
        "margins" => margins, c"{NSEdgeInsets=dddd}16@0:8";
        "setMargins:" => set_margins, c"v48@0:8{NSEdgeInsets=dddd}16";
    });
    properties!(grid_class, {
        copied "backgroundColors", "setBackgroundColors:";
    });

    let view = objc::class::lookup(c"NSCollectionView").expect("declared before its methods");
    properties!(view, {
        assigned "dataSource", "setDataSource:";
        assigned "delegate", "setDelegate:";
        bool "isSelectable", "setSelectable:";
        bool "allowsMultipleSelection", "setAllowsMultipleSelection:";
    });
    methods!(view, {
        "isFlipped" => is_flipped, c"c16@0:8";
        "collectionViewLayout" => props::get_object, c"@16@0:8";
        "setCollectionViewLayout:" => set_collection_view_layout, c"v24@0:8@16";
        "registerClass:forItemWithIdentifier:" => register_class, c"v32@0:8#16@24";
        "makeItemWithIdentifier:forIndexPath:" => make_item, c"@32@0:8@16@24";
        "reloadData" => reload_data, c"v16@0:8";
        "numberOfSections" => number_of_sections, c"q16@0:8";
        "numberOfItemsInSection:" => number_of_items_in_section, c"q24@0:8q16";
        "itemAtIndexPath:" => item_at_index_path, c"@24@0:8@16";
        "indexPathForItem:" => index_path_for_item_of_view, c"@24@0:8@16";
        "visibleItems" => visible_items, c"@16@0:8";
        "indexPathsForVisibleItems" => index_paths_for_visible_items, c"@16@0:8";
        "layoutAttributesForItemAtIndexPath:" => layout_attributes_for_item, c"@24@0:8@16";
        "selectionIndexPaths" => selection_index_paths, c"@16@0:8";
        "setSelectionIndexPaths:" => set_selection_index_paths, c"v24@0:8@16";
        "selectItemsAtIndexPaths:scrollPosition:" => select_items, c"v32@0:8@16Q24";
        "deselectItemsAtIndexPaths:" => deselect_items, c"v24@0:8@16";
        "selectAll:" => select_all, c"v24@0:8@16";
        "deselectAll:" => deselect_all, c"v24@0:8@16";
        "allowsEmptySelection" => allows_empty_selection, c"c16@0:8";
        "setAllowsEmptySelection:" => set_allows_empty_selection, c"v20@0:8c16";
        "backgroundColors" => background_colors, c"@16@0:8";
        "setBackgroundColors:" => set_background_colors, c"v24@0:8@16";
        "setDraggingSourceOperationMask:forLocal:" => set_dragging_source_operation_mask,
        c"v28@0:8Q16c24";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "mouseDown:" => mouse_down, c"v24@0:8@16";
        "setFrameSize:" => set_frame_size, c"v32@0:8{CGSize=dd}16";
        "setFrame:" => set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });

    let item = objc::class::lookup(c"NSCollectionViewItem").expect("declared before its methods");
    properties!(item, {
        assigned "collectionView", "setCollectionView:";
        integer "highlightState", "setHighlightState:";
        assigned "imageView", "setImageView:";
        assigned "textField", "setTextField:";
    });
    properties!(item, {
        bool "isSelected", "setSelected:";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NOTIFIED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_change(_: Id, _: Sel, _: Id, _: Id, _: Id, _: *mut c_void) {
        NOTIFIED.fetch_add(1, Ordering::SeqCst);
    }

    fn observed_view() -> Id {
        crate::bootstrap();
        let observer_class =
            corefoundation::base::register(c"RosemarySelectionObserver", None, 0, false);
        methods!(observer_class, {
            "observeValueForKeyPath:ofObject:change:context:" => count_change, c"v48@0:8@16@24@32^v40";
        });
        let observer = objc::object::create_instance(observer_class, 0);
        let view = send_id(send_id(class_named(c"NSCollectionView"), "alloc"), "init");
        // SAFETY: `-addObserver:forKeyPath:options:context:` is
        // `void (id, SEL, id, NSString *, NSUInteger, void *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, usize, *mut c_void) =
                std::mem::transmute(objc::msg::entry());
            f(
                view,
                objc::sel::get("addObserver:forKeyPath:options:context:"),
                observer,
                foundation::string::from_str("selectionIndexPaths"),
                0,
                std::ptr::null_mut(),
            );
        }
        view
    }

    #[test]
    fn only_setting_the_selection_notifies_its_observers() {
        let view = observed_view();
        let first = set_of_paths(&[(0, 0)]);
        // SAFETY: `-selectItemsAtIndexPaths:scrollPosition:` is
        // `void (id, SEL, NSSet *, NSCollectionViewScrollPosition)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, usize) =
                std::mem::transmute(objc::msg::entry());
            f(
                view,
                objc::sel::get("selectItemsAtIndexPaths:scrollPosition:"),
                first,
                0,
            );
        }
        assert_eq!(paths_in(selection_of(view)), [(0, 0)]);
        send_id_id(view, "deselectItemsAtIndexPaths:", first);
        send_id_id(view, "deselectAll:", std::ptr::null_mut());
        assert_eq!(NOTIFIED.load(Ordering::SeqCst), 0);
        send_id_id(view, "setSelectionIndexPaths:", first);
        assert_eq!(NOTIFIED.load(Ordering::SeqCst), 1);
    }
}
