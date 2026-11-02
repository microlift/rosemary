mod common;

use std::sync::OnceLock;

const SOURCE: &str = r##"
#import <Cocoa/Cocoa.h>
#include <OpenGL/OpenGL.h>
#include <OpenGL/gl.h>
#include <OpenGL/glu.h>
#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

// Core enumerators from OpenGL/gl3.h; it cannot be included with legacy gl.h.
#define GL_MAJOR_VERSION 0x821B
#define GL_MINOR_VERSION 0x821C
#define GL_NUM_EXTENSIONS 0x821D

#define CHECK(test) do { if (!(test)) { fprintf(stderr, "line %d: %s\n", __LINE__, #test); return 1; } } while (0)
static CGLContextObj transfer;
static GLuint transferred_vao;
static int worker_result;
static void *draw_thread(void *unused) {
    worker_result = CGLSetCurrentContext(transfer);
    if (CGLGetCurrentContext() != transfer) worker_result = 2;
    glBindVertexArrayAPPLE(transferred_vao);
    GLint bound = 0;
    glGetIntegerv(GL_VERTEX_ARRAY_BINDING_APPLE, &bound);
    if ((GLuint)bound != transferred_vao || glGetError()) worker_result = 3;
    CGLSetCurrentContext(NULL);
    return NULL;
}

