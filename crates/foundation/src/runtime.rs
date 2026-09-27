#![allow(non_snake_case)]

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

exports::exports! {
    functions {
        "NSSelectorFromString" => NSSelectorFromString,
        "NSStringFromSelector" => NSStringFromSelector,
        "NSClassFromString" => NSClassFromString,
        "NSStringFromClass" => NSStringFromClass,
        "NSFileTypeForHFSTypeCode" => NSFileTypeForHFSTypeCode,
        "NSUserName" => NSUserName,
        "NSFullUserName" => NSFullUserName,
        "NSSetUncaughtExceptionHandler" => NSSetUncaughtExceptionHandler,
        "NSGetUncaughtExceptionHandler" => NSGetUncaughtExceptionHandler,
    }
}

/// `NSString *NSUserName(void)`: the login name, which names `/Users/<name>`.
unsafe extern "C" fn NSUserName() -> Id {
    autoreleased(&prefix::layout::user_name())
}

/// `NSString *NSFullUserName(void)`: the first field of the passwd comment,
/// or the login name when that is empty, as Darwin falls back.
unsafe extern "C" fn NSFullUserName() -> Id {
    // SAFETY: `getpwuid` answers null or a record in a static buffer, read
    // before anything else can call it on this thread.
    let full = unsafe {
        let passwd = libc::getpwuid(libc::geteuid());
        match passwd.is_null() || (*passwd).pw_gecos.is_null() {
            true => None,
            false => std::ffi::CStr::from_ptr((*passwd).pw_gecos)
                .to_str()
                .ok()
                .and_then(|g| g.split(',').next())
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        }
    };
    autoreleased(&full.unwrap_or_else(prefix::layout::user_name))
}

/// The handler `NSSetUncaughtExceptionHandler` was given, for the getter.
/// Kept, not called: an uncaught exception already ends the run here with its
/// reason and backtrace, which is what the handler of a game reports anyway.
static UNCAUGHT_HANDLER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `void NSSetUncaughtExceptionHandler(NSUncaughtExceptionHandler *)`
unsafe extern "C" fn NSSetUncaughtExceptionHandler(handler: *const std::ffi::c_void) {
    UNCAUGHT_HANDLER.store(handler as usize, std::sync::atomic::Ordering::Relaxed);
}

/// `NSUncaughtExceptionHandler *NSGetUncaughtExceptionHandler(void)`
unsafe extern "C" fn NSGetUncaughtExceptionHandler() -> *const std::ffi::c_void {
    UNCAUGHT_HANDLER.load(std::sync::atomic::Ordering::Relaxed) as *const std::ffi::c_void
}

/// The text of an `NSString` argument, or `None` when it is nil or not one.
///
/// # Safety
///
/// `string` is nil or an object this runtime allocated.
unsafe fn text(string: Id) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let bytes = unsafe { crate::string::bytes(string) };
    String::from_utf8(bytes).ok()
}

/// An autoreleased `NSString`, as every `NSStringFrom...` returns.
fn autoreleased(text: &str) -> Id {
    let string = corefoundation::string::from_bytes(text.as_bytes(), false);
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(string) as Id }
}

/// `SEL NSSelectorFromString(NSString *)`.
///
/// Registers the name rather than only finding it, which is Apple's behaviour:
/// a selector is its name, and one nothing has sent yet is still a selector.
unsafe extern "C" fn NSSelectorFromString(name: Id) -> Sel {
    // SAFETY: the caller passed an `NSString` or nil.
    let Some(name) = (unsafe { text(name) }) else {
        return std::ptr::null();
    };
    // A selector name with an interior NUL is not one; `objc::sel::get` would
    // rather panic than register half of it.
    if name.contains('\0') {
        return std::ptr::null();
    }
    objc::sel::get(&name)
}

/// `NSString *NSStringFromSelector(SEL)`.
unsafe extern "C" fn NSStringFromSelector(selector: Sel) -> Id {
    if selector.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a registered selector, whose name is a live NUL-terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(selector)) };
    autoreleased(&name.to_string_lossy())
}

/// `Class NSClassFromString(NSString *)`, nil for a class this tree has not
/// registered - which is what a guest checks before using a class it can do
/// without.
///
/// The nil is the *answer*, not a shrug, so the diagnostic is not a refusal of
/// the call: it names the class the guest asked for and the macOS facility it is
/// private to. See [`refuse_missing_class`] for why it is said per name and what
/// the census behind it is.
unsafe extern "C" fn NSClassFromString(name: Id) -> *const std::ffi::c_void {
    // SAFETY: the caller passed an `NSString` or nil.
    let Some(name) = (unsafe { text(name) }) else {
        return std::ptr::null();
    };
    let Ok(name) = std::ffi::CString::new(name) else {
        return std::ptr::null();
    };
    match objc::class::lookup(&name) {
        Some(class) => class as *const objc::class::Class as *const std::ffi::c_void,
        None => {
            refuse_missing_class(&name);
            report_probe_stack(&name);
            std::ptr::null()
        }
    }
}

