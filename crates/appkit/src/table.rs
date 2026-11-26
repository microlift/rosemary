//! `NSTableView` and `NSTableColumn`, as a read-only list a data source or a
//! `content` binding fills.
//!
//! A cell-based table draws each column's value in its font and alignment. A
//! view-based one - which registered nibs for its cells, or whose delegate
//! makes their views - is made of a row view per row holding a view per
//! column, made whenever the rows change and kept as subviews; the table is
//! then as tall as its rows, for its scroll view to scroll.
//!
//! FIXME (ACCURACY): a click selects one row and drags extend nothing; no
//! editing, headers or grid are drawn; a cell-based table that does not fit
//! its clip view shows the rows `-scrollRowToVisible:` last asked for rather
//! than scrolling; and a view-based table makes every row's views, not only
//! the visible ones, and reuses none of them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use foundation::geometry::{NSPoint, NSRect};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::view;

#[derive(Clone, Default)]
struct Table {
    /// Unretained, as AppKit's are.
    data_source: usize,
    delegate: usize,
    /// Retained columns, in order.
    columns: Vec<usize>,
    row_height: f64,
    spacing_height: f64,
    /// Retained colour, or zero for white.
    background: usize,
    /// The row last asked to be visible.
    visible_row: Option<isize>,
    /// Selected rows, ascending.
    selected: Vec<isize>,
    /// Unretained, as a control's target is.
    target: usize,
    /// The selector a double click sends, or zero.
    double_action: usize,
    /// The row and column the last click landed on.
    clicked: Option<(isize, isize)>,
    /// Unretained: the header view is in the scroll view's tree, which holds it.
    header: usize,
    /// Retained: what a `content` binding gives the table - the arranged
    /// objects of a table, the root tree node of an outline - or zero.
    content: usize,
    /// The row views of a view-based table, each retained, by row.
    row_views: Vec<usize>,
    /// The top of each row of a view-based table, and the bottom of the last.
    row_tops: Vec<f64>,
    spacing_width: f64,
    /// `NSTableViewColumnAutoresizingStyle`.
    autoresizing_style: isize,
}

#[derive(Clone, Default)]
struct Column {
    /// Retained string, or zero.
    identifier: usize,
    width: f64,
    /// Retained font, or zero for the system font.
    font: usize,
    /// `NSTextAlignment`.
    alignment: isize,
    /// Retained string the header shows, or zero.
    title: usize,
}

fn tables() -> &'static Mutex<HashMap<usize, Table>> {
    static TABLES: OnceLock<Mutex<HashMap<usize, Table>>> = OnceLock::new();
    TABLES.get_or_init(Default::default)
}

fn columns() -> &'static Mutex<HashMap<usize, Column>> {
    static COLUMNS: OnceLock<Mutex<HashMap<usize, Column>>> = OnceLock::new();
    COLUMNS.get_or_init(Default::default)
}

fn table(receiver: Id) -> Table {
    tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(receiver as usize))
        .cloned()
        .unwrap_or(Table {
            row_height: 17.0,
            spacing_height: 2.0,
            spacing_width: 3.0,
            ..Default::default()
        })
}

fn update_table(receiver: Id, change: impl FnOnce(&mut Table)) {
    let current = table(receiver);
    let mut tables = tables().lock().unwrap_or_else(|e| e.into_inner());
    change(tables.entry(receiver as usize).or_insert(current));
    drop(tables);
    crate::backing::needs_display(receiver);
}

fn column(receiver: Id) -> Column {
    columns()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(receiver as usize))
        .cloned()
        .unwrap_or(Column {
            width: 100.0,
            ..Default::default()
        })
}

fn update_column(receiver: Id, change: impl FnOnce(&mut Column)) {
    let current = column(receiver);
    let mut columns = columns().lock().unwrap_or_else(|e| e.into_inner());
    change(columns.entry(receiver as usize).or_insert(current));
}

/// Store a retained object in `slot`, releasing what it held.
fn swap_retained(slot: &mut usize, value: Id) -> Id {
    std::mem::replace(slot, objc::arc::retain(value) as usize) as Id
}

/// Drop the state of a table that is going away.
pub(crate) fn forget(receiver: Id) {
    let gone = tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
    if let Some(table) = gone {
        table
            .columns
            .into_iter()
            .chain(table.row_views)
            .chain([table.background, table.content])
            .for_each(|o| objc::arc::release(o as Id));
    }
    crate::outline::forget(receiver);
}

pub(crate) fn bound_content(receiver: Id) -> Id {
    table(receiver).content as Id
}

pub(crate) fn data_source_of(receiver: Id) -> Id {
    table(receiver).data_source as Id
}

pub(crate) fn delegate_of(receiver: Id) -> Id {
    table(receiver).delegate as Id
}

pub(crate) fn column_index(receiver: Id, column: Id) -> Option<usize> {
    table(receiver).columns.iter().position(|c| *c == column as usize)
}

pub(crate) fn column_at(receiver: Id, index: usize) -> Id {
    table(receiver).columns.get(index).copied().unwrap_or(0) as Id
}

/// The `content` binding's value: a table's arranged objects, or the root
/// node of an outline's tree.
pub(crate) fn set_bound_content(receiver: Id, content: Id) {
    let mut old = std::ptr::null_mut();
    update_table(receiver, |t| old = swap_retained(&mut t.content, content));
    objc::arc::release(old);
    reload(receiver);
}

/// The `selectionIndexPaths` binding of an outline, or the
/// `selectionIndexes` binding of a table.
pub(crate) fn set_bound_selection(receiver: Id, value: Id) {
    let rows = match crate::outline::is_outline(receiver) {
        true => crate::outline::rows_of_paths(receiver, value),
        false => index_set_members(value),
    };
    change_selection(receiver, rows, Origin::Binding);
}

/// Work out the rows again and lay them out.
fn reload(receiver: Id) {
    if crate::outline::is_outline(receiver) {
        crate::outline::rebuild_rows(receiver);
    }
    rows_changed(receiver);
}

/// The rows changed: a view-based table makes their views again, and the
/// selection keeps only rows that still exist.
pub(crate) fn rows_changed(receiver: Id) {
    let count = rows(receiver);
    update_table(receiver, |t| t.selected.retain(|row| *row < count));
    if is_view_based(receiver) {
        tile(receiver);
    }
    crate::backing::needs_display(receiver);
}

unsafe extern "C" fn set_data_source(receiver: Id, _sel: Sel, source: Id) {
    update_table(receiver, |t| t.data_source = source as usize);
    reload(receiver);
}

unsafe extern "C" fn data_source(receiver: Id, _sel: Sel) -> Id {
    table(receiver).data_source as Id
}

unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    update_table(receiver, |t| t.delegate = delegate as usize);
    if is_view_based(receiver) {
        tile(receiver);
    }
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    table(receiver).delegate as Id
}

unsafe extern "C" fn set_row_height(receiver: Id, _sel: Sel, height: f64) {
    update_table(receiver, |t| t.row_height = height);
}

unsafe extern "C" fn row_height(receiver: Id, _sel: Sel) -> f64 {
    table(receiver).row_height
}

unsafe extern "C" fn set_background_color(receiver: Id, _sel: Sel, color: Id) {
    let mut old = std::ptr::null_mut();
    update_table(receiver, |t| old = swap_retained(&mut t.background, color));
    objc::arc::release(old);
}

unsafe extern "C" fn background_color(receiver: Id, _sel: Sel) -> Id {
    table(receiver).background as Id
}

unsafe extern "C" fn add_table_column(receiver: Id, _sel: Sel, added: Id) {
    if added.is_null() {
        return;
    }
    objc::arc::retain(added);
    update_table(receiver, |t| t.columns.push(added as usize));
}