int main(int argc, char **argv) {
    const int core = argc > 1 && !strcmp(argv[1], "core");
    NSOpenGLPixelFormatAttribute attributes[] = {
        NSOpenGLPFADoubleBuffer, NSOpenGLPFAColorSize, 24,
        NSOpenGLPFAOpenGLProfile,
        core ? NSOpenGLProfileVersion4_1Core : NSOpenGLProfileVersionLegacy, 0
    };
    NSOpenGLPixelFormat *format = [[NSOpenGLPixelFormat alloc] initWithAttributes:attributes];
    NSOpenGLContext *first = [[NSOpenGLContext alloc] initWithFormat:format shareContext:nil];
    NSOpenGLContext *second = [[NSOpenGLContext alloc] initWithFormat:format shareContext:nil];
    CGLContextObj first_handle = [first CGLContextObj];
    CGLContextObj second_handle = [second CGLContextObj];
    CHECK(first_handle && second_handle && first_handle != second_handle);
    CHECK([first CGLContextObj] == first_handle);
    CHECK(CGLGetCurrentContext() == NULL && [NSOpenGLContext currentContext] == nil);
    void *framework = dlopen("/System/Library/Frameworks/OpenGL.framework/OpenGL", RTLD_LAZY);
    CHECK(framework);
    CHECK(dlsym(framework, "CGLGetCurrentContext") == (void *)CGLGetCurrentContext);
    CHECK(dlsym(RTLD_DEFAULT, "CGLGetCurrentContext") == (void *)CGLGetCurrentContext);
    CHECK(dlsym(framework, "CGLGetShareGroup") == (void *)CGLGetShareGroup);
    CHECK(dlsym(framework, "CGLFlushDrawable") == (void *)CGLFlushDrawable);
    CHECK(dlsym(framework, "gluOrtho2D") == (void *)gluOrtho2D);
    CHECK(dlsym(framework, "glGetString") == (void *)glGetString);
    CHECK(dlsym(framework, "CGLChoosePixelFormat") == (void *)CGLChoosePixelFormat);
    CHECK(dlsym(framework, "gluPerspective") == NULL);
    CHECK(dlsym(RTLD_DEFAULT, "gluPerspective") == NULL);
    // GL 4.3, which a Mac's framework does not export.
    CHECK(dlsym(framework, "glDispatchCompute") == NULL);
    CHECK(dlsym(RTLD_DEFAULT, "glDispatchCompute") == NULL);
    // libSystem does not link OpenGL, so its handle finds none of it.
    void *system = dlopen("/usr/lib/libSystem.B.dylib", RTLD_LAZY);
    CHECK(system && dlsym(system, "glDispatchCompute") == NULL);
    CHECK(dlsym(system, "CGLChoosePixelFormat") == NULL);
    CHECK(dlsym(system, "glGetString") == NULL);
    GLint zero = 0, one = 1;
    CHECK(CGLSetParameter(first_handle, kCGLCPSwapInterval, &zero) == kCGLNoError);
    CHECK(CGLSetParameter(second_handle, kCGLCPSwapInterval, &one) == kCGLNoError);
    CHECK(CGLGetCurrentContext() == NULL);
    NSWindow *window = [[NSWindow alloc]
        initWithContentRect:NSMakeRect(0, 0, 64, 64) styleMask:0 backing:2 defer:NO];
    [first setView:[window contentView]];
    [first makeCurrentContext];
    CHECK(CGLGetCurrentContext() == first_handle);
    CHECK([NSOpenGLContext currentContext] == first);
    // Native fixtures can inspect SDL directly without adding guest API imports.
    typedef _Bool (*GetInterval)(int *);
    GetInterval interval = argc > 2 && !strcmp(argv[2], "interval")
        ? (GetInterval)dlsym(RTLD_DEFAULT, "SDL_GL_GetSwapInterval") : NULL;
    int actual_interval = -99;
    CHECK(argc <= 2 || strcmp(argv[2], "interval") || interval);
    if (interval) {
        CHECK(interval(&actual_interval) && actual_interval == 0);
        CHECK(CGLSetParameter(second_handle, kCGLCPSwapInterval, &one) == kCGLNoError);
        CHECK(interval(&actual_interval) && actual_interval == 0);
    }
    CHECK((void *)CGLGetShareGroup(first_handle) == (void *)first_handle);
    CHECK((void *)CGLGetShareGroup(second_handle) == (void *)second_handle);
    GLuint vao = 0;
    glGenVertexArraysAPPLE(1, &vao);
    glBindVertexArrayAPPLE(vao);
    GLint bound = 0;
    glGetIntegerv(GL_VERTEX_ARRAY_BINDING_APPLE, &bound);
    CHECK(vao && (GLuint)bound == vao && glGetError() == GL_NO_ERROR);
    glViewport(0, 0, 64, 64);
    glClearColor(0.25f, 0.5f, 0.75f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    unsigned char pixel[4] = {0};
    glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
    CHECK(abs(pixel[0] - 64) <= 1 && abs(pixel[1] - 128) <= 1 && abs(pixel[2] - 191) <= 1);
    CHECK(glGetError() == GL_NO_ERROR);
    CHECK(CGLFlushDrawable(first_handle) == kCGLNoError);
    CHECK(CGLEnable(first_handle, kCGLCERasterization) == kCGLNoError);
    CHECK(CGLDisable(first_handle, kCGLCERasterization) == kCGLNoError);
    if (core) {
        GLint major = 0, minor = 0, count = 0;
        glGetIntegerv(GL_MAJOR_VERSION, &major);
        glGetIntegerv(GL_MINOR_VERSION, &minor);
        CHECK(major < 4 || (major == 4 && minor <= 1));
        glGetIntegerv(GL_NUM_EXTENSIONS, &count);
        typedef const GLubyte *(*GetStringi)(GLenum, GLuint);
        GetStringi indexed = (GetStringi)dlsym(framework, "glGetStringi");
        CHECK(indexed);
        for (GLint i = 0; i < count; ++i) {
            const char *extension = (const char *)indexed(GL_EXTENSIONS, i);
            CHECK(extension && strcmp(extension, "GL_ARB_direct_state_access"));
        }
        CHECK(glGetError() == GL_NO_ERROR);
        const char *source = "#version 150\n#extension GL_EXT_bindable_uniform : require\nvoid main() { gl_Position = vec4(0.0); }\n";
        GLuint shader = glCreateShader(GL_VERTEX_SHADER);
        glShaderSource(shader, 1, &source, NULL);
        glCompileShader(shader);
        GLint compiled = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &compiled);
        CHECK(compiled && glGetError() == GL_NO_ERROR);
        glDeleteShader(shader);
    } else {
        // A Mac's legacy profile is GL 2.1, which has no GL_MAJOR_VERSION.
        CHECK(!strncmp((const char *)glGetString(GL_VERSION), "2.1", 3));
        CHECK(!strcmp((const char *)glGetString(GL_SHADING_LANGUAGE_VERSION), "1.20"));
        GLint major = -1;
        glGetIntegerv(GL_MAJOR_VERSION, &major);
        CHECK(major == -1 && glGetError() == GL_INVALID_ENUM);
        CHECK(!strstr((const char *)glGetString(GL_EXTENSIONS), "GL_ARB_direct_state_access"));
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        gluOrtho2D(0, 10, 0, 20);
        GLdouble matrix[16];
        glGetDoublev(GL_PROJECTION_MATRIX, matrix);
        CHECK(matrix[0] > 0.199 && matrix[0] < 0.201);
        CHECK(matrix[5] > 0.099 && matrix[5] < 0.101);
        CHECK(matrix[10] == -1 && matrix[12] == -1 && matrix[13] == -1 && matrix[15] == 1);
        CHECK(glGetError() == GL_NO_ERROR);
    }
    CHECK(CGLSetCurrentContext(second_handle) == kCGLNoError);
    CHECK(CGLGetCurrentContext() == second_handle && [NSOpenGLContext currentContext] == second);
    if (interval) CHECK(interval(&actual_interval) && actual_interval == 1);
    CHECK(CGLSetCurrentContext(first_handle) == kCGLNoError);
    CHECK(CGLGetCurrentContext() == first_handle);
    if (interval) CHECK(interval(&actual_interval) && actual_interval == 0);
    [NSOpenGLContext clearCurrentContext];
    CHECK(CGLGetCurrentContext() == NULL);
    // Clearing the logical binding and then blocking must hand off the primary
    // context, preserving VAOs that a sibling context would not share.
    transfer = first_handle;
    transferred_vao = vao;
    pthread_t worker;
    CHECK(pthread_create(&worker, NULL, draw_thread, NULL) == 0);
    CHECK(pthread_join(worker, NULL) == 0 && worker_result == 0);
    CHECK([NSOpenGLContext currentContext] == nil && CGLGetCurrentContext() == NULL);
    CHECK(CGLSetCurrentContext(first_handle) == kCGLNoError);
    glBindVertexArrayAPPLE(vao);
    CHECK(glGetError() == GL_NO_ERROR);
    glDeleteVertexArraysAPPLE(1, &vao);
    CHECK(CGLSetCurrentContext(NULL) == kCGLNoError);
    CHECK(CGLClearDrawable(first_handle) == kCGLNoError);
    CHECK([first view] == nil);
    puts(core ? "core GL verified" : "legacy GL verified");
    return 0;
}
"##;

