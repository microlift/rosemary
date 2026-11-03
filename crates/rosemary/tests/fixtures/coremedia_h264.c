// Declarations rather than the SDK headers, so the fixture also builds with a
// bare clang and the rootfs libSystem.
#include <stddef.h>
#include <stdint.h>

typedef const void *CFTypeRef;
typedef int32_t OSStatus;
typedef struct { int32_t width, height; } CMVideoDimensions;

extern const CFTypeRef kCFBooleanTrue;
extern const CFTypeRef kCMFormatDescriptionExtension_FormatName;
extern const CFTypeRef kCMFormatDescriptionExtension_FullRangeVideo;
extern const CFTypeRef kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms;

int printf(const char *, ...);
int memcmp(const void *, const void *, size_t);
CFTypeRef CFStringCreateWithCString(CFTypeRef, const char *, uint32_t);
CFTypeRef CFDictionaryGetValue(CFTypeRef, CFTypeRef);
const uint8_t *CFDataGetBytePtr(CFTypeRef);
long CFDataGetLength(CFTypeRef);
unsigned char CFEqual(CFTypeRef, CFTypeRef);
unsigned long CFGetTypeID(CFTypeRef);
void CFRelease(CFTypeRef);

OSStatus CMVideoFormatDescriptionCreateFromH264ParameterSets(
    CFTypeRef, size_t, const uint8_t *const *, const size_t *, int, CFTypeRef *);
OSStatus CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
    CFTypeRef, size_t, const uint8_t **, size_t *, size_t *, int *);
CMVideoDimensions CMVideoFormatDescriptionGetDimensions(CFTypeRef);
uint32_t CMFormatDescriptionGetMediaType(CFTypeRef);
uint32_t CMFormatDescriptionGetMediaSubType(CFTypeRef);
CFTypeRef CMFormatDescriptionGetExtension(CFTypeRef, CFTypeRef);
unsigned long CMFormatDescriptionGetTypeID(void);

#define CHECK(c) do { if (!(c)) { printf("coremedia check failed: %s\n", #c); return 1; } } while (0)

// x264's 1920x1080 High profile, and a 1280x720 full-range one.
static const uint8_t sps_1080[] = {
    0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00,
    0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x08, 0x3c, 0x60, 0xc6, 0x58,
};
static const uint8_t sps_720_full[] = {
    0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50, 0x05, 0xbb, 0x01, 0x6e, 0x04, 0x04, 0x02,
    0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x03, 0x01, 0x07, 0x8c, 0x18, 0xcb,
};
static const uint8_t pps[] = {0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0};

static OSStatus create(const uint8_t *sps, size_t sps_size, CFTypeRef *out) {
    const uint8_t *sets[] = {sps, pps};
    size_t sizes[] = {sps_size, sizeof pps};
    return CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, sets, sizes, 4, out);
}

int main(void) {
    CFTypeRef desc = NULL;
    CHECK(create(sps_1080, sizeof sps_1080, &desc) == 0);
    CHECK(desc != NULL);
    CHECK(CFGetTypeID(desc) == CMFormatDescriptionGetTypeID());
    CHECK(CMFormatDescriptionGetMediaType(desc) == 'vide');
    CHECK(CMFormatDescriptionGetMediaSubType(desc) == 'avc1');
    CMVideoDimensions size = CMVideoFormatDescriptionGetDimensions(desc);
    CHECK(size.width == 1920 && size.height == 1080);

    size_t count = 0;
    int nal_length = 0;
    const uint8_t *set = NULL;
    size_t set_size = 0;
    CHECK(CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
              desc, 0, &set, &set_size, &count, &nal_length) == 0);
    CHECK(count == 2 && nal_length == 4);
    CHECK(set_size == sizeof sps_1080 && memcmp(set, sps_1080, set_size) == 0);
    CHECK(CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
              desc, 1, &set, &set_size, NULL, NULL) == 0);
    CHECK(set_size == sizeof pps && memcmp(set, pps, set_size) == 0);

    CFTypeRef name = CFStringCreateWithCString(NULL, "H.264", 0x08000100);
    CHECK(CFEqual(CMFormatDescriptionGetExtension(
              desc, kCMFormatDescriptionExtension_FormatName), name));
    CFRelease(name);
    CFTypeRef atoms = CMFormatDescriptionGetExtension(
        desc, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms);
    CHECK(atoms != NULL);
    CFTypeRef key = CFStringCreateWithCString(NULL, "avcC", 0x08000100);
    CFTypeRef avcc = CFDictionaryGetValue(atoms, key);
    CFRelease(key);
    CHECK(avcc != NULL);
    const uint8_t *record = CFDataGetBytePtr(avcc);
    CHECK(CFDataGetLength(avcc) > 8);
    // Version 1, then profile, compatibility and level from the SPS, then the
    // NAL length less one and the SPS count.
    CHECK(record[0] == 1 && record[1] == 0x64 && record[3] == 0x28);
    CHECK((record[4] & 3) == 3 && (record[5] & 0x1f) == 1);
    CFRelease(desc);

    CHECK(create(sps_720_full, sizeof sps_720_full, &desc) == 0);
    size = CMVideoFormatDescriptionGetDimensions(desc);
    CHECK(size.width == 1280 && size.height == 720);
    CHECK(CFEqual(CMFormatDescriptionGetExtension(
              desc, kCMFormatDescriptionExtension_FullRangeVideo), kCFBooleanTrue));
    CFRelease(desc);

    printf("coremedia ok\n");
    return 0;
}
