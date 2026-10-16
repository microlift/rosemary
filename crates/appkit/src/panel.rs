use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrome::View;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[cfg(target_os = "linux")]
mod portal;

const CANCEL: i64 = 0;
const ACCEPT: i64 = 1;
const WINDOW_WIDTH: i32 = 720;
const WINDOW_HEIGHT: i32 = 500;
const FILE_PACKAGE_EXTENSIONS: &[&str] = &[
    "app",
    "appex",
    "bundle",
    "framework",
    "key",
    "numbers",
    "pages",
    "pkg",
    "plugin",
    "playground",
    "rtfd",
    "xcworkspace",
    "xcodeproj",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectedPath {
    guest_path: String,
    is_directory: bool,
}

#[derive(Clone, Debug, Default)]
struct PanelState {
    save: bool,
    multiple: bool,
    can_choose_files: bool,
    can_choose_directories: bool,
    directory: Option<String>,
    filename: Option<String>,
    title: Option<String>,
    prompt: Option<String>,
    allowed_file_types: Vec<String>,
    accessory_view: usize,
    accessory_view_disclosed: bool,
    treats_file_packages_as_directories: bool,
    shows_hidden_files: bool,
    can_select_hidden_extension: bool,
    selected: Vec<SelectedPath>,
}

static PANELS: OnceLock<Mutex<HashMap<usize, PanelState>>> = OnceLock::new();
static LAST_DIRECTORY: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn panels() -> &'static Mutex<HashMap<usize, PanelState>> {
    PANELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn last_directory() -> &'static Mutex<Option<PathBuf>> {
    LAST_DIRECTORY.get_or_init(|| Mutex::new(None))
}

fn state(receiver: Id) -> PanelState {
    panels()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&(receiver as usize))
        .cloned()
        .unwrap_or_default()
}

fn update(receiver: Id, update: impl FnOnce(&mut PanelState)) {
    update(
        panels()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(receiver as usize)
            .or_default(),
    );
}

unsafe extern "C" fn open_panel(class: Id, _sel: Sel) -> Id {
    create_panel(class, false)
}

unsafe extern "C" fn save_panel(class: Id, _sel: Sel) -> Id {
    create_panel(class, true)
}

fn create_panel(class: Id, save: bool) -> Id {
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return std::ptr::null_mut();
    };
    let receiver = objc::object::create_instance(class, 0);
    if !receiver.is_null() {
        update(receiver, |state| {
            state.save = save;
            state.can_choose_files = !save;
        });
    }
    objc::arc::autorelease(receiver)
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let accessory_view = panels()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(receiver as usize))
        .map_or(std::ptr::null_mut(), |state| state.accessory_view as Id);
    objc::arc::release(accessory_view);
    objc::arc::finalize(receiver);
}

unsafe fn string_text(value: Id) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let bytes = unsafe { foundation::string::bytes(value) };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

unsafe fn file_url_path(value: Id) -> Option<String> {
    unsafe { corefoundation::url::as_url(value as corefoundation::base::CFTypeRef) }?.file_path()
}

unsafe fn array_strings(value: Id) -> Vec<String> {
    let Some(array) =
        (unsafe { corefoundation::collections::array(value as corefoundation::base::CFTypeRef) })
    else {
        return Vec::new();
    };
    array
        .values
        .iter()
        .filter_map(|value| unsafe { string_text(*value as Id) })
        .collect()
}

unsafe extern "C" fn set_directory_url(receiver: Id, _sel: Sel, value: Id) {
    let path = unsafe { file_url_path(value) };
    update(receiver, |state| state.directory = path);
}

unsafe extern "C" fn set_filename(receiver: Id, _sel: Sel, value: Id) {
    let filename = unsafe { string_text(value) };
    update(receiver, |state| state.filename = filename);
}

unsafe extern "C" fn set_title(receiver: Id, _sel: Sel, value: Id) {
    let title = unsafe { string_text(value) };
    update(receiver, |state| state.title = title);
}

unsafe extern "C" fn set_prompt(receiver: Id, _sel: Sel, value: Id) {
    let prompt = unsafe { string_text(value) };
    update(receiver, |state| state.prompt = prompt);
}

unsafe extern "C" fn set_allowed_file_types(receiver: Id, _sel: Sel, value: Id) {
    let types = unsafe { array_strings(value) };
    update(receiver, |state| state.allowed_file_types = types);
}

unsafe extern "C" fn set_accessory_view(receiver: Id, _sel: Sel, view: Id) {
    let retained = objc::arc::retain(view);
    let previous = {
        let mut panels = panels().lock().unwrap_or_else(|error| error.into_inner());
        let state = panels.entry(receiver as usize).or_default();
        std::mem::replace(&mut state.accessory_view, retained as usize) as Id
    };
    objc::arc::release(previous);
}

unsafe extern "C" fn accessory_view(receiver: Id, _sel: Sel) -> Id {
    state(receiver).accessory_view as Id
}

unsafe extern "C" fn set_accessory_view_disclosed(receiver: Id, _sel: Sel, disclosed: u8) {
    update(receiver, |state| {
        state.accessory_view_disclosed = disclosed != 0
    });
}

unsafe extern "C" fn accessory_view_disclosed(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).accessory_view_disclosed)
}

unsafe extern "C" fn set_multiple(receiver: Id, _sel: Sel, value: u8) {
    update(receiver, |state| state.multiple = value != 0);
}

unsafe extern "C" fn allows_multiple(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).multiple)
}

unsafe extern "C" fn set_can_choose_files(receiver: Id, _sel: Sel, value: u8) {
    update(receiver, |state| state.can_choose_files = value != 0);
}

unsafe extern "C" fn can_choose_files(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).can_choose_files)
}

