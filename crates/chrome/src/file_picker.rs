use crate::geometry::{Point, Rect};
use crate::paint::{Canvas, Color};
use crate::text::Font;
use crate::theme::Theme;
use crate::view::View;

const PADDING: f32 = 14.0;
const TOOLBAR_HEIGHT: f32 = 32.0;
const ROW_HEIGHT: f32 = 28.0;
const BUTTON_HEIGHT: f32 = 30.0;
const BUTTON_RADIUS: f32 = 6.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Parent,
    Entry(usize),
    Filename,
    Search,
    HiddenFiles,
    Cancel,
    Accept,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub bounds: Rect,
    pub location: Rect,
    pub parent: Rect,
    pub search: Rect,
    pub hidden_files: Rect,
    pub list: Rect,
    pub rows: Vec<(usize, Rect)>,
    pub filename: Option<Rect>,
    pub cancel: Rect,
    pub accept: Rect,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilePicker {
    pub title: String,
    pub location: String,
    pub entries: Vec<Entry>,
    pub selected: Vec<usize>,
    pub scroll: usize,
    pub filename: String,
    pub save: bool,
    pub multiple: bool,
    pub accept_title: String,
    pub pressed: Option<Part>,
    pub filename_focused: bool,
    pub search: String,
    pub search_focused: bool,
    pub shows_hidden_files: bool,
}

impl FilePicker {
    pub fn matching_indices(&self) -> Vec<usize> {
        let search = self.search.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| search.is_empty() || entry.name.to_lowercase().contains(&search))
            .map(|(index, _)| index)
            .collect()
    }
}

impl View for FilePicker {
    type Part = Part;
    type Layout = Layout;

    fn layout(&self, bounds: Rect, _theme: &Theme) -> Layout {
        let width = bounds.width.max(480.0);
        let height = bounds.height.max(320.0);
        let parent = Rect::new(bounds.x + PADDING, bounds.y + 52.0, 58.0, TOOLBAR_HEIGHT);
        let location = Rect::new(
            parent.right() + 8.0,
            parent.y,
            (width - (parent.right() - bounds.x) - PADDING - 8.0).max(80.0),
            TOOLBAR_HEIGHT,
        );
        let toolbar_y = parent.bottom() + 10.0;
        let hidden_width = 122.0;
        let search = Rect::new(
            bounds.x + PADDING,
            toolbar_y,
            (width - PADDING * 2.0 - hidden_width - 8.0).max(96.0),
            TOOLBAR_HEIGHT,
        );
        let hidden_files = Rect::new(
            search.right() + 8.0,
            toolbar_y,
            hidden_width,
            TOOLBAR_HEIGHT,
        );
        let footer_height = if self.save { 96.0 } else { 58.0 };
        let list = Rect::new(
            bounds.x + PADDING,
            search.bottom() + 10.0,
            width - PADDING * 2.0,
            (height - footer_height - (search.bottom() - bounds.y) - 24.0).max(96.0),
        );
        let filename = self.save.then(|| {
            Rect::new(
                bounds.x + PADDING + 70.0,
                bounds.y + height - 85.0,
                width - PADDING * 2.0 - 70.0,
                TOOLBAR_HEIGHT,
            )
        });
        let buttons_y = bounds.y + height - BUTTON_HEIGHT - 14.0;
        let accept_width = 88.0;
        let cancel_width = 84.0;
        let cancel = Rect::new(
            bounds.x + width - PADDING - accept_width - 8.0 - cancel_width,
            buttons_y,
            cancel_width,
            BUTTON_HEIGHT,
        );
        let accept = Rect::new(
            bounds.x + width - PADDING - accept_width,
            buttons_y,
            accept_width,
            BUTTON_HEIGHT,
        );
        let row_count = (list.height / ROW_HEIGHT).floor().max(0.0) as usize;
        let matching_entries = self.matching_indices();
        let rows = matching_entries
            .iter()
            .skip(self.scroll)
            .take(row_count)
            .enumerate()
            .map(|(visible, index)| {
                (
                    *index,
                    Rect::new(
                        list.x,
                        list.y + visible as f32 * ROW_HEIGHT,
                        list.width,
                        ROW_HEIGHT,
                    ),
                )
            })
            .collect();
        Layout {
            bounds: Rect::new(bounds.x, bounds.y, width, height),
            location,
            parent,
            search,
            hidden_files,
            list,
            rows,
            filename,
            cancel,
            accept,
        }
    }