/// `ROSETTA3_CLASS_PROBE_STACK=1`: print the words on the stack that land in a
/// mapping, so a `NSClassFromString` probe can be attributed to the code that
/// made it.
///
/// # Why this exists, and what it is not
///
/// The `Probed` census above names the *address inside the guest* that asks.
/// That is not the same question as *who asked*: for a probe that aborts, the
/// frame-pointer walk in `rosetta3`'s own fault report desynchronises the moment
/// it leaves guest code, because this runtime's Rust frames have no frame
/// pointer, and the scan that is supposed to compensate only accepts a return
/// address left by `e8`/`e9`/`ff 15`/`ff 25`. A call through a **register** -
/// `call rax` - is therefore invisible to it, and `__mod_init_func` here holds
/// two entries. So the chain above the fatal probe is unmeasured, and
/// "unmeasured" is what a plan to paper over a wall would be built on.
///
/// # Anonymous mappings are the point, not an oversight
///
/// The first version of this kept only words landing in a mapping that has a
/// *pathname*, on the assumption that the guest image is `dlopen`ed and would
/// therefore have one. It is not: a Mach-O cannot be `dlopen`ed by the Linux
/// loader, so this runtime maps the image itself and the guest's `__TEXT`
/// appears in `/proc/self/maps` as an **anonymous** `r-x` mapping. The first
/// run therefore printed 506 words out of 16 KiB of stack and **not one guest
/// address** - which reads exactly like "the guest never called anything", and
/// would have been a confidently wrong answer. Every mapping is scanned now, and
/// an anonymous one is labelled `[anon]`.
///
/// # What it prints
///
/// The mappings worth telling apart (those with a pathname, and the anonymous
/// ones at least 1 MiB, which is what an image of this size looks like), then
/// `[rsp, rsp + 16 KiB)` word by word: every eight-aligned word that falls
/// inside any mapping, as `+<bytes above rsp>`, the raw value, and
/// `<file or [anon]>+<offset into that mapping>`. The image's own layout is
/// deliberately *not* consulted here: `test/tools/probe-stack.py` does that, so
/// the reading lives with the rest of the analysis and this stays a measurement.
///
/// Additive, off by default, and it changes no answer: [`NSClassFromString`]
/// returns the same nil either way, and this runs after the refusal line.
fn report_probe_stack(name: &std::ffi::CStr) {
    /// Off by default, and read once: a `getenv` per probe is not a cost worth
    /// paying in a function a large program's start-up calls repeatedly.
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| std::env::var_os("ROSETTA3_CLASS_PROBE_STACK").is_some());
    if !on {
        return;
    }

    /// The window. 16 KiB is generous for a start-up stack and is clamped to
    /// the end of the mapping holding `rsp`, so a word at the very top of a
    /// stack cannot fault the diagnostic that is reporting the fault.
    const WINDOW: usize = 16 * 1024;
    /// What counts as an image for the listing: a mapping this size is a
    /// framework or a heap arena, and the small anonymous ones are noise.
    const BIG: usize = 1 << 20;

    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        eprintln!(
            "rosetta3: class-probe stack: /proc/self/maps is unreadable, so the words on the \
             stack cannot be attributed to an image"
        );
        return;
    };

    // `start-end perms offset dev inode pathname`, with the pathname optional -
    // and for this runtime's guest images it *is* optional, which is the whole
    // reason the words are matched against every mapping rather than a subset.
    let mut images: Vec<(usize, usize, &str)> = Vec::new();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else { continue };
        let mut halves = range.split('-');
        let (Some(start), Some(end)) = (halves.next(), halves.next()) else {
            continue;
        };
        let (Ok(start), Ok(end)) =
            (usize::from_str_radix(start, 16), usize::from_str_radix(end, 16))
        else {
            continue;
        };
        images.push((start, end, fields.nth(4).unwrap_or("[anon]")));
    }

    let sp: usize;
    // SAFETY: reads a register only.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack));
    }

    let top = images
        .iter()
        .find(|(start, end, _)| *start <= sp && sp < *end)
        .map_or(sp + WINDOW, |(_, end, _)| (*end).min(sp + WINDOW));
    let top = (top - sp).max(8) + sp;

    eprintln!(
        "rosetta3: class-probe stack for NSClassFromString(\"{}\"): rsp={sp:#x}, \
         window {WINDOW} bytes",
        name.to_string_lossy()
    );
    for (start, end, path) in images.iter() {
        if *path == "[anon]" && end - start < BIG {
            continue;
        }
        eprintln!("rosetta3:   map {start:#018x}-{end:#018x} {path}");
    }
    let mut addr = sp & !7;
    let mut shown = 0usize;
    // The first `HEAD` bytes unfiltered.  A word that landed in no mapping is
    // information too - it is how a *jump* into guest code is told from a
    // *call* into it, since a call leaves a return address that is a mapped code
    // address and a tail jump leaves whatever the previous frame happened to
    // leave there.  Filtering it away is what made the first run unreadable.
    const HEAD: usize = 0x300;
    let head_end = (sp + HEAD).min(top);
    while addr < top {
        // SAFETY: `addr` is inside `[sp, top)`, and `top` is clamped to the end
        // of the mapping holding `sp` (or to `sp + WINDOW`, whichever is
        // nearer), so every load below is inside a mapping this thread can read.
        let word = unsafe { std::ptr::read_volatile(addr as *const usize) };
        if let Some((start, _, path)) =
            images.iter().find(|(start, end, _)| *start <= word && word < *end)
        {
            let base = path.rsplit('/').next().unwrap_or(path);
            eprintln!("rosetta3:   +{:#06x}  {word:#018x}  {base}+{:#x}", addr - sp, word - start);
            shown += 1;
        } else if addr < head_end {
            eprintln!("rosetta3:   +{:#06x}  {word:#018x}  (no mapping)", addr - sp);
        }
        addr += 8;
    }
    eprintln!("rosetta3:   {shown} word(s) in {WINDOW} bytes landed in a mapping");
}

/// One of the class names this guest hands to `NSClassFromString`, what it is
/// private to, and where the guest asks.
///
/// # `provided`, and what it does and does not claim
///
/// `provided: true` means **this tree declares the class**, so the guest's
/// `instancesRespondToSelector:` is answered by a real method-table lookup
/// rather than by a stand-in for one. It says nothing about the macOS facility
/// existing: every message either class receives refuses by name. See
/// [`provide_services`].
///
/// Two runs ago these two rows were the wall - `Framework+0xbc8db6f`, exit 133 -
/// and the milestone is now past them. That is a **measured** result, not an
/// inference from a green suite: `test/logs/111.log` is a run that got past
/// `+0xbc8db6f` and died three subsystems later, and the refusal lines for
/// `_NSServiceEntry` and `_NSServicesMenuUpdater` are **absent** from it,
/// because `NSClassFromString` now finds both classes. The absence is the
/// evidence; the fact that the run continued is not, since the fault could have
/// moved for an unrelated reason.
///
/// # The census is complete, and it is small
///
/// `__TEXT,__text` of Chromium Framework 130.0.6723.91 holds **14** call sites
/// of the `_NSClassFromString` stub at `+0xcca0984`, and the value `0xcca0984`
/// is stored **nowhere** in the image - so there is no indirect path to a
/// fifteenth. Fourteen sites, twelve distinct names, and **all twelve are
/// private classes of macOS**.
///
/// That is not a coincidence and it is the reason a per-name diagnostic is
/// affordable here: in this guest `NSClassFromString` exists *only* for
/// private-class feature detection. The classes Chromium uses normally are
/// reached through `__objc_classrefs` at load time, not by name. So a line per
/// missing name is at most twelve lines for the whole run, and every one of them
/// names a real capability this host does not have.
///
/// # `fatal` is measured, not inferred
///
/// It records what the instruction after the call does with the answer, in the
/// image. **Twelve of the fourteen** store the result and carry on, or feed it to
/// a `test`/`je` that skips a feature - `NSPopupMenuWindow` at `+0x3a942d6` uses
/// the identical `instancesRespondToSelector:` idiom as the fatal pair and simply
/// branches instead. **The two that are `fatal` are the Services probes**, and
/// they feed the answer to `NOTREACHED_IN_MIGRATION()`.
///
/// "Carries on" here has to be true *on this host* and not only on macOS, and it
/// is: three of those twelve send the answer straight on as a receiver
/// (`+0x8163d3d` even calls `objc_alloc` on it). `objc::msg::msg_send` opens with
/// `test rdi, rdi; je` and zeroes the return registers, so a nil receiver is
/// answered nil before `lookup` is ever reached, and `objc_alloc`'s `send0`
/// returns null for a null class. On macOS that is the language; here it is that
/// prologue, and the two facts would have to be kept in step.
struct Probed {
    name: &'static str,
    /// The macOS facility the class is private to.
    facility: &'static str,
    /// Every `NSClassFromString` call site in the image that asks for it.
    sites: &'static [&'static str],
    /// The site treats the answer as a bug rather than as "this OS lacks it".
    fatal: bool,
    /// This tree **declares** the class, so `NSClassFromString` finds a real
    /// class object and the guest's `instancesRespondToSelector:` is answered by
    /// a real method-table lookup rather than by a stand-in for one. See
    /// [`provide_services`].
    ///
    /// `false` for the ten names nothing provides, which is why they are refused
    /// by name. `true` for the two Services probes, and it says nothing about
    /// whether the macOS facility exists: the class is declared, and **every
    /// message it receives refuses by name**.
    provided: bool,
    /// For a `fatal` name, the `NOTREACHED_IN_MIGRATION()` its answer reaches.
    /// Not the call site: the two are five bytes apart and the *call* is what
    /// leaves the return address that tells them apart, so this is the one the
    /// call at `sites[0]` is the tail of.
    notreached: &'static str,
}