unsafe extern "C" fn set_can_choose_directories(receiver: Id, _sel: Sel, value: u8) {
    update(receiver, |state| state.can_choose_directories = value != 0);
}

unsafe extern "C" fn can_choose_directories(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).can_choose_directories)
}

fn initial_directory(panel: &PanelState) -> PathBuf {
    if let Some(guest) = panel.directory.as_deref() {
        let path = prefix::ns::host_path(guest);
        if path.is_dir() {
            return path;
        }
        if let Some(parent) = path.parent() {
            return parent.to_path_buf();
        }
    }
    if let Some(directory) = last_directory()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        && directory.is_dir()
    {
        return directory;
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|directory| directory.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn recognized_extensions(types: &[String]) -> Vec<String> {
    types
        .iter()
        .filter_map(|value| {
            let extension = value.trim().trim_start_matches('.');
            (!extension.is_empty()
                && !extension.starts_with("public.")
                && extension.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
                }))
            .then(|| extension.to_ascii_lowercase())
        })
        .collect()
}

fn allowed_file(path: &Path, types: &[String]) -> bool {
    let extensions = recognized_extensions(types);
    if extensions.is_empty() {
        return true;
    }
    path.extension().is_some_and(|extension| {
        let extension = extension.to_string_lossy().to_ascii_lowercase();
        extensions.iter().any(|allowed| allowed == &extension)
    })
}

pub(crate) fn is_file_package(path: &Path) -> bool {
    let has_package_extension = path.extension().is_some_and(|extension| {
        let extension = extension.to_string_lossy().to_ascii_lowercase();
        FILE_PACKAGE_EXTENSIONS.contains(&extension.as_str())
    });
    has_package_extension
        || path.join("Contents/Info.plist").is_file()
        || path.join("Info.plist").is_file()
}

fn displayed_as_directory(path: &Path, is_directory: bool, panel: &PanelState) -> bool {
    is_directory && (panel.treats_file_packages_as_directories || !is_file_package(path))
}

fn hidden_file_visible(name: &str, shows_hidden_files: bool) -> bool {
    shows_hidden_files || !name.starts_with('.') || name == "." || name == ".."
}

fn read_entries(
    directory: &Path,
    panel: &PanelState,
    shows_hidden_files: bool,
) -> Vec<chrome::file_picker::Entry> {
    let mut entries: Vec<(bool, String)> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !hidden_file_visible(&name, shows_hidden_files) {
                return None;
            }
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            let directory = displayed_as_directory(&path, metadata.is_dir(), panel);
            if !directory && !allowed_file(&path, &panel.allowed_file_types) {
                return None;
            }
            Some((directory, name))
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.to_lowercase().cmp(&right.1.to_lowercase()))
    });
    entries
        .into_iter()
        .map(|(directory, name)| chrome::file_picker::Entry { name, directory })
        .collect()
}

fn refresh_picker(
    picker: &mut chrome::file_picker::FilePicker,
    directory: &Path,
    panel: &PanelState,
) {
    picker.location = directory.display().to_string();
    let shows_hidden_files = picker.shows_hidden_files;
    picker.entries = read_entries(directory, panel, shows_hidden_files);
    picker.selected.clear();
    picker.scroll = 0;
}

fn selected_paths(
    picker: &chrome::file_picker::FilePicker,
    panel: &PanelState,
    directory: &Path,
) -> Option<Vec<PathBuf>> {
    if panel.save {
        let name = picker.filename.trim();
        if name.is_empty() || name.contains(['/', '\\', '\0']) {
            return None;
        }
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return None;
        }
        return Some(vec![directory.join(name)]);
    }
    let paths: Vec<PathBuf> = picker
        .selected
        .iter()
        .filter_map(|index| {
            let entry = picker.entries.get(*index)?;
            if entry.directory {
                panel
                    .can_choose_directories
                    .then(|| directory.join(&entry.name))
            } else {
                panel.can_choose_files.then(|| directory.join(&entry.name))
            }
        })
        .collect();
    (!paths.is_empty()).then_some(paths)
}

fn navigate_to(
    directory: &mut PathBuf,
    target: PathBuf,
    picker: &mut chrome::file_picker::FilePicker,
    panel: &PanelState,
) {
    if target.is_dir() {
        *directory = target;
        refresh_picker(picker, directory, panel);
        picker.search.clear();
        picker.search_focused = false;
    }
}

fn paint_picker(
    window: sdl3::Window,
    picker: &chrome::file_picker::FilePicker,
    theme: &chrome::Theme,
) {
    let Some((width, height)) = sdl3::video::window_size(window) else {
        return;
    };
    sdl3::video::present_window(window, |surface| {
        let scale = if width > 0 {
            surface.width as f32 / width as f32
        } else {
            1.0
        };
        let length = surface.pitch as usize * surface.height as usize;
        let pixels = unsafe { std::slice::from_raw_parts_mut(surface.pixels, length) };
        let mut canvas = chrome::Canvas::new(
            pixels,
            surface.width as usize,
            surface.height as usize,
            surface.pitch as usize,
            scale,
        );
        canvas.clear(chrome::Color::rgba(0, 0, 0, 0.0));
        let layout = picker.layout(
            chrome::Rect::new(0.0, 0.0, width as f32, height as f32),
            theme,
        );
        picker.paint(&mut canvas, &layout, theme);
        true
    });
}

struct PickerResult {
    selected: Option<Vec<PathBuf>>,
    shows_hidden_files: bool,
}

enum PortalOutcome {
    Selected(Vec<PathBuf>),
    Cancelled,
    Unavailable,
}