    fn paint(&self, canvas: &mut Canvas, layout: &Layout, theme: &Theme) {
        canvas.fill_rect(layout.bounds, theme.panel_background);
        canvas.stroke_rounded_rect(
            layout.bounds.inset(0.5, 0.5),
            8.0,
            1.0 / canvas.scale(),
            theme.panel_border,
        );
        let Some(font) = Font::system(400) else {
            return;
        };
        let bold = Font::system(700).unwrap_or_else(|| font.clone());
        let title_size = 16.0;
        draw_label(
            canvas,
            &bold,
            title_size,
            &self.title,
            Point::new(layout.bounds.x + PADDING, layout.bounds.y + 20.0),
            theme.panel_text,
        );
        draw_button(
            canvas,
            &font,
            layout.parent,
            "Up",
            self.pressed == Some(Part::Parent),
            false,
            theme,
        );
        canvas.fill_rounded_rect(layout.location, 5.0, theme.panel_input_background);
        canvas.stroke_rounded_rect(
            layout.location,
            5.0,
            1.0 / canvas.scale(),
            theme.panel_border,
        );
        let location = font.truncate(
            &self.location,
            theme.panel_text_size,
            layout.location.width - 16.0,
        );
        draw_centered(
            canvas,
            &font,
            theme.panel_text_size,
            &location,
            layout.location,
            theme.panel_text,
        );
        canvas.fill_rounded_rect(layout.search, 5.0, theme.panel_input_background);
        canvas.stroke_rounded_rect(
            layout.search,
            5.0,
            if self.search_focused { 2.0 } else { 1.0 } / canvas.scale(),
            if self.search_focused {
                theme.panel_selection
            } else {
                theme.panel_border
            },
        );
        let search_text = if self.search.is_empty() {
            "Search this folder (Ctrl+F)"
        } else {
            &self.search
        };
        let search_color = if self.search.is_empty() {
            theme.panel_text_disabled
        } else {
            theme.panel_text
        };
        let search_text = font.truncate(
            search_text,
            theme.panel_text_size,
            layout.search.width - 18.0,
        );
        draw_label(
            canvas,
            &font,
            theme.panel_text_size,
            &search_text,
            Point::new(layout.search.x + 9.0, layout.search.y + 7.0),
            search_color,
        );
        if self.search_focused {
            let cursor_x = layout.search.x
                + 9.0
                + if self.search.is_empty() {
                    0.0
                } else {
                    font.measure(&search_text, theme.panel_text_size)
                };
            canvas.fill_rect(
                Rect::new(
                    cursor_x.min(layout.search.right() - 4.0),
                    layout.search.y + 7.0,
                    1.0,
                    18.0,
                ),
                theme.panel_text,
            );
        }
        draw_button(
            canvas,
            &font,
            layout.hidden_files,
            if self.shows_hidden_files {
                "Hidden: On"
            } else {
                "Hidden: Off"
            },
            self.pressed == Some(Part::HiddenFiles),
            self.shows_hidden_files,
            theme,
        );
        canvas.fill_rounded_rect(layout.list, 5.0, theme.panel_input_background);
        canvas.stroke_rounded_rect(layout.list, 5.0, 1.0 / canvas.scale(), theme.panel_border);
        canvas.push_clip(layout.list.inset(1.0, 1.0));
        if layout.rows.is_empty() {
            draw_centered(
                canvas,
                &font,
                theme.panel_text_size,
                if self.entries.is_empty() {
                    "This folder is empty"
                } else {
                    "No matching files"
                },
                layout.list,
                theme.panel_text_disabled,
            );
        }
        for (index, row) in &layout.rows {
            let selected = self.selected.contains(index);
            if selected {
                canvas.fill_rect(*row, theme.panel_selection);
            } else if index % 2 == 1 {
                canvas.fill_rect(*row, Color::rgba(0, 0, 0, 0.025));
            }
            let Some(entry) = self.entries.get(*index) else {
                continue;
            };
            let foreground = if selected {
                theme.panel_selection_text
            } else {
                theme.panel_text
            };
            draw_entry_icon(
                canvas,
                &font,
                Rect::new(row.x + 9.0, row.y + 4.0, 20.0, 20.0),
                EntryIcon::for_entry(entry),
            );
            let name = font.truncate(&entry.name, theme.panel_text_size, row.width - 44.0);
            draw_label(
                canvas,
                &font,
                theme.panel_text_size,
                &name,
                Point::new(row.x + 36.0, row.y + 6.0),
                foreground,
            );
        }
        canvas.pop_clip();
        if let Some(filename) = layout.filename {
            draw_label(
                canvas,
                &font,
                theme.panel_text_size,
                "Save as:",
                Point::new(layout.bounds.x + PADDING, filename.y + 8.0),
                theme.panel_text,
            );
            let focused = self.filename_focused;
            canvas.fill_rounded_rect(filename, 5.0, theme.panel_input_background);
            canvas.stroke_rounded_rect(
                filename,
                5.0,
                if focused { 2.0 } else { 1.0 } / canvas.scale(),
                if focused {
                    theme.panel_selection
                } else {
                    theme.panel_border
                },
            );
            let name = font.truncate(&self.filename, theme.panel_text_size, filename.width - 16.0);
            draw_label(
                canvas,
                &font,
                theme.panel_text_size,
                &name,
                Point::new(filename.x + 8.0, filename.y + 8.0),
                theme.panel_text,
            );
        }
        draw_button(
            canvas,
            &font,
            layout.cancel,
            "Cancel",
            self.pressed == Some(Part::Cancel),
            false,
            theme,
        );
        draw_button(
            canvas,
            &font,
            layout.accept,
            &self.accept_title,
            self.pressed == Some(Part::Accept),
            true,
            theme,
        );
    }

