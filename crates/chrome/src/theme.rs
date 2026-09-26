use crate::paint::Color;

/// Colours and metrics for the chrome, after a current Mac's.
///
/// Every length is in points.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,

    pub titlebar_height: f32,
    pub strip_height: f32,
    /// The radius of a titled window's corners.
    pub window_corner_radius: f32,
    pub titlebar_top: Color,
    pub titlebar_bottom: Color,
    pub titlebar_inactive: Color,
    pub separator: Color,
    pub title: Color,
    pub title_inactive: Color,
    pub title_size: f32,

    pub light_diameter: f32,
    /// From the window's left edge to the first light's centre.
    pub light_inset: f32,
    /// Between neighbouring lights' centres.
    pub light_pitch: f32,
    pub close: Color,
    pub minimize: Color,
    pub zoom: Color,
    pub light_border: Color,
    pub light_inactive: Color,
    pub light_glyph: Color,

    pub menu_title: Color,
    pub menu_title_disabled: Color,
    pub menu_title_highlight: Color,
    pub menu_title_size: f32,
    pub menu_title_padding: f32,

    pub panel_background: Color,
    pub panel_border: Color,
    pub panel_text: Color,
    pub panel_text_disabled: Color,
    pub panel_selection: Color,
    pub panel_selection_text: Color,
    pub panel_separator: Color,
    pub panel_item_height: f32,
    pub panel_separator_height: f32,
    pub panel_padding: f32,
    pub panel_text_size: f32,
    /// Room left of the titles for a check mark.
    pub panel_leading: f32,
    pub panel_trailing: f32,
    pub panel_min_width: f32,

    pub alert_width: f32,
    pub alert_corner_radius: f32,
    pub alert_icon_size: f32,
    pub alert_background: Color,
    pub alert_border: Color,
    pub alert_text: Color,
    pub alert_message_size: f32,
    pub alert_informative_size: f32,
    pub alert_button_height: f32,
    pub alert_button: Color,
    pub alert_button_pressed: Color,
    pub alert_button_text: Color,
    /// The default button, in the accent colour.
    pub alert_default: Color,
    pub alert_default_pressed: Color,
    pub alert_default_text: Color,
}

impl Theme {
    pub fn light() -> Theme {
        Theme {
            dark: false,
            titlebar_height: 28.0,
            strip_height: 24.0,
            window_corner_radius: 10.0,
            titlebar_top: Color::rgb(0xEE, 0xEE, 0xEE),
            titlebar_bottom: Color::rgb(0xE4, 0xE4, 0xE4),
            titlebar_inactive: Color::rgb(0xF6, 0xF6, 0xF6),
            separator: Color::rgb(0xCF, 0xCF, 0xCF),
            title: Color::rgb(0x4D, 0x4D, 0x4D),
            title_inactive: Color::rgb(0xAE, 0xAE, 0xAE),
            title_size: 13.0,
            light_diameter: 12.0,
            light_inset: 14.0,
            light_pitch: 20.0,
            close: Color::rgb(0xFF, 0x5F, 0x57),
            minimize: Color::rgb(0xFE, 0xBC, 0x2E),
            zoom: Color::rgb(0x28, 0xC8, 0x40),
            light_border: Color::rgba(0, 0, 0, 0.12),
            light_inactive: Color::rgb(0xDC, 0xDC, 0xDC),
            light_glyph: Color::rgba(0, 0, 0, 0.55),
            menu_title: Color::rgba(0, 0, 0, 0.85),
            menu_title_disabled: Color::rgba(0, 0, 0, 0.3),
            menu_title_highlight: Color::rgba(0, 0, 0, 0.1),
            menu_title_size: 13.0,
            menu_title_padding: 9.0,
            panel_background: Color::rgb(0xF2, 0xF2, 0xF2),
            panel_border: Color::rgb(0xC4, 0xC4, 0xC4),
            panel_text: Color::rgba(0, 0, 0, 0.85),
            panel_text_disabled: Color::rgba(0, 0, 0, 0.3),
            panel_selection: Color::rgb(0x0A, 0x64, 0xD6),
            panel_selection_text: Color::rgb(0xFF, 0xFF, 0xFF),
            panel_separator: Color::rgba(0, 0, 0, 0.1),
            panel_item_height: 22.0,
            panel_separator_height: 11.0,
            panel_padding: 5.0,
            panel_text_size: 13.0,
            panel_leading: 22.0,
            panel_trailing: 18.0,
            panel_min_width: 180.0,
            alert_width: 260.0,
            alert_corner_radius: 12.0,
            alert_icon_size: 64.0,
            alert_background: Color::rgb(0xEC, 0xEC, 0xEC),
            alert_border: Color::rgba(0, 0, 0, 0.22),
            alert_text: Color::rgba(0, 0, 0, 0.85),
            alert_message_size: 13.0,
            alert_informative_size: 11.0,
            alert_button_height: 28.0,
            alert_button: Color::rgba(0, 0, 0, 0.08),
            alert_button_pressed: Color::rgba(0, 0, 0, 0.18),
            alert_button_text: Color::rgba(0, 0, 0, 0.85),
            alert_default: Color::rgb(0x00, 0x7A, 0xFF),
            alert_default_pressed: Color::rgb(0x00, 0x62, 0xD4),
            alert_default_text: Color::rgb(0xFF, 0xFF, 0xFF),
        }
    }

    pub fn dark() -> Theme {
        Theme {
            dark: true,
            titlebar_top: Color::rgb(0x3A, 0x3A, 0x3A),
            titlebar_bottom: Color::rgb(0x32, 0x32, 0x32),
            titlebar_inactive: Color::rgb(0x2C, 0x2C, 0x2C),
            separator: Color::rgb(0x12, 0x12, 0x12),
            title: Color::rgb(0xDD, 0xDD, 0xDD),
            title_inactive: Color::rgb(0x7A, 0x7A, 0x7A),
            light_border: Color::rgba(0, 0, 0, 0.25),
            light_inactive: Color::rgb(0x55, 0x55, 0x55),
            menu_title: Color::rgba(255, 255, 255, 0.88),
            menu_title_disabled: Color::rgba(255, 255, 255, 0.3),
            menu_title_highlight: Color::rgba(255, 255, 255, 0.14),
            panel_background: Color::rgb(0x2A, 0x2A, 0x2B),
            panel_border: Color::rgb(0x55, 0x55, 0x55),
            panel_text: Color::rgba(255, 255, 255, 0.88),
            panel_text_disabled: Color::rgba(255, 255, 255, 0.28),
            panel_selection: Color::rgb(0x0A, 0x5F, 0xD1),
            panel_separator: Color::rgba(255, 255, 255, 0.12),
            alert_background: Color::rgb(0x32, 0x32, 0x32),
            alert_border: Color::rgba(255, 255, 255, 0.18),
            alert_text: Color::rgba(255, 255, 255, 0.88),
            alert_button: Color::rgba(255, 255, 255, 0.16),
            alert_button_pressed: Color::rgba(255, 255, 255, 0.28),
            alert_button_text: Color::rgba(255, 255, 255, 0.88),
            alert_default: Color::rgb(0x0A, 0x84, 0xFF),
            alert_default_pressed: Color::rgb(0x40, 0x9C, 0xFF),
            ..Theme::light()
        }
    }

    pub fn for_appearance(dark: bool) -> Theme {
        match dark {
            true => Theme::dark(),
            false => Theme::light(),
        }
    }
}
