use static_init::dynamic;

use super::ArgType as T;

/// One imported function's microlift signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionAbi {
    /// Symbol name without the Mach-O underscore.
    pub name: &'static str,
    /// Return type followed by parameter types, in `DynamicSymbol` order.
    pub types: &'static [T],
    /// C-variadic; callable only through [`crate::shims`].
    pub variadic: bool,
    /// Parameter indices containing guest function pointers that need thunks.
    pub callbacks: &'static [u8],
    /// May run guest code outside callbacks, as `dlopen` does with initializers.
    /// Such calls cannot be inlined before guest state is synchronized.
    pub reenters_guest: bool,
}

impl FunctionAbi {
    pub fn ret(&self) -> T {
        self.types.first().copied().unwrap_or(T::None)
    }

    pub fn params(&self) -> &'static [T] {
        self.types.split_first().map_or(&[], |(_, rest)| rest)
    }
}

/// What the table has to say about a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Known(&'static FunctionAbi),
    /// Described, and deliberately not callable. The payload is why.
    Refused(&'static str),
    Unknown,
}

/// Strip the leading underscore a Mach-O import carries.
fn c_name(symbol: &str) -> &str {
    symbol.strip_prefix('_').unwrap_or(symbol)
}

/// Look a symbol up by tag first, then by name.
pub fn lookup(tag: u64, symbol: &str) -> Lookup {
    if tag != 0
        && let Some(abi) = by_tag(tag)
    {
        return Lookup::Known(abi);
    }
    let name = c_name(symbol);
    match lookup_exact(name) {
        Lookup::Unknown => {
            if let Some(reason) = data_family(name) {
                return Lookup::Refused(reason);
            }
            match name.split_once('$') {
                // `_fts_open$INODE64` and friends: the variant only ever existed
                // on x86, and its signature is the plain symbol's.
                Some((base, _)) if !base.is_empty() => lookup_exact(base),
                _ => Lookup::Unknown,
            }
        }
        found => found,
    }
}

/// Data imports whose names carry a suffix, so [`REFUSED`] cannot list them.
fn data_family(name: &str) -> Option<&'static str> {
    const DATA: &str = "an Objective-C metadata import, read through its slot and never called";
    let families = [
        "OBJC_CLASS_$_",
        "OBJC_METACLASS_$_",
        "OBJC_IVAR_$_",
        // The type object a `@catch` clause matches against, which is data for
        // the same reason a C++ `type_info` is.
        "OBJC_EHTYPE_",
        "OBJC_PROTOCOL_$_",
        "OBJC_LABEL_PROTOCOL_$_",
        // The empty cache and vtable every `objc_class` the compiler emits
        // points at, and the `isa` of a constant `@"..."` string.
        "_objc_empty_cache",
        "_objc_empty_vtable",
        "__CFConstantStringClassReference",
    ];
    families.iter().any(|f| name.starts_with(f)).then_some(DATA)
}

fn lookup_exact(name: &str) -> Lookup {
    match index().get(name) {
        Some(Described::Known(abi)) => Lookup::Known(abi),
        Some(Described::Refused(reason)) => Lookup::Refused(reason),
        None => Lookup::Unknown,
    }
}

/// What the table says about one name: a callable signature, or a refusal.
#[derive(Clone, Copy)]
enum Described {
    Known(&'static FunctionAbi),
    Refused(&'static str),
}

/// [`SIGNATURES`] and [`REFUSED`], by name, built once.
fn index() -> &'static std::collections::HashMap<&'static str, Described> {
    #[dynamic]
    static INDEX: std::collections::HashMap<&'static str, Described> = {
        let mut map = std::collections::HashMap::with_capacity(SIGNATURES.len() + REFUSED.len());
        for (name, reason) in REFUSED {
            map.insert(*name, Described::Refused(reason));
        }
        // Signatures last, so a name in both tables gets the callable answer.
        for abi in SIGNATURES {
            map.insert(abi.name, Described::Known(abi));
        }
        map
    };
    &INDEX
}

/// Signatures reachable by a provider-assigned tag. Empty today.
fn by_tag(_tag: u64) -> Option<&'static FunctionAbi> {
    None
}

/// `(ret, args...)`, in `DynamicSymbol` order.
macro_rules! sig {
    ($name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        FunctionAbi { name: $name, types: &[$ret $(, $arg)*], variadic: false, callbacks: &[], reenters_guest: false }
    };
}

/// As [`sig`], for a C-variadic function. The fixed arguments are listed; the
/// ellipsis is implied by the flag.
macro_rules! vsig {
    ($name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        FunctionAbi {
            name: $name,
            types: &[$ret $(, $arg)* , T::VariadicList],
            variadic: true,
            callbacks: &[],
            reenters_guest: false,
        }
    };
}

/// As [`sig`], for a function taking guest function pointers. `$cb` lists the
/// zero-based parameter indices that are callbacks.
macro_rules! cbsig {
    ([$($cb:literal),* $(,)?], $name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        FunctionAbi {
            name: $name,
            types: &[$ret $(, $arg)*],
            variadic: false,
            callbacks: &[$($cb),*],
            reenters_guest: false,
        }
    };
}

/// As [`sig`], for a function that can run guest code; see
/// [`FunctionAbi::reenters_guest`].
macro_rules! rsig {
    ($name:literal, $ret:expr $(, $arg:expr)* $(,)?) => {
        FunctionAbi {
            name: $name,
            types: &[$ret $(, $arg)*],
            variadic: false,
            callbacks: &[],
            reenters_guest: true,
        }
    };
}

const V: T = T::None;
const I32: T = T::SignedWord;
const U32: T = T::UnsignedWord;
const I64: T = T::SignedDoubleword;
const U64: T = T::UnsignedDoubleword;
const F32: T = T::SinglePrecisionFp;
const F64: T = T::DoublePrecisionFp;
const P: T = T::Pointer;
const U8: T = T::UnsignedByte;
const I16: T = T::SignedHalfword;
const U16: T = T::UnsignedHalfword;
const BOOL: T = U8;