fn resolve_picker_result(
    outcome: PortalOutcome,
    shows_hidden_files: bool,
    fallback: impl FnOnce() -> Option<PickerResult>,
) -> Option<PickerResult> {
    match outcome {
        PortalOutcome::Selected(selected) => Some(PickerResult {
            selected: Some(selected),
            shows_hidden_files,
        }),
        PortalOutcome::Cancelled => Some(PickerResult {
            selected: None,
            shows_hidden_files,
        }),
        PortalOutcome::Unavailable => fallback(),
    }
}

fn picker_shortcut(scancode: u32, modifiers: u16) -> Option<PickerShortcut> {
    let shortcut_modifiers = sdl3::keycodes::keymod::CTRL | sdl3::keycodes::keymod::GUI;
    if modifiers & shortcut_modifiers == 0 {
        return None;
    }
    match scancode {
        sdl3::keycodes::scancode::H => Some(PickerShortcut::ToggleHiddenFiles),
        sdl3::keycodes::scancode::F => Some(PickerShortcut::FocusSearch),
        sdl3::keycodes::scancode::R => Some(PickerShortcut::Refresh),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerShortcut {
    ToggleHiddenFiles,
    FocusSearch,
    Refresh,
}

fn run_picker(panel: &PanelState) -> Option<PickerResult> {
    if chrome::Font::system(400).is_none() {
        eprintln!("rosemary: appkit: no system font is available for the file chooser");
        return None;
    }
    let started_video = !sdl3::video::video_up();
    if !sdl3::video::init_video() {
        return None;
    }
    let title =
        panel
            .title
            .as_deref()
            .unwrap_or(if panel.save { "Save File" } else { "Open File" });
    let title = std::ffi::CString::new(title.replace('\0', " ")).unwrap_or_default();
    let flags = sdl3::video::flags::HIDDEN
        | sdl3::video::flags::RESIZABLE
        | sdl3::video::flags::HIGH_PIXEL_DENSITY;
    let Some(window) = sdl3::video::create_window(&title, WINDOW_WIDTH, WINDOW_HEIGHT, flags)
    else {
        eprintln!(
            "rosemary: appkit: could not open file chooser: {}",
            sdl3::error()
        );
        if started_video {
            sdl3::video::quit_video();
        }
        return None;
    };
    let parent = crate::dialog::parent();
    if !parent.is_null() {
        sdl3::video::set_window_modal_for(window, parent);
    }
    sdl3::video::set_window_minimum_size(window, 480, 320);
    crate::icon::apply(window);
    crate::dialog::center(window, parent, WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32);

    let mut directory = initial_directory(panel);
    let mut picker = chrome::file_picker::FilePicker {
        title: title.to_string_lossy().into_owned(),
        location: String::new(),
        entries: Vec::new(),
        selected: Vec::new(),
        scroll: 0,
        filename: panel.filename.clone().unwrap_or_default(),
        save: panel.save,
        multiple: panel.multiple,
        accept_title: panel
            .prompt
            .clone()
            .unwrap_or_else(|| if panel.save { "Save" } else { "Open" }.into()),
        pressed: None,
        filename_focused: panel.save,
        search: String::new(),
        search_focused: false,
        shows_hidden_files: panel.shows_hidden_files,
    };
    refresh_picker(&mut picker, &directory, panel);
    let mut filename_focused = panel.save;
    if filename_focused {
        sdl3::video::start_text_input(window);
    }
    paint_picker(
        window,
        &picker,
        &chrome::Theme::for_appearance(
            sdl3::video::system_theme() == sdl3::video::SystemTheme::Dark,
        ),
    );
    sdl3::video::show_window(window);
    sdl3::video::raise_window(window);
    let id = sdl3::video::window_id(window);
    let mut theme = chrome::Theme::for_appearance(
        sdl3::video::system_theme() == sdl3::video::SystemTheme::Dark,
    );
    let (selected, quit) = crate::dialog::run_modal_window(window, None, |event| {
        use chrome::file_picker::Part;
        use sdl3::keycodes::scancode;
        use sdl3::video::Event;

        let layout = picker.layout(
            chrome::Rect::new(
                0.0,
                0.0,
                sdl3::video::window_size(window).map_or(WINDOW_WIDTH as f32, |size| size.0 as f32),
                sdl3::video::window_size(window).map_or(WINDOW_HEIGHT as f32, |size| size.1 as f32),
            ),
            &theme,
        );
        match event {
            Event::MouseButton {
                window: event_window,
                button: sdl3::mouse::LEFT,
                down,
                clicks,
                x,
                y,
                ..
            } if *event_window == id => {
                let hit = picker.hit(&layout, chrome::Point::new(*x, *y));
                if *down {
                    picker.pressed = hit;
                    if hit == Some(Part::Filename) {
                        filename_focused = true;
                        picker.filename_focused = true;
                        picker.search_focused = false;
                        sdl3::video::start_text_input(window);
                    } else if hit == Some(Part::Search) {
                        filename_focused = false;
                        picker.filename_focused = false;
                        picker.search_focused = true;
                        sdl3::video::start_text_input(window);
                    } else if hit.is_some() {
                        filename_focused = false;
                        picker.filename_focused = false;
                        picker.search_focused = false;
                        sdl3::video::stop_text_input(window);
                    }
                    paint_picker(window, &picker, &theme);
                    return None;
                }
                let pressed = picker.pressed.take();
                if pressed != hit {
                    paint_picker(window, &picker, &theme);
                    return None;
                }
                match hit {
                    Some(Part::Parent) => {
                        if let Some(parent) = directory.parent().map(Path::to_path_buf) {
                            navigate_to(&mut directory, parent, &mut picker, panel);
                        }
                    }
                    Some(Part::Entry(index)) => {
                        let Some(entry) = picker.entries.get(index).cloned() else {
                            return None;
                        };
                        let can_select = if entry.directory {
                            true
                        } else {
                            panel.can_choose_files
                        };
                        if *clicks >= 2 {
                            if entry.directory {
                                if !panel.can_choose_files && panel.can_choose_directories {
                                    picker.selected = vec![index];
                                    if let Some(paths) = selected_paths(&picker, panel, &directory)
                                    {
                                        paint_picker(window, &picker, &theme);
                                        return Some(Some(paths));
                                    }
                                }
                                let target = directory.join(&entry.name);
                                navigate_to(&mut directory, target, &mut picker, panel);
                            } else if can_select {
                                picker.selected = vec![index];
                                if let Some(paths) = selected_paths(&picker, panel, &directory) {
                                    paint_picker(window, &picker, &theme);
                                    return Some(Some(paths));
                                }
                            }
                        } else if can_select {
                            let modifiers = sdl3::video::keyboard_modifiers();
                            let control = modifiers
                                & (sdl3::keycodes::keymod::CTRL | sdl3::keycodes::keymod::GUI)
                                != 0;
                            if picker.multiple && control {
                                if picker.selected.contains(&index) {
                                    picker.selected.retain(|selected| *selected != index);
                                } else {
                                    picker.selected.push(index);
                                }
                            } else {
                                picker.selected = vec![index];
                            }
                            filename_focused = false;
                            picker.filename_focused = false;
                            sdl3::video::stop_text_input(window);
                        } else {
                            picker.selected.clear();
                        }
                    }
                    Some(Part::Filename) => {
                        filename_focused = true;
                        picker.filename_focused = true;
                        picker.search_focused = false;
                        sdl3::video::start_text_input(window);
                    }
                    Some(Part::Search) => {
                        filename_focused = false;
                        picker.filename_focused = false;
                        picker.search_focused = true;
                        sdl3::video::start_text_input(window);
                    }
                    Some(Part::HiddenFiles) => {
                        picker.shows_hidden_files = !picker.shows_hidden_files;
                        refresh_picker(&mut picker, &directory, panel);
                    }
                    Some(Part::Cancel) => {
                        paint_picker(window, &picker, &theme);
                        return Some(None);
                    }
                    Some(Part::Accept) => {
                        if let Some(paths) = selected_paths(&picker, panel, &directory) {
                            paint_picker(window, &picker, &theme);
                            return Some(Some(paths));
                        }
                    }
                    None => {}
                }
                paint_picker(window, &picker, &theme);
            }
            Event::MouseWheel {
                window: event_window,
                y,
                ..
            } if *event_window == id => {
                let visible = layout.rows.len();
                let maximum = picker.matching_indices().len().saturating_sub(visible);
                if *y > 0.0 {
                    picker.scroll = picker.scroll.saturating_sub(y.ceil() as usize);
                } else if *y < 0.0 {
                    picker.scroll = (picker.scroll + (-y).ceil() as usize).min(maximum);
                }
                paint_picker(window, &picker, &theme);
            }
            Event::Text {
                window: event_window,
                text,
            } if *event_window == id && (filename_focused || picker.search_focused) => {
                if picker.search_focused {
                    picker.search.push_str(text);
                    picker.selected.clear();
                    picker.scroll = 0;
                } else {
                    picker.filename.push_str(text);
                }
                paint_picker(window, &picker, &theme);
            }
            Event::Key {
                window: event_window,
                scancode: key,
                modifiers,
                down: true,
                ..
            } if *event_window == id => {
                match picker_shortcut(*key, *modifiers) {
                    Some(PickerShortcut::ToggleHiddenFiles) => {
                        picker.shows_hidden_files = !picker.shows_hidden_files;
                        refresh_picker(&mut picker, &directory, panel);
                        paint_picker(window, &picker, &theme);
                        return None;
                    }
                    Some(PickerShortcut::FocusSearch) => {
                        filename_focused = false;
                        picker.filename_focused = false;
                        picker.search_focused = true;
                        sdl3::video::start_text_input(window);
                        paint_picker(window, &picker, &theme);
                        return None;
                    }
                    Some(PickerShortcut::Refresh) => {
                        refresh_picker(&mut picker, &directory, panel);
                        paint_picker(window, &picker, &theme);
                        return None;
                    }
                    None => {}
                }
                match *key {
                    scancode::ESCAPE if picker.search_focused && !picker.search.is_empty() => {
                        picker.search.clear();
                        picker.selected.clear();
                        picker.scroll = 0;
                        paint_picker(window, &picker, &theme);
                    }
                    scancode::ESCAPE => return Some(None),
                    scancode::RETURN | scancode::KP_ENTER => {
                        if let Some(index) = picker.selected.first().copied()
                            && picker
                                .entries
                                .get(index)
                                .is_some_and(|entry| entry.directory)
                            && !panel.can_choose_directories
                        {
                            let target = directory.join(&picker.entries[index].name);
                            navigate_to(&mut directory, target, &mut picker, panel);
                            paint_picker(window, &picker, &theme);
                            return None;
                        }
                        if let Some(paths) = selected_paths(&picker, panel, &directory) {
                            return Some(Some(paths));
                        }
                    }
                    scancode::BACKSPACE if picker.search_focused => {
                        picker.search.pop();
                        picker.selected.clear();
                        picker.scroll = 0;
                        paint_picker(window, &picker, &theme);
                    }
                    scancode::BACKSPACE if filename_focused => {
                        picker.filename.pop();
                        paint_picker(window, &picker, &theme);
                    }
                    scancode::BACKSPACE => {
                        if let Some(parent) = directory.parent().map(Path::to_path_buf) {
                            navigate_to(&mut directory, parent, &mut picker, panel);
                        }
                        paint_picker(window, &picker, &theme);
                    }
                    scancode::UP
                    | scancode::DOWN
                    | scancode::HOME
                    | scancode::END
                    | scancode::PAGEUP
                    | scancode::PAGEDOWN => {
                        let matching = picker.matching_indices();
                        if matching.is_empty() {
                            return None;
                        }
                        let current = picker.selected.first().and_then(|selected| {
                            matching.iter().position(|index| index == selected)
                        });
                        let page = layout.rows.len().max(1);
                        let next_position = match *key {
                            scancode::UP => current.unwrap_or(0).saturating_sub(1),
                            scancode::DOWN => {
                                current.map_or(0, |position| (position + 1).min(matching.len() - 1))
                            }
                            scancode::HOME => 0,
                            scancode::END => matching.len() - 1,
                            scancode::PAGEUP => current.unwrap_or(page).saturating_sub(page),
                            scancode::PAGEDOWN => current
                                .map_or(0, |position| (position + page).min(matching.len() - 1)),
                            _ => unreachable!(),
                        };
                        let next = matching[next_position];
                        picker.selected = vec![next];
                        if next_position < picker.scroll {
                            picker.scroll = next_position;
                        } else if next_position >= picker.scroll + layout.rows.len() {
                            picker.scroll = next_position + 1 - layout.rows.len().max(1);
                        }
                        filename_focused = false;
                        picker.filename_focused = false;
                        picker.search_focused = false;
                        sdl3::video::stop_text_input(window);
                        paint_picker(window, &picker, &theme);
                    }
                    _ => {}
                }
            }
            Event::WindowResized {
                window: event_window,
                ..
            }
            | Event::WindowBackingChanged {
                window: event_window,
            } if *event_window == id => paint_picker(window, &picker, &theme),
            Event::Other(sdl3::video::event_type::WINDOW_EXPOSED) => {
                paint_picker(window, &picker, &theme);
            }
            Event::WindowClosed {
                window: event_window,
            } if *event_window == id => {
                return Some(None);
            }
            Event::Other(sdl3::video::event_type::SYSTEM_THEME_CHANGED) => {
                theme = chrome::Theme::for_appearance(
                    sdl3::video::system_theme() == sdl3::video::SystemTheme::Dark,
                );
                paint_picker(window, &picker, &theme);
            }
            _ => {}
        }
        None
    });
    sdl3::video::stop_text_input(window);
    sdl3::video::destroy_window(window);
    if started_video {
        sdl3::video::quit_video();
    }
    if quit && !started_video {
        crate::event::deliver(sdl3::video::Event::Quit);
    }
    Some(PickerResult {
        selected,
        shows_hidden_files: picker.shows_hidden_files,
    })
}

fn select(receiver: Id) -> i64 {
    let mut panel = state(receiver);
    panel.selected.clear();
    #[cfg(target_os = "linux")]
    let portal_outcome = portal::show(&panel);
    #[cfg(not(target_os = "linux"))]
    let portal_outcome = PortalOutcome::Unavailable;
    let Some(result) = resolve_picker_result(portal_outcome, panel.shows_hidden_files, || {
        run_picker(&panel)
    }) else {
        update(receiver, |state| state.selected.clear());
        return CANCEL;
    };
    panel.shows_hidden_files = result.shows_hidden_files;
    let Some(host_paths) = result.selected else {
        update(receiver, |state| {
            state.selected.clear();
            state.shows_hidden_files = panel.shows_hidden_files;
        });
        return CANCEL;
    };
    let mut selected = Vec::new();
    for host_path in host_paths {
        let Some(guest_path) = prefix::ns::to_guest(&host_path) else {
            eprintln!("rosemary: appkit: selected path is outside the guest filesystem");
            continue;
        };
        let Some(guest_path) = guest_path.to_str().map(str::to_owned) else {
            continue;
        };
        let is_directory = host_path.is_dir();
        let remembered_directory = if is_directory {
            host_path.clone()
        } else {
            host_path
                .parent()
                .unwrap_or_else(|| Path::new("/"))
                .to_path_buf()
        };
        *last_directory()
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(remembered_directory);
        selected.push(SelectedPath {
            guest_path,
            is_directory,
        });
        if !panel.multiple {
            break;
        }
    }
    let response = if selected.is_empty() { CANCEL } else { ACCEPT };
    update(receiver, |state| {
        state.selected = selected;
        state.shows_hidden_files = panel.shows_hidden_files;
    });
    response
}

fn complete(handler: Id, response: i64) {
    if let Some(invoke) = unsafe { crate::animation::invoke(handler) } {
        let callback: unsafe extern "C" fn(Id, isize) = unsafe { std::mem::transmute(invoke) };
        unsafe { callback(handler, response as isize) };
    }
}

unsafe extern "C" fn run_modal(receiver: Id, _sel: Sel) -> i64 {
    select(receiver)
}

unsafe extern "C" fn begin_with_completion(receiver: Id, _sel: Sel, handler: Id) {
    let response = select(receiver);
    complete(handler, response);
}

unsafe extern "C" fn begin_sheet_with_completion(
    receiver: Id,
    _sel: Sel,
    _window: Id,
    handler: Id,
) {
    let response = select(receiver);
    complete(handler, response);
}

fn selected(receiver: Id) -> Vec<SelectedPath> {
    state(receiver).selected
}

fn autoreleased_array(values: &[Id]) -> Id {
    let values: Vec<corefoundation::base::CFTypeRef> = values
        .iter()
        .map(|value| *value as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for value in values {
        objc::arc::release(value as Id);
    }
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn urls(receiver: Id, _sel: Sel) -> Id {
    let urls: Vec<Id> = selected(receiver)
        .into_iter()
        .map(|selection| {
            corefoundation::url::new_file_url(&selection.guest_path, selection.is_directory) as Id
        })
        .collect();
    autoreleased_array(&urls)
}

unsafe extern "C" fn filenames(receiver: Id, _sel: Sel) -> Id {
    let filenames: Vec<Id> = selected(receiver)
        .into_iter()
        .map(|selection| foundation::string::from_str(&selection.guest_path))
        .collect();
    autoreleased_array(&filenames)
}

unsafe extern "C" fn url(receiver: Id, _sel: Sel) -> Id {
    selected(receiver)
        .first()
        .map(|selection| {
            objc::arc::autorelease(corefoundation::url::new_file_url(
                &selection.guest_path,
                selection.is_directory,
            ) as Id)
        })
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn filename(receiver: Id, _sel: Sel) -> Id {
    selected(receiver)
        .first()
        .map(|selection| {
            objc::arc::autorelease(foundation::string::from_str(&selection.guest_path))
        })
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn directory_url(receiver: Id, _sel: Sel) -> Id {
    let panel = state(receiver);
    let guest_directory = panel
        .selected
        .first()
        .and_then(|selection| {
            let path = Path::new(&selection.guest_path);
            if selection.is_directory {
                Some(path.to_path_buf())
            } else {
                path.parent().map(Path::to_path_buf)
            }
        })
        .or_else(|| panel.directory.map(PathBuf::from));
    guest_directory
        .and_then(|path| path.to_str().map(str::to_owned))
        .map(|path| objc::arc::autorelease(corefoundation::url::new_file_url(&path, true) as Id))
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn empty_array(_receiver: Id, _sel: Sel) -> Id {
    autoreleased_array(&[])
}

unsafe extern "C" fn allowed_file_types(receiver: Id, _sel: Sel) -> Id {
    let values: Vec<Id> = state(receiver)
        .allowed_file_types
        .iter()
        .map(|value| foundation::string::from_str(value))
        .collect();
    autoreleased_array(&values)
}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn set_shows_hidden_files(receiver: Id, _sel: Sel, value: u8) {
    update(receiver, |state| state.shows_hidden_files = value != 0);
}

unsafe extern "C" fn shows_hidden_files(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).shows_hidden_files)
}

unsafe extern "C" fn set_can_select_hidden_extension(receiver: Id, _sel: Sel, value: u8) {
    update(receiver, |state| {
        state.can_select_hidden_extension = value != 0
    });
}

unsafe extern "C" fn can_select_hidden_extension(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).can_select_hidden_extension)
}

unsafe extern "C" fn set_treats_file_packages_as_directories(receiver: Id, _sel: Sel, value: u8) {
    update(receiver, |state| {
        state.treats_file_packages_as_directories = value != 0
    });
}

unsafe extern "C" fn treats_file_packages_as_directories(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).treats_file_packages_as_directories)
}

unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _value: u8) {}

