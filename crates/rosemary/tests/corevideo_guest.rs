mod common;

use common::{build_native_with, build_x86_64, run_guest_env, run_native};

const PIXEL_BUFFER_GUEST: &str = r#"
#include <CoreFoundation/CoreFoundation.h>
#include <CoreVideo/CoreVideo.h>
#include <stdint.h>
#include <stdio.h>

#define CHECK(c) do { if (!(c)) { printf("corevideo check failed: %s\n", #c); return 1; } } while (0)

static int check_attribute_parity(void) {
    const OSType formats[] = {
        kCVPixelFormatType_32BGRA,
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    };
    const CFStringRef extended[] = {
        kCVPixelBufferExtendedPixelsLeftKey,
        kCVPixelBufferExtendedPixelsRightKey,
        kCVPixelBufferExtendedPixelsTopKey,
        kCVPixelBufferExtendedPixelsBottomKey,
    };
    const double multiples[] = {0.0, 0.3, 0.9, -0.9, -1.0, -1.2, 1.2};
    for (size_t f = 0; f < 2; ++f) {
        for (size_t i = 0; i < 4; ++i) {
            for (size_t j = 0; j < 7; ++j) {
                CFNumberRef number = CFNumberCreate(NULL, kCFNumberFloat64Type,
                                                    &multiples[j]);
                CHECK(number != NULL);
                const void *key = extended[i];
                const void *value = number;
                CFDictionaryRef attrs = CFDictionaryCreate(NULL, &key, &value,
                    1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
                CHECK(attrs != NULL);
                CVPixelBufferRef buffer = NULL;
                CVReturn result = CVPixelBufferCreate(NULL, 7, 5,
                    formats[f], attrs, &buffer);
                CVReturn expected = kCVReturnSuccess;
                if (multiples[j] <= -1.0) expected = kCVReturnAllocationFailed;
#ifdef ROSEMARY_GUEST
                // CPU-only storage deliberately cannot honor nonzero padding.
                if (multiples[j] >= 1.0) expected = kCVReturnUnsupported;
#endif
                CHECK(result == expected);
                if (expected == kCVReturnSuccess) {
                    CHECK(buffer != NULL);
                    CVPixelBufferRelease(buffer);
                } else {
                    CHECK(buffer == NULL);
                }
                CFRelease(attrs);
                CFRelease(number);
            }
            const void *key = extended[i];
            const void *value = kCFBooleanTrue;
            CFDictionaryRef attrs = CFDictionaryCreate(NULL, &key, &value,
                1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            CHECK(attrs != NULL);
            CVPixelBufferRef buffer = NULL;
            CHECK(CVPixelBufferCreate(NULL, 7, 5, formats[f], attrs, &buffer)
                  == kCVReturnSuccess);
            CHECK(buffer != NULL);
            CVPixelBufferRelease(buffer);
            CFRelease(attrs);
        }
    }
    const CFStringRef compatibility[] = {
        kCVPixelBufferCGBitmapContextCompatibilityKey,
        kCVPixelBufferCGImageCompatibilityKey,
        kCVPixelBufferMetalCompatibilityKey,
        kCVPixelBufferOpenGLCompatibilityKey,
    };
    for (size_t i = 0; i < 4; ++i) {
        const void *key = compatibility[i];
        const void *value = CFSTR("non-Boolean compatibility hint");
        CFDictionaryRef attrs = CFDictionaryCreate(NULL, &key, &value,
            1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        CHECK(attrs != NULL);
        CVPixelBufferRef buffer = NULL;
        CHECK(CVPixelBufferCreate(NULL, 7, 5,
            kCVPixelFormatType_32BGRA, attrs, &buffer) == kCVReturnSuccess);
        CHECK(buffer != NULL);
        CVPixelBufferRelease(buffer);
        CFRelease(attrs);
    }
    return 0;
}

static int check_plane_alignment_minimum(void) {
    const OSType formats[] = {
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        kCVPixelFormatType_420YpCbCr8Planar,
    };
    const int64_t values[] = {0, 1, 3, 24, 65, 4097};
    for (size_t f = 0; f < 2; ++f) {
        for (size_t i = 0; i < 6; ++i) {
            CFNumberRef number = CFNumberCreate(NULL, kCFNumberSInt64Type, &values[i]);
            CHECK(number != NULL);
            const void *key = kCVPixelBufferPlaneAlignmentKey;
            const void *value = number;
            CFDictionaryRef attrs = CFDictionaryCreate(NULL, &key, &value,
                1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            CHECK(attrs != NULL);
            CVPixelBufferRef b = NULL;
            CHECK(CVPixelBufferCreate(NULL, 7, 5, formats[f], attrs, &b) == kCVReturnSuccess);
            CHECK(b != NULL);
            CHECK(CVPixelBufferLockBaseAddress(b, 0) == kCVReturnSuccess);
            uintptr_t base = (uintptr_t)CVPixelBufferGetBaseAddress(b);
            CHECK(base != 0 && base % 64 == 0);
            for (size_t plane = 0; plane < CVPixelBufferGetPlaneCount(b); ++plane) {
                uintptr_t address = (uintptr_t)CVPixelBufferGetBaseAddressOfPlane(b, plane);
                CHECK(address >= base && address % 64 == 0);
#ifdef ROSEMARY_GUEST
                // This CPU allocation also preserves the requested multiple.
                if (values[i] > 0) CHECK(address % (uint64_t)values[i] == 0);
#endif
                size_t stride = CVPixelBufferGetBytesPerRowOfPlane(b, plane);
                size_t height = CVPixelBufferGetHeightOfPlane(b, plane);
                CHECK(stride <= SIZE_MAX / height);
                CHECK(address - base + stride * height <= CVPixelBufferGetDataSize(b));
            }
            CHECK(CVPixelBufferUnlockBaseAddress(b, 0) == kCVReturnSuccess);
            CVPixelBufferRelease(b);
            CFRelease(attrs);
            CFRelease(number);
        }
    }
    return 0;
}

int main(void) {
    CHECK(check_attribute_parity() == 0);
    CHECK(check_plane_alignment_minimum() == 0);
    CVPixelBufferRef packed = NULL;
    CHECK(CVPixelBufferCreate(NULL, 7, 5, kCVPixelFormatType_32BGRA,
                              NULL, &packed) == kCVReturnSuccess);
    CHECK(packed != NULL);
    CHECK(CFGetTypeID(packed) == CVPixelBufferGetTypeID());
    CHECK(CVPixelBufferGetWidth(packed) == 7);
    CHECK(CVPixelBufferGetHeight(packed) == 5);
    CHECK(CVPixelBufferGetPixelFormatType(packed) == kCVPixelFormatType_32BGRA);
    CHECK(CVPixelBufferIsPlanar(packed) == 0);
    CHECK(CVPixelBufferGetPlaneCount(packed) == 0);
    CHECK(CVPixelBufferGetBytesPerRow(packed) >= 28);
    CHECK(CVPixelBufferGetWidthOfPlane(packed, 0) == 7);
    CHECK(CVPixelBufferGetHeightOfPlane(packed, 0) == 5);
    CHECK(CVPixelBufferGetBytesPerRowOfPlane(packed, 0) == CVPixelBufferGetBytesPerRow(packed));
    CHECK(CVPixelBufferGetWidthOfPlane(packed, 1) == 0);
    CHECK(CVPixelBufferGetHeightOfPlane(packed, 1) == 0);
    CHECK(CVPixelBufferGetBytesPerRowOfPlane(packed, 1) == 0);
    CHECK(CVPixelBufferGetBaseAddress(packed) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(packed, 0) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(packed, 1) == NULL);
    CHECK(CVPixelBufferLockBaseAddress(packed, 0) == kCVReturnSuccess);
    uint8_t *base = CVPixelBufferGetBaseAddress(packed);
    CHECK(base != NULL);
    CHECK(base == CVPixelBufferGetBaseAddressOfPlane(packed, 0));
    CHECK(CVPixelBufferGetBaseAddressOfPlane(packed, 1) == NULL);
    CHECK(CVPixelBufferLockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(packed) == base);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(packed, 0) == base);
    base[0] = 0x35;
    base[CVPixelBufferGetBytesPerRow(packed) * 4 + 27] = 0x79;
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(packed) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(packed, 0) == NULL);
    CHECK(CVPixelBufferLockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(((uint8_t *)CVPixelBufferGetBaseAddress(packed))[0] == 0x35);
    CHECK(((uint8_t *)CVPixelBufferGetBaseAddress(packed))[CVPixelBufferGetBytesPerRow(packed) * 4 + 27] == 0x79);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(packed) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(packed, 0) == NULL);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 0) == kCVReturnError);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnError);
    CHECK(CVPixelBufferLockBaseAddress(packed, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferLockBaseAddress(packed, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 0) == kCVReturnError);
    CHECK(CVPixelBufferLockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferLockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, kCVPixelBufferLock_ReadOnly) == kCVReturnError);
    for (CVOptionFlags flags = 2; flags <= 8; ++flags) {
        CHECK(CVPixelBufferLockBaseAddress(packed, flags) == kCVReturnSuccess);
        CHECK(CVPixelBufferGetBaseAddress(packed) != NULL);
        CHECK(CVPixelBufferUnlockBaseAddress(packed, flags) == kCVReturnSuccess);
        CHECK(CVPixelBufferGetBaseAddress(packed) == NULL);
        CHECK(CVPixelBufferUnlockBaseAddress(packed, flags) == kCVReturnError);
    }
    CHECK(CVPixelBufferLockBaseAddress(packed, 2) == kCVReturnSuccess);
    CHECK(CVPixelBufferLockBaseAddress(packed, 8) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 3) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(packed) != NULL);
    CHECK(CVPixelBufferUnlockBaseAddress(packed, 8) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(packed) == NULL);
    CVPixelBufferRef retained = CVPixelBufferRetain(packed);
    CHECK(retained == packed);
    CVPixelBufferRelease(packed);
    CHECK(CVPixelBufferGetWidth(retained) == 7);
    CVBufferRelease(retained);

    // GPU-shareable requests still give a buffer the CPU can lock.
    CFDictionaryRef surface = CFDictionaryCreate(NULL, NULL, NULL, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CHECK(surface != NULL);
    const void *keys[] = {
        kCVPixelBufferIOSurfacePropertiesKey,
        kCVPixelBufferIOSurfaceCoreAnimationCompatibilityKey,
        kCVPixelBufferMetalCompatibilityKey,
    };
    const void *values[] = { surface, kCFBooleanTrue, kCFBooleanTrue };
    CFDictionaryRef options = CFDictionaryCreate(NULL, keys, values, 3,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CHECK(options != NULL);
    CVPixelBufferRef shareable = NULL;
    CHECK(CVPixelBufferCreate(NULL, 7, 5, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                              options, &shareable) == kCVReturnSuccess);
    CHECK(CVPixelBufferLockBaseAddress(shareable, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(shareable, 1) != NULL);
    CHECK(CVPixelBufferUnlockBaseAddress(shareable, 0) == kCVReturnSuccess);
    CVPixelBufferRelease(shareable);
    CFRelease(options);
    CFRelease(surface);

    CVPixelBufferRef planar = NULL;
    CHECK(CVPixelBufferCreate(NULL, 7, 5, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                              NULL, &planar) == kCVReturnSuccess);
    CHECK(CVPixelBufferIsPlanar(planar) != 0);
    CHECK(CVPixelBufferGetPlaneCount(planar) == 2);
    CHECK(CVPixelBufferGetWidthOfPlane(planar, 1) == 4);
    CHECK(CVPixelBufferGetHeightOfPlane(planar, 1) == 3);
    CHECK(CVPixelBufferGetBaseAddress(planar) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(planar, 0) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(planar, 1) == NULL);
    CHECK(CVPixelBufferLockBaseAddress(planar, 0) == kCVReturnSuccess);
    uint8_t *uv = CVPixelBufferGetBaseAddressOfPlane(planar, 1);
    CHECK(uv != NULL);
    CHECK(CVPixelBufferGetBytesPerRowOfPlane(planar, 1) >= 8);
    uv[0] = 0x41;
    CHECK(CVPixelBufferLockBaseAddress(planar, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferUnlockBaseAddress(planar, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(planar) != NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(planar, 1) == uv);
    CHECK(CVPixelBufferUnlockBaseAddress(planar, 0) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(planar) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(planar, 1) == NULL);
    CHECK(CVPixelBufferLockBaseAddress(planar, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(((uint8_t *)CVPixelBufferGetBaseAddressOfPlane(planar, 1))[0] == 0x41);
    CHECK(CVPixelBufferUnlockBaseAddress(planar, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    CHECK(CVPixelBufferGetBaseAddress(planar) == NULL);
    CHECK(CVPixelBufferGetBaseAddressOfPlane(planar, 0) == NULL);
    CFRetain(planar);
    CVPixelBufferRelease(planar);
    CHECK(CVPixelBufferGetPlaneCount(planar) == 2);
    CFRelease(planar);
    // A buffer-wide planar stride must describe accessible storage even for
    // tall, narrow buffers where rounding exceeds the final plane offset.
    const OSType planar_types[] = {
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        kCVPixelFormatType_420YpCbCr8Planar,
    };
    const size_t heights[] = {5, 1000, 1001};
    for (size_t f = 0; f < 2; ++f) {
        for (size_t h = 0; h < 3; ++h) {
            CVPixelBufferRef tall = NULL;
            CHECK(CVPixelBufferCreate(NULL, 7, heights[h], planar_types[f], NULL,
                                      &tall) == kCVReturnSuccess);
            CHECK(tall != NULL);
            size_t global_stride = CVPixelBufferGetBytesPerRow(tall);
            CHECK(global_stride <= SIZE_MAX / heights[h]);
            size_t envelope = global_stride * heights[h];
            CHECK(CVPixelBufferGetDataSize(tall) >= envelope);
            CHECK(CVPixelBufferLockBaseAddress(tall, 0) == kCVReturnSuccess);
            uint8_t *whole = CVPixelBufferGetBaseAddress(tall);
            CHECK(whole != NULL);
            whole[envelope - 1] = 0xA5;
            CHECK(whole[envelope - 1] == 0xA5);
            CHECK(CVPixelBufferUnlockBaseAddress(tall, 0) == kCVReturnSuccess);
            CVPixelBufferRelease(tall);
        }
    }
    puts("corevideo pixel buffer abi ok");
    return 0;
}
"#;

#[test]
fn emulated_x86_64_corevideo_pixel_buffers_cross_the_guest_abi() {
    let Some(path) = build_x86_64(
        "corevideo_pixelbuffer",
        PIXEL_BUFFER_GUEST,
        &[
            "-DROSEMARY_GUEST",
            "-framework",
            "CoreVideo",
            "-framework",
            "CoreFoundation",
        ],
    ) else {
        skip!("cannot build x86_64 Mach-O CoreVideo fixture");
    };
    let prefix = format!("{}-corevideo-x86_64", common::test_prefix());
    let (status, out, err) = run_guest_env(
        &[("ROSEMARY_OBJC", "builtin"), ("ROSEMARY_PREFIX", &prefix)],
        &path,
        &[],
    );
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(out, "corevideo pixel buffer abi ok\n", "stderr: {err}");
}

#[test]
fn native_macos_corevideo_fixture_obeys_the_same_contract() {
    let Some(path) = build_native_with(
        "corevideo_pixelbuffer_native",
        PIXEL_BUFFER_GUEST,
        &["-framework", "CoreVideo", "-framework", "CoreFoundation"],
    ) else {
        skip!("cannot build a native macOS CoreVideo fixture");
    };
    let Some((status, out)) = run_native(&path) else {
        skip!("native Mach-O execution unavailable");
    };
    assert_eq!(status, Some(0), "stdout: {out}");
    assert_eq!(out, "corevideo pixel buffer abi ok\n");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_aarch64_rosemary_corevideo_pixel_buffers_cross_the_guest_abi() {
    let Some(path) = build_native_with(
        "corevideo_pixelbuffer_arm64_rosemary",
        PIXEL_BUFFER_GUEST,
        &[
            "-DROSEMARY_GUEST",
            "-framework",
            "CoreVideo",
            "-framework",
            "CoreFoundation",
        ],
    ) else {
        skip!("cannot build native arm64 Mach-O CoreVideo fixture");
    };
    let prefix = format!("{}-corevideo-arm64", common::test_prefix());
    let output = common::driver_command()
        .env("ROSEMARY_OBJC", "builtin")
        .env("ROSEMARY_PREFIX", prefix)
        .arg("run")
        .arg("--native")
        .arg(&path)
        .output()
        .expect("launch Rosemary native guest");
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {out}\nstderr: {err}"
    );
    assert_eq!(out, "corevideo pixel buffer abi ok\n", "stderr: {err}");
}

#[test]
fn x86_64_guest_registers_marshal_pixel_buffer_create_and_boolean_returns() {
    use machish::memory::{AddressSpace, BufferSpace, Placement};
    use microlift::arch::x86_64::context::RegisterContext;
    use rosemary::abi::{GuestMemory, marshal, table};

    const STACK: u64 = 0x2_3000_0000;
    let mut mem = BufferSpace::new(STACK);
    mem.reserve(4096, 16, Placement::Fixed(STACK))
        .expect("test guest stack");
    AddressSpace::write(&mut mem, STACK, &[0; 16]).expect("initialize stack");

    let abi = match table::lookup(0, "CVPixelBufferCreate") {
        table::Lookup::Known(abi) => abi,
        _ => panic!("missing CVPixelBufferCreate ABI declaration"),
    };
    let mut buffer: corefoundation::base::CFTypeRef = std::ptr::null();
    let mut regs = RegisterContext::new();
    regs.rsp = STACK;
    regs.rdi = 0;
    regs.rsi = 7;
    regs.rdx = 5;
    regs.rcx = u64::from(u32::from_be_bytes(*b"420v"));
    regs.r8 = 0;
    regs.r9 = (&mut buffer as *mut corefoundation::base::CFTypeRef) as u64;
    marshal::call(
        "CVPixelBufferCreate",
        abi,
        corevideo::pixel_buffer::CVPixelBufferCreate as *const () as usize as u64,
        &mut regs,
        &mem as &dyn GuestMemory,
    )
    .expect("ABI call succeeds");
    assert_eq!(regs.rax as u32 as i32, 0);
    assert!(!buffer.is_null());

    let bool_abi = match table::lookup(0, "CVPixelBufferIsPlanar") {
        table::Lookup::Known(abi) => abi,
        _ => panic!("missing CVPixelBufferIsPlanar ABI declaration"),
    };
    regs = RegisterContext::new();
    regs.rsp = STACK;
    regs.rdi = buffer as u64;
    marshal::call(
        "CVPixelBufferIsPlanar",
        bool_abi,
        corevideo::pixel_buffer::CVPixelBufferIsPlanar as *const () as usize as u64,
        &mut regs,
        &mem as &dyn GuestMemory,
    )
    .expect("Boolean ABI call succeeds");
    assert_eq!(regs.rax & 0xff, 1);
    unsafe { corevideo::pixel_buffer::CVPixelBufferRelease(buffer) };
}

const COREGRAPHICS_GUEST: &str = include_str!("fixtures/corevideo_coregraphics.c");

#[test]
fn native_apple_coregraphics_consumes_cpu_pixel_buffers() {
    let Some(path) = build_native_with(
        "corevideo_coregraphics_native",
        COREGRAPHICS_GUEST,
        &[
            "-framework",
            "CoreVideo",
            "-framework",
            "CoreGraphics",
            "-framework",
            "CoreFoundation",
        ],
    ) else {
        skip!("cannot compile native CoreGraphics pixel-buffer fixture");
    };
    let Some((status, out)) = run_native(&path) else {
        skip!("native Mach-O execution unavailable");
    };
    assert_eq!(status, Some(0), "stdout: {out}");
    assert_eq!(out, "coregraphics consumer ok\n");
}

#[test]
fn emulated_x86_64_guest_uses_coregraphics_consumers() {
    let Some(path) = build_x86_64(
        "corevideo_coregraphics_emulated",
        COREGRAPHICS_GUEST,
        &[
            "-framework",
            "CoreVideo",
            "-framework",
            "CoreGraphics",
            "-framework",
            "CoreFoundation",
        ],
    ) else {
        skip!("cannot build x86_64 Mach-O CoreGraphics fixture");
    };
    let prefix = format!("{}-corevideo-x86_64", common::test_prefix());
    let (status, out, err) = run_guest_env(
        &[("ROSEMARY_OBJC", "builtin"), ("ROSEMARY_PREFIX", &prefix)],
        &path,
        &[],
    );
    assert_eq!(status, Some(0), "stdout: {out}\nstderr: {err}");
    assert_eq!(out, "coregraphics consumer ok\n", "stderr: {err}");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_arm64_guest_uses_coregraphics_consumers() {
    let Some(path) = build_native_with(
        "corevideo_coregraphics_arm64",
        COREGRAPHICS_GUEST,
        &[
            "-framework",
            "CoreVideo",
            "-framework",
            "CoreGraphics",
            "-framework",
            "CoreFoundation",
        ],
    ) else {
        skip!("cannot build native arm64 CoreGraphics fixture");
    };
    let prefix = format!("{}-corevideo-arm64", common::test_prefix());
    let output = common::driver_command()
        .env("ROSEMARY_OBJC", "builtin")
        .env("ROSEMARY_PREFIX", prefix)
        .arg("run")
        .arg("--native")
        .arg(&path)
        .output()
        .expect("launch Rosemary ARM64 guest");
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {out}\nstderr: {err}"
    );
    assert_eq!(out, "coregraphics consumer ok\n", "stderr: {err}");
}
