use std::sync::{Mutex, OnceLock};

use crate::raster::Bitmap;

/// What `-setApplicationIconImage:` last set, which wins over the bundle's.
static REPLACEMENT: Mutex<Option<Bitmap>> = Mutex::new(None);

/// The bundle's icon, read and decoded once.
///
/// A bundle with no icon is the common case for a loose binary and is silent; a
/// bundle whose icon this tree cannot decode says so once, because that is a
/// missing decoder rather than a missing file.
fn bundled() -> Option<&'static Bitmap> {
    static ICON: OnceLock<Option<Bitmap>> = OnceLock::new();
    ICON.get_or_init(|| {
        let path = foundation::bundle::main()?.icon()?;
        let bytes = std::fs::read(&path).ok()?;
        let bitmap = crate::raster::decode(&bytes);
        if bitmap.is_none() {
            eprintln!(
                "rosetta3: appkit: {}: no decoder for this icon - see appkit::raster",
                path.display()
            );
        }
        bitmap
    })
    .as_ref()
}

/// The application's icon: what `-setApplicationIconImage:` set, else the
/// bundle's.
pub fn current() -> Option<Bitmap> {
    let replacement = REPLACEMENT.lock().unwrap_or_else(|e| e.into_inner());
    replacement.as_ref().or_else(|| bundled()).cloned()
}

/// Give `window` the application's icon, if there is one.
pub fn apply(window: sdl3::Window) {
    let replacement = REPLACEMENT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(icon) = replacement.as_ref().or_else(|| bundled()) else {
        return;
    };
    sdl3::video::set_window_icon(window, icon.width as i32, icon.height as i32, &icon.pixels);
}

/// Replace the icon, and tell the windows that already have the old one.
pub fn replace(bitmap: Option<Bitmap>) {
    *REPLACEMENT.lock().unwrap_or_else(|e| e.into_inner()) = bitmap;
    for window in crate::window::sdl_windows() {
        apply(window);
    }
}
