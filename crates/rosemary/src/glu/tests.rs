use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

const RGB: u32 = 0x1907;
const RGBA: u32 = 0x1908;
const UBYTE: u32 = 0x1401;
const USHORT: u32 = 0x1403;
const FLOAT: u32 = 0x1406;
const SMOOTH: u32 = 100000;
const FLAT: u32 = 100001;
const NONE: u32 = 100002;
const POINT: u32 = 100010;
const LINE: u32 = 100011;
const FILL: u32 = 100012;
const SILHOUETTE: u32 = 100013;
const INSIDE: u32 = 100021;
const INVALID_ENUM: i32 = 100900;
const INVALID_VALUE: i32 = 100901;

fn identity() -> [f64; 16] {
    let mut m = [0.0; 16];
    for i in [0, 5, 10, 15] {
        m[i] = 1.0;
    }
    m
}

#[derive(Debug, Clone)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

#[derive(Debug)]
struct Image {
    level: i32,
    width: i32,
    height: i32,
    pixels: Vec<f64>,
}

struct Gl {
    matrix: [f64; 16],
    store: BTreeMap<u32, i32>,
    vertices: Vec<Vertex>,
    modes: Vec<u32>,
    normal: [f32; 3],
    uv: [f32; 2],
    images: Vec<Image>,
    ends: usize,
    normals: usize,
    textures: usize,
    proxy: bool,
    proxy_width: i32,
}

impl Default for Gl {
    fn default() -> Self {
        Self {
            matrix: identity(),
            store: (0x0d00..=0x0d05)
                .chain(0x0cf0..=0x0cf5)
                .map(|name| {
                    (
                        name,
                        if matches!(name, 0x0d05 | 0x0cf5) {
                            4
                        } else {
                            0
                        },
                    )
                })
                .chain([(0x0d33, 64)])
                .collect(),
            vertices: Vec::new(),
            modes: Vec::new(),
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            images: Vec::new(),
            ends: 0,
            normals: 0,
            textures: 0,
            proxy: false,
            proxy_width: 0,
        }
    }
}

thread_local! {
    static GL: RefCell<Gl> = RefCell::new(Gl::default());
}

fn reset() {
    GL.with(|gl| *gl.borrow_mut() = Gl::default());
}

pub(super) fn resolve(name: &CStr) -> Option<usize> {
    Some(match name.to_bytes() {
        b"glBegin" => begin as *const () as usize,
        b"glEnd" => end as *const () as usize,
        b"glNormal3f" => normal as *const () as usize,
        b"glTexCoord2f" => tex_coord as *const () as usize,
        b"glVertex3f" => vertex as *const () as usize,
        b"glMultMatrixd" => mult_d as *const () as usize,
        b"glMultMatrixf" => mult_f as *const () as usize,
        b"glTranslated" => translate_d as *const () as usize,
        b"glTranslatef" => translate_f as *const () as usize,
        b"glScalef" => scale as *const () as usize,
        b"glGetIntegerv" => get_integer as *const () as usize,
        b"glGetString" => get_string as *const () as usize,
        b"glGetTexLevelParameteriv" => get_texture as *const () as usize,
        b"glPixelStorei" => pixel_store as *const () as usize,
        b"glTexImage1D" => image_1d as *const () as usize,
        b"glTexImage2D" => image_2d as *const () as usize,
        _ => return None,
    })
}

unsafe extern "C" fn begin(mode: u32) {
    GL.with(|gl| gl.borrow_mut().modes.push(mode));
}
unsafe extern "C" fn end() {
    GL.with(|gl| gl.borrow_mut().ends += 1);
}
unsafe extern "C" fn normal(x: f32, y: f32, z: f32) {
    GL.with(|gl| {
        let mut gl = gl.borrow_mut();
        gl.normal = [x, y, z];
        gl.normals += 1;
    });
}
unsafe extern "C" fn tex_coord(s: f32, t: f32) {
    GL.with(|gl| {
        let mut gl = gl.borrow_mut();
        gl.uv = [s, t];
        gl.textures += 1;
    });
}
unsafe extern "C" fn vertex(x: f32, y: f32, z: f32) {
    GL.with(|gl| {
        let mut gl = gl.borrow_mut();
        let v = Vertex {
            position: [x, y, z],
            normal: gl.normal,
            uv: gl.uv,
        };
        gl.vertices.push(v);
    });
}