/// Known signatures, in no particular order - [`index`] builds the lookup. Names must
/// be unique; `signatures_are_unique` says so.
pub static SIGNATURES: &[FunctionAbi] = &[
    // The Steam bridge's one entry point, which the generated
    // `steamclient.dylib` calls for every Steamworks call the guest makes. See
    // [`crate::steam`].
    sig!("rosemary_steamclient_dispatch", I32, U32, P),
    sig!("getsegbyname", P, P),
    sig!("getsectbyname", P, P, P),
    sig!("get_etext", U64),
    sig!("get_end", U64),
    // OpenAL 1.1. Its scalar typedefs are fixed-width even on Darwin.
    sig!("alBufferData", V, U32, I32, P, I32, I32),
    sig!("alDeleteBuffers", V, I32, P),
    sig!("alDeleteSources", V, I32, P),
    sig!("alDistanceModel", V, I32),
    sig!("alGenBuffers", V, I32, P),
    sig!("alGenSources", V, I32, P),
    sig!("alGetError", I32),
    sig!("alGetFloat", F32, I32),
    sig!("alGetSourcei", V, U32, I32, P),
    sig!("alIsExtensionPresent", U8, P),
    sig!("alListenerfv", V, I32, P),
    sig!("alSourcePlay", V, U32),
    sig!("alSourceQueueBuffers", V, U32, I32, P),
    sig!("alSourceStop", V, U32),
    sig!("alSourcef", V, U32, I32, F32),
    sig!("alSourcefv", V, U32, I32, P),
    sig!("alSourcei", V, U32, I32, I32),
    sig!("alcCloseDevice", U8, P),
    sig!("alcCreateContext", P, P, P),
    sig!("alcDestroyContext", V, P),
    sig!("alcGetCurrentContext", P),
    sig!("alcGetEnumValue", I32, P, P),
    sig!("alcGetProcAddress", P, P, P),
    sig!("alcIsExtensionPresent", U8, P, P),
    sig!("alcMakeContextCurrent", U8, P),
    sig!("alcOpenDevice", P, P),
    sig!("AudioComponentFindNext", P, P, P),
    sig!("AudioComponentInstanceDispose", I32, P),
    sig!("AudioComponentInstanceNew", I32, P, P),
    //
    // The render callback is installed as an `AURenderCallbackStruct`
    // property rather than as an argument, so `AudioUnitSetProperty` is an
    // ordinary pointer here and [`crate::shims::audiounit`] handles the
    // callback.
    cbsig!([2], "AudioObjectAddPropertyListener", I32, U32, P, P, P),
    sig!("AudioObjectGetPropertyData", I32, U32, P, U32, P, P, P),
    sig!("AudioObjectGetPropertyDataSize", I32, U32, P, U32, P, P),
    // The removal takes the same listener, and it must be the *same pointer* the
    // host was given - so it goes out as a thunk too, and [`crate::thunk`]
    // hands back the one already made for that guest address rather than a
    // second trampoline CoreAudio would not recognise.
    cbsig!([2], "AudioObjectRemovePropertyListener", I32, U32, P, P, P),
    sig!("AudioOutputUnitStart", I32, P),
    sig!("AudioOutputUnitStop", I32, P),
    //
    // A queue takes its callback as an ordinary argument, where an
    // `AudioUnit` takes one inside a structure - so `cbsig!` describes it and
    // the thunk is made at the seam.
    sig!("AudioQueueAllocateBuffer", I32, P, U32, P),
    sig!("AudioQueueDispose", I32, P, BOOL),
    sig!("AudioQueueEnqueueBuffer", I32, P, P, U32, P),
    sig!("AudioQueueFlush", I32, P),
    sig!("AudioQueueFreeBuffer", I32, P, P),
    cbsig!([1], "AudioQueueNewInput", I32, P, P, P, P, P, U32, P),
    cbsig!([1], "AudioQueueNewOutput", I32, P, P, P, P, P, U32, P),
    sig!("AudioQueuePause", I32, P),
    sig!("AudioQueueSetProperty", I32, P, U32, P, U32),
    sig!("AudioQueueStart", I32, P, P),
    sig!("AudioQueueStop", I32, P, BOOL),
    sig!("AudioUnitGetProperty", I32, P, U32, U32, U32, P, P),
    sig!("AudioUnitInitialize", I32, P),
    sig!("AudioUnitRender", I32, P, P, P, U32, U32, P),
    sig!("AudioUnitSetProperty", I32, P, U32, U32, U32, P, U32),
    sig!("AudioUnitUninitialize", I32, P),
    //
    // An `AUNode` is a bare `SInt32`, not a handle, and everything returns an
    // `OSStatus`. `AUGraphAddNode` takes its description by address, where the
    // deprecated `AUGraphNewNode` took it by value.
    sig!("NewAUGraph", I32, P),
    sig!("DisposeAUGraph", I32, P),
    sig!("AUGraphAddNode", I32, P, P, P),
    sig!("AUGraphConnectNodeInput", I32, P, I32, U32, I32, U32),
    sig!("AUGraphDisconnectNodeInput", I32, P, I32, U32),
    sig!("AUGraphInitialize", I32, P),
    // `AUGraphNodeInfo(AUGraph, AUNode, AudioComponentDescription *out,
    // AudioUnit *out)` - both outputs optional.
    sig!("AUGraphNodeInfo", I32, P, I32, P, P),
    sig!("AUGraphOpen", I32, P),
    sig!("AUGraphStart", I32, P),
    sig!("AUGraphStop", I32, P),
    // `AUGraphUpdate(AUGraph, Boolean *isUpdated)`, whose second argument is an
    // optional output and not a flag.
    sig!("AUGraphUpdate", I32, P, P),
    // `AudioUnitSetParameter(AudioUnit, AudioUnitParameterID,
    // AudioUnitParameterScope, AudioUnitElement, AudioUnitParameterValue,
    // UInt32 bufferOffsetInFrames)` - the value is a `Float32`, so it arrives in
    // `xmm0` while the four words before it are integers.
    sig!("AudioUnitSetParameter", I32, P, U32, U32, U32, F32, U32),
    // The listener is never called, so it need not cross as a thunk.
    sig!("AudioUnitAddPropertyListener", I32, P, U32, P, P),
    sig!(
        "AudioUnitRemovePropertyListenerWithUserData",
        I32,
        P,
        U32,
        P,
        P
    ),
    sig!("AudioUnitReset", I32, P, U32, U32),
    //
    // `FillComplexBuffer`'s second argument is the guest's input callback, which
    // the host calls, so it crosses as a thunk.
    sig!("AudioConverterNew", I32, P, P, P),
    sig!("AudioConverterDispose", I32, P),
    sig!("AudioConverterReset", I32, P),
    cbsig!(
        [1],
        "AudioConverterFillComplexBuffer",
        I32,
        P,
        P,
        P,
        P,
        P,
        P
    ),
    // `CCCrypt`: 11 integer-class arguments, within `MAX_INT_ARGS_TOTAL` - the
    // three past the eighth spill to the host stack, all doubleword-wide.
    sig!("CCCrypt", I32, U32, U32, U32, P, U64, P, P, U64, P, U64, P),
    sig!("CCCryptorCreate", I32, U32, U32, U32, P, U64, P, P),
    sig!(
        "CCCryptorCreateFromData",
        I32,
        U32,
        U32,
        U32,
        P,
        U64,
        P,
        P,
        U64,
        P,
        P
    ),
    sig!("CCCryptorRelease", I32, P),
    sig!("CCCryptorUpdate", I32, P, P, U64, P, U64, P),
    sig!("CCCryptorFinal", I32, P, P, U64, P),
    sig!("CCCryptorGetOutputLength", U64, P, U64, BOOL),
    sig!("CCCryptorReset", I32, P, P),
    sig!("CC_MD5", P, P, U32, P),
    sig!("CC_MD5_Final", I32, P, P),
    sig!("CC_MD5_Init", I32, P),
    sig!("CC_MD5_Update", I32, P, P, U32),
    sig!("CC_SHA1", P, P, U32, P),
    sig!("CC_SHA1_Final", I32, P, P),
    sig!("CC_SHA1_Init", I32, P),
    sig!("CC_SHA1_Update", I32, P, P, U32),
    sig!("CC_SHA224", P, P, U32, P),
    sig!("CC_SHA224_Final", I32, P, P),
    sig!("CC_SHA224_Init", I32, P),
    sig!("CC_SHA224_Update", I32, P, P, U32),
    sig!("CC_SHA256", P, P, U32, P),
    sig!("CC_SHA256_Final", I32, P, P),
    sig!("CC_SHA256_Init", I32, P),
    sig!("CC_SHA256_Update", I32, P, P, U32),
    sig!("CC_SHA384", P, P, U32, P),
    sig!("CC_SHA384_Final", I32, P, P),
    sig!("CC_SHA384_Init", I32, P),
    sig!("CC_SHA384_Update", I32, P, P, U32),
    sig!("CC_SHA512", P, P, U32, P),
    sig!("CC_SHA512_Final", I32, P, P),
    sig!("CC_SHA512_Init", I32, P),
    sig!("CC_SHA512_Update", I32, P, P, U32),
    sig!("CFArrayAppendValue", V, P, P),
    // `CFRange` by value, which is the case the roadmap already settled for
    // `CFStringGetBytes`.
    cbsig!([3], "CFArrayApplyFunction", V, P, I64, I64, P, P),
    //
    // `CFIndex` is `long`, `Boolean` is `unsigned char`, `CFStringEncoding` is a
    // `UInt32` - three narrow types a "pointer or int64" guess gets wrong in the
    // direction that does not fault.
    sig!("CFArrayCreate", P, P, P, I64, P),
    sig!("CFArrayCreateMutable", P, P, I64, P),
    sig!("CFArrayCreateCopy", P, P, P),
    sig!("CFArrayInsertValueAtIndex", V, P, I64, P),
    sig!("CFArrayRemoveValueAtIndex", V, P, I64),
    sig!("CFArrayRemoveAllValues", V, P),
    sig!("CFArrayExchangeValuesAtIndices", V, P, I64, I64),
    sig!("CFArrayGetCount", I64, P),
    sig!("CFArrayGetTypeID", U64),
    sig!("CFArrayGetValueAtIndex", P, P, I64),
    // The bundle these answer for is the *guest's*, because
    // [`crate::bundle`]'s re-exec made the running executable the guest's own -
    // so `CFBundleCopyExecutableURL(CFBundleGetMainBundle())` names a path
    // inside the mirror rather than `rosemary`'s.
    sig!("CFBundleCopyExecutableURL", P, P),
    sig!("CFBundleCopyPrivateFrameworksURL", P, P),
    sig!("CFBundleCopyBundleLocalizations", P, P),
    sig!("CFBundleCopyPreferredLocalizationsFromArray", P, P),
    sig!("CFBundleCopyResourcesDirectoryURL", P, P),
    // `CFURLRef CFBundleCopyResourceURL(CFBundleRef, CFStringRef name,
    // CFStringRef type, CFStringRef subDirName)` - three `CFStringRef`s, any of
    // which may be null.
    sig!("CFBundleCopyResourceURL", P, P, P, P, P),
    sig!("CFBundleCopyLocalizedString", P, P, P, P, P),
    sig!("CFBundleCopyBundleURL", P, P),
    sig!("CFBundleCreate", P, P, P),
    sig!("CFBundleGetIdentifier", P, P),
    sig!("CFBundleGetInfoDictionary", P, P),
    sig!("CFBundleGetMainBundle", P),
    sig!("CFBundleLoadExecutable", U8, P),
    sig!("CFBundleGetFunctionPointerForName", P, P, P),
    sig!("CFBundleGetVersionNumber", U32, P),
    sig!("CFBundleGetValueForInfoDictionaryKey", P, P, P),
    sig!("CFStringGetCharactersPtr", P, P),
    //
    // `CFTypeID` is an `unsigned long`; each takes no argument and answers a
    // process-wide constant. A guest branching on `CFGetTypeID(x) ==
    // CFStringGetTypeID()` works only while both come from the host's own.
    sig!("CFBagGetTypeID", U64),
    sig!("CFBooleanGetTypeID", U64),
    sig!("CFDateGetTypeID", U64),
    sig!("CFNullGetTypeID", U64),
    sig!("CFSetGetTypeID", U64),
    sig!("CFURLGetTypeID", U64),
    sig!("DASessionCreate", P, P),
    sig!("DADiskCreateFromVolumePath", P, P, P, P),
    sig!("DADiskCopyDescription", P, P),
    sig!("CFURLEnumeratorCreateForMountedVolumes", P, P, U64, P),
    sig!("CFURLEnumeratorGetNextURL", I64, P, P, P),
    sig!("CFURLCreateFromFSRef", P, P, P),
    sig!("CFURLGetFSRef", BOOL, P, P),
    sig!("CFRunLoopTimerGetContext", V, P, P),
    sig!("CFURLCreateBookmarkData", P, P, P, U64, P, P, P),
    sig!(
        "CFURLCreateByResolvingBookmarkData",
        P,
        P,
        P,
        U64,
        P,
        P,
        P,
        P
    ),
    sig!("CFURLWriteBookmarkDataToFile", BOOL, P, P, U64, P),
    sig!("CFURLCreateBookmarkDataFromFile", P, P, P, P),
    sig!("CFUUIDGetTypeID", U64),
    sig!("CFBooleanGetValue", BOOL, P),
    sig!("CFDateCreate", P, P, F64),
    sig!("CFDateGetAbsoluteTime", F64, P),
    sig!("CFAllocatorCreate", P, P, P),
    sig!("CFAllocatorGetDefault", P),
    sig!("CFAllocatorGetTypeID", U64),
    // `CFDataRef CFDataCreate(CFAllocatorRef, const UInt8 *, CFIndex)` - the
    // bytes are read out of guest memory and copied into the host's object,
    // which is what makes the copy the right one.
    sig!("CFDataCreate", P, P, P, I64),
    sig!("CFDataCreateWithBytesNoCopy", P, P, P, I64, P),
    sig!("CFDataCreateCopy", P, P, P),
    sig!("CFDataGetBytePtr", P, P),
    sig!("CFDataGetMutableBytePtr", P, P),
    sig!("CFDataSetLength", V, P, I64),
    sig!("CFDataIncreaseLength", V, P, I64),
    sig!("CFDataAppendBytes", V, P, P, I64),
    // `CFRange` by value again, flattened to its two `CFIndex` fields.
    sig!("CFDataDeleteBytes", V, P, I64, I64),
    sig!("CFDataReplaceBytes", V, P, I64, I64, P, I64),
    sig!("CFDataGetLength", I64, P),
    sig!("CFDataGetTypeID", U64),
    sig!("CFDictionaryAddValue", V, P, P, P),
    cbsig!([1], "CFDictionaryApplyFunction", V, P, P, P),
    sig!("CFDictionaryContainsKey", BOOL, P, P),
    sig!("CFDictionaryGetCount", I64, P),
    sig!("CFDictionaryCreate", P, P, P, P, I64, P, P),
    sig!("CFDictionaryCreateMutable", P, P, I64, P, P),
    sig!("CFDictionaryCreateCopy", P, P, P),
    sig!("CFDictionaryCreateMutableCopy", P, P, I64, P),
    sig!("CFDictionaryGetCountOfKey", I64, P, P),
    sig!("CFDictionaryGetTypeID", U64),
    sig!("CFDictionaryGetValue", P, P, P),
    sig!("CFDictionaryGetValueIfPresent", BOOL, P, P, P),
    sig!("CFDictionarySetValue", V, P, P, P),
    sig!("CFEqual", BOOL, P, P),
    sig!("CFGetTypeID", U64, P),
    sig!("CFHTTPAuthenticationCopyMethod", P, P),
    sig!("CFHTTPAuthenticationCopyRealm", P, P),
    sig!("CFHTTPAuthenticationCreateFromResponse", P, P, P),
    sig!("CFHTTPAuthenticationIsValid", U8, P, P),
    sig!("CFHTTPAuthenticationRequiresAccountDomain", U8, P),
    sig!("CFHTTPAuthenticationRequiresUserNameAndPassword", U8, P),
    sig!("CFHTTPMessageApplyCredentialDictionary", U8, P, P, P, P),
    sig!("CFHTTPMessageCopyAllHeaderFields", P, P),
    sig!("CFHTTPMessageCopyResponseStatusLine", P, P),
    sig!("CFHTTPMessageCopyVersion", P, P),
    sig!("CFHTTPMessageCreateRequest", P, P, P, P, P),
    sig!("CFHTTPMessageGetResponseStatusCode", I64, P),
    sig!("CFHTTPMessageIsHeaderComplete", U8, P),
    sig!("CFHTTPMessageSetHeaderFieldValue", V, P, P, P),
    sig!("CFHash", U64, P),
    sig!("CFLocaleCopyCurrent", P),
    sig!("CFLocaleGetIdentifier", P, P),
    sig!("CFLocaleGetValue", P, P, P),
    sig!("CFNetworkCopySystemProxySettings", P),
    //
    // The callback is argument two and the host calls it on whatever thread
    // posts, so it goes out as a thunk. `CFNotificationSuspensionBehavior` is a
    // `CF_ENUM(CFIndex, ...)` - a doubleword, not the `uint32_t` it looks like.
    cbsig!(
        [2],
        "CFNotificationCenterAddObserver",
        V,
        P,
        P,
        P,
        P,
        P,
        I64
    ),
    sig!("CFNotificationCenterGetDarwinNotifyCenter", P),
    sig!("CFNotificationCenterGetDistributedCenter", P),
    sig!("CFNotificationCenterGetLocalCenter", P),
    sig!("CFNotificationCenterPostNotification", V, P, P, P, P, BOOL),
    sig!("CFNotificationCenterRemoveEveryObserver", V, P, P),
    sig!("CFNotificationCenterRemoveObserver", V, P, P, P, P),
    sig!("CFNetworkCopyProxiesForAutoConfigurationScript", P, P, P, P),
    sig!("CFNetworkCopyProxiesForURL", P, P, P),
    sig!("CFNumberCreate", P, P, I64, P),
    // `CFNumberType` is a `CFIndex` enum.
    sig!("CFNumberGetType", I64, P),
    sig!("CFNumberGetTypeID", U64),
    // `Boolean` is an `unsigned char`, not an `int`.
    sig!("CFNumberGetValue", U8, P, I64, P),
    //
    // The two `GetApp*Value` forms take a `Boolean *keyExistsAndHasValidFormat`
    // out-parameter, which the guest may pass as null and which the host writes
    // through directly.
    sig!("CFPreferencesCopyAppValue", P, P, P),
    sig!("CFPropertyListCreateWithData", P, P, P, U64, P, P),
    sig!("CFPreferencesCopyValue", P, P, P, P, P),
    sig!("CFPreferencesCopyKeyList", P, P, P, P),
    sig!("CFPreferencesCopyMultiple", P, P, P, P, P),
    sig!("CFPreferencesGetAppBooleanValue", U8, P, P, P),
    sig!("CFPreferencesGetAppIntegerValue", I64, P, P, P),
    sig!("CFReadStreamCopyError", P, P),
    sig!("CFReadStreamCopyProperty", P, P, P),
    sig!("CFReadStreamCreateForHTTPRequest", P, P, P),
    sig!("CFReadStreamCreateForStreamedHTTPRequest", P, P, P, P),
    sig!("CFReadStreamHasBytesAvailable", U8, P),
    sig!("CFReadStreamOpen", U8, P),
    // The client callback is guest code the host calls back on the run loop;
    // the fourth argument is a `CFStreamClientContext`, whose `retain`,
    // `release` and `copyDescription` fields are function pointers a guest
    // conventionally leaves null.
    cbsig!([2], "CFReadStreamSetClient", U8, P, U64, P, P),
    sig!("CFReadStreamSetProperty", U8, P, P, P),
    sig!("CFRelease", V, P),
    sig!("CFRetain", P, P),
    sig!("CFLocaleGetSystem", P),
    sig!("CFRunLoopAddObserver", V, P, P, P),
    sig!("CFRunLoopRemoveObserver", V, P, P, P),
    sig!("CFRunLoopObserverInvalidate", V, P),
    sig!("CFRunLoopAddSource", V, P, P, P),
    sig!("CFRunLoopGetCurrent", P),
    sig!("CFRunLoopGetMain", P),
    sig!("CFRunLoopGetNextTimerFireDate", F64, P, P),
    // `void CFRunLoopRun(void)` - no parameters.
    sig!("CFRunLoopRun", V),
    // `CFTimeInterval` is a `double`, so this is the first entry in the table
    // whose second argument is floating-point while its first and third are not
    // - exactly the shape the two independent register counters exist for.
    sig!("CFRunLoopRunInMode", I32, P, F64, U8),
    sig!("CFRunLoopSourceSignal", V, P),
    sig!("CFRunLoopSourceInvalidate", V, P),
    sig!("CFRunLoopSourceIsValid", U8, P),
    sig!("CFRunLoopRemoveSource", V, P, P, P),
    sig!("CFRunLoopContainsSource", U8, P, P, P),
    sig!("CFRunLoopAddTimer", V, P, P, P),
    sig!("CFRunLoopRemoveTimer", V, P, P, P),
    sig!("CFRunLoopTimerInvalidate", V, P),
    sig!("CFRunLoopTimerIsValid", U8, P),
    sig!("CFRunLoopStop", V, P),
    sig!("CFRunLoopWakeUp", V, P),
    cbsig!([3], "CFSocketCreateWithNative", P, P, I32, U64, P, P),
    sig!("CFSocketCreateRunLoopSource", P, P, P, I64),
    sig!("CFSocketGetNative", I32, P),
    sig!("CFSocketIsValid", U8, P),
    sig!("CFSocketGetSocketFlags", U64, P),
    sig!("CFSocketSetSocketFlags", V, P, U64),
    sig!("CFSocketEnableCallBacks", V, P, U64),
    sig!("CFSocketDisableCallBacks", V, P, U64),
    sig!("CFSocketInvalidate", V, P),
    // `CFAbsoluteTime` is a `double`, and this is where a timer's fire date
    // comes from.
    sig!("CFAbsoluteTimeGetCurrent", F64),
    //
    // `CFAbsoluteTime` is a `double` of seconds, and the two that take or
    // return a `CFGregorianDate` by value are refused below rather than
    // described here.
    sig!("CFCalendarCreateWithIdentifier", P, P, P),
    sig!("CFCalendarGetIdentifier", P, P),
    sig!("CFCalendarCopyTimeZone", P, P),
    sig!("CFCalendarSetTimeZone", V, P, P),
    // `Boolean CFCalendarDecomposeAbsoluteTime(CFCalendarRef, CFAbsoluteTime,
    // const char *, ...)` and its inverse, whose variadic half is one `int *`
    // or one `int` per component the description names.
    vsig!("CFCalendarDecomposeAbsoluteTime", BOOL, P, F64, P),
    vsig!("CFCalendarComposeAbsoluteTime", BOOL, P, P, P),
    sig!("CFAbsoluteTimeGetDayOfWeek", I32, F64, P),
    sig!("CFAbsoluteTimeGetDayOfYear", I32, F64, P),
    // `CFSetApplierFunction(const void *value, void *context)`, called once
    // per member before the call returns - so unlike most callbacks here it is
    // reached on the guest's own thread, and unlike most it must still go out
    // as a thunk, because CoreFoundation is what calls it.
    cbsig!([1], "CFSetApplyFunction", V, P, P, P),
    sig!("CFSetGetCount", I64, P),
    sig!("CFSetGetValues", V, P, P),
    sig!("CFStringAppendCharacters", V, P, P, I64),
    // `CFComparisonResult` is a `CFIndex`, so a signed doubleword, and
    // `CFStringCompareFlags` is a `CFOptionFlags` - both 64-bit on LP64.
    sig!("CFStringCompare", I64, P, P, U64),
    sig!("CFCopyDescription", P, P),
    sig!("CFCopyTypeIDDescription", P, U64),
    sig!("CFStringCompareWithOptions", I64, P, P, I64, I64, U64),
    // `Boolean CFStringFindWithOptions(CFStringRef, CFStringRef, CFRange,
    // CFStringCompareFlags, CFRange *)`, with the range flattened.
    sig!("CFStringFindWithOptions", BOOL, P, P, I64, I64, U64, P),
    sig!("CFStringConvertEncodingToIANACharSetName", P, U32),
    sig!("CFStringConvertEncodingToNSStringEncoding", U64, U32),
    sig!("CFStringConvertIANACharSetNameToEncoding", U32, P),
    sig!("CFStringConvertNSStringEncodingToEncoding", U32, U64),
    sig!("CFStringConvertWindowsCodepageToEncoding", U32, U32),
    sig!("CFStringCreateWithCharacters", P, P, P, I64),
    sig!("CFStringCreateWithCharactersNoCopy", P, P, P, I64, P),
    sig!("CFStringCreateWithCString", P, P, P, U32),
    // `CFStringRef CFStringCreateWithBytes(CFAllocatorRef, const UInt8 *bytes,
    // CFIndex numBytes, CFStringEncoding, Boolean isExternalRepresentation)`.
    sig!("CFStringCreateWithBytes", P, P, P, I64, U32, BOOL),
    //
    // A `CFRange` is two `CFIndex` - sixteen bytes, both integer - and SysV
    // AMD64 and AAPCS64 agree on passing one in two consecutive integer
    // registers.
    sig!("CFStringGetBytes", I64, P, I64, I64, U32, U8, U8, P, I64, P),
    sig!("CFStringCreateExternalRepresentation", P, P, P, U32, U8),
    // A `UniChar` is a `UInt16`.
    sig!("CFStringGetCharacterAtIndex", U16, P, I64),
    // `Boolean CFStringGetCString(CFStringRef, char *buffer, CFIndex
    // bufferSize, CFStringEncoding encoding)` - **four** parameters.
    sig!("CFStringCreateMutableCopy", P, P, I64, P),
    sig!("CFStringCreateWithFileSystemRepresentation", P, P, P),
    sig!("CFStringCreateWithSubstring", P, P, P, I64, I64),
    // `void CFStringUppercase(CFMutableStringRef, CFLocaleRef)`, and its
    // mirror. The locale is the second argument and is conventionally null.
    sig!("CFStringLowercase", V, P, P),
    sig!("CFStringUppercase", V, P, P),
    // `void CFStringGetCharacters(CFStringRef, CFRange, UniChar *)` - the range
    // flattened into two words, as `CFStringGetBytes` above has it.
    sig!("CFStringGetCharacters", V, P, I64, I64, P),
    sig!("CFTimeZoneCopyDefault", P),
    sig!("CFTimeZoneCopySystem", P),
    sig!("CFTimeZoneCreateWithName", P, P, P, U8),
    sig!("CFTimeZoneGetName", P, P),
    sig!("CFStringGetCString", BOOL, P, P, I64, U32),
    sig!("CFStringGetCStringPtr", P, P, U32),
    sig!("CFStringGetLength", I64, P),
    sig!("CFStringGetIntValue", I32, P),
    sig!("CFStringGetDoubleValue", F64, P),
    sig!("CFStringAppend", V, P, P),
    sig!("CFStringInsert", V, P, I64, P),
    sig!("CFStringDelete", V, P, I64, I64),
    // `CFRange` by value, flattened, as `CFStringGetBytes` settled.
    sig!("CFStringReplace", V, P, I64, I64, P),
    sig!("CFStringReplaceAll", V, P, P),
    // `CFIndex CFStringFindAndReplace(CFMutableStringRef, CFStringRef,
    // CFStringRef, CFRange, CFStringCompareFlags)`, whose flags are a `UInt32`.
    sig!("CFStringFindAndReplace", I64, P, P, P, I64, I64, U32),
    sig!("CFStringCreateWithBytesNoCopy", P, P, P, I64, U32, BOOL, P),
    sig!("CFStringGetFileSystemRepresentation", BOOL, P, P, I64),
    sig!("CFStringGetMaximumSizeOfFileSystemRepresentation", I64, P),
    sig!("CFStringGetMaximumSizeForEncoding", I64, I64, U32),
    sig!("CFStringGetSystemEncoding", U32),
    sig!("CFStringGetTypeID", U64),
    sig!("CFStringHasPrefix", BOOL, P, P),
    sig!("CFStringHasSuffix", BOOL, P, P),
    sig!(
        "CFURLCreateStringByAddingPercentEscapes",
        P,
        P,
        P,
        P,
        P,
        U32
    ),
    sig!("CFURLCreateStringByReplacingPercentEscapes", P, P, P, P),
    sig!(
        "CFURLCreateStringByReplacingPercentEscapesUsingEncoding",
        P,
        P,
        P,
        P,
        U32
    ),
    // `CFURLPathStyle` is a `CF_ENUM(CFIndex, ...)`, so the style is a
    // doubleword and not the `uint32_t` the neighbouring encoding arguments
    // are - the two look interchangeable in a header and are not.
    sig!("CFURLCopyFileSystemPath", P, P, I64),
    sig!("CFURLCreateWithBytes", P, P, P, I64, U32, P),
    sig!(
        "CFURLCreateWithFileSystemPathRelativeToBase",
        P,
        P,
        P,
        I64,
        BOOL,
        P
    ),
    sig!("CFURLCreateWithString", P, P, P, P),
    sig!("CFURLGetString", P, P),
    sig!("CFURLCopyAbsoluteURL", P, P),
    sig!("CFURLCopyPath", P, P),
    sig!("CFURLCopyLastPathComponent", P, P),
    sig!("CFURLCopyScheme", P, P),
    sig!("CFURLCopyHostName", P, P),
    sig!("CFURLGetPortNumber", I32, P),
    sig!("CFURLHasDirectoryPath", BOOL, P),
    // A `CFURLPathStyle` is a `CFIndex`, not an `int`, and `isDirectory` is a
    // `Boolean` - the two narrow types this family keeps getting wrong in the
    // direction that does not fault.
    sig!("CFURLCreateWithFileSystemPath", P, P, P, I64, BOOL),
    sig!("CFURLCreateCopyAppendingPathComponent", P, P, P, P, BOOL),
    sig!("CFURLCreateCopyDeletingLastPathComponent", P, P, P),
    sig!("CFURLCreateCopyDeletingPathExtension", P, P, P),
    sig!("CFURLGetFileSystemRepresentation", U8, P, U8, P, I64),
    sig!("CFUUIDCreate", P, P),
    sig!("CFUUIDCreateString", P, P, P),
    //
    // Everything SDL3's Cocoa backend asks about a screen: which displays
    // exist, what modes each has, and which one is in use.
    sig!("CGAcquireDisplayFadeReservation", I32, F32, P),
    sig!("CGDirectDisplayCopyCurrentMetalDevice", P, I32),
    sig!("MTLCreateSystemDefaultDevice", P),
    sig!("CGAssociateMouseAndMouseCursorPosition", I32, I32),
    // `boolean_t CGCursorIsVisible(void)` - an `int`, not a `CGError`, and the
    // pair to `CGDisplayHideCursor`.
    sig!("CGCursorIsVisible", U32),
    //
    // cocos2d-x decodes every texture the same way: a `CGImageSource` over
    // the file's bytes produces a `CGImage`, a bitmap context is created over
    // a buffer the *guest* allocated, and the image is drawn into it.
    sig!("CGBitmapContextCreate", P, P, U64, U64, U64, U64, P, U32),
    //
    // The whole-desktop forms take nothing and the per-display ones a
    // `CGDirectDisplayID`; every one returns a `CGError`.
    sig!("CGCaptureAllDisplays", I32),
    sig!("CGReleaseAllDisplays", I32),
    sig!("CGDisplayCapture", I32, U32),
    sig!("CGDisplayRelease", I32, U32),
    sig!("CGColorSpaceCreateDeviceRGB", P),
    sig!("CGColorSpaceRelease", V, P),
    // `CGFloat` is `double` on both architectures, so the CTM translation is
    // two ordinary vector arguments.
    sig!("CGContextRelease", V, P),
    sig!("CGContextTranslateCTM", V, P, F64, F64),
    sig!("CGContextScaleCTM", V, P, F64, F64),
    sig!("CGContextSetTextDrawingMode", V, P, I32),
    sig!("CGContextSetGrayFillColor", V, P, F64, F64),
    sig!("CGContextSetShouldSmoothFonts", V, P, BOOL),
    sig!("CGDisplayCopyAllDisplayModes", P, U32, P),
    sig!("CGDisplayCopyDisplayMode", P, U32),
    // Seven floats and a flag, and the `boolean_t` at the end is the argument
    // that makes the count worth checking.
    sig!("CGDisplayFade", I32, U32, F32, F32, F32, F32, F32, F32, I32),
    // Both take a `CGDirectDisplayID`, and both are reference-counted rather
    // than absolute: a game hiding the cursor for a level must show it again.
    sig!("CGDisplayHideCursor", I32, U32),
    sig!("CGDisplayIDToOpenGLDisplayMask", U32, U32),
    sig!("CGDisplayIOServicePort", U32, U32),
    sig!("CGDisplayIsMain", I32, U32),
    sig!("CGDisplayIsBuiltin", I32, U32),
    sig!("CGDisplayIsOnline", I32, U32),
    sig!("CGDisplayIsAsleep", I32, U32),
    sig!("CGDisplaySerialNumber", U32, U32),
    sig!("CGDisplayRegisterReconfigurationCallback", I32, P, P),
    sig!("CGDisplayRemoveReconfigurationCallback", I32, P, P),
    sig!("CGDisplayPrimaryDisplay", U32, U32),
    sig!("CGDisplayRelease", I32, U32),
    sig!("CGDisplayRotation", F64, U32),
    sig!("CGDisplayMirrorsDisplay", U32, U32),
    sig!("CGDisplayModeCopyPixelEncoding", P, P),
    sig!("CGDisplayModeGetHeight", U64, P),
    sig!("CGDisplayModeGetIOFlags", U32, P),
    sig!("CGDisplayModeGetPixelHeight", U64, P),
    sig!("CGDisplayModeGetPixelWidth", U64, P),
    sig!("CGDisplayModeGetRefreshRate", F64, P),
    sig!("CGDisplayModeGetWidth", U64, P),
    sig!("CGDisplayModeIsUsableForDesktopGUI", BOOL, P),
    sig!("CGDisplayModeRelease", V, P),
    sig!("CGDisplayModeRetain", P, P),
    // `CGPoint` by value, which is the [`CFRange`] case rather than the
    // `NSRect` one.
    sig!("CGDisplayMoveCursorToPoint", I32, U32, F64, F64),
    sig!("CGDisplayPixelsHigh", U64, U32),
    sig!("CGDisplayPixelsWide", U64, U32),
    // The host calls these back on its own thread when a display is
    // reconfigured, so the first argument leaves as a thunk like every other
    // guest function pointer crossing the seam.
    cbsig!([0], "CGDisplayRegisterReconfigurationCallback", I32, P, P),
    cbsig!([0], "CGDisplayRemoveReconfigurationCallback", I32, P, P),
    sig!("CGDisplaySetDisplayMode", I32, U32, P, P),
    sig!("CGDisplayShowCursor", I32, U32),
    // `CGEventCreate(CGEventSourceRef)` - a null source means the event
    // describes the current input state, which is how a game asks where the
    // pointer is without a window under it.
    sig!("CGEventCreate", P, P),
    sig!("CGEventPost", V, U32, P),
    sig!("CGEventSetType", V, P, U32),
    // The gamma ramp, read and written.
    sig!("CGGetDisplayTransferByTable", I32, U32, U32, P, P, P, P),
    sig!("CGSetDisplayTransferByTable", I32, U32, U32, P, P, P),
    sig!("CGGetOnlineDisplayList", I32, U32, P, P),
    // `void CGGetLastMouseDelta(int32_t *dx, int32_t *dy)` - how a game reads
    // relative mouse motion when the cursor is hidden and locked, which is
    // every first-person camera.
    sig!("CGGetLastMouseDelta", V, P, P),
    // `CGEventFlags` is a `uint64_t` of modifier bits; the source state id is
    // an enum.
    sig!("CGEventSourceFlagsState", U64, U32),
    sig!("CGDisplayModelNumber", U32, U32),
    sig!("CGDisplayVendorNumber", U32, U32),
    sig!("CGDisplayRotation", F64, U32),
    sig!("CGColorSpaceCreateWithName", P, P),
    sig!("CGColorSpaceGetName", P, P),
    sig!("CGColorSpaceCopyName", P, P),
    sig!("CGColorSpaceCopyPropertyList", P, P),
    sig!("CGGetActiveDisplayList", I32, U32, P, P),
    //
    // `NSOpenGLContext` is a thin wrapper, and a Unity player written before
    // Metal talks to the layer below it directly.
    sig!("CGLChoosePixelFormat", I32, P, P, P),
    sig!("CGLClearDrawable", I32, P),
    sig!("CGLCreateContext", I32, P, P, P),
    sig!("CGLDescribePixelFormat", I32, P, I32, U32, P),
    sig!("CGLDescribeRenderer", I32, P, I32, U32, P),
    sig!("CGLDestroyContext", I32, P),
    sig!("CGLGetVersion", I32, P, P),
    sig!("CGLDestroyPixelFormat", I32, P),
    sig!("CGLDestroyRendererInfo", I32, P),
    sig!("CGLDisable", I32, P, U32),
    sig!("CGLEnable", I32, P, U32),
    sig!("CGLErrorString", P, I32),
    sig!("CGLFlushDrawable", I32, P),
    sig!("CGLGetCurrentContext", P),
    sig!("CGLGetParameter", I32, P, U32, P),
    sig!("CGLGetPixelFormat", P, P),
    sig!("CGLGetShareGroup", P, P),
    sig!("CGLGetVirtualScreen", I32, P, P),
    sig!("CGLLockContext", I32, P),
    sig!("CGLQueryRendererInfo", I32, U32, P, P),
    sig!("CGLReleaseContext", V, P),
    sig!("CGLReleasePixelFormat", V, P),
    sig!("CGLRetainContext", P, P),
    sig!("CGLRetainPixelFormat", P, P),
    sig!("CGLSetCurrentContext", I32, P),
    sig!("CGLSetParameter", I32, P, U32, P),
    sig!("CGLSetVirtualScreen", I32, P, I32),
    sig!("CGLUnlockContext", I32, P),
    sig!("CGLUpdateContext", I32, P),
    //
    // `CGImageGetWidth`/`Height`/`BitsPerComponent` are `size_t`; a
    // `CGImageAlphaInfo` is a `uint32_t` enum; the index into a source is a
    // `size_t`.
    sig!("CGDataProviderCreateWithData", P, P, P, U64, P),
    sig!("CGDataProviderRelease", V, P),
    // `CGImageCreate(size_t width, size_t height, size_t bitsPerComponent,
    // size_t bitsPerPixel, size_t bytesPerRow, CGColorSpaceRef,
    // CGBitmapInfo, CGDataProviderRef, const CGFloat *decode,
    // bool shouldInterpolate, CGColorRenderingIntent)`.
    sig!(
        "CGImageCreate",
        P,
        U64,
        U64,
        U64,
        U64,
        U64,
        P,
        U32,
        P,
        P,
        U8,
        I32
    ),
    sig!("CGImageCreateCopyWithColorSpace", P, P, P),
    sig!("CGImageRetain", P, P),
    sig!("CGImageRelease", V, P),
    sig!("CGImageGetAlphaInfo", U32, P),
    sig!("CGImageGetBitsPerComponent", U64, P),
    sig!("CGImageGetColorSpace", P, P),
    sig!("CGImageGetHeight", U64, P),
    sig!("CGImageGetWidth", U64, P),
    sig!("CGImageSourceCreateImageAtIndex", P, P, U64, P),
    sig!("CGImageSourceCreateWithData", P, P, P),
    sig!("CGMainDisplayID", U32),
    sig!("CGDisplayRestoreColorSyncSettings", V),
    sig!("NXGetLocalArchInfo", P),
    sig!("NSDisableScreenUpdates", V),
    sig!("NSEnableScreenUpdates", V),
    sig!("CGSessionCopyCurrentDictionary", P),
    sig!("CGWindowServerCreateServerPort", U32),
    sig!("CGReleaseDisplayFadeReservation", I32, U32),
    // A `CGWindowLevel` and a `CGWindowLevelKey` are both `int32_t`.
    sig!("CGShieldingWindowLevel", I32),
    // `CGPoint` by value again; see `CGDisplayMoveCursorToPoint`.
    sig!("CGWarpMouseCursorPosition", I32, F64, F64),
    sig!("CGSetLocalEventsSuppressionInterval", I32, F64),
    sig!("CGWindowLevelForKey", I32, I32),
    sig!("CGColorGetConstantColor", P, P),
    sig!("CGColorCreateCopy", P, P),
    sig!("CGColorRelease", V, P),
    sig!("CGColorRetain", P, P),
    sig!("NXGetArchInfoFromCpuType", P, I32, I32),
    sig!("SCDynamicStoreCopyMultiple", P, P, P, P),
    sig!("SCDynamicStoreCopyKeyList", P, P, P),
    sig!("SCDynamicStoreGetTypeID", U64),
    sig!("devname", P, I32, I32),
    cbsig!([3], "SCDynamicStoreCreateWithOptions", P, P, P, P, P, P),
    sig!("SCDynamicStoreCreateRunLoopSource", P, P, P, I64),
    sig!("SCDynamicStoreSetDispatchQueue", BOOL, P, P),
    sig!("SCDynamicStoreSetNotificationKeys", BOOL, P, P, P),
    sig!("SCDynamicStoreCopyComputerName", P, P, P),
    sig!("SCDynamicStoreCopyLocalHostName", P, P),
    sig!("SCDynamicStoreCopyLocation", P, P),
    sig!("SCDynamicStoreCopyConsoleUser", P, P, P, P),
    sig!("SCDynamicStoreKeyCreateComputerName", P, P),
    sig!("SCDynamicStoreKeyCreateConsoleUser", P, P),
    sig!("SCDynamicStoreKeyCreateHostNames", P, P),
    sig!("SCDynamicStoreKeyCreateNetworkGlobalEntity", P, P, P, P),
    sig!("SCError", I32),
    sig!("SCErrorString", P, I32),
    sig!("SCCopyLastError", P),
    //
    // The display-link callback arrives on CoreVideo's own thread, which this
    // runtime never created, so it reaches the guest the way a `pthread_create`
    // start routine does: through a thunk, on a guest stack made on demand.
    // `CACurrentMediaTime` is `mach_absolute_time` in seconds, as a `double`.
    sig!("CACurrentMediaTime", F64),
    //
    // The rumble half of the HID surface, which a game reaches once it has
    // found a gamepad through IOKit.
    sig!("FFIsForceFeedback", I32, U32),
    sig!("FFCreateDevice", I32, U32, P),
    sig!("FFReleaseDevice", I32, P),
    sig!("FFDeviceCreateEffect", I32, P, P, P, P),
    sig!("FFDeviceReleaseEffect", I32, P, P),
    sig!("FFDeviceGetForceFeedbackCapabilities", I32, P, P),
    sig!("FFDeviceGetForceFeedbackProperty", I32, P, U32, P, U64),
    sig!("FFDeviceSetForceFeedbackProperty", I32, P, U32, P),
    sig!("FFDeviceSendForceFeedbackCommand", I32, P, U32),
    sig!("FFEffectGetEffectStatus", I32, P, P),
    sig!("FFEffectSetParameters", I32, P, P, U32),
    sig!("FFEffectStart", I32, P, U32, U32),
    sig!("FFEffectStop", I32, P),
    sig!("FFEffectDownload", I32, P),
    sig!("FFEffectUnload", I32, P),
    sig!("FFDeviceGetForceFeedbackState", I32, P, P),
    sig!("CVDisplayLinkCreateWithActiveCGDisplays", I32, P),
    sig!("CVDisplayLinkCreateWithCGDisplay", I32, U32, P),
    sig!("CVDisplayLinkGetActualOutputVideoRefreshPeriod", F64, P),
    // `CVTime` by value: sixteen bytes, both eightbytes integer, and the two
    // ABIs *agree* - `rax`/`rdx` on one side, `x0`/`x1` on the other.
    sig!("CVDisplayLinkGetNominalOutputVideoRefreshPeriod", V, P),
    sig!("CVDisplayLinkRelease", V, P),
    sig!("CVDisplayLinkRetain", P, P),
    sig!("CVDisplayLinkSetCurrentCGDisplay", I32, P, U32),
    sig!(
        "CVDisplayLinkSetCurrentCGDisplayFromOpenGLContext",
        I32,
        P,
        P,
        P
    ),
    cbsig!([1], "CVDisplayLinkSetOutputCallback", I32, P, P, P),
    sig!("CVDisplayLinkStart", I32, P),
    sig!("CVDisplayLinkStop", I32, P),
    //
    // IOSurface. `IOSurfaceID`, `OSType` and `IOSurfaceLockOptions` are
    // `uint32_t`; sizes are `size_t`; `IOReturn` is an `int`.
    sig!("IOSurfaceCreate", P, P),
    sig!("IOSurfaceLookup", P, U32),
    sig!("IOSurfaceGetID", U32, P),
    sig!("IOSurfaceGetTypeID", U64),
    sig!("IOSurfaceLock", I32, P, U32, P),
    sig!("IOSurfaceUnlock", I32, P, U32, P),
    sig!("IOSurfaceGetSeed", U32, P),
    sig!("IOSurfaceGetAllocSize", U64, P),
    sig!("IOSurfaceGetWidth", U64, P),
    sig!("IOSurfaceGetHeight", U64, P),
    sig!("IOSurfaceGetBytesPerElement", U64, P),
    sig!("IOSurfaceGetBytesPerRow", U64, P),
    sig!("IOSurfaceGetElementWidth", U64, P),
    sig!("IOSurfaceGetElementHeight", U64, P),
    sig!("IOSurfaceGetPixelFormat", U32, P),
    sig!("IOSurfaceGetBaseAddress", P, P),
    sig!("IOSurfaceIncrementUseCount", V, P),
    sig!("IOSurfaceDecrementUseCount", V, P),
    sig!("IOSurfaceGetUseCount", I32, P),
    sig!("IOSurfaceIsInUse", BOOL, P),
    sig!("IOSurfaceSetValue", V, P, P, P),
    sig!("IOSurfaceCopyValue", P, P, P),
    sig!("IOSurfaceRemoveValue", V, P, P),
    sig!("IOSurfaceSetValues", V, P, P),
    sig!("IOSurfaceCopyAllValues", P, P),
    sig!("IOSurfaceRemoveAllValues", V, P),
    sig!("IOSurfaceAlignProperty", U64, P, U64),
    sig!("IOSurfaceGetPropertyAlignment", U64, P),
    sig!("IOSurfaceGetPropertyMaximum", U64, P),
    //
    // How a Unity player gets a decoded video frame onto the GPU: lock the
    // buffer, hand it to the cache, get a texture name back.
    sig!("CVBufferRelease", V, P),
    sig!("CVBufferRetain", P, P),
    sig!("CVOpenGLTextureCacheCreate", I32, P, P, P, P, P, P),
    sig!(
        "CVOpenGLTextureCacheCreateTextureFromImage",
        I32,
        P,
        P,
        P,
        P,
        P
    ),
    sig!("CVOpenGLTextureCacheFlush", V, P, U64),
    sig!("CVOpenGLTextureCacheRelease", V, P),
    sig!("CVOpenGLTextureGetName", U32, P),
    sig!("CVOpenGLTextureGetTarget", U32, P),
    sig!("CVPixelBufferGetBaseAddress", P, P),
    sig!("CVPixelBufferGetBytesPerRow", U64, P),
    sig!("CVPixelBufferGetHeight", U64, P),
    sig!("CVPixelBufferGetWidth", U64, P),
    sig!("CVPixelBufferLockBaseAddress", I32, P, U64),
    sig!("CVPixelBufferUnlockBaseAddress", I32, P, U64),
    //
    // Removed from the SDK's headers years ago and still exported by
    // CoreServices, which is why these signatures are written from the last
    // published declarations rather than read out of a header.
    sig!("CloseComponent", I32, P),
    sig!("FindNextComponent", P, P, P),
    sig!("Gestalt", I32, U32, P),
    //
    // Every handle here - `mach_port_t`, `io_object_t`, `io_iterator_t`,
    // `io_registry_entry_t` - is a 32-bit port name, not a pointer.
    sig!("IOBSDNameMatching", P, U32, U32, P),
    // `CFDictionaryRef IODisplayCreateInfoDictionary(io_service_t,
    // IOOptionBits)` - how SDL asks a framebuffer service for its display's
    // localized name.
    sig!("IODisplayCreateInfoDictionary", P, U32, U32),
    sig!("IOHIDDeviceCopyMatchingElements", P, P, P, U32),
    // `IOReturn IOHIDDeviceOpen(IOHIDDeviceRef, IOOptionBits)`, and its close.
    sig!("IOHIDDeviceOpen", I32, P, U32),
    sig!("IOHIDDeviceClose", I32, P, U32),
    sig!("IOHIDDeviceGetTypeID", U64),
    sig!("IOHIDDeviceGetProperty", P, P, P),
    // `IOHIDDeviceGetService` hands back the `io_service_t` behind a device,
    // which is the 32-bit port name the registry calls are asked in terms of -
    // the one place in this family where a handle *becomes* a port name.
    sig!("IOHIDDeviceGetService", U32, P),
    // `IOReturn IOHIDDeviceGetValue(IOHIDDeviceRef, IOHIDElementRef,
    // IOHIDValueRef *)` - the polled counterpart to the value callback below.
    sig!("IOHIDDeviceGetValue", I32, P, P, P),
    // The host calls the callback with `(context, IOReturn, sender, value)`.
    cbsig!([1], "IOHIDDeviceRegisterInputValueCallback", V, P, P, P),
    // Removal is `IOHIDCallback(void *context, IOReturn, void *sender)`: the
    // guest's function pointer leaves as a thunk like every other.
    cbsig!([1], "IOHIDDeviceRegisterRemovalCallback", V, P, P, P),
    // Both take the run loop and a mode string, and both are `void`.
    sig!("IOHIDDeviceScheduleWithRunLoop", V, P, P, P),
    sig!("IOHIDDeviceUnscheduleFromRunLoop", V, P, P, P),
    // `IOHIDReportCallback(void *context, IOReturn, void *sender,
    // IOHIDReportType, uint32_t reportID, uint8_t *report, CFIndex length)`.
    cbsig!(
        [3],
        "IOHIDDeviceRegisterInputReportCallback",
        V,
        P,
        P,
        I64,
        P,
        P
    ),
    sig!("IOHIDDeviceSetProperty", BOOL, P, P, P),
    sig!("IOHIDDeviceSetReport", I32, P, U32, I64, P, I64),
    sig!("IOHIDElementGetChildren", P, P),
    sig!("IOHIDElementGetDevice", P, P),
    sig!("IOHIDElementGetCookie", U32, P),
    // `CFIndex` is a `long`, so these are sixty-four bits wide and signed - a
    // logical minimum is routinely negative.
    sig!("IOHIDElementGetCollectionType", U32, P),
    sig!("IOHIDElementGetName", P, P),
    sig!("IOHIDElementGetParent", P, P),
    sig!("IOHIDElementGetPhysicalMax", I64, P),
    sig!("IOHIDElementGetPhysicalMin", I64, P),
    sig!("IOHIDElementGetUnit", U32, P),
    sig!("IOHIDElementGetUnitExponent", U32, P),
    sig!("IOHIDElementHasNullState", BOOL, P),
    sig!("IOHIDElementHasPreferredState", BOOL, P),
    sig!("IOHIDElementIsArray", BOOL, P),
    sig!("IOHIDElementIsNonLinear", BOOL, P),
    sig!("IOHIDElementIsRelative", BOOL, P),
    sig!("IOHIDElementIsVirtual", BOOL, P),
    sig!("IOHIDElementIsWrapping", BOOL, P),
    sig!("IOHIDElementGetLogicalMax", I64, P),
    sig!("IOHIDElementGetLogicalMin", I64, P),
    sig!("IOHIDElementGetType", U32, P),
    // A `CFTypeID` is an `unsigned long`, so sixty-four bits, and this takes
    // nothing.
    sig!("IOHIDElementGetTypeID", U64),
    sig!("IOHIDElementGetUsage", U32, P),
    sig!("IOHIDElementGetUsagePage", U32, P),
    sig!("IOHIDManagerClose", I32, P, U32),
    sig!("IOHIDManagerGetTypeID", U64),
    // `IOHIDManagerRef IOHIDManagerCreate(CFAllocatorRef, IOOptionBits)` -
    // a null allocator is the default one, which is what a game passes.
    sig!("IOHIDManagerCopyDevices", P, P),
    sig!("IOHIDManagerCreate", P, P, U32),
    sig!("IOHIDManagerOpen", I32, P, U32),
    cbsig!(
        [1],
        "IOHIDManagerRegisterDeviceMatchingCallback",
        V,
        P,
        P,
        P
    ),
    cbsig!([1], "IOHIDManagerRegisterDeviceRemovalCallback", V, P, P, P),
    // `IOHIDValueCallback(void *context, IOReturn, void *sender,
    // IOHIDValueRef)` - four integer arguments and no result, delivered on
    // whichever run loop the manager was scheduled on.
    cbsig!([1], "IOHIDManagerRegisterInputValueCallback", V, P, P, P),
    sig!("IOHIDManagerSetDeviceMatching", V, P, P),
    sig!("IONotificationPortCreate", P, I32),
    sig!("IONotificationPortGetMachPort", I32, P),
    sig!("IOHIDManagerScheduleWithRunLoop", V, P, P, P),
    sig!("IOHIDManagerSetDeviceMatchingMultiple", V, P, P),
    sig!("IOHIDManagerUnscheduleFromRunLoop", V, P, P, P),
    // The queue is the pull half of HID: a device's elements are added to it
    // and read back one value at a time.
    sig!("IOHIDQueueAddElement", V, P, P),
    sig!("IOHIDQueueContainsElement", BOOL, P, P),
    sig!("IOHIDQueueCopyNextValueWithTimeout", P, P, F64),
    sig!("IOHIDQueueCopyNextValue", P, P),
    sig!("IOHIDQueueCreate", P, P, P, I64, U32),
    sig!("IOHIDQueueRemoveElement", V, P, P),
    sig!("IOHIDQueueStart", V, P),
    sig!("IOHIDQueueStop", V, P),
    sig!("IOHIDValueGetElement", P, P),
    sig!("IOHIDValueGetIntegerValue", I64, P),
    sig!("IOHIDValueGetLength", I64, P),
    // `double IOHIDValueGetScaledValue(IOHIDValueRef, IOHIDValueScaleType)`,
    // whose scale type is a `uint32_t`.
    sig!("IOHIDValueGetScaledValue", F64, P, U32),
    sig!("IOHIDValueGetTypeID", U64),
    sig!("IOIteratorIsValid", I32, U32),
    sig!("IOIteratorNext", U32, U32),
    sig!("IOMasterPort", I32, U32, P),
    sig!("IOMainPort", I32, U32, P),
    sig!("IOIteratorReset", V, U32),
    sig!("IOObjectIsEqualTo", I32, U32, U32),
    sig!("IOObjectRelease", I32, U32),
    sig!("IOObjectRetain", I32, U32),
    //
    // An `IOPMAssertionID` is a `uint32_t`; the timeout in the middle of the
    // creation call is a `CFTimeInterval`, a `double`, and it is the only
    // floating-point argument in IOKit's whole surface here.
    sig!(
        "IOPMAssertionCreateWithDescription",
        I32,
        P,
        P,
        P,
        P,
        P,
        F64,
        P,
        P
    ),
    sig!("IOPMAssertionRelease", I32, U32),
    // The sleep/wake notification family is the *other* power API, and it is
    // the one an application registers with in order to be told before the
    // machine sleeps.
    cbsig!([2], "IORegisterForSystemPower", U32, P, P, P, P),
    sig!("IODeregisterForSystemPower", I32, P),
    sig!("IOAllowPowerChange", I32, U32, I64),
    sig!("IOCancelPowerChange", I32, U32, I64),
    // `IONotificationPortRef` is opaque, and the run-loop source it vends is
    // what puts the notification on the application's own run loop.
    sig!("IONotificationPortGetRunLoopSource", P, P),
    sig!("IONotificationPortDestroy", V, P),
    // The power-source family is pure CoreFoundation: an opaque blob, a list
    // over it, and a dictionary per source. Every one is a `CFTypeRef`.
    sig!("IOPSCopyPowerSourcesInfo", P),
    sig!("IOPSCopyPowerSourcesList", P, P),
    sig!("IOPSGetPowerSourceDescription", P, P, P),
    sig!("IOPSCopyExternalPowerAdapterDetails", P),
    // `CFRunLoopSourceRef IOPSNotificationCreateRunLoopSource(
    // IOPowerSourceCallbackType callback, void *context)` - the callback is
    // guest code the host invokes from the run loop when the battery changes,
    // so it is substituted like any other.
    cbsig!([0], "IOPSNotificationCreateRunLoopSource", P, P, P),
    sig!("IORegistryEntryCreateCFProperties", I32, U32, P, P, U32),
    sig!("IORegistryEntryCreateCFProperty", P, U32, P, P, U32),
    // An iterator over the registry, and the two ways of naming a starting
    // point.
    sig!("IORegistryEntryCreateIterator", I32, U32, P, U32, P),
    sig!("IORegistryEntryFromPath", U32, U32, P),
    sig!("IORegistryEntrySearchCFProperty", P, U32, P, P, P, U32),
    sig!("IORegistryEntryGetParentEntry", I32, U32, P, P),
    // `kern_return_t IOServiceAddMatchingNotification(IONotificationPortRef,
    // const io_name_t, CFDictionaryRef, IOServiceMatchingCallback, void *refCon,
    // io_iterator_t *)`. The callback is `void (*)(void *, io_iterator_t)`.
    cbsig!(
        [3],
        "IOServiceAddMatchingNotification",
        I32,
        P,
        P,
        P,
        P,
        P,
        P
    ),
    sig!("IOServiceGetMatchingServices", I32, U32, P, P),
    sig!("IOServiceMatching", P, P),
    sig!("KextManagerCopyLoadedKextInfo", P, P, P),
    sig!("clGetPlatformIDs", I32, U32, P, P),
    sig!("clGetPlatformInfo", I32, P, U32, U64, P, P),
    sig!("clGetDeviceIDs", I32, P, U64, U32, P, P),
    sig!("clGetDeviceInfo", I32, P, U32, U64, P, P),
    sig!("IOServiceNameMatching", P, P),
    sig!("IOObjectConformsTo", U32, U32, P),
    sig!("IOCreatePlugInInterfaceForService", I32, U32, P, P, P, P),
    sig!("IODestroyPlugInInterface", I32, P),
    // `PhysicalKeyboardLayoutType KBGetLayoutType(SInt16)`. Its OSType result
    // and narrow argument travel through the integer registers as 32-bit
    // values, which is the shape SDL's call site also uses.
    sig!("KBGetLayoutType", I32, I32),
    // `EventTime GetCurrentEventTime(void)` is a `double` of seconds since
    // boot, and `OSErr UpdateSystemActivity(UInt8)` is how a program that is
    // busy but not touching the keyboard tells the system not to sleep - which
    // a game does every frame.
    sig!("GetCurrentEventTime", F64),
    sig!("UpdateSystemActivity", I32, U8),
    //
    // `EventTargetRef GetApplicationEventTarget(void)` and the two other
    // singletons, each an opaque pointer.
    sig!("GetApplicationEventTarget", P),
    sig!("GetEventDispatcherTarget", P),
    sig!("GetMainEventLoop", P),
    // `OSStatus InstallEventHandler(EventTargetRef, EventHandlerUPP, ItemCount,
    // const EventTypeSpec *, void *, EventHandlerRef *)` - the handler is guest
    // code, so it is substituted like any other callback.
    cbsig!([1], "InstallEventHandler", I32, P, P, U64, P, P, P),
    sig!("RemoveEventHandler", I32, P),
    // `OSStatus InstallEventLoopTimer(EventLoopRef, EventTimerInterval,
    // EventTimerInterval, EventLoopTimerUPP, void *, EventLoopTimerRef *)`.
    // The two intervals are `double` seconds.
    cbsig!([3], "InstallEventLoopTimer", I32, P, F64, F64, P, P, P),
    sig!("RemoveEventLoopTimer", I32, P),
    // `OSStatus ReceiveNextEvent(ItemCount, const EventTypeSpec *, EventTimeout,
    // Boolean, EventRef *)`, whose timeout is `double` seconds.
    sig!("ReceiveNextEvent", I32, U64, P, F64, U8, P),
    sig!("ReleaseEvent", V, P),
    sig!("SendEventToEventTarget", I32, P, P),
    // `OSStatus SetSystemUIMode(SystemUIMode, SystemUIOptions)`, two `UInt32`.
    sig!("SetSystemUIMode", I32, U32, U32),
    // `OSErr FSFindFolder(FSVolumeRefNum, OSType, Boolean, FSRef *)`.
    sig!("FSFindFolder", I16, I16, U32, U8, P),
    sig!("GetIconRef", I16, I16, U32, U32, P),
    sig!("ReleaseIconRef", I16, P),
    sig!("LocaleRefFromLocaleString", I32, P, P),
    sig!("UCCreateCollator", I32, P, U32, U32, P),
    sig!("UCDisposeCollator", I32, P),
    sig!("UCCompareText", I32, P, P, U64, P, U64, P, P),
    sig!("UCGetCollationKey", I32, P, P, U64, U64, P, P),
    sig!("UCCompareCollationKeys", I32, P, U64, P, U64, P, P),
    // The handler is a guest function the Apple Event Manager calls when an
    // event arrives, so it crosses as a thunk; removal names the same one.
    cbsig!([2], "AEInstallEventHandler", I16, U32, U32, P, P, U8),
    cbsig!([2], "AERemoveEventHandler", I16, U32, U32, P, U8),
    sig!("AECreateDesc", I16, U32, P, I64, P),
    sig!("AEDisposeDesc", I16, P),
    sig!("AEGetDescDataSize", I64, P),
    sig!("AEGetDescData", I16, P, P, I64),
    // `OSStatus FSPathMakeRef(const UInt8 *, FSRef *, Boolean *)`, and the
    // reverse - `OSStatus FSRefMakePath(const FSRef *, UInt8 *, UInt32)`.
    sig!("FSPathMakeRef", I32, P, P, P),
    sig!("FSRefMakePath", I32, P, P, U32),
    // `OSStatus FSResolveAliasFile(FSRef *, Boolean, Boolean *, Boolean *)`.
    sig!("FSResolveAliasFile", I32, P, U8, P, P),
    // `OSStatus FSGetCatalogInfo(const FSRef *, FSCatalogInfoBitmap,
    // FSCatalogInfo *, HFSUniStr255 *, FSSpec *, FSRef *)`.
    sig!("FSGetCatalogInfo", I32, P, U32, P, P, P, P),
    sig!("FSCreateFileUnicode", I32, P, U64, P, U32, P, P, P),
    sig!("FSCompareFSRefs", I32, P, P),
    sig!("FSExchangeObjects", I32, P, P),
    sig!("FSIsAliasFile", I32, P, P, P),
    sig!("FSNewAlias", I32, P, P, P),
    sig!("FSSetCatalogInfo", I32, P, U32, P),
    sig!("FSGetVolumeInfo", I32, I16, U32, P, U32, P, P, P),
    sig!("FSGetVolumeParms", I32, I16, P, U32),
    sig!("FSPathMoveObjectToTrashSync", I32, P, P, U32),
    sig!("FSFileOperationCreate", P, P),
    sig!("FSFileOperationScheduleWithRunLoop", V, P, P, P),
    sig!("CSCopyMachineName", P),
    sig!("GetCurrentProcess", I32, P),
    sig!("GetFrontProcess", I32, P),
    sig!("GetProcessBundleLocation", I32, P, P),
    sig!("SameProcess", I32, P, P, P),
    sig!("SetFrontProcess", I32, P),
    sig!("TransformProcessType", I32, P, U32),
    // The two error-string lookups beside them, which hand back a pointer into
    // a static table.
    sig!("GetMacOSStatusCommentString", P, I32),
    sig!("GetMacOSStatusErrorString", P, I32),
    // The Resource Manager, which never has a file open; see
    // `coreservices::resources`.
    sig!("GetResource", P, U32, I16),
    sig!("Get1Resource", P, U32, I16),
    sig!("ResError", I16),
    sig!("ReleaseResource", V, P),
    sig!("HLock", V, P),
    sig!("HUnlock", V, P),
    sig!("TickCount", U32),
    // The two keyboard low-memory globals, both `UInt8 (*)(void)` and both an
    // ordinary function on a 64-bit system: `LMGetKbdType` is what still backs
    // `UCKeyTranslate`'s keyboard-type argument, and `LMGetKbdLast` is the ADB
    // address of the keyboard last used.
    sig!("LMGetKbdLast", U8),
    sig!("LMGetKbdType", U8),
    //
    // `NSApplicationMain` loads the main nib, creates `NSApp`, sends
    // `applicationDidFinishLaunching:` to the guest's delegate, and does not return
    // until the application terminates.
    sig!("NSApplicationMain", I32, I32, P),
    // `NSLog` takes an `NSString` format, not a C one, so even the rendering
    // path the `printf` family uses needs a conversion first.
    vsig!("NSLog", V, P),
    // Shimmed, and listed for the reason `NSLog` is. The format is the *second*
    // argument, so the entry names five pointers before the tail.
    vsig!("NSRunAlertPanel", I64, P, P, P, P, P),
    sig!("NSHomeDirectory", P),
    sig!("NSUserName", P),
    sig!("NSFullUserName", P),
    sig!("NSGetUncaughtExceptionHandler", P),
    sig!("NSClassFromString", P, P),
    // `SEL NSSelectorFromString(NSString *)` - two pointers, and the reason it
    // needs no translation is.
    sig!("NSSelectorFromString", P, P),
    sig!("NSFileTypeForHFSTypeCode", P, U32),
    // The handler is a guest function pointer the *runtime* calls, from inside
    // whatever raised - so it goes out as a thunk like every other callback
    // crossing the seam.
    cbsig!([0], "NSSetUncaughtExceptionHandler", V, P),
    sig!("NSSearchPathForDirectoriesInDomains", P, U64, U64, U8),
    sig!("NSTemporaryDirectory", P),
    // The `Add` pair returns the value *after* adding, and the `CompareAndSwap`
    // pair returns whether the swap happened.
    sig!("OSAtomicAdd32", I32, I32, P),
    sig!("OSAtomicAdd32Barrier", I32, I32, P),
    sig!("OSAtomicAdd64", I64, I64, P),
    sig!("OSAtomicAdd64Barrier", I64, I64, P),
    sig!("OSAtomicCompareAndSwap32", BOOL, I32, I32, P),
    sig!("OSAtomicCompareAndSwap32Barrier", BOOL, I32, I32, P),
    sig!("OSAtomicCompareAndSwap64", BOOL, I64, I64, P),
    sig!("OSAtomicCompareAndSwap64Barrier", BOOL, I64, I64, P),
    sig!("OSAtomicCompareAndSwapInt", BOOL, I32, I32, P),
    sig!("OSAtomicCompareAndSwapIntBarrier", BOOL, I32, I32, P),
    sig!("OSAtomicCompareAndSwapPtr", BOOL, P, P, P),
    sig!("OSAtomicCompareAndSwapPtrBarrier", BOOL, P, P, P),
    sig!("OSMemoryBarrier", V),
    // `OSSpinLock` is a bare `int32_t` the guest allocates itself, and the
    // host's implementation operates on it in place - the same arrangement as
    // `pthread_mutex_t`, and true for the same checked reason.
    sig!("OSSpinLockLock", V, P),
    sig!("OSSpinLockTry", BOOL, P),
    sig!("OSSpinLockUnlock", V, P),
    // `os_unfair_lock` is likewise a bare `uint32_t` the guest owns and the
    // host operates on in place; `options` is an `os_unfair_lock_options_t`.
    sig!("os_unfair_lock_lock", V, P),
    sig!("os_unfair_lock_lock_with_options", V, P, U32),
    sig!("os_unfair_lock_trylock", BOOL, P),
    sig!("os_unfair_lock_unlock", V, P),
    sig!("os_unfair_lock_assert_owner", V, P),
    sig!("os_unfair_lock_assert_not_owner", V, P),
    // Filed after the `OSAtomic` family because lowercase `p` sorts above `S`,
    // which is now a reader's convenience rather than a correctness condition -
    // [`index`] finds it wherever it is.
    sig!("OpenAComponent", I32, P, P),
    sig!("SCDynamicStoreCopyProxies", P, P),
    //
    // Every argument is a `CFTypeRef` and every return an `OSStatus`. The `kSec*`
    // keys are data imports resolving to the host's own constants, so the query
    // dictionary arrives keyed by the same pointers the framework compares against.
    // The out-parameter is a `CFTypeRef *` the framework writes a retained object
    // into, and the retain count being manipulated is the host object's own.
    sig!("SecItemAdd", I32, P, P),
    sig!("SecItemCopyMatching", I32, P, P),
    sig!("SecItemDelete", I32, P),
    sig!("SecItemUpdate", I32, P, P),
    sig!("SecCertificateCreateWithData", P, P, P),
    sig!("SecPolicyCreateBasicX509", P),
    sig!("SecTrustCreateWithCertificates", I32, P, P, P),
    sig!("SecTrustSetOptions", I32, P, U32),
    sig!("SecTrustEvaluate", I32, P, P),
    sig!("SecRandomCopyBytes", I32, P, U64, P),
    //
    // `SCDynamicStoreCreate`'s third argument is a callback the host invokes on
    // the run loop when a watched key changes; its fourth is an
    // `SCDynamicStoreContext`, carrying the same `retain`/`release`/
    // `copyDescription` triple as `CFStreamClientContext` and the same caveat -
    // a guest filling those in would hand the host x86-64 addresses inside a
    // structure this runtime never sees. Conventionally they are null.
    cbsig!([2], "SCDynamicStoreCreate", P, P, P, P, P),
    sig!("SCDynamicStoreCopyValue", P, P, P),
    sig!("SCNetworkInterfaceGetInterfaceType", P, P),
    sig!("SCNetworkServiceCopy", P, P, P),
    sig!("SCNetworkServiceGetInterface", P, P),
    sig!("SCPreferencesCreate", P, P, P, P),
    sig!("SCPreferencesGetValue", P, P, P),
    sig!("SCNetworkReachabilityCreateWithAddress", P, P, P),
    sig!("SCNetworkReachabilityCreateWithName", P, P, P),
    sig!("SCNetworkReachabilityGetFlags", U8, P, P),
    sig!("SCNetworkReachabilityScheduleWithRunLoop", U8, P, P, P),
    cbsig!([1], "SCNetworkReachabilitySetCallback", U8, P, P, P),
    sig!("SCNetworkReachabilityUnscheduleFromRunLoop", U8, P, P, P),
    //
    // Unity's networking asks the system for its trust anchors before it
    // validates anything, and copies each certificate's DER out to hand to
    // whichever TLS implementation it was built against.
    sig!("SecCertificateCopyData", P, P),
    sig!("SecCertificateGetData", I32, P, P),
    sig!("SecKeychainItemCopyContent", I32, P, P, P, P, P),
    sig!("SecKeychainItemFreeContent", I32, P, P),
    sig!("SecKeychainOpen", I32, P, P),
    sig!("SecKeychainSearchCopyNext", I32, P, P),
    sig!("SecKeychainSearchCreateFromAttributes", I32, P, U32, P, P),
    sig!("SecTrustCopyAnchorCertificates", I32, P),
    sig!("SecStaticCodeCreateWithPath", I32, P, U64, P),
    sig!("SecRequirementCreateWithString", I32, P, U64, P),
    sig!("SecRequirementCreateWithData", I32, P, U32, P),
    sig!("SecCodeCopySelf", I32, U32, P),
    sig!("SecCodeCheckValidity", I32, P, U32, P),
    sig!("SecStaticCodeCheckValidityWithErrors", I32, P, U64, P, P),
    //
    // SDL3 asks the current layout for its Unicode key-layout data so it can
    // translate a virtual key code into characters.
    sig!("TISCopyCurrentASCIICapableKeyboardInputSource", P),
    sig!("TISCopyCurrentASCIICapableKeyboardLayoutInputSource", P),
    sig!("GetCurrentKeyModifiers", U32),
    sig!("TISCopyCurrentKeyboardInputSource", P),
    sig!("TISCopyCurrentKeyboardLayoutInputSource", P),
    sig!("TISCopyInputMethodKeyboardLayoutOverride", P),
    // `TISInputSourceRef TISCopyInputSourceForLanguage(CFStringRef)`, and the
    // two list calls beside it - the second argument of the general one is a
    // `Boolean`, which is a byte rather than the four this ABI would guess.
    sig!("TISCopyInputSourceForLanguage", P, P),
    sig!("TISCreateASCIICapableInputSourceList", P),
    sig!("TISCreateInputSourceList", P, P, U8),
    // The four that turn a source on and off, each an `OSStatus` over one
    // `TISInputSourceRef`.
    sig!("TISDeselectInputSource", I32, P),
    sig!("TISDisableInputSource", I32, P),
    sig!("TISEnableInputSource", I32, P),
    sig!("TISGetInputSourceProperty", P, P, P),
    sig!("TISSelectInputSource", I32, P),
    // `OSStatus UCKeyTranslate(const UCKeyboardLayout *, UInt16, UInt16,
    // UInt32, UInt32, OptionBits, UInt32 *, UniCharCount, UniCharCount *,
    // UniChar[])` - the call that turns a virtual key code into characters,
    // read out of CarbonCore's `UnicodeUtilities.h`.
    sig!(
        "UCKeyTranslate",
        I32,
        P,
        U16,
        U16,
        U32,
        U32,
        U32,
        P,
        U64,
        P,
        P
    ),
    sig!("UTTypeCopyPreferredTagWithClass", P, P, P),
    sig!("UTTypeCreatePreferredIdentifierForTag", P, P, P, P),
    //
    // `_Block_copy` is **shimmed** - see [`crate::shims::block::block_copy`].
    // The signature stays for [`longjmp`]'s reason: it is what stops the site
    // being promoted to an inlined fast import and what names it at an exit.
    sig!("_Block_copy", P, P),
    sig!("_Block_release", V, P),
    sig!("_Block_object_assign", V, P, P, I32),
    sig!("_Block_object_dispose", V, P, I32),
    // Answered by `crate::shims::runtime` rather than by the host, whose
    // executable path is only the right answer because `crate::bundle` made it
    // so.
    sig!("_NSGetArgc", P),
    sig!("_NSGetArgv", P),
    sig!("_NSGetEnviron", P),
    sig!("_NSGetExecutablePath", I32, P, P),
    // Clang's stack-clash probe. It has no arguments and no result; it only
    // touches the guest stack before the function's body continues.
    sig!("___chkstk_darwin", V),
    //
    // The `_l` family and the wide-character calls libc++ reaches on the same
    // paths, scattered through this table by name.
    //
    // Every prototype was read out of the SDK headers (`xlocale/_ctype.h`,
    // `xlocale/_stdlib.h`, `xlocale/_wchar.h`) rather than recalled: `mbtowc_l` had
    // been wrong here for as long as it had been here and nothing found it.
    //
    // `locale_t` is an opaque `struct _xlocale *` the guest only obtains from
    // `newlocale` and hands straight back, so it crosses as a plain pointer and
    // neither side reads the structure behind it. That is what makes the family
    // passthrough rather than the [`crate::cxxlib`] problem again.
    sig!("___mb_cur_max_l", I32, P),
    sig!("___mb_cur_max", I32),
    // Private libSystem collation lookups that Apple's bash declares itself.
    sig!("__collate_lookup", V, P, P, P, P),
    sig!("__collate_lookup_l", V, P, P, P, P, P),
    sig!("___tolower_l", I32, I32, P),
    sig!("___toupper_l", I32, I32, P),
    sig!("__assert_rtn", V, P, P, I32, P),
    sig!("__bzero", V, P, U64),
    // Listed to keep it off the fast path and out of the audit's unknown list.
    // The handler is a guest function pointer, but no `cbsig!` marks it.
    sig!("__cxa_atexit", I32, P, P, P),
    sig!("__cxa_finalize", V, P),
    sig!("__darwin_check_fd_set_overflow", I32, I32, P, I32),
    sig!("__error", P),
    sig!("__exp10f", F32, F32),
    sig!("__isnand", I32, F64),
    // The `float` twin, which clang emits for `isnan()` on a `float` - a
    // different symbol from `__isnand` and not a variant of it, so a table with
    // only the `double` form leaves it undescribed.
    sig!("__isnanf", I32, F32),
    // The rest of `<math.h>`'s classification macros, which are functions on
    // Darwin.
    sig!("__isfinitef", I32, F32),
    sig!("__isfinited", I32, F64),
    sig!("__isinff", I32, F32),
    sig!("__isinfd", I32, F64),
    sig!("__isnormalf", I32, F32),
    sig!("__isnormald", I32, F64),
    sig!("__signbitf", I32, F32),
    sig!("__signbitd", I32, F64),
    sig!("__fpclassifyf", I32, F32),
    sig!("__fpclassifyd", I32, F64),
    sig!("__maskrune", I32, I32, U64),
    sig!("__maskrune_l", I32, I32, U64, P),
    sig!("__memcpy_chk", P, P, P, U64, U64),
    sig!("__memmove_chk", P, P, P, U64, U64),
    sig!("__memset_chk", P, P, I32, U64, U64),
    vsig!("__snprintf_chk", I32, P, U64, I32, U64, P),
    vsig!("__sprintf_chk", I32, P, I32, U64, P),
    sig!("__stack_chk_fail", V),
    sig!("__stpcpy_chk", P, P, P, U64),
    sig!("__strcat_chk", P, P, P, U64),
    sig!("__strcpy_chk", P, P, P, U64),
    sig!("__strlcat_chk", U64, P, P, U64, U64),
    sig!("__strlcpy_chk", U64, P, P, U64, U64),
    sig!("__strncat_chk", P, P, P, U64, U64),
    sig!("__strncpy_chk", P, P, P, U64, U64),
    sig!("__tolower", I32, I32),
    sig!("__toupper", I32, I32),
    // Darwin's futex, which is how a modern `libc++` blocks on
    // `std::atomic::wait` and how mimalloc parks a contended thread.
    sig!("__ulock_wait", I32, U32, P, U64, U32),
    sig!("__ulock_wait2", I32, U32, P, U64, U64, U64),
    sig!("__ulock_wake", I32, U32, P, U64),
    // The fortified `vsnprintf`; shimmed with the rest of the `va_list` family
    // for the reason [`crate::shims::valist`] gives.
    sig!("__vsnprintf_chk", I32, P, U64, I32, U64, P, P),
    vsig!("__vsprintf_chk", I32, P, I32, U64, P),
    // What `@available` / `__builtin_available` compiles to: `bool
    // _availability_version_check(uint32_t count, dyld_build_version_t *)`.
    sig!("_availability_version_check", BOOL, U32, P),
    // Served by [`crate::dyld::entry`] under both backends.
    sig!("_dyld_image_count", U32),
    sig!("_dyld_get_image_header", P, U32),
    sig!("_NSGetMachExecuteHeader", P),
    sig!("_dyld_get_image_vmaddr_slide", U64, U32),
    sig!("_dyld_image_containing_address", P, P),
    cbsig!([0], "_dyld_register_func_for_add_image", V, P),
    cbsig!([0], "_dyld_register_func_for_remove_image", V, P),
    sig!("_dyld_get_image_name", P, U32),
    sig!("_dyld_find_unwind_sections", BOOL, P, P),
    sig!("_exit", V, I32),
    // The Darwin spellings that skip the signal mask, imported as `__longjmp`
    // and `__setjmp`.
    sig!("_longjmp", V, P, I32),
    sig!("_setjmp", I32, P),
    // `void _tlv_atexit(void (*termfunc)(void *), void *objAddr)` - the
    // destructor half of the thread-local family.
    cbsig!([0], "_tlv_atexit", V, P, P),
    // Both spellings of the thread-local bootstrap.
    sig!("_tlv_bootstrap", P, P),
    // `void *tlv_allocate(tlv_descriptor *)`: the slow path of this runtime's
    // guest-code `tlv_get_addr`. See `crate::tls`.
    sig!("rosemary_tlv_allocate", P, P),
    sig!("abort", V),
    sig!("abs", I32, I32),
    sig!("accept", I32, I32, P, P),
    sig!("access", I32, P, I32),
    sig!("faccessat", I32, I32, P, I32, I32),
    sig!("acl_free", I32, P),
    sig!("acl_get_entry", I32, P, I32, P),
    sig!("acl_get_fd_np", P, I32, I32),
    sig!("acl_get_file", P, P, I32),
    sig!("acl_get_flag_np", I32, P, I32),
    sig!("acl_get_flagset_np", I32, P, P),
    sig!("acl_get_link_np", P, P, I32),
    sig!("acl_get_perm_np", I32, P, I32),
    sig!("acl_get_permset", I32, P, P),
    sig!("acl_get_qualifier", P, P),
    sig!("acl_get_tag_type", I32, P, P),
    sig!("acos", F64, F64),
    sig!("acosf", F32, F32),
    sig!("acosh", F64, F64),
    sig!("acoshf", F32, F32),
    // C11's aligned allocation, reached because the vendored libc++abi uses it
    // for an exception object with an over-aligned type in it.
    sig!("aligned_alloc", P, U64, U64),
    sig!("arc4random", U32),
    sig!("arc4random_buf", V, P, U64),
    sig!("asin", F64, F64),
    sig!("asinf", F32, F32),
    sig!("asinh", F64, F64),
    sig!("asinhf", F32, F32),
    vsig!("asprintf", I32, P, P),
    sig!("atan", F64, F64),
    sig!("atan2", F64, F64, F64),
    sig!("atan2f", F32, F32, F32),
    sig!("atanf", F32, F32),
    sig!("atanh", F64, F64),
    sig!("atanhf", F32, F32),
    // `atexit`, `bsearch`, `qsort` and `qsort_r` were refused until the
    // host->guest thunk existed.
    cbsig!([0], "atexit", I32, P),
    sig!("atof", F64, P),
    sig!("atoi", I32, P),
    sig!("atol", I64, P),
    sig!("atoll", I64, P),
    // `<execinfo.h>`, and the reason a *crash handler* is the first thing a C++
    // game reaches.
    sig!("backtrace", I32, P, I32),
    sig!("backtrace_symbols", P, P, I32),
    sig!("backtrace_symbols_fd", V, P, I32, I32),
    // `basename` and `dirname` both hand back a pointer into a static buffer
    // the host owns, which the guest only ever reads.
    sig!("basename", P, P),
    sig!("bcmp", I32, P, P, U64),
    // `void bcopy(const void *src, void *dst, size_t n)` - the BSD spelling,
    // and the one place in this table where the two pointers are the other way
    // round from `memcpy`.
    sig!("bcopy", V, P, P, U64),
    sig!("bind", I32, I32, P, U32),
    // `kern_return_t bootstrap_look_up(mach_port_t, const name_t, mach_port_t
    // *)`.
    sig!("bootstrap_look_up", I32, U32, P, P),
    cbsig!([4], "bsearch", P, P, P, U64, U64, P),
    sig!("btowc", I32, I32),
    sig!("btowc_l", I32, I32, P),
    sig!("bzero", V, P, U64),
    sig!("calloc", P, U64, U64),
    sig!("cbrt", F64, F64),
    sig!("cbrtf", F32, F32),
    sig!("ceil", F64, F64),
    sig!("ceilf", F32, F32),
    sig!("chdir", I32, P),
    sig!("chmod", I32, P, U32),
    sig!("clearerr", V, P),
    // `clock_t clock(void)`.
    sig!("clock", U64),
    // `kern_return_t clock_get_time(clock_serv_t, mach_timespec_t *)`.
    sig!("clock_get_time", I32, U32, P),
    // `struct timespec` is two 64-bit words on both architectures, and the
    // guest passes it by address, so the host fills it in place.
    sig!("clock_gettime", I32, U32, P),
    sig!("clock_getres", I32, U32, P),
    sig!("clock_settime", I32, U32, P),
    sig!("clock_gettime_nsec_np", U64, I32),
    // `kern_return_t clock_sleep(mach_port_t, sleep_type_t, mach_timespec_t,
    // mach_timespec_t *)`.
    sig!("clock_sleep", I32, U32, I32, U64, P),
    sig!("close", I32, I32),
    sig!("closedir", I32, P),
    sig!("compat_mode", U8, P, P),
    sig!("compress", I32, P, P, P, U64),
    sig!("connect", I32, I32, P, U32),
    sig!("cos", F64, F64),
    sig!("cosf", F32, F32),
    sig!("cosh", F64, F64),
    sig!("coshf", F32, F32),
    //
    // `deflateInit_` and `inflateInit_` are the real entry points: `deflateInit`
    // is a macro that passes `ZLIB_VERSION` and `sizeof(z_stream)`, so a program
    // importing the underscore forms is one that used the ordinary API.
    sig!("adler32", U64, U64, P, U32),
    sig!("alarm", U32, U32),
    // The comparator `scandir` is usually called with. Both arguments are
    // `const struct dirent **`, which is a pointer to an array slot rather
    // than to an entry.
    sig!("alphasort", I32, P, P),
    // `double cabs(double complex)`. A `double complex` is two doubles, and it
    // is one of the aggregates the two ABIs agree about.
    sig!("cabs", F64, F64, F64),
    sig!("cfgetispeed", U64, P),
    sig!("cfgetospeed", U64, P),
    sig!("cfsetispeed", I32, P, U64),
    sig!("cfsetospeed", I32, P, U64),
    sig!("cfsetspeed", I32, P, U64),
    sig!("chflags", I32, P, U32),
    sig!("lchflags", I32, P, U32),
    sig!("closelog", V),
    sig!("confstr", U64, I32, P, U64),
    sig!("crc32", U64, U64, P, U32),
    sig!("creat", I32, P, U32),
    sig!("ctime_r", P, P, P),
    //
    // Passthrough to `/usr/lib/libcurl.4.dylib`, which is the library the
    // guest names in its own install path - so the host's copy *is* the one
    // it asked for.
    sig!("curl_easy_cleanup", V, P),
    vsig!("curl_easy_getinfo", I32, P, I32),
    sig!("curl_easy_init", P),
    sig!("curl_easy_perform", I32, P),
    sig!("curl_easy_reset", V, P),
    vsig!("curl_easy_setopt", I32, P, I32),
    sig!("curl_global_cleanup", V),
    sig!("curl_global_init", I32, I64),
    sig!("deflate", I32, P, I32),
    sig!("deflateEnd", I32, P),
    sig!("deflateInit2_", I32, P, I32, I32, I32, I32, I32, P, I32),
    sig!("deflateInit_", I32, P, I32, P, I32),
    sig!("deflateReset", I32, P),
    //
    // A dispatch object is opaque and reference-counted by libdispatch
    // itself, so every one of these is a handle the guest received and hands
    // back - nothing about the object crosses.
    sig!("difftime", F64, I64, I64),
    sig!("dirname", P, P),
    sig!("dispatch_get_context", P, P),
    sig!("dispatch_get_global_queue", P, I64, U64),
    cbsig!([3], "dispatch_group_async_f", V, P, P, P, P),
    cbsig!([3], "dispatch_apply_f", V, U64, P, P, P),
    sig!("dispatch_group_create", P),
    sig!("dispatch_group_wait", I64, P, U64),
    cbsig!([2], "dispatch_once_f", V, P, P, P),
    sig!("dispatch_release", V, P),
    sig!("dispatch_resume", V, P),
    sig!("dispatch_queue_create", P, P, P),
    sig!("dispatch_semaphore_create", P, I64),
    // Both halves return `intptr_t` rather than `void`: `wait` answers whether
    // it timed out, and `signal` answers whether a thread was woken.
    sig!("dispatch_semaphore_signal", I64, P),
    sig!("dispatch_semaphore_wait", I64, P, U64),
    // Block-taking calls go through the block shim, which rewrites the guest
    // invoke pointer before libdispatch sees it - essential for `dispatch_sync`,
    // where an uncallable guest block makes its caller wait forever.
    sig!("dispatch_set_context", V, P, P),
    sig!("dispatch_source_cancel", V, P),
    sig!("dispatch_source_create", P, P, U64, U64, P),
    cbsig!([1], "dispatch_source_set_cancel_handler_f", V, P, P),
    cbsig!([1], "dispatch_source_set_event_handler_f", V, P, P),
    sig!("dispatch_source_set_timer", V, P, U64, U64, U64),
    sig!("dispatch_suspend", V, P),
    // `dispatch_time(dispatch_time_t base, int64_t delta)` - the base is an
    // opaque absolute time and the delta is signed nanoseconds, which is why
    // the two arguments are not the same type despite both being doublewords.
    sig!("dispatch_time", U64, U64, I64),
    // `Dl_info` is four pointers and nothing else, so the guest allocates one
    // and the host fills it in place.
    sig!("dladdr", I32, P, P),
    // Initializers run inside `dlopen`, terminators inside `dlclose`.
    rsig!("dlclose", I32, P),
    sig!("dlerror", P),
    rsig!("dlopen", P, P, I32),
    sig!("dlsym", P, P, P),
    vsig!("dprintf", I32, I32, P),
    sig!("dup", I32, I32),
    sig!("dup2", I32, I32, I32),
    sig!("dup3", I32, I32, I32, I32),
    sig!("endgrent", V),
    sig!("endpwent", V),
    sig!("endusershell", V),
    vsig!("err", V, I32, P),
    vsig!("errx", V, I32, P),
    vsig!("errc", V, I32, I32, P),
    sig!("verr", V, I32, P, P),
    sig!("verrx", V, I32, P, P),
    sig!("verrc", V, I32, I32, P, P),
    sig!("execv", I32, P, P),
    sig!("execve", I32, P, P, P),
    sig!("execvp", I32, P, P),
    vsig!("execl", I32, P, P),
    vsig!("execlp", I32, P, P),
    vsig!("execle", I32, P, P),
    sig!("exit", V, I32),
    sig!("exp", F64, F64),
    sig!("exp2", F64, F64),
    sig!("exp2f", F32, F32),
    sig!("expf", F32, F32),
    sig!("expm1", F64, F64),
    sig!("expm1f", F32, F32),
    //
    // Added as a family rather than one at a time, which is worth saying
    // because everything else in this table got here by stopping a program by
    // name.
    sig!("erf", F64, F64),
    sig!("erff", F32, F32),
    sig!("erfc", F64, F64),
    sig!("erfcf", F32, F32),
    sig!("tgamma", F64, F64),
    sig!("tgammaf", F32, F32),
    sig!("lgamma", F64, F64),
    sig!("lgammaf", F32, F32),
    sig!("exp10", F64, F64),
    sig!("exp10f", F32, F32),
    sig!("remainder", F64, F64, F64),
    sig!("remainderf", F32, F32, F32),
    sig!("remquo", F64, F64, F64, P),
    sig!("remquof", F32, F32, F32, P),
    sig!("nextafterf", F32, F32, F32),
    sig!("copysign", F64, F64, F64),
    sig!("copysignf", F32, F32, F32),
    // `double nan(const char *tagp)` takes a **string**, not a number: the
    // payload of the quiet NaN it builds, spelled in digits.
    sig!("nan", F64, P),
    sig!("nanf", F32, P),
    sig!("fabs", F64, F64),
    sig!("fabsf", F32, F32),
    sig!("fchmod", I32, I32, U32),
    sig!("fclose", I32, P),
    sig!("clonefileat", I32, I32, P, I32, P, I32),
    vsig!("fcntl", I32, I32, I32),
    // `copyfile(const char *, const char *, copyfile_state_t,
    // copyfile_flags_t)`: the state is an opaque pointer and Darwin's flag
    // word is unsigned 32-bit on both architectures.
    sig!("copyfile", I32, P, P, P, U32),
    sig!("fcopyfile", I32, I32, P, U32),
    sig!("copyfile_state_set", I32, P, U32, P),
    sig!("copyfile_state_get", I32, P, U32, P),
    sig!("fdopen", P, I32, P),
    sig!("feof", I32, P),
    sig!("ferror", I32, P),
    sig!("fflagstostr", P, U64),
    sig!("fflush", I32, P),
    sig!("fgetc", I32, P),
    sig!("fgetpos", I32, P, P),
    sig!("fgets", P, P, I32, P),
    sig!("fgetln", P, P, P),
    sig!("fileno", I32, P),
    //
    // Each entry stopped an initializer by name and was added for it.
    //
    // Every one is scalar or writes through a pointer to a structure that was
    // *checked* identical on both architectures rather than assumed: `rlimit`,
    // `utsname`, `passwd`, `msghdr`, `ifaddrs`, `in_addr` and `Dl_info` were
    // compiled both ways and their sizes and every field offset compared.
    sig!("flock", I32, I32, I32),
    // The `FILE *` lock a guest takes around a run of unlocked reads. Both are
    // `void`, and the lock is recursive per thread.
    sig!("flockfile", V, P),
    sig!("floor", F64, F64),
    sig!("floorf", F32, F32),
    sig!("fma", F64, F64, F64, F64),
    sig!("fmaf", F32, F32, F32, F32),
    sig!("fmax", F64, F64, F64),
    sig!("fmaxf", F32, F32, F32),
    sig!("fmin", F64, F64, F64),
    sig!("fminf", F32, F32, F32),
    sig!("fmod", F64, F64, F64),
    sig!("fmtcheck", P, P, P),
    sig!("fmodf", F32, F32, F32),
    sig!("fnmatch", I32, P, P, I32),
    sig!("fopen", P, P, P),
    // `fork` passes through, and what the child gets is this whole runtime.
    sig!("fork", I32),
    sig!("vfork", I32),
    sig!("fpathconf", I64, I32, I32),
    vsig!("fprintf", I32, P, P),
    sig!("fputc", I32, I32, P),
    sig!("fputs", I32, P, P),
    sig!("fputwc", I32, I32, P),
    sig!("fread", U64, P, U64, U64, P),
    sig!("free", V, P),
    sig!("freeaddrinfo", V, P),
    sig!("freeifaddrs", V, P),
    sig!("freeifmaddrs", V, P),
    sig!("freelocale", V, P),
    sig!("freopen", P, P, P, P),
    // `double frexp(double, int *)` - the exponent goes to a pointer the
    // guest owns and the mantissa comes back in a vector register.
    sig!("frexp", F64, F64, P),
    vsig!("fscanf", I32, P, P),
    sig!("fseek", I32, P, I64, I32),
    sig!("fseeko", I32, P, I64, I32),
    sig!("fsetpos", I32, P, P),
    sig!("fstat", I32, I32, P),
    sig!("fstatat", I32, I32, P, P, I32),
    sig!("fstatfs", I32, I32, P),
    sig!("getmntinfo", I32, P, I32),
    sig!("getfsstat", I32, P, I32, I32),
    sig!("fstatvfs", I32, I32, P),
    sig!("fsync", I32, I32),
    sig!("ftell", I64, P),
    sig!("ftello", I64, P),
    // `int ftime(struct timeb *)`, which is older than POSIX and still
    // exported.
    sig!("ftime", I32, P),
    sig!("ftruncate", I32, I32, I64),
    sig!("fts_children", P, P, I32),
    sig!("fts_close", I32, P),
    // The comparator takes `const FTSENT **`, which is still just a pointer.
    cbsig!([2], "fts_open", P, P, I32, P),
    sig!("fts_read", P, P),
    sig!("fts_set", I32, P, P, I32),
    cbsig!([1, 2, 3, 4], "funopen", P, P, P, P, P, P),
    sig!("funlockfile", V, P),
    sig!("futimes", I32, I32, P),
    sig!("fwrite", U64, P, U64, U64, P),
    sig!("gai_strerror", P, I32),
    sig!("getaddrinfo", I32, P, P, P, P),
    sig!("getbsize", P, P, P),
    sig!("getc", I32, P),
    sig!("getcwd", P, P, U64),
    sig!("getdomainname", I32, P, I32),
    sig!("getdtablesize", I32),
    sig!("getegid", U32),
    sig!("getenv", P, P),
    sig!("geteuid", U32),
    sig!("getgid", U32),
    sig!("getgroups", I32, I32, P),
    sig!("getgrouplist_2", I32, P, U32, P),
    //
    // `struct passwd` is five pointers, two 32-bit ids and two `time_t`s;
    // `struct group` is two pointers, a `gid_t` and a pointer.
    sig!("getgrent", P),
    sig!("getgrgid", P, U32),
    sig!("getgrgid_r", I32, U32, P, P, U64, P),
    sig!("getgrnam", P, P),
    sig!("getgrnam_r", I32, P, P, P, U64, P),
    sig!("gethostbyname", P, P),
    sig!("gethostbyname2", P, P, I32),
    sig!("gethostbyaddr", P, P, U32, I32),
    sig!("gethostid", I64),
    sig!("gethostname", I32, P, U64),
    sig!("getifaddrs", I32, P),
    sig!("getifmaddrs", I32, P),
    sig!("getloadavg", I32, P, I32),
    sig!("getmode", U16, P, U16),
    sig!("getlogin", P),
    sig!("getdelim", I64, P, P, I32, P),
    sig!("getline", I64, P, P, P),
    sig!("getlogin_r", I32, P, U64),
    sig!("getnameinfo", I32, P, U32, P, U32, P, U32, I32),
    sig!("getopt", I32, I32, P, P),
    sig!("getopt_long", I32, I32, P, P, P, P),
    sig!("getpagesize", I32),
    sig!("getpeername", I32, I32, P, P),
    sig!("getpeereid", I32, I32, P, P),
    sig!("getpgid", I32, I32),
    sig!("getpid", I32),
    sig!("getpgrp", I32),
    sig!("getppid", I32),
    sig!("getpriority", I32, I32, U32),
    sig!("getprogname", P),
    // `struct protoent` - a name, an alias vector and an `int` - comes back the
    // same way `getpwuid`'s does, as a pointer into libSystem's storage, and has
    // the same layout on both architectures.
    sig!("getprotobyname", P, P),
    sig!("getpwent", P),
    sig!("getpwnam", P, P),
    sig!("getpwnam_r", I32, P, P, P, U64, P),
    // `struct passwd` comes back as a pointer into libSystem's own storage,
    // which the guest then reads directly - so this one depends on the layout
    // check above in a way the writers-through-a-pointer do not.
    sig!("getpwuid", P, U32),
    sig!("getpwuid_r", I32, U32, P, P, U64, P),
    sig!("getrlimit", I32, I32, P),
    sig!("getrusage", I32, I32, P),
    // `const struct section_64 *getsectbynamefromheader_64(const struct
    // mach_header_64 *, const char *seg, const char *sect)`.
    sig!("getsectbynamefromheader_64", P, P, P, P),
    sig!("getservbyname", P, P, P),
    // `struct servent` is four pointer-aligned fields in the same order on
    // both, so the host's static result reads correctly.
    sig!("getservent", P),
    sig!("setservent", V, I32),
    sig!("endservent", V),
    sig!("getsid", I32, I32),
    sig!("getsockname", I32, I32, P, P),
    sig!("getsockopt", I32, I32, I32, I32, P, P),
    sig!("gettimeofday", I32, P, P),
    sig!("getuid", U32),
    sig!("getaudit_addr", I32, P, U32),
    sig!("getwc", I32, P),
    sig!("getxattr", I64, P, P, P, U64, U32, I32),
    sig!("getentropy", I32, P, U64),
    sig!("getattrlist", I32, P, P, P, U64, U64),
    sig!("fgetattrlist", I32, I32, P, P, U64, U64),
    // `int getattrlistbulk(int, struct attrlist *, void *, size_t, uint64_t)`
    sig!("getattrlistbulk", I32, I32, P, P, U64, U64),
    sig!("csops", I32, I32, U32, P, U64),
    //
    // `GLenum`, `GLuint` and `GLbitfield` are `unsigned int`, `GLint` and `GLsizei`
    // are `int`, `GLfloat` is `float`, `GLclampd` is `double`, `GLboolean` is
    // `unsigned char`.
    //
    // `GLfloat` versus `GLclampd` is the one to check rather than assume:
    // `glClearColor` takes four `float`s and `glClearDepth` one `double`, so a
    // uniform "floating point is `f64`" rule silently mis-clears the screen.
    sig!("glActiveTexture", V, U32),
    // A `GLclampf` is a `float`, so the reference value lands in a vector
    // register while the comparison function does not.
    sig!("glAlphaFunc", V, U32, F32),
    sig!("glAttachShader", V, U32, U32),
    sig!("glBegin", V, U32),
    sig!("glBindAttribLocation", V, U32, U32, P),
    sig!("glBindBuffer", V, U32, U32),
    sig!("glBindFramebuffer", V, U32, U32),
    sig!("glBindFramebufferEXT", V, U32, U32),
    sig!("glBindRenderbuffer", V, U32, U32),
    sig!("glBindRenderbufferEXT", V, U32, U32),
    sig!("glBindTexture", V, U32, U32),
    // The APPLE vertex-array-object extension, which is what an OpenGL 2.1
    // context on macOS has instead of the core entry points - same shapes as
    // `glGenVertexArrays`/`glBindVertexArray`/`glDeleteVertexArrays`.
    sig!("glBindVertexArrayAPPLE", V, U32),
    sig!("glBlendEquation", V, U32),
    sig!("glBlendFunc", V, U32, U32),
    sig!("glBlendFuncSeparate", V, U32, U32, U32, U32),
    // A `GLsizeiptr` and a `GLintptr` are both `long`, not `int`.
    sig!("glBufferData", V, U32, I64, P, U32),
    sig!("glBufferSubData", V, U32, I64, I64, P),
    sig!("glCheckFramebufferStatus", U32, U32),
    sig!("glCheckFramebufferStatusEXT", U32, U32),
    sig!("glClear", V, U32),
    sig!("glClearColor", V, F32, F32, F32, F32),
    sig!("glClearDepth", V, F64),
    sig!("glClearStencil", V, I32),
    sig!("glColorMask", V, U8, U8, U8, U8),
    sig!("glCompileShader", V, U32),
    // Eight integer arguments exactly, which is the guest's six registers plus
    // two it spilled; the host takes all eight in `x0`–`x7`.
    sig!(
        "glCompressedTexImage2D",
        V,
        U32,
        I32,
        U32,
        I32,
        I32,
        I32,
        I32,
        P
    ),
    sig!("glCreateProgram", U32),
    sig!("glCreateShader", U32, U32),
    sig!("glDeleteBuffers", V, I32, P),
    sig!("glDeleteFramebuffers", V, I32, P),
    sig!("glDeleteFramebuffersEXT", V, I32, P),
    sig!("glDeleteProgram", V, U32),
    sig!("glDeleteRenderbuffers", V, I32, P),
    sig!("glDeleteRenderbuffersEXT", V, I32, P),
    sig!("glDeleteShader", V, U32),
    sig!("glDeleteTextures", V, I32, P),
    sig!("glDeleteVertexArraysAPPLE", V, I32, P),
    sig!("glDepthFunc", V, U32),
    sig!("glDepthMask", V, U8),
    sig!("glDepthRange", V, F64, F64),
    sig!("glDisable", V, U32),
    sig!("glDisableVertexAttribArray", V, U32),
    sig!("glDrawArrays", V, U32, I32, I32),
    sig!("glDrawBuffer", V, U32),
    sig!("glDrawBuffers", V, I32, P),
    // The last argument is a pointer *or* a byte offset into the bound element
    // buffer, depending on whether one is bound - an integer either way, and
    // identity mapping means a guest pointer needs no translation.
    sig!("glDrawElements", V, U32, I32, U32, P),
    sig!("glEnable", V, U32),
    sig!("glEnableVertexAttribArray", V, U32),
    sig!("glEnd", V),
    sig!("glFinish", V),
    sig!("glFlush", V),
    sig!("glFramebufferRenderbuffer", V, U32, U32, U32, U32),
    sig!("glFramebufferRenderbufferEXT", V, U32, U32, U32, U32),
    sig!("glFramebufferTexture2D", V, U32, U32, U32, U32, I32),
    sig!("glFramebufferTexture2DEXT", V, U32, U32, U32, U32, I32),
    sig!("glFrontFace", V, U32),
    sig!("glGenBuffers", V, I32, P),
    sig!("glGenFramebuffers", V, I32, P),
    sig!("glGenFramebuffersEXT", V, I32, P),
    sig!("glGenRenderbuffers", V, I32, P),
    sig!("glGenRenderbuffersEXT", V, I32, P),
    sig!("glGenTextures", V, I32, P),
    sig!("glGenVertexArraysAPPLE", V, I32, P),
    sig!("glGenerateMipmap", V, U32),
    sig!("glGetActiveUniform", V, U32, U32, I32, P, P, P, P),
    sig!("glGetAttribLocation", I32, U32, P),
    sig!("glGetBooleanv", V, U32, P),
    sig!("glGetError", U32),
    sig!("glGetFloatv", V, U32, P),
    sig!("glGetIntegerv", V, U32, P),
    sig!("glGetProgramInfoLog", V, U32, I32, P, P),
    sig!("glGetProgramiv", V, U32, U32, P),
    sig!("glGetShaderInfoLog", V, U32, I32, P, P),
    sig!("glGetShaderSource", V, U32, I32, P, P),
    sig!("glGetShaderiv", V, U32, U32, P),
    sig!("glGetString", P, U32),
    sig!("glGetUniformLocation", I32, U32, P),
    sig!("glIsEnabled", U8, U32),
    sig!("glLineWidth", V, F32),
    sig!("glLinkProgram", V, U32),
    sig!("glLoadIdentity", V),
    // Returns a pointer into the driver's own mapping of a buffer object,
    // which the guest then writes through - a host address in guest hands, which
    // identity mapping makes ordinary.
    sig!("glMapBuffer", P, U32, U32),
    sig!("glMatrixMode", V, U32),
    sig!("glPixelStorei", V, U32, I32),
    sig!("glPopMatrix", V),
    sig!("glPushMatrix", V),
    sig!("glReadPixels", V, I32, I32, I32, I32, U32, U32, P),
    sig!("glRenderbufferStorage", V, U32, U32, I32, I32),
    sig!("glRenderbufferStorageEXT", V, U32, U32, I32, I32),
    sig!("glScissor", V, I32, I32, I32, I32),
    sig!("glShaderSource", V, U32, I32, P, P),
    sig!("glStencilFunc", V, U32, I32, U32),
    sig!("glStencilMask", V, U32),
    sig!("glStencilOp", V, U32, U32, U32),
    sig!("glTexCoord2f", V, F32, F32),
    // Nine integer arguments, which is one more than either the guest's six
    // registers or the host's eight.
    sig!("glTexImage2D", V, U32, I32, I32, I32, I32, I32, U32, U32, P),
    //
    // A `GLsync` is an opaque pointer and a `GLuint64` timeout is a
    // doubleword, so the fence family crosses without ceremony.
    sig!("glClientWaitSync", U32, P, U32, U64),
    sig!("glDeleteSync", V, P),
    sig!("glFenceSync", P, U32, U32),
    sig!("glGetTexLevelParameteriv", V, U32, I32, U32, P),
    //
    // Almost nothing below is a static import.
    sig!("glBeginQuery", V, U32, U32),
    sig!("glBindBufferBase", V, U32, U32, U32),
    sig!("glBindBufferRange", V, U32, U32, U32, I64, I64),
    sig!("glBindFragDataLocation", V, U32, U32, P),
    sig!("glBindSampler", V, U32, U32),
    sig!("glBindVertexArray", V, U32),
    sig!("glBlendEquationSeparate", V, U32, U32),
    sig!("glClearBufferfi", V, U32, I32, F32, I32),
    sig!("glClearBufferfv", V, U32, I32, P),
    sig!("glClearBufferiv", V, U32, I32, P),
    sig!("glClearBufferuiv", V, U32, I32, P),
    sig!("glClearDepthf", V, F32),
    sig!("glColorMaski", V, U32, U8, U8, U8, U8),
    sig!(
        "glCompressedTexSubImage2D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        U32,
        I32,
        P
    ),
    sig!("glCopyBufferSubData", V, U32, U32, I64, I64, I64),
    sig!(
        "glCopyTexSubImage2D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32
    ),
    sig!("glCullFace", V, U32),
    sig!("glDeleteQueries", V, I32, P),
    sig!("glDeleteSamplers", V, I32, P),
    sig!("glDeleteVertexArrays", V, I32, P),
    sig!("glDepthRangef", V, F32, F32),
    sig!("glDetachShader", V, U32, U32),
    sig!("glDisablei", V, U32, U32),
    sig!("glDrawArraysInstanced", V, U32, I32, I32, I32),
    sig!("glDrawElementsInstanced", V, U32, I32, U32, P, I32),
    sig!("glDrawRangeElements", V, U32, U32, U32, I32, U32, P),
    sig!("glEnablei", V, U32, U32),
    sig!("glEndQuery", V, U32),
    sig!("glFlushMappedBufferRange", V, U32, I64, I64),
    sig!("glFramebufferTextureLayer", V, U32, U32, U32, I32, I32),
    sig!("glGenQueries", V, I32, P),
    sig!("glGenSamplers", V, I32, P),
    sig!("glGenVertexArrays", V, I32, P),
    sig!("glGetActiveAttrib", V, U32, U32, I32, P, P, P, P),
    sig!("glGetActiveUniformBlockName", V, U32, U32, I32, P, P),
    sig!("glGetActiveUniformBlockiv", V, U32, U32, U32, P),
    sig!("glGetAttachedShaders", V, U32, I32, P, P),
    sig!("glGetBufferParameteriv", V, U32, U32, P),
    sig!("glGetFramebufferAttachmentParameteriv", V, U32, U32, U32, P),
    sig!("glGetInteger64v", V, U32, P),
    sig!("glGetInternalformativ", V, U32, U32, U32, I32, P),
    sig!("glGetMultisamplefv", V, U32, U32, P),
    sig!("glGetProgramBinary", V, U32, I32, P, P, P),
    sig!("glGetQueryObjectui64v", V, U32, U32, P),
    sig!("glGetQueryObjectuiv", V, U32, U32, P),
    sig!("glGetRenderbufferParameteriv", V, U32, U32, P),
    // `const GLubyte *glGetStringi(GLenum name, GLuint index)` - how a core
    // context enumerates its extensions, since `glGetString(GL_EXTENSIONS)` is
    // an error there.
    sig!("glGetStringi", P, U32, U32),
    sig!("glGetSynciv", V, P, U32, I32, P, P),
    sig!("glGetTexParameterfv", V, U32, U32, P),
    sig!("glGetTexParameteriv", V, U32, U32, P),
    sig!("glGetUniformBlockIndex", U32, U32, P),
    sig!("glGetVertexAttribiv", V, U32, U32, P),
    sig!("glHint", V, U32, U32),
    sig!("glIsBuffer", U8, U32),
    sig!("glIsEnabledi", U8, U32, U32),
    sig!("glIsFramebuffer", U8, U32),
    sig!("glIsProgram", U8, U32),
    sig!("glIsRenderbuffer", U8, U32),
    sig!("glIsShader", U8, U32),
    sig!("glIsTexture", U8, U32),
    sig!("glIsQuery", U8, U32),
    sig!("glIsSampler", U8, U32),
    sig!("glIsSync", U8, P),
    sig!("glIsVertexArray", U8, U32),
    //
    // Added from the same header for the same reason as the block above:
    // these arrive by `dlsym`, so waiting for each to stop a run costs a
    // build and a launch per entry point.
    sig!("glBeginTransformFeedback", V, U32),
    sig!("glBindFragDataLocationIndexed", V, U32, U32, U32, P),
    sig!("glBindProgramPipeline", V, U32),
    sig!("glBindTransformFeedback", V, U32, U32),
    sig!("glBlendEquationSeparatei", V, U32, U32, U32),
    sig!("glBlendEquationi", V, U32, U32),
    sig!("glBlendFuncSeparatei", V, U32, U32, U32, U32, U32),
    sig!("glBlendFunci", V, U32, U32, U32),
    sig!("glCreateShaderProgramv", U32, U32, I32, P),
    sig!("glDebugMessageControl", V, U32, U32, U32, I32, P, U8),
    sig!("glDebugMessageInsert", V, U32, U32, U32, U32, I32, P),
    sig!("glDeleteProgramPipelines", V, I32, P),
    sig!("glDeleteTransformFeedbacks", V, I32, P),
    sig!("glDrawArraysIndirect", V, U32, P),
    sig!("glDrawElementsIndirect", V, U32, U32, P),
    sig!("glDrawTransformFeedback", V, U32, U32),
    sig!("glEndTransformFeedback", V),
    sig!("glFlushRenderAPPLE", V),
    sig!("glGenProgramPipelines", V, I32, P),
    sig!("glGenTransformFeedbacks", V, I32, P),
    sig!("glGetActiveUniformsiv", V, U32, I32, P, U32, P),
    sig!("glGetDoublev", V, U32, P),
    sig!("glGetFragDataIndex", I32, U32, P),
    sig!("glGetProgramPipelineInfoLog", V, U32, I32, P, P),
    sig!("glGetProgramPipelineiv", V, U32, U32, P),
    sig!("glGetQueryObjecti64v", V, U32, U32, P),
    sig!("glGetShaderPrecisionFormat", V, U32, U32, P, P),
    sig!("glGetSubroutineIndex", U32, U32, U32, P),
    sig!("glGetUniformIndices", V, U32, I32, P, P),
    sig!("glInsertEventMarkerEXT", V, I32, P),
    sig!("glLabelObjectEXT", V, U32, U32, I32, P),
    sig!("glGetObjectLabelEXT", V, U32, U32, I32, P, P),
    //
    // Added in bulk because a refusal on the render thread is the *outermost* guest
    // frame there, so it exits the process rather than parking - one launch per
    // missing entry point, and a launch is ninety seconds.
    sig!("glBlendColor", V, F32, F32, F32, F32),
    sig!("glDrawElementsBaseVertex", V, U32, I32, U32, P, I32),
    //
    // Unreal Engine 3's OpenGL RHI predates these being core, so it resolves
    // the *extension* names - same functions, different symbols, and a table
    // that has only `glDrawArraysInstanced` does not answer `...InstancedARB`.
    sig!("glDrawArraysInstancedARB", V, U32, I32, I32, I32),
    sig!("glDrawElementsInstancedARB", V, U32, I32, U32, P, I32),
    sig!("glVertexAttribDivisorARB", V, U32, U32),
    // `void glBufferParameteriAPPLE(GLenum target, GLenum pname, GLint param)`,
    // which is how `APPLE_flush_buffer_range` turns off the driver's implicit
    // flush, and `glFlushMappedBufferRangeAPPLE(target, offset, size)`, which is
    // the explicit one - an offset and a size, both pointer-width.
    sig!("glBufferParameteriAPPLE", V, U32, U32, I32),
    sig!("glFlushMappedBufferRangeAPPLE", V, U32, I64, I64),
    sig!("glGetQueryObjectiv", V, U32, U32, P),
    sig!("glEnableClientState", V, U32),
    sig!("glDisableClientState", V, U32),
    sig!("glVertexPointer", V, I32, U32, I32, P),
    sig!("glTexCoordPointer", V, I32, U32, I32, P),
    sig!("glTexEnvf", V, U32, U32, F32),
    sig!(
        "glDrawElementsInstancedBaseVertex",
        V,
        U32,
        I32,
        U32,
        P,
        I32,
        I32
    ),
    sig!(
        "glDrawRangeElementsBaseVertex",
        V,
        U32,
        U32,
        U32,
        I32,
        U32,
        P,
        I32
    ),
    sig!("glGetBooleani_v", V, U32, U32, P),
    sig!("glGetFloati_v", V, U32, U32, P),
    sig!("glGetIntegeri_v", V, U32, U32, P),
    sig!("glGetUniformfv", V, U32, I32, P),
    sig!("glGetUniformiv", V, U32, I32, P),
    sig!("glLogicOp", V, U32),
    sig!("glMultiDrawArrays", V, U32, P, P, I32),
    sig!("glMultiDrawElements", V, U32, P, U32, P, I32),
    sig!("glMultiDrawElementsBaseVertex", V, U32, P, U32, P, I32, P),
    sig!("glPixelStoref", V, U32, F32),
    sig!("glTexParameterfv", V, U32, U32, P),
    sig!("glTexParameteriv", V, U32, U32, P),
    sig!("glUniform1ui", V, I32, U32),
    sig!("glUniform1uiv", V, I32, I32, P),
    sig!("glUniform2ui", V, I32, U32, U32),
    sig!("glUniform2uiv", V, I32, I32, P),
    sig!("glUniform3ui", V, I32, U32, U32, U32),
    sig!("glUniform3uiv", V, I32, I32, P),
    sig!("glUniform4ui", V, I32, U32, U32, U32, U32),
    sig!("glUniform4uiv", V, I32, I32, P),
    sig!("glUniformMatrix2x3fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix2x4fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix3x2fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix3x4fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix4x2fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix4x3fv", V, I32, I32, U8, P),
    sig!("glVertexAttrib1f", V, U32, F32),
    sig!("glVertexAttrib2f", V, U32, F32, F32),
    sig!("glVertexAttrib3f", V, U32, F32, F32, F32),
    sig!("glVertexAttrib4f", V, U32, F32, F32, F32, F32),
    sig!("glVertexAttrib4fv", V, U32, P),
    sig!("glVertexAttribI4i", V, U32, I32, I32, I32, I32),
    sig!("glVertexAttribI4ui", V, U32, U32, U32, U32, U32),
    sig!("glMinSampleShading", V, F32),
    sig!("glPatchParameterfv", V, U32, P),
    sig!("glPatchParameteri", V, U32, I32),
    sig!("glPopGroupMarkerEXT", V),
    sig!("glProgramUniform1f", V, U32, I32, F32),
    sig!("glProgramUniform1i", V, U32, I32, I32),
    sig!("glProgramUniform4fv", V, U32, I32, I32, P),
    sig!("glProgramUniformMatrix4fv", V, U32, I32, I32, U8, P),
    sig!("glPushGroupMarkerEXT", V, I32, P),
    sig!("glQueryCounter", V, U32, U32),
    sig!("glScissorArrayv", V, U32, I32, P),
    sig!("glTextureBarrierNV", V),
    sig!("glTextureRangeAPPLE", V, U32, I32, P),
    sig!("glTransformFeedbackVaryings", V, U32, I32, P, U32),
    sig!("glUniformSubroutinesuiv", V, U32, I32, P),
    sig!("glUseProgramStages", V, U32, U32, U32),
    sig!("glVertexAttribLPointer", V, U32, I32, U32, I32, P),
    sig!("glViewportArrayv", V, U32, I32, P),
    sig!("glMapBufferRange", P, U32, I64, I64, U32),
    sig!("glObjectLabel", V, U32, U32, I32, P),
    sig!("glPointSize", V, F32),
    sig!("glPolygonMode", V, U32, U32),
    sig!("glPolygonOffset", V, F32, F32),
    sig!("glPopDebugGroup", V),
    sig!("glPrimitiveRestartIndex", V, U32),
    sig!("glProgramBinary", V, U32, U32, P, I32),
    sig!("glProgramParameteri", V, U32, U32, I32),
    sig!("glProvokingVertex", V, U32),
    sig!("glPushDebugGroup", V, U32, U32, I32, P),
    sig!("glReadBuffer", V, U32),
    sig!(
        "glRenderbufferStorageMultisample",
        V,
        U32,
        I32,
        U32,
        I32,
        I32
    ),
    sig!("glSampleCoverage", V, F32, U8),
    sig!("glSampleMaski", V, U32, U32),
    sig!("glSamplerParameterf", V, U32, U32, F32),
    sig!("glSamplerParameterfv", V, U32, U32, P),
    sig!("glSamplerParameteri", V, U32, U32, I32),
    sig!("glStencilFuncSeparate", V, U32, U32, I32, U32),
    sig!("glStencilMaskSeparate", V, U32, U32),
    sig!("glStencilOpSeparate", V, U32, U32, U32, U32),
    sig!("glTexBuffer", V, U32, U32, U32),
    sig!("glTexImage2DMultisample", V, U32, I32, U32, I32, I32, U8),
    sig!("glTexStorage2D", V, U32, I32, U32, I32, I32),
    sig!("glTexStorage3D", V, U32, I32, U32, I32, I32, I32),
    sig!(
        "glTexImage3D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        U32,
        U32,
        P
    ),
    sig!(
        "glTexSubImage3D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        U32,
        U32,
        P
    ),
    sig!(
        "glCompressedTexSubImage3D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        U32,
        I32,
        P
    ),
    sig!(
        "glCopyTexSubImage3D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32
    ),
    sig!(
        "glBlitFramebuffer",
        V,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        U32,
        U32
    ),
    sig!("glTexStorage1D", V, U32, I32, U32, I32),
    sig!(
        "glCompressedTexImage3D",
        V,
        U32,
        I32,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        P
    ),
    sig!("glFramebufferTexture3D", V, U32, U32, U32, U32, I32, I32),
    sig!("glGetCompressedTexImage", V, U32, I32, P),
    sig!("glGetTexImage", V, U32, I32, U32, U32, P),
    sig!("glUniformBlockBinding", V, U32, U32, U32),
    sig!("glValidateProgram", V, U32),
    sig!("glVertexAttribDivisor", V, U32, U32),
    sig!("glVertexAttribIPointer", V, U32, I32, U32, I32, P),
    sig!("glWaitSync", V, P, U32, U64),
    sig!("glTexParameterf", V, U32, U32, F32),
    sig!("glTexParameteri", V, U32, U32, I32),
    sig!(
        "glTexSubImage2D",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        U32,
        U32,
        P
    ),
    sig!("glUniform1f", V, I32, F32),
    sig!("glUniform1fv", V, I32, I32, P),
    sig!("glUniform1i", V, I32, I32),
    sig!("glUniform1iv", V, I32, I32, P),
    sig!("glUniform2f", V, I32, F32, F32),
    sig!("glUniform2fv", V, I32, I32, P),
    sig!("glUniform2i", V, I32, I32, I32),
    sig!("glUniform2iv", V, I32, I32, P),
    sig!("glUniform3f", V, I32, F32, F32, F32),
    sig!("glUniform3fv", V, I32, I32, P),
    sig!("glUniform3i", V, I32, I32, I32, I32),
    sig!("glUniform3iv", V, I32, I32, P),
    sig!("glUniform4f", V, I32, F32, F32, F32, F32),
    sig!("glUniform4fv", V, I32, I32, P),
    sig!("glUniform4i", V, I32, I32, I32, I32, I32),
    sig!("glUniform4iv", V, I32, I32, P),
    sig!("glUniformMatrix2fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix3fv", V, I32, I32, U8, P),
    sig!("glUniformMatrix4fv", V, I32, I32, U8, P),
    sig!("glUnmapBuffer", U8, U32),
    sig!("glUseProgram", V, U32),
    sig!("glVertex2f", V, F32, F32),
    sig!("glVertexAttribPointer", V, U32, I32, U32, U8, I32, P),
    sig!("glViewport", V, I32, I32, I32, I32),
    sig!("glBufferStorage", V, U32, I64, P, U32),
    cbsig!([0], "glDebugMessageCallback", V, P, P),
    cbsig!([0], "glDebugMessageCallbackARB", V, P, P),
    sig!("glDebugMessageControlARB", V, U32, U32, U32, I32, P, U8),
    sig!("glDebugMessageInsertARB", V, U32, U32, U32, U32, I32, P),
    sig!("glGetDebugMessageLog", U32, U32, I32, P, P, P, P, P, P),
    sig!("glGetDebugMessageLogARB", U32, U32, I32, P, P, P, P, P, P),
    sig!("glGetObjectLabel", V, U32, U32, I32, P, P),
    sig!("glGetObjectPtrLabel", V, P, I32, P, P),
    sig!("glInvalidateBufferData", V, U32),
    sig!("glInvalidateBufferSubData", V, U32, I64, I64),
    sig!("glInvalidateFramebuffer", V, U32, I32, P),
    sig!(
        "glInvalidateSubFramebuffer",
        V,
        U32,
        I32,
        P,
        I32,
        I32,
        I32,
        I32
    ),
    sig!("glInvalidateTexImage", V, U32, I32),
    sig!(
        "glInvalidateTexSubImage",
        V,
        U32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32,
        I32
    ),
    sig!("glNamedBufferStorageEXT", V, U32, I64, P, U32),
    sig!("glObjectPtrLabel", V, P, I32, P),
    sig!("glTextureStorage1DEXT", V, U32, U32, I32, U32, I32),
    sig!("glTextureStorage2DEXT", V, U32, U32, I32, U32, I32, I32),
    sig!(
        "glTextureStorage3DEXT",
        V,
        U32,
        U32,
        I32,
        U32,
        I32,
        I32,
        I32
    ),
    // `glob`'s error callback is guest code called once per unreadable
    // directory.
    cbsig!([2], "glob", I32, P, I32, P, P),
    // Frees what `glob` allocated, and must be the shim's own: the host's would
    // read `gl_pathv` out of a Darwin `glob_t` at the wrong offset.
    sig!("globfree", V, P),
    sig!("gluOrtho2D", V, F64, F64, F64, F64),
    sig!("gmtime", P, P),
    sig!("gmtime_r", P, P, P),
    sig!("group_from_gid", P, U32, I32),
    // `strhash.h`'s table, still exported by libSystem. The replace function is
    // guest code, called when a key is already present.
    sig!("hash_create", P, I32),
    cbsig!([3], "hash_search", P, P, P, P, P),
    // The rest of the same header, whose callbacks are guest code for the same
    // reason. `libYoYoGamepad` imports the pair above; a table that stops there
    // refuses the teardown of a table it was happy to build.
    cbsig!([2], "hash_destroy", V, P, P, P),
    cbsig!([1], "hash_purge", V, P, P),
    cbsig!([1], "hash_traverse", V, P, P, P),
    sig!("herror", V, P),
    // Two `host_*` calls Valve's tier0 makes to size physical memory.
    sig!("host_get_clock_service", I32, U32, U32, P),
    //
    // Every entry below is a MIG routine: the arguments are `mach_port_t`,
    // `integer_t` and friends - fixed-width 32-bit types by definition, not
    // `long` - and the structures behind the pointers are arrays of those.
    sig!("host_page_size", I32, U32, P),
    sig!("host_statistics", I32, U32, I32, P, P),
    // The 64-bit flavours, which differ from the pair above only in that the
    // structure they fill has 64-bit counters.
    sig!("host_statistics64", I32, U32, I32, P, P),
    sig!("host_info", I32, U32, I32, P, P),
    sig!("hypot", F64, F64, F64),
    sig!("humanize_number", I32, P, U64, I64, P, I32, I32),
    sig!("expand_number", I32, P, P),
    sig!("hypotf", F32, F32, F32),
    sig!("iconv", U64, P, P, P, P, P),
    sig!("iconv_close", I32, P),
    sig!("iconv_open", P, P, P),
    sig!("if_indextoname", P, U32, P),
    sig!("if_nametoindex", U32, P),
    sig!("ilogb", I32, F64),
    sig!("ilogbf", I32, F32),
    sig!("inet_addr", U32, P),
    sig!("inet_aton", I32, P, P),
    sig!("inet_ntop", P, I32, P, P, U32),
    sig!("inet_pton", I32, I32, P, P),
    sig!("inflate", I32, P, I32),
    sig!("inflateEnd", I32, P),
    sig!("inflateInit2_", I32, P, I32, P, I32),
    sig!("inflateInit_", I32, P, P, I32),
    sig!("inflateReset", I32, P),
    sig!("initgroups", I32, P, I32),
    vsig!("ioctl", I32, I32, U64),
    sig!("isalnum", I32, I32),
    sig!("isalpha", I32, I32),
    sig!("isascii", I32, I32),
    sig!("isatty", I32, I32),
    sig!("issetugid", I32),
    sig!("isdigit", I32, I32),
    sig!("islower", I32, I32),
    sig!("isprint", I32, I32),
    sig!("ispunct", I32, I32),
    sig!("isspace", I32, I32),
    sig!("isupper", I32, I32),
    sig!("isxdigit", I32, I32),
    // BSD event notification, which is how `steamclient.dylib` waits on its
    // sockets and its pipe to the client.
    sig!("kevent", I32, I32, P, I32, P, I32, P),
    sig!("kill", I32, I32, I32),
    sig!("kqueue", I32),
    sig!("labs", I64, I64),
    sig!("ldexp", F64, F64, I32),
    sig!("ldexpf", F32, F32, I32),
    sig!("listen", I32, I32, I32),
    sig!("listxattr", I64, P, P, U64, I32),
    // `struct lconv *localeconv(void)` - the current locale's numeric
    // punctuation, which C++'s `num_put` reads on the way to formatting a
    // number. A pointer to storage the host owns.
    sig!("localeconv", P),
    sig!("localeconv_l", P, P),
    sig!("localtime", P, P),
    // `localtime_r` and `gmtime_r` above: the reentrant forms, which write into
    // a `struct tm` the *caller* owns.
    sig!("localtime_r", P, P, P),
    sig!("lockf", I32, I32, I32, I64),
    // `const char *locale_charset(void)` - libiconv's, not libc's, and the way
    // Mono asks what encoding the current locale uses before it converts a
    // managed string.
    sig!("locale_charset", P),
    sig!("log", F64, F64),
    sig!("log10", F64, F64),
    sig!("log10f", F32, F32),
    sig!("log1p", F64, F64),
    sig!("log1pf", F32, F32),
    sig!("log2", F64, F64),
    sig!("log2f", F32, F32),
    sig!("logb", F64, F64),
    sig!("logbf", F32, F32),
    sig!("logf", F32, F32),
    // The integer-returning rounding family.
    sig!("llrint", I64, F64),
    sig!("llrintf", I64, F32),
    sig!("llround", I64, F64),
    sig!("llroundf", I64, F32),
    // `longjmp` and `setjmp` are *shimmed*, not forwarded.
    sig!("longjmp", V, P, I32),
    sig!("lrint", I64, F64),
    sig!("lrintf", I64, F32),
    sig!("lround", I64, F64),
    sig!("lroundf", I64, F32),
    sig!("lseek", I64, I32, I64, I32),
    sig!("lstat", I32, P, P),
    sig!("lutimes", I32, P, P),
    sig!("mach_absolute_time", U64),
    sig!("mach_host_self", U32),
    // The three Mach IPC calls `libsteam_api.dylib` makes to talk to the Steam
    // client. They need no translation at all.
    sig!("mach_msg", I32, P, I32, U32, U32, U32, U32, U32),
    sig!("mach_msg_destroy", V, P),
    sig!("mach_port_allocate", I32, U32, U32, P),
    sig!("mach_port_deallocate", I32, U32, U32),
    // `kern_return_t mach_port_extract_right(ipc_space_t, mach_port_name_t,
    // mach_msg_type_name_t, mach_port_t *poly, mach_msg_type_name_t *polyPoly)`
    // - the two out-parameters are a 32-bit port name and a 32-bit type name,
    // written through pointers the guest owns.
    sig!("mach_port_extract_right", I32, U32, U32, U32, P, P),
    sig!("mach_port_insert_right", I32, U32, U32, U32, U32),
    sig!("mach_port_mod_refs", I32, U32, U32, U32, I32),
    sig!("mach_port_type", I32, U32, U32, P),
    //
    // Passthrough, and unlike `mmap` they need no shim: none of these carries
    // a protection argument, so none of them can hand the guest a page this
    // CPU would be asked to execute.
    sig!("mach_make_memory_entry_64", I32, U32, P, U64, I32, P, U32),
    sig!("mach_vm_allocate", I32, U32, P, U64, I32),
    sig!("mach_vm_deallocate", I32, U32, U64, U64),
    sig!("mach_vm_region", I32, U32, P, P, I32, P, P, P),
    sig!("mach_vm_read_overwrite", I32, U32, U64, U64, U64, P),
    sig!("vm_allocate", I32, U32, P, U64, I32),
    sig!("vm_deallocate", I32, U32, U64, U64),
    sig!("vm_read_overwrite", I32, U32, U64, U64, U64, P),
    sig!("vm_region_64", I32, U32, P, P, I32, P, P, P),
    sig!("vm_region_recurse_64", I32, U32, P, P, P, P, P),
    sig!("mach_thread_self", U32),
    sig!("mach_timebase_info", I32, P),
    sig!("madvise", U64, P, U64, I32),
    sig!("malloc", P, U64),
    sig!("mblen", I32, P, U64),
    sig!("malloc_size", U64, P),
    sig!("malloc_zone_statistics", V, P, P),
    sig!("mbrlen_l", U64, P, U64, P, P),
    sig!("mbrtowc", U64, P, P, U64, P),
    sig!("mbrlen", U64, P, U64, P),
    sig!("mbsinit", I32, P),
    sig!("mbr_identifier_translate", I32, I32, P, U64, I32, P, P),
    sig!("mbrtowc_l", U64, P, P, U64, P, P),
    sig!("mbsnrtowcs_l", U64, P, P, U64, U64, P, P),
    sig!("mbsrtowcs", U64, P, P, U64, P),
    sig!("mbsrtowcs_l", U64, P, P, U64, P, P),
    sig!("mbstowcs", U64, P, P, U64),
    sig!("mbtowc", I32, P, P, U64),
    // `int mbtowc_l(wchar_t *, const char *, size_t, locale_t)`.
    sig!("mbtowc_l", I32, P, P, U64, P),
    sig!("memchr", P, P, I32, U64),
    sig!("memcmp", I32, P, P, U64),
    sig!("memcpy", P, P, P, U64),
    sig!("memmove", P, P, P, U64),
    sig!("memset", P, P, I32, U64),
    // `errno_t memset_s(void *s, rsize_t smax, int c, rsize_t n)` - Annex K's
    // memset, and the reason a program uses it is that the compiler may not
    // optimize it away.
    sig!("memset_s", I32, P, U64, I32, U64),
    sig!("memset_pattern16", V, P, P, U64),
    // Answered honestly rather than usefully.
    sig!("mincore", I32, P, U64, P),
    sig!("mkdir", I32, P, U32),
    sig!("mkdtemp", P, P),
    sig!("mknod", I32, P, U32, I32),
    sig!("mkstemp", I32, P),
    sig!("mktemp", P, P),
    sig!("mktime", I64, P),
    sig!("mlock", I32, P, U64),
    //
    // Passthrough, with one caveat: the guest was compiled expecting 4 KiB pages and
    // Apple Silicon enforces 16 KiB, so a guest rounding a length or an address to
    // its own page size gets a request the kernel rounds again. Every use in a real
    // program is either a whole allocation or an `mprotect` of a region the same
    // program `mmap`ed, both of which survive the coarser granularity. A guest doing
    // page arithmetic against `getpagesize` would not.
    sig!("mmap", P, P, U64, I32, I32, I32, I64),
    sig!("modf", F64, F64, P),
    sig!("modff", F32, F32, P),
    sig!("mprotect", I32, P, U64, I32),
    sig!("msync", I32, P, U64, I32),
    sig!("munlock", I32, P, U64),
    sig!("munmap", I32, P, U64),
    sig!("nanosleep", I32, P, P),
    sig!("nearbyintf", F32, F32),
    sig!("newlocale", P, I32, P, P),
    sig!("nextafter", F64, F64, F64),
    sig!("nl_langinfo", P, I32),
    sig!("nl_langinfo_l", P, I32, P),
    // Fixed-arity adapters for x86-64 imports. Native Apple arm64 retains
    // the host's variadic ABI, whose optional arguments are on the stack.
    sig!("open", I32, P, I32, U32),
    sig!("openat", I32, I32, P, I32, U32),
    sig!("opendir", P, P),
    sig!("openlog", V, P, I32, I32),
    sig!("pathconf", I64, P, I32),
    sig!("pclose", I32, P),
    sig!("perror", V, P),
    sig!("pipe", I32, P),
    // Standardized in POSIX.1-2024, added in macOS 27.
    // Thanks for finally adding this, Apple :)
    sig!("pipe2", I32, P, I32),
    // `int poll(struct pollfd *, nfds_t, int)`.
    sig!("poll", I32, P, U32, I32),
    sig!("popen", P, P, P),
    sig!("posix_madvise", I32, P, U64, I32),
    sig!("posix_memalign", I32, P, U64, U64),
    // `posix_spawn` and its two attribute objects, which is how the Steam
    // client starts a helper process.
    sig!("posix_spawn", I32, P, P, P, P, P, P),
    // `posix_spawnp` differs from `posix_spawn` only in searching `PATH`, so it
    // takes the same six arguments.
    sig!("posix_spawnp", I32, P, P, P, P, P, P),
    sig!("posix_spawn_file_actions_addchdir_np", I32, P, P),
    sig!("posix_spawn_file_actions_addfchdir_np", I32, P, I32),
    // `int posix_spawn_file_actions_addinherit_np(posix_spawn_file_actions_t *,
    // int)` - "keep this descriptor open across the spawn", which is how a game
    // hands its crash handler the pipe it will report on.
    sig!("posix_spawn_file_actions_addinherit_np", I32, P, I32),
    sig!("posix_spawn_file_actions_adddup2", I32, P, I32, I32),
    sig!("posix_spawn_file_actions_addclose", I32, P, I32),
    sig!("posix_spawn_file_actions_addopen", I32, P, I32, P, I32, U16),
    sig!("posix_spawn_file_actions_destroy", I32, P),
    sig!("posix_spawn_file_actions_init", I32, P),
    sig!("posix_spawnattr_destroy", I32, P),
    sig!("posix_spawnattr_init", I32, P),
    sig!("posix_spawnattr_getflags", I32, P, P),
    sig!("posix_spawnattr_getpgroup", I32, P, P),
    sig!("posix_spawnattr_getsigmask", I32, P, P),
    sig!("posix_spawnattr_getsigdefault", I32, P, P),
    sig!("posix_spawnattr_setpgroup", I32, P, I32),
    sig!("posix_spawnattr_setbinpref_np", I32, P, U64, P, P),
    sig!("posix_spawnattr_setarchpref_np", I32, P, U64, P, P, P),
    // `short flags`, which SysV passes in the low half of a register and
    // AAPCS64 reads back out of one: narrower than the slot on both sides.
    sig!("posix_spawnattr_setflags", I32, P, U32),
    sig!("posix_spawnattr_setsigmask", I32, P, P),
    sig!("pow", F64, F64, F64),
    sig!("powf", F32, F32, F32),
    sig!("pread", I64, I32, P, U64, I64),
    vsig!("printf", I32, P),
    sig!("proc_name", I32, I32, P, U32),
    // `int proc_pidinfo(int pid, int flavour, uint64_t arg, void *buffer, int
    // buffersize)` - the general form of `proc_pidpath` above, and how a
    // program asks the kernel about its own task.
    sig!("proc_pidinfo", I32, I32, I32, U64, P, I32),
    sig!("proc_pid_rusage", I32, I32, I32, P),
    sig!("kpersona_pidinfo", I32, I32, P),
    sig!("proc_listpids", I32, U32, U32, P, I32),
    sig!("proc_listallpids", I32, P, I32),
    sig!("memorystatus_control", I32, U32, I32, U32, P, U64),
    sig!("proc_pidpath", I32, I32, P, U32),
    sig!("psignal", V, U32, P),
    cbsig!([0, 1, 2], "pthread_atfork", I32, P, P, P),
    //
    // Every opaque pthread type is byte-identical on x86_64 and arm64 macOS:
    // `pthread_mutex_t` is 64 bytes on both, `pthread_cond_t` 48,
    // `pthread_rwlock_t` 200, `pthread_attr_t` 64, `pthread_t` a pointer. So the
    // guest allocates one on its own stack and the host's libpthread operates on
    // it in place. (`pthread_types_agree_across_architectures` checks this
    // rather than trusting it.)
    //
    // `pthread_create` is the exception. Its start routine goes out as a thunk,
    // and the host calls it on a brand-new host thread. The attribute set is
    // where "the layouts agree, so operate in place" stops being enough: a guest
    // thread has two stacks - the host's for the JIT's frames and the guest's
    // for its own - and every stack attribute is about the second while
    // passthrough applied it to the first. The entry stays for its signature and
    // its callback index; the call is serviced by [`crate::shims::pthread`].
    sig!("pthread_attr_destroy", I32, P),
    // Every getter in this family is `(const pthread_attr_t *, T *)` and every
    // setter takes its value directly, which is why the two halves do not read
    // symmetrically.
    sig!("pthread_attr_getdetachstate", I32, P, P),
    sig!("pthread_attr_getguardsize", I32, P, P),
    sig!("pthread_attr_getinheritsched", I32, P, P),
    sig!("pthread_attr_getschedparam", I32, P, P),
    sig!("pthread_attr_getschedpolicy", I32, P, P),
    sig!("pthread_attr_getscope", I32, P, P),
    sig!("pthread_attr_getstack", I32, P, P, P),
    sig!("pthread_attr_getstackaddr", I32, P, P),
    sig!("pthread_attr_getstacksize", I32, P, P),
    sig!("pthread_attr_init", I32, P),
    sig!("pthread_attr_setdetachstate", I32, P, I32),
    sig!("pthread_attr_setguardsize", I32, P, U64),
    sig!("pthread_attr_setinheritsched", I32, P, I32),
    sig!("pthread_attr_setscope", I32, P, I32),
    sig!("pthread_attr_setstack", I32, P, P, U64),
    sig!("pthread_attr_setstackaddr", I32, P, P),
    // `struct sched_param` is `int sched_priority` and an opaque tail, and the
    // guest passes it by address - the same in-place arrangement every other
    // `pthread` type here rests on. The QoS surface.
    sig!("pthread_attr_set_qos_class_np", I32, P, U32, I32),
    sig!("pthread_attr_get_qos_class_np", I32, P, P, P),
    sig!("pthread_attr_setschedparam", I32, P, P),
    sig!("pthread_attr_setschedpolicy", I32, P, I32),
    sig!("pthread_attr_setstacksize", I32, P, U64),
    sig!("pthread_cond_broadcast", I32, P),
    sig!("pthread_cond_destroy", I32, P),
    sig!("pthread_cond_init", I32, P, P),
    sig!("pthread_cond_signal", I32, P),
    sig!("pthread_cond_timedwait", I32, P, P, P),
    sig!("pthread_cond_timedwait_relative_np", I32, P, P, P),
    sig!("pthread_cond_wait", I32, P, P),
    sig!("pthread_condattr_destroy", I32, P),
    sig!("pthread_condattr_init", I32, P),
    sig!("pthread_cpu_number_np", I32, P),
    cbsig!([2], "pthread_create", I32, P, P, P, P),
    sig!("pthread_detach", I32, U64),
    sig!("pthread_equal", I32, U64, U64),
    // Terminates the calling host thread from inside our thunk: the Rust frames
    // between here and the JIT entry are abandoned rather than unwound.
    sig!("pthread_exit", V, P),
    sig!("pthread_get_qos_class_np", I32, U64, P, P),
    sig!("pthread_get_stackaddr_np", P, U64),
    sig!("pthread_get_stacksize_np", U64, U64),
    sig!("pthread_getname_np", I32, U64, P, U64),
    sig!("pthread_getschedparam", I32, U64, P, P),
    sig!("pthread_getspecific", P, U64),
    sig!("pthread_jit_write_protect_np", I32, I32),
    sig!("pthread_join", I32, U64, P),
    // The destructor is guest code, called by libpthread on thread teardown.
    cbsig!([1], "pthread_key_create", I32, P, P),
    sig!("pthread_key_delete", I32, U64),
    // Signals a *host* thread, which is what a guest thread is here, so the
    // disposition it reaches is the one `shims::signal` installed.
    sig!("pthread_kill", I32, U64, I32),
    // The thread's Mach port name. One task, one port namespace - see
    // `mach_task_self_` in `shims::runtime`.
    sig!("pthread_mach_thread_np", U32, U64),
    sig!("pthread_main_np", I32),
    sig!("pthread_mutex_destroy", I32, P),
    sig!("pthread_mutex_init", I32, P, P),
    sig!("pthread_mutex_lock", I32, P),
    sig!("pthread_mutex_trylock", I32, P),
    sig!("pthread_mutex_unlock", I32, P),
    sig!("pthread_mutexattr_destroy", I32, P),
    sig!("pthread_mutexattr_init", I32, P),
    sig!("pthread_mutexattr_setprotocol", I32, P, I32),
    sig!("pthread_mutexattr_getprotocol", I32, P, P),
    sig!("pthread_mutexattr_settype", I32, P, I32),
    sig!("pthread_mutexattr_gettype", I32, P, P),
    sig!("pthread_mutexattr_setpshared", I32, P, I32),
    sig!("pthread_mutexattr_getpshared", I32, P, P),
    sig!("pthread_condattr_setpshared", I32, P, I32),
    sig!("pthread_condattr_getpshared", I32, P, P),
    sig!("pthread_rwlockattr_init", I32, P),
    sig!("pthread_rwlockattr_destroy", I32, P),
    sig!("pthread_rwlockattr_setpshared", I32, P, I32),
    sig!("pthread_rwlockattr_getpshared", I32, P, P),
    // The two cancellation settings, which carry Darwin's own numbering - inverted
    // relative to the host's. `pthread_cancel` itself stays refused below: setting the
    // state is harmless, and acting on it is what unwinds a stack nothing here saw.
    sig!("pthread_testcancel", V),
    sig!("pthread_yield_np", I32),
    sig!("pthread_getconcurrency", I32),
    sig!("pthread_setconcurrency", I32, I32),
    // The initialiser is guest code, and `pthread_once` guarantees it runs
    // exactly once - a guarantee that survives thunking, since the thunk is
    // stable per guest address.
    cbsig!([1], "pthread_once", I32, P, P),
    sig!("pthread_rwlock_destroy", I32, P),
    sig!("pthread_rwlock_init", I32, P, P),
    sig!("pthread_rwlock_rdlock", I32, P),
    sig!("pthread_rwlock_unlock", I32, P),
    sig!("pthread_rwlock_wrlock", I32, P),
    sig!("pthread_self", U64),
    // Cancellation state and type are two `int`s in and the previous value out
    // through a pointer. Nothing here is a guest callback.
    sig!("pthread_set_qos_class_self_np", I32, U32, I32),
    sig!("pthread_setcancelstate", I32, I32, P),
    sig!("pthread_setcanceltype", I32, I32, P),
    sig!("pthread_setname_np", I32, P),
    sig!("pthread_setschedparam", I32, U64, I32, P),
    sig!("pthread_setspecific", I32, U64, P),
    // `sigset_t` is a `uint32_t` on both architectures - the fact `struct
    // sigaction` rests on - so the two masks are pointers to four bytes the
    // host reads and writes in place.
    sig!("pthread_sigmask", I32, I32, P, P),
    sig!("pthread_threadid_np", I32, U64, P),
    sig!("putc", I32, I32, P),
    sig!("putchar", I32, I32),
    sig!("putenv", I32, P),
    sig!("_setenvp", I32, P, P, I32, P, P),
    sig!("_unsetenvp", I32, P, P, P),
    sig!("_copyenv", P, P),
    sig!("_cthread_init", V),
    sig!("sysdir_start_search_path_enumeration", U32, U32, U32),
    sig!("sysdir_get_next_search_path_enumeration", U32, U32, P),
    sig!("__keymgr_dwarf2_register_sections", V),
    sig!("puts", I32, P),
    sig!("putwchar", I32, I32),
    sig!("pwrite", I64, I32, P, U64, I64),
    cbsig!([3], "qsort", V, P, U64, U64, P),
    cbsig!([4], "qsort_r", V, P, U64, U64, P, P),
    // `raise` and `kill` deliver to this process, so a guest handler installed
    // through [`crate::signal`] is reached exactly the way the kernel reaches
    // any other: asynchronously, on whichever thread the kernel picks.
    sig!("raise", I32, I32),
    sig!("rand", I32),
    sig!("random", I64),
    sig!("read", I64, I32, P, U64),
    sig!("readdir", P, P),
    sig!("readdir_r", I32, P, P, P),
    sig!("readlink", I64, P, P, U64),
    sig!("readlinkat", I64, I32, P, P, U64),
    sig!("readv", I64, I32, P, I32),
    sig!("reboot", I32, I32),
    sig!("realloc", P, P, U64),
    sig!("reallocf", P, P, U64),
    // Darwin's `regex_t` and `regmatch_t` have the same pointer/size_t/off_t
    // layout on x86_64 and arm64, so libc can use the guest's buffers directly.
    sig!("regcomp", I32, P, P, I32),
    sig!("regncomp", I32, P, P, U64, I32),
    sig!("regerror", U64, I32, P, P, U64),
    sig!("regexec", I32, P, P, U64, P, I32),
    sig!("regnexec", I32, P, P, U64, U64, P, I32),
    sig!("regfree", V, P),
    sig!("recv", I64, I32, P, U64, I32),
    sig!("recvfrom", I64, I32, P, U64, I32, P, P),
    sig!("recvmsg", I64, I32, P, I32),
    sig!("remove", I32, P),
    sig!("rename", I32, P, P),
    sig!("rewind", V, P),
    // `strrchr` under the name BSD gave it first. `libsteam_api.dylib` uses it
    // to take the directory off its own path while looking for the client.
    sig!("rindex", P, P, I32),
    sig!("rint", F64, F64),
    sig!("rintf", F32, F32),
    sig!("rmdir", I32, P),
    sig!("round", F64, F64),
    sig!("roundf", F32, F32),
    sig!("scalbn", F64, F64, I32),
    sig!("scalbnf", F32, F32, I32),
    vsig!("scanf", I32, P),
    // `int scandir(const char *, struct dirent ***, int (*)(const struct
    // dirent *), int (*)(const struct dirent **, const struct dirent **))`
    sig!("scandir", I32, P, P, P, P),
    sig!("sched_get_priority_max", I32, I32),
    sig!("sched_get_priority_min", I32, I32),
    sig!("sched_yield", I32),
    // The import is `_select$1050`, the variant with the 10.5 `fd_set`
    // semantics; arm64 has only the one, and the `$`-suffix fallback finds it.
    sig!("select", I32, I32, P, P, P, P),
    // `pselect`'s last two arguments are a `timespec` and Darwin's 32-bit
    // `sigset_t`, neither of which `select` takes.
    sig!("pselect", I32, I32, P, P, P, P, P),
    // The named-semaphore surface. `sem_open` is `open`'s shape exactly.
    sig!("sem_close", I32, P),
    sig!("sem_open", P, P, I32, U32, U32),
    sig!("sem_post", I32, P),
    sig!("sem_trywait", I32, P),
    sig!("sem_unlink", I32, P),
    sig!("sem_wait", I32, P),
    // System V semaphores, which Steam's `libtier0` uses to name a lock across
    // processes.
    sig!("semaphore_create", I32, U32, P, I32, I32),
    sig!("semaphore_destroy", I32, U32, U32),
    sig!("semaphore_signal", I32, U32),
    // The `mach_timespec_t` by value; see `clock_sleep` for why `U64` is the
    // right placement rather than an approximation of one.
    sig!("semaphore_timedwait", I32, U32, U64),
    sig!("semaphore_wait", I32, U32),
    sig!("semget", I32, I32, I32, I32),
    sig!("semop", I32, I32, P, U64),
    sig!("send", I64, I32, P, U64, I32),
    sig!("sendmsg", I64, I32, P, I32),
    sig!("sendto", I64, I32, P, U64, I32, P, U32),
    sig!("setbuf", V, P, P),
    sig!("setdomainname", I32, P, I32),
    sig!("setenv", I32, P, P, I32),
    sig!("setgid", I32, U32),
    sig!("setregid", I32, U32, U32),
    sig!("setreuid", I32, U32, U32),
    sig!("setgrent", V),
    sig!("setgroups", I32, I32, P),
    sig!("sethostid", V, I64),
    sig!("sethostname", I32, P, I32),
    sig!("setjmp", I32, P),
    sig!("setlocale", P, I32, P),
    sig!("setmode", P, P),
    sig!("setprogname", V, P),
    sig!("setpgid", I32, I32, I32),
    sig!("setpriority", I32, I32, U32, I32),
    sig!("setpwent", V),
    sig!("setrlimit", I32, I32, P),
    // `pid_t setsid(void)`. `setpgrp` is deliberately absent beside it.
    sig!("setsid", I32),
    sig!("setsockopt", I32, I32, I32, I32, P, U32),
    sig!("settimeofday", I32, P, P),
    sig!("setuid", I32, U32),
    sig!("seteuid", I32, U32),
    sig!("setegid", I32, U32),
    sig!("getgrouplist", I32, P, I32, P, P),
    sig!("setgroupent", I32, I32),
    sig!("setpassent", I32, I32),
    sig!("mkdtempat_np", P, I32, P),
    sig!("getauid", I32, P),
    sig!("setaudit_addr", I32, P, U32),
    // Variadic, but `darwin::audit` reads none of the format's arguments, so
    // only the named ones are carried.
    sig!("audit_submit", I32, I16, U32, U8, I32, P),
    // System Integrity Protection, which `darwin::sip` says is off.
    sig!("csr_check", I32, U32),
    sig!("rootless_restricted_environment", I32),
    sig!("rootless_check_trusted_fd", I32, I32),
    // OpenPAM, `darwin::pam`. The conversation function arrives inside a
    // `struct pam_conv` and is only stored, never called from the host side;
    // a data cleanup is called, from `pam_end`, so it is a callback.
    sig!("pam_start", I32, P, P, P, P),
    rsig!("pam_end", I32, P, I32),
    sig!("pam_authenticate", I32, P, I32),
    sig!("pam_setcred", I32, P, I32),
    sig!("pam_acct_mgmt", I32, P, I32),
    sig!("pam_open_session", I32, P, I32),
    sig!("pam_close_session", I32, P, I32),
    sig!("pam_chauthtok", I32, P, I32),
    sig!("pam_get_item", I32, P, I32, P),
    sig!("pam_set_item", I32, P, I32, P),
    sig!("pam_getenv", P, P, P),
    sig!("pam_putenv", I32, P, P),
    sig!("pam_getenvlist", P, P),
    sig!("pam_strerror", P, P, I32),
    cbsig!([3], "pam_set_data", I32, P, P, P, P),
    sig!("pam_get_data", I32, P, P, P),
    sig!("openpam_ttyconv", I32, I32, P, P, P),
    sig!("setusershell", V),
    sig!("setvbuf", I32, P, P, I32, U64),
    sig!("shutdown", I32, I32, I32),
    // Shimmed, like the `setjmp` family and for a related reason.
    sig!("sigaction", I32, I32, P, P),
    // Installed honestly and never used.
    sig!("sigaltstack", I32, P, P),
    sig!("siglongjmp", V, P, I32),
    sig!("signal", P, I32, P),
    sig!("sigpending", I32, P),
    sig!("sigprocmask", I32, I32, P, P),
    sig!("sigsetjmp", I32, P, I32),
    sig!("sin", F64, F64),
    sig!("sinf", F32, F32),
    sig!("sinh", F64, F64),
    sig!("sinhf", F32, F32),
    sig!("sleep", U32, U32),
    vsig!("snprintf", I32, P, U64, P),
    sig!("socket", I32, I32, I32, I32),
    sig!("socketpair", I32, I32, I32, I32, P),
    vsig!("sprintf", I32, P, P),
    sig!("sqrt", F64, F64),
    sig!("sqrtf", F32, F32),
    sig!("srand", V, U32),
    sig!("srandom", V, U32),
    vsig!("sscanf", I32, P, P),
    sig!("stat", I32, P, P),
    sig!("statfs", I32, P, P),
    sig!("statvfs", I32, P, P),
    sig!("stpcpy", P, P, P),
    sig!("strcasecmp", I32, P, P),
    sig!("strcasestr", P, P, P),
    sig!("strcat", P, P, P),
    sig!("strchr", P, P, I32),
    sig!("strcmp", I32, P, P),
    sig!("strcoll", I32, P, P),
    sig!("strcoll_l", I32, P, P, P),
    sig!("strcpy", P, P, P),
    sig!("strcspn", U64, P, P),
    sig!("strdup", P, P),
    sig!("strerror", P, I32),
    sig!("strerror_r", I32, I32, P, U64),
    sig!("strftime", U64, P, U64, P, P),
    sig!("strftime_l", U64, P, U64, P, P, P),
    sig!("strlcat", U64, P, P, U64),
    sig!("strlcpy", U64, P, P, U64),
    sig!("strlen", U64, P),
    sig!("u_charDigitValue", I32, I32),
    sig!("strmode", V, I32, P),
    sig!("strncasecmp", I32, P, P, U64),
    sig!("strncat", P, P, P, U64),
    sig!("strncmp", I32, P, P, U64),
    sig!("strncpy", P, P, P, U64),
    sig!("strndup", P, P, U64),
    sig!("strnlen", U64, P, U64),
    sig!("strnstr", P, P, P, U64),
    sig!("strpbrk", P, P, P),
    // `strptime` writes into a guest `struct tm`, which is twelve `int`s and
    // two pointers on both architectures.
    sig!("strptime", P, P, P, P),
    sig!("strrchr", P, P, I32),
    sig!("strspn", U64, P, P),
    sig!("strsep", P, P, P),
    // `char *strsignal(int)`. A **pointer** return, and the reason to say so.
    sig!("strsignal", P, I32),
    //
    // A voucher is an opaque kernel object carried alongside a Mach message
    // so that work done on the receiving side is attributed - for QoS, for
    // activity tracing - to whoever asked for it.
    sig!("voucher_mach_msg_set", I32, P),
    sig!("voucher_mach_msg_clear", V, P),
    sig!("voucher_mach_msg_adopt", P, P),
    sig!("voucher_mach_msg_revert", V, P),
    sig!("voucher_copy", P),
    sig!("voucher_copy_without_importance", P),
    sig!("voucher_adopt", P, P),
    sig!("voucher_release", V, P),
    sig!("voucher_retain", P, P),
    sig!("strstr", P, P, P),
    sig!("strtod", F64, P, P),
    sig!("strtod_l", F64, P, P, P),
    sig!("strtof", F32, P, P),
    sig!("strtof_l", F32, P, P, P),
    sig!("strtok", P, P, P),
    sig!("strtok_r", P, P, P, P),
    sig!("strtol", I64, P, P, I32),
    sig!("strtoll", I64, P, P, I32),
    sig!("strtoll_l", I64, P, P, I32, I32),
    sig!("strtonum", I64, P, I64, I64, P),
    sig!("strtoul", U64, P, P, I32),
    sig!("strtoimax", I64, P, P, I32),
    sig!("strtoumax", U64, P, P, I32),
    sig!("strtoull", U64, P, P, I32),
    sig!("strvis", I32, P, P, I32),
    sig!("strtoull_l", U64, P, P, I32, I32),
    sig!("strxfrm_l", U64, P, P, U64, P),
    sig!("swab", V, P, P, I64),
    sig!("symlink", I32, P, P),
    sig!("sync", V),
    sig!("sysconf", I64, I32),
    sig!("sysctl", I32, P, U32, P, P, P, U64),
    sig!("sysctlbyname", I32, P, P, P, P, U64),
    sig!("sysctlnametomib", I32, P, P, P),
    vsig!("syslog", V, I32, P),
    // The string is the guest's and the shell is the host's, which is exactly
    // what a guest re-launching itself through `open steam://` wants.
    sig!("system", I32, P),
    sig!("shm_open", I32, P, I32, I32),
    sig!("shm_unlink", I32, P),
    sig!("tan", F64, F64),
    sig!("tanf", F32, F32),
    sig!("tanh", F64, F64),
    sig!("tanhf", F32, F32),
    sig!("task_for_pid", I32, U32, I32, P),
    sig!("task_name_for_pid", I32, U32, I32, P),
    sig!("task_read_for_pid", I32, U32, I32, P),
    sig!("task_inspect_for_pid", I32, U32, I32, P),
    sig!("host_processor_sets", I32, U32, P, P),
    sig!("processor_set_default", I32, U32, P),
    sig!("host_processor_set_priv", I32, U32, U32, P),
    sig!("processor_set_tasks", I32, U32, P, P),
    sig!("processor_set_tasks_with_flavor", I32, U32, U32, P, P),
    sig!("mach_port_space_basic_info", I32, U32, P),
    sig!("task_policy_get", I32, U32, U32, P, P, P),
    sig!("task_info", I32, U32, U32, P, P),
    sig!("task_threads", I32, U32, P, P),
    sig!("openpty", I32, P, P, P, P, P),
    sig!("tcflush", I32, I32, I32),
    sig!("tcgetattr", I32, I32, P),
    sig!("tcsendbreak", I32, I32, I32),
    sig!("tcsetattr", I32, I32, I32, P),
    sig!("tcgetpgrp", I32, I32),
    sig!("tcsetpgrp", I32, I32, I32),
    sig!("tempnam", P, P, P),
    sig!("tgetent", I32, P, P),
    sig!("tgetflag", I32, P),
    sig!("tgetnum", I32, P),
    sig!("tgetstr", P, P, P),
    sig!("tgoto", P, P, I32, I32),
    sig!("thread_abort", I32, U32),
    sig!("thread_abort_safely", I32, U32),
    // `thread_get_state` and `thread_set_state` pass through and will not do
    // what a guest asking for `x86_THREAD_STATE64` means by them.
    sig!("thread_get_state", I32, U32, I32, P, P),
    sig!("thread_info", I32, U32, U32, P, P),
    // The scheduling policies, which pass through and *do* mean what the guest
    // means by them - unlike the register-state pair above.
    sig!("thread_policy_get", I32, U32, U32, P, P, P),
    sig!("thread_policy_set", I32, U32, U32, P, U32),
    // The exception-port family.
    sig!("thread_get_exception_ports", I32, U32, U32, P, P, P, P, P),
    sig!("thread_set_exception_ports", I32, U32, U32, U32, I32, I32),
    sig!("thread_resume", I32, U32),
    sig!("thread_set_state", I32, U32, I32, P, U32),
    sig!("thread_suspend", I32, U32),
    sig!("time", I64, P),
    sig!("timingsafe_bcmp", I32, P, P, U64),
    sig!("timegm", I64, P),
    // The other spellings; see `_tlv_bootstrap` and `_tlv_atexit` above.
    cbsig!([0], "tlv_atexit", V, P, P),
    sig!("tlv_bootstrap", P, P),
    sig!("tmpfile", P),
    sig!("tolower", I32, I32),
    sig!("toupper", I32, I32),
    // The output function is called once per character.
    cbsig!([2], "tputs", I32, P, I32, P),
    sig!("trunc", F64, F64),
    sig!("truncate", I32, P, I64),
    sig!("truncf", F32, F32),
    sig!("ttyname", P, I32),
    sig!("ttyname_r", I32, I32, P, U64),
    sig!("umask", U32, U32),
    sig!("uname", I32, P),
    sig!("ungetc", I32, I32, P),
    sig!("ungetwc", I32, I32, P),
    sig!("unlink", I32, P),
    sig!("undelete", I32, P),
    sig!("unsetenv", I32, P),
    sig!("unvis", I32, P, I32, P, I32),
    sig!("vis", P, P, I32, I32, I32),
    sig!("user_from_uid", P, U32, I32),
    sig!("usleep", I32, U32),
    sig!("utime", I32, P, P),
    sig!("utimes", I32, P, P),
    sig!("uuid_unparse_upper", V, P, P),
    //
    // Not variadic - each takes a `va_list` its caller already built - and still
    // impossible to forward: the argument is a *guest* `va_list`, three fields and a
    // register save area laid out by SysV AMD64, and the host's `vsnprintf` would
    // read it as whatever arm64's happens to be. Shimmed; the signatures exist to
    // name them at an exit. See [`crate::shims::valist`].
    sig!("vasprintf", I32, P, P, P),
    sig!("vfprintf", I32, P, P, P),
    sig!("vprintf", I32, P, P),
    sig!("vsnprintf", I32, P, U64, P, P),
    // Shimmed. The wide form, and the one every Unreal Engine 3 string is built
    // by; see [`crate::shims::printf::vswprintf`].
    sig!("vswprintf", I32, P, U64, P, P),
    sig!("vsprintf", I32, P, P, P),
    sig!("wait", I32, P),
    sig!("wait3", I32, P, I32, P),
    sig!("wait4", I32, I32, P, I32, P),
    sig!("waitid", I32, I32, I32, P, I32),
    sig!("waitpid", I32, I32, P, I32),
    vsig!("warn", V, P),
    vsig!("warnx", V, P),
    vsig!("warnc", V, I32, P),
    sig!("vwarn", V, P, P),
    sig!("vwarnx", V, P, P),
    sig!("vwarnc", V, I32, P, P),
    sig!("wcrtomb_l", U64, P, I32, P, P),
    sig!("wcscat", P, P, P),
    // The second argument is a `wchar_t`, which is a 32-bit integer and not a
    // pointer - the one member of this family whose scalar is easy to misread.
    sig!("wcschr", P, P, I32),
    sig!("wcscmp", I32, P, P),
    sig!("wcsncmp", I32, P, P, U64),
    // `wcscpy` and `wcsstr` return `wchar_t *`, and both were `I32` - the same
    // truncation `OSAtomicAdd32` above is about, in the other direction and one
    // step worse, because the value is an address rather than a count.
    sig!("wcscpy", P, P, P),
    sig!("wcsstr", P, P, P),
    sig!("wcscoll_l", I32, P, P, P),
    sig!("wcslen", U64, P),
    sig!("wcrtomb", U64, P, I32, P),
    sig!("wcscoll", I32, P, P),
    sig!("wcscspn", U64, P, P),
    sig!("wctob", I32, I32),
    sig!("wctype", U64, P),
    sig!("iswctype", I32, U32, U64),
    sig!("wmemcpy", P, P, P, U64),
    sig!("wcsncat", P, P, P, U64),
    sig!("wcsncpy", P, P, P, U64),
    sig!("wcsnrtombs_l", U64, P, P, U64, U64, P, P),
    sig!("wcsrchr", P, P, I32),
    sig!("wcsrtombs", U64, P, P, U64, P),
    sig!("wcstod", F64, P, P),
    sig!("wcstof", F32, P, P),
    sig!("wcstol", I64, P, P, I32),
    sig!("wcstoll", I64, P, P, I32),
    sig!("wcstombs", U64, P, P, U64),
    sig!("wcstoul", U64, P, P, I32),
    sig!("wcstoull", U64, P, P, I32),
    sig!("wcsxfrm_l", U64, P, P, U64, P),
    sig!("wctob_l", I32, I32, P),
    sig!("wctomb", I32, P, I32),
    sig!("wcwidth", I32, I32),
    sig!("wmemchr", P, P, I32, U64),
    sig!("wmemcmp", I32, P, P, U64),
    sig!("write", I64, I32, P, U64),
    sig!("writev", I64, I32, P, I32),
    sig!("wtmpxname", I32, P),
    sig!("zError", P, I32),
    sig!("zlibVersion", P),
    // Metal symbols
    sig!("MTLCopyAllDevices", P),
    sig!("AudioConvertHostTimeToNanos", U64, U64),
    sig!("AudioGetCurrentHostTime", U64),
    // `AudioDeviceIOProcID` looks like a function pointer in the header - it is
    // typedef'd as one - and is not a callback argument anywhere but the
    // creating call.
    cbsig!([1], "AudioDeviceCreateIOProcID", I32, U32, P, P, P),
    sig!("AudioDeviceDestroyIOProcID", I32, U32, P),
    sig!("AudioDeviceStart", I32, U32, P),
    sig!("AudioDeviceStop", I32, U32, P),
    sig!("AudioHardwareUnload", I32),
    // The pre-10.5 HAL spellings. The listener is never called, so it is not a
    // callback argument.
    sig!("AudioHardwareGetPropertyInfo", I32, U32, P, P),
    sig!("AudioHardwareGetProperty", I32, U32, P, P),
    sig!("AudioHardwareAddPropertyListener", I32, U32, P, P),
    sig!("AudioDeviceGetPropertyInfo", I32, U32, U32, BOOL, U32, P, P),
    sig!("AudioDeviceGetProperty", I32, U32, U32, BOOL, U32, P, P),
    sig!(
        "AudioDeviceSetProperty",
        I32,
        U32,
        P,
        U32,
        BOOL,
        U32,
        U32,
        P
    ),
    sig!("AudioObjectHasProperty", BOOL, U32, P),
    sig!("AudioObjectIsPropertySettable", I32, U32, P, P),
    sig!("AudioObjectSetPropertyData", I32, U32, P, U32, P, U32, P),
    sig!("AudioFormatGetProperty", I32, U32, U32, P, P, P),
    // The converter family's remaining half.
    sig!("AudioConverterNewSpecific", I32, P, P, U32, P, P),
    sig!("AudioConverterGetProperty", I32, P, U32, P, P),
    sig!("AudioConverterGetPropertyInfo", I32, P, U32, P, P),
    sig!("AudioConverterSetProperty", I32, P, U32, U32, P),
    sig!(
        "CMBlockBufferCreateContiguous",
        I32,
        P,
        P,
        P,
        P,
        U64,
        U64,
        U32,
        P
    ),
    sig!(
        "CMBlockBufferCreateWithMemoryBlock",
        I32,
        P,
        P,
        U64,
        P,
        P,
        U64,
        U64,
        U32,
        P
    ),
    sig!("CMBlockBufferGetDataPointer", I32, P, U64, P, P, P),
    sig!("CMCopyDictionaryOfAttachments", P, P, P, U32),
    sig!("CMFormatDescriptionGetMediaSubType", U32, P),
    // Twelve parameters, four past AAPCS64's eighth, and the fourth is guest
    // code.
    cbsig!(
        [3],
        "CMSampleBufferCreate",
        I32,
        P,
        P,
        BOOL,
        P,
        P,
        P,
        I64,
        I64,
        P,
        I64,
        P,
        P
    ),
    sig!("CMSampleBufferDataIsReady", BOOL, P),
    sig!("CMSampleBufferIsValid", BOOL, P),
    sig!("CMSampleBufferGetDataBuffer", P, P),
    sig!("CMSampleBufferGetFormatDescription", P, P),
    sig!("CMSampleBufferGetImageBuffer", P, P),
    sig!("CMSampleBufferGetNumSamples", I64, P),
    sig!("CMSampleBufferGetSampleAttachmentsArray", P, P, BOOL),
    sig!(
        "CMVideoFormatDescriptionCreate",
        I32,
        P,
        U32,
        I32,
        I32,
        P,
        P
    ),
    sig!(
        "CMVideoFormatDescriptionCreateFromH264ParameterSets",
        I32,
        P,
        U64,
        P,
        P,
        I32,
        P
    ),
    sig!(
        "CMVideoFormatDescriptionCreateFromHEVCParameterSets",
        I32,
        P,
        U64,
        P,
        P,
        I32,
        P,
        P
    ),
    // `CMVideoDimensions` is two `int32_t` - **eight** bytes - which is the
    // aggregate both ABIs agree about completely.
    sig!("CMVideoFormatDescriptionGetDimensions", U64, P),
    sig!(
        "CMVideoFormatDescriptionGetH264ParameterSetAtIndex",
        I32,
        P,
        U64,
        P,
        P,
        P,
        P
    ),
    sig!(
        "CMVideoFormatDescriptionGetHEVCParameterSetAtIndex",
        I32,
        P,
        U64,
        P,
        P,
        P,
        P
    ),
    sig!("CVMetalTextureCacheCreate", I32, P, P, P, P, P),
    sig!(
        "CVMetalTextureCacheCreateTextureFromImage",
        I32,
        P,
        P,
        P,
        P,
        U64,
        U64,
        U64,
        U64,
        P
    ),
    sig!("CVMetalTextureGetTexture", P, P),
    sig!("CVPixelBufferCreate", I32, P, U64, U64, U32, P, P),
    sig!("CVPixelBufferGetBaseAddressOfPlane", P, P, U64),
    sig!("CVPixelBufferGetBytesPerRowOfPlane", U64, P, U64),
    sig!("CVPixelBufferGetHeightOfPlane", U64, P, U64),
    sig!("CVPixelBufferGetWidthOfPlane", U64, P, U64),
    sig!("CVPixelBufferGetPixelFormatType", U32, P),
    sig!("CVPixelBufferPoolCreatePixelBuffer", I32, P, P, P),
    sig!("CVPixelBufferRelease", V, P),
    // VideoToolbox.
    cbsig!(
        [7],
        "VTCompressionSessionCreate",
        I32,
        P,
        I32,
        I32,
        U32,
        P,
        P,
        P,
        P,
        P,
        P
    ),
    sig!("VTCompressionSessionGetPixelBufferPool", P, P),
    sig!("VTCompressionSessionInvalidate", V, P),
    sig!("VTCompressionSessionPrepareToEncodeFrames", I32, P),
    sig!("VTCopyVideoEncoderList", I32, P, P),
    sig!(
        "VTDecompressionSessionCanAcceptFormatDescription",
        BOOL,
        P,
        P
    ),
    sig!("VTDecompressionSessionDecodeFrame", I32, P, P, U32, P, P),
    sig!("VTDecompressionSessionFinishDelayedFrames", I32, P),
    sig!("VTDecompressionSessionInvalidate", V, P),
    sig!("VTDecompressionSessionWaitForAsynchronousFrames", I32, P),
    sig!("VTIsHardwareDecodeSupported", BOOL, U32),
    sig!("VTRegisterSupplementalVideoDecoderIfAvailable", V, U32),
    sig!("VTSessionCopyProperty", I32, P, P, P, P),
    sig!("VTSessionSetProperty", I32, P, P, P),
    cbsig!([1], "FSEventStreamCreate", P, P, P, P, P, U64, F64, U32),
    sig!("FSEventStreamSetDispatchQueue", V, P, P),
    sig!("FSEventStreamStart", BOOL, P),
    sig!("FSEventStreamStop", V, P),
    sig!("FSEventStreamFlushSync", V, P),
    sig!("FSEventStreamInvalidate", V, P),
    sig!("FSEventStreamRelease", V, P),
    //
    // `IORegistryEntryIDMatching` and `IOServiceGetMatchingService` are how a
    // program finds one device rather than iterating; `IOHIDDeviceCreate` wraps
    // an `io_service_t` a match produced. All handles.
    sig!("IOHIDDeviceCreate", P, P, U32),
    sig!("IOHIDDeviceGetReport", I32, P, U32, I64, P, P),
    sig!("IORegistryEntryGetRegistryEntryID", I32, U32, P),
    sig!("IORegistryEntryIDMatching", P, U64),
    sig!("IOServiceGetMatchingService", U32, U32, P),
    sig!("CGColorGetTypeID", U64),
    sig!("CTFontGetTypeID", U64),
    sig!("CTRunGetTypeID", U64),
    sig!("CTFontCreateUIFontForLanguage", P, U32, F64, P),
    sig!("CTFontCreateWithName", P, P, F64, P),
    sig!("CTFontCopyFontDescriptor", P, P),
    sig!("CTFontCopyGraphicsFont", P, P, P),
    sig!("CTFontCopyName", P, P, P),
    sig!("CTFontCopyFamilyName", P, P),
    sig!("CTFontCopyPostScriptName", P, P),
    sig!("CTFontGetSymbolicTraits", U32, P),
    sig!("CTFontGetUnitsPerEm", U32, P),
    sig!("CTFontGetSize", F64, P),
    sig!("CTFontGetAscent", F64, P),
    sig!("CTFontGetDescent", F64, P),
    sig!("CTFontGetLeading", F64, P),
    sig!("CTFontGetCapHeight", F64, P),
    sig!("CTFontGetXHeight", F64, P),
    sig!("CTFontGetSlantAngle", F64, P),
    sig!("CTFontGetUnderlinePosition", F64, P),
    sig!("CTFontGetUnderlineThickness", F64, P),
    sig!("CTFontGetAdvancesForGlyphs", F64, P, U32, P, P, I64),
    sig!("CTFontGetGlyphsForCharacters", BOOL, P, P, P, I64),
    sig!("CTFontCreatePathForGlyph", P, P, U16, P),
    sig!("CTFontDrawGlyphs", V, P, P, P, U64, P),
    sig!("CTFontDescriptorCopyAttribute", P, P, P),
    sig!("CTFontCopyAttribute", P, P, P),
    sig!("CTFontCopyTraits", P, P),
    sig!("CTFontDescriptorCreateWithAttributes", P, P),
    sig!("CTFontDescriptorCreateMatchingFontDescriptor", P, P, P),
    sig!("CTFontCreateWithFontDescriptor", P, P, F64, P),
    sig!("CTFontCopyTable", P, P, U32, U32),
    sig!("CTFontCopyDefaultCascadeListForLanguages", P, P, P),
    sig!("CTFontManagerCopyAvailableFontFamilyNames", P),
    sig!("CTFontManagerCreateFontDescriptorsFromURL", P, P),
    sig!("CTFontManagerCreateFontDescriptorFromData", P, P),
    sig!("CTFontDescriptorCreateCopyWithAttributes", P, P, P),
    sig!("CTFontDescriptorCreateWithNameAndSize", P, P, F64),
    sig!("CTFontDescriptorCreateMatchingFontDescriptors", P, P, P),
    sig!("CTFontCreateCopyWithAttributes", P, P, F64, P, P),
    sig!("CTFontCreateWithGraphicsFont", P, P, F64, P, P),
    sig!("CTGetCoreTextVersion", U32),
    sig!("NSColorSpaceFromDepth", P, I32),
    sig!("NSBestDepth", I32, P, I64, I64, BOOL, P),
    sig!("CGEventSourceKeyState", BOOL, I32, U16),
    sig!("UTTypeConformsTo", BOOL, P, P),
    // `CFDictionaryRef _CFCopySystemVersionDictionary(void)` - private CF, and
    // the same question [`crate::compat`] answers for the SDK gates.
    sig!("_CFCopySystemVersionDictionary", P),
    //
    // The `_f` suffix is libdispatch's pre-blocks spelling: a context pointer
    // and a bare function, rather than a literal carrying both.
    cbsig!([2], "dispatch_async_f", V, P, P, P),
    cbsig!([2], "dispatch_sync_f", V, P, P, P),
    cbsig!([3], "dispatch_after_f", V, U64, P, P, P),
    cbsig!([1], "dispatch_set_finalizer_f", V, P, P),
    sig!("dispatch_group_enter", V, P),
    sig!("dispatch_group_leave", V, P),
    sig!("dispatch_queue_get_label", P, P),
    // `dispatch_queue_create_with_target$V2` is the only spelling a modern
    // compiler emits, and the `$`-suffix fallback in [`lookup`] finds this
    // entry for it.
    sig!("dispatch_queue_create_with_target", P, P, P, P),
    // `void _os_log_impl(void *dso, os_log_t, os_log_type_t, const char
    // *format, uint8_t *buf, uint32_t size)` - **not** variadic, which is the
    // surprise.
    sig!("_os_log_impl", V, P, P, U8, P, P, U32),
    sig!("os_log_create", P, P, P),
    sig!("os_log_type_enabled", BOOL, P, U8),
    sig!("os_release", V, P),
    //
    // The exception-port family is **not** here - it is in [`crate::signal`],
    // for `task_set_exception_ports`' reason.
    sig!("mach_error_string", P, I32),
    sig!("mach_port_insert_member", I32, U32, U32, U32),
    sig!(
        "mach_port_request_notification",
        I32,
        U32,
        U32,
        I32,
        U32,
        U32,
        U32,
        P
    ),
    sig!("mach_vm_read", I32, U32, U64, U64, P, P),
    sig!("mach_vm_region_recurse", I32, U32, P, P, P, P, P),
    sig!("mig_dealloc_reply_port", V, U32),
    sig!("mig_get_reply_port", U32),
    sig!("mig_put_reply_port", V, U32),
    sig!("host_processor_info", I32, U32, I32, P, P, P),
    sig!("semaphore_signal_all", I32, U32),
    sig!("task_suspend", I32, U32),
    sig!("task_resume", I32, U32),
    sig!("pid_for_task", I32, U32, P),
    sig!("bootstrap_check_in", I32, U32, P, P),
    sig!("bootstrap_strerror", P, I32),
    //
    // `darwin::mach` answers all of these off Darwin; on it they are the
    // kernel's own. Either way the arguments are `mach_port_t`, `integer_t` and
    // pointers to arrays of those, so none of them needs translating - see the
    // note on the MIG routines above.
    sig!("mach_task_self", U32),
    sig!("mach_continuous_time", U64),
    sig!("mach_error", V, P, I32),
    // `mach_msg` with a separate reply buffer: the same seven arguments, then
    // the buffer and its limit.
    sig!(
        "mach_msg_overwrite",
        I32,
        P,
        I32,
        U32,
        U32,
        U32,
        U32,
        U32,
        P,
        U32
    ),
    sig!("mach_port_destroy", I32, U32, U32),
    sig!("mach_port_get_refs", I32, U32, U32, U32, P),
    sig!("mach_port_move_member", I32, U32, U32, U32),
    sig!("mach_port_set_attributes", I32, U32, U32, I32, P, U32),
    sig!("task_get_special_port", I32, U32, I32, P),
    sig!("task_set_special_port", I32, U32, I32, U32),
    sig!("semaphore_signal_thread", I32, U32, U32),
    sig!("bootstrap_register", I32, U32, P, U32),
    sig!("bootstrap_look_up2", I32, U32, P, P, I32, U64),
    sig!("bootstrap_parent", I32, U32, P),
    //
    // `signal`, `sigaction` and `sigaltstack` are shimmed above - see
    // [`crate::signal`] - and the rest are `darwin::signal::entry`'s. The
    // `sigset_t` these five take is Darwin's **one word**, not glibc's
    // hundred-and-twenty-eight-byte structure, which is why they cannot be
    // forwarded.
    sig!("killpg", I32, I32, I32),
    sig!("sigsuspend", I32, P),
    sig!("sigemptyset", I32, P),
    sig!("sigfillset", I32, P),
    sig!("sigaddset", I32, P, I32),
    sig!("sigdelset", I32, P, I32),
    sig!("sigismember", I32, P, I32),
    sig!("sys_icache_invalidate", V, P, U64),
    // compiler-rt's generic atomics: `void (int size, void *, void *, int model)`.
    sig!("__atomic_load", V, I32, P, P, I32),
    sig!("__atomic_store", V, I32, P, P, I32),
    // `int proc_get_wakemon_params(pid_t, int32_t *rate_hz, int32_t *flags)` -
    // libproc SPI with no header on this system, read out of the kernel's own
    // `proc_info` interface.
    sig!("proc_get_wakemon_params", I32, I32, P, P),
    //
    // Imported by the interposing mimalloc rather than by the guest itself, which is
    // what makes most of it passthrough: a zone from `malloc_default_zone` is
    // libsystem_malloc's own structure, so calling through it is the host allocating
    // for itself. `malloc_zone_register` and `malloc_get_all_zones` are refused -
    // they hand the host a structure of *guest* function pointers.
    sig!("malloc_default_zone", P),
    sig!("malloc_default_purgeable_zone", P),
    sig!("malloc_create_zone", P, U64, U32),
    sig!("malloc_destroy_zone", V, P),
    sig!("malloc_zone_from_ptr", P, P),
    sig!("malloc_get_zone_name", P, P),
    sig!("malloc_set_zone_name", V, P, P),
    sig!("malloc_zone_malloc", P, P, U64),
    sig!("malloc_zone_calloc", P, P, U64, U64),
    sig!("malloc_zone_realloc", P, P, P, U64),
    sig!("malloc_zone_valloc", P, P, U64),
    sig!("malloc_zone_memalign", P, P, U64, U64),
    sig!("malloc_zone_free", V, P, P),
    sig!("malloc_zone_batch_malloc", U32, P, U64, P, U32),
    sig!("malloc_zone_batch_free", V, P, P, U32),
    sig!("malloc_zone_check", I32, P),
    sig!("malloc_zone_log", V, P, P),
    sig!("malloc_zone_print", V, P, I32),
    sig!("malloc_zone_print_ptr_info", V, P),
    sig!("malloc_zone_unregister", V, P),
    sig!("malloc_good_size", U64, U64),
    // `void *valloc(size_t)` and its private counterpart `vfree`, which
    // libsystem_malloc exports without a header.
    sig!("valloc", P, U64),
    sig!("vfree", V, P),
    // `int malloc_jumpstart(int cookie)` - undocumented libsystem_malloc SPI
    // taking and returning an `int`, called once during allocator start-up.
    sig!("malloc_jumpstart", I32, I32),
    // The three halves of the allocator's `pthread_atfork` handlers, which
    // mimalloc calls directly because it installs its own. All `void (void)`.
    sig!("_malloc_fork_prepare", V),
    sig!("_malloc_fork_parent", V),
    sig!("_malloc_fork_child", V),
    sig!("_Exit", V, I32),
    sig!("__exp10", F64, F64),
    // `int __srget(FILE *)` and `int __swbuf(int, FILE *)` - the out-of-line
    // halves of the `getc`/`putc` macros, which a guest inlines and which
    // therefore *read the `FILE` structure's fields directly*.
    sig!("__srget", I32, P),
    sig!("__swbuf", I32, I32, P),
    sig!("fmemopen", P, P, U64, P),
    sig!("open_memstream", P, P, P),
    sig!("open_wmemstream", P, P, P),
    sig!("getc_unlocked", I32, P),
    sig!("getchar", I32),
    sig!("getchar_unlocked", I32),
    sig!("putc_unlocked", I32, I32, P),
    sig!("putchar_unlocked", I32, I32),
    sig!("gets", P, P),
    sig!("getw", I32, P),
    sig!("putw", I32, I32, P),
    sig!("fpurge", I32, P),
    sig!("__fpurge", I32, P),
    sig!("feof_unlocked", I32, P),
    sig!("ferror_unlocked", I32, P),
    sig!("clearerr_unlocked", V, P),
    sig!("fileno_unlocked", I32, P),
    sig!("setbuffer", V, P, P, I32),
    sig!("setlinebuf", I32, P),
    sig!("ftrylockfile", I32, P),
    sig!("fgetwc", I32, P),
    sig!("getwchar", I32),
    sig!("putwc", I32, I32, P),
    sig!("fgetws", P, P, I32, P),
    sig!("fputws", I32, P, P),
    sig!("fgetwln", P, P, P),
    sig!("fwide", I32, P, I32),
    // `double __floattidf(__int128)`.
    sig!("__floattidf", F64, U64, U64),
    // `char *realpath(const char *restrict path, char *restrict resolved)`.
    sig!("realpath", P, P, P),
    sig!("cfmakeraw", V, P),
    sig!("chown", I32, P, U32, U32),
    sig!("fchown", I32, I32, U32, U32),
    sig!("fchownat", I32, I32, P, U32, U32, I32),
    sig!("lchown", I32, P, U32, U32),
    sig!("fchmodat", I32, I32, P, U16, I32),
    sig!("link", I32, P, P),
    sig!("dirfd", I32, P),
    sig!("fsctl", I32, P, U64, P, U32),
    sig!("gethostuuid", I32, P, P),
    sig!("uuid_generate", V, P),
    sig!("setxattr", I32, P, P, P, U64, U32, I32),
    sig!("removexattr", I32, P, P, I32),
    sig!("sendfile", I32, I32, I32, I64, P, P, I32),
    sig!("connectx", I32, I32, P, U32, U32, P, U32, P, P),
    sig!("posix_spawnattr_setsigdefault", I32, P, P),
    sig!("pthread_mutexattr_setpolicy_np", I32, P, I32),
    sig!("pthread_rwlock_tryrdlock", I32, P),
    sig!("pthread_rwlock_trywrlock", I32, P),
    sig!("rand_r", I32, P),
    sig!("srandomdev", V),
    sig!("tzset", V),
    sig!("frexpf", F32, F32, P),
    sig!("res_9_init", I32),
    sig!("res_9_close", V),
];