unsafe extern "C" fn table_columns(receiver: Id, _sel: Sel) -> Id {
    let values: Vec<*const std::ffi::c_void> = table(receiver)
        .columns
        .iter()
        .map(|c| *c as *const _)
        .collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn number_of_columns(receiver: Id, _sel: Sel) -> isize {
    table(receiver).columns.len() as isize
}

/// `-numberOfRows`: the data source's `-numberOfRowsInTableView:`.
unsafe extern "C" fn number_of_rows(receiver: Id, _sel: Sel) -> isize {
    rows(receiver)
}

fn rows(receiver: Id) -> isize {
    if crate::outline::is_outline(receiver) {
        return crate::outline::row_count(receiver);
    }
    let content = bound_content(receiver);
    if !content.is_null() {
        return crate::tree_controller::send_word(content, "count") as isize;
    }
    let source = table(receiver).data_source as Id;
    let sel = objc::sel::get("numberOfRowsInTableView:");
    if !responds(source, sel) {
        return 0;
    }
    // SAFETY: `NSInteger (id, SEL, NSTableView *)`, as the protocol declares.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> isize = std::mem::transmute(objc::msg::entry());
        f(source, sel, receiver)
    }
}

fn responds(object: Id, sel: Sel) -> bool {
    // SAFETY: a live object, or nil.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| c.method(sel).is_some())
}

/// Whether anything gives the rows object values: a `content` binding or the
/// data source.
fn has_object_values(receiver: Id) -> bool {
    let source = table(receiver).data_source as Id;
    !bound_content(receiver).is_null()
        || responds(
            source,
            objc::sel::get(match crate::outline::is_outline(receiver) {
                true => "outlineView:objectValueForTableColumn:byItem:",
                false => "tableView:objectValueForTableColumn:row:",
            }),
        )
}

/// The object at `row` of `column`: the bound content's, or the data
/// source's.
fn object_value(receiver: Id, column: Id, row: isize) -> Id {
    if crate::outline::is_outline(receiver) {
        return crate::outline::object_value(receiver, column, row);
    }
    let content = bound_content(receiver);
    if !content.is_null() {
        // SAFETY: `id (id, SEL, NSUInteger)`.
        return unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(content, objc::sel::get("objectAtIndex:"), row as usize)
        };
    }
    let source = table(receiver).data_source as Id;
    let sel = objc::sel::get("tableView:objectValueForTableColumn:row:");
    if !responds(source, sel) {
        return std::ptr::null_mut();
    }
    // SAFETY: `id (id, SEL, NSTableView *, NSTableColumn *, NSInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, isize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(source, sel, receiver, column, row)
    }
}

/// The value at `row` of `column`, as the text the cell would show.
fn value(receiver: Id, column: Id, row: isize) -> String {
    let object = object_value(receiver, column, row);
    if object.is_null() {
        return String::new();
    }
    // SAFETY: `-description` is `id (id, SEL)` on every object.
    let text = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(object, objc::sel::get("description"))
    };
    // SAFETY: a string, or nil.
    String::from_utf8_lossy(&unsafe { foundation::string::bytes(text) }).into_owned()
}

unsafe extern "C" fn reload_data(receiver: Id, _sel: Sel) {
    reload(receiver);
}

unsafe extern "C" fn scroll_row_to_visible(receiver: Id, _sel: Sel, row: isize) {
    update_table(receiver, |t| t.visible_row = Some(row));
}

unsafe extern "C" fn selected_row(receiver: Id, _sel: Sel) -> isize {
    table(receiver).selected.last().copied().unwrap_or(-1)
}

unsafe extern "C" fn number_of_selected_rows(receiver: Id, _sel: Sel) -> isize {
    table(receiver).selected.len() as isize
}

unsafe extern "C" fn is_row_selected(receiver: Id, _sel: Sel, row: isize) -> u8 {
    table(receiver).selected.contains(&row) as u8
}

/// The members of an `NSIndexSet`, walked with `-firstIndex` and
/// `-indexGreaterThanIndex:`.
fn index_set_members(set: Id) -> Vec<isize> {
    let mut rows = Vec::new();
    if set.is_null() {
        return rows;
    }
    let first = objc::sel::get("firstIndex");
    let next = objc::sel::get("indexGreaterThanIndex:");
    if !responds(set, first) || !responds(set, next) {
        return rows;
    }
    // SAFETY: `NSUInteger (id, SEL)` and `NSUInteger (id, SEL, NSUInteger)`.
    unsafe {
        let get_first: unsafe extern "C" fn(Id, Sel) -> usize =
            std::mem::transmute(objc::msg::entry());
        let get_next: unsafe extern "C" fn(Id, Sel, usize) -> usize =
            std::mem::transmute(objc::msg::entry());
        let mut index = get_first(set, first);
        while index < isize::MAX as usize && rows.len() < 1 << 20 {
            rows.push(index as isize);
            index = get_next(set, next, index);
        }
    }
    rows
}

unsafe extern "C" fn select_row_indexes(receiver: Id, _sel: Sel, set: Id, extend: u8) {
    let mut rows = index_set_members(set);
    if extend != 0 {
        rows.extend(table(receiver).selected);
    }
    change_selection(receiver, rows, Origin::Program);
}

/// What changed a table's selection, which decides who is told.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    /// The user, or the program: the selection binding hears of it.
    Program,
    /// The selection binding, which needs no telling.
    Binding,
}

/// Select `rows`, showing it in the row views, and tell the selection
/// binding, the delegate and anyone listening if that changed anything.
fn change_selection(receiver: Id, mut selected: Vec<isize>, origin: Origin) {
    let count = rows(receiver);
    selected.retain(|row| (0..count).contains(row));
    selected.sort_unstable();
    selected.dedup();
    if table(receiver).selected == selected {
        return;
    }
    let rows = selected;
    update_table(receiver, |t| t.selected = rows.clone());
    for (row, view) in table(receiver).row_views.iter().enumerate() {
        let selected = rows.contains(&(row as isize));
        // SAFETY: `void (id, SEL, BOOL)` on a row view.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(*view as Id, objc::sel::get("setSelected:"), selected as u8);
        }
        refresh_background_styles(*view as Id);
    }
    let outline = crate::outline::is_outline(receiver);
    if origin == Origin::Program {
        let (name, value) = match outline {
            true => (
                "selectionIndexPaths",
                crate::outline::paths_of_rows(receiver, &rows),
            ),
            false => ("selectionIndexes", index_set_of(&rows)),
        };
        crate::bindings::push(receiver, name, value);
    }
    let (name, selector) = match outline {
        true => (
            "NSOutlineViewSelectionDidChangeNotification",
            "outlineViewSelectionDidChange:",
        ),
        false => (
            "NSTableViewSelectionDidChangeNotification",
            "tableViewSelectionDidChange:",
        ),
    };
    // AppKit registers the delegate for this notification; nothing here
    // keeps that registration, so the delegate is told directly.
    let delegate = table(receiver).delegate as Id;
    let changed = objc::sel::get(selector);
    if responds(delegate, changed) {
        let notification = foundation::notification::make(name, receiver, std::ptr::null_mut());
        // SAFETY: `void (id, SEL, NSNotification *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(delegate, changed, notification);
        }
    }
    foundation::notification::post(name, receiver, std::ptr::null_mut());
}

