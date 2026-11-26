//! `NSTableView` and `NSTableColumn`, as a read-only list a data source
//! fills: rows of each column's value, drawn in its font and alignment.
//!
//! FIXME (ACCURACY): a click selects one row and drags extend nothing; no
//! editing, headers or grid are drawn, and a
//! table that does not fit its clip view shows the rows `-scrollRowToVisible:`
//! last asked for rather than scrolling.

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
            .for_each(|c| objc::arc::release(c as Id));
        objc::arc::release(table.background as Id);
    }
}

unsafe extern "C" fn set_data_source(receiver: Id, _sel: Sel, source: Id) {
    update_table(receiver, |t| t.data_source = source as usize);
}

unsafe extern "C" fn data_source(receiver: Id, _sel: Sel) -> Id {
    table(receiver).data_source as Id
}

unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    update_table(receiver, |t| t.delegate = delegate as usize);
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

/// The value at `row` of `column`, as the text the cell would show.
fn value(receiver: Id, column: Id, row: isize) -> String {
    let source = table(receiver).data_source as Id;
    let sel = objc::sel::get("tableView:objectValueForTableColumn:row:");
    if !responds(source, sel) {
        return String::new();
    }
    // SAFETY: `id (id, SEL, NSTableView *, NSTableColumn *, NSInteger)`.
    let object = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, isize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(source, sel, receiver, column, row)
    };
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
    crate::backing::needs_display(receiver);
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
    let rows = index_set_members(set);
    update_table(receiver, |t| {
        if extend == 0 {
            t.selected.clear();
        }
        t.selected.extend(rows);
        t.selected.sort_unstable();
        t.selected.dedup();
    });
}

unsafe extern "C" fn selected_row_indexes(receiver: Id, _sel: Sel) -> Id {
    let class = objc::class::lookup(c"NSMutableIndexSet").expect("declared by Foundation");
    let index_set = objc::sel::get("indexSet");
    let add = objc::sel::get("addIndex:");
    // SAFETY: `+indexSet` is `id (Class, SEL)`, `-addIndex:` is
    // `void (id, SEL, NSUInteger)`.
    unsafe {
        let make: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        let set = make(objc::class::ptr(class) as Id, index_set);
        let add_index: unsafe extern "C" fn(Id, Sel, usize) =
            std::mem::transmute(objc::msg::entry());
        for row in table(receiver).selected {
            add_index(set, add, row as usize);
        }
        set
    }
}

unsafe extern "C" fn deselect_all(receiver: Id, _sel: Sel, _sender: Id) {
    update_table(receiver, |t| t.selected.clear());
}

unsafe extern "C" fn deselect_row(receiver: Id, _sel: Sel, row: isize) {
    update_table(receiver, |t| t.selected.retain(|r| *r != row));
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

/// Make `row` the one selected row, if the delegate agrees, and tell it.
fn select_by_click(receiver: Id, row: isize) {
    let state = table(receiver);
    if state.selected == [row] {
        return;
    }
    let delegate = state.delegate as Id;
    let should = objc::sel::get("tableView:shouldSelectRow:");
    // SAFETY: `BOOL (id, SEL, NSTableView *, NSInteger)`, as the protocol
    // declares.
    if responds(delegate, should)
        && unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, isize) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(delegate, should, receiver, row)
        } == 0
    {
        return;
    }
    update_table(receiver, |t| t.selected = vec![row]);
    // AppKit registers the delegate for this notification; nothing here
    // keeps that registration, so the delegate is told directly.
    const DID_CHANGE: &str = "NSTableViewSelectionDidChangeNotification";
    let changed = objc::sel::get("tableViewSelectionDidChange:");
    if responds(delegate, changed) {
        let notification =
            foundation::notification::make(DID_CHANGE, receiver, std::ptr::null_mut());
        // SAFETY: `void (id, SEL, NSNotification *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(delegate, changed, notification);
        }
    }
    foundation::notification::post(DID_CHANGE, receiver, std::ptr::null_mut());
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
    let row = match pitch > 0.0 {
        true => ((from_top + row_offset(receiver, &state, count)) / pitch).floor() as isize,
        false => -1,
    };
    let row = if (0..count).contains(&row) { row } else { -1 };
    let mut x = bounds.origin.x;
    let column = state
        .columns
        .iter()
        .position(|c| {
            x += column(*c as Id).width + 3.0;
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

unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
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
