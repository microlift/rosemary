use std::ffi::c_void;

use foundation::geometry::{NSPoint, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSAffineTransformStruct`: `x' = m11 x + m21 y + tX`, `y' = m12 x + m22 y + tY`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Matrix {
    pub m11: f64,
    pub m12: f64,
    pub m21: f64,
    pub m22: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        m11: 1.0,
        m12: 0.0,
        m21: 0.0,
        m22: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn apply(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        [
            self.m11 * x + self.m21 * y + self.tx,
            self.m12 * x + self.m22 * y + self.ty,
        ]
    }

    /// `self` first, then `after`.
    pub fn then(&self, after: &Matrix) -> Matrix {
        Matrix {
            m11: self.m11 * after.m11 + self.m12 * after.m21,
            m12: self.m11 * after.m12 + self.m12 * after.m22,
            m21: self.m21 * after.m11 + self.m22 * after.m21,
            m22: self.m21 * after.m12 + self.m22 * after.m22,
            tx: self.tx * after.m11 + self.ty * after.m21 + after.tx,
            ty: self.tx * after.m12 + self.ty * after.m22 + after.ty,
        }
    }

    fn translation(dx: f64, dy: f64) -> Matrix {
        Matrix {
            tx: dx,
            ty: dy,
            ..Matrix::IDENTITY
        }
    }

    fn rotation(degrees: f64) -> Matrix {
        let (sin, cos) = degrees.to_radians().sin_cos();
        Matrix {
            m11: cos,
            m12: sin,
            m21: -sin,
            m22: cos,
            ..Matrix::IDENTITY
        }
    }

    fn scale(sx: f64, sy: f64) -> Matrix {
        Matrix {
            m11: sx,
            m22: sy,
            ..Matrix::IDENTITY
        }
    }

    fn inverse(&self) -> Option<Matrix> {
        let det = self.m11 * self.m22 - self.m12 * self.m21;
        if det.abs() < f64::EPSILON {
            return None;
        }
        Some(Matrix {
            m11: self.m22 / det,
            m12: -self.m12 / det,
            m21: -self.m21 / det,
            m22: self.m11 / det,
            tx: (self.m21 * self.ty - self.m22 * self.tx) / det,
            ty: (self.m12 * self.tx - self.m11 * self.ty) / det,
        })
    }

    /// How much the transform scales lengths, on average over directions.
    pub fn scale_factor(&self) -> f64 {
        (self.m11 * self.m22 - self.m12 * self.m21).abs().sqrt()
    }
}

#[repr(C)]
struct Transform {
    isa: *const c_void,
    matrix: Matrix,
}

pub const INSTANCE_SIZE: usize = size_of::<Transform>();

fn matrix_of<'a>(receiver: Id) -> &'a mut Matrix {
    // SAFETY: an `NSAffineTransform`, which this module laid out.
    unsafe { &mut (*(receiver as *mut Transform)).matrix }
}

/// The transform an `NSAffineTransform` holds, for a caller handed one.
pub(crate) fn matrix(transform: Id) -> Matrix {
    *matrix_of(transform)
}

fn new_transform(matrix: Matrix) -> Id {
    let class = objc::class::lookup(c"NSAffineTransform").expect("declared");
    let object = objc::object::create_instance(class, 0);
    *matrix_of(object) = matrix;
    object
}

unsafe extern "C" fn transform(_class: Id, _sel: Sel) -> Id {
    let object = new_transform(Matrix::IDENTITY);
    // SAFETY: a fresh convenience-constructor result.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    *matrix_of(receiver) = Matrix::IDENTITY;
    receiver
}

unsafe extern "C" fn init_with_transform(receiver: Id, _sel: Sel, other: Id) -> Id {
    *matrix_of(receiver) = match other.is_null() {
        true => Matrix::IDENTITY,
        false => matrix(other),
    };
    receiver
}

unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    new_transform(matrix(receiver))
}

// The receiver's own operations happen *before* what it already holds: Apple
// documents `translateXBy:yBy:` and friends as applied to the coordinate system
// the transform maps from.

unsafe extern "C" fn translate(receiver: Id, _sel: Sel, dx: f64, dy: f64) {
    let m = matrix_of(receiver);
    *m = Matrix::translation(dx, dy).then(m);
}

unsafe extern "C" fn rotate_by_degrees(receiver: Id, _sel: Sel, degrees: f64) {
    let m = matrix_of(receiver);
    *m = Matrix::rotation(degrees).then(m);
}

unsafe extern "C" fn rotate_by_radians(receiver: Id, _sel: Sel, radians: f64) {
    let m = matrix_of(receiver);
    *m = Matrix::rotation(radians.to_degrees()).then(m);
}

unsafe extern "C" fn scale_by(receiver: Id, _sel: Sel, scale: f64) {
    let m = matrix_of(receiver);
    *m = Matrix::scale(scale, scale).then(m);
}

unsafe extern "C" fn scale_x_by(receiver: Id, _sel: Sel, sx: f64, sy: f64) {
    let m = matrix_of(receiver);
    *m = Matrix::scale(sx, sy).then(m);
}

unsafe extern "C" fn append_transform(receiver: Id, _sel: Sel, other: Id) {
    if other.is_null() {
        return;
    }
    let m = matrix_of(receiver);
    *m = m.then(&matrix(other));
}

unsafe extern "C" fn prepend_transform(receiver: Id, _sel: Sel, other: Id) {
    if other.is_null() {
        return;
    }
    let m = matrix_of(receiver);
    *m = matrix(other).then(m);
}

unsafe extern "C" fn invert(receiver: Id, _sel: Sel) {
    let m = matrix_of(receiver);
    if let Some(inverse) = m.inverse() {
        *m = inverse;
    }
}

unsafe extern "C" fn transform_point(receiver: Id, _sel: Sel, point: NSPoint) -> NSPoint {
    let [x, y] = matrix(receiver).apply([point.x, point.y]);
    NSPoint { x, y }
}

unsafe extern "C" fn transform_size(receiver: Id, _sel: Sel, size: NSSize) -> NSSize {
    let m = matrix(receiver);
    NSSize {
        width: m.m11 * size.width + m.m21 * size.height,
        height: m.m12 * size.width + m.m22 * size.height,
    }
}

/// `-concat`: draw through this transform as well as the current one.
unsafe extern "C" fn concat(receiver: Id, _sel: Sel) {
    let m = matrix(receiver);
    crate::bezier::update_ctm(|ctm| m.then(ctm));
    crate::cgcontext::focus_update_ctm(|ctm| cg(m).then(ctm));
}

/// `-set`: draw through this transform instead of the current one.
unsafe extern "C" fn set(receiver: Id, _sel: Sel) {
    let m = matrix(receiver);
    crate::bezier::update_ctm(|_| m);
    crate::cgcontext::focus_update_ctm(|_| cg(m));
}

fn cg(m: Matrix) -> crate::cgcontext::CGAffineTransform {
    crate::cgcontext::CGAffineTransform {
        a: m.m11,
        b: m.m12,
        c: m.m21,
        d: m.m22,
        tx: m.tx,
        ty: m.ty,
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAffineTransform").expect("declared before its methods");
    class_methods!(class, {
        "transform" => transform, c"@16@0:8";
    });
    methods!(class, {
        "init" => init, c"@16@0:8";
        "initWithTransform:" => init_with_transform, c"@24@0:8@16";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "translateXBy:yBy:" => translate, c"v32@0:8d16d24";
        "rotateByDegrees:" => rotate_by_degrees, c"v24@0:8d16";
        "rotateByRadians:" => rotate_by_radians, c"v24@0:8d16";
        "scaleBy:" => scale_by, c"v24@0:8d16";
        "scaleXBy:yBy:" => scale_x_by, c"v32@0:8d16d24";
        "appendTransform:" => append_transform, c"v24@0:8@16";
        "prependTransform:" => prepend_transform, c"v24@0:8@16";
        "invert" => invert, c"v16@0:8";
        "transformPoint:" => transform_point, c"{CGPoint=dd}32@0:8{CGPoint=dd}16";
        "transformSize:" => transform_size, c"{CGSize=dd}32@0:8{CGSize=dd}16";
        "concat" => concat, c"v16@0:8";
        "set" => set, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 2], b: [f64; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn translate_then_rotate_rotates_about_the_translated_origin() {
        // [t translateXBy:10 yBy:0]; [t rotateByDegrees:90];
        let m = Matrix::translation(10.0, 0.0);
        let m = Matrix::rotation(90.0).then(&m);
        assert!(close(m.apply([1.0, 0.0]), [10.0, 1.0]));
    }

    #[test]
    fn inverse_undoes_the_transform() {
        let m = Matrix::rotation(30.0).then(&Matrix::translation(3.0, -4.0));
        let back = m.then(&m.inverse().unwrap());
        assert!(close(back.apply([5.0, 7.0]), [5.0, 7.0]));
    }
}