/// A new, autoreleased `NSIndexSet` of `rows`.
fn index_set_of(rows: &[isize]) -> Id {
    let class = objc::class::lookup(c"NSMutableIndexSet").expect("declared by Foundation");
    // SAFETY: `+indexSet` is `id (Class, SEL)`, `-addIndex:` is
    // `void (id, SEL, NSUInteger)`.
    unsafe {
        let make: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        let set = make(objc::class::ptr(class) as Id, objc::sel::get("indexSet"));
        let add: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
        for row in rows {
            add(set, objc::sel::get("addIndex:"), *row as usize);
        }
        set
    }
}

unsafe extern "C" fn selected_row_indexes(receiver: Id, _sel: Sel) -> Id {
    index_set_of(&table(receiver).selected)
}

unsafe extern "C" fn deselect_all(receiver: Id, _sel: Sel, _sender: Id) {
    change_selection(receiver, Vec::new(), Origin::Program);
}

unsafe extern "C" fn deselect_row(receiver: Id, _sel: Sel, row: isize) {
    let mut rows = table(receiver).selected;
    rows.retain(|r| *r != row);
    change_selection(receiver, rows, Origin::Program);
}

unsafe extern "C" fn set_target(receiver: Id, _sel: Sel, target: Id) {
    update_table(receiver, |t| t.target = target as usize);
}

unsafe extern "C" fn target(receiver: Id, _sel: Sel) -> Id {
    table(receiver).target as Id
}

unsafe extern "C" fn set_double_action(receiver: Id, _sel: Sel, action: Sel) {
    update_table(receiver, |t| t.double_action = action as usize);
}

unsafe extern "C" fn double_action(receiver: Id, _sel: Sel) -> Sel {
    table(receiver).double_action as Sel
}

unsafe extern "C" fn clicked_row(receiver: Id, _sel: Sel) -> isize {
    table(receiver).clicked.map_or(-1, |(row, _)| row)
}

unsafe extern "C" fn clicked_column(receiver: Id, _sel: Sel) -> isize {
    table(receiver).clicked.map_or(-1, |(_, column)| column)
}

/// Where the rows start: the offset that keeps the row last asked for in
/// sight.
fn row_offset(receiver: Id, state: &Table, count: isize) -> f64 {
    let bounds = view::bounds_of(receiver);
    let pitch = state.row_height + state.spacing_height;
    // SAFETY: a view of this crate's.
    let visible = match unsafe { view::superview_of(receiver) } {
        clip if !clip.is_null() => view::bounds_of(clip).size.height,
        _ => bounds.size.height,
    }
    .min(bounds.size.height);
    scroll_offset(state.visible_row, pitch, count as f64 * pitch, visible)
}

/// Make `row` the one selected row, if the delegate agrees.
fn select_by_click(receiver: Id, row: isize) {
    let state = table(receiver);
    if state.selected == [row] {
        return;
    }
    let delegate = state.delegate as Id;
    // SAFETY: `BOOL (id, SEL, NSTableView *, NSInteger)` and
    // `BOOL (id, SEL, NSOutlineView *, id)`, as the protocols declare.
    let allowed = unsafe {
        match crate::outline::is_outline(receiver) {
            true => {
                let should = objc::sel::get("outlineView:shouldSelectItem:");
                !responds(delegate, should) || {
                    let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 =
                        std::mem::transmute(objc::msg::entry());
                    f(
                        delegate,
                        should,
                        receiver,
                        crate::outline::item_at(receiver, row),
                    ) != 0
                }
            }
            false => {
                let should = objc::sel::get("tableView:shouldSelectRow:");
                !responds(delegate, should) || {
                    let f: unsafe extern "C" fn(Id, Sel, Id, isize) -> u8 =
                        std::mem::transmute(objc::msg::entry());
                    f(delegate, should, receiver, row) != 0
                }
            }
        }
    };
    if allowed {
        change_selection(receiver, vec![row], Origin::Program);
    }
}

/// `-mouseDown:`: the row under the pointer is selected, and a double click
/// sends the double action.
unsafe extern "C" fn mouse_down(receiver: Id, _sel: Sel, event: Id) {
    // SAFETY: `-locationInWindow` answers an `NSPoint`, `-clickCount` an
    // `NSInteger`, and `-convertPoint:fromView:` is the view's own.
    let (point, clicks) = unsafe {
        let location: unsafe extern "C" fn(Id, Sel) -> NSPoint =
            std::mem::transmute(objc::msg::entry());
        let convert: unsafe extern "C" fn(Id, Sel, NSPoint, Id) -> NSPoint =
            std::mem::transmute(objc::msg::entry());
        let count: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        (
            convert(
                receiver,
                objc::sel::get("convertPoint:fromView:"),
                location(event, objc::sel::get("locationInWindow")),
                std::ptr::null_mut(),
            ),
            count(event, objc::sel::get("clickCount")),
        )
    };
    let state = table(receiver);
    let bounds = view::bounds_of(receiver);
    let from_top = match view::is_flipped(receiver) {
        true => point.y - bounds.origin.y,
        false => bounds.origin.y + bounds.size.height - point.y,
    };
    let count = rows(receiver);
    let pitch = state.row_height + state.spacing_height;
    let row = match (state.row_tops.is_empty(), pitch > 0.0) {
        (false, _) => row_at(&state, from_top),
        (true, true) => ((from_top + row_offset(receiver, &state, count)) / pitch).floor() as isize,
        (true, false) => -1,
    };
    let row = if (0..count).contains(&row) { row } else { -1 };
    let mut x = bounds.origin.x;
    let column = state
        .columns
        .iter()
        .position(|c| {
            x += column(*c as Id).width + state.spacing_width;
            point.x < x
        })
        .map_or(-1, |at| at as isize);
    update_table(receiver, |t| t.clicked = Some((row, column)));
    if row >= 0 {
        select_by_click(receiver, row);
    }
    let app = crate::application::shared();
    if clicks >= 2 && state.double_action != 0 && !app.is_null() {
        // SAFETY: `-sendAction:to:from:` is `BOOL (id, SEL, SEL, id, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Sel, Id, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(
                app,
                objc::sel::get("sendAction:to:from:"),
                state.double_action as Sel,
                state.target as Id,
                receiver,
            );
        }
    }
}

/// The vertical offset that puts `visible_row` inside a clip view of
/// `visible` points, for a table `total` points tall.
fn scroll_offset(row: Option<isize>, pitch: f64, total: f64, visible: f64) -> f64 {
    let Some(row) = row.filter(|row| *row >= 0) else {
        return 0.0;
    };
    let bottom = (row as f64 + 1.0) * pitch;
    (bottom - visible).clamp(0.0, (total - visible).max(0.0))
}

unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, dirty: NSRect) {
    if is_view_based(receiver) {
        // The row views draw the rows; the table only its background, which
        // a source list leaves to the sidebar material behind it.
        let color = table(receiver).background as Id;
        if !color.is_null() && highlight_style(receiver) != HIGHLIGHT_SOURCE_LIST {
            // SAFETY: an `NSColor`, sent `-set`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                f(color, objc::sel::get("set"));
            }
            crate::color::NSRectFill(dirty);
        }
        return;
    }
    let bounds = view::bounds_of(receiver);
    let density = crate::backing::focus_density();
    let width = (bounds.size.width * density).ceil() as usize;
    let height = (bounds.size.height * density).ceil() as usize;
    if width == 0 || height == 0 {
        return;
    }
    let state = table(receiver);
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = chrome::Canvas::new(&mut pixels, width, height, width * 4, density as f32);
    let (w, h) = (bounds.size.width as f32, bounds.size.height as f32);
    let fill = crate::label::color_of(state.background, chrome::Color::rgba(255, 255, 255, 1.0));
    canvas.fill_rect(chrome::Rect::new(0.0, 0.0, w, h), fill);
    let pitch = state.row_height + state.spacing_height;
    let count = rows(receiver);
    let offset = row_offset(receiver, &state, count) as f32;
    for row in &state.selected {
        let top = *row as f32 * pitch as f32 - offset;
        if *row < count && top < h && top + pitch as f32 > 0.0 {
            canvas.fill_rect(
                chrome::Rect::new(0.0, top, w, pitch as f32),
                chrome::Color::rgba(0, 99, 225, 1.0),
            );
        }
    }
    let mut x = 0.0f32;
    for column_object in &state.columns {
        let column_state = column(*column_object as Id);
        let column_width = column_state.width as f32;
        if let Some((font, size)) = crate::label::chrome_font(column_state.font as Id) {
            let size = size as f32;
            for row in 0..count {
                let top = row as f32 * pitch as f32 - offset;
                if top > h || top + pitch as f32 <= 0.0 {
                    continue;
                }
                let text = value(receiver, *column_object as Id, row);
                let line = font.truncate(&text, size, (column_width - 4.0).max(0.0));
                let advance = font.measure(&line, size);
                let left = match column_state.alignment {
                    1 => x + column_width - 2.0 - advance,
                    2 => x + (column_width - advance) / 2.0,
                    _ => x + 2.0,
                };
                let baseline = font.centered_baseline(size, top, state.row_height as f32);
                canvas.draw_text(
                    &line,
                    &font,
                    size,
                    chrome::Point::new(left, baseline),
                    match state.selected.contains(&row) {
                        true => chrome::Color::rgba(255, 255, 255, 1.0),
                        false => chrome::Color::rgba(0, 0, 0, 0.85),
                    },
                );
            }
        }
        x += column_width + 3.0;
    }
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        bounds.size,
        bounds,
        NSRect {
            origin: NSPoint::default(),
            size: bounds.size,
        },
        // Source-over.
        2,
        1.0,
        true,
    );
}

unsafe extern "C" fn column_set_identifier(receiver: Id, _sel: Sel, identifier: Id) {
    let mut old = std::ptr::null_mut();
    update_column(receiver, |c| {
        old = swap_retained(&mut c.identifier, identifier)
    });
    objc::arc::release(old);
}

unsafe extern "C" fn column_identifier(receiver: Id, _sel: Sel) -> Id {
    column(receiver).identifier as Id
}

unsafe extern "C" fn column_init_with_identifier(receiver: Id, sel: Sel, identifier: Id) -> Id {
    // SAFETY: the same column.
    unsafe { column_set_identifier(receiver, sel, identifier) };
    receiver
}

unsafe extern "C" fn column_set_title(receiver: Id, _sel: Sel, title: Id) {
    let mut old = std::ptr::null_mut();
    update_column(receiver, |c| old = swap_retained(&mut c.title, title));
    objc::arc::release(old);
}

/// `-title`: the empty string for a column with none.
unsafe extern "C" fn column_title(receiver: Id, _sel: Sel) -> Id {
    match column(receiver).title as Id {
        title if title.is_null() => objc::arc::autorelease(foundation::string::from_str("")),
        title => title,
    }
}

unsafe extern "C" fn column_set_width(receiver: Id, _sel: Sel, width: f64) {
    update_column(receiver, |c| c.width = width);
}

unsafe extern "C" fn column_width(receiver: Id, _sel: Sel) -> f64 {
    column(receiver).width
}

/// What a nib's `NSTableColumn` and its data cell hold, for
/// [`crate::decode`].
pub(crate) fn set_archived_column(receiver: Id, width: f64, font: Id, alignment: isize) {
    let font = objc::arc::retain(font) as usize;
    update_column(receiver, |c| {
        c.width = width;
        c.font = font;
        c.alignment = alignment;
    });
}

fn headers() -> &'static Mutex<HashMap<usize, usize>> {
    static HEADERS: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    HEADERS.get_or_init(Default::default)
}

/// Drop what a header view knew of its table, as it goes away.
pub(crate) fn forget_header(receiver: Id) {
    headers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
}

fn table_of_header(receiver: Id) -> Id {
    headers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(receiver as usize))
        .copied()
        .unwrap_or(0) as Id
}

/// `-setHeaderView:`: the header shows this table's columns.
unsafe extern "C" fn set_header_view(receiver: Id, _sel: Sel, header: Id) {
    let old = table(receiver).header as Id;
    if !old.is_null() {
        forget_header(old);
    }
    update_table(receiver, |t| t.header = header as usize);
    if !header.is_null() {
        headers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(header as usize, receiver as usize);
        crate::backing::needs_display(header);
    }
}

unsafe extern "C" fn header_view(receiver: Id, _sel: Sel) -> Id {
    table(receiver).header as Id
}

/// `-[NSTableHeaderView tableView]`.
unsafe extern "C" fn header_table_view(receiver: Id, _sel: Sel) -> Id {
    table_of_header(receiver)
}

/// `-[NSTableHeaderView drawRect:]`: each column's title over it, laid out
/// as the table lays out its columns.
unsafe extern "C" fn header_draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
    let bounds = view::bounds_of(receiver);
    let density = crate::backing::focus_density();
    let width = (bounds.size.width * density).ceil() as usize;
    let height = (bounds.size.height * density).ceil() as usize;
    if width == 0 || height == 0 {
        return;
    }
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = chrome::Canvas::new(&mut pixels, width, height, width * 4, density as f32);
    let (w, h) = (bounds.size.width as f32, bounds.size.height as f32);
    let edge = chrome::Color::rgba(0, 0, 0, 0.15);
    canvas.fill_rect(
        chrome::Rect::new(0.0, 0.0, w, h),
        chrome::Color::rgba(255, 255, 255, 1.0),
    );
    canvas.fill_rect(chrome::Rect::new(0.0, h - 1.0, w, 1.0), edge);
    let state = table(table_of_header(receiver));
    let font = crate::label::chrome_font(std::ptr::null_mut());
    let mut x = 0.0f32;
    for column_object in &state.columns {
        let column_state = column(*column_object as Id);
        let column_width = column_state.width as f32 + 3.0;
        // SAFETY: a string this column retained, or nil.
        let title = String::from_utf8_lossy(&unsafe {
            foundation::string::bytes(column_state.title as Id)
        })
        .into_owned();
        if let Some((font, size)) = &font {
            let size = (*size as f32).min(11.0);
            let line = font.truncate(&title, size, (column_width - 8.0).max(0.0));
            let baseline = font.centered_baseline(size, 0.0, h);
            canvas.draw_text(
                &line,
                font,
                size,
                chrome::Point::new(x + 4.0, baseline),
                chrome::Color::rgba(0, 0, 0, 0.85),
            );
        }
        x += column_width;
        canvas.fill_rect(chrome::Rect::new(x - 1.0, 3.0, 1.0, h - 6.0), edge);
    }
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        bounds.size,
        bounds,
        NSRect {
            origin: NSPoint::default(),
            size: bounds.size,
        },
        // Source-over.
        2,
        1.0,
        true,
    );
}

/// `-sortDescriptors`: none until some are set.
unsafe extern "C" fn sort_descriptors(receiver: Id, _sel: Sel) -> Id {
    let kept = crate::props::object(receiver, "sortDescriptors");
    if !kept.is_null() {
        return kept;
    }
    // SAFETY: `+[NSArray array]`.
    unsafe {
        let class = objc::class::lookup(c"NSArray").expect("NSArray");
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(objc::class::ptr(class) as Id, objc::sel::get("array"))
    }
}

// View-based tables.

/// `NSTableViewSelectionHighlightStyle`.
const HIGHLIGHT_SOURCE_LIST: isize = 1;

