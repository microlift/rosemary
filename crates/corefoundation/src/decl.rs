#![allow(non_upper_case_globals)]

use exports::decl::{Api, ConstPtr, Double, Fragment, Long, Ptr, UChar, UInt, ULong, UShort, Void};

use crate::collections::number_type;
use crate::string::encoding;
use crate::{api, callbacks, propertylist};

// FIXME (ACCURACY): `Boolean`, `UInt8`, `UInt32` and `UniChar` are MacTypes.h's,
// which CFBase.h includes on macOS. They are declared here until an SDK
// supplies that header.
exports::c_types! {
    "CoreFoundation/CFBase.h" {
        typedef Boolean = UChar;
        typedef UInt8 = UChar;
        typedef UInt32 = UInt;
        typedef UniChar = UShort;
        typedef CFIndex = Long;
        typedef CFTypeID = ULong;
        typedef CFOptionFlags = ULong;
        typedef CFHashCode = ULong;
        typedef CFTypeRef = ConstPtr<Void>;
        typedef CFPropertyListRef = CFTypeRef;
        opaque CFStringRef = const __CFString;
        opaque CFMutableStringRef = __CFString;
        opaque CFAllocatorRef = const __CFAllocator;
    }
    "CoreFoundation/CFString.h" {
        typedef CFStringEncoding = UInt32;
    }
    "CoreFoundation/CFNumber.h" {
        opaque CFNumberRef = const __CFNumber;
        opaque CFBooleanRef = const __CFBoolean;
        typedef CFNumberType = CFIndex;
    }
    "CoreFoundation/CFData.h" {
        opaque CFDataRef = const __CFData;
        opaque CFMutableDataRef = __CFData;
    }
    "CoreFoundation/CFDate.h" {
        typedef CFTimeInterval = Double;
        typedef CFAbsoluteTime = CFTimeInterval;
    }
    "CoreFoundation/CFError.h" {
        opaque CFErrorRef = __CFError;
    }
    "CoreFoundation/CFArray.h" {
        opaque CFArrayRef = const __CFArray;
        opaque CFMutableArrayRef = __CFArray;
        callback CFArrayRetainCallBack = fn(CFAllocatorRef, ConstPtr<Void>) -> ConstPtr<Void>;
        callback CFArrayReleaseCallBack = fn(CFAllocatorRef, ConstPtr<Void>);
        callback CFArrayCopyDescriptionCallBack = fn(ConstPtr<Void>) -> CFStringRef;
        callback CFArrayEqualCallBack = fn(ConstPtr<Void>, ConstPtr<Void>) -> Boolean;
        struct CFArrayCallBacks = callbacks::ValueCallBacks {
            version: CFIndex,
            retain: CFArrayRetainCallBack,
            release: CFArrayReleaseCallBack,
            copyDescription: CFArrayCopyDescriptionCallBack,
            equal: CFArrayEqualCallBack,
        }
    }
    "CoreFoundation/CFDictionary.h" {
        opaque CFDictionaryRef = const __CFDictionary;
        opaque CFMutableDictionaryRef = __CFDictionary;
        callback CFDictionaryRetainCallBack =
            fn(CFAllocatorRef, ConstPtr<Void>) -> ConstPtr<Void>;
        callback CFDictionaryReleaseCallBack = fn(CFAllocatorRef, ConstPtr<Void>);
        callback CFDictionaryCopyDescriptionCallBack = fn(ConstPtr<Void>) -> CFStringRef;
        callback CFDictionaryEqualCallBack = fn(ConstPtr<Void>, ConstPtr<Void>) -> Boolean;
        callback CFDictionaryHashCallBack = fn(ConstPtr<Void>) -> CFHashCode;
        struct CFDictionaryKeyCallBacks = callbacks::KeyCallBacks {
            version: CFIndex,
            retain: CFDictionaryRetainCallBack,
            release: CFDictionaryReleaseCallBack,
            copyDescription: CFDictionaryCopyDescriptionCallBack,
            equal: CFDictionaryEqualCallBack,
            hash: CFDictionaryHashCallBack,
        }
        struct CFDictionaryValueCallBacks = callbacks::ValueCallBacks {
            version: CFIndex,
            retain: CFDictionaryRetainCallBack,
            release: CFDictionaryReleaseCallBack,
            copyDescription: CFDictionaryCopyDescriptionCallBack,
            equal: CFDictionaryEqualCallBack,
        }
    }
    "CoreFoundation/CFPropertyList.h" {
        typedef CFPropertyListFormat = CFIndex;
    }
}