fn multiply(m: [f64; 16]) {
    GL.with(|gl| {
        let mut gl = gl.borrow_mut();
        let a = gl.matrix;
        for col in 0..4 {
            for row in 0..4 {
                gl.matrix[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * m[col * 4 + k]).sum();
            }
        }
    });
}
unsafe extern "C" fn mult_d(m: *const f64) {
    multiply(unsafe { *m.cast::<[f64; 16]>() });
}
unsafe extern "C" fn mult_f(m: *const f32) {
    multiply(unsafe { *m.cast::<[f32; 16]>() }.map(f64::from));
}
unsafe extern "C" fn translate_d(x: f64, y: f64, z: f64) {
    let mut m = identity();
    m[12..15].copy_from_slice(&[x, y, z]);
    multiply(m);
}
unsafe extern "C" fn translate_f(x: f32, y: f32, z: f32) {
    unsafe { translate_d(x.into(), y.into(), z.into()) };
}
unsafe extern "C" fn scale(x: f32, y: f32, z: f32) {
    let mut m = identity();
    m[0] = x.into();
    m[5] = y.into();
    m[10] = z.into();
    multiply(m);
}
unsafe extern "C" fn get_integer(name: u32, out: *mut i32) {
    unsafe { *out = GL.with(|gl| *gl.borrow().store.get(&name).unwrap_or(&0)) };
}
unsafe extern "C" fn get_string(_name: u32) -> *const u8 {
    GL.with(|gl| {
        if gl.borrow().proxy {
            c"1.2".as_ptr().cast()
        } else {
            c"1.0".as_ptr().cast()
        }
    })
}
unsafe extern "C" fn get_texture(_target: u32, _level: i32, _name: u32, out: *mut i32) {
    unsafe { *out = GL.with(|gl| gl.borrow().proxy_width) };
}
unsafe extern "C" fn pixel_store(name: u32, value: i32) {
    GL.with(|gl| {
        gl.borrow_mut().store.insert(name, value);
    });
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn image_1d(
    target: u32,
    level: i32,
    internal: i32,
    width: i32,
    border: i32,
    format: u32,
    kind: u32,
    data: *const c_void,
) {
    unsafe {
        image_2d(
            target, level, internal, width, 1, border, format, kind, data,
        )
    };
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn image_2d(
    target: u32,
    level: i32,
    _internal: i32,
    width: i32,
    height: i32,
    _border: i32,
    format: u32,
    kind: u32,
    data: *const c_void,
) {
    GL.with(|gl| {
        let mut gl = gl.borrow_mut();
        if matches!(target, 0x8063 | 0x8064) {
            gl.proxy_width = if width <= 2 && height <= 2 { width } else { 0 };
            return;
        }
        assert!(!data.is_null());
        let components = match format {
            RGB => 3,
            RGBA => 4,
            _ => 1,
        };
        let bytes = match kind {
            UBYTE => 1,
            USHORT => 2,
            FLOAT => 4,
            _ => panic!("unexpected upload type {kind:x}"),
        };
        let row_length = *gl.store.get(&0x0cf2).unwrap_or(&0);
        let row_length = if row_length == 0 { width } else { row_length } as usize;
        let alignment = gl.store[&0x0cf5] as usize;
        let stride = (row_length * components * bytes).div_ceil(alignment) * alignment;
        let skip_rows = *gl.store.get(&0x0cf3).unwrap_or(&0) as usize;
        let skip_pixels = *gl.store.get(&0x0cf4).unwrap_or(&0) as usize;
        let mut pixels = Vec::new();
        for y in 0..height as usize {
            for x in 0..width as usize * components {
                let offset = (y + skip_rows) * stride + (x + skip_pixels * components) * bytes;
                let p = unsafe { data.cast::<u8>().add(offset) };
                pixels.push(unsafe {
                    match kind {
                        UBYTE => *p as f64,
                        USHORT => p.cast::<u16>().read_unaligned() as f64,
                        FLOAT => p.cast::<f32>().read_unaligned() as f64,
                        _ => unreachable!(),
                    }
                });
            }
        }
        gl.images.push(Image {
            level,
            width,
            height,
            pixels,
        });
    });
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
}

struct Quad(*mut c_void);
impl Quad {
    fn new() -> Self {
        let p = unsafe { gluNewQuadric() };
        assert!(!p.is_null());
        Self(p)
    }
}
impl Drop for Quad {
    fn drop(&mut self) {
        unsafe { gluDeleteQuadric(self.0) };
    }
}

#[test]
fn extension_queries_match_complete_space_delimited_tokens() {
    for (name, list, expected) in [
        (c"GL_EXT_a", c" GL_EXT_ab GL_EXT_a  GL_EXT_b ", 1),
        (c"GL_EXT_a", c"GL_EXT_ab", 0),
        (c"EXT_a", c"GL_EXT_a", 0),
        (c"", c" ", 0),
        (c"GL_EXT_a GL_EXT_b", c"GL_EXT_a GL_EXT_b", 0),
    ] {
        assert_eq!(
            unsafe { gluCheckExtension(name.as_ptr().cast(), list.as_ptr().cast()) },
            expected
        );
    }
    assert_eq!(
        unsafe { gluCheckExtension(c"x".as_ptr().cast(), std::ptr::null()) },
        0
    );
}

#[test]
fn perspective_look_at_and_pick_multiply_the_current_matrix() {
    reset();
    unsafe { gluPerspective(90.0, 2.0, 1.0, 11.0) };
    let projection = GL.with(|gl| gl.borrow().matrix);
    close(projection[0], 0.5);
    close(projection[5], 1.0);
    close(projection[10], -1.2);
    close(projection[14], -2.2);
    assert_eq!(projection[11], -1.0);
    assert_eq!(projection[15], 0.0);
    unsafe { gluPerspective(60.0, 0.0, 1.0, 10.0) };
    assert_eq!(GL.with(|gl| gl.borrow().matrix), projection);
    reset();
    unsafe { gluLookAt(0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0) };
    let mut expected = identity();
    expected[14] = -5.0;
    assert_eq!(GL.with(|gl| gl.borrow().matrix), expected);
    reset();
    let viewport = [10, 20, 800, 600];
    unsafe { gluPickMatrix(410.0, 320.0, 100.0, 200.0, viewport.as_ptr()) };
    expected = identity();
    expected[0] = 8.0;
    expected[5] = 3.0;
    assert_eq!(GL.with(|gl| gl.borrow().matrix), expected);
    unsafe { gluPickMatrix(0.0, 0.0, -1.0, 2.0, viewport.as_ptr()) };
    assert_eq!(GL.with(|gl| gl.borrow().matrix), expected);
}

#[test]
fn project_and_unproject_round_trip_nonidentity_matrices() {
    reset();
    unsafe { gluPerspective(60.0, 4.0 / 3.0, 0.1, 100.0) };
    let projection = GL.with(|gl| gl.borrow().matrix);
    reset();
    unsafe { gluLookAt(3.0, 4.0, 5.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0) };
    let model = GL.with(|gl| gl.borrow().matrix);
    let viewport = [31, 17, 800, 600];
    let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
    assert_eq!(
        unsafe {
            gluProject(
                0.2,
                -0.4,
                0.5,
                model.as_ptr(),
                projection.as_ptr(),
                viewport.as_ptr(),
                &mut x,
                &mut y,
                &mut z,
            )
        },
        1
    );
    assert_eq!(
        unsafe {
            gluUnProject(
                x,
                y,
                z,
                model.as_ptr(),
                projection.as_ptr(),
                viewport.as_ptr(),
                &mut x,
                &mut y,
                &mut z,
            )
        },
        1
    );
    close(x, 0.2);
    close(y, -0.4);
    close(z, 0.5);
    let zero = [0.0; 16];
    assert_eq!(
        unsafe {
            gluUnProject(
                0.0,
                0.0,
                0.0,
                zero.as_ptr(),
                projection.as_ptr(),
                viewport.as_ptr(),
                &mut x,
                &mut y,
                &mut z,
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            gluProject(
                0.0,
                0.0,
                0.0,
                model.as_ptr(),
                zero.as_ptr(),
                viewport.as_ptr(),
                &mut x,
                &mut y,
                &mut z,
            )
        },
        0
    );
}

#[test]
fn quadrics_default_to_smooth_outside_fill_without_texture() {
    reset();
    let q = Quad::new();
    unsafe { gluCylinder(q.0, 1.0, 1.0, 2.0, 4, 2) };
    GL.with(|gl| {
        let gl = gl.borrow();
        assert_eq!(gl.modes, [8, 8]);
        assert_eq!(gl.ends, 2);
        assert_eq!(gl.vertices.len(), 20);
        assert!(gl.normals > 0);
        assert_eq!(gl.textures, 0);
        assert_eq!(gl.vertices[0].position, [0.0, 1.0, 0.0]);
        assert_eq!(gl.vertices[1].position, [0.0, 1.0, 1.0]);
        assert_eq!(gl.vertices[0].normal, [0.0, 1.0, 0.0]);
    });
}

#[test]
fn quadric_orientation_reverses_winding_and_normals() {
    reset();
    let q = Quad::new();
    unsafe {
        gluQuadricOrientation(q.0, INSIDE);
        gluQuadricTexture(q.0, 1);
        gluCylinder(q.0, 1.0, 1.0, 2.0, 4, 1);
    }
    GL.with(|gl| {
        let gl = gl.borrow();
        assert_eq!(gl.vertices[0].position, [0.0, 1.0, 2.0]);
        assert_eq!(gl.vertices[1].position, [0.0, 1.0, 0.0]);
        assert_eq!(gl.vertices[0].normal, [-0.0, -1.0, 0.0]);
        assert_eq!(gl.vertices[0].uv, [1.0, 1.0]);
        assert_eq!(gl.vertices[1].uv, [1.0, 0.0]);
    });
}

#[test]
fn disk_fill_has_center_fan_and_silhouette_omits_interior_edges() {
    reset();
    let q = Quad::new();
    unsafe { gluDisk(q.0, 0.0, 2.0, 4, 2) };
    GL.with(|gl| {
        let gl = gl.borrow();
        assert_eq!(gl.modes, [6, 8]);
        assert_eq!(gl.vertices[0].position, [0.0; 3]);
        assert_eq!(gl.vertices[0].normal, [0.0, 0.0, 1.0]);
    });
    reset();
    unsafe {
        gluQuadricDrawStyle(q.0, SILHOUETTE);
        gluPartialDisk(q.0, 1.0, 2.0, 4, 2, 90.0, -90.0);
    }
    GL.with(|gl| {
        let gl = gl.borrow();
        assert_eq!(gl.modes, [3, 3, 3, 3]);
        assert_eq!(gl.vertices[0].position, [0.0, 2.0, 0.0]);
    });
}

#[test]
fn sphere_supports_all_styles_normals_and_texture_seams() {
    let q = Quad::new();
    for style in [POINT, LINE, FILL, SILHOUETTE] {
        for normals in [SMOOTH, FLAT, NONE] {
            reset();
            unsafe {
                gluQuadricDrawStyle(q.0, style);
                gluQuadricNormals(q.0, normals);
                gluQuadricTexture(q.0, 1);
                gluSphere(q.0, 2.0, 8, 4);
            }
            GL.with(|gl| {
                let gl = gl.borrow();
                assert!(!gl.vertices.is_empty());
                assert_eq!(gl.modes.len(), gl.ends);
                assert_eq!(gl.normals == 0, normals == NONE);
                assert_eq!(gl.textures, gl.vertices.len());
                for v in &gl.vertices {
                    if normals != NONE {
                        close(v.normal.iter().map(|&x| (x as f64).powi(2)).sum(), 1.0);
                    }
                    close(v.position.iter().map(|&x| (x as f64).powi(2)).sum(), 4.0);
                    assert!((0.0..=1.0).contains(&v.uv[0]));
                    assert!((0.0..=1.0).contains(&v.uv[1]));
                }
            });
        }
    }
}

#[test]
fn invalid_quadric_parameters_do_not_emit_geometry_or_change_state() {
    reset();
    let q = Quad::new();
    unsafe {
        gluCylinder(q.0, -1.0, 1.0, 2.0, 8, 4);
        gluDisk(q.0, 3.0, 2.0, 8, 4);
        gluSphere(q.0, 1.0, 1, 4);
        gluQuadricDrawStyle(q.0, 0);
        gluQuadricNormals(q.0, 0);
        gluQuadricOrientation(q.0, 0);
    }
    assert!(GL.with(|gl| gl.borrow().vertices.is_empty()));
    unsafe { gluDisk(q.0, 0.0, 1.0, 4, 1) };
    assert_eq!(GL.with(|gl| gl.borrow().modes.clone()), [6]);
}

#[test]
fn scale_image_box_filter_averages_and_converts_components() {
    reset();
    let input: [u8; 8] = [0, 64, 0, 0, 128, 255, 0, 0];
    let mut output = [0_u16; 1];
    assert_eq!(
        unsafe {
            gluScaleImage(
                0x1909,
                2,
                2,
                UBYTE,
                input.as_ptr().cast(),
                1,
                1,
                USHORT,
                output.as_mut_ptr().cast(),
            )
        },
        0
    );
    assert_eq!(
        output[0],
        ((64_u32 * 257 + 128 * 257 + 65535 + 2) / 4) as u16
    );
    let input = [1.0_f32, 0.5, 0.0, 1.0];
    let mut output = [0_u8; 4];
    assert_eq!(
        unsafe {
            gluScaleImage(
                RGBA,
                1,
                1,
                FLOAT,
                input.as_ptr().cast(),
                1,
                1,
                UBYTE,
                output.as_mut_ptr().cast(),
            )
        },
        0
    );
    assert_eq!(output, [255, 127, 0, 255]);
}

#[test]
fn scale_image_respects_pack_unpack_stride_skips_and_byte_swapping() {
    reset();
    GL.with(|gl| {
        let mut gl = gl.borrow_mut();
        gl.store.extend([
            (0x0cf2, 3),
            (0x0cf3, 1),
            (0x0cf4, 1),
            (0x0cf0, 1),
            (0x0d02, 3),
            (0x0d03, 1),
            (0x0d04, 1),
        ]);
    });
    let mut input = [0_u16; 12];
    input[5] = 0x1234_u16.swap_bytes();
    let mut output = [0xeeee_u16; 12];
    let before = GL.with(|gl| gl.borrow().store.clone());
    assert_eq!(
        unsafe {
            gluScaleImage(
                0x1909,
                1,
                1,
                USHORT,
                input.as_ptr().cast(),
                1,
                1,
                USHORT,
                output.as_mut_ptr().cast(),
            )
        },
        0
    );
    assert_eq!(output[5], 0x1234);
    assert_eq!(output.iter().filter(|&&v| v == 0xeeee).count(), 11);
    assert_eq!(GL.with(|gl| gl.borrow().store.clone()), before);
}

#[test]
fn scale_image_supports_packed_pixels_and_bitmap_bit_order() {
    reset();
    let input = [0xf800_u16, 0x07e0, 0x001f, 0xffff];
    let mut output = [0_u8; 8];
    assert_eq!(
        unsafe {
            gluScaleImage(
                RGB,
                2,
                1,
                0x8363,
                input.as_ptr().cast(),
                2,
                1,
                UBYTE,
                output.as_mut_ptr().cast(),
            )
        },
        0
    );
    assert_eq!(&output[..6], &[255, 0, 0, 0, 255, 0]);
    GL.with(|gl| {
        gl.borrow_mut().store.insert(0x0cf1, 1);
    });
    let input = [1_u8, 0, 0, 0];
    let mut output = [0_u8; 4];
    assert_eq!(
        unsafe {
            gluScaleImage(
                0x1900,
                2,
                1,
                0x1a00,
                input.as_ptr().cast(),
                2,
                1,
                UBYTE,
                output.as_mut_ptr().cast(),
            )
        },
        0
    );
    assert_eq!(&output[..2], &[1, 0]);
}

#[test]
fn scale_image_validates_dimensions_formats_and_packed_types() {
    reset();
    let p = std::ptr::null();
    let q = std::ptr::null_mut();
    unsafe {
        assert_eq!(gluScaleImage(RGB, 0, 1, UBYTE, p, 1, 1, UBYTE, q), 0);
        assert_eq!(
            gluScaleImage(RGB, -1, 1, UBYTE, p, 1, 1, UBYTE, q),
            INVALID_VALUE
        );
        assert_eq!(
            gluScaleImage(0, 1, 1, UBYTE, p, 1, 1, UBYTE, q),
            INVALID_ENUM
        );
        assert_eq!(gluScaleImage(RGBA, 1, 1, 0x8363, p, 1, 1, UBYTE, q), 100904);
        assert_eq!(
            gluBuild1DMipmaps(0x0de0, 3, 0, RGB, UBYTE, p),
            INVALID_VALUE
        );
        assert_eq!(
            gluBuild2DMipmaps(0x0de1, 3, 1, -1, RGB, UBYTE, p),
            INVALID_VALUE
        );
    }
}

#[test]
fn mipmaps_round_to_nearest_power_of_two_and_restore_unpack_state() {
    reset();
    GL.with(|gl| {
        gl.borrow_mut().store.insert(0x0cf5, 1);
    });
    let before = GL.with(|gl| gl.borrow().store.clone());
    let input = [80_u8; 5 * 3 * 3];
    assert_eq!(
        unsafe { gluBuild2DMipmaps(0x0de1, 3, 5, 3, RGB, UBYTE, input.as_ptr().cast()) },
        0
    );
    GL.with(|gl| {
        let gl = gl.borrow();
        assert_eq!(
            gl.images
                .iter()
                .map(|i| (i.level, i.width, i.height))
                .collect::<Vec<_>>(),
            [(0, 4, 4), (1, 2, 2), (2, 1, 1)]
        );
        assert!(gl.images.iter().flat_map(|i| &i.pixels).all(|&x| x == 80.0));
        assert_eq!(gl.store, before);
    });
}

#[test]
fn one_dimensional_mipmaps_and_proxy_limits_reach_one_texel() {
    reset();
    let input = [64_u8; 8];
    assert_eq!(
        unsafe { gluBuild1DMipmaps(0x0de0, 1, 8, 0x1909, UBYTE, input.as_ptr().cast()) },
        0
    );
    GL.with(|gl| {
        let gl = gl.borrow();
        assert_eq!(
            gl.images.iter().map(|i| i.width).collect::<Vec<_>>(),
            [8, 4, 2, 1]
        );
        // The SGI 1D path uploads normalized unsigned-short pixels.
        assert!(
            gl.images
                .iter()
                .flat_map(|i| &i.pixels)
                .all(|&x| x == 64.0 * 257.0)
        );
    });
    reset();
    GL.with(|gl| gl.borrow_mut().proxy = true);
    let input = [32_u8; 16 * 16 * 4];
    assert_eq!(
        unsafe { gluBuild2DMipmaps(0x0de1, 4, 16, 16, RGBA, UBYTE, input.as_ptr().cast()) },
        0
    );
    GL.with(|gl| {
        assert_eq!(
            gl.borrow()
                .images
                .iter()
                .map(|i| i.width)
                .collect::<Vec<_>>(),
            [4, 2, 1]
        );
    });
}

#[test]
fn glu_exports_have_callable_abis_and_resolve_without_a_display() {
    use crate::abi::table::{Lookup, lookup};
    assert_eq!(names().len(), 19);
    for name in names() {
        // Nine doubles do not fit the marshaller; a shim reads the ninth.
        if name == "gluLookAt" {
            assert!(crate::shims::opengl::shim_for(name).is_some());
        } else {
            assert!(matches!(lookup(0, name), Lookup::Known(_)), "{name}");
        }
        if crate::frameworks::using_builtin() {
            assert_eq!(super::super::symbol(name), symbol(name), "{name}");
        }
    }
    let Lookup::Known(check) = lookup(0, "gluCheckExtension") else {
        panic!()
    };
    assert_eq!(check.ret(), crate::abi::ArgType::UnsignedByte);
    let Lookup::Known(texture) = lookup(0, "gluQuadricTexture") else {
        panic!()
    };
    assert_eq!(texture.params()[1], crate::abi::ArgType::UnsignedByte);
}