fn highlight_style(receiver: Id) -> isize {
    crate::props::integer(receiver, "selectionHighlightStyle") as isize
}

unsafe extern "C" fn set_selection_highlight_style(receiver: Id, _sel: Sel, style: isize) {
    crate::props::set_integer_of(receiver, "selectionHighlightStyle", style as usize);
    for row_view in table(receiver).row_views {
        set_row_highlight_style(row_view as Id, style);
    }
    crate::backing::needs_display(receiver);
}

fn set_row_highlight_style(row_view: Id, style: isize) {
    // SAFETY: `void (id, SEL, NSTableViewSelectionHighlightStyle)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc::msg::entry());
        f(row_view, objc::sel::get("setSelectionHighlightStyle:"), style);
    }
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

fn send_rect(receiver: Id, sel: &str, rect: NSRect) {
    // SAFETY: `void (id, SEL, NSRect)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), rect);
    }
}

/// The nibs registered for cell views, by identifier: a dictionary the table
/// owns.
fn registered_nibs(receiver: Id) -> Id {
    let found = crate::props::object(receiver, "registeredNibsByIdentifier");
    if !found.is_null() {
        return found;
    }
    let class = objc::class::lookup(c"NSMutableDictionary").expect("NSMutableDictionary");
    let made = send_id(objc::class::ptr(class) as Id, "dictionary");
    // SAFETY: the strong property setter.
    unsafe {
        crate::props::set_retained(
            receiver,
            objc::sel::get("setRegisteredNibsByIdentifier:"),
            made,
        )
    };
    made
}

unsafe extern "C" fn registered_nibs_by_identifier(receiver: Id, _sel: Sel) -> Id {
    registered_nibs(receiver)
}

unsafe extern "C" fn register_nib(receiver: Id, _sel: Sel, nib: Id, identifier: Id) {
    let nibs = registered_nibs(receiver);
    match nib.is_null() {
        true => send_id_id(nibs, "removeObjectForKey:", identifier),
        // SAFETY: `void (id, SEL, id, id)`.
        false => unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
            f(nibs, objc::sel::get("setObject:forKey:"), nib, identifier);
            std::ptr::null_mut()
        },
    };
}

/// Whether the table is made of views: it has nibs for them, or its delegate
/// makes them.
fn is_view_based(receiver: Id) -> bool {
    let nibs = crate::props::object(receiver, "registeredNibsByIdentifier");
    let delegate = table(receiver).delegate as Id;
    (!nibs.is_null() && crate::tree_controller::send_word(nibs, "count") != 0)
        || responds(
            delegate,
            objc::sel::get(match crate::outline::is_outline(receiver) {
                true => "outlineView:viewForTableColumn:item:",
                false => "tableView:viewForTableColumn:row:",
            }),
        )
}

/// `-makeViewWithIdentifier:owner:`: a new view from the nib registered for
/// `identifier`, wired to `owner`; nil when there is none.
///
/// FIXME (ACCURACY): every call instantiates the nib again; nothing is reused.
unsafe extern "C" fn make_view(receiver: Id, _sel: Sel, identifier: Id, owner: Id) -> Id {
    if identifier.is_null() {
        return std::ptr::null_mut();
    }
    let nib = send_id_id(registered_nibs(receiver), "objectForKey:", identifier);
    if nib.is_null() {
        return std::ptr::null_mut();
    }
    let Some(objects) = crate::nib::instantiate_nib(nib, owner) else {
        return std::ptr::null_mut();
    };
    let views: Vec<Id> = objects
        .into_iter()
        .filter(|object| view::is_view(*object))
        .collect();
    let same = |view: Id| {
        let own = send_id(view, "identifier");
        // SAFETY: `BOOL (id, SEL, id)`.
        !own.is_null()
            && unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(own, objc::sel::get("isEqualToString:"), identifier) != 0
            }
    };
    let Some(made) = views
        .iter()
        .copied()
        .find(|view| same(*view))
        .or(views.first().copied())
    else {
        return std::ptr::null_mut();
    };
    if !same(made) {
        send_id_id(made, "setIdentifier:", identifier);
    }
    objc::arc::autorelease(objc::arc::retain(made))
}

/// How tall `row` is, its intercell spacing aside: the delegate's answer, or
/// the row height.
fn height_of_row(receiver: Id, state: &Table, row: isize) -> f64 {
    let delegate = state.delegate as Id;
    // SAFETY: `CGFloat (id, SEL, NSTableView *, NSInteger)` and
    // `CGFloat (id, SEL, NSOutlineView *, id)`, as the protocols declare.
    unsafe {
        match crate::outline::is_outline(receiver) {
            true if responds(delegate, objc::sel::get("outlineView:heightOfRowByItem:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> f64 =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("outlineView:heightOfRowByItem:"),
                    receiver,
                    crate::outline::item_at(receiver, row),
                )
            }
            false if responds(delegate, objc::sel::get("tableView:heightOfRow:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, isize) -> f64 =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("tableView:heightOfRow:"),
                    receiver,
                    row,
                )
            }
            _ => state.row_height,
        }
    }
}

/// The row whose span holds `from_top`, or -1.
fn row_at(state: &Table, from_top: f64) -> isize {
    state
        .row_tops
        .windows(2)
        .position(|span| span[0] <= from_top && from_top < span[1])
        .map_or(-1, |row| row as isize)
}

/// `-rectOfRow:`: the row across the whole table.
unsafe extern "C" fn rect_of_row(receiver: Id, _sel: Sel, row: isize) -> NSRect {
    let state = table(receiver);
    let width = view::bounds_of(receiver).size.width;
    let span = match state.row_tops.is_empty() {
        true => {
            let pitch = state.row_height + state.spacing_height;
            (row >= 0 && row < rows(receiver)).then(|| (row as f64 * pitch, pitch))
        }
        false => usize::try_from(row)
            .ok()
            .and_then(|row| Some((*state.row_tops.get(row)?, *state.row_tops.get(row + 1)?)))
            .map(|(top, bottom)| (top, bottom - top)),
    };
    span.map_or(NSRect::default(), |(top, height)| NSRect {
        origin: NSPoint { x: 0.0, y: top },
        size: foundation::geometry::NSSize { width, height },
    })
}

/// `-rectOfColumn:`: the column down the whole table, its spacing included.
unsafe extern "C" fn rect_of_column(receiver: Id, _sel: Sel, index: isize) -> NSRect {
    let state = table(receiver);
    let Some(columns) = usize::try_from(index)
        .ok()
        .and_then(|index| state.columns.get(..=index))
    else {
        return NSRect::default();
    };
    let widths: Vec<f64> = columns
        .iter()
        .map(|c| column(*c as Id).width + state.spacing_width)
        .collect();
    let (last, before) = widths.split_last().expect("one column at least");
    NSRect {
        origin: NSPoint {
            x: before.iter().sum(),
            y: 0.0,
        },
        size: foundation::geometry::NSSize {
            width: *last,
            height: view::bounds_of(receiver).size.height,
        },
    }
}

/// `-frameOfCellAtColumn:row:`: where the row and column meet, inside the
/// intercell spacing, and past an outline's indentation.
unsafe extern "C" fn frame_of_cell(receiver: Id, sel: Sel, index: isize, row: isize) -> NSRect {
    // SAFETY: the same receiver.
    let (row_rect, column_rect) = unsafe {
        (
            rect_of_row(receiver, sel, row),
            rect_of_column(receiver, sel, index),
        )
    };
    let state = table(receiver);
    let indent = crate::outline::indentation_at(receiver, column_at(receiver, index as usize), row);
    let x = column_rect.origin.x + state.spacing_width / 2.0 + indent;
    NSRect {
        origin: NSPoint {
            x,
            y: row_rect.origin.y + state.spacing_height / 2.0,
        },
        size: foundation::geometry::NSSize {
            width: (column_rect.size.width - state.spacing_width - indent).max(0.0),
            height: (row_rect.size.height - state.spacing_height).max(0.0),
        },
    }
}