exports::c_functions! {
    "CoreFoundation/CFBase.h" {
        fn CFRetain(cf: CFTypeRef) -> CFTypeRef = api::CFRetain;
        fn CFRelease(cf: CFTypeRef) = api::CFRelease;
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID = api::CFGetTypeID;
        fn CFEqual(cf1: CFTypeRef, cf2: CFTypeRef) -> Boolean = api::CFEqual;
        fn CFHash(cf: CFTypeRef) -> CFHashCode = api::CFHash;
    }
    "CoreFoundation/CFString.h" {
        fn CFStringGetTypeID() -> CFTypeID = api::CFStringGetTypeID;
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            cStr: ConstPtr<exports::decl::Char>,
            encoding: CFStringEncoding,
        ) -> CFStringRef = api::CFStringCreateWithCString;
        fn CFStringGetLength(theString: CFStringRef) -> CFIndex = api::CFStringGetLength;
        fn CFStringGetCString(
            theString: CFStringRef,
            buffer: Ptr<exports::decl::Char>,
            bufferSize: CFIndex,
            encoding: CFStringEncoding,
        ) -> Boolean = api::CFStringGetCString;
    }
    "CoreFoundation/CFNumber.h" {
        fn CFBooleanGetTypeID() -> CFTypeID = api::CFBooleanGetTypeID;
        fn CFBooleanGetValue(boolean: CFBooleanRef) -> Boolean = api::CFBooleanGetValue;
        fn CFNumberGetTypeID() -> CFTypeID = api::CFNumberGetTypeID;
        fn CFNumberCreate(
            allocator: CFAllocatorRef,
            theType: CFNumberType,
            valuePtr: ConstPtr<Void>,
        ) -> CFNumberRef = api::CFNumberCreate;
        fn CFNumberGetValue(
            number: CFNumberRef,
            theType: CFNumberType,
            valuePtr: Ptr<Void>,
        ) -> Boolean = api::CFNumberGetValue;
    }
    "CoreFoundation/CFData.h" {
        fn CFDataGetTypeID() -> CFTypeID = api::CFDataGetTypeID;
        fn CFDataCreate(
            allocator: CFAllocatorRef,
            bytes: ConstPtr<UInt8>,
            length: CFIndex,
        ) -> CFDataRef = api::CFDataCreate;
        fn CFDataGetLength(theData: CFDataRef) -> CFIndex = api::CFDataGetLength;
        fn CFDataGetBytePtr(theData: CFDataRef) -> ConstPtr<UInt8> = api::CFDataGetBytePtr;
    }
    "CoreFoundation/CFDate.h" {
        fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime = api::CFAbsoluteTimeGetCurrent;
    }
    "CoreFoundation/CFArray.h" {
        fn CFArrayGetTypeID() -> CFTypeID = api::CFArrayGetTypeID;
        fn CFArrayCreateMutable(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            callBacks: ConstPtr<CFArrayCallBacks>,
        ) -> CFMutableArrayRef = api::CFArrayCreateMutable;
        fn CFArrayGetCount(theArray: CFArrayRef) -> CFIndex = api::CFArrayGetCount;
        fn CFArrayGetValueAtIndex(theArray: CFArrayRef, idx: CFIndex) -> ConstPtr<Void>
            = api::CFArrayGetValueAtIndex;
        fn CFArrayAppendValue(theArray: CFMutableArrayRef, value: ConstPtr<Void>)
            = api::CFArrayAppendValue;
    }
    "CoreFoundation/CFDictionary.h" {
        fn CFDictionaryGetTypeID() -> CFTypeID = api::CFDictionaryGetTypeID;
        fn CFDictionaryCreateMutable(
            allocator: CFAllocatorRef,
            capacity: CFIndex,
            keyCallBacks: ConstPtr<CFDictionaryKeyCallBacks>,
            valueCallBacks: ConstPtr<CFDictionaryValueCallBacks>,
        ) -> CFMutableDictionaryRef = api::CFDictionaryCreateMutable;
        fn CFDictionaryGetCount(theDict: CFDictionaryRef) -> CFIndex = api::CFDictionaryGetCount;
        fn CFDictionaryGetValue(theDict: CFDictionaryRef, key: ConstPtr<Void>) -> ConstPtr<Void>
            = api::CFDictionaryGetValue;
        fn CFDictionarySetValue(
            theDict: CFMutableDictionaryRef,
            key: ConstPtr<Void>,
            value: ConstPtr<Void>,
        ) = api::CFDictionarySetValue;
    }
    "CoreFoundation/CFPropertyList.h" {
        fn CFPropertyListCreateData(
            allocator: CFAllocatorRef,
            propertyList: CFPropertyListRef,
            format: CFPropertyListFormat,
            options: CFOptionFlags,
            error: Ptr<CFErrorRef>,
        ) -> CFDataRef = propertylist::CFPropertyListCreateData;
    }
}

