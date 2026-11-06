//! Built-in SGI GLU algorithms, with every GL call routed through SDL.
#![allow(non_snake_case)]

use std::ffi::{CStr, c_void};

// Apple OpenGL/glu.h defines GLboolean as an unsigned byte, not a Rust bool.
macro_rules! entry {
    ($name:ident, $implementation:ident, ($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty) => {
        unsafe extern "C" { fn $implementation($($arg: $ty),*) -> $ret; }
        unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
            unsafe { $implementation($($arg),*) }
        }
    };
}

entry!(
    gluBuild1DMipmaps,
    rosemary_gluBuild1DMipmaps,
    (
        target: u32,
        internal: i32,
        width: i32,
        format: u32,
        kind: u32,
        data: *const c_void,
    ) -> i32
);
entry!(
    gluBuild2DMipmaps,
    rosemary_gluBuild2DMipmaps,
    (
        target: u32,
        internal: i32,
        width: i32,
        height: i32,
        format: u32,
        kind: u32,
        data: *const c_void,
    ) -> i32
);
entry!(
    gluCylinder,
    rosemary_gluCylinder,
    (
        quad: *mut c_void,
        base: f64,
        top: f64,
        height: f64,
        slices: i32,
        stacks: i32,
    ) -> ()
);
entry!(gluDeleteQuadric, rosemary_gluDeleteQuadric, (quad: *mut c_void) -> ());
entry!(
    gluDisk,
    rosemary_gluDisk,
    (
        quad: *mut c_void,
        inner: f64,
        outer: f64,
        slices: i32,
        loops: i32,
    ) -> ()
);
entry!(
    gluLookAt,
    rosemary_gluLookAt,
    (
        eye_x: f64,
        eye_y: f64,
        eye_z: f64,
        center_x: f64,
        center_y: f64,
        center_z: f64,
        up_x: f64,
        up_y: f64,
        up_z: f64,
    ) -> ()
);
entry!(gluNewQuadric, rosemary_gluNewQuadric, () -> *mut c_void);
entry!(
    gluPartialDisk,
    rosemary_gluPartialDisk,
    (
        quad: *mut c_void,
        inner: f64,
        outer: f64,
        slices: i32,
        loops: i32,
        start: f64,
        sweep: f64,
    ) -> ()
);
entry!(
    gluPerspective,
    rosemary_gluPerspective,
    (
        fovy: f64,
        aspect: f64,
        near: f64,
        far: f64,
    ) -> ()
);
entry!(
    gluPickMatrix,
    rosemary_gluPickMatrix,
    (
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        viewport: *const i32,
    ) -> ()
);
entry!(
    gluProject,
    rosemary_gluProject,
    (
        x: f64,
        y: f64,
        z: f64,
        model: *const f64,
        projection: *const f64,
        viewport: *const i32,
        out_x: *mut f64,
        out_y: *mut f64,
        out_z: *mut f64,
    ) -> i32
);
entry!(gluQuadricDrawStyle, rosemary_gluQuadricDrawStyle, (quad: *mut c_void, style: u32) -> ());
entry!(gluQuadricNormals, rosemary_gluQuadricNormals, (quad: *mut c_void, normals: u32) -> ());
entry!(
    gluQuadricOrientation,
    rosemary_gluQuadricOrientation,
    (
        quad: *mut c_void,
        orientation: u32,
    ) -> ()
);
entry!(gluQuadricTexture, rosemary_gluQuadricTexture, (quad: *mut c_void, texture: u8) -> ());
entry!(
    gluScaleImage,
    rosemary_gluScaleImage,
    (
        format: u32,
        width_in: i32,
        height_in: i32,
        type_in: u32,
        data_in: *const c_void,
        width_out: i32,
        height_out: i32,
        type_out: u32,
        data_out: *mut c_void,
    ) -> i32
);
entry!(
    gluSphere,
    rosemary_gluSphere,
    (
        quad: *mut c_void,
        radius: f64,
        slices: i32,
        stacks: i32,
    ) -> ()
);
entry!(
    gluUnProject,
    rosemary_gluUnProject,
    (
        x: f64,
        y: f64,
        z: f64,
        model: *const f64,
        projection: *const f64,
        viewport: *const i32,
        out_x: *mut f64,
        out_y: *mut f64,
        out_z: *mut f64,
    ) -> i32
);

unsafe extern "C" fn gluCheckExtension(name: *const u8, extensions: *const u8) -> u8 {
    if name.is_null() || extensions.is_null() {
        return 0;
    }
    let name = unsafe { CStr::from_ptr(name.cast()) }.to_bytes();
    let extensions = unsafe { CStr::from_ptr(extensions.cast()) }.to_bytes();
    u8::from(!name.is_empty() && extensions.split(|&c| c == b' ').any(|word| word == name))
}