/// `-rowAtPoint:`.
unsafe extern "C" fn row_at_point(receiver: Id, _sel: Sel, point: NSPoint) -> isize {
    let state = table(receiver);
    if state.row_tops.is_empty() {
        let pitch = state.row_height + state.spacing_height;
        let row = (point.y / pitch).floor() as isize;
        return if pitch > 0.0 && (0..rows(receiver)).contains(&row) {
            row
        } else {
            -1
        };
    }
    row_at(&state, point.y)
}

/// Whether the delegate shows `row` as a group row.
fn is_group_row(receiver: Id, row: isize) -> bool {
    let delegate = table(receiver).delegate as Id;
    // SAFETY: `BOOL (id, SEL, NSTableView *, NSInteger)` and
    // `BOOL (id, SEL, NSOutlineView *, id)`.
    unsafe {
        match crate::outline::is_outline(receiver) {
            true if responds(delegate, objc::sel::get("outlineView:isGroupItem:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("outlineView:isGroupItem:"),
                    receiver,
                    crate::outline::item_at(receiver, row),
                ) != 0
            }
            false if responds(delegate, objc::sel::get("tableView:isGroupRow:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, isize) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("tableView:isGroupRow:"),
                    receiver,
                    row,
                ) != 0
            }
            _ => false,
        }
    }
}

/// The delegate's row view for `row`, or a plain one.
fn make_row_view(receiver: Id, row: isize, frame: NSRect) -> Id {
    let delegate = table(receiver).delegate as Id;
    // SAFETY: `id (id, SEL, NSTableView *, NSInteger)` and
    // `id (id, SEL, NSOutlineView *, id)`.
    let given = unsafe {
        match crate::outline::is_outline(receiver) {
            true if responds(delegate, objc::sel::get("outlineView:rowViewForItem:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("outlineView:rowViewForItem:"),
                    receiver,
                    crate::outline::item_at(receiver, row),
                )
            }
            false if responds(delegate, objc::sel::get("tableView:rowViewForRow:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, isize) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("tableView:rowViewForRow:"),
                    receiver,
                    row,
                )
            }
            _ => std::ptr::null_mut(),
        }
    };
    if !given.is_null() {
        send_rect(given, "setFrame:", frame);
        return objc::arc::retain(given);
    }
    let class = objc::class::lookup(c"NSTableRowView").expect("declared in crate::classes");
    let made = send_id(objc::class::ptr(class) as Id, "alloc");
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id = std::mem::transmute(objc::msg::entry());
        f(made, objc::sel::get("initWithFrame:"), frame)
    }
}

/// The delegate's view for `column` of `row` - `column` nil for a group row -
/// or one from the nib registered for the column's identifier.
fn make_cell_view(receiver: Id, column: Id, row: isize) -> Id {
    let delegate = table(receiver).delegate as Id;
    // SAFETY: `id (id, SEL, NSTableView *, NSTableColumn *, NSInteger)` and
    // `id (id, SEL, NSOutlineView *, NSTableColumn *, id)`.
    unsafe {
        match crate::outline::is_outline(receiver) {
            true if responds(delegate, objc::sel::get("outlineView:viewForTableColumn:item:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("outlineView:viewForTableColumn:item:"),
                    receiver,
                    column,
                    crate::outline::item_at(receiver, row),
                )
            }
            false if responds(delegate, objc::sel::get("tableView:viewForTableColumn:row:")) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id, isize) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("tableView:viewForTableColumn:row:"),
                    receiver,
                    column,
                    row,
                )
            }
            _ if column.is_null() => std::ptr::null_mut(),
            _ => make_view(
                receiver,
                objc::sel::get("makeViewWithIdentifier:owner:"),
                column_identifier(column, objc::sel::get("identifier")),
                delegate,
            ),
        }
    }
}

/// Tell the cell views of a row view what their row is drawn on.
fn refresh_background_styles(row_view: Id) {
    // SAFETY: `NSBackgroundStyle (id, SEL)` on a row view.
    let style = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(row_view, objc::sel::get("interiorBackgroundStyle"))
    };
    for cell in view::subviews_of(row_view) {
        if responds(cell, objc::sel::get("setBackgroundStyle:")) {
            // SAFETY: `void (id, SEL, NSBackgroundStyle)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, isize) =
                    std::mem::transmute(objc::msg::entry());
                f(cell, objc::sel::get("setBackgroundStyle:"), style);
            }
        }
    }
}

/// Tell the delegate a row view was added to the table, or removed from it.
fn tell_row_view(receiver: Id, row_view: Id, row: isize, added: bool) {
    let delegate = table(receiver).delegate as Id;
    let selector = match (crate::outline::is_outline(receiver), added) {
        (true, true) => "outlineView:didAddRowView:forRow:",
        (true, false) => "outlineView:didRemoveRowView:forRow:",
        (false, true) => "tableView:didAddRowView:forRow:",
        (false, false) => "tableView:didRemoveRowView:forRow:",
    };
    let sel = objc::sel::get(selector);
    if responds(delegate, sel) {
        // SAFETY: `void (id, SEL, NSTableView *, NSTableRowView *, NSInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, isize) =
                std::mem::transmute(objc::msg::entry());
            f(delegate, sel, receiver, row_view, row);
        }
    }
}

/// The last column, the first or all of them, by the autoresizing style,
/// grown or shrunk so the columns fill the table's width.
fn fit_columns(receiver: Id, width: f64) {
    /// `NSTableViewColumnAutoresizingStyle`.
    const NONE: isize = 0;
    const UNIFORM: isize = 1;
    const FIRST_COLUMN_ONLY: isize = 5;
    let state = table(receiver);
    if state.autoresizing_style == NONE || state.columns.is_empty() {
        return;
    }
    let used: f64 = state
        .columns
        .iter()
        .map(|c| column(*c as Id).width + state.spacing_width)
        .sum();
    let spare = width - used;
    // FIXME (ACCURACY): the sequential styles give all the change to the last
    // column, and no column's minimum or maximum width is kept.
    let changed: Vec<(usize, f64)> = match state.autoresizing_style {
        UNIFORM => {
            let share = spare / state.columns.len() as f64;
            state.columns.iter().map(|c| (*c, share)).collect()
        }
        FIRST_COLUMN_ONLY => vec![(state.columns[0], spare)],
        _ => vec![(*state.columns.last().expect("not empty"), spare)],
    };
    for (c, change) in changed {
        update_column(c as Id, |c| c.width = (c.width + change).max(0.0));
    }
}