exports::c_variables! {
    "CoreFoundation/CFBase.h" {
        kCFAllocatorDefault: CFAllocatorRef;
    }
    "CoreFoundation/CFNumber.h" {
        kCFBooleanTrue: CFBooleanRef;
        kCFBooleanFalse: CFBooleanRef;
    }
    "CoreFoundation/CFArray.h" {
        kCFTypeArrayCallBacks: CFArrayCallBacks;
    }
    "CoreFoundation/CFDictionary.h" {
        kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
        kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    }
}

exports::c_enums! {
    "CoreFoundation/CFString.h" {
        CFStringEncoding {
            kCFStringEncodingMacRoman = encoding::MAC_ROMAN,
            kCFStringEncodingWindowsLatin1 = encoding::WINDOWS_LATIN1,
            kCFStringEncodingISOLatin1 = encoding::ISO_LATIN_1,
            kCFStringEncodingASCII = encoding::ASCII,
            kCFStringEncodingUnicode = encoding::UTF16,
            kCFStringEncodingUTF8 = encoding::UTF8,
            kCFStringEncodingUTF16 = encoding::UTF16,
            kCFStringEncodingUTF16BE = encoding::UTF16_BE,
            kCFStringEncodingUTF16LE = encoding::UTF16_LE,
            kCFStringEncodingUTF32 = encoding::UTF32,
            kCFStringEncodingUTF32BE = encoding::UTF32_BE,
            kCFStringEncodingUTF32LE = encoding::UTF32_LE,
        }
    }
    "CoreFoundation/CFNumber.h" {
        CFNumberType {
            kCFNumberSInt8Type = number_type::SINT8,
            kCFNumberSInt16Type = number_type::SINT16,
            kCFNumberSInt32Type = number_type::SINT32,
            kCFNumberSInt64Type = number_type::SINT64,
            kCFNumberFloat32Type = number_type::FLOAT32,
            kCFNumberFloat64Type = number_type::FLOAT64,
            kCFNumberCharType = number_type::CHAR,
            kCFNumberShortType = number_type::SHORT,
            kCFNumberIntType = number_type::INT,
            kCFNumberLongType = number_type::LONG,
            kCFNumberLongLongType = number_type::LONG_LONG,
            kCFNumberFloatType = number_type::FLOAT,
            kCFNumberDoubleType = number_type::DOUBLE,
            kCFNumberCFIndexType = number_type::CFINDEX,
            kCFNumberNSIntegerType = number_type::NSINTEGER,
            kCFNumberCGFloatType = number_type::CGFLOAT,
        }
    }
    "CoreFoundation/CFPropertyList.h" {
        CFPropertyListFormat {
            kCFPropertyListXMLFormat_v1_0 = propertylist::format::XML_V1_0,
            kCFPropertyListBinaryFormat_v1_0 = propertylist::format::BINARY_V1_0,
        }
    }
}

const FRAGMENTS: &[Fragment] = &[Fragment {
    header: "CoreFoundation/CFString.h",
    // Clang lays the literal out as a constant CFString whose `isa` binds
    // `___CFConstantStringClassReference`, which this crate exports.
    text: "#define CFSTR(cStr) ((CFStringRef)__builtin___CFStringMakeConstantString(\"\" cStr \"\"))",
}];

pub const API: Api = Api {
    framework: "CoreFoundation",
    types: TYPES,
    functions: FUNCTIONS,
    variables: VARIABLES,
    enums: ENUMS,
    fragments: FRAGMENTS,
};
