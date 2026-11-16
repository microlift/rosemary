//! `glGenerateMipmap` through the host's own OpenGL.framework, which this
//! runtime answers itself for the 32-bit float formats Apple's GL would filter
//! on the CPU. See `opengl::mipmap`.
#![cfg(target_os = "macos")]

mod common;

const SOURCE: &str = r#"
#define GL_SILENCE_DEPRECATION
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl3.h>
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

#define CHECK(test) do { if (!(test)) { printf("line %d: %s\n", __LINE__, #test); return 1; } } while (0)
#define SIZE 64
#define LEVELS 7

typedef void (*Generate)(GLenum);
typedef void (*Storage)(GLenum, GLsizei, GLenum, GLsizei, GLsizei);

/// Every level of the bound texture against a 2x2 box filter of the one above.
static int levels_are_box_filtered(GLenum format, int channels, float tolerance) {
    float *above = malloc(SIZE * SIZE * channels * sizeof(float));
    float *below = malloc(SIZE * SIZE * channels * sizeof(float));
    glGetTexImage(GL_TEXTURE_2D, 0, format, GL_FLOAT, above);
    for (int level = 1; level < LEVELS; level++) {
        int size = SIZE >> level, wide = size * 2;
        glGetTexImage(GL_TEXTURE_2D, level, format, GL_FLOAT, below);
        for (int y = 0; y < size; y++)
            for (int x = 0; x < size; x++)
                for (int c = 0; c < channels; c++) {
                    float sum = above[((2 * y) * wide + 2 * x) * channels + c]
                              + above[((2 * y) * wide + 2 * x + 1) * channels + c]
                              + above[((2 * y + 1) * wide + 2 * x) * channels + c]
                              + above[((2 * y + 1) * wide + 2 * x + 1) * channels + c];
                    if (fabsf(sum / 4 - below[(y * size + x) * channels + c]) > tolerance) return 0;
                }
        float *swap = above; above = below; below = swap;
    }
    free(above);
    free(below);
    return 1;
}

static int check(Generate generate, Storage storage, GLenum internal, GLenum format, int channels,
                 float tolerance) {
    float *data = malloc(SIZE * SIZE * channels * sizeof(float));
    for (int i = 0; i < SIZE * SIZE * channels; i++) data[i] = (float)((i * 2654435761u) % 1000) / 1000;
    GLuint texture, other;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    storage(GL_TEXTURE_2D, LEVELS, internal, SIZE, SIZE);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, SIZE, SIZE, format, GL_FLOAT, data);

    // The guest's own state, which generating mipmaps must leave as it was.
    glGenFramebuffers(1, &other);
    glBindFramebuffer(GL_FRAMEBUFFER, other);
    glEnable(GL_SCISSOR_TEST);
    glScissor(0, 0, 1, 1);
    glColorMask(GL_TRUE, GL_FALSE, GL_TRUE, GL_FALSE);

    generate(GL_TEXTURE_2D);

    GLint read = 0, draw = 0;
    GLboolean mask[4];
    glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &read);
    glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &draw);
    glGetBooleanv(GL_COLOR_WRITEMASK, mask);
    CHECK(read == (GLint)other && draw == (GLint)other);
    CHECK(glIsEnabled(GL_SCISSOR_TEST));
    CHECK(mask[0] && !mask[1] && mask[2] && !mask[3]);
    CHECK(glGetError() == GL_NO_ERROR);
    CHECK(levels_are_box_filtered(format, channels, tolerance));

    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDisable(GL_SCISSOR_TEST);
    glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
    glDeleteFramebuffers(1, &other);
    glDeleteTextures(1, &texture);
    free(data);
    return 0;
}

int main(void) {
    CGLPixelFormatAttribute attributes[] = {
        kCGLPFAOpenGLProfile, (CGLPixelFormatAttribute)kCGLOGLPVersion_GL4_Core, 0
    };
    CGLPixelFormatObj pixel_format;
    GLint count;
    CGLContextObj context;
    CHECK(CGLChoosePixelFormat(attributes, &pixel_format, &count) == kCGLNoError && pixel_format);
    CHECK(CGLCreateContext(pixel_format, NULL, &context) == kCGLNoError);
    CGLSetCurrentContext(context);
    Storage storage = (Storage)dlsym(RTLD_DEFAULT, "glTexStorage2D");
    Generate looked_up = (Generate)dlsym(RTLD_DEFAULT, "glGenerateMipmap");
    CHECK(storage && looked_up);
    // The same function whichever way the guest reaches it, as under dyld.
    CHECK(looked_up == (Generate)glGenerateMipmap);
    if (check(glGenerateMipmap, storage, GL_RG32F, GL_RG, 2, 1e-3f)) return 1;
    if (check(looked_up, storage, GL_RGBA32F, GL_RGBA, 4, 1e-3f)) return 1;
    // A format the driver filters on the GPU itself, rounded to 8 bits a level.
    if (check(glGenerateMipmap, storage, GL_RGBA8, GL_RGBA, 4, 2.0f / 255)) return 1;
    puts("mipmaps verified");
    return 0;
}
"#;

#[test]
fn float_mipmaps_are_box_filtered_and_leave_state_alone() {
    let Some(path) = common::build_x86_64("gl_mipmap", SOURCE, &["-framework", "OpenGL"]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = common::run_guest_env(&[("ROSEMARY_PREFIX", "off")], &path, &[]);
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(out, "mipmaps verified\n", "stderr: {err}");
}