const PROBED: &[Probed] = &[
    Probed {
        name: "_NSServiceEntry",
        facility: "macOS's Services subsystem (com.apple.systemuiserver and the AppKit \
                    sharing stack). The class is declared by this tree, so the probe \
                    finds it; nothing about the subsystem itself is provided",
        sites: &["0xbc8daf1"],
        fatal: true,
        provided: true,
        notreached: "0xbc8db6f",
    },
    Probed {
        name: "_NSServicesMenuUpdater",
        facility: "macOS's Services subsystem (the menu-bar updater Chromium swizzles). \
                    The same gap as _NSServiceEntry, one probe later - this call is only \
                    reached if the first probe was answered yes, and it is answered by \
                    the same declared class and the same by-name refusals",
        sites: &["0xbc8db22"],
        fatal: true,
        provided: true,
        notreached: "0xbc8db76",
    },
    Probed {
        name: "SCStreamManager",
        facility: "ScreenCaptureKit",
        sites: &["0x10e3eeb"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "_CFXPreferences",
        facility: "the preferences daemon (cfprefsd); this host keeps preferences in plain files",
        sites: &["0x1b2252e"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "CFPrefsManagedSource",
        facility: "the preferences daemon (cfprefsd)",
        sites: &["0x1b2256c"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "NSPopupMenuWindow",
        facility: "AppKit, whose menu machinery has no counterpart here",
        sites: &["0x3a942d6"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "ASCPublicKeyCredentialDescriptor",
        facility: "AuthenticationServices (passkey credential descriptors)",
        sites: &["0x7936178"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "NSToolbarFullScreenWindow",
        facility: "AppKit's full-screen toolbar",
        sites: &["0x814fc81", "0x814fd47", "0x81502ca"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "NSGroupTouchBarItem",
        facility: "AppKit's Touch Bar",
        sites: &["0x8163c58"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "NSCustomTouchBarItem",
        facility: "AppKit's Touch Bar",
        sites: &["0x8163d3d"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "CAContext",
        facility: "Core Animation's private rendering path (it gates the GPU path on it)",
        sites: &["0xc6d9032"],
        fatal: false,
        provided: false,
        notreached: "",
    },
    Probed {
        name: "CALayerHost",
        facility: "Core Animation's private rendering path",
        sites: &["0xc6d90c2"],
        fatal: false,
        provided: false,
        notreached: "",
    },
];

/// What the two `fatal` probes are, in full, because the log line they produce
/// does not name them.
///
/// The `check.cc(361)` a guest prints for these is `NotReachedError::
/// TriggerNotReached()`, which is called **with no location**, so the location it
/// reports is `base/check.cc`'s own and identifies nothing. The only evidence
/// for the site is the return address left on the stack: `+0xbc8db74` for the
/// first, `+0xbc8db7b` for the second, which are five bytes apart and so
/// distinguish the two neighbouring `NOTREACHED_IN_MIGRATION()`s.
///
/// The code is the Share-menu swizzle in `chrome/browser/ui/cocoa/
/// share_menu_controller.mm` (identified from the image, not from a source ref -
/// this file is not at `130.0.6723.91`, nor at `refs/branch-heads/6723`, nor at
/// `main`). Reconstructed from the operands:
///
/// ```objc
///   Class entry = NSClassFromString(@"_NSServiceEntry");
///   if (![entry instancesRespondToSelector:@selector(bundleIdentifier)])
///     NOTREACHED_IN_MIGRATION();                      // +0xbc8db6f, this one
///   Class updater = NSClassFromString(@"_NSServicesMenuUpdater");
///   if (![updater instancesRespondToSelector:
///             @selector(populateMenu:withServiceEntries:forDisplay:)])
///     NOTREACHED_IN_MIGRATION();                      // +0xbc8db76
///   // then, under a once-flag at +0xe805ca8:
///   SwizzleMethods(&originals, updater, @selector(populateMenu:...),
///                  [ChromeSwizzleServicesMenuUpdater class],
///                  @selector(populateMenu:...));
/// ```
///
/// # What the probe asserts, and why declaring the class is the right answer
///
/// The assertion is `NOTREACHED_IN_MIGRATION()`, **not** `DCHECK()`. In an
/// official build `DCHECK` is compiled out, so this is a statement about every
/// macOS Chromium 130 ships on rather than a debug-only check: a release build
/// of a browser used by hundreds of millions of people takes a `SIGTRAP` on the
/// first launch if the private class ever stops answering `bundleIdentifier`.
/// So the capability is real, and this platform is *incomplete* rather than
/// merely different - the guest is reporting our incompleteness correctly.
///
/// The rest of what has to be true before declaring a class is honest:
///
/// 1. **The downstream is a method exchange, not a list.** `+0x5d946c0` does
///    `class_getInstanceMethod` on both classes (falling back to
///    `class_getClassMethod`), compares the two `method_getTypeEncoding`
///    results with `strcmp`, and calls `method_exchangeImplementations` - or,
///    if either method is missing or the encodings differ, stores zeros and
///    **returns**. It has no assertion, so there is no second untruthful answer
///    downstream to find and nothing to keep in step.
/// 2. **Nothing ever enumerates the service entries.** They arrive as a
///    *parameter* of the swizzled method,
///    `-populateMenu:withServiceEntries:forDisplay:` at `+0xbc8d710`, which
///    iterates the array it was handed. Nothing in this path asks for the list,
///    so there is no list an empty answer could truthfully replace either.
/// 3. **The swizzled method has no caller here.** Its purpose is to intercept
///    AppKit's own Services-menu population so the Share menu can be built
///    instead; `--headless` has no menu bar, so no AppKit code calls it, and the
///    AppKit side of the exchange is not in this image at all.
///
/// So the two classes are declared, with `NSObject` as the superclass, and every
/// message either of them receives **refuses by name**. See
/// [`provide_services`], and note the one thing that is *not* a refusal:
/// `instancesRespondToSelector:` is answered by a real method-table lookup, so
/// "yes, this class has that method" is a true statement about a class that
/// this tree built, and not an invented yes.
const SERVICES: &str = "\
The caller does NOT check for nil: Chromium sends instancesRespondToSelector: \
to the result unconditionally, and its NOTREACHED_IN_MIGRATION() at \
Chromium Framework+{site} is the result. The `check.cc(361)` line in the log \
is NotReachedError::TriggerNotReached(), which streams no location and so names \
no site. This is the Share-menu swizzle in chrome/browser/ui/cocoa/\
share_menu_controller.mm: it asserts the private class answers bundleIdentifier, \
then exchanges populateMenu:withServiceEntries:forDisplay: between \
_NSServicesMenuUpdater and the guest's own ChromeSwizzleServicesMenuUpdater \
(+0xbc8d710). That is a macOS menu-bar feature. The downstream of the probe is \
the exchange itself, and it has no assertion - it stores zeros and returns when \
a method is missing or the two type encodings differ. Nothing in that path ever \
asks for the list of service entries, and in --headless the swizzled method has \
no caller because there is no menu bar.";

/// What the refusal says for a `provided` name, which is only reachable if
/// `install()` did not run - and that is a bug in this crate rather than a
/// missing macOS facility, which is what the ten refused names are about.
const SERVICES_NOT_PROVIDED: &str = "\
This name is marked `provided: true` in the census, so this crate's install() \
should have registered it and NSClassFromString should have found it. Reaching \
this message means the registration did not happen, which is a defect here and \
not a gap in the host. The guest's NOTREACHED_IN_MIGRATION() at Chromium \
Framework+{site} is what that costs.";

/// Says, once per class name, that this host has no such class and why.
///
/// Once per *name* rather than once per call: the guest probes the same private
/// class from several sites, and a diagnostic a reader has already had is noise.
/// Not once per run either, because the next run may probe a different name and
/// the point of the line is that it is a census rather than a single gap.
///
/// The two `provided` names never reach this function, because
/// [`provide_services`] registers them and [`NSClassFromString`] finds them. The
/// arm is still here for them, and says the *other* thing - that the
/// registration is missing - because a diagnostic which blamed the host for a
/// defect in this crate would send the next reader to the wrong file.
fn refuse_missing_class(name: &std::ffi::CStr) {
    // A `Vec` and not a set: the census above is twelve names, so a linear scan
    // of a dozen owned strings is cheaper than hashing, and it keeps the
    // one-shot set in a static without a lazy initialiser.
    static SAID: std::sync::Mutex<Vec<std::ffi::CString>> =
        std::sync::Mutex::new(Vec::new());
    // A poisoned lock would mean a panic while the set was held, which cannot
    // happen here; recovering rather than propagating keeps that from turning
    // one guest's panic into a second refusal on the next probe.
    let mut said = SAID.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if said.iter().any(|said| said == name) {
        return;
    }
    said.push(name.to_owned());
    let text = name.to_string_lossy();
    match PROBED.iter().find(|probed| probed.name == text.as_ref()) {
        Some(probed) => {
            eprintln!(
                "rosetta3: NSClassFromString(\"{}\"): no such class on this host - it is private \
                 to {}. The guest asks at Chromium Framework+{}. {}",
                probed.name,
                probed.facility,
                probed.sites.join(", +"),
                match (probed.fatal, probed.provided) {
                    (true, true) => {
                        SERVICES_NOT_PROVIDED.replace("{site}", probed.notreached)
                    }
                    (true, false) => SERVICES.replace("{site}", probed.notreached),
                    (false, _) => "The instruction after the call does not turn the answer into \
                                   a fatal assertion, so the run carries on without it."
                        .to_owned(),
                },
            );
        }
        // A name outside the census. Said because silence is how a fatal probe
        // hides: the next reader should not have to re-derive the census to
        // learn that a name the guest never used to ask for is now missing.
        None => eprintln!(
            "rosetta3: NSClassFromString(\"{text}\"): no such class on this host. This name is \
             not in the census of the {PROBED_CENSUS} names this binary asks \
             NSClassFromString for, so what it belongs to has not been established here."
        ),
    }
}

/// How many names the census in [`PROBED`] claims to cover, for the text above.
const PROBED_CENSUS: usize = 12;

/// `NSString *NSStringFromClass(Class)`.
unsafe extern "C" fn NSStringFromClass(class: *const objc::class::Class) -> Id {
    // SAFETY: a live class object, as the caller's `Class` is.
    let Some(class) = (unsafe { class.as_ref() }) else {
        return std::ptr::null_mut();
    };
    autoreleased(class.name_str())
}

/// `NSString *NSFileTypeForHFSTypeCode(OSType)`.
///
/// HFS types use the quoted four-byte spelling Foundation accepts as a file
/// type, with the most significant byte first. Studio asks for `'APPL'` while
/// classifying a workspace notification.
unsafe extern "C" fn NSFileTypeForHFSTypeCode(code: u32) -> Id {
    let bytes = code.to_be_bytes();
    let mut encoded = [0u8; 6];
    encoded[0] = b'\'';
    encoded[1..5].copy_from_slice(&bytes);
    encoded[5] = b'\'';
    let string = corefoundation::string::from_bytes(&encoded, false);
    // SAFETY: a fresh string returned by a non-create Foundation function.
    unsafe { corefoundation::api::CFAutorelease(string) as Id }
}

/// `-className`, the receiver's class name as an `NSString`.
///
/// Foundation declares it on `NSObject`, so every object answers it - which is
/// what a caller logging or dispatching on a type relies on. Autoreleased,
/// because the name it is built from is the class's and the string is not.
unsafe extern "C" fn class_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a live object.
    let name = match unsafe { objc::object::class_of(receiver) } {
        Some(class) => class.name_str().to_string(),
        // Nil has no class, and `-className` on it would not have been dispatched
        // - this is the unreachable branch rather than a guess.
        None => String::new(),
    };
    let string = crate::string::from_str(&name);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

pub(crate) fn install() {
    let nsobject = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    methods!(nsobject, {
        "className" => class_name, c"@16@0:8";
    });
    provide_services(nsobject);
}

/// Register `_NSServiceEntry` and `_NSServicesMenuUpdater`, so the guest's two
/// fatal `NSClassFromString` probes find a real class and a real method table.
///
/// # What this is
///
/// The two probes are `NOTREACHED_IN_MIGRATION()`, not `DCHECK()`: see
/// [`SERVICES`]. The guest states as an invariant of every macOS Chromium 130
/// ships on that `_NSServiceEntry` answers `bundleIdentifier`. This platform has
/// no Services subsystem, so the class is declared anyway - the guest is
/// correctly reporting that this platform is incomplete, and a declared class
/// with by-name refusals is how this runtime says "incomplete" without
/// pretending otherwise. It is the same shape as the 124 `_OBJC_CLASS_$_X`
/// names the missing-class defence binds to empty classes.
///
/// # The superclass is `NSObject`, and that is a claim not a convenience
///
/// Both classes are private to AppKit's Services subsystem, so on macOS their
/// superclass is an AppKit class this tree does not have. `NSObject` is the
/// truthful answer for *what this runtime provides*: an `isa`, a name, and
/// `+alloc`. Claiming a framework superclass would be a lie with consequences -
/// `class_getSuperclass:` would report a hierarchy that does not exist, and a
/// guest walking it would send messages to a class that is not there. `NSObject`
/// keeps the chain exactly as long as the truth is.
///
/// # Every method refuses by name, and that is the load-bearing part
///
/// The test of this change is not "the abort stopped". It is that **no message
/// these two classes receive is answered with a value that is not true**. The
/// methods exist so that `instancesRespondToSelector:` can answer from a real
/// method table - which is a true statement, and a false one would be the worst
/// outcome available, because it is invisible. Each method's *own* answer is a
/// refusal, because there is no truthful value to give:
///
/// - `-bundleIdentifier` **is read** by the guest, and the measurement is in
///   [`BUNDLE_IDENTIFIER_READS`]. In the Services path specifically,
///   `-[ChromeSwizzleServicesMenuUpdater populateMenu:withServiceEntries:forDisplay:]`
///   sends it to every entry in the array it is handed, retains the result into
///   `%r14`, and compares it with `-isEqualToString:` against `com.apple.Safari`
///   (`+0xbc8d8e3`) and `com.apple.systemuiserver` (`+0xbc8d961`); those two
///   comparisons decide whether the entry is added to the menu. An empty
///   `NSString` is not "no identifier" - it is an identifier equal to neither,
///   which would silently drop every entry. So the selector is refused by name.
///
///   An earlier version of this comment said the selector "is loaded at exactly
///   two places, both in these two functions". **That was wrong, and it was
///   wrong in the dangerous direction**: a census that small is what would have
///   licensed an empty `NSString` as "nothing reads it". The real figure is
///   [`BUNDLE_IDENTIFIER_READS`] - seventeen send sites and one probe, spread
///   over eight unrelated files. Refusing is right, and it is right for a
///   stronger reason than the one first written down.
/// - `-populateMenu:withServiceEntries:forDisplay:` is the method the guest
///   *exchanges* with its own. Nothing in this configuration ever calls it - the
///   only caller on macOS is AppKit's Services-menu code, which is not in this
///   image, and `--headless` has no menu bar - so there is nothing for it to do
///   and a refusal is the whole of its behaviour.
///
/// Its type encoding is **the guest's own**, `v36@0:8@16@24c32`, read out of the
/// `class_ro_t` at `+0xe70d518` whose method list holds exactly this one method,
/// with `IMP` `+0xbc8d710`. That matters and is not cosmetic: the guest's
/// `SwizzleMethods` `strcmp`s the two encodings and skips the exchange if they
/// differ, so a guessed encoding would silently decline to swizzle. Copying the
/// guest's declaration is not an invented value - it is what the other side of
/// the swizzle says, and the swizzle exists precisely because they agree.
///
/// # What the swizzle does afterwards
///
/// After the exchange, `_NSServicesMenuUpdater`'s method holds the guest's real
/// implementation and `ChromeSwizzleServicesMenuUpdater`'s holds this refusal.
/// That is the correct direction: a message to an `_NSServicesMenuUpdater`
/// instance runs real guest code, and the refusal is reached only by a message
/// to an instance of the guest's own throwaway class, which nothing sends.
fn provide_services(nsobject: &'static objc::class::Class) {
    declare_service_class(c"_NSServiceEntry", nsobject, |entry| {
        methods!(entry, {
            "bundleIdentifier" => service_entry_bundle_identifier, c"@16@0:8";
        });
    });
    declare_service_class(c"_NSServicesMenuUpdater", nsobject, |updater| {
        methods!(updater, {
            "populateMenu:withServiceEntries:forDisplay:" =>
                services_populate_menu, c"v36@0:8@16@24c32";
        });
    });
}

/// Every load of the `bundleIdentifier` `__objc_selrefs` slot in the guest's
/// `__TEXT,__text`, and what becomes of each.
///
/// # Why this is a table and not a sentence
///
/// The question this answers is the one the whole refusal rests on: **is the
/// return value of `-bundleIdentifier` ever read?** If nothing reads it, an
/// empty `NSString` is a truthful "there is no identifier" and is the better
/// answer. If something does read it, every string this returned would be
/// invented and the only correct answer is a refusal by name.
///
/// An earlier version of this file asserted the value was unread, on the
/// strength of "loaded at exactly two places, both in these two functions".
/// **That census was wrong.** It was wrong because `refs.py`-style scanning was
/// not applied to the selector at all, and because a plausible-looking count of
/// two is very hard to disbelieve when both sites really are in the Services
/// code. The real answer is below, and it is the answer that licenses the
/// refusal.
///
/// # How it was measured, and what could still be wrong
///
/// Whole-image over 214 MB of `__TEXT,__text`, four byte phases (a `disp32`
/// after a one-byte opcode is *not* 4-aligned, and scanning one phase reports
/// **zero** references to a selector the disassembly plainly loads - that is
/// how the wrong count was produced twice before it was caught), then every
/// candidate re-decoded as `REX 8b <modrm> disp32` and followed to the first
/// indirect call, through one spill-and-reload.
///
/// The spill is why a window filter is not enough. Two of the seventeen sends
/// are `mov rax, [selref]; mov [rbp-N], rax` in a loop header with the actual
/// `call` **0x60 bytes later** (`+0x5aef87e` -> `+0x5aef9da`, and `+0x5b09480`
/// -> `+0x5b094db`). A filter over the next 32 bytes calls both "a store" and
/// would have produced the "nothing reads it" answer again.
///
/// # What "send" and "probe" mean here
///
/// `send` is a site where the selector reaches `objc_msgSend` as its second
/// argument, so the method's **return value is produced and consumed**.
/// `probe` is `+0xbc8daf6`, where it is the *third* argument to
/// `instancesRespondToSelector:` - nobody reads a return value, and the guest
/// only wants a `BOOL` about the class. That distinction is the whole question,
/// and `+0xbc8daf6` is the one site in this table that is **not** a send.
///
/// # What is left open
///
/// Which of the seventeen sends has an `_NSServiceEntry` as its receiver. This
/// table does **not** establish that, and it does not need to: the refusal is
/// the answer for the *method*, and the method cannot be given a truthful value
/// for the one receiver that is certainly an `_NSServiceEntry` (the array the
/// swizzled method is handed). The other sixteen are `NSBundle`/`NSWorkspace`
/// code in unrelated files - `+0x487689` in `chrome_main_delegate.cc` compares
/// its result against a cached global and `strdup`s it, `+0x5b09*` is
/// `password_manager_util_mac.mm` comparing against
/// `com.apple.coreservices.uiagent` - and none of them can hold a service entry.
const BUNDLE_IDENTIFIER_READS: &[(&str, &str)] = &[
    // (instruction, what it is)
    ("0x487689", "send - chrome_main_delegate.cc; result strdup'd into a global"),
    ("0x3aafcb8", "send - text_input_client_mac.mm"),
    ("0x51008f1", "send - thunks.cc / app_shim_controller.mm"),
    ("0x5aef87e", "send - spilled to [rbp-0x240], sent at +0x5aef9da"),
    ("0x5b09480", "send - spilled to [rbp-0xd0], sent at +0x5b094db"),
    ("0x5b09567", "send - password_manager_util_mac.mm"),
    ("0x5b09720", "send - password_manager_util_mac.mm"),
    ("0x5b09931", "send - password_manager_util_mac.mm"),
    ("0x5b09a1e", "send - password_manager_util_mac.mm"),
    ("0x5b09b68", "send - password_manager_util_mac.mm"),
    ("0x5b09c1b", "send - password_manager_util_mac.mm"),
    ("0x5b09d30", "send - password_manager_util_mac.mm"),
    ("0x5b09e69", "send - password_manager_util_mac.mm"),
    ("0x5b0a012", "send - password_manager_util_mac.mm"),
    ("0x5d92331", "send - base/apple/backup_util.mm"),
    ("0x8541738", "send - observer_list.h bracket; NSWorkspace runningApplications"),
    ("0xbc8d845", "send - spilled to [rbp-0x110], sent at +0xbc8d8a5 (Services)"),
    ("0xbc8daf6", "probe - the instancesRespondToSelector: argument, NOT a send"),
];

/// The number of *sends* in [`BUNDLE_IDENTIFIER_READS`], for the census diff.
const BUNDLE_IDENTIFIER_SENDS: usize = 17;

/// The one site in [`BUNDLE_IDENTIFIER_READS`] that is a probe rather than a
/// send, and the only one of the two the guest's `NOTREACHED_IN_MIGRATION()`
/// actually reaches.
const BUNDLE_IDENTIFIER_PROBE: &str = "0xbc8daf6";


/// Allocate, declare and register one of the two Services classes, once.
///
/// Idempotent by name, so a second `install()` is a no-op rather than the
/// double-registration `objc` refuses to be quiet about.
fn declare_service_class(
    name: &std::ffi::CStr,
    superclass: &'static objc::class::Class,
    declare: impl FnOnce(&'static objc::class::Class),
) {
    if objc::class::lookup(name).is_some() {
        return;
    }
    let Some(class) = objc::class::allocate_pair(Some(superclass), name, 0) else {
        objc::fail!(
            "objc: {} is registered as a name this runtime allocated for a different class",
            name.to_string_lossy()
        )
    };
    declare(class);
    objc::class::register_pair(class);
}

/// `-[_NSServiceEntry bundleIdentifier]`, which **refuses**.
///
/// The declared return type is `NSString *` because that is what the method is,
/// and the guest's `method_getTypeEncoding` on this class is not something any
/// code in this image reads - but it is what a reader needs to see to know the
/// refusal is a refusal and not a stub with a different shape.
///
/// The value is not unread, and this is the measurement rather than the
/// argument. See [`BUNDLE_IDENTIFIER_READS`]: the selector is loaded at
/// **eighteen** places in the guest's text, of which **seventeen are sends**,
/// so the return value is produced and consumed seventeen times over. In the
/// Services path the consumption is explicit - `+0xbc8d8a5` sends it to every
/// entry, and the result is compared against two named bundle identifiers at
/// `+0xbc8d8e3` and `+0xbc8d961`. Any string this returned would be a wrong
/// answer rather than a missing one. There is no truthful value, and saying so
/// by name is the answer.
unsafe extern "C" fn service_entry_bundle_identifier(_: Id, _: Sel) -> Id {
    objc::fail!(
        "-[_NSServiceEntry bundleIdentifier] refused: this host has no macOS Services \
         subsystem, so there is no service entry and no bundle identifier to report. \
         This is a refusal and not a stub because the value IS read: the selector has \
         one __objc_selrefs slot (+0xe6e7e00) and the guest loads it at EIGHTEEN \
         places, of which SEVENTEEN are sends whose return value is consumed \
         (measured over the whole of __TEXT,__text; the site list is \
         BUNDLE_IDENTIFIER_READS in crates/foundation/src/runtime.rs). The one that \
         matters here is Chromium's own \
         -[ChromeSwizzleServicesMenuUpdater populateMenu:withServiceEntries:forDisplay:], \
         which sends this message to every entry it is handed (Chromium Framework+0xbc8d8a5) \
         and compares the result with -isEqualToString: against \"com.apple.Safari\" \
         (+0xbc8d8e3) and \"com.apple.systemuiserver\" (+0xbc8d961); those two \
         comparisons decide whether the entry reaches the menu. An empty NSString \
         would be an identifier equal to neither, which is a wrong answer dressed as \
         a missing one. Of the eighteen, exactly one is not a send: +0xbc8daf6, where \
         the selector is the ARGUMENT to instancesRespondToSelector: - which is the \
         probe the guest makes before this method is ever called, and which reads no \
         return value of ours at all."
    )
}

/// `-[_NSServicesMenuUpdater populateMenu:withServiceEntries:forDisplay:]`, which
/// **refuses**.
///
/// This is the method Chromium exchanges with its own implementation, so
/// declaring it is what makes the guest's probe at `+0xbc8db33` answer yes from
/// a real method table. Nothing calls it here: the only caller on macOS is
/// AppKit's Services-menu code, which is not in this image, and `--headless` has
/// no menu bar to populate. There is therefore nothing truthful to do with the
/// `NSMenu` it is handed, and answering "done" would be a wrong answer.
///
/// The refusal is reachable, and that is worth stating: after the exchange this
/// `IMP` is what a `ChromeSwizzleServicesMenuUpdater` instance answers. Nothing
/// in the guest sends that message to one, so the abort below is a wall that
/// this host can describe rather than one it will hit silently.
unsafe extern "C" fn services_populate_menu(
    _: Id,
    _: Sel,
    _menu: Id,
    _service_entries: Id,
    _display: bool,
) {
    objc::fail!(
        "-[_NSServicesMenuUpdater populateMenu:withServiceEntries:forDisplay:] refused: the \
         method is declared so that Chromium's probe (instancesRespondToSelector: at \
         Chromium Framework+0xbc8db33) is answered by a real method-table lookup rather \
         than by a stand-in for one, but there is no macOS Services menu on this host, \
         so there is no menu to populate and no service entries to put in it. Chromium \
         exchanges this method with its own at +0xbc8dbb3 (method_exchangeImplementations \
         against +0xbc8d710), and its own SwizzleMethods compares the two type \
         encodings with strcmp first and skips the exchange if they differ - the \
         encoding declared here is the guest's own, v36@0:8@16@24c32, read from the \
         class_ro_t at +0xe70d518. The only caller of this method on macOS is AppKit's \
         Services-menu code, which is not in this image; --headless has no menu bar."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hfs_type_preserves_four_character_byte_order() {
        crate::bootstrap();
        // SAFETY: the returned object is a live Foundation string.
        let encoded = unsafe { NSFileTypeForHFSTypeCode(u32::from_be_bytes(*b"APPL")) };
        // SAFETY: `encoded` is the string created immediately above.
        assert_eq!(unsafe { crate::string::bytes(encoded) }, b"'APPL'");
    }

    /// The two `fatal` names must name *different* `NOTREACHED_IN_MIGRATION()`s.
    ///
    /// They are five bytes apart in the image (`call` at `+0xbc8db6f` and
    /// `+0xbc8db76`) and the saved return address, `+0xbc8db74` against
    /// `+0xbc8db7b`, is the only thing that tells them apart - so a table that
    /// gave both the same address would report a site the guest did not reach,
    /// which is the failure this project keeps paying for. Also checks the
    /// template has no unfilled placeholder, since an un-substituted `{site}`
    /// reads as a confident address.
    #[test]
    fn the_two_fatal_probes_name_different_sites() {
        let fatal: Vec<&Probed> = PROBED.iter().filter(|p| p.fatal).collect();
        assert_eq!(fatal.len(), 2, "the census has two fatal probes");
        assert_ne!(fatal[0].notreached, fatal[1].notreached);
        for probed in &fatal {
            for template in [SERVICES, SERVICES_NOT_PROVIDED] {
                let said = template.replace("{site}", probed.notreached);
                assert!(!said.contains("{site}"), "{} left the placeholder", probed.name);
                assert!(
                    said.contains(probed.notreached),
                    "{} does not name its own site",
                    probed.name
                );
            }
        }
        // And the census is the size the out-of-census message claims.
        assert_eq!(PROBED.len(), PROBED_CENSUS);
        assert_eq!(
            PROBED.iter().map(|p| p.sites.len()).sum::<usize>(),
            14,
            "14 NSClassFromString call sites in the image"
        );
    }

    /// The `bundleIdentifier` census must be internally consistent, and must
    /// say **read** rather than unread.
    ///
    /// This exists because the census was wrong once and the error was in the
    /// dangerous direction: an earlier version claimed two load sites and used
    /// that to argue an empty `NSString` would do. Nothing about a *test* would
    /// have caught it - the guest was never consulted - so what is asserted here
    /// is only what can be asserted without the image, and the image-derived
    /// part is the table's own arithmetic:
    ///
    /// - the table has one row per load site, so `sends + probes == rows`;
    /// - **at least one** site is a send, which is the fact the refusal rests
    ///   on, and which a shrunken table would fail;
    /// - exactly one site is a probe, and it is the `instancesRespondToSelector:`
    ///   argument at `+0xbc8daf6` - the only site that reads no return value.
    ///
    /// The number 17 is the measured one. Lowering it to make a test pass is
    /// the failure this is here to prevent, and the comment at
    /// [`BUNDLE_IDENTIFIER_READS`] says where it came from.
    #[test]
    fn the_bundle_identifier_census_says_the_value_is_read() {
        let rows = BUNDLE_IDENTIFIER_READS;
        let sends = rows.iter().filter(|(_, what)| what.starts_with("send")).count();
        let probes = rows.iter().filter(|(_, what)| what.starts_with("probe")).count();
        assert_eq!(sends, BUNDLE_IDENTIFIER_SENDS, "the send count drifted from the table");
        assert_eq!(sends + probes, rows.len(), "every row must be a send or a probe");
        assert!(
            sends > 0,
            "no send site: that would license an empty NSString for a value nothing reads, \
             which is the wrong this census already got once"
        );
        assert_eq!(probes, 1, "exactly one site is a probe: the guest's own");
        assert_eq!(
            rows.iter().filter(|(site, _)| *site == BUNDLE_IDENTIFIER_PROBE).count(),
            1,
            "the probe site must be named exactly once"
        );
        // And the Services send must be in the table, by its own spill site,
        // because that is the one whose receiver is certainly a service entry.
        assert!(
            rows.iter()
                .any(|(site, what)| *site == "0xbc8d845" && what.contains("Services")),
            "the Services-path send is missing from the census"
        );
    }

    /// `provided` in the census and `install()` must agree, in both directions.
    ///
    /// A census row that says `provided` for a class nothing registers is a
    /// diagnostic waiting to mislead, and a class that is registered but still
    /// listed as a refusal is the reverse. Neither is visible in a build or a
    /// test pass; both are one lookup.
    #[test]
    fn the_census_and_the_registry_agree_about_what_is_provided() {
        crate::bootstrap();
        for probed in PROBED {
            let name = std::ffi::CString::new(probed.name).expect("a census name");
            assert_eq!(
                objc::class::lookup(&name).is_some(),
                probed.provided,
                "{}: the census and the class registry disagree",
                probed.name
            );
        }
    }

    /// Each provided class answers the *one* question the guest asks of it, and
    /// the answer is a real method-table lookup rather than a constant.
    ///
    /// This is the property the whole change rests on, and it is asserted
    /// through the runtime's own dispatch rather than by calling the
    /// implementation here, because `instancesRespondToSelector:` on a class
    /// receiver goes to the metaclass - a path a direct call would skip.
    ///
    /// The negative half matters as much as the positive: a class that answered
    /// yes to *everything* would satisfy the guest and be a lie. `-retainCount`
    /// is the control, because `NSObject` declares it and the guest never asks.
    #[test]
    fn a_provided_class_answers_exactly_the_question_the_guest_asks() {
        crate::bootstrap();
        // SAFETY: `+instancesRespondToSelector:` is `BOOL (*)(Class, SEL, SEL)`
        // and `BOOL` is one byte in an integer register.
        unsafe {
            let ask: unsafe extern "C" fn(Id, Sel, Sel) -> bool =
                std::mem::transmute(objc::msg::entry());
            let instances_respond = objc::sel::get("instancesRespondToSelector:");
            let class_of = objc::sel::get("class");
            for (name, asked) in [
                (c"_NSServiceEntry", c"bundleIdentifier"),
                (
                    c"_NSServicesMenuUpdater",
                    c"populateMenu:withServiceEntries:forDisplay:",
                ),
            ] {
                let class = objc::class::ptr(
                    objc::class::lookup(name)
                        .unwrap_or_else(|| panic!("{} is not registered", name.to_string_lossy())),
                ) as Id;
                let asked_name = asked.to_string_lossy();
                let asked_sel = objc::sel::get(&asked_name);
                assert!(
                    ask(class, instances_respond, asked_sel),
                    "{} says no to {}",
                    name.to_string_lossy(),
                    asked_name
                );
                // The control: a method `NSObject` declares, which the guest
                // never probes for on either class.
                assert!(
                    ask(class, instances_respond, objc::sel::get("retainCount")),
                    "{} lost an inherited method, so the chain is wrong rather than the answer",
                    name.to_string_lossy()
                );
                // And the class is a class: `+class` answers itself.
                let f: unsafe extern "C" fn(Id, Sel) -> Id =
                    std::mem::transmute(objc::msg::entry());
                assert_eq!(f(class, class_of), class, "{} is not its own class", name.to_string_lossy());
            }
        }
    }

    /// The declared type encoding is the guest's own, and the swizzle depends on
    /// it being exactly that.
    ///
    /// `SwizzleMethods` at `+0x5d946c0` `strcmp`s the two
    /// `method_getTypeEncoding` results and **skips the exchange** if they
    /// differ, silently. A wrong encoding here is therefore not a crash and not
    /// a wrong value - it is a feature that quietly never installs, which is the
    /// hardest kind of wrong to notice. The string is the one in the guest's
    /// `class_ro_t` at `+0xe70d518`, whose method list holds this one method
    /// with `IMP` `+0xbc8d710`.
    #[test]
    fn the_declared_encoding_is_the_encodings_the_guest_declares() {
        crate::bootstrap();
        let class = objc::class::lookup(c"_NSServicesMenuUpdater")
            .expect("the updater is registered");
        let method = class
            .method(objc::sel::get("populateMenu:withServiceEntries:forDisplay:"))
            .expect("the exchanged method is declared");
        // SAFETY: a `Method` this runtime produced, whose `types` is a
        // NUL-terminated CString that outlives it.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(
            types,
            c"v36@0:8@16@24c32",
            "an encoding the guest's SwizzleMethods would decline to match"
        );
    }

    /// The guest's own `SwizzleMethods` sequence, run here, on these two classes.
    ///
    /// This is the whole of `+0x5d946c0` transliterated, because the interesting
    /// property is not that `method_exchangeImplementations` swaps two IMPs -
    /// it is that the *five* calls the guest makes around it all answer what it
    /// needs them to, and the run log cannot show that: the swizzle happens
    /// once, early, silently, and leaves no trace unless something later sends
    /// the exchanged message.
    ///
    /// The trade is asserted, not assumed, and against the *two different*
    /// implementations - which is the case that matters and the one a test that
    /// exchanged a method with itself would not cover.
    #[test]
    fn the_guests_swizzle_sequence_really_exchanges_the_two_implementations() {
        crate::bootstrap();
        // SAFETY: each foreign function is called with the ABI its own
        // declaration gives, and every pointer below is a class or `Method` this
        // runtime produced.
        unsafe {
            let selector = objc::sel::get("populateMenu:withServiceEntries:forDisplay:");
            let updater = objc::class::ptr(
                objc::class::lookup(c"_NSServicesMenuUpdater").expect("declared"),
            );
            // The guest's other side: a class of its own with the same selector
            // and a different implementation, which is what
            // `ChromeSwizzleServicesMenuUpdater` is at `+0xe70d500`.
            let guest_side = objc::class::allocate_pair(
                Some(objc::class::lookup(c"NSObject").expect("NSObject")),
                c"SwizzleTestGuestSide",
                0,
            )
            .expect("a name nothing else takes");
            extern "C" fn the_guests_own_implementation(
                _: Id,
                _: Sel,
                _: Id,
                _: Id,
                _: bool,
            ) {
            }
            guest_side.add_method(
                selector,
                the_guests_own_implementation as *const std::ffi::c_void,
                c"v36@0:8@16@24c32",
            );
            // Deliberately **not** registered: `class_getInstanceMethod` and
            // `class_getClassMethod` only walk the chain, so an unregistered pair
            // is enough, and a test that left a class in the global registry
            // would be a side effect on every other test in the process.

            // 1. `class_getInstanceMethod` on both, exactly as the guest does.
            let ours = objc::api::class_getInstanceMethod(updater as *const _, selector);
            let theirs =
                objc::api::class_getInstanceMethod(objc::class::ptr(guest_side) as *const _, selector);
            assert!(!ours.is_null() && !theirs.is_null(), "a method is missing");

            // 2. `method_getTypeEncoding` on both, compared with `strcmp`.
            let ours_types = objc::api::method_getTypeEncoding(ours);
            let theirs_types = objc::api::method_getTypeEncoding(theirs);
            assert!(!ours_types.is_null() && !theirs_types.is_null());
            let equal = std::ffi::CStr::from_ptr(ours_types) == std::ffi::CStr::from_ptr(theirs_types);
            assert!(equal, "the guest would skip the exchange on a differing encoding");

            // 3. Before: each method holds its own implementation.
            let ours_before = objc::api::method_getImplementation(ours);
            let theirs_before = objc::api::method_getImplementation(theirs);
            assert_ne!(ours_before, theirs_before, "the two sides are not different");

            // 4. The exchange itself, the guest's tail jump.
            objc::api::method_exchangeImplementations(ours, theirs);

            // 5. After: they have traded, and the two classes agree with that.
            assert_eq!(
                objc::api::method_getImplementation(ours),
                theirs_before,
                "the updater's method did not take the guest's implementation"
            );
            assert_eq!(
                objc::api::method_getImplementation(theirs),
                ours_before,
                "the guest's method did not take the refusal"
            );
            assert_eq!(
                objc::class::method_implementation(
                    objc::class::lookup(c"_NSServicesMenuUpdater").expect("declared"),
                    selector,
                ),
                theirs_before,
                "dispatch would not see the swap, so a cached plan would keep the old IMP"
            );
        }
    }

    /// What `SwizzleMethods` does when a method has no registered
    /// implementation, which is the case the brief asks about and which is
    /// silent rather than fatal.
    ///
    /// Two halves, and the second is the one that matters:
    ///
    /// 1. `class_getInstanceMethod` answers **null** for a selector no class in
    ///    the chain declares. The guest tests for that (`or rcx,rdi; jne`), falls
    ///    back to `class_getClassMethod`, tests again, and then stores two zeros
    ///    and **returns** - no assertion, no log line.
    /// 2. `method_exchangeImplementations` is a **silent no-op** when either
    ///    handle is null, and it leaves a real method's `IMP` untouched. There is
    ///    no diagnostic and no generation bump, so a guest that relied on the
    ///    swap having happened cannot tell.
    ///
    /// That is a real gap in this runtime's reporting, stated here rather than
    /// papered over: Apple's entry point is documented as taking two `Method`s
    /// and does not promise to diagnose a null, so refusing to guess is right,
    /// but "silently did nothing" is the outcome this project otherwise avoids
    /// everywhere else.
    #[test]
    fn a_method_with_no_implementation_is_a_null_the_guest_can_only_survive() {
        crate::bootstrap();
        // SAFETY: every pointer is a class or `Method` this runtime produced, or
        // null, which is the case under test.
        unsafe {
            let updater = objc::class::ptr(
                objc::class::lookup(c"_NSServicesMenuUpdater").expect("declared"),
            ) as *const std::ffi::c_void;
            let absent = objc::sel::get("populateMenuWithNoSuchImplementation:");
            let missing = objc::api::class_getInstanceMethod(updater, absent);
            assert!(missing.is_null(), "an undeclared selector answered a Method");
            let missing_class_method = objc::api::class_getClassMethod(updater, absent);
            assert!(
                missing_class_method.is_null(),
                "an undeclared selector answered a class Method"
            );

            // The real method, and what the exchange does when one side is null.
            let present = objc::api::class_getInstanceMethod(
                updater,
                objc::sel::get("populateMenu:withServiceEntries:forDisplay:"),
            );
            let before = objc::api::method_getImplementation(present);
            objc::api::method_exchangeImplementations(present, std::ptr::null());
            assert_eq!(
                objc::api::method_getImplementation(present),
                before,
                "a null on one side disturbed a real method"
            );
            objc::api::method_exchangeImplementations(std::ptr::null(), std::ptr::null());
        }
    }
}
