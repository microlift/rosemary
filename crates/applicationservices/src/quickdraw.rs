//! The two QuickDraw rectangle calls Clip Studio Paint's plugin bundles still
//! import out of `ApplicationServices` - deprecated since Mac OS X 10.4, long
//! after QuickDraw itself, but never removed from the framework.

#![allow(non_snake_case)]

/// `Rect`: four `short`s in `top, left, bottom, right` order - QuickDraw's own
/// field order, not the left-top-right-bottom order [`SetRect`]'s arguments
/// come in.
#[repr(C)]
pub struct Rect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

/// `void SetRect(Rect *r, short left, short top, short right, short bottom)`.
///
/// # Safety
///
/// `r` is null or a writable [`Rect`].
pub unsafe extern "C" fn SetRect(r: *mut Rect, left: i16, top: i16, right: i16, bottom: i16) {
    if r.is_null() {
        return;
    }
    // SAFETY: checked non-null above; writable is the caller's obligation.
    unsafe {
        (*r).top = top;
        (*r).left = left;
        (*r).bottom = bottom;
        (*r).right = right;
    }
}

/// `void OffsetRect(Rect *r, short dh, short dv)`: `dh` moves both vertical
/// edges, `dv` both horizontal ones.
///
/// # Safety
///
/// As [`SetRect`].
pub unsafe extern "C" fn OffsetRect(r: *mut Rect, dh: i16, dv: i16) {
    if r.is_null() {
        return;
    }
    // SAFETY: as `SetRect`.
    unsafe {
        (*r).left = (*r).left.wrapping_add(dh);
        (*r).right = (*r).right.wrapping_add(dh);
        (*r).top = (*r).top.wrapping_add(dv);
        (*r).bottom = (*r).bottom.wrapping_add(dv);
    }
}

exports::exports! {
    functions {
        "SetRect" => SetRect,
        "OffsetRect" => OffsetRect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_rect_takes_its_arguments_left_top_right_bottom() {
        let mut r = Rect {
            top: 0,
            left: 0,
            bottom: 0,
            right: 0,
        };
        // SAFETY: `r` is a live, writable `Rect`.
        unsafe { SetRect(&mut r, 1, 2, 3, 4) };
        assert_eq!((r.top, r.left, r.bottom, r.right), (2, 1, 4, 3));
    }

    #[test]
    fn offset_rect_moves_every_edge() {
        let mut r = Rect {
            top: 10,
            left: 20,
            bottom: 30,
            right: 40,
        };
        // SAFETY: `r` is a live, writable `Rect`.
        unsafe { OffsetRect(&mut r, 5, -2) };
        assert_eq!((r.top, r.left, r.bottom, r.right), (8, 25, 28, 45));
    }

    #[test]
    fn a_null_rect_is_a_no_op() {
        // SAFETY: null is documented as accepted.
        unsafe {
            SetRect(std::ptr::null_mut(), 1, 2, 3, 4);
            OffsetRect(std::ptr::null_mut(), 1, 2);
        }
    }
}