/// The commonest refusal by far, and the least interesting: a symbol that is a
/// *variable*.
const DATA: &str = "a data import, read through its slot and never called";

/// A libc++ entry point whose whole purpose is to `throw`.
///
/// Describing the signature would be easy and calling it would be wrong.
const THROWS: &str = "throws a C++ exception, which cannot be raised across the JIT boundary";

/// A struct by value, in a *C* function.
///
/// [`crate::abi::aggregate`] marshals these, and an Objective-C method reaches it -
/// because a method carries its own type encoding, which describes the struct's members
/// and their offsets.
const AGGREGATE: &str =
    "passes or returns a struct by value, which this table has no way to describe";

/// Takes a `std::string` and is not a string operation.
///
/// libc++ lays `basic_string` out differently on the two architectures - see
/// [`crate::shims::cxxstring`], which is where that is written down and where the
/// string's own entry points are implemented against the *guest's* layout.
const GUEST_STRING: &str =
    "takes a std::string, whose layout differs between x86-64 and arm64 macOS";

/// The [`crate::jmpbuf`] refusal, one level further out.
///
/// `getcontext` and friends save the *machine's* registers and stack pointer.
const CONTEXT: &str =
    "saves or restores a machine context, which here is the host's and not the guest's";

/// The one scalar the two architectures spell differently.
///
/// x86-64 macOS's `long double` is x87's 80-bit extended format; arm64's is IEEE
/// binary128.
const LONG_DOUBLE: &str =
    "takes or returns a long double, which is x87 80-bit on x86-64 and binary128 on arm64";