fn fixture() -> Option<String> {
    static FIXTURE: OnceLock<Option<String>> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            if let Ok(path) = std::env::var("ROSEMARY_GL_TEST_FIXTURE") {
                return Some(path);
            }
            let host_arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x86_64"
            };
            common::build_x86_64(
                "builtin_opengl",
                SOURCE,
                &[
                    "-x",
                    "objective-c",
                    "-framework",
                    "Cocoa",
                    "-framework",
                    "OpenGL",
                    "-arch",
                    host_arch,
                    "-Wno-deprecated-declarations",
                ],
            )
        })
        .clone()
}

fn run(mode: &str, engine: &str) {
    if std::env::var_os("ROSEMARY_GL_TEST_FIXTURE").is_none() && !common::can_build_x86_64() {
        skip!("cannot build Mach-O OpenGL fixture")
    }
    let path = fixture().expect("compile the OpenGL fixture");
    let output = common::driver_command()
        .args([
            "run",
            engine,
            &path,
            mode,
            if engine == "--native" {
                "interval"
            } else {
                "nointerval"
            },
        ])
        .env("ROSEMARY_OBJC", "builtin")
        .env("ROSEMARY_BUNDLED", "1")
        .env("ROSEMARY_PREFIX", "off")
        .env("ROSEMARY_LIBC", "builtin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{engine} {mode}: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("{mode} GL verified")));
}

#[test]
fn legacy_gl_routes_and_renders_through_the_builtin_framework() {
    run("legacy", "--emulated");
}

#[test]
fn core_gl_routes_and_renders_through_the_builtin_framework() {
    run("core", "--emulated");
}

#[test]
fn native_guests_use_the_same_builtin_gl_framework() {
    if !darwin::tls::can_enter_darwin() {
        skip!("the host cannot provide Darwin thread-local storage for native guests")
    }
    run("legacy", "--native");
    run("core", "--native");
}
