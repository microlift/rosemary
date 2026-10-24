#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals
)]

pub mod cgaffinetransform;
pub mod cgbitmap;
pub mod cgcolor;
pub mod cgcontext;
pub mod cgdataprovider;
pub mod cgdisplay;
pub mod cgfont;
pub mod cggeometry;
pub mod cgimage;
pub mod cgpaint;
pub mod cgpath;
pub mod cgpdf;
pub mod cgwindow;
pub mod focus;
pub mod font_attributes;
pub mod fonts;
pub mod metal;
pub mod quartz;
pub mod quartz_display;
pub mod quartz_event;
pub mod quartz_window;
pub mod raster;

pub use cgaffinetransform::*;
pub use cgpath::*;

pub type CGFloat = f64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: CGFloat,
    pub height: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGVector {
    pub dx: CGFloat,
    pub dy: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: CGFloat,
    pub y: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// Register the classes a Core Graphics object is, once.
///
/// Called from [`data_symbols`] the way `appkit::bootstrap` is: the provider is
/// built before anything can ask for a class, so this has to have run by then.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        cgpath::install();
        cgcolor::install();
        cgdataprovider::install();
        cgimage::install();
        cgcontext::install();
    });
}

/// The data symbols this framework publishes, in the Mach-O spelling.
///
/// [`exports!`]'s `data` section feeds `provide()` only; a `dlsym` of a
/// variable goes through here, so the two must name the same addresses.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = vec![(
        "_CGAffineTransformIdentity".to_string(),
        (&raw const CGAffineTransformIdentity) as usize as u64,
    )];
    out.extend(cgcolor::data_symbols());
    out
}

exports::exports! {
    groups {
        cgaffinetransform,
        cgbitmap,
        cgcolor,
        cgdataprovider,
        cgdisplay,
        cgfont,
        cggeometry,
        cgimage,
        cgpath,
        cgpdf,
        cgwindow,
        cgcontext,
        cgpaint,
        metal,
        quartz_display,
        quartz_event,
        quartz_window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_groups_publish_the_drawing_calls() {
        let names = names();
        for name in [
            "CGBitmapContextCreate",
            "CGColorCreate",
            "CGColorSpaceCreateDeviceRGB",
            "CGContextFillRect",
            "CGDataProviderCreateWithData",
            "CGEventCreate",
            "CGImageCreate",
            "CGPatternCreate",
        ] {
            assert!(names.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_identity_is_published_and_points_at_its_fields() {
        let symbols = data_symbols();
        let (_, identity) = symbols
            .iter()
            .find(|(name, _)| name == "_CGAffineTransformIdentity")
            .expect("published");
        // SAFETY: the symbol points at the six-double static above.
        let fields = unsafe { std::slice::from_raw_parts(*identity as *const f64, 6) };
        assert_eq!(fields, &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }
}
