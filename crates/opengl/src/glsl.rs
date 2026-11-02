//! Shader source on its way to the driver.

use std::ffi::{CStr, c_char};

use crate::driver::Entry;

/// Reassemble the source a guest passed as `count` pieces.
///
/// A negative or absent length means the piece is NUL-terminated, which is what
/// `glShaderSource` promises.
///
/// # Safety
///
/// `strings` must point to `count` readable pointers, and `lengths` must be
/// null or point to `count` readable integers, as `glShaderSource` requires.
unsafe fn joined(count: i32, strings: *const *const c_char, lengths: *const i32) -> String {
    let mut source = String::new();
    for i in 0..count.max(0) as usize {
        let piece = unsafe { *strings.add(i) };
        if piece.is_null() {
            continue;
        }
        // SAFETY: the caller's obligation; a negative length means the piece is
        // NUL-terminated instead.
        let length = match lengths.is_null() {
            true => -1,
            false => unsafe { *lengths.add(i) },
        };
        let bytes = match length < 0 {
            // SAFETY: NUL-terminated, as the negative length declares.
            true => unsafe { CStr::from_ptr(piece) }.to_bytes(),
            // SAFETY: `length` bytes, as the guest just said.
            false => unsafe { std::slice::from_raw_parts(piece as *const u8, length as usize) },
        };
        source.push_str(&String::from_utf8_lossy(bytes));
    }
    source
}

/// `ROSEMARY_DUMP_SHADERS=DIR`: write every shader source there, before and
/// after the rewrite.
fn dump(shader: u32, before: &str, after: &str) {
    let Some(dir) = std::env::var_os("ROSEMARY_DUMP_SHADERS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(format!("{shader}.before.glsl")), before);
    if after != before {
        let _ = std::fs::write(dir.join(format!("{shader}.after.glsl")), after);
    }
}

// SAFETY: `void glShaderSource(GLuint, GLsizei, const GLchar *const *, const GLint *)`.
static SHADER_SOURCE: Entry<unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32)> =
    unsafe { Entry::new(c"glShaderSource") };

/// `glShaderSource`, with the rewrite in the middle.
pub unsafe extern "C" fn shader_source(
    shader: u32,
    count: i32,
    strings: *const *const c_char,
    lengths: *const i32,
) {
    let Some(driver) = SHADER_SOURCE.get() else {
        return;
    };
    if strings.is_null() || count <= 0 {
        // SAFETY: passed through exactly as the guest wrote it.
        unsafe { driver(shader, count, strings, lengths) };
        return;
    }
    // SAFETY: the guest's own arguments, read as `glShaderSource` defines them.
    let before = unsafe { joined(count, strings, lengths) };
    let after = rewrite(&before);
    dump(shader, &before, &after);

    let length = after.len() as i32;
    let pointer = after.as_ptr() as *const c_char;
    // SAFETY: one piece, its length, and a pointer to bytes that outlive the
    // call - the driver copies the source before returning.
    unsafe { driver(shader, 1, &pointer, &length) };
}

/// The extension a program built against Apple's OpenGL asks for and this host
/// has not implemented since it existed.
const BINDABLE_UNIFORM: &str = "GL_EXT_bindable_uniform";

/// Whether `line` is an `#extension` directive naming `extension`.
fn is_directive_for(line: &str, extension: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("#extension") && line.contains(extension)
}

/// Whether the shader actually declares a `bindable uniform`.
///
/// The interesting case is that it usually does not. A generator that can turn
/// the feature off writes `bindable uniform` and then patches the same bytes to
/// `/*ndabl*/uniform` - the same length, so no offset in its own tables moves -
/// which leaves an ordinary uniform and a `#extension` line that now asks for
/// nothing. That vestigial line is the whole problem: `require` is an error on
/// a driver without the extension, so a shader that would have compiled fine is
/// refused over a feature it does not use.
fn declares_a_bindable_uniform(source: &str) -> bool {
    source
        .lines()
        .filter(|line| !is_directive_for(line, BINDABLE_UNIFORM))
        .any(|line| line.contains("bindable"))
}

/// Blank the `#extension` directives naming `extension`, keeping the newlines.
///
/// A blank line rather than a deleted one: the driver reports compile errors by
/// line number, and a source whose numbering no longer matches what the guest
/// handed over turns every later diagnostic into a small puzzle.
fn blank_directives(source: &str, extension: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        match is_directive_for(line, extension) {
            true => out.push('\n'),
            false => out.push_str(line),
        }
    }
    out
}

/// The rewrite: drop a `require` for an extension the shader never uses.
///
/// Only when it is unused, so a shader that really does want the feature is
/// still refused by the driver, naming the extension - which is a far better
/// report than one about a `bindable` keyword nothing explained.
fn rewrite(source: &str) -> String {
    if !source.contains(BINDABLE_UNIFORM) || declares_a_bindable_uniform(source) {
        return source.to_string();
    }
    blank_directives(source, BINDABLE_UNIFORM)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case every shader in the program that prompted this is in: the
    /// declarations are already plain uniforms and the directive is left over.
    #[test]
    fn a_vestigial_require_is_dropped() {
        let source = "#version 120\n\
                      #extension GL_EXT_bindable_uniform : require\n\
                      /*ndabl*/uniform vec4 PConstFloat[224];\n";
        let rewritten = rewrite(source);
        assert!(!rewritten.contains(BINDABLE_UNIFORM), "{rewritten}");
        assert!(rewritten.contains("uniform vec4 PConstFloat[224];"));
    }

    /// And the numbering is what it was, so a compile error still points at the
    /// line the guest wrote.
    #[test]
    fn dropping_it_keeps_every_line_where_it_was() {
        let source = "#version 120\n\
                      #extension GL_EXT_bindable_uniform : require\n\
                      uniform vec4 a;\n";
        assert_eq!(rewrite(source).lines().count(), source.lines().count());
        assert_eq!(rewrite(source).lines().nth(2), Some("uniform vec4 a;"));
    }

    /// A shader that really uses the feature keeps its directive, so the driver
    /// refuses it by name instead of over a keyword with no explanation.
    #[test]
    fn a_shader_that_uses_the_feature_is_left_alone() {
        let source = "#extension GL_EXT_bindable_uniform : require\n\
                      bindable uniform vec4 PConstFloat[224];\n";
        assert_eq!(rewrite(source), source);
    }

    /// Nothing else is touched.
    #[test]
    fn another_extension_is_not_dropped() {
        let source = "#extension GL_ARB_shader_texture_lod : require\nuniform vec4 a;\n";
        assert_eq!(rewrite(source), source);
    }
}
