//! `CGAffineTransform` and the functions that build and combine one.

#![allow(non_snake_case)]

use crate::{CGFloat, CGPoint, CGRect, CGSize, CGVector};

fn concat(t1: CGAffineTransform, t2: CGAffineTransform) -> CGAffineTransform {
    CGAffineTransform {
        a: t1.a * t2.a + t1.b * t2.c,
        b: t1.a * t2.b + t1.b * t2.d,
        c: t1.c * t2.a + t1.d * t2.c,
        d: t1.c * t2.b + t1.d * t2.d,
        tx: t1.tx * t2.a + t1.ty * t2.c + t2.tx,
        ty: t1.tx * t2.b + t1.ty * t2.d + t2.ty,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CGAffineTransform {
    pub a: CGFloat,
    pub b: CGFloat,
    pub c: CGFloat,
    pub d: CGFloat,
    pub tx: CGFloat,
    pub ty: CGFloat,
}

impl CGAffineTransform {
    /// `self` then `then`, as `CGAffineTransformConcat(self, then)`.
    pub fn then(self, then: CGAffineTransform) -> CGAffineTransform {
        CGAffineTransform {
            a: self.a * then.a + self.b * then.c,
            b: self.a * then.b + self.b * then.d,
            c: self.c * then.a + self.d * then.c,
            d: self.c * then.b + self.d * then.d,
            tx: self.tx * then.a + self.ty * then.c + then.tx,
            ty: self.tx * then.b + self.ty * then.d + then.ty,
        }
    }

    pub fn apply(self, x: CGFloat, y: CGFloat) -> CGPoint {
        CGPoint {
            x: self.a * x + self.c * y + self.tx,
            y: self.b * x + self.d * y + self.ty,
        }
    }

    /// The inverse, or `None` for a singular transform.
    pub fn inverse(self) -> Option<CGAffineTransform> {
        let det = self.a * self.d - self.b * self.c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(CGAffineTransform {
            a,
            b,
            c,
            d,
            tx: -(self.tx * a + self.ty * c),
            ty: -(self.tx * b + self.ty * d),
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CGAffineTransformComponents {
    pub horizontal_sheer: CGFloat,
    pub rotation: CGFloat,
    pub scale: CGSize,
    pub translation: CGVector,
}

pub static CGAffineTransformIdentity: CGAffineTransform = CGAffineTransform {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    tx: 0.0,
    ty: 0.0,
};

pub unsafe extern "C" fn CGAffineTransformMake(
    a: CGFloat,
    b: CGFloat,
    c: CGFloat,
    d: CGFloat,
    tx: CGFloat,
    ty: CGFloat,
) -> CGAffineTransform {
    CGAffineTransform { a, b, c, d, tx, ty }
}

pub unsafe extern "C" fn CGAffineTransformMakeRotation(angle: CGFloat) -> CGAffineTransform {
    let (s, c) = angle.sin_cos();
    CGAffineTransform {
        a: c,
        b: s,
        c: -s,
        d: c,
        tx: 0.0,
        ty: 0.0,
    }
}

pub unsafe extern "C" fn CGAffineTransformMakeScale(sx: CGFloat, sy: CGFloat) -> CGAffineTransform {
    CGAffineTransform {
        a: sx,
        b: 0.0,
        c: 0.0,
        d: sy,
        tx: 0.0,
        ty: 0.0,
    }
}

pub unsafe extern "C" fn CGAffineTransformMakeTranslation(
    tx: CGFloat,
    ty: CGFloat,
) -> CGAffineTransform {
    CGAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx,
        ty,
    }
}

pub unsafe extern "C" fn CGAffineTransformRotate(
    t: CGAffineTransform,
    angle: CGFloat,
) -> CGAffineTransform {
    concat(unsafe { CGAffineTransformMakeRotation(angle) }, t)
}

pub unsafe extern "C" fn CGAffineTransformScale(
    t: CGAffineTransform,
    sx: CGFloat,
    sy: CGFloat,
) -> CGAffineTransform {
    concat(unsafe { CGAffineTransformMakeScale(sx, sy) }, t)
}

pub unsafe extern "C" fn CGAffineTransformTranslate(
    t: CGAffineTransform,
    tx: CGFloat,
    ty: CGFloat,
) -> CGAffineTransform {
    concat(unsafe { CGAffineTransformMakeTranslation(tx, ty) }, t)
}

pub unsafe extern "C" fn CGAffineTransformConcat(
    t1: CGAffineTransform,
    t2: CGAffineTransform,
) -> CGAffineTransform {
    concat(t1, t2)
}

pub unsafe extern "C" fn CGAffineTransformInvert(t: CGAffineTransform) -> CGAffineTransform {
    let det = t.a * t.d - t.b * t.c;
    if det == 0.0 || !det.is_finite() {
        return t;
    }
    CGAffineTransform {
        a: t.d / det,
        b: -t.b / det,
        c: -t.c / det,
        d: t.a / det,
        tx: (t.c * t.ty - t.d * t.tx) / det,
        ty: (t.b * t.tx - t.a * t.ty) / det,
    }
}

pub unsafe extern "C" fn CGAffineTransformIsIdentity(t: CGAffineTransform) -> bool {
    t == CGAffineTransformIdentity
}

pub unsafe extern "C" fn CGAffineTransformEqualToTransform(
    t1: CGAffineTransform,
    t2: CGAffineTransform,
) -> bool {
    t1 == t2
}

/// `CGRect CGRectApplyAffineTransform(CGRect rect, CGAffineTransform t)`:
/// the rectangle's four corners transformed, and the bounds of the result.
pub unsafe extern "C" fn CGRectApplyAffineTransform(rect: CGRect, t: CGAffineTransform) -> CGRect {
    let (x0, y0) = (rect.origin.x, rect.origin.y);
    let (x1, y1) = (x0 + rect.size.width, y0 + rect.size.height);
    let corners = [
        t.apply(x0, y0),
        t.apply(x1, y0),
        t.apply(x0, y1),
        t.apply(x1, y1),
    ];
    let [left, bottom, right, top] = corners.iter().fold(
        [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
        |[l, b, r, t], p| [l.min(p.x), b.min(p.y), r.max(p.x), t.max(p.y)],
    );
    CGRect {
        origin: CGPoint { x: left, y: bottom },
        size: CGSize {
            width: right - left,
            height: top - bottom,
        },
    }
}

exports::exports! {
    functions {
        "CGAffineTransformMake" => CGAffineTransformMake,
        "CGAffineTransformMakeRotation" => CGAffineTransformMakeRotation,
        "CGAffineTransformMakeScale" => CGAffineTransformMakeScale,
        "CGAffineTransformMakeTranslation" => CGAffineTransformMakeTranslation,
        "CGAffineTransformRotate" => CGAffineTransformRotate,
        "CGAffineTransformScale" => CGAffineTransformScale,
        "CGAffineTransformTranslate" => CGAffineTransformTranslate,
        "CGAffineTransformInvert" => CGAffineTransformInvert,
        "CGAffineTransformConcat" => CGAffineTransformConcat,
        "CGAffineTransformIsIdentity" => CGAffineTransformIsIdentity,
        "CGAffineTransformEqualToTransform" => CGAffineTransformEqualToTransform,
        "CGRectApplyAffineTransform" => CGRectApplyAffineTransform,
    }
}