/// Symbols that are described and deliberately not callable, in no particular
/// order for the same reason as [`SIGNATURES`].
pub static REFUSED: &[(&str, &str)] = &[
    // The CoreFoundation constructors whose *argument* is a table of callbacks
    // rather than a callback. Refused only against the host's CoreFoundation:
    // with this tree's own, `shims::corefoundation` copies the structure out and
    // substitutes the pointers inside it, and a shim is consulted first.
    (
        "CFRunLoopSourceCreate",
        "its context structure holds guest function pointers the host would call as arm64",
    ),
    (
        "CFRunLoopTimerCreate",
        "its context structure holds guest function pointers the host would call as arm64",
    ),
    // `CFUUIDBytes CFUUIDGetUUIDBytes(CFUUIDRef)` returns sixteen bytes by
    // value, in two integer registers, and the marshaller has no type wider
    // than a doubleword to describe that with. Implemented all the same - the
    // native backend calls it directly, with no marshaller in the way.
    ("CFUUIDGetUUIDBytes", AGGREGATE),
    // Seventeen parameters: an allocator and the sixteen bytes of a UUID.
    (
        "CFUUIDGetConstantUUIDWithBytes",
        "seventeen parameters, more than the marshaller can place",
    ),
    // `CFGregorianDate` is four narrow integers and a `double`, which travel in
    // an integer register and a vector one. Implemented all the same - the
    // native backend calls both directly, with no marshaller in the way.
    ("CFAbsoluteTimeGetGregorianDate", AGGREGATE),
    ("CFGregorianDateGetAbsoluteTime", AGGREGATE),
    ("CGRectContainsPoint", AGGREGATE),
    ("CGRectInset", AGGREGATE),
    ("NSMouseInRect", AGGREGATE),
    ("NSRectFill", AGGREGATE),
    ("NSDrawNinePartImage", AGGREGATE),
    ("CTFontGetBoundingBox", AGGREGATE),
    ("CTFontGetBoundingRectsForGlyphs", AGGREGATE),
    // `NSApp` is data like the rest of this block, and the least obvious of
    // them.
    ("NSApp", DATA),
    ("NSCalibratedRGBColorSpace", DATA),
    ("NSDefaultRunLoopMode", DATA),
    ("NSDeviceResolution", DATA),
    ("NSFontAttributeName", DATA),
    ("NSForegroundColorAttributeName", DATA),
    ("NSLocaleCountryCode", DATA),
    ("NSLocaleLanguageCode", DATA),
    ("NSLocalizedDescriptionKey", DATA),
    ("NSLocalizedFailureReasonErrorKey", DATA),
    ("NSOSStatusErrorDomain", DATA),
    ("NSFilePosixPermissions", DATA),
    ("NSPOSIXErrorDomain", DATA),
    ("NSCocoaErrorDomain", DATA),
    ("NSFilePathErrorKey", DATA),
    ("NSURLErrorKey", DATA),
    ("NSParagraphStyleAttributeName", DATA),
    ("NSPasteboardTypeString", DATA),
    ("NSStringPboardType", DATA),
    ("NSURLAuthenticationMethodDefault", DATA),
    ("NSURLProtectionSpaceHTTPProxy", DATA),
    ("NSUnderlyingErrorKey", DATA),
    ("NSWindowDidMoveNotification", DATA),
    ("NSWindowDidResizeNotification", DATA),
    ("NSWindowDidBecomeMainNotification", DATA),
    ("NSWindowDidResignMainNotification", DATA),
    ("NSWindowWillCloseNotification", DATA),
    ("_DefaultRuneLocale", DATA),
    ("_NSConcreteGlobalBlock", DATA),
    ("_NSConcreteMallocBlock", DATA),
    ("_NSConcreteStackBlock", DATA),
    (
        "_ZNKSt3__120__vector_base_commonILb1EE20__throw_length_errorEv",
        THROWS,
    ),
    (
        "_ZNKSt3__121__basic_string_commonILb1EE20__throw_length_errorEv",
        THROWS,
    ),
    (
        "_ZNSt13runtime_errorC1ERKNSt3__112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEE",
        GUEST_STRING,
    ),
    (
        "_ZNSt3__113random_deviceC1ERKNS_12basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEE",
        GUEST_STRING,
    ),
    ("_ZNSt3__120__throw_system_errorEiPKc", THROWS),
    ("_ZTISt12length_error", DATA),
    ("_ZTISt13runtime_error", DATA),
    ("_ZTISt9bad_alloc", DATA),
    // A `type_info` and the vtable a guest's own `type_info` objects point at.
    ("_ZTISt9exception", DATA),
    ("_ZTIi", DATA),
    ("_ZTVN10__cxxabiv117__class_type_infoE", DATA),
    ("_ZTVN10__cxxabiv120__si_class_type_infoE", DATA),
    ("_ZTVN10__cxxabiv121__vmi_class_type_infoE", DATA),
    ("_ZTVSt12length_error", DATA),
    ("_ZTVSt9exception", DATA),
    // A *variable* holding a function pointer, not a function.
    ("__explicit_bzero_hook", DATA),
    // The personality routine a frame's LSDA names. Called by the *unwinder*,
    // as guest code, rather than through this table - which is what these
    // refusals mean: a personality routine reached through an import slot is a
    // call nothing makes. `crate::cxxlib` binds the C++ one to the vendored
    // `libc++abi`, and `crate::objcexc` points the Objective-C spelling at the
    // same routine, because on 64-bit macOS it is the same routine.
    (
        "__gxx_personality_v0",
        "a personality routine, named by an LSDA and called by the unwinder",
    ),
    ("__mb_cur_max", DATA),
    (
        "__objc_personality_v0",
        "as __gxx_personality_v0, which it is",
    ),
    ("__stack_chk_guard", DATA),
    ("__stderrp", DATA),
    ("__stdinp", DATA),
    ("__stdoutp", DATA),
    ("sys_signame", DATA),
    // `dispatch_get_main_queue()` is the *address* of this global rather than
    // a call, so the main queue a guest hands [`crate::shims::block`] is
    // libdispatch's own.
    ("_dispatch_main_q", DATA),
    (
        "dyld_stub_binder",
        "the lazy-binding entry point; every slot is bound eagerly",
    ),
    ("environ", DATA),
    ("getcontext", CONTEXT),
    // Fifteen parameters, seven of them past the eighth: more overflowing
    // 4-byte stack args than the marshaller's narrow-tail patterns cover.
    (
        "glCopyImageSubData",
        "fifteen parameters, more than the marshaller can place",
    ),
    ("inet_ntoa", AGGREGATE),
    ("kCFAllocatorDefault", DATA),
    ("kCFBooleanTrue", DATA),
    ("kCFBundleIdentifierKey", DATA),
    ("kCFCoreFoundationVersionNumber", DATA),
    ("kCFErrorDomainCFNetwork", DATA),
    ("kCFHTTPAuthenticationAccountDomain", DATA),
    ("kCFHTTPAuthenticationPassword", DATA),
    ("kCFHTTPAuthenticationSchemeBasic", DATA),
    ("kCFHTTPAuthenticationSchemeNTLM", DATA),
    ("kCFHTTPAuthenticationUsername", DATA),
    ("kCFHTTPVersion1_0", DATA),
    ("kCFHTTPVersion1_1", DATA),
    ("kCFNull", DATA),
    ("kCFProxyAutoConfigurationURLKey", DATA),
    ("kCFProxyHostNameKey", DATA),
    ("kCFProxyPortNumberKey", DATA),
    ("kCFProxyTypeHTTP", DATA),
    ("kCFProxyTypeKey", DATA),
    ("kCFProxyTypeSOCKS", DATA),
    ("kCFRunLoopDefaultMode", DATA),
    ("kCFStreamErrorDomainHTTP", DATA),
    ("kCFStreamPropertyHTTPAttemptPersistentConnection", DATA),
    ("kCFStreamPropertyHTTPProxy", DATA),
    ("kCFStreamPropertyHTTPProxyHost", DATA),
    ("kCFStreamPropertyHTTPProxyPort", DATA),
    ("kCFStreamPropertyHTTPRequestBytesWrittenCount", DATA),
    ("kCFStreamPropertyHTTPResponseHeader", DATA),
    ("kCFStreamPropertyHTTPSProxyHost", DATA),
    ("kCFStreamPropertyHTTPSProxyPort", DATA),
    ("kCFStreamPropertySOCKSProxy", DATA),
    ("kCFStreamPropertySOCKSProxyHost", DATA),
    ("kCFStreamPropertySOCKSProxyPort", DATA),
    ("kCFStreamPropertySSLSettings", DATA),
    ("kCFStreamSSLCertificates", DATA),
    ("kCFStreamSSLPeerName", DATA),
    ("kCFStreamSSLValidatesCertificateChain", DATA),
    ("kCFTypeArrayCallBacks", DATA),
    ("kUTTagClassFilenameExtension", DATA),
    ("kUTTagClassMIMEType", DATA),
    ("ldiv", AGGREGATE),
    ("makecontext", CONTEXT),
    ("optarg", DATA),
    ("opterr", DATA),
    ("optind", DATA),
    ("optopt", DATA),
    ("optreset", DATA),
    (
        "pthread_cancel",
        "cancellation unwinds a stack the host's unwinder never saw (M5)",
    ),
    ("setcontext", CONTEXT),
    //
    // The only family here where the two architectures disagree about an
    // ordinary scalar. On x86-64 macOS a `long double` is the x87 80-bit
    // extended format, passed on the stack and returned in `st(0)`; on arm64 it
    // is IEEE binary128, passed and returned in a `v` register. Neither is a
    // widening of the other, so these are refused rather than approximated:
    // narrowing through `double` would lose the eleven bits that are the reason
    // a program asked for the type.
    ("powl", LONG_DOUBLE),
    ("fmal", LONG_DOUBLE),
    ("ldexpl", LONG_DOUBLE),
    ("frexpl", LONG_DOUBLE),
    ("strtold", LONG_DOUBLE),
    // `pid_t audit_token_to_pid(audit_token_t)` - thirty-two bytes of
    // `audit_token_t` **by value**. SysV AMD64 classifies it MEMORY and puts it
    // on the caller's stack; AAPCS64 has the caller copy it and pass a pointer.
    // Both are "in memory" and they are still not the same thing.
    ("audit_token_to_pid", AGGREGATE),
    // `void IODispatchCalloutFromMessage(void *unused, mach_msg_header_t *msg,
    // void *reference)` - the IOKit notification pump.
    (
        "IODispatchCalloutFromMessage",
        "calls a guest function pointer carried inside a Mach message the host would read",
    ),
    // `void malloc_zone_register(malloc_zone_t *)` - hands libsystem_malloc a
    // structure of **guest** function pointers and asks it to allocate through
    // them.
    (
        "malloc_zone_register",
        "registers a zone of guest function pointers the host would call as arm64",
    ),
    // `kern_return_t malloc_get_all_zones(task_t, memory_reader_t *,
    // vm_address_t **, unsigned *)` - the same shape once removed.
    (
        "malloc_get_all_zones",
        "walks another task's zones through a guest reader callback",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every signature in the table can actually be called.
    #[test]
    fn every_signature_is_one_the_marshaller_can_place() {
        let refused: Vec<&str> = SIGNATURES
            .iter()
            .filter(|abi| !abi.variadic && !crate::abi::marshal::is_marshallable(abi))
            .map(|abi| abi.name)
            .collect();
        assert!(
            refused.is_empty(),
            "described but not callable: {refused:?}"
        );
    }

    /// A name appears once, which is the one thing [`index`] cannot notice.
    #[test]
    fn signatures_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for abi in SIGNATURES {
            assert!(seen.insert(abi.name), "{} is described twice", abi.name);
        }
    }

    /// Every entry declares the number of parameters the function really has.
    ///
    /// The bug this exists for is invisible by construction.
    #[test]
    fn the_table_declares_the_parameters_these_functions_really_have() {
        // A struct passed by value is *split* across registers by this table -
        // a `CFRange` is two `CFIndex`, a `double complex` is two `double` - so
        // these declare more arguments than the prototype has, deliberately.
        const SPLIT_AGGREGATES: &[&str] = &[
            "CFArrayApplyFunction",
            "CFDataDeleteBytes",
            "CFDataReplaceBytes",
            "CFStringGetBytes",
            "CFStringCompareWithOptions",
            "CFStringFindWithOptions",
            "CFStringCreateWithSubstring",
            "CFStringGetCharacters",
            "CFStringDelete",
            "CFStringReplace",
            "CFStringFindAndReplace",
            "cabs",
            // A `CGPoint` written out as its two `double`s.
            "CGDisplayMoveCursorToPoint",
            "CGWarpMouseCursorPosition",
        ];
        // Clang treats these names as its three-argument builtins. The runtime
        // table describes compiler-rt's four-argument generic functions.
        const COMPILER_RT_GENERIC: &[&str] = &["__atomic_load", "__atomic_store"];

        let mut source = String::from(HEADERS);
        source.push_str("void probe(void) {\n");
        let mut at = HEADERS.lines().count() + 1;
        let mut lines = std::collections::HashMap::new();
        for abi in SIGNATURES {
            // A `$`-suffixed spelling is not a C identifier, and a variadic's
            // last "parameter" is the ellipsis its shim consumes.
            if abi.name.contains('$')
                || abi.variadic
                || SPLIT_AGGREGATES.contains(&abi.name)
                || COMPILER_RT_GENERIC.contains(&abi.name)
            {
                continue;
            }
            let args = vec!["0"; abi.types.len() - 1].join(",");
            source.push_str(&format!("  {}({});\n", abi.name, args));
            at += 1;
            lines.insert(at, abi.name);
        }
        source.push_str("}\n");

        let dir = std::env::temp_dir().join("rosemary-arity");
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("skipping: no temp dir");
            return;
        }
        let path = dir.join("probe.c");
        if std::fs::write(&path, &source).is_err() {
            eprintln!("skipping: cannot write the probe");
            return;
        }
        let Ok(out) = std::process::Command::new("cc")
            .args(["-fsyntax-only", "-Wno-everything", "-fno-caret-diagnostics"])
            .arg(&path)
            .output()
        else {
            eprintln!("skipping: no C compiler");
            return;
        };

        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut wrong = Vec::new();
        for line in stderr.lines() {
            let Some(rest) = line.split("probe.c:").nth(1) else {
                continue;
            };
            if !rest.contains("too few arguments") && !rest.contains("too many arguments") {
                continue;
            }
            let Some(number) = rest.split(':').next().and_then(|n| n.parse::<usize>().ok()) else {
                continue;
            };
            if let Some(name) = lines.get(&number) {
                let how = match rest.contains("too few") {
                    true => "too few",
                    false => "too many",
                };
                wrong.push(format!("{name} ({how})"));
            }
        }
        assert!(
            wrong.is_empty(),
            "these entries declare the wrong number of parameters: {}",
            wrong.join(", ")
        );
    }

    /// What the arity probe includes, which is every header this table's C entries are
    /// declared in.
    const HEADERS: &str = "\
#include <CoreFoundation/CoreFoundation.h>
#include <AudioToolbox/AudioToolbox.h>
#include <CoreAudio/CoreAudio.h>
#include <CoreAudio/HostTime.h>
#include <CoreGraphics/CoreGraphics.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreServices/CoreServices.h>
#include <CoreText/CoreText.h>
#include <CoreVideo/CoreVideo.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/hid/IOHIDLib.h>
#include <VideoToolbox/VideoToolbox.h>
#include <bsm/libbsm.h>
#include <dirent.h>
#include <dispatch/dispatch.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <malloc/malloc.h>
#include <os/log.h>
#include <servers/bootstrap.h>
#include <sys/xattr.h>
#include <termios.h>
#include <uuid/uuid.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <fts.h>
#include <glob.h>
#include <iconv.h>
#include <langinfo.h>
#include <locale.h>
#include <math.h>
#include <netdb.h>
#include <poll.h>
#include <pthread.h>
#include <pwd.h>
#include <sched.h>
#include <semaphore.h>
#include <setjmp.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>
#include <wchar.h>
#include <wctype.h>
#include <zlib.h>
";

    #[test]
    fn refusals_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in REFUSED {
            assert!(seen.insert(*name), "{name} is refused twice");
        }
    }

    #[test]
    fn a_symbol_is_never_both_known_and_refused() {
        let refused: std::collections::HashSet<&str> =
            REFUSED.iter().map(|(name, _)| *name).collect();
        for abi in SIGNATURES {
            assert!(
                !refused.contains(abi.name),
                "{} is both described and refused",
                abi.name
            );
        }
    }

    #[test]
    fn the_leading_underscore_is_stripped_exactly_once() {
        assert!(matches!(lookup(0, "_strlen"), Lookup::Known(_)));
        assert!(matches!(lookup(0, "___stdoutp"), Lookup::Refused(_)));
        assert!(matches!(lookup(0, "_stdoutp"), Lookup::Unknown));
    }

    #[test]
    fn an_x86_only_variant_falls_back_to_the_plain_symbol() {
        // The same fallback machish applies when resolving the address; arm64
        // never had `$INODE64`, so what is known about the variant is whatever
        // is known about the plain symbol - here, that it takes a comparator.
        assert_eq!(lookup(0, "_fts_open"), lookup(0, "_fts_open$INODE64"));
        // `fts_open` is a signature now that the thunk exists, and the point of
        // this assertion is unchanged.
        assert!(
            matches!(lookup(0, "_fts_open$INODE64"), Lookup::Known(abi) if abi.name == "fts_open")
        );
        assert_eq!(lookup(0, "_strtod$UNIX2003"), lookup(0, "_strtod"));
    }

    #[test]
    fn objc_metadata_is_refused_as_data_rather_than_reported_unknown() {
        // One name per class an image mentions, so this cannot be a list. The
        // distinction that matters is refused-versus-unknown.
        for name in [
            "_OBJC_CLASS_$_NSObject",
            "_OBJC_METACLASS_$_NSObject",
            "_OBJC_IVAR_$_Dog._weight",
            "_OBJC_EHTYPE_id",
            "__objc_empty_cache",
            "___CFConstantStringClassReference",
        ] {
            assert!(matches!(lookup(0, name), Lookup::Refused(_)), "{name}");
        }
        // Not a blanket "anything starting with OBJC": the entry points are
        // functions with signatures, and swallowing one here would hide it.
        assert!(matches!(lookup(0, "_objc_msgSend"), Lookup::Unknown));
    }

    #[test]
    fn a_variadic_carries_its_fixed_arguments_and_the_marker() {
        let Lookup::Known(printf) = lookup(0, "_printf") else {
            panic!("no _printf")
        };
        assert!(printf.variadic);
        assert_eq!(printf.params(), &[T::Pointer, T::VariadicList]);
    }
}
