//! The view a focus context draws into, as AppKit tells CoreGraphics.
//!
//! A focus context has no pixels of its own: it draws into the backing store
//! of the view `-drawRect:` focused. CoreGraphics cannot know about views, so
//! AppKit installs a [`Provider`] that answers where the focused view is and
//! runs a draw against its pixels.

use std::sync::OnceLock;

/// What vector drawing needs to know about the focused view: where its pixels
/// are, and which focus it is.
#[derive(Clone, Copy, PartialEq)]
pub struct Device {
    /// The view (or offscreen key) and how deep in the focus stack it is,
    /// which together tell one `-drawRect:` from the next.
    pub key: usize,
    pub depth: usize,
    pub width: u32,
    pub height: u32,
    pub density: f64,
    pub flipped: bool,
}

impl Device {
    /// A point in the view's coordinates, in pixels from its top left.
    pub fn to_pixels(self, x: f64, y: f64) -> [f64; 2] {
        let y = match self.flipped {
            true => y * self.density,
            false => f64::from(self.height) - y * self.density,
        };
        [x * self.density, y]
    }
}

/// The rectangle of pixels a draw changed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Damage {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Damage {
    pub fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

/// A draw over the focused view's pixels: premultiplied B, G, R, A, top row
/// first, with their width and height, answering the rectangle it changed.
pub type Draw<'a> = dyn FnMut(&mut [u8], u32, u32) -> Option<Damage> + 'a;

/// What AppKit provides to CoreGraphics: the focused view's device, and a way
/// to draw into its pixels.
pub trait Provider: Send + Sync {
    fn device(&self) -> Option<Device>;

    /// Run `draw` over the focused view's pixels. False with nothing focused.
    fn draw(&self, draw: &mut Draw<'_>) -> bool;
}

static PROVIDER: OnceLock<Box<dyn Provider>> = OnceLock::new();

/// Install the provider AppKit answers focus queries with.
pub fn set_provider(provider: Box<dyn Provider>) {
    let _ = PROVIDER.set(provider);
}

/// The focused view, or `None` with nothing focused.
pub fn device() -> Option<Device> {
    PROVIDER.get()?.device()
}

/// Draw into the focused view's pixels, or answer false with nothing focused.
pub fn draw(draw: &mut Draw<'_>) -> bool {
    PROVIDER.get().is_some_and(|provider| provider.draw(draw))
}
