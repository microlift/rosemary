//! Rosemary's own interface toolkit.
//!
//! A small, host-side layer - geometry, a software painter, text, a theme and
//! a few views.

pub mod alert;
pub mod checkbox;
pub mod corners;
pub mod file_picker;
pub mod fonts;
pub mod geometry;
pub mod menu;
pub mod paint;
pub mod rich;
pub mod text;
pub mod theme;
pub mod titlebar;
pub mod view;

pub use geometry::{Point, Rect, Size};
pub use paint::{Canvas, Color, Image, Path};
pub use rich::RichText;
pub use text::Font;
pub use theme::Theme;
pub use view::View;