pub(crate) fn install() {
    for name in [c"NSOpenPanel", c"NSSavePanel"] {
        let class = objc::class::lookup(name).expect("declared class");
        class_methods!(class, {
            "openPanel" => open_panel, c"@16@0:8";
            "savePanel" => save_panel, c"@16@0:8";
        });
        methods!(class, {
            "runModal" => run_modal, c"q16@0:8";
            "beginWithCompletionHandler:" => begin_with_completion, c"v24@0:8@?16";
            "beginSheetModalForWindow:completionHandler:" => begin_sheet_with_completion, c"v32@0:8@16@?24";
            "URLs" => urls, c"@16@0:8";
            "filenames" => filenames, c"@16@0:8";
            "URL" => url, c"@16@0:8";
            "filename" => filename, c"@16@0:8";
            "directoryURL" => directory_url, c"@16@0:8";
            "allowedContentTypes" => empty_array, c"@16@0:8";
            "allowedFileTypes" => allowed_file_types, c"@16@0:8";
            "accessoryView" => accessory_view, c"@16@0:8";
            "setAccessoryView:" => set_accessory_view, c"v24@0:8@16";
            "isAccessoryViewDisclosed" => accessory_view_disclosed, c"c16@0:8";
            "accessoryViewDisclosed" => accessory_view_disclosed, c"c16@0:8";
            "setAccessoryViewDisclosed:" => set_accessory_view_disclosed, c"v20@0:8c16";
            "setDirectoryURL:" => set_directory_url, c"v24@0:8@16";
            "setAllowedFileTypes:" => set_allowed_file_types, c"v24@0:8@16";
            "setAllowedContentTypes:" => ignored_object, c"v24@0:8@16";
            "setNameFieldStringValue:" => set_filename, c"v24@0:8@16";
            "setTitle:" => set_title, c"v24@0:8@16";
            "setMessage:" => ignored_object, c"v24@0:8@16";
            "setPrompt:" => set_prompt, c"v24@0:8@16";
            "setCanChooseFiles:" => set_can_choose_files, c"v20@0:8c16";
            "canChooseFiles" => can_choose_files, c"c16@0:8";
            "setCanChooseDirectories:" => set_can_choose_directories, c"v20@0:8c16";
            "canChooseDirectories" => can_choose_directories, c"c16@0:8";
            "setAllowsMultipleSelection:" => set_multiple, c"v20@0:8c16";
            "allowsMultipleSelection" => allows_multiple, c"c16@0:8";
            "setCanCreateDirectories:" => ignored_flag, c"v20@0:8c16";
            "setResolvesAliases:" => ignored_flag, c"v20@0:8c16";
            "showsHiddenFiles" => shows_hidden_files, c"c16@0:8";
            "setShowsHiddenFiles:" => set_shows_hidden_files, c"v20@0:8c16";
            "canSelectHiddenExtension" => can_select_hidden_extension, c"c16@0:8";
            "setCanSelectHiddenExtension:" => set_can_select_hidden_extension, c"v20@0:8c16";
            "treatsFilePackagesAsDirectories" => treats_file_packages_as_directories, c"c16@0:8";
            "setTreatsFilePackagesAsDirectories:" => set_treats_file_packages_as_directories, c"v20@0:8c16";
            "dealloc" => dealloc, c"v16@0:8";
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
    static CALLBACK_RESPONSE: AtomicI64 = AtomicI64::new(0);
    static HIDDEN_TEST_ID: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn record_response(_block: Id, response: isize) {
        CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
        CALLBACK_RESPONSE.store(response as i64, Ordering::SeqCst);
    }

    #[test]
    fn accessory_view_is_retained_replaced_and_released_with_panel() {
        crate::bootstrap();
        let panel = objc::object::create_instance(objc::class::lookup(c"NSOpenPanel").unwrap(), 0);
        let first = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let second = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let setter: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let getter: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_accessory = objc::sel::get("setAccessoryView:");
        let get_accessory = objc::sel::get("accessoryView");

        unsafe {
            setter(panel, set_accessory, first);
            assert_eq!(objc::arc::retain_count(first), 2);
            assert_eq!(getter(panel, get_accessory), first);

            setter(panel, set_accessory, second);
            assert_eq!(objc::arc::retain_count(first), 1);
            assert_eq!(objc::arc::retain_count(second), 2);
            assert_eq!(getter(panel, get_accessory), second);

            setter(panel, set_accessory, std::ptr::null_mut());
            assert_eq!(objc::arc::retain_count(second), 1);
            assert!(getter(panel, get_accessory).is_null());

            setter(panel, set_accessory, second);
            objc::arc::release(panel);
        }

        assert_eq!(objc::arc::retain_count(second), 1);
        objc::arc::release(first);
        objc::arc::release(second);
    }

    #[test]
    fn accessory_view_disclosure_state_round_trips() {
        crate::bootstrap();
        let panel = objc::object::create_instance(objc::class::lookup(c"NSOpenPanel").unwrap(), 0);
        let setter: unsafe extern "C" fn(Id, Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let getter: unsafe extern "C" fn(Id, Sel) -> u8 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_disclosed = objc::sel::get("setAccessoryViewDisclosed:");
        let is_disclosed = objc::sel::get("isAccessoryViewDisclosed");
        let disclosed = objc::sel::get("accessoryViewDisclosed");

        unsafe {
            assert_eq!(getter(panel, is_disclosed), 0);
            setter(panel, set_disclosed, 1);
            assert_eq!(getter(panel, is_disclosed), 1);
            assert_eq!(getter(panel, disclosed), 1);
            setter(panel, set_disclosed, 0);
            assert_eq!(getter(panel, is_disclosed), 0);
            objc::arc::release(panel);
        }
    }

    #[test]
    fn file_package_display_preference_round_trips_and_controls_entry_kind() {
        crate::bootstrap();
        let panel = objc::object::create_instance(objc::class::lookup(c"NSOpenPanel").unwrap(), 0);
        let setter: unsafe extern "C" fn(Id, Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let getter: unsafe extern "C" fn(Id, Sel) -> u8 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_preference = objc::sel::get("setTreatsFilePackagesAsDirectories:");
        let preference = objc::sel::get("treatsFilePackagesAsDirectories");
        let package = Path::new("/Applications/Studio.app");
        let folder = Path::new("/tmp/Assets");

        unsafe {
            assert_eq!(getter(panel, preference), 0);
            assert!(!displayed_as_directory(package, true, &state(panel)));
            setter(panel, set_preference, 1);
            assert_eq!(getter(panel, preference), 1);
            assert!(displayed_as_directory(package, true, &state(panel)));
            assert!(displayed_as_directory(folder, true, &state(panel)));
            setter(panel, set_preference, 0);
            assert_eq!(getter(panel, preference), 0);
            objc::arc::release(panel);
        }
    }

    #[test]
    fn can_select_hidden_extension_preference_round_trips_on_save_panel() {
        crate::bootstrap();
        let panel = objc::object::create_instance(objc::class::lookup(c"NSSavePanel").unwrap(), 0);
        let setter: unsafe extern "C" fn(Id, Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let getter: unsafe extern "C" fn(Id, Sel) -> u8 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_preference = objc::sel::get("setCanSelectHiddenExtension:");
        let preference = objc::sel::get("canSelectHiddenExtension");

        unsafe {
            assert_eq!(getter(panel, preference), 0);
            assert!(!state(panel).can_select_hidden_extension);
            setter(panel, set_preference, 1);
            assert_eq!(getter(panel, preference), 1);
            assert!(state(panel).can_select_hidden_extension);
            setter(panel, set_preference, 0);
            assert_eq!(getter(panel, preference), 0);
            objc::arc::release(panel);
        }
    }

    #[test]
    fn shows_hidden_files_preference_controls_dotfile_listing() {
        crate::bootstrap();
        let panel = objc::object::create_instance(objc::class::lookup(c"NSOpenPanel").unwrap(), 0);
        let setter: unsafe extern "C" fn(Id, Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let getter: unsafe extern "C" fn(Id, Sel) -> u8 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_preference = objc::sel::get("setShowsHiddenFiles:");
        let preference = objc::sel::get("showsHiddenFiles");

        unsafe {
            assert_eq!(getter(panel, preference), 0);
            setter(panel, set_preference, 1);
            assert_eq!(getter(panel, preference), 1);
            setter(panel, set_preference, 0);
            assert_eq!(getter(panel, preference), 0);
            objc::arc::release(panel);
        }

        let root = std::env::temp_dir().join(format!(
            "rosemary-file-picker-hidden-{}-{}",
            std::process::id(),
            HIDDEN_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("visible.txt"), []).unwrap();
        std::fs::write(root.join(".secret"), []).unwrap();
        std::fs::create_dir(root.join(".private-folder")).unwrap();
        let state = PanelState::default();
        let visible = read_entries(&root, &state, false);
        assert_eq!(
            visible
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["visible.txt"]
        );
        let all = read_entries(&root, &state, true);
        assert!(all.iter().any(|entry| entry.name == ".secret"));
        assert!(
            all.iter()
                .any(|entry| entry.name == ".private-folder" && entry.directory)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_picker_shortcuts_accept_control_or_command_and_require_a_modifier() {
        use sdl3::keycodes::{keymod, scancode};

        assert_eq!(
            picker_shortcut(scancode::H, keymod::CTRL),
            Some(PickerShortcut::ToggleHiddenFiles)
        );
        assert_eq!(
            picker_shortcut(scancode::H, keymod::GUI),
            Some(PickerShortcut::ToggleHiddenFiles)
        );
        assert_eq!(
            picker_shortcut(scancode::F, keymod::CTRL),
            Some(PickerShortcut::FocusSearch)
        );
        assert_eq!(
            picker_shortcut(scancode::R, keymod::CTRL),
            Some(PickerShortcut::Refresh)
        );
        assert_eq!(picker_shortcut(scancode::H, keymod::NONE), None);
        assert_eq!(picker_shortcut(scancode::H, keymod::ALT), None);
    }

    #[test]
    fn parsed_extensions_are_case_insensitive_and_ignore_uniform_type_identifiers() {
        assert_eq!(
            recognized_extensions(&[".RBXL".into(), "rbxm".into(), "public.data".into()]),
            vec!["rbxl".to_string(), "rbxm".to_string()]
        );
        assert!(allowed_file(Path::new("place.RBXL"), &["rbxl".into()]));
        assert!(!allowed_file(Path::new("image.png"), &["rbxl".into()]));
        assert!(allowed_file(
            Path::new("anything.bin"),
            &["public.data".into()]
        ));
    }

    #[test]
    fn selected_save_name_is_joined_to_the_current_directory() {
        let panel = PanelState {
            save: true,
            ..PanelState::default()
        };
        let picker = chrome::file_picker::FilePicker {
            filename: "place.rbxl".into(),
            ..chrome::file_picker::FilePicker::default()
        };
        assert_eq!(
            selected_paths(&picker, &panel, Path::new("/tmp")),
            Some(vec![PathBuf::from("/tmp/place.rbxl")])
        );
    }

    #[test]
    fn save_names_cannot_escape_the_selected_directory() {
        let panel = PanelState {
            save: true,
            ..PanelState::default()
        };

        for filename in [
            "",
            ".",
            "..",
            "../outside.rbxl",
            "nested/place.rbxl",
            "/tmp/place.rbxl",
            r"nested\place.rbxl",
            "place\0.rbxl",
        ] {
            let picker = chrome::file_picker::FilePicker {
                filename: filename.into(),
                ..chrome::file_picker::FilePicker::default()
            };
            assert_eq!(
                selected_paths(&picker, &panel, Path::new("/tmp")),
                None,
                "accepted unsafe save name {filename:?}"
            );
        }
    }

    #[test]
    fn an_unavailable_portal_uses_the_internal_picker() {
        let fallback_calls = std::cell::Cell::new(0);
        let result = resolve_picker_result(PortalOutcome::Unavailable, false, || {
            fallback_calls.set(fallback_calls.get() + 1);
            Some(PickerResult {
                selected: Some(vec![PathBuf::from("/tmp/place.rbxl")]),
                shows_hidden_files: false,
            })
        })
        .unwrap();

        assert_eq!(fallback_calls.get(), 1);
        assert_eq!(
            result.selected,
            Some(vec![PathBuf::from("/tmp/place.rbxl")])
        );
    }

    #[test]
    fn cancelling_the_portal_does_not_open_the_internal_picker() {
        let fallback_calls = std::cell::Cell::new(0);
        let result = resolve_picker_result(PortalOutcome::Cancelled, true, || {
            fallback_calls.set(fallback_calls.get() + 1);
            None
        })
        .unwrap();

        assert_eq!(fallback_calls.get(), 0);
        assert!(result.selected.is_none());
        assert!(result.shows_hidden_files);
    }

    #[test]
    fn selected_open_entries_keep_multiple_selection_order() {
        let panel = PanelState {
            multiple: true,
            can_choose_files: true,
            ..PanelState::default()
        };
        let picker = chrome::file_picker::FilePicker {
            entries: vec![
                chrome::file_picker::Entry {
                    name: "first.rbxl".into(),
                    directory: false,
                },
                chrome::file_picker::Entry {
                    name: "second.rbxm".into(),
                    directory: false,
                },
            ],
            selected: vec![1, 0],
            ..chrome::file_picker::FilePicker::default()
        };
        assert_eq!(
            selected_paths(&picker, &panel, Path::new("/tmp")),
            Some(vec![
                PathBuf::from("/tmp/second.rbxm"),
                PathBuf::from("/tmp/first.rbxl")
            ])
        );
    }

    #[test]
    fn completion_receives_the_selected_response_once() {
        let block = [0usize, 0usize, record_response as *const () as usize];
        CALLBACK_COUNT.store(0, Ordering::SeqCst);
        CALLBACK_RESPONSE.store(0, Ordering::SeqCst);

        complete(block.as_ptr() as Id, ACCEPT);

        assert_eq!(CALLBACK_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(CALLBACK_RESPONSE.load(Ordering::SeqCst), ACCEPT);
    }
}
