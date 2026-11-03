#include <CoreFoundation/CoreFoundation.h>
#include <CoreGraphics/CoreGraphics.h>
#include <CoreVideo/CoreVideo.h>
#include <stdio.h>

#define CHECK(expr)                                                           \
    do {                                                                      \
        if (!(expr)) {                                                        \
            printf("corevideo/coregraphics failed: %s (%d)\n", #expr, __LINE__); \
            return 1;                                                         \
        }                                                                     \
    } while (0)

static int check_allocators(void) {
    CFAllocatorRef current = CFAllocatorGetDefault();
    CHECK(kCFAllocatorDefault == NULL);
    CHECK(current != NULL);
    CHECK(current == kCFAllocatorSystemDefault);
    CHECK(current != kCFAllocatorMalloc);
    CHECK(CFRetain(current) == current);
    CFRelease(current);
    CHECK(CFAllocatorGetDefault() == kCFAllocatorSystemDefault);

    const CFAllocatorRef allocators[] = {
        kCFAllocatorDefault,
        kCFAllocatorSystemDefault,
        kCFAllocatorMalloc,
        kCFAllocatorMallocZone,
    };
    for (size_t i = 0; i < sizeof(allocators) / sizeof(allocators[0]); ++i) {
        CVPixelBufferRef buffer = NULL;
        CHECK(CVPixelBufferCreate(allocators[i], 7, 5,
                                  kCVPixelFormatType_32BGRA, NULL,
                                  &buffer) == kCVReturnSuccess && buffer);
        CVPixelBufferRelease(buffer);

        // The default is NULL and has no CFDictionary value to supply.
        if (allocators[i] == NULL)
            continue;
        const void *keys[] = {kCVPixelBufferMemoryAllocatorKey};
        const void *values[] = {allocators[i]};
        CFDictionaryRef attrs = CFDictionaryCreate(
            NULL, keys, values, 1, &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks);
        CHECK(attrs);
        CHECK(CVPixelBufferCreate(NULL, 7, 5, kCVPixelFormatType_32BGRA,
                                  attrs, &buffer) == kCVReturnSuccess && buffer);
        CVPixelBufferRelease(buffer);
        CFRelease(attrs);
    }
    return 0;
}

static int check_narrow_dimensions(void) {
    const size_t sizes[][2] = {
        {16384, 1}, {16385, 1}, {20000, 1}, {1, 20000}, {1, 32768},
    };
    for (size_t i = 0; i < sizeof(sizes) / sizeof(sizes[0]); ++i) {
        CVPixelBufferRef buffer = NULL;
        CHECK(CVPixelBufferCreate(NULL, sizes[i][0], sizes[i][1],
                                  kCVPixelFormatType_32BGRA, NULL,
                                  &buffer) == kCVReturnSuccess && buffer);
        CHECK(CVPixelBufferGetWidth(buffer) == sizes[i][0]);
        CHECK(CVPixelBufferGetHeight(buffer) == sizes[i][1]);
        CVPixelBufferRelease(buffer);
    }
    return 0;
}

static int check_numeric_alignment(void) {
    const OSType formats[] = {
        kCVPixelFormatType_32BGRA,
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    };
    const CFStringRef keys[] = {
        kCVPixelBufferBytesPerRowAlignmentKey,
        kCVPixelBufferPlaneAlignmentKey,
    };
    const double values[] = {0, 3.5, 64, -0.5, -1};
    for (int format_index = 0; format_index < 2; ++format_index) {
        for (int key_index = 0; key_index < 2; ++key_index) {
            for (int i = 0; i < 5; ++i) {
                CFNumberRef number = CFNumberCreate(
                    NULL, kCFNumberFloat64Type, &values[i]);
                CHECK(number);
                const void *key = keys[key_index];
                const void *value = number;
                CFDictionaryRef attrs = CFDictionaryCreate(
                    NULL, &key, &value, 1, &kCFTypeDictionaryKeyCallBacks,
                    &kCFTypeDictionaryValueCallBacks);
                CHECK(attrs);
                CVPixelBufferRef buffer = NULL;
                CVReturn result = CVPixelBufferCreate(
                    NULL, 7, 5, formats[format_index], attrs, &buffer);
                // The plane alignment value is ignored on a packed BGRA image.
                const bool negative = values[i] <= -1 &&
                    (key_index == 0 || format_index == 1);
                CHECK(result == (negative ? kCVReturnAllocationFailed : kCVReturnSuccess));
                if (negative) {
                    CHECK(buffer == NULL);
                } else {
                    CHECK(buffer != NULL);
                    size_t stride = format_index
                        ? CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
                        : CVPixelBufferGetBytesPerRow(buffer);
                    size_t expected = key_index == 0 && values[i] == 3.5
                        ? 192 : 64;
                    CHECK(stride == expected);
                    CVPixelBufferRelease(buffer);
                }
                CFRelease(attrs);
                CFRelease(number);
            }
        }
    }
    return 0;
}

static int check_coregraphics_consumers(void) {
    const OSType formats[] = {
        kCVPixelFormatType_32BGRA,
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        kCVPixelFormatType_420YpCbCr8Planar,
    };
    for (size_t format_index = 0; format_index < 3; ++format_index) {
        for (int kind = 0; kind < 2; ++kind) {
            const bool planar = format_index != 0;
            const void *keys[] = {
                kind ? kCVPixelBufferCGImageCompatibilityKey
                     : kCVPixelBufferCGBitmapContextCompatibilityKey,
            };
            const void *values[] = {kCFBooleanTrue};
            CFDictionaryRef attrs = CFDictionaryCreate(
                NULL, keys, values, 1, &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks);
            CHECK(attrs);
            CVPixelBufferRef buffer = NULL;
            CHECK(CVPixelBufferCreate(NULL, 7, 5, formats[format_index], attrs,
                                      &buffer) == kCVReturnSuccess && buffer);
            CHECK(CVPixelBufferLockBaseAddress(buffer, 0) == kCVReturnSuccess);

            // For YCbCr buffers CoreGraphics consumes the Y plane as an 8-bit
            // grayscale image. No RGB conversion or GPU backing is promised.
            void *pixels = planar ? CVPixelBufferGetBaseAddressOfPlane(buffer, 0)
                                  : CVPixelBufferGetBaseAddress(buffer);
            size_t row_bytes = planar ? CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
                                      : CVPixelBufferGetBytesPerRow(buffer);
            CHECK(pixels != NULL);
            CGColorSpaceRef color = planar ? CGColorSpaceCreateDeviceGray()
                                           : CGColorSpaceCreateDeviceRGB();
            CHECK(color);
            CGBitmapInfo bitmap_info = planar
                ? (CGBitmapInfo)kCGImageAlphaNone
                : (kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little);

            CGContextRef bitmap = CGBitmapContextCreate(
                pixels, 7, 5, 8, row_bytes, color, bitmap_info);
            CHECK(bitmap);
            CHECK(CGBitmapContextGetData(bitmap) == pixels);
            CGImageRef snapshot = CGBitmapContextCreateImage(bitmap);
            CHECK(snapshot);
            CHECK(CGImageGetWidth(snapshot) == 7 && CGImageGetHeight(snapshot) == 5);
            CGImageRelease(snapshot);

            CGDataProviderRef provider = CGDataProviderCreateWithData(
                NULL, pixels, row_bytes * 5, NULL);
            CHECK(provider);
            CGImageRef direct_image = CGImageCreate(
                7, 5, 8, planar ? 8 : 32, row_bytes, color, bitmap_info,
                provider, NULL, false, kCGRenderingIntentDefault);
            CHECK(direct_image);
            CHECK(CGImageGetWidth(direct_image) == 7);
            CGImageRelease(direct_image);
            CGDataProviderRelease(provider);
            CGContextRelease(bitmap);
            CGColorSpaceRelease(color);
            CHECK(CVPixelBufferUnlockBaseAddress(buffer, 0) == kCVReturnSuccess);
            CVPixelBufferRelease(buffer);
            CFRelease(attrs);
        }
    }
    return 0;
}

int main(void) {
    CHECK(check_allocators() == 0);
    CHECK(check_numeric_alignment() == 0);
    CHECK(check_narrow_dimensions() == 0);
    CHECK(check_coregraphics_consumers() == 0);
    puts("coregraphics consumer ok");
    return 0;
}