thread_local! {
    /// Tables making their rows now, and whether each was asked to again.
    static TILING: std::cell::RefCell<HashMap<usize, bool>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Make a view-based table's row and cell views again, and size the table to
/// its rows. A request made while it is doing so - a delegate reloading from
/// a callback - does it again once it is done.
fn tile(receiver: Id) {
    let busy = TILING.with(|t| match t.borrow_mut().entry(receiver as usize) {
        std::collections::hash_map::Entry::Occupied(mut again) => {
            again.insert(true);
            true
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(false);
            false
        }
    });
    if busy {
        return;
    }
    const PASSES: usize = 4;
    for _ in 0..PASSES {
        make_rows(receiver);
        let again = TILING.with(|t| {
            std::mem::replace(
                t.borrow_mut()
                    .get_mut(&(receiver as usize))
                    .expect("inserted above"),
                false,
            )
        });
        if !again {
            break;
        }
    }
    TILING.with(|t| t.borrow_mut().remove(&(receiver as usize)));
}

fn make_rows(receiver: Id) {
    let count = rows(receiver);
    let state = table(receiver);
    let mut tops = Vec::with_capacity(count.max(0) as usize + 1);
    let mut bottom = 0.0;
    for row in 0..count {
        tops.push(bottom);
        bottom += height_of_row(receiver, &state, row) + state.spacing_height;
    }
    tops.push(bottom);
    update_table(receiver, |t| t.row_tops = tops);

    // A document view at least fills the clip view it is in.
    // SAFETY: a view of this crate's.
    let clip = unsafe { view::superview_of(receiver) };
    let frame = view::frame_of(receiver);
    let (width, height) = match objc::class::lookup(c"NSClipView") {
        // SAFETY: a live view.
        Some(class)
            if view::is_view(clip) && unsafe { corefoundation::base::is_kind_of(clip.cast(), class) } =>
        {
            let visible = view::bounds_of(clip).size;
            (visible.width, bottom.max(visible.height))
        }
        _ => (frame.size.width, bottom),
    };
    if (width, height) != (frame.size.width, frame.size.height) {
        // SAFETY: `void (id, SEL, NSSize)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSSize) =
                std::mem::transmute(objc::msg::entry());
            f(
                receiver,
                objc::sel::get("setFrameSize:"),
                foundation::geometry::NSSize { width, height },
            );
        }
    }
    fit_columns(receiver, width);

    let old = std::mem::take(&mut tables()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(receiver as usize)
        .or_default()
        .row_views);
    for (row, row_view) in old.into_iter().enumerate() {
        let row_view = row_view as Id;
        send_id(row_view, "removeFromSuperview");
        tell_row_view(receiver, row_view, row as isize, false);
        objc::arc::release(row_view);
    }

    let gives_values = has_object_values(receiver);
    let columns = table(receiver).columns;
    let mut made = Vec::with_capacity(count.max(0) as usize);
    for row in 0..count {
        // SAFETY: the same receiver.
        let rect = unsafe { rect_of_row(receiver, std::ptr::null(), row) };
        let row_view = make_row_view(receiver, row, rect);
        let group = is_group_row(receiver, row);
        // SAFETY: `void (id, SEL, BOOL)` on a row view.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(row_view, objc::sel::get("setGroupRowStyle:"), group as u8);
            f(
                row_view,
                objc::sel::get("setSelected:"),
                table(receiver).selected.contains(&row) as u8,
            );
            f(row_view, objc::sel::get("setEmphasized:"), 1);
        }
        set_row_highlight_style(row_view, highlight_style(receiver));
        let cells: Vec<(Id, NSRect)> = match group {
            true => vec![(std::ptr::null_mut(), rect)],
            false => columns
                .iter()
                .enumerate()
                .map(|(index, c)| {
                    // SAFETY: `NSRect (id, SEL, NSInteger, NSInteger)`, sent
                    // so a subclass's placement is used.
                    let frame = unsafe {
                        let f: unsafe extern "C" fn(Id, Sel, isize, isize) -> NSRect =
                            std::mem::transmute(objc::msg::entry());
                        f(
                            receiver,
                            objc::sel::get("frameOfCellAtColumn:row:"),
                            index as isize,
                            row,
                        )
                    };
                    (*c as Id, frame)
                })
                .collect(),
        };
        for (column, frame) in cells {
            let cell = make_cell_view(receiver, column, row);
            if cell.is_null() {
                continue;
            }
            if gives_values && responds(cell, objc::sel::get("setObjectValue:")) {
                send_id_id(cell, "setObjectValue:", object_value(receiver, column, row));
            }
            send_rect(
                cell,
                "setFrame:",
                NSRect {
                    origin: NSPoint {
                        x: frame.origin.x - rect.origin.x,
                        y: frame.origin.y - rect.origin.y,
                    },
                    size: frame.size,
                },
            );
            send_id_id(row_view, "addSubview:", cell);
        }
        refresh_background_styles(row_view);
        send_id_id(receiver, "addSubview:", row_view);
        made.push(row_view as usize);
        update_table(receiver, |t| t.row_views = made.clone());
        tell_row_view(receiver, row_view, row, true);
    }
}

unsafe extern "C" fn row_view_at_row(receiver: Id, _sel: Sel, row: isize, _make: u8) -> Id {
    usize::try_from(row)
        .ok()
        .and_then(|row| table(receiver).row_views.get(row).copied())
        .unwrap_or(0) as Id
}

/// `-viewAtColumn:row:makeIfNecessary:`: the row's view for that column, the
/// row's only view for a group row.
unsafe extern "C" fn view_at(receiver: Id, sel: Sel, index: isize, row: isize, make: u8) -> Id {
    // SAFETY: the same receiver.
    let row_view = unsafe { row_view_at_row(receiver, sel, row, make) };
    if row_view.is_null() {
        return row_view;
    }
    let cells = view::subviews_of(row_view);
    let index = match is_group_row(receiver, row) {
        true => 0,
        false => index.max(0) as usize,
    };
    cells.get(index).copied().unwrap_or(std::ptr::null_mut())
}

/// The row view `view` is in, and its row.
fn enclosing_row(receiver: Id, view: Id) -> Option<(usize, Id)> {
    let row_views = table(receiver).row_views;
    let mut at = view;
    while view::is_view(at) {
        if let Some(row) = row_views.iter().position(|r| *r == at as usize) {
            return Some((row, at));
        }
        // SAFETY: a view of this crate's.
        at = unsafe { view::superview_of(at) };
    }
    None
}

unsafe extern "C" fn row_for_view(receiver: Id, _sel: Sel, view: Id) -> isize {
    enclosing_row(receiver, view).map_or(-1, |(row, _)| row as isize)
}

unsafe extern "C" fn column_for_view(receiver: Id, _sel: Sel, view: Id) -> isize {
    let Some((row, row_view)) = enclosing_row(receiver, view) else {
        return -1;
    };
    if is_group_row(receiver, row as isize) {
        return -1;
    }
    let cells = view::subviews_of(row_view);
    let mut at = view;
    while view::is_view(at) && at != row_view {
        if let Some(index) = cells.iter().position(|c| *c == at) {
            return index as isize;
        }
        // SAFETY: a view of this crate's.
        at = unsafe { view::superview_of(at) };
    }
    -1
}

unsafe extern "C" fn column_with_identifier(receiver: Id, _sel: Sel, identifier: Id) -> isize {
    table(receiver)
        .columns
        .iter()
        .position(|c| {
            let own = column(*c as Id).identifier as Id;
            // SAFETY: `BOOL (id, SEL, id)` on a string.
            !own.is_null()
                && unsafe {
                    let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                        std::mem::transmute(objc::msg::entry());
                    f(own, objc::sel::get("isEqual:"), identifier) != 0
                }
        })
        .map_or(-1, |index| index as isize)
}

