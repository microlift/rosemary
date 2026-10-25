use std::cell::Cell;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use coregraphics::cgcontext::*;
use coregraphics::focus::{Device, Draw, Provider};
use coregraphics::{
    CGAffineTransformIdentity, CGAffineTransformMakeScale, CGAffineTransformMakeTranslation,
    CGPoint, CGRect, CGSize,
};

thread_local! {
    static DEVICE: Cell<Option<Device>> = const { Cell::new(None) };
}

struct FocusProvider;

impl Provider for FocusProvider {
    fn device(&self) -> Option<Device> {
        DEVICE.get()
    }

    fn draw(&self, _draw: &mut Draw<'_>) -> bool {
        false
    }
}

struct Focus;

impl Focus {
    fn new() -> Self {
        static INSTALL: Once = Once::new();
        static NEXT_KEY: AtomicUsize = AtomicUsize::new(1);
        INSTALL.call_once(|| {
            coregraphics::bootstrap();
            coregraphics::focus::set_provider(Box::new(FocusProvider));
        });
        DEVICE.set(Some(Device {
            key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
            depth: 1,
            width: 2,
            height: 2,
            density: 1.0,
            flipped: true,
        }));
        Self
    }
}

impl Drop for Focus {
    fn drop(&mut self) {
        DEVICE.set(None);
    }
}

fn clip_left_column(context: CGContextRef) {
    // SAFETY: the live focus context and a rectangle passed by value.
    unsafe {
        CGContextClipToRect(
            context,
            CGRect {
                origin: CGPoint::default(),
                size: CGSize {
                    width: 1.0,
                    height: 2.0,
                },
            },
        );
    }
}

#[test]
fn appkit_save_and_restore_share_the_quartz_transform_and_clip() {
    let _focus = Focus::new();
    let context = focus_context();
    assert!(!context.is_null());
    assert_eq!(with_focus_clip(|_, _| ()), None);

    focus_save();
    // SAFETY: the transform constructor takes only values.
    let translated = unsafe { CGAffineTransformMakeTranslation(1.0, 0.0) };
    focus_update_ctm(|ctm| translated.then(ctm));
    clip_left_column(context);
    assert_eq!(
        with_focus_clip(|coverage, width| (coverage.to_vec(), width)),
        Some((vec![0, 255, 0, 255], 2)),
    );
    // SAFETY: the focus context remains live for this thread.
    assert_eq!(unsafe { CGContextGetCTM(context) }, translated);

    focus_restore();
    assert_eq!(with_focus_clip(|_, _| ()), None);
    // SAFETY: the focus context remains live for this thread.
    assert_eq!(
        unsafe { CGContextGetCTM(context) },
        CGAffineTransformIdentity
    );
    focus_restore();
    // SAFETY: restoring an empty stack leaves the context's state unchanged.
    assert_eq!(
        unsafe { CGContextGetCTM(context) },
        CGAffineTransformIdentity
    );
}

#[test]
fn appkit_can_concatenate_and_replace_the_quartz_transform() {
    let _focus = Focus::new();
    let context = focus_context();
    // SAFETY: the transform constructors take only values.
    let (translated, scaled) = unsafe {
        (
            CGAffineTransformMakeTranslation(3.0, 4.0),
            CGAffineTransformMakeScale(2.0, 3.0),
        )
    };
    // SAFETY: the live focus context.
    unsafe { CGContextSetCTM(context, translated) };

    focus_update_ctm(|ctm| scaled.then(ctm));
    // SAFETY: the live focus context.
    assert_eq!(unsafe { CGContextGetCTM(context) }, scaled.then(translated));

    focus_update_ctm(|_| scaled);
    // SAFETY: the live focus context.
    assert_eq!(unsafe { CGContextGetCTM(context) }, scaled);
}

#[test]
fn a_new_focus_resets_the_shared_transform_clip_and_saved_state() {
    let _focus = Focus::new();
    let context = focus_context();
    clip_left_column(context);
    // SAFETY: the transform constructor takes only values.
    focus_update_ctm(|_| unsafe { CGAffineTransformMakeScale(2.0, 3.0) });
    focus_save();
    DEVICE.set(DEVICE.get().map(|device| Device {
        depth: device.depth + 1,
        ..device
    }));

    assert_eq!(focus_context(), context);
    assert_eq!(with_focus_clip(|_, _| ()), None);
    focus_restore();
    // SAFETY: the focus context is reused with a fresh state.
    assert_eq!(
        unsafe { CGContextGetCTM(context) },
        CGAffineTransformIdentity
    );
}

#[test]
fn focus_state_operations_do_nothing_without_a_focused_view() {
    drop(Focus::new());
    assert!(focus_context().is_null());
    focus_save();
    focus_restore();
    focus_update_ctm(|_| panic!("there is no transform to update without a focus"));
    assert_eq!(
        with_focus_clip(|_, _| panic!("there is no clip without a focus")),
        None::<()>,
    );
}