exports::exports! {
    functions {
        "gluBuild1DMipmaps" => gluBuild1DMipmaps,
        "gluBuild2DMipmaps" => gluBuild2DMipmaps,
        "gluCheckExtension" => gluCheckExtension,
        "gluCylinder" => gluCylinder,
        "gluDeleteQuadric" => gluDeleteQuadric,
        "gluDisk" => gluDisk,
        "gluLookAt" => gluLookAt,
        "gluNewQuadric" => gluNewQuadric,
        "gluPartialDisk" => gluPartialDisk,
        "gluPerspective" => gluPerspective,
        "gluPickMatrix" => gluPickMatrix,
        "gluProject" => gluProject,
        "gluQuadricDrawStyle" => gluQuadricDrawStyle,
        "gluQuadricNormals" => gluQuadricNormals,
        "gluQuadricOrientation" => gluQuadricOrientation,
        "gluQuadricTexture" => gluQuadricTexture,
        "gluScaleImage" => gluScaleImage,
        "gluSphere" => gluSphere,
        "gluUnProject" => gluUnProject,
    }
}

// These names are private C link symbols; they never resolve through host libGL.
macro_rules! gl {
    ($name:ident, $symbol:literal, ($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty, $fallback:expr) => {
        #[unsafe(no_mangle)]
        unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
            #[cfg(not(test))]
            let address = {
                static ENTRY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
                ENTRY.get().copied().or_else(|| {
                    if !super::open() {
                        return None;
                    }
                    let entry = sdl3::video::gl_proc_address($symbol)? as usize;
                    // A lookup before a context exists must not cache failure.
                    let _ = ENTRY.set(entry);
                    Some(entry)
                })
            };
            #[cfg(test)]
            let address = tests::resolve($symbol);
            if let Some(address) = address {
                let f: unsafe extern "C" fn($($ty),*) -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f($($arg),*) }
            } else {
                $fallback
            }
        }
    };
}

gl!(rosemary_glu_glBegin, c"glBegin", (mode: u32) -> (), ());
gl!(rosemary_glu_glEnd, c"glEnd", () -> (), ());
gl!(rosemary_glu_glNormal3f, c"glNormal3f", (x: f32, y: f32, z: f32) -> (), ());
gl!(rosemary_glu_glTexCoord2f, c"glTexCoord2f", (s: f32, t: f32) -> (), ());
gl!(rosemary_glu_glVertex3f, c"glVertex3f", (x: f32, y: f32, z: f32) -> (), ());
gl!(rosemary_glu_glMultMatrixd, c"glMultMatrixd", (m: *const f64) -> (), ());
gl!(rosemary_glu_glMultMatrixf, c"glMultMatrixf", (m: *const f32) -> (), ());
gl!(rosemary_glu_glOrtho, c"glOrtho", (left: f64, right: f64, bottom: f64, top: f64, near: f64, far: f64) -> (), ());
gl!(rosemary_glu_glScalef, c"glScalef", (x: f32, y: f32, z: f32) -> (), ());
gl!(rosemary_glu_glTranslatef, c"glTranslatef", (x: f32, y: f32, z: f32) -> (), ());
gl!(rosemary_glu_glTranslated, c"glTranslated", (x: f64, y: f64, z: f64) -> (), ());
gl!(rosemary_glu_glGetIntegerv, c"glGetIntegerv", (name: u32, value: *mut i32) -> (), if !value.is_null() { unsafe { *value = 0 }; });
gl!(rosemary_glu_glGetString, c"glGetString", (name: u32) -> *const u8, c"".as_ptr().cast());
gl!(rosemary_glu_glGetTexLevelParameteriv, c"glGetTexLevelParameteriv", (target: u32, level: i32, name: u32, value: *mut i32) -> (), if !value.is_null() { unsafe { *value = 0 }; });
gl!(rosemary_glu_glPixelStorei, c"glPixelStorei", (name: u32, value: i32) -> (), ());
gl!(rosemary_glu_glTexImage1D, c"glTexImage1D", (target: u32, level: i32, internal: i32, width: i32, border: i32, format: u32, kind: u32, pixels: *const c_void) -> (), ());
gl!(rosemary_glu_glTexImage2D, c"glTexImage2D", (target: u32, level: i32, internal: i32, width: i32, height: i32, border: i32, format: u32, kind: u32, pixels: *const c_void) -> (), ());
gl!(rosemary_glu_glTexImage3D, c"glTexImage3D", (target: u32, level: i32, internal: i32, width: i32, height: i32, depth: i32, border: i32, format: u32, kind: u32, pixels: *const c_void) -> (), ());

#[cfg(test)]
mod tests;