unsafe extern "C" fn table_column_with_identifier(receiver: Id, sel: Sel, identifier: Id) -> Id {
    // SAFETY: the same receiver.
    let index = unsafe { column_with_identifier(receiver, sel, identifier) };
    match usize::try_from(index) {
        Ok(index) => column_at(receiver, index),
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn intercell_spacing(receiver: Id, _sel: Sel) -> foundation::geometry::NSSize {
    let state = table(receiver);
    foundation::geometry::NSSize {
        width: state.spacing_width,
        height: state.spacing_height,
    }
}

unsafe extern "C" fn set_intercell_spacing(
    receiver: Id,
    _sel: Sel,
    spacing: foundation::geometry::NSSize,
) {
    update_table(receiver, |t| {
        t.spacing_width = spacing.width;
        t.spacing_height = spacing.height;
    });
    rows_changed(receiver);
}

unsafe extern "C" fn column_autoresizing_style(receiver: Id, _sel: Sel) -> isize {
    table(receiver).autoresizing_style
}

unsafe extern "C" fn set_column_autoresizing_style(receiver: Id, _sel: Sel, style: isize) {
    update_table(receiver, |t| t.autoresizing_style = style);
}

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

/// NSView's own implementation of `sel`, for an override to run first.
fn view_implementation(sel: &str) -> objc::class::Imp {
    objc::class::lookup(c"NSView")
        .and_then(|c| c.method(objc::sel::get(sel)))
        .expect("NSView's geometry methods are installed first")
        .implementation()
}

/// A change of width lays a view-based table's rows out again.
unsafe extern "C" fn set_frame_size(receiver: Id, sel: Sel, size: foundation::geometry::NSSize) {
    let before = view::bounds_of(receiver).size.width;
    // SAFETY: NSView's `-setFrameSize:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSSize) =
            std::mem::transmute(view_implementation("setFrameSize:"));
        f(receiver, sel, size);
    }
    if view::bounds_of(receiver).size.width != before && is_view_based(receiver) {
        tile(receiver);
    }
}

unsafe extern "C" fn set_frame(receiver: Id, sel: Sel, frame: NSRect) {
    let before = view::bounds_of(receiver).size.width;
    // SAFETY: NSView's `-setFrame:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) =
            std::mem::transmute(view_implementation("setFrame:"));
        f(receiver, sel, frame);
    }
    if view::bounds_of(receiver).size.width != before && is_view_based(receiver) {
        tile(receiver);
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSTableHeaderView").expect("declared in crate::classes");
    methods!(class, {
        "tableView" => header_table_view, c"@16@0:8";
        "drawRect:" => header_draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
    let class = objc::class::lookup(c"NSTableView").expect("declared in crate::classes");
    methods!(class, {
        "setDataSource:" => set_data_source, c"v24@0:8@16";
        "dataSource" => data_source, c"@16@0:8";
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "delegate" => delegate, c"@16@0:8";
        "setRowHeight:" => set_row_height, c"v24@0:8d16";
        "rowHeight" => row_height, c"d16@0:8";
        "setBackgroundColor:" => set_background_color, c"v24@0:8@16";
        "backgroundColor" => background_color, c"@16@0:8";
        "addTableColumn:" => add_table_column, c"v24@0:8@16";
        "tableColumns" => table_columns, c"@16@0:8";
        "numberOfColumns" => number_of_columns, c"q16@0:8";
        "numberOfRows" => number_of_rows, c"q16@0:8";
        "reloadData" => reload_data, c"v16@0:8";
        "noteNumberOfRowsChanged" => reload_data, c"v16@0:8";
        "scrollRowToVisible:" => scroll_row_to_visible, c"v24@0:8q16";
        "selectedRow" => selected_row, c"q16@0:8";
        "numberOfSelectedRows" => number_of_selected_rows, c"q16@0:8";
        "isRowSelected:" => is_row_selected, c"c24@0:8q16";
        "selectRowIndexes:byExtendingSelection:" => select_row_indexes, c"v28@0:8@16c24";
        "selectedRowIndexes" => selected_row_indexes, c"@16@0:8";
        "deselectAll:" => deselect_all, c"v24@0:8@16";
        "deselectRow:" => deselect_row, c"v24@0:8q16";
        "setTarget:" => set_target, c"v24@0:8@16";
        "target" => target, c"@16@0:8";
        "setDoubleAction:" => set_double_action, c"v24@0:8:16";
        "doubleAction" => double_action, c":16@0:8";
        "clickedRow" => clicked_row, c"q16@0:8";
        "clickedColumn" => clicked_column, c"q16@0:8";
        "mouseDown:" => mouse_down, c"v24@0:8@16";
        "setHeaderView:" => set_header_view, c"v24@0:8@16";
        "headerView" => header_view, c"@16@0:8";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        // FIXME (ACCURACY): kept only: a header click sorts nothing, and the
        // data source is not told the descriptors changed.
        "sortDescriptors" => sort_descriptors, c"@16@0:8";
        "setSortDescriptors:" => crate::props::set_copied, c"v24@0:8@16";
        "isFlipped" => yes, c"c16@0:8";
        "selectionHighlightStyle" => crate::props::get_integer, c"q16@0:8";
        "setSelectionHighlightStyle:" => set_selection_highlight_style, c"v24@0:8q16";
        "registerNib:forIdentifier:" => register_nib, c"v32@0:8@16@24";
        "registeredNibsByIdentifier" => registered_nibs_by_identifier, c"@16@0:8";
        "makeViewWithIdentifier:owner:" => make_view, c"@32@0:8@16@24";
        "rectOfRow:" => rect_of_row, c"{CGRect={CGPoint=dd}{CGSize=dd}}24@0:8q16";
        "rectOfColumn:" => rect_of_column, c"{CGRect={CGPoint=dd}{CGSize=dd}}24@0:8q16";
        "frameOfCellAtColumn:row:" => frame_of_cell, c"{CGRect={CGPoint=dd}{CGSize=dd}}32@0:8q16q24";
        "rowAtPoint:" => row_at_point, c"q32@0:8{CGPoint=dd}16";
        "rowViewAtRow:makeIfNecessary:" => row_view_at_row, c"@28@0:8q16c24";
        "viewAtColumn:row:makeIfNecessary:" => view_at, c"@36@0:8q16q24c32";
        "rowForView:" => row_for_view, c"q24@0:8@16";
        "columnForView:" => column_for_view, c"q24@0:8@16";
        "columnWithIdentifier:" => column_with_identifier, c"q24@0:8@16";
        "tableColumnWithIdentifier:" => table_column_with_identifier, c"@24@0:8@16";
        "intercellSpacing" => intercell_spacing, c"{CGSize=dd}16@0:8";
        "setIntercellSpacing:" => set_intercell_spacing, c"v32@0:8{CGSize=dd}16";
        "columnAutoresizingStyle" => column_autoresizing_style, c"q16@0:8";
        "setColumnAutoresizingStyle:" => set_column_autoresizing_style, c"v24@0:8q16";
        "setFrameSize:" => set_frame_size, c"v32@0:8{CGSize=dd}16";
        "setFrame:" => set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
    let column_class = objc::class::lookup(c"NSTableColumn").expect("declared in crate::classes");
    methods!(column_class, {
        "initWithIdentifier:" => column_init_with_identifier, c"@24@0:8@16";
        "setIdentifier:" => column_set_identifier, c"v24@0:8@16";
        "identifier" => column_identifier, c"@16@0:8";
        "setTitle:" => column_set_title, c"v24@0:8@16";
        "title" => column_title, c"@16@0:8";
        "setWidth:" => column_set_width, c"v24@0:8d16";
        "width" => column_width, c"d16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_to_a_row_shows_it_at_the_bottom_when_the_table_overflows() {
        assert_eq!(scroll_offset(None, 20.0, 200.0, 80.0), 0.0);
        assert_eq!(scroll_offset(Some(2), 20.0, 200.0, 80.0), 0.0);
        assert_eq!(scroll_offset(Some(9), 20.0, 200.0, 80.0), 120.0);
        assert_eq!(scroll_offset(Some(9), 20.0, 60.0, 80.0), 0.0);
    }
}