    fn hit(&self, layout: &Layout, point: Point) -> Option<Part> {
        if layout.parent.contains(point) {
            return Some(Part::Parent);
        }
        if let Some((index, _)) = layout.rows.iter().find(|(_, rect)| rect.contains(point)) {
            return Some(Part::Entry(*index));
        }
        if layout.search.contains(point) {
            return Some(Part::Search);
        }
        if layout.hidden_files.contains(point) {
            return Some(Part::HiddenFiles);
        }
        if layout.filename.is_some_and(|rect| rect.contains(point)) {
            return Some(Part::Filename);
        }
        if layout.cancel.contains(point) {
            return Some(Part::Cancel);
        }
        if layout.accept.contains(point) {
            return Some(Part::Accept);
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryIcon {
    Folder,
    Package,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Text,
    Document,
    Generic,
}

impl EntryIcon {
    fn for_entry(entry: &Entry) -> Self {
        let extension = entry
            .name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "app" | "appex" | "bundle" | "framework" | "plugin" | "pkg" | "xcodeproj"
            | "xcworkspace" | "playground" | "key" | "numbers" | "pages" | "rtfd" => Self::Package,
            _ if entry.directory => Self::Folder,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "tif" | "tiff" | "icns" => {
                Self::Image
            }
            "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => Self::Video,
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" => Self::Audio,
            "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "dmg" => Self::Archive,
            "rs" | "c" | "cc" | "cpp" | "h" | "hpp" | "m" | "mm" | "swift" | "py" | "rb" | "sh"
            | "bash" | "zsh" | "js" | "jsx" | "ts" | "tsx" | "json" | "xml" | "html" | "css"
            | "toml" | "yaml" | "yml" => Self::Code,
            "txt" | "md" | "log" | "csv" | "plist" | "ini" | "cfg" | "conf" => Self::Text,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "rtf" => {
                Self::Document
            }
            _ => Self::Generic,
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Folder => Color::rgb(0xE0, 0xAC, 0x31),
            Self::Package => Color::rgb(0x9A, 0x78, 0xC5),
            Self::Image => Color::rgb(0x48, 0x9A, 0x73),
            Self::Video => Color::rgb(0x9B, 0x68, 0xC4),
            Self::Audio => Color::rgb(0xC5, 0x78, 0x3B),
            Self::Archive => Color::rgb(0xB0, 0x80, 0x43),
            Self::Code => Color::rgb(0x4E, 0x82, 0xB9),
            Self::Text => Color::rgb(0x68, 0x8A, 0xA2),
            Self::Document => Color::rgb(0xC2, 0x62, 0x62),
            Self::Generic => Color::rgb(0x7C, 0x9B, 0xC5),
        }
    }

    fn badge(self) -> &'static str {
        match self {
            Self::Folder | Self::Package => "",
            Self::Image => "IMG",
            Self::Video => "VID",
            Self::Audio => "AUD",
            Self::Archive => "ZIP",
            Self::Code => "<>",
            Self::Text => "TXT",
            Self::Document => "DOC",
            Self::Generic => "FILE",
        }
    }
}

fn draw_entry_icon(canvas: &mut Canvas, font: &Font, rect: Rect, icon: EntryIcon) {
    let color = icon.color();
    canvas.fill_rounded_rect(rect, 4.0, color);
    if icon == EntryIcon::Folder {
        canvas.fill_rounded_rect(
            Rect::new(rect.x + 2.0, rect.y + 3.0, rect.width * 0.48, 3.0),
            1.0,
            color,
        );
    } else if icon == EntryIcon::Package {
        canvas.stroke_rounded_rect(rect.inset(3.0, 3.0), 2.0, 1.0, Color::rgb(255, 255, 255));
    } else {
        draw_centered(
            canvas,
            font,
            6.0,
            icon.badge(),
            rect,
            Color::rgb(255, 255, 255),
        );
    }
}

fn draw_label(
    canvas: &mut Canvas,
    font: &Font,
    size: f32,
    text: &str,
    origin: Point,
    color: Color,
) {
    canvas.draw_text(
        text,
        font,
        size,
        Point::new(
            origin.x.round(),
            font.centered_baseline(size, origin.y, size * 1.5),
        ),
        color,
    );
}

fn draw_centered(
    canvas: &mut Canvas,
    font: &Font,
    size: f32,
    text: &str,
    rect: Rect,
    color: Color,
) {
    let text = font.truncate(text, size, rect.width - 16.0);
    let advance = font.measure(&text, size);
    draw_label(
        canvas,
        font,
        size,
        &text,
        Point::new(rect.center().x - advance / 2.0, rect.y),
        color,
    );
}

fn draw_button(
    canvas: &mut Canvas,
    font: &Font,
    rect: Rect,
    label: &str,
    pressed: bool,
    primary: bool,
    theme: &Theme,
) {
    let (background, foreground) = match (primary, pressed) {
        (true, true) => (theme.alert_default_pressed, theme.alert_default_text),
        (true, false) => (theme.alert_default, theme.alert_default_text),
        (false, true) => (theme.alert_button_pressed, theme.alert_button_text),
        (false, false) => (theme.alert_button, theme.alert_button_text),
    };
    canvas.fill_rounded_rect(rect, BUTTON_RADIUS, background);
    draw_centered(canvas, font, theme.panel_text_size, label, rect, foreground);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_names_case_insensitively_and_keeps_entry_indices() {
        let mut picker = FilePicker {
            entries: vec![
                Entry {
                    name: "Folder".into(),
                    directory: true,
                },
                Entry {
                    name: "Place.RBXL".into(),
                    directory: false,
                },
                Entry {
                    name: "Image.PNG".into(),
                    directory: false,
                },
            ],
            ..FilePicker::default()
        };
        picker.search = "rbxl".into();
        assert_eq!(picker.matching_indices(), vec![1]);
        picker.search = "PNG".into();
        assert_eq!(picker.matching_indices(), vec![2]);
        picker.search.clear();
        assert_eq!(picker.matching_indices(), vec![0, 1, 2]);
    }

    #[test]
    fn filtered_rows_hit_the_underlying_entry_index() {
        let picker = FilePicker {
            entries: vec![
                Entry {
                    name: "Place.rbxl".into(),
                    directory: false,
                },
                Entry {
                    name: "Model.rbxm".into(),
                    directory: false,
                },
            ],
            search: "model".into(),
            ..FilePicker::default()
        };
        let layout = picker.layout(Rect::new(0.0, 0.0, 720.0, 500.0), &Theme::light());
        assert_eq!(layout.rows.len(), 1);
        assert_eq!(
            picker.hit(&layout, layout.rows[0].1.center()),
            Some(Part::Entry(1))
        );
    }

    #[test]
    fn entry_icons_follow_common_file_categories() {
        let entry = |name: &str, directory| Entry {
            name: name.into(),
            directory,
        };
        assert_eq!(
            EntryIcon::for_entry(&entry("Assets", true)),
            EntryIcon::Folder
        );
        assert_eq!(
            EntryIcon::for_entry(&entry("Studio.app", true)),
            EntryIcon::Package
        );
        assert_eq!(
            EntryIcon::for_entry(&entry("icon.PNG", false)),
            EntryIcon::Image
        );
        assert_eq!(
            EntryIcon::for_entry(&entry("main.rs", false)),
            EntryIcon::Code
        );
        assert_eq!(
            EntryIcon::for_entry(&entry("archive.zip", false)),
            EntryIcon::Archive
        );
        assert_eq!(
            EntryIcon::for_entry(&entry("unknown.bin", false)),
            EntryIcon::Generic
        );
    }

    #[test]
    fn rows_and_footer_controls_do_not_overlap() {
        let picker = FilePicker {
            save: true,
            entries: (0..20)
                .map(|index| Entry {
                    name: format!("file-{index}.rbxl"),
                    directory: false,
                })
                .collect(),
            ..FilePicker::default()
        };
        let layout = picker.layout(Rect::new(0.0, 0.0, 720.0, 500.0), &Theme::light());
        assert!(
            layout
                .rows
                .iter()
                .all(|(_, row)| { row.y >= layout.list.y && row.bottom() <= layout.list.bottom() })
        );
        assert!(layout.list.bottom() < layout.filename.unwrap().y);
        assert!(layout.filename.unwrap().bottom() < layout.cancel.y);
        assert!(layout.cancel.right() < layout.accept.x);
    }

    #[test]
    fn hit_testing_uses_the_same_visible_rows_that_are_painted() {
        let picker = FilePicker {
            entries: vec![
                Entry {
                    name: "folder".into(),
                    directory: true,
                },
                Entry {
                    name: "place.rbxl".into(),
                    directory: false,
                },
            ],
            ..FilePicker::default()
        };
        let layout = picker.layout(Rect::new(0.0, 0.0, 720.0, 500.0), &Theme::light());
        assert_eq!(
            picker.hit(&layout, layout.rows[1].1.center()),
            Some(Part::Entry(1))
        );
        assert_eq!(
            picker.hit(&layout, layout.accept.center()),
            Some(Part::Accept)
        );
        assert_eq!(
            picker.hit(&layout, layout.cancel.center()),
            Some(Part::Cancel)
        );
    }

    #[test]
    fn layout_respects_its_origin() {
        let picker = FilePicker::default();
        let bounds = Rect::new(20.0, 30.0, 720.0, 500.0);
        let layout = picker.layout(bounds, &Theme::light());
        assert_eq!(layout.parent.x, 20.0 + PADDING);
        assert_eq!(layout.parent.y, 30.0 + 52.0);
        assert!(layout.cancel.x >= bounds.x);
        assert!(layout.accept.right() <= bounds.right());
    }

    #[test]
    fn dark_mode_file_names_have_contrast_against_the_list_background() {
        if Font::system(400).is_none() {
            return;
        }
        let theme = Theme::dark();
        let picker = FilePicker {
            entries: vec![Entry {
                name: "visible-file.rbxl".into(),
                directory: false,
            }],
            ..FilePicker::default()
        };
        let bounds = Rect::new(0.0, 0.0, 720.0, 500.0);
        let layout = picker.layout(bounds, &theme);
        let mut pixels = vec![0; 720 * 500 * 4];
        let mut canvas = Canvas::new(&mut pixels, 720, 500, 720 * 4, 1.0);
        picker.paint(&mut canvas, &layout, &theme);
        drop(canvas);

        let row = layout.rows[0].1;
        let has_dark_background = (row.y as usize..row.bottom() as usize).any(|y| {
            ((row.x as usize + 32)..(row.x as usize + 180)).any(|x| {
                let pixel = &pixels[(y * 720 + x) * 4..][..4];
                pixel[0] < 90 && pixel[1] < 90 && pixel[2] < 90
            })
        });
        let has_light_text = (row.y as usize..row.bottom() as usize).any(|y| {
            ((row.x as usize + 32)..(row.x as usize + 180)).any(|x| {
                let pixel = &pixels[(y * 720 + x) * 4..][..4];
                pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200
            })
        });

        assert!(has_dark_background);
        assert!(has_light_text);
    }
}
