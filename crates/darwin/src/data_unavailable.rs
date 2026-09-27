//! The 504 unbound **data** imports of `Chromium Framework` this file refuses,
//! and why each of those has no value.
//!
//! # What this is
//!
//! The Framework's unbound-import census is a count of *names this tree does not
//! answer*, and the image's own bind report - decoded by the same loader that
//! binds it at run time, and pinned by
//! [`tests::the_census_reports_a_slot_for_every_published_name`] - says these
//! **504 names occupy 558 slots**: 556 in `__DATA_CONST` and 2 in
//! `__DATA,__objc_data`. This file publishes all 504. The other unbound names are
//! `_OBJC_CLASS_$_X` references, which `ROSETTA3_NAME_MISSING_CLASSES` turns
//! into an empty class that **aborts by name** on the first message, and they
//! are deliberately left to it: a zero would answer `nil` where a named abort
//! says what to implement next.
//!
//! The number was **521**, and **519** before that, in a `const` that no test
//! derived from the table and a test that asserted the `const`. Both agreed;
//! neither was right. It is **504** now because the seventeen
//! `_kSCPropNetProxies*` keys moved to `corefoundation::sysconfig`, which
//! answers them from Apple's `SCSchemaDefinitions.h`; see "The proxy keys are
//! answered, and this is why" below, which is also the account of what those
//! keys were doing while they were here.
//!
//! Each name is published at the address of a **zeroed** cell. That is a refusal
//! and not a value, and the difference is the whole content of this file:
//!
//! * the value the guest reads is **unchanged** from what
//!   `ROSETTA3_FILL_UNBOUND_DATA=1` already gives it, so publishing these names
//!   cannot change behaviour;
//! * the address the guest holds is **ours**, so a slot that is read shows up in
//!   a debugger as an address in this module rather than as an anonymous page;
//! * the count of what is *not* implemented stops being a number in a log line
//!   and becomes a list with a call site behind every entry.
//!
//! # Why there is no value, for any of them
//!
//! **No name in this set is a version, capability or feature bit whose zero
//! turns a feature off**, which is the failure mode the census is most feared
//! for. Measured over all 1017 `__TEXT` references to the 575 slots the loader
//! binds for these names (`test/tools/data-reads.py`, which decodes every
//! instruction rather than searching for a `disp32`), the *instruction after
//! the load* is:
//!
//! | what the guest does with the loaded value | sites |
//! |---|---:|
//! | dereferences it | 939 |
//! | passes its address on in a tail call | 66 |
//! | something unrelated (a `cmp`/`test` of a register the load did not fill) | 10 |
//! | moves its address to another register | 2 |
//! | **calls it** | **0** |
//! | **jumps to it** | **0** |
//! | **branches on it** | **0** |
//! | **writes through it** | **0** |
//!
//! The three bold zeros are the load-bearing rows, and they survive the
//! remeasurement, which is the useful part: a scan that decodes instructions
//! rather than guessing at bytes reaches the same answer about the shapes that
//! would make a zero a *silent* answer. The tenth row is the one that has to be
//! read carefully - a `cmp` whose operands do not include the register the load
//! just filled is a comparison of something else, and counting it as "branches
//! on the value" is how a nine-site finding becomes a false one.
//!
//! Two things the table above does **not** cover, and both matter:
//!
//! * an **array**. 50 of these slots are elements of a 53-entry `CFStringRef`
//!   table read as `lea rcx,[rip+base]; mov rax,[rcx+rax*8]`, which has no
//!   per-element reference at all, and whose *next* instruction is
//!   `mov rdx,[rax]; test rdx,rdx; je` - a branch on the value. See the last
//!   section.
//! * a **second instruction**. The window is one instruction wide, and the
//!   proxy keys' branch is two instructions after the load.
//!
//! An earlier measurement of 1014 sites first read four of them as
//! unparseable, because the
//! disassembly window was cut at a `function_range` boundary that is not an
//! instruction boundary - 39 of macho.py's 258 raw starts are mid-instruction,
//! and 191 of those sites were anchored on one. Re-disassembled, all four
//! are `mov rax,[rip+slot]; mov rax,[rax]`, i.e. the first row. A fourth site
//! read as a comparison and is `mov rax,[rip+slot]; jmp` in the bytes.
//!
//! The two bold zeros are the load-bearing rows, and it is worth being exact
//! about them: no site calls the value, and no site branches on it. What happens
//! instead is that the load yields the **address of** the symbol, the very next
//! instruction dereferences it, and the value that reaches CoreFoundation is
//! therefore **zero** - which is what `kFoo` reads as when the `CFStringRef`
//! global behind it is null. The
//! comparison the brief is looking for is real, but it is one level down, inside
//! `CFDictionaryGetValue` and `CFEqual`, on a null. So for this image the
//! zero-fill does **not** produce "a version that reads as 0"; it produces "the
//! key is not in the dictionary", which is a different and much better-understood
//! failure.
//!
//! # The one family where that reasoning would not be enough
//!
//! `net/proxy_resolution/proxy_chain_util_apple.cc` at `Framework+0x6756470` is
//! the clearest read-and-compare in the image, and it is *not* fixed by a null:
//!
//! ```text
//! 6756494  mov  rax, [rip+…]        ; &_kCFProxyTypeHTTP
//! 675649b  mov  rsi, [rax]          ; <- a NULL, with the fill
//! 675649e  mov  rdi, rbx            ; the type string out of the system dictionary
//! 67564a1  call _CFEqual
//! 67564a6  mov  r14d, 0x4           ; ProxyServer::SCHEME_HTTP, 1 << 2
//! 67564ac  test al, al
//! 67564ae  jne  0x67564e9
//! 67564b0  … kCFProxyTypeHTTPS …    ; the same shape
//! 67564c6  … kCFProxyTypeSOCKS …
//! 67564d8  mov  eax, 0x1            ; ProxyServer::SCHEME_INVALID, 1 << 0
//! 67564df  mov  r14d, 0x10          ; SCHEME_SOCKS5, 1 << 4
//! 67564e5  cmove r14d, eax
//! 67564e9  … kCFProxyTypeNone …
//! 67564fd  je   0x6756580           ; -> ProxyChain::Direct()
//! 6756580  cmp  r14d, 0x1           ; scheme == SCHEME_INVALID?
//! ```
//!
//! The three constants are `net/base/proxy_server.h`'s bit flags, and the
//! immediates in the guest are `4`, `0x10` and `1` - the header and the binary
//! agree exactly.
//!
//! ## What the null did here was the *opposite* of what this section used to
//! say, and it is worth setting down because the wrong version sounded strictly
//! worse and so nobody questioned it
//!
//! The claim was: with every `kCFProxyType*` null, **all four `CFEqual` calls
//! fail**, `GetProxyServerScheme` returns `SCHEME_INVALID` unconditionally, the
//! "no proxy" test never fires, and `ProxyDictionaryToProxyChain` returns an
//! **empty** `ProxyChain` for every scheme.
//!
//! **All four succeed.** `CFEqual` is
//!
//! ```text
//! CFEqual(a, b):  if (a == b) return 1;      ; the identity test is FIRST
//!                 if (!a || !b) return 0;
//!                 … -isEqual: …
//! ```
//!
//! — `crates/corefoundation/src/api.rs:250`. The pointer comparison precedes
//! the null check, so **`CFEqual(NULL, NULL)` is 1**. Apple's does the same: it
//! is an identity comparison with a character comparison behind it, and identity
//! includes "both null".
//!
//! So the first test in `ProxyDictionaryToProxyChain` is
//!
//! ```text
//! 67564f6  call _CFEqual             ; CFEqual(proxy_type, kCFProxyTypeNone)
//! 67564fb  test al, al
//! 67564fd  je   0x6756580            ; "not equal" -> carry on building a chain
//! ```
//!
//! and with both operands null `al` is 1, so the `je` is **not** taken and the
//! function falls through into the `ProxyChain::Direct()` construction at
//! `0x6756503`. **Every call reached `ProxyChain::Direct()`, on every host, for
//! every scheme** — the one path that produces a working, correct-looking "go
//! direct" answer was being taken, unconditionally, for the wrong reason.
//!
//! That is this project's *worst* outcome, and it is worse than the one the old
//! note described. An empty `ProxyChain` at least does not look like an answer;
//! `ProxyChain::Direct()` is exactly right for a host with no proxy, so on
//! **this** host the defect was invisible - and there is no way to tell from the
//! outside whether a browser is direct because the host says so or because this
//! runtime could not read a name. It would have stayed invisible on a host *with*
//! a proxy too: the same `Direct()` comes back, the run succeeds, the DOM dumps,
//! and the only symptom is a browser that quietly ignores a proxy the user
//! configured.
//!
//! Measured, not inferred: `CFEqual`'s order is in the source above, and the
//! branch it decides is at `0x67564fd` in a disassembly re-read for this note.
//!
//! ## Why the seven `_kCFProxy*` names here are still refused
//!
//! Not for want of a header. `<CFNetwork/CFProxySupport.h>` is public and
//! documents each one's meaning ("`kCFProxyTypeHTTP` - the proxy is an HTTP
//! proxy") and its availability. What it does **not** carry is the **string**,
//! because these are `extern const CFStringRef` *variables*:
//!
//! ```c
//! CFN_EXPORT const CFStringRef kCFProxyTypeHTTP CF_AVAILABLE(10_5, 2_0);
//! ```
//!
//! The value lives in CFNetwork's `__cstring`. CFNetwork is closed source and is
//! not on this host, so the value is not derivable from anything reachable — and
//! a plausible `CFSTR("HTTP")` is precisely the invented constant this project
//! refuses to ship. Seven rows stay as zeroed cells: `kCFProxyTypeHTTP`,
//! `kCFProxyTypeHTTPS`, `kCFProxyTypeSOCKS`, `kCFProxyTypeNone`,
//! `kCFProxyTypeKey`, `kCFProxyHostNameKey` and `kCFProxyPortNumberKey`.
//!
//! What keeps that a **bounded, named** gap rather than a silent one is that the
//! guest passes the four type constants as *compile-time arguments* —
//! `ProxyDictionaryToProxyChain(kCFProxyTypeHTTP, …)`, so the symbol is the
//! argument and no value is read from the system — which means those four
//! comparisons are `CFEqual` of a null against a null, decide nothing, and are
//! dominated by the first one described above. Answering them would change no
//! decision on this path; it would only stop the coincidence being a
//! coincidence. The three *key* names are read together at `0x7e7c10b` by a
//! function in `configured_proxy_resolution_service.cc` — **not** the
//! `proxy_config_service_mac.cc` above — and that site is where a real value
//! would start to matter.
//!
//! # The proxy keys are answered, and this is why
//!
//! **The seventeen `_kSCPropNetProxies*` keys are no longer refusals.** They were
//! the eight that fire on the measured path, and they are now published by
//! `corefoundation::sysconfig` with the values transcribed from Apple's
//! `<SystemConfiguration/SCSchemaDefinitions.h>`, whose schema table prints the
//! string beside each name. That is a *transcription*, not a derivation, and the
//! distinction is the whole reason it was safe to do: the value of every one of
//! the seventeen is the symbol's own tail, and that coincidence is exactly what
//! made this look like a guess. It is not a guess — but the coincidence is why
//! the transcription is quoted at the point of use rather than left implicit.
//!
//! ## The two things that were wrong about the old account of them
//!
//! **One: the value was never "not derivable here".** The claim was that the
//! key string could not be checked, because the Framework's `__TEXT,__cstring`
//! holds no copy of `HTTPEnable`. True, and irrelevant: the header *does* print
//! it. "Nothing in the guest can check it" is a statement about the guest, used
//! to justify an answer about a value that came from somewhere else entirely.
//!
//! **Two: the pair of refusals was *mutually* concealing, and neither was
//! checkable alone.** `SCDynamicStoreCopyProxies` returned NULL **and** every
//! key was null, so the guest's seventeen lookups all failed — and a guest that
//! gets NULL from all seventeen lookups cannot report which of the two absences
//! caused it. Answering the dictionary alone would have produced a **worse**
//! silent wrong answer, not a better one: a present dictionary whose every value
//! the guest could not name, so every `*Enable` read still fails and the guest
//! still concludes "no proxy" — now with a dictionary in hand saying so. The
//! refusal was not a safe placeholder; it was one half of a pair whose other
//! half hid it, and the two had to move together. That is the real reason for
//! the order below, and it is a reason about *observability*, not about taste.
//!
//! ## The order the two were done in, and why it was that way
//!
//! The dictionary first, the keys second — and the reason is worth keeping,
//! because the reverse order produces a *plausible* wrong answer rather than an
//! obvious one. `SCDynamicStoreCopyProxies` answers the host's real proxy
//! configuration, and the guest's `GetCurrentProxyConfig` then reads seventeen
//! keys out of it. If the keys had been answered first and the dictionary second,
//! the dictionary would have had to be built out of the key constants to stay
//! consistent, and a dictionary built from the constants is a dictionary that
//! agrees with them no matter what the host says — which is a fabricated answer
//! with a real-looking shape. The dictionary is the fact; the keys are the
//! spelling. Spelling first would have made the spelling the truth.
//!
//! # Why the rest are not answered, name by name
//!
//! The right value for each of these is a `CFStringRef` (or an ObjC object, or a
//! `CMTime`) that **belongs to a framework this host does not have**, and its
//! *contents* are a string this tree cannot know:
//! * `__NSArray0__` and `__NSDictionary0__` are the shared empty-collection
//!   singletons. They are real and `crates/foundation` owns them; this crate
//!   must not depend on that, and a second empty array is a second identity.
//! * `__OBJC_EHTYPE_id` and `__OBJC_EHTYPE_$_NSException` are the ObjC runtime's
//!   own exception-type records. They get a cell of their own - [`EHTYPE`] -
//!   because what a debugger then knows about the address is true and specific,
//!   but the eight bytes there are still zero, and why they have to be is in
//!   [`EHTYPE`]'s own note.
//! * `__objc_personality_v0` **is not here any more**, and that is the one
//!   substantive change this file has made. It is a **function pointer in a data
//!   slot**, read by `__TEXT,__unwind_info` rather than by any instruction, so a
//!   zero cell there is a jump to the first instruction of a read-only zero page
//!   the moment the guest's unwinder calls it - and the unwinder *does* call it:
//!   the Framework's `__TEXT,__unwind_info` names it through the four bytes at
//!   `0xdc1ae74`, whose value is `0x0dd51b28` - the image address of a
//!   `__DATA_CONST,__got` slot - and that word is the **only** reference to the
//!   name in the whole image. (Those four bytes are an **absolute** address, not
//!   a `DW_EH_PE_pcrel|sdata4` offset: a pcrel encoding of the same reference
//!   would hold `0x136cb4`, the difference between the two, and it does not.
//!   An earlier version of this note called it pcrel. `test/tools/data-reads.py`
//!   reports it as `abs32` and the distinction matters, because a `pcrel` test
//!   over `__unwind_info` is 50% noise and an `abs32` test over it is not.)
//!   It is now published as a function - [`objc_personality_refused`] - which
//!   stops being a jump and becomes an abort that says which symbol it was. The
//!   truthful answer is libobjc's `__objc_personality_v0`, which pairs with a
//!   libobjc this tree does not have; the reasoning, and why libc++abi's
//!   `__gxx_personality_v0` is not it either, is in that function's note.
//! * `_kCMTimeInvalid`, `_CGRectNull` and `_CATransform3DIdentity` are struct
//!   constants. A page of zeros read as a `CMTime` is all-zero, which is *also*
//!   what `kCMTimeInvalid` is on any reading of the header I can verify - and
//!   "accidentally right" is the outcome this project treats as worst, so it is
//!   not claimed.
//!
//! So: no value is invented here, and the count of unimplemented data imports
//! stops being invisible.
//!
//! # And the measurement that says what of this *is* read
//!
//! **This section used to say "none of it is read on the measured path", and
//! that was false.**  It was a statement about one `gdb` run at *function-entry*
//! granularity, carried forward as though it were a statement about the image.
//! Measured properly, over the 1017 decoded instructions that read these 575
//! slots ([`tests::the_census_reports_a_slot_for_every_published_name`] for the
//! slots, `test/tools/data-reads.py` for the reads):
//!
//! | | |
//! |---|---:|
//! | slots read by at least one instruction | **521 of 575** |
//! | code references | **1017** |
//! | slots with no reference of any kind | 2 (the two `_OBJC_METACLASS_$_ASAuthorizationController` sites) |
//! | slots no instruction reads but `__TEXT,__gcc_except_tab` names | 2 (`_OBJC_EHTYPE_id`, `_OBJC_EHTYPE_$_NSException`) |
//!
//! and, in one `gdb` run of the `--headless --dump-dom about:blank` milestone
//! with a breakpoint on every one of the 1017 sites and two positive controls
//! in the same instrument (37 and 38 hits), **17 of them fired**:
//!
//! ```text
//! _kSCPropNetProxiesHTTPEnable   _kSCPropNetProxiesHTTPSEnable
//! _kSCPropNetProxiesSOCKSEnable  _kSCPropNetProxiesFTPEnable
//! _kSCPropNetProxiesProxyAutoConfigEnable
//! _kSCPropNetProxiesProxyAutoDiscoveryEnable
//! _kSCPropNetProxiesExceptionsList
//! _kSCPropNetProxiesExcludeSimpleHostnames
//! _kSecReturnRef  _kSecMatchLimitAll  _kSecPolicyAppleSSL  _kSecClassCertificate
//! _NSURLIsExcludedFromBackupKey  _NSScreenColorSpaceDidChangeNotification
//! _NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification
//! _NSWorkspaceWillPowerOffNotification
//! _kMACaptionAppearanceSettingsChangedNotification
//! ```
//!
//! Eight of the eleven SystemConfiguration keys fire at
//! `net/proxy_resolution/proxy_config_service_mac.cc`, and the shape is the one
//! this file's own note describes, reached for real:
//!
//! ```text
//! 674bad0  mov  rax, [rip+0x7604fe9]   ; &_kSCPropNetProxiesHTTPEnable
//! 674bad7  mov  rsi, [rax]             ; <- a NULL, with the fill
//! 674bada  mov  rdi, r13               ; the proxy dictionary this function built
//! 674badd  call _CFDictionaryGetValue
//! 674bae2  test rax, rax
//! 674bae5  je   0x674bbea              ; "HTTP proxy is not enabled"
//! ```
//!
//! So the wrong decision this file documents is not hypothetical and not
//! future: it is being taken, on the milestone's own path, eight times per
//! start-up, and the decision it takes is indistinguishable from the guest
//! having decided it.  `SCDynamicStoreCopyProxies` - the function that would
//! fill `r13` - is answered `NULL` by `crates/corefoundation/src/sysconfig.rs`,
//! so on this host the guest learns "no proxy" from *two* absences rather than
//! from a configuration.
//!
//! **The other half of the old claim does survive, and it is the half that
//! matters.**  Over all 1017 sites the guest never *calls* one of these values,
//! never *jumps* to one, and never *branches on* one: the instruction after the
//! load is a dereference 939 times, a tail call that passes the address on 66, a
//! `mov` of the address 2, and something unrelated 10.  A zero is therefore an
//! absence everywhere except the 8 proxy keys, where the absence is *reported to
//! the guest as data* rather than faulting.
//!
//! One methodological note, because it changed the answer: a per-element
//! reference scan calls 50 of these slots unreferenced, and they are not.  They
//! are a 53-entry array of `CFStringRef` at `__DATA_CONST,__const`
//! `0x0e407858`, read as `lea rcx,[rip+base]; mov rax,[rcx+rax*8]` at
//! `0x2fd6252`, `0x2fd6293` and `0x2fd6357`, and the instruction after *that*
//! is `mov rdx,[rax]; test rdx,rdx; je` - a **branch on the value**.  An array
//! has no per-element reference, so the negative result was an artefact of the
//! method, not a property of the image.
//!
//! # Three things this file is careful about
//!
//! * **The spellings.** `exports!`'s `data` section takes the Mach-O spelling
//!   **verbatim** and the synthetic dylib resolves imports verbatim, with no
//!   normalisation. Four of these names carry a second or third leading
//!   underscore - `___NSArray0__`, `___NSDictionary0__`, `__kLSOpenOptionBackgroundLaunchKey`,
//!   and, in the `functions` section, `__objc_personality_v0` - and a clean build
//!   and a green suite both accept a wrong spelling.
//!   [`names_are_the_image_spellings`] pins every one of them against
//!   `__TEXT,__stubs`' indirect symbol table *and* against [`crate::base_name`]'s
//!   reduction, which strips exactly one underscore and folds a `$` suffix. The
//!   two sections spell the *same* name differently on purpose and the trap is
//!   in the difference: a `functions` row is the **bare C** spelling and
//!   `exports!` prepends the Mach-O underscore, so
//!   `"__objc_personality_v0"` is what publishes `___objc_personality_v0`.
//!   [`the_personality_is_a_function_and_not_a_cell`] pins both directions.
//! * **The 128 class references are not here**, for the reason in the first
//!   section. `no_class_reference_is_published_here` is the test that keeps that
//!   true, because a zero would quietly take away a diagnostic.

#[cfg(not(target_os = "macos"))]
mod imp {
    //! The cells, the one-shot line, and the table.
    //!
    //! One cell per *shape* rather than per name, which is what the
    //! disassembly says these are: at 1007 of the 1017 reference sites the
    //! loaded value is dereferenced, returned or stored, and **never** called and
    //! **never** branched on - and no site *writes* through it, so a cell shared
    //! by a whole family cannot be corrupted by one name being read. What a
    //! family does with the zero differs (`PTR` hands CoreFoundation a null key,
    //! `RECT` hands it a null rectangle, `METACLASS` makes an `isa` null); what it
    //! is - absent - does not, and a cell per name would be 522 statics saying
    //! the same thing.

    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// How many names this module publishes as **data**, for the one-shot line
    /// and for [`published`]'s test. Kept next to the count the tests check so
    /// the two cannot drift.
    ///
    /// It was **519** and the table had **521** `data` rows, which is the
    /// failure this project exists to catch: a hard-coded count that nobody
    /// derives from the table it describes, and a test that asserts the
    /// hard-coded number, so the two agree with each other and both disagree
    /// with reality. [`tests::the_census_reports_a_slot_for_every_published_name`]
    /// now derives the number from the loader's own bind report, which is the
    /// only source that cannot be a guess.
    ///
    /// **521, then 504.** The seventeen `_kSCPropNetProxies*` keys left for
    /// `corefoundation::sysconfig`, which answers them with the string values
    /// Apple's `SCSchemaDefinitions.h` gives - see the module note's section on
    /// the proxy dictionary, which is the account of why those seventeen and not
    /// others, and of what the null was doing. The count moved by exactly the
    /// seventeen rows that left and by nothing else, which is what makes the
    /// drop evidence rather than a coincidence: a *different* number would mean
    /// a row moved that was not supposed to.
    const PUBLISHED: usize = 504;

    /// How many it publishes as a **function**, which is a different shape and a
    /// different claim: a function address is somewhere a guest can *arrive*,
    /// so it has to be code, and a zero there is a jump into a read-only zero
    /// page. Exactly one, and [`objc_personality_refused`] is it.
    const FUNCTIONS: usize = 1;

    /// The 130 `_OBJC_CLASS_$_X` references left to `ROSETTA3_NAME_MISSING_CLASSES`.
    ///
    /// In the census these are 130 of the 706 bind sites. They are *not* in the
    /// table below and [`tests::no_class_reference_is_published_here`] is what
    /// keeps them out.
    pub const CLASS_REFERENCES_LEFT: usize = 128;

    /// A refusal cell: eight zero bytes, which is what a null pointer-sized
    /// global reads as.
    ///
    /// `repr(C, align(8))` so the address the loader writes into a `__got` slot
    /// is the address of those eight bytes and nothing else, and so a guest that
    /// reads all eight gets zero rather than a neighbouring constant.
    #[repr(C, align(8))]
    pub struct Refused([u8; 8]);

    /// A pointer-sized global: a `CFStringRef`, an `NSString *`, a notification
    /// name, an `id`. The load gives the address of the global and the next
    /// instruction reads it, so the guest receives **zero**.
    pub static PTR: Refused = Refused([0; 8]);

    /// A 4-byte scalar global: `AVSpeechUtteranceDefaultSpeechRate`,
    /// `AVSpeechUtteranceMaximumSpeechRate`, `NSURLSessionTaskPriorityHigh`.
    ///
    /// `movss` / `movd` from the cell: zero. For the two speech rates that is a
    /// rate of 0.0 and for the task priority it is the *lowest* priority rather
    /// than the highest, so the decision it steers is a scheduling one.
    pub static F32: Refused = Refused([0; 8]);

    /// An 8-byte scalar global: `kCLLocationAccuracyHundredMeters`, a `double`.
    ///
    /// A geolocation accuracy of 0.0 is a claim about the fix that this host
    /// cannot make; see the module note for why it is not invented.
    pub static F64: Refused = Refused([0; 8]);

    /// A 16-byte struct global: `kCMTimeInvalid` (a `CMTime`) and `CGRectNull`.
    ///
    /// Zeroes here are *also* the plausible reading of both constants, which is
    /// exactly why this file does not claim them: a refusal that happens to be
    /// right is a refusal that will never be noticed being wrong.
    pub static RECT: Refused = Refused([0; 8]);

    /// A 64-byte affine transform, read as `[cell + 0x70]`:
    /// `CATransform3DIdentity`.
    pub static AFFINE: Refused = Refused([0; 8]);

    /// `_OBJC_METACLASS_$_ASAuthorizationController`, and the only name here
    /// where the zero is a *structure* rather than an absence.
    ///
    /// It is bound twice in `__DATA,__objc_data` and it is the `isa` of a class
    /// the Framework itself declares, so a null here is a class whose `isa` is
    /// null. Nothing on the measured path sends a message to it. It is published
    /// rather than left unbound so that the census reaches a number this file can
    /// account for, and it is called out here so that the number is not read as
    /// "all implemented".
    pub static METACLASS: Refused = Refused([0; 8]);

    /// `_OBJC_EHTYPE_$_NSException` and `_OBJC_EHTYPE_id`, which get a cell of
    /// their own rather than sharing `PTR`'s.
    ///
    /// The bytes are still zero, and the reason is Apple's, not this file's.
    /// On 64-bit macOS these are libobjc's **`__objc_exception_type`**, which is
    /// one word - `{ const char *name }` - and a personality routine matches a
    /// catch against it by **string name**, walking the thrown class's
    /// superclasses. The Framework *imports* both names from
    /// `libobjc.A.dylib`; its own symbol table contains no `_OBJC_EHTYPE_$_X`
    /// definition at all, so there is nothing in the guest to read and no layout
    /// to be derived from one. A zero here is therefore a personality routine
    /// that dereferences null, which is a crash *inside* the unwinder - and
    /// unwinding is exactly what a fault report must not be doing.
    ///
    /// Its own cell rather than `PTR`'s costs 8 bytes and buys one thing: a
    /// debugger that lands here can be told *which* absent thing it is, instead
    /// of being told "a pointer-sized global", which for a `CFStringRef` key and
    /// for an exception-type record are not the same absence.
    pub static EHTYPE: Refused = Refused([0; 8]);

    /// `int __objc_personality_v0(int version, int actions, uint64_t exception_class,
    /// _Unwind_Exception *exception_object, _Unwind_Context *context)`.
    ///
    /// The **only** consumer of this name is the unwinder, and it is a real
    /// consumer: the Framework's `__TEXT,__unwind_info` names its personality
    /// through a four-byte `DW_EH_PE_pcrel|sdata4` at `0xdc1ae74` whose value
    /// `0x0dd51b28` is a `__DATA_CONST,__got` slot, and that four-byte word is
    /// the only reference to the name in the whole image. So the address in that
    /// slot is a function pointer, it is called, and publishing eight zero bytes
    /// for it makes the first exception to unwind through guest code jump to
    /// the first instruction of a read-only page of zeros - a fault with no
    /// symbol, no name, and no message. That is the worst outcome this project
    /// has, and it was reported *bound* by the unbound-import census, because
    /// binding a name and having an answer for it are different things.
    ///
    /// So it is a function, and it stops. What the truthful answer is, and why
    /// the two candidates are both wrong:
    ///
    /// * **libobjc's `__objc_personality_v0`** is correct and is not available:
    ///   it is the Apple Objective-C EH ABI, and it needs libobjc's
    ///   `__objc_exception_type` records to match against - the two names above,
    ///   which live in the libobjc this tree does not have.
    /// * **libc++abi's `__gxx_personality_v0`** is *available* - this tree
    ///   vendors it - and it is **wrong here**. It reads a catch type as the
    ///   Itanium `__si_class_type_info { vptr, name, base }` and calls the
    ///   vtable's `__do_catch`, where Apple's is a one-word name compare. On
    ///   64-bit macOS the two are different routines in different libraries;
    ///   `crates/rosetta3/src/abi/table.rs` calls them "the same routine" and
    ///   that sentence is wrong. Substituting it would turn "abort by name" into
    ///   "call through the first word of a record we do not have", which is the
    ///   same class of fault wearing a better name.
    ///
    /// Aborting is the designed refusal: it is what this tree already does for
    /// an unanswered selector, and it says which symbol was reached and which
    /// capability is missing. There is no argument for returning a
    /// `_Unwind_Reason_Code` instead, because every value in that enum is either
    /// a lie about having searched the frames or an instruction to terminate.
    ///
    /// `write(2)` rather than `eprintln!` for [`refused`]'s reason: the message
    /// has to be out before the process stops.
    pub extern "C" fn objc_personality_refused(
        _version: libc::c_int,
        _actions: libc::c_int,
        _exception_class: u64,
        _exception_object: *mut c_void,
        _context: *mut c_void,
    ) -> libc::c_int {
        const LINE: &[u8] = b"rosetta3: ___objc_personality_v0: refused by name.\n\
            The guest's unwinder called the Objective-C exception personality routine, which is\n\
            libobjc's and needs libobjc's __objc_exception_type records to match a catch against;\n\
            neither exists here, and libc++abi's __gxx_personality_v0 is a different ABI (Itanium\n\
            __si_class_type_info, not Apple's one-word { const char *name }) and would call through\n\
            a vtable this tree has no value for. The guest is right to need this: Chromium Framework\n\
            names this routine from __TEXT,__unwind_info. No Objective-C exception can be caught or\n\
            thrown in this program until it is implemented.\n";
        // SAFETY: `LINE` is a `&'static [u8]`, so the pointer and the length are
        // both valid for the length read, and `write` is called before anything
        // that can stop the process.
        unsafe { libc::write(2, LINE.as_ptr().cast(), LINE.len()) };
        // Nothing here is recoverable. The two reasons a personality routine
        // cannot answer are that it found no handler - in which case the guest's
        // throw is already lost - and that it is not a personality routine at
        // all. Both are `std::terminate` on a real system, and both are this
        // process stopping with the name of the missing capability in its last
        // line.
        std::process::abort()
    }

    /// The address `exports!` publishes for the personality, and the one place
    /// the one-shot line is still printed from a value expression.
    ///
    /// Split from [`objc_personality_refused`] so that the announcement happens
    /// when the **loader** publishes the name, on the same footing as the 521
    /// data rows, and not only if and when the unwinder gets as far as calling
    /// it. The arguments are the personality ABI's own, passed straight through
    /// so that the abort happens in the one function that documents it.
    fn announce_and_abort(
        version: libc::c_int,
        actions: libc::c_int,
        exception_class: u64,
        exception_object: *mut c_void,
        context: *mut c_void,
    ) -> libc::c_int {
        announce();
        objc_personality_refused(version, actions, exception_class, exception_object, context)
    }

    /// The cell a family reads through, and the one-shot line.
    ///
    /// The line is printed from **inside a `data` row's value expression**, which
    /// `provide()` evaluates at link time - so the refusal is announced exactly
    /// when the loader is publishing these names, with no edit to a shared
    /// registry and no constructor. `write(2)` rather than `eprintln!` for
    /// [`crate::unavailable`]'s reason: a message about a refusal has to be out
    /// before the guest runs, and a buffered one can be lost. Allocating to build
    /// it is safe for the reason given there: this runtime suspends nothing, so
    /// no thread is ever stopped holding this tree's allocator lock.
    fn refused(cell: &'static Refused) -> u64 {
        announce();
        cell as *const Refused as usize as u64
    }

    /// The one-shot line, from whichever of this module's rows is published
    /// first.
    ///
    /// Its own function because the personality is a `functions` row, whose
    /// value expression is a function address rather than a `refused(&CELL)`
    /// call, and it must say the same thing about itself. One flag either way.
    ///
    /// The line does **not** say "none is read on the measured path". It said
    /// that, and it was wrong: a `gdb` run with a breakpoint on each of the
    /// 1017 instructions that read these slots, and two positive controls in
    /// the same instrument, watched **17 of them fire** on
    /// `--headless --dump-dom about:blank` - eight SystemConfiguration proxy
    /// keys among them, each of which is a null `CFStringRef` handed to
    /// `CFDictionaryGetValue` and answered "not configured". The module note
    /// has the addresses and the disassembly.
    fn announce() {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            let line = format!(
                "rosetta3: data_unavailable: {PUBLISHED} data import(s) published as zeroed \
                 refusals, at 558 slot(s) the loader binds; 1000 instruction(s) in the image \
                 read them and 9 of those are on the measured --dump-dom path \
                 (test/tools/data-reads.py). The 17 _kSCPropNetProxies* keys that used to be \
                 among both numbers are answered by corefoundation::sysconfig. \
                 {FUNCTIONS} more, ___objc_personality_v0, is a function and refuses by name, \
                 because a zero cell there is a call into a read-only page. The \
                 {CLASS_REFERENCES_LEFT} _OBJC_CLASS_$_X references are bound to named empty \
                 classes instead (ROSETTA3_NAME_MISSING_CLASSES).\n"
            );
            // SAFETY: a live buffer for the length written.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
    }

    /// How many names this module publishes as data. Read by the tests, and
    /// available to a future diagnostic that wants to say what a slot's address
    /// means.
    pub const fn published() -> usize {
        PUBLISHED
    }

    /// The cells, by name, for a diagnostic that is handed an address.
    ///
    /// Seven entries, not 521: a guest that read one of these holds an address in
    /// this module, and the most that can honestly be said about it is *which
    /// shape of absent thing* it was reading. Which of the 521 names it was is
    /// not recoverable from the address, because a family shares a cell - that is
    /// the trade [`imp`]'s note records.
    #[must_use]
    pub fn cell_address(which: Cell) -> u64 {
        match which {
            Cell::Ptr => &raw const PTR as usize as u64,
            Cell::F32 => &raw const F32 as usize as u64,
            Cell::F64 => &raw const F64 as usize as u64,
            Cell::Rect => &raw const RECT as usize as u64,
            Cell::Affine => &raw const AFFINE as usize as u64,
            Cell::Metaclass => &raw const METACLASS as usize as u64,
            Cell::Ehtype => &raw const EHTYPE as usize as u64,
        }
    }

    /// Which of the seven cells an address is, if it is one of ours.
    pub fn which_cell(address: u64) -> Option<Cell> {
        [
            Cell::Ptr,
            Cell::F32,
            Cell::F64,
            Cell::Rect,
            Cell::Affine,
            Cell::Metaclass,
            Cell::Ehtype,
        ]
        .into_iter()
        .find(|cell| cell_address(*cell) == address)
    }

    /// The seven cells, named. Deliberately `Copy + Debug` and nothing else: this
    /// is a label for a diagnostic, not a handle to anything a caller may write.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Cell {
        /// A pointer-sized global - a `CFStringRef` or an `id`.
        Ptr,
        /// A 4-byte scalar global.
        F32,
        /// An 8-byte scalar global.
        F64,
        /// A 16-byte struct global.
        Rect,
        /// A 64-byte affine transform, read at `+0x70`.
        Affine,
        /// An Objective-C metaclass pointer.
        Metaclass,
        /// An Objective-C exception-type record: `_OBJC_EHTYPE_$_X`, and
        /// `_OBJC_EHTYPE_id`.
        Ehtype,
    }

    exports::exports! {
        // One function, and it is here for the reason its own note gives: its
        // slot holds a *pointer a guest arrives at*, so a zero there is a jump
        // into a read-only page rather than an absence anybody can read. The
        // `functions` section takes the **bare C** spelling and prepends the
        // Mach-O underscore, so this row publishes `___objc_personality_v0` -
        // three of them, which is the image's own spelling and one more than
        // `c_name`'s reduction leaves.
        functions {
            "__objc_personality_v0" => announce_and_abort,
        }

        // Every name below is a Mach-O spelling, written verbatim: the `data`
        // section does not add an underscore, and the loader resolves an import
        // with no normalisation either. `test/tools/macho.py`'s `ist_name` over
        // `__TEXT,__stubs` confirms each one; see
        // `names_are_the_image_spellings`.
        data {

        // ---- AppKit: 204 name(s), bound at 204 site(s) in __DATA_CONST,__got
            "_NSAccessibilityAnnouncementKey" => refused(&PTR),
            "_NSAccessibilityAnnouncementRequestedNotification" => refused(&PTR),
            "_NSAccessibilityApplicationActivatedNotification" => refused(&PTR),
            "_NSAccessibilityApplicationDeactivatedNotification" => refused(&PTR),
            "_NSAccessibilityApplicationHiddenNotification" => refused(&PTR),
            "_NSAccessibilityApplicationShownNotification" => refused(&PTR),
            "_NSAccessibilityAscendingSortDirectionValue" => refused(&PTR),
            "_NSAccessibilityAttributedStringForRangeParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityBackgroundColorTextAttribute" => refused(&PTR),
            "_NSAccessibilityBoundsForRangeParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityButtonRole" => refused(&PTR),
            "_NSAccessibilityCancelAction" => refused(&PTR),
            "_NSAccessibilityCellForColumnAndRowParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityCellRole" => refused(&PTR),
            "_NSAccessibilityCheckBoxRole" => refused(&PTR),
            "_NSAccessibilityChildrenAttribute" => refused(&PTR),
            "_NSAccessibilityColorWellRole" => refused(&PTR),
            "_NSAccessibilityColumnHeaderUIElementsAttribute" => refused(&PTR),
            "_NSAccessibilityColumnIndexRangeAttribute" => refused(&PTR),
            "_NSAccessibilityColumnRole" => refused(&PTR),
            "_NSAccessibilityColumnsAttribute" => refused(&PTR),
            "_NSAccessibilityComboBoxRole" => refused(&PTR),
            "_NSAccessibilityContentListSubrole" => refused(&PTR),
            "_NSAccessibilityContentsAttribute" => refused(&PTR),
            "_NSAccessibilityCreatedNotification" => refused(&PTR),
            "_NSAccessibilityDecrementAction" => refused(&PTR),
            "_NSAccessibilityDefinitionListSubrole" => refused(&PTR),
            "_NSAccessibilityDescendingSortDirectionValue" => refused(&PTR),
            "_NSAccessibilityDescriptionAttribute" => refused(&PTR),
            "_NSAccessibilityDisclosedByRowAttribute" => refused(&PTR),
            "_NSAccessibilityDisclosedRowsAttribute" => refused(&PTR),
            "_NSAccessibilityDisclosingAttribute" => refused(&PTR),
            "_NSAccessibilityDisclosureLevelAttribute" => refused(&PTR),
            "_NSAccessibilityDrawerCreatedNotification" => refused(&PTR),
            "_NSAccessibilityEnabledAttribute" => refused(&PTR),
            "_NSAccessibilityExpandedAttribute" => refused(&PTR),
            "_NSAccessibilityFocusedAttribute" => refused(&PTR),
            "_NSAccessibilityFocusedUIElementChangedNotification" => refused(&PTR),
            "_NSAccessibilityFocusedWindowChangedNotification" => refused(&PTR),
            "_NSAccessibilityFontSizeKey" => refused(&PTR),
            "_NSAccessibilityFontTextAttribute" => refused(&PTR),
            "_NSAccessibilityForegroundColorTextAttribute" => refused(&PTR),
            "_NSAccessibilityGridRole" => refused(&PTR),
            "_NSAccessibilityGroupRole" => refused(&PTR),
            "_NSAccessibilityHeaderAttribute" => refused(&PTR),
            "_NSAccessibilityHelpAttribute" => refused(&PTR),
            "_NSAccessibilityHelpTagCreatedNotification" => refused(&PTR),
            "_NSAccessibilityHelpTagRole" => refused(&PTR),
            "_NSAccessibilityHorizontalOrientationValue" => refused(&PTR),
            "_NSAccessibilityImageRole" => refused(&PTR),
            "_NSAccessibilityIncrementAction" => refused(&PTR),
            "_NSAccessibilityIncrementorRole" => refused(&PTR),
            "_NSAccessibilityIndexAttribute" => refused(&PTR),
            "_NSAccessibilityInsertionPointLineNumberAttribute" => refused(&PTR),
            "_NSAccessibilityLayoutChangedNotification" => refused(&PTR),
            "_NSAccessibilityLevelIndicatorRole" => refused(&PTR),
            "_NSAccessibilityLineForIndexParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityLinkRole" => refused(&PTR),
            "_NSAccessibilityLinkedUIElementsAttribute" => refused(&PTR),
            "_NSAccessibilityListRole" => refused(&PTR),
            "_NSAccessibilityMainWindowChangedNotification" => refused(&PTR),
            "_NSAccessibilityMarkedMisspelledTextAttribute" => refused(&PTR),
            "_NSAccessibilityMaxValueAttribute" => refused(&PTR),
            "_NSAccessibilityMenuBarRole" => refused(&PTR),
            "_NSAccessibilityMenuItemRole" => refused(&PTR),
            "_NSAccessibilityMenuRole" => refused(&PTR),
            "_NSAccessibilityMinValueAttribute" => refused(&PTR),
            "_NSAccessibilityMovedNotification" => refused(&PTR),
            "_NSAccessibilityNumberOfCharactersAttribute" => refused(&PTR),
            "_NSAccessibilityOrientationAttribute" => refused(&PTR),
            "_NSAccessibilityOutlineRole" => refused(&PTR),
            "_NSAccessibilityOutlineRowSubrole" => refused(&PTR),
            "_NSAccessibilityParentAttribute" => refused(&PTR),
            "_NSAccessibilityPlaceholderValueAttribute" => refused(&PTR),
            "_NSAccessibilityPopUpButtonRole" => refused(&PTR),
            "_NSAccessibilityPositionAttribute" => refused(&PTR),
            "_NSAccessibilityPressAction" => refused(&PTR),
            "_NSAccessibilityPriorityKey" => refused(&PTR),
            "_NSAccessibilityProgressIndicatorRole" => refused(&PTR),
            "_NSAccessibilityRTFForRangeParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityRadioButtonRole" => refused(&PTR),
            "_NSAccessibilityRadioGroupRole" => refused(&PTR),
            "_NSAccessibilityRangeForIndexParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityRangeForLineParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityRangeForPositionParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityRequiredAttribute" => refused(&PTR),
            "_NSAccessibilityResizedNotification" => refused(&PTR),
            "_NSAccessibilityRoleAttribute" => refused(&PTR),
            "_NSAccessibilityRoleDescriptionAttribute" => refused(&PTR),
            "_NSAccessibilityRowCollapsedNotification" => refused(&PTR),
            "_NSAccessibilityRowCountChangedNotification" => refused(&PTR),
            "_NSAccessibilityRowExpandedNotification" => refused(&PTR),
            "_NSAccessibilityRowHeaderUIElementsAttribute" => refused(&PTR),
            "_NSAccessibilityRowIndexRangeAttribute" => refused(&PTR),
            "_NSAccessibilityRowRole" => refused(&PTR),
            "_NSAccessibilityRowsAttribute" => refused(&PTR),
            "_NSAccessibilityScrollAreaRole" => refused(&PTR),
            "_NSAccessibilityScrollBarRole" => refused(&PTR),
            "_NSAccessibilitySecureTextFieldSubrole" => refused(&PTR),
            "_NSAccessibilitySelectedAttribute" => refused(&PTR),
            "_NSAccessibilitySelectedCellsChangedNotification" => refused(&PTR),
            "_NSAccessibilitySelectedChildrenAttribute" => refused(&PTR),
            "_NSAccessibilitySelectedChildrenChangedNotification" => refused(&PTR),
            "_NSAccessibilitySelectedChildrenMovedNotification" => refused(&PTR),
            "_NSAccessibilitySelectedColumnsChangedNotification" => refused(&PTR),
            "_NSAccessibilitySelectedRowsAttribute" => refused(&PTR),
            "_NSAccessibilitySelectedRowsChangedNotification" => refused(&PTR),
            "_NSAccessibilitySelectedTextAttribute" => refused(&PTR),
            "_NSAccessibilitySelectedTextChangedNotification" => refused(&PTR),
            "_NSAccessibilitySelectedTextRangeAttribute" => refused(&PTR),
            "_NSAccessibilitySheetCreatedNotification" => refused(&PTR),
            "_NSAccessibilityShowMenuAction" => refused(&PTR),
            "_NSAccessibilitySizeAttribute" => refused(&PTR),
            "_NSAccessibilitySliderRole" => refused(&PTR),
            "_NSAccessibilitySortDirectionAttribute" => refused(&PTR),
            "_NSAccessibilitySplitterRole" => refused(&PTR),
            "_NSAccessibilityStaticTextRole" => refused(&PTR),
            "_NSAccessibilityStrikethroughTextAttribute" => refused(&PTR),
            "_NSAccessibilityStringForRangeParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilityStyleRangeForIndexParameterizedAttribute" => refused(&PTR),
            "_NSAccessibilitySubroleAttribute" => refused(&PTR),
            "_NSAccessibilityTabGroupRole" => refused(&PTR),
            "_NSAccessibilityTableRole" => refused(&PTR),
            "_NSAccessibilityTabsAttribute" => refused(&PTR),
            "_NSAccessibilityTextAreaRole" => refused(&PTR),
            "_NSAccessibilityTextFieldRole" => refused(&PTR),
            "_NSAccessibilityTitleAttribute" => refused(&PTR),
            "_NSAccessibilityTitleChangedNotification" => refused(&PTR),
            "_NSAccessibilityTitleUIElementAttribute" => refused(&PTR),
            "_NSAccessibilityToolbarRole" => refused(&PTR),
            "_NSAccessibilityTopLevelUIElementAttribute" => refused(&PTR),
            "_NSAccessibilityUIElementDestroyedNotification" => refused(&PTR),
            "_NSAccessibilityURLAttribute" => refused(&PTR),
            "_NSAccessibilityUnderlineTextAttribute" => refused(&PTR),
            "_NSAccessibilityUnitsChangedNotification" => refused(&PTR),
            "_NSAccessibilityUnknownRole" => refused(&PTR),
            "_NSAccessibilityUnknownSortDirectionValue" => refused(&PTR),
            "_NSAccessibilityValueAttribute" => refused(&PTR),
            "_NSAccessibilityValueChangedNotification" => refused(&PTR),
            "_NSAccessibilityValueDescriptionAttribute" => refused(&PTR),
            "_NSAccessibilityVerticalOrientationValue" => refused(&PTR),
            "_NSAccessibilityVisibleCellsAttribute" => refused(&PTR),
            "_NSAccessibilityVisibleCharacterRangeAttribute" => refused(&PTR),
            "_NSAccessibilityVisibleChildrenAttribute" => refused(&PTR),
            "_NSAccessibilityVisibleColumnsAttribute" => refused(&PTR),
            "_NSAccessibilityVisibleRowsAttribute" => refused(&PTR),
            "_NSAccessibilityWindowAttribute" => refused(&PTR),
            "_NSAccessibilityWindowCreatedNotification" => refused(&PTR),
            "_NSAccessibilityWindowDeminiaturizedNotification" => refused(&PTR),
            "_NSAccessibilityWindowMiniaturizedNotification" => refused(&PTR),
            "_NSAccessibilityWindowMovedNotification" => refused(&PTR),
            "_NSAccessibilityWindowResizedNotification" => refused(&PTR),
            "_NSAppearanceNameAccessibilityHighContrastAqua" => refused(&PTR),
            "_NSAppearanceNameAccessibilityHighContrastDarkAqua" => refused(&PTR),
            "_NSApplicationDidHideNotification" => refused(&PTR),
            "_NSApplicationDidResignActiveNotification" => refused(&PTR),
            "_NSApplicationLaunchUserNotificationKey" => refused(&PTR),
            "_NSBackgroundColorAttributeName" => refused(&PTR),
            "_NSDisplayNameBindingOption" => refused(&PTR),
            "_NSDisplayPatternBindingOption" => refused(&PTR),
            "_NSEventTrackingRunLoopMode" => refused(&PTR),
            "_NSFontNameAttribute" => refused(&PTR),
            "_NSFontSymbolicTrait" => refused(&PTR),
            "_NSFontTraitsAttribute" => refused(&PTR),
            "_NSImageHintCTM" => refused(&PTR),
            "_NSImageNameFolder" => refused(&PTR),
            "_NSMarkedClauseSegmentAttributeName" => refused(&PTR),
            "_NSMenuDidBeginTrackingNotification" => refused(&PTR),
            "_NSMenuDidEndTrackingNotification" => refused(&PTR),
            "_NSMultipleValuesPlaceholderBindingOption" => refused(&PTR),
            "_NSNoSelectionPlaceholderBindingOption" => refused(&PTR),
            "_NSNotApplicablePlaceholderBindingOption" => refused(&PTR),
            "_NSNullPlaceholderBindingOption" => refused(&PTR),
            "_NSObservedKeyPathKey" => refused(&PTR),
            "_NSObservedObjectKey" => refused(&PTR),
            "_NSOptionsKey" => refused(&PTR),
            "_NSPasteboardNameFind" => refused(&PTR),
            "_NSPasteboardNameGeneral" => refused(&PTR),
            "_NSPasteboardTypeFileURL" => refused(&PTR),
            "_NSPasteboardTypeHTML" => refused(&PTR),
            "_NSPasteboardTypePNG" => refused(&PTR),
            "_NSPasteboardTypeRTF" => refused(&PTR),
            "_NSPasteboardTypeTIFF" => refused(&PTR),
            "_NSPasteboardTypeURL" => refused(&PTR),
            "_NSPrintAllPages" => refused(&PTR),
            "_NSPrintFirstPage" => refused(&PTR),
            "_NSPrintLastPage" => refused(&PTR),
            "_NSScreenColorSpaceDidChangeNotification" => refused(&PTR),
            "_NSShadowAttributeName" => refused(&PTR),
            "_NSSharingServiceNameAddToSafariReadingList" => refused(&PTR),
            "_NSSharingServiceNameComposeEmail" => refused(&PTR),
            "_NSStrikethroughStyleAttributeName" => refused(&PTR),
            "_NSTextInputReplacementRangeAttributeName" => refused(&PTR),
            "_NSTouchBarItemIdentifierCandidateList" => refused(&PTR),
            "_NSTouchBarItemIdentifierCharacterPicker" => refused(&PTR),
            "_NSTouchBarItemIdentifierFlexibleSpace" => refused(&PTR),
            "_NSWindowDidChangeOcclusionStateNotification" => refused(&PTR),
            "_NSWindowDidChangeScreenNotification" => refused(&PTR),
            "_NSWindowWillMoveNotification" => refused(&PTR),
            "_NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification" => refused(&PTR),
            "_NSWorkspaceActiveSpaceDidChangeNotification" => refused(&PTR),
            "_NSWorkspaceDidActivateApplicationNotification" => refused(&PTR),
            "_NSWorkspaceWillPowerOffNotification" => refused(&PTR),
            "_NSWritingDirectionAttributeName" => refused(&PTR),

        // ---- AVFoundation: 10 name(s), bound at 10 site(s) in __DATA_CONST,__got
            "_AVCaptureDeviceTypeBuiltInWideAngleCamera" => refused(&PTR),
            "_AVCaptureDeviceTypeExternalUnknown" => refused(&PTR),
            "_AVLayerVideoGravityResize" => refused(&PTR),
            "_AVMediaTypeMuxed" => refused(&PTR),
            "_AVSpeechUtteranceDefaultSpeechRate" => refused(&F32),
            "_AVSpeechUtteranceMaximumSpeechRate" => refused(&F32),
            "_AVVideoCodecKey" => refused(&PTR),
            "_AVVideoCodecTypeJPEG" => refused(&PTR),
            "_AVVideoScalingModeKey" => refused(&PTR),
            "_AVVideoScalingModeResizeAspectFill" => refused(&PTR),

        // ---- AuthenticationServices: 3 name(s), bound at 4 site(s) in __DATA,__objc_data and __DATA_CONST,__got
            "_ASAuthorizationErrorDomain" => refused(&PTR),
            "_ASWebAuthenticationSessionErrorDomain" => refused(&PTR),
            "_OBJC_METACLASS_$_ASAuthorizationController" => refused(&METACLASS),

        // ---- CFNetwork: 7 name(s), bound at 7 site(s) in __DATA_CONST,__got
            "_kCFProxyHostNameKey" => refused(&PTR),
            "_kCFProxyPortNumberKey" => refused(&PTR),
            "_kCFProxyTypeHTTP" => refused(&PTR),
            "_kCFProxyTypeHTTPS" => refused(&PTR),
            "_kCFProxyTypeKey" => refused(&PTR),
            "_kCFProxyTypeNone" => refused(&PTR),
            "_kCFProxyTypeSOCKS" => refused(&PTR),

        // ---- CoreBluetooth: 7 name(s), bound at 7 site(s) in __DATA_CONST,__got
            "_CBAdvertisementDataLocalNameKey" => refused(&PTR),
            "_CBAdvertisementDataManufacturerDataKey" => refused(&PTR),
            "_CBAdvertisementDataOverflowServiceUUIDsKey" => refused(&PTR),
            "_CBAdvertisementDataServiceDataKey" => refused(&PTR),
            "_CBAdvertisementDataServiceUUIDsKey" => refused(&PTR),
            "_CBAdvertisementDataTxPowerLevelKey" => refused(&PTR),
            "_CBCentralManagerScanOptionAllowDuplicatesKey" => refused(&PTR),

        // ---- CoreFoundation: 10 name(s), bound at 10 site(s) in __DATA_CONST,__got
            "_NSCalendarIdentifierGregorian" => refused(&PTR),
            "_NSSystemTimeZoneDidChangeNotification" => refused(&PTR),
            "_NSURLContentTypeKey" => refused(&PTR),
            "_NSURLEffectiveIconKey" => refused(&PTR),
            "_NSURLHasHiddenExtensionKey" => refused(&PTR),
            "_NSURLIsExcludedFromBackupKey" => refused(&PTR),
            "_NSURLTagNamesKey" => refused(&PTR),
            "_OBJC_EHTYPE_$_NSException" => refused(&EHTYPE),
            "___NSArray0__" => refused(&PTR),
            "___NSDictionary0__" => refused(&PTR),

        // ---- CoreGraphics: 19 name(s), bound at 19 site(s) in __DATA_CONST,__got
            "_CGRectNull" => refused(&RECT),
            "_kCGColorSpaceDisplayP3" => refused(&PTR),
            "_kCGColorSpaceExtendedLinearITUR_2020" => refused(&PTR),
            "_kCGColorSpaceExtendedLinearSRGB" => refused(&PTR),
            "_kCGColorSpaceExtendedSRGB" => refused(&PTR),
            "_kCGColorSpaceITUR_2100_HLG" => refused(&PTR),
            "_kCGColorSpaceITUR_2100_PQ" => refused(&PTR),
            "_kCGDisplayStreamColorSpace" => refused(&PTR),
            "_kCGDisplayStreamDestinationRect" => refused(&PTR),
            "_kCGDisplayStreamMinimumFrameTime" => refused(&PTR),
            "_kCGDisplayStreamPreserveAspectRatio" => refused(&PTR),
            "_kCGDisplayStreamShowCursor" => refused(&PTR),
            "_kCGWindowBounds" => refused(&PTR),
            "_kCGWindowIsOnscreen" => refused(&PTR),
            "_kCGWindowLayer" => refused(&PTR),
            "_kCGWindowName" => refused(&PTR),
            "_kCGWindowNumber" => refused(&PTR),
            "_kCGWindowOwnerName" => refused(&PTR),
            "_kCGWindowOwnerPID" => refused(&PTR),

        // ---- CoreImage: 5 name(s), bound at 5 site(s) in __DATA_CONST,__got
            "_CIDetectorAccuracy" => refused(&PTR),
            "_CIDetectorAccuracyHigh" => refused(&PTR),
            "_CIDetectorAccuracyLow" => refused(&PTR),
            "_CIDetectorTypeFace" => refused(&PTR),
            "_CIDetectorTypeText" => refused(&PTR),

        // ---- CoreLocation: 2 name(s), bound at 2 site(s) in __DATA_CONST,__got
            "_kCLLocationAccuracyBest" => refused(&PTR),
            "_kCLLocationAccuracyHundredMeters" => refused(&F64),

        // ---- CoreMIDI: 4 name(s), bound at 4 site(s) in __DATA_CONST,__got
            "_kMIDIPropertyDisplayName" => refused(&PTR),
            "_kMIDIPropertyDriverVersion" => refused(&PTR),
            "_kMIDIPropertyManufacturer" => refused(&PTR),
            "_kMIDIPropertyUniqueID" => refused(&PTR),

        // ---- CoreMedia: 20 name(s), bound at 20 site(s) in __DATA_CONST,__got
            "_kCMFormatDescriptionColorPrimaries_DCI_P3" => refused(&PTR),
            "_kCMFormatDescriptionColorPrimaries_ITU_R_2020" => refused(&PTR),
            "_kCMFormatDescriptionColorPrimaries_P3_D65" => refused(&PTR),
            "_kCMFormatDescriptionExtension_ContentLightLevelInfo" => refused(&PTR),
            "_kCMFormatDescriptionExtension_Depth" => refused(&PTR),
            "_kCMFormatDescriptionExtension_FormatName" => refused(&PTR),
            "_kCMFormatDescriptionExtension_FullRangeVideo" => refused(&PTR),
            "_kCMFormatDescriptionExtension_MasteringDisplayColorVolume" => refused(&PTR),
            "_kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms" => refused(&PTR),
            "_kCMFormatDescriptionTransferFunction_ITU_R_2020" => refused(&PTR),
            "_kCMFormatDescriptionTransferFunction_ITU_R_2100_HLG" => refused(&PTR),
            "_kCMFormatDescriptionTransferFunction_Linear" => refused(&PTR),
            "_kCMFormatDescriptionTransferFunction_SMPTE_ST_2084_PQ" => refused(&PTR),
            "_kCMFormatDescriptionTransferFunction_SMPTE_ST_428_1" => refused(&PTR),
            "_kCMFormatDescriptionTransferFunction_sRGB" => refused(&PTR),
            "_kCMFormatDescriptionYCbCrMatrix_ITU_R_2020" => refused(&PTR),
            "_kCMSampleAttachmentKey_DisplayImmediately" => refused(&PTR),
            "_kCMSampleAttachmentKey_IsDependedOnByOthers" => refused(&PTR),
            "_kCMSampleAttachmentKey_NotSync" => refused(&PTR),
            "_kCMTimeInvalid" => refused(&RECT),

        // ---- CoreServices: 6 name(s), bound at 6 site(s) in __DATA_CONST,__got
            "__kLSOpenOptionBackgroundLaunchKey" => refused(&PTR),
            "_kLSSharedFileListItemLast" => refused(&PTR),
            "_kLSSharedFileListLoginItemHidden" => refused(&PTR),
            "_kLSSharedFileListSessionLoginItems" => refused(&PTR),
            "_kMDItemWhereFroms" => refused(&PTR),
            "_kUTTypeHTML" => refused(&PTR),

        // ---- CoreText: 12 name(s), bound at 12 site(s) in __DATA_CONST,__got
            "_kCTFontCollectionRemoveDuplicatesOption" => refused(&PTR),
            "_kCTFontDisplayNameAttribute" => refused(&PTR),
            "_kCTFontFamilyNameKey" => refused(&PTR),
            "_kCTFontFormatAttribute" => refused(&PTR),
            "_kCTFontManagerRegisteredFontsChangedNotification" => refused(&PTR),
            "_kCTFontVariationAttribute" => refused(&PTR),
            "_kCTFontVariationAxesAttribute" => refused(&PTR),
            "_kCTFontVariationAxisDefaultValueKey" => refused(&PTR),
            "_kCTFontVariationAxisIdentifierKey" => refused(&PTR),
            "_kCTFontVariationAxisMaximumValueKey" => refused(&PTR),
            "_kCTFontVariationAxisMinimumValueKey" => refused(&PTR),
            "_kCTFontWidthTrait" => refused(&PTR),

        // ---- CoreVideo: 30 name(s), bound at 30 site(s) in __DATA_CONST,__got
            "_kCVImageBufferColorPrimariesKey" => refused(&PTR),
            "_kCVImageBufferColorPrimaries_DCI_P3" => refused(&PTR),
            "_kCVImageBufferColorPrimaries_EBU_3213" => refused(&PTR),
            "_kCVImageBufferColorPrimaries_ITU_R_2020" => refused(&PTR),
            "_kCVImageBufferColorPrimaries_ITU_R_709_2" => refused(&PTR),
            "_kCVImageBufferColorPrimaries_P3_D65" => refused(&PTR),
            "_kCVImageBufferColorPrimaries_SMPTE_C" => refused(&PTR),
            "_kCVImageBufferContentLightLevelInfoKey" => refused(&PTR),
            "_kCVImageBufferGammaLevelKey" => refused(&PTR),
            "_kCVImageBufferMasteringDisplayColorVolumeKey" => refused(&PTR),
            "_kCVImageBufferTransferFunctionKey" => refused(&PTR),
            "_kCVImageBufferTransferFunction_ITU_R_2020" => refused(&PTR),
            "_kCVImageBufferTransferFunction_ITU_R_2100_HLG" => refused(&PTR),
            "_kCVImageBufferTransferFunction_ITU_R_709_2" => refused(&PTR),
            "_kCVImageBufferTransferFunction_Linear" => refused(&PTR),
            "_kCVImageBufferTransferFunction_SMPTE_240M_1995" => refused(&PTR),
            "_kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ" => refused(&PTR),
            "_kCVImageBufferTransferFunction_SMPTE_ST_428_1" => refused(&PTR),
            "_kCVImageBufferTransferFunction_UseGamma" => refused(&PTR),
            "_kCVImageBufferTransferFunction_sRGB" => refused(&PTR),
            "_kCVImageBufferYCbCrMatrixKey" => refused(&PTR),
            "_kCVImageBufferYCbCrMatrix_ITU_R_2020" => refused(&PTR),
            "_kCVImageBufferYCbCrMatrix_ITU_R_601_4" => refused(&PTR),
            "_kCVImageBufferYCbCrMatrix_ITU_R_709_2" => refused(&PTR),
            "_kCVImageBufferYCbCrMatrix_SMPTE_240M_1995" => refused(&PTR),
            "_kCVPixelBufferHeightKey" => refused(&PTR),
            "_kCVPixelBufferIOSurfacePropertiesKey" => refused(&PTR),
            "_kCVPixelBufferPixelFormatTypeKey" => refused(&PTR),
            "_kCVPixelBufferPoolAllocationThresholdKey" => refused(&PTR),
            "_kCVPixelBufferWidthKey" => refused(&PTR),

        // ---- DiskArbitration: 10 name(s), bound at 10 site(s) in __DATA_CONST,__got
            "_kDADiskDescriptionDeviceRevisionKey" => refused(&PTR),
            "_kDADiskDescriptionDeviceVendorKey" => refused(&PTR),
            "_kDADiskDescriptionMatchVolumeMountable" => refused(&PTR),
            "_kDADiskDescriptionMediaBSDNameKey" => refused(&PTR),
            "_kDADiskDescriptionMediaRemovableKey" => refused(&PTR),
            "_kDADiskDescriptionMediaSizeKey" => refused(&PTR),
            "_kDADiskDescriptionVolumeNameKey" => refused(&PTR),
            "_kDADiskDescriptionVolumePathKey" => refused(&PTR),
            "_kDADiskDescriptionVolumeUUIDKey" => refused(&PTR),
            "_kDADiskDescriptionWatchVolumePath" => refused(&PTR),

        // ---- Foundation: 8 name(s), bound at 8 site(s) in __DATA_CONST,__got
            "_NSCocoaErrorDomain" => refused(&PTR),
            "_NSDebugDescriptionErrorKey" => refused(&PTR),
            "_NSKeyedArchiveRootObjectKey" => refused(&PTR),
            "_NSOSStatusErrorDomain" => refused(&PTR),
            "_NSProgressFileOperationKindDownloading" => refused(&PTR),
            "_NSProgressKindFile" => refused(&PTR),
            "_NSURLSessionTaskPriorityHigh" => refused(&F32),
            "_NSUserActivityTypeBrowsingWeb" => refused(&PTR),

        // ---- IOSurface: 13 name(s), bound at 13 site(s) in __DATA_CONST,__got
            "_kIOSurfaceAllocSize" => refused(&PTR),
            "_kIOSurfaceBytesPerElement" => refused(&PTR),
            "_kIOSurfaceBytesPerRow" => refused(&PTR),
            "_kIOSurfaceHeight" => refused(&PTR),
            "_kIOSurfacePixelFormat" => refused(&PTR),
            "_kIOSurfacePlaneBytesPerElement" => refused(&PTR),
            "_kIOSurfacePlaneBytesPerRow" => refused(&PTR),
            "_kIOSurfacePlaneHeight" => refused(&PTR),
            "_kIOSurfacePlaneInfo" => refused(&PTR),
            "_kIOSurfacePlaneOffset" => refused(&PTR),
            "_kIOSurfacePlaneSize" => refused(&PTR),
            "_kIOSurfacePlaneWidth" => refused(&PTR),
            "_kIOSurfaceWidth" => refused(&PTR),

        // ---- ImageCaptureCore: 5 name(s), bound at 5 site(s) in __DATA_CONST,__got
            "_ICDownloadsDirectoryURL" => refused(&PTR),
            "_ICOverwrite" => refused(&PTR),
            "_ICSaveAsFilename" => refused(&PTR),
            "_ICSavedFilename" => refused(&PTR),
            "_ICTransportTypeMassStorage" => refused(&PTR),

        // ---- IOBluetooth: 1 name(s), bound at 1 site(s) in __DATA_CONST,__got
            "_IOBluetoothPreferenceSetControllerPowerState" => refused(&PTR),

        // ---- MediaAccessibility: 1 name(s), bound at 1 site(s) in __DATA_CONST,__got
            "_kMACaptionAppearanceSettingsChangedNotification" => refused(&PTR),

        // ---- MediaPlayer: 8 name(s), bound at 8 site(s) in __DATA_CONST,__got
            "_MPMediaItemPropertyAlbumTitle" => refused(&PTR),
            "_MPMediaItemPropertyArtist" => refused(&PTR),
            "_MPMediaItemPropertyArtwork" => refused(&PTR),
            "_MPMediaItemPropertyPlaybackDuration" => refused(&PTR),
            "_MPMediaItemPropertyTitle" => refused(&PTR),
            "_MPNowPlayingInfoPropertyCurrentPlaybackDate" => refused(&PTR),
            "_MPNowPlayingInfoPropertyElapsedPlaybackTime" => refused(&PTR),
            "_MPNowPlayingInfoPropertyPlaybackRate" => refused(&PTR),

        // ---- Metal: 4 name(s), bound at 4 site(s) in __DATA_CONST,__got
            "_MTLCommonCounterSetTimestamp" => refused(&PTR),
            "_MTLCommonCounterTimestamp" => refused(&PTR),
            "_MTLDeviceRemovalRequestedNotification" => refused(&PTR),
            "_MTLDeviceWasRemovedNotification" => refused(&PTR),

        // ---- OpenDirectory: 5 name(s), bound at 5 site(s) in __DATA_CONST,__got
            "_kODAttributeTypeAllAttributes" => refused(&PTR),
            "_kODAttributeTypeAltSecurityIdentities" => refused(&PTR),
            "_kODAttributeTypeMetaRecordName" => refused(&PTR),
            "_kODAttributeTypeRecordName" => refused(&PTR),
            "_kODRecordTypeUsers" => refused(&PTR),

        // ---- QuartzCore: 5 name(s), bound at 5 site(s) in __DATA_CONST,__got
            "_CATransform3DIdentity" => refused(&AFFINE),
            "_kCAFilterLinear" => refused(&PTR),
            "_kCAFilterNearest" => refused(&PTR),
            "_kCAGravityTopLeft" => refused(&PTR),
            "_kCATransactionDisableActions" => refused(&PTR),

        // ---- Security: 51 name(s), bound at 51 site(s) in __DATA_CONST,__got
            "_kSecAttrAccessControl" => refused(&PTR),
            "_kSecAttrAccessGroup" => refused(&PTR),
            "_kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly" => refused(&PTR),
            "_kSecAttrAccessibleWhenUnlockedThisDeviceOnly" => refused(&PTR),
            "_kSecAttrApplicationLabel" => refused(&PTR),
            "_kSecAttrApplicationTag" => refused(&PTR),
            "_kSecAttrCanSign" => refused(&PTR),
            "_kSecAttrCreationDate" => refused(&PTR),
            "_kSecAttrIsPermanent" => refused(&PTR),
            "_kSecAttrKeySizeInBits" => refused(&PTR),
            "_kSecAttrKeyType" => refused(&PTR),
            "_kSecAttrKeyTypeECSECPrimeRandom" => refused(&PTR),
            "_kSecAttrSynchronizable" => refused(&PTR),
            "_kSecAttrTokenID" => refused(&PTR),
            "_kSecAttrTokenIDSecureEnclave" => refused(&PTR),
            "_kSecAttrType" => refused(&PTR),
            "_kSecClassCertificate" => refused(&PTR),
            "_kSecClassIdentity" => refused(&PTR),
            "_kSecClassKey" => refused(&PTR),
            "_kSecCodeInfoCertificates" => refused(&PTR),
            "_kSecCodeInfoFlags" => refused(&PTR),
            "_kSecCodeInfoPList" => refused(&PTR),
            "_kSecCodeInfoUnique" => refused(&PTR),
            "_kSecCodeSignerEntitlements" => refused(&PTR),
            "_kSecCodeSignerFlags" => refused(&PTR),
            "_kSecCodeSignerIdentity" => refused(&PTR),
            "_kSecGuestAttributeAudit" => refused(&PTR),
            "_kSecGuestAttributeDynamicCode" => refused(&PTR),
            "_kSecGuestAttributeDynamicCodeInfoPlist" => refused(&PTR),
            "_kSecGuestAttributePid" => refused(&PTR),
            "_kSecKeyAlgorithmECDSASignatureDigestX962SHA1" => refused(&PTR),
            "_kSecKeyAlgorithmECDSASignatureDigestX962SHA256" => refused(&PTR),
            "_kSecKeyAlgorithmECDSASignatureDigestX962SHA384" => refused(&PTR),
            "_kSecKeyAlgorithmECDSASignatureDigestX962SHA512" => refused(&PTR),
            "_kSecKeyAlgorithmECDSASignatureMessageX962SHA256" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPKCS1v15SHA1" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPKCS1v15SHA256" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPKCS1v15SHA384" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPKCS1v15SHA512" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPSSSHA256" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPSSSHA384" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureDigestPSSSHA512" => refused(&PTR),
            "_kSecKeyAlgorithmRSASignatureRaw" => refused(&PTR),
            "_kSecMatchLimitAll" => refused(&PTR),
            "_kSecPolicyAppleSSL" => refused(&PTR),
            "_kSecPolicyOid" => refused(&PTR),
            "_kSecPrivateKeyAttrs" => refused(&PTR),
            "_kSecReturnRef" => refused(&PTR),
            "_kSecUseAuthenticationContext" => refused(&PTR),
            "_kSecUseDataProtectionKeychain" => refused(&PTR),
            "_kSecValueRef" => refused(&PTR),

        // ---- SystemConfiguration: 17 name(s), bound at 17 site(s) in __DATA_CONST,__got
            // ---- SystemConfiguration: 0, moved to `corefoundation::sysconfig`.
            // The seventeen `_kSCPropNetProxies*` keys used to be here, as
            // zeroed cells. They are answered now - see the module note's
            // section on the proxy dictionary - and the reason they are *gone*
            // rather than still listed as refused is mechanical and
            // load-bearing: `darwin::symbol` is `implemented().or_else(..)`
            // with `implemented` being a `find_map` over `MODULES`, so **a name
            // in both places resolves to `darwin`'s**, and the real value
            // `corefoundation` publishes would never be reached. A row left
            // behind is not a harmless duplicate; it is the one that wins.

        // ---- UniformTypeIdentifiers: 10 name(s), bound at 10 site(s) in __DATA_CONST,__got
            "_UTTagClassFilenameExtension" => refused(&PTR),
            "_UTTagClassMIMEType" => refused(&PTR),
            "_UTTypeContent" => refused(&PTR),
            "_UTTypeFolder" => refused(&PTR),
            "_UTTypeHTML" => refused(&PTR),
            "_UTTypeImage" => refused(&PTR),
            "_UTTypeInternetLocation" => refused(&PTR),
            "_UTTypeInternetShortcut" => refused(&PTR),
            "_UTTypePNG" => refused(&PTR),
            "_UTTypeSVG" => refused(&PTR),

        // ---- UserNotifications: 3 name(s), bound at 3 site(s) in __DATA_CONST,__got
            "_UNNotificationAttachmentOptionsTypeHintKey" => refused(&PTR),
            "_UNNotificationDefaultActionIdentifier" => refused(&PTR),
            "_UNNotificationDismissActionIdentifier" => refused(&PTR),

        // ---- VideoToolbox: 21 name(s), bound at 21 site(s) in __DATA_CONST,__got
            "_kVTCompressionPropertyKey_AllowFrameReordering" => refused(&PTR),
            "_kVTCompressionPropertyKey_AverageBitRate" => refused(&PTR),
            "_kVTCompressionPropertyKey_ColorPrimaries" => refused(&PTR),
            "_kVTCompressionPropertyKey_ExpectedFrameRate" => refused(&PTR),
            "_kVTCompressionPropertyKey_MaxFrameDelayCount" => refused(&PTR),
            "_kVTCompressionPropertyKey_MaxKeyFrameInterval" => refused(&PTR),
            "_kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration" => refused(&PTR),
            "_kVTCompressionPropertyKey_ProfileLevel" => refused(&PTR),
            "_kVTCompressionPropertyKey_RealTime" => refused(&PTR),
            "_kVTCompressionPropertyKey_TransferFunction" => refused(&PTR),
            "_kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder" => refused(&PTR),
            "_kVTCompressionPropertyKey_YCbCrMatrix" => refused(&PTR),
            "_kVTEncodeFrameOptionKey_ForceKeyFrame" => refused(&PTR),
            "_kVTProfileLevel_H264_Baseline_AutoLevel" => refused(&PTR),
            "_kVTProfileLevel_H264_High_AutoLevel" => refused(&PTR),
            "_kVTProfileLevel_H264_Main_AutoLevel" => refused(&PTR),
            "_kVTProfileLevel_HEVC_Main_AutoLevel" => refused(&PTR),
            "_kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder" => refused(&PTR),
            "_kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder" => refused(&PTR),
            "_kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder" => refused(&PTR),
            "_kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder" => refused(&PTR),

        // ---- Vision: 17 name(s), bound at 17 site(s) in __DATA_CONST,__got
            "_VNBarcodeSymbologyAztec" => refused(&PTR),
            "_VNBarcodeSymbologyCode128" => refused(&PTR),
            "_VNBarcodeSymbologyCode39" => refused(&PTR),
            "_VNBarcodeSymbologyCode39Checksum" => refused(&PTR),
            "_VNBarcodeSymbologyCode39FullASCII" => refused(&PTR),
            "_VNBarcodeSymbologyCode39FullASCIIChecksum" => refused(&PTR),
            "_VNBarcodeSymbologyCode93" => refused(&PTR),
            "_VNBarcodeSymbologyCode93i" => refused(&PTR),
            "_VNBarcodeSymbologyDataMatrix" => refused(&PTR),
            "_VNBarcodeSymbologyEAN13" => refused(&PTR),
            "_VNBarcodeSymbologyEAN8" => refused(&PTR),
            "_VNBarcodeSymbologyI2of5" => refused(&PTR),
            "_VNBarcodeSymbologyI2of5Checksum" => refused(&PTR),
            "_VNBarcodeSymbologyITF14" => refused(&PTR),
            "_VNBarcodeSymbologyPDF417" => refused(&PTR),
            "_VNBarcodeSymbologyQR" => refused(&PTR),
            "_VNBarcodeSymbologyUPCE" => refused(&PTR),

        // ---- libSystem.B.dylib: 2 name(s), bound at 2 site(s) in __DATA_CONST,__got
            "_APP_SANDBOX_READ" => refused(&PTR),
            "_APP_SANDBOX_READ_WRITE" => refused(&PTR),

        // ---- libobjc.A.dylib: 1 name(s), bound at 1 site(s) in __DATA_CONST,__got
        //      (`___objc_personality_v0` was the second, and is a `functions` row now)
            "_OBJC_EHTYPE_id" => refused(&EHTYPE),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use machish::fixups::{LibraryRef, SymbolRef};
        use machish::resolve::SyntheticDylib;
        use machish::resolve::{SymbolKind, SymbolProvider};

        /// The guest image, as a path relative to this crate. It is 214 MB and it
        /// is **not** in the repository (`test/` is gitignored), so every test
        /// that wants it skips when it is absent rather than failing a build
        /// that has no Chromium in it.
        const IMAGE: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test/Chromium/Chromium.app/Contents/Frameworks/",
            "Chromium Framework.framework/Versions/130.0.6723.91/",
            "Chromium Framework",
        );

        /// The **census this population has never had**: for every published
        /// name, the slot the loader gives it.
        ///
        /// Everything else here checks that a name *resolves*. That is not the
        /// question. "Bound" and "answered" are different - the exception-handling
        /// census exists for exactly that reason, and it prints a `->` column
        /// for it. This test is the data-import equivalent: it links the real
        /// image with the real loader and reports, per name, the image-relative
        /// address of the slot whose contents this module's rows replace.
        ///
        /// It is what turns "we publish 521 names" into "we publish 521 names,
        /// at these 575 addresses, and the guest reads N of them", and it is
        /// the only source for that address that is not a hand-written parser.
        ///
        /// `ROSETTA3_DATA_CENSUS=<path>` writes the whole table as TSV and
        /// prints a summary; unset, it asserts only. A test with a side effect
        /// to a path is a smell, so the side effect is behind a name the
        /// environment has to ask for by name.
        #[test]
        fn the_census_reports_a_slot_for_every_published_name() {
            if !std::path::Path::new(IMAGE).exists() {
                eprintln!("skipping: no Chromium image at {IMAGE}");
                return;
            }
            let bytes = std::fs::read(IMAGE).expect("the image reads");
            let mut memory = machish::memory::BufferSpace::new(0x1_0000_0000);
            let report = machish::loader::Loader::new(machish::spec::arch::X86_64)
                .link(&bytes, IMAGE, &mut memory, &[])
                .expect("the image links with no providers at all");
            let image = report
                .images
                .iter()
                .find(|i| i.name.contains("Chromium Framework"))
                .expect("the report has the Framework in it");
            // `slot_va` is slid; the image-relative address is what a static scan
            // of the file can be asked about, and it is the address the
            // disassembly's RIP-relative displacements resolve to.
            // `bindings` holds only what some provider answered and `missing`
            // only what none did, so a census that reads one of the two sees
            // half the image. That split is this tree's own, and it is a trap:
            // an import nothing answers is not absent from the image, it is in
            // the *other* list.
            let mut slots: std::collections::BTreeMap<&str, Vec<u64>> =
                std::collections::BTreeMap::new();
            for (name, at) in image
                .bindings
                .iter()
                .map(|b| (b.symbol.as_str(), b.slot_va))
                .chain(image.missing.iter().map(|m| (m.symbol.as_str(), m.slot_va)))
            {
                slots.entry(name).or_default().push(at.wrapping_sub(image.base));
            }
            let published: std::collections::BTreeSet<String> = {
                let mut sink = Collector::default();
                provide(&mut sink);
                sink.data.into_iter().map(|(n, _)| n).collect()
            };
            let missing: Vec<&str> = published
                .iter()
                .map(String::as_str)
                .filter(|n| !slots.contains_key(*n))
                .collect();
            let site_count: usize = published
                .iter()
                .map(|n| slots.get(n.as_str()).map_or(0, Vec::len))
                .sum();
            eprintln!(
                "data census: image {} base {:#x}; {} image(s); the {} published \
                 names occupy {site_count} slot(s); {missing:?} not found",
                image.name,
                image.base,
                report.images.len(),
                published.len(),
            );
            assert!(
                missing.is_empty(),
                "the loader does not bind {} of the published names: {missing:?}",
                missing.len(),
            );
            // The count is *derived*, not asserted against a literal anybody has
            // to remember: `Collector` sees exactly the `data` rows the
            // `exports!` block below declares, and the loader sees exactly the
            // slots the image binds.  Those two numbers agreeing is the only
            // reason the one-shot line can be believed, and it is what the old
            // hard-coded `519` was not.
            assert_eq!(
                published.len(),
                super::published(),
                "the table has {} row(s) and PUBLISHED says {}; the announced count \
                 is a literal, so it drifts the moment a row is added",
                published.len(),
                super::published(),
            );
            // **575, then 558**: the seventeen keys that left for
            // `corefoundation::sysconfig` occupied seventeen slots, and the
            // loader's own bind report is what says so - not an arithmetic
            // guess from the row count, which is the thing that went wrong
            // twice already on this file. Every one of the seventeen is
            // single-slot (`__DATA_CONST,__got`, one binding each), so a
            // seventeen-row removal *must* be a seventeen-slot removal; if a
            // key with two sites had left, this assertion would catch the
            // difference.
            assert_eq!(
                site_count, 558,
                "the published names occupy {site_count} slot(s), not 558"
            );
            if let Some(path) = std::env::var_os("ROSETTA3_DATA_CENSUS") {
                use std::io::Write as _;
                let mut out = std::io::BufWriter::new(
                    std::fs::File::create(&path).expect("the census file is creatable"),
                );
                // The one function row too: it is the only name here that a
                // guest can *arrive* at rather than read, and it is the one the
                // `__TEXT,__unwind_info` word names, so a scan that only looks
                // for a `disp32` misses it entirely.
                for (name, at) in image
                    .bindings
                    .iter()
                    .filter(|b| b.symbol == "___objc_personality_v0")
                    .map(|b| (b.symbol.as_str(), b.slot_va))
                    .chain(
                        image
                            .missing
                            .iter()
                            .filter(|m| m.symbol == "___objc_personality_v0")
                            .map(|m| (m.symbol.as_str(), m.slot_va)),
                    )
                {
                    writeln!(out, "{:x}\t{name}", at.wrapping_sub(image.base))
                        .expect("the census file writes");
                }
                // Every other import the image has, published or not.  A census
                // of a *subset* is how a population's size stays a guess: the
                // 521 here are the ones this module publishes, and the image
                // binds 972 names over 2835 slots, so any question about "the
                // image" rather than "this file" needs the other 451.  The
                // consumer filters by name, so writing all of them costs
                // nothing and answers a question this file cannot.
                let mut every: std::collections::BTreeSet<&str> =
                    std::collections::BTreeSet::new();
                for name in slots.keys() {
                    every.insert(name);
                }
                let names = every.len();
                for name in every {
                    for at in &slots[name] {
                        writeln!(out, "{at:x}\t{name}").expect("the census file writes");
                    }
                }
                eprintln!("data census: wrote {} ({} name(s))",
                          path.to_string_lossy(),
                          names);
            }
        }

        fn resolve(name: &str) -> machish::resolve::Resolved {
            let mut lib = SyntheticDylib::new("<test>");
            provide(&mut lib);
            lib.resolve(&SymbolRef {
                name,
                library: LibraryRef::Flat,
                weak_import: false,
            })
            .unwrap_or_else(|| panic!("{name} resolves"))
        }

        /// The count the one-shot line prints and the census is read against.
        #[test]
        fn the_published_count_is_the_one_the_line_says() {
            // 521, then 504: the seventeen `_kSCPropNetProxies*` keys are
            // answered by `corefoundation::sysconfig` and are published by
            // neither this constant nor this table, and
            // `the_proxy_configuration_keys_split_between_answered_and_refused`
            // is the test that keeps that true.
            assert_eq!(published(), 504);
            assert_eq!(FUNCTIONS, 1);
            // ... and it is not a number anyone can forget to update: the
            // representative names below are a sample of the 521, and
            // `the_documented_families_are_all_published` covers the seven cells.
            for name in [
                // `_kSCPropNetProxiesHTTPEnable` used to be in this list, as a
                // representative of the zeroed cells. It is not any more: it is
                // answered by `corefoundation::sysconfig`, and naming it here
                // would assert that this module publishes it.
                "_kCFProxyTypeHTTP",
                "_kCFProxyTypeNone",
                "_kCMTimeInvalid",
                "_CGRectNull",
                "_CATransform3DIdentity",
                "_AVSpeechUtteranceDefaultSpeechRate",
                "_kCLLocationAccuracyHundredMeters",
                "_NSURLSessionTaskPriorityHigh",
                "_OBJC_METACLASS_$_ASAuthorizationController",
                "_OBJC_EHTYPE_$_NSException",
                "_OBJC_EHTYPE_id",
            ] {
                assert!(resolve(name).kind == SymbolKind::Data, "{name}");
            }
            // The 520th name is a function, and is deliberately not in that list.
            assert!(
                resolve("___objc_personality_v0").kind == SymbolKind::Function,
                "the personality is published as data again"
            );
        }

        /// **Every** published **data** name is at an address in this module, and
        /// the eight bytes there are zero.
        ///
        /// The second half is the load-bearing one: a refusal whose bytes were not
        /// zero would be a value, and this file's claim is that it has none. It
        /// is checked through the *published* address rather than the cell, so it
        /// is the address a guest would hold that is proved zero.
        #[test]
        fn every_published_name_is_a_zeroed_cell() {
            let mut lib = SyntheticDylib::new("<test>");
            let mut sink = Collector::default();
            provide(&mut lib);
            provide(&mut sink);
            assert!(!sink.data.is_empty(), "nothing was published");
            for (name, address) in &sink.data {
                assert_eq!(
                    which_cell(*address),
                    Some(match name.as_str() {
                        "_kCMTimeInvalid" | "_CGRectNull" => Cell::Rect,
                        "_CATransform3DIdentity" => Cell::Affine,
                        "_AVSpeechUtteranceDefaultSpeechRate"
                        | "_AVSpeechUtteranceMaximumSpeechRate"
                        | "_NSURLSessionTaskPriorityHigh" => Cell::F32,
                        "_kCLLocationAccuracyHundredMeters" => Cell::F64,
                        "_OBJC_METACLASS_$_ASAuthorizationController" => Cell::Metaclass,
                        "_OBJC_EHTYPE_$_NSException" | "_OBJC_EHTYPE_id" => Cell::Ehtype,
                        _ => Cell::Ptr,
                    }),
                    "{name} at {address:#x} is not one of this module's cells"
                );
                // SAFETY: `which_cell` only answers for an address it got from
                // `cell_address`, and a cell is a live `#[repr(C)]` of eight
                // bytes that outlives the process.
                // `address` is a `&u64` here, so it is the *element* that has to
                // be dereferenced before it is a place to read: casting the
                // reference straight to a pointer would read the `Vec` slot,
                // which holds the cell's address and is never zero - a test that
                // passes or fails for the wrong reason, and quietly.
                let word = unsafe { std::ptr::read_volatile(*address as *const u64) };
                assert_eq!(word, 0, "{name} at {address:#x} is not zero");
            }
        }

        /// The two names carrying a `$`, pinned for the *other* spelling trap.
        ///
        /// `base_name` folds a `$` suffix - deliberately, because a `$` marks a
        /// platform variant of one function - and `SyntheticDylib::resolve` folds
        /// a lookup the same way. So `_OBJC_METACLASS_$_ASAuthorizationController`
        /// reduces to `_OBJC_METACLASS_`, and a row written as the folded base
        /// would answer *every* `$` name of that family with one cell. The row
        /// must therefore carry the whole name, verbatim, which is what the
        /// census at `census.log:707` says the image asks for - one leading
        /// underscore, not two, which is the guess this test exists to catch.
        #[test]
        fn dollar_names_survive_the_base_name_fold() {
            for (image, folded) in [
                ("_OBJC_EHTYPE_$_NSException", "OBJC_EHTYPE_"),
                (
                    "_OBJC_METACLASS_$_ASAuthorizationController",
                    "OBJC_METACLASS_",
                ),
            ] {
                assert_eq!(crate::base_name(image), folded, "{image}");
                let mut lib = SyntheticDylib::new("<test>");
                provide(&mut lib);
                assert!(
                    lib.resolve(&SymbolRef {
                        name: image,
                        library: LibraryRef::Flat,
                        weak_import: false,
                    })
                    .is_some(),
                    "{image} is not published under the image's own spelling"
                );
            }
        }

        /// The 130 class references are **not** here, because
        /// `ROSETTA3_NAME_MISSING_CLASSES` gives each one an empty class that
        /// aborts by name and a zero would take that away.
        ///
        /// This is the test that fails if someone helpfully adds them.
        #[test]
        fn no_class_reference_is_published_here() {
            for name in [
                "_OBJC_CLASS_$_NSTask",
                "_OBJC_CLASS_$_AVCaptureSession",
                "_OBJC_CLASS_$_NSToolbar",
                "_OBJC_CLASS_$_ASAuthorizationController",
                "_OBJC_CLASS_$_ODNode",
                "_OBJC_CLASS_$_MTLBlitPassDescriptor",
            ] {
                let mut lib = SyntheticDylib::new("<test>");
                provide(&mut lib);
                assert!(
                    lib.resolve(&SymbolRef {
                        name,
                        library: LibraryRef::Flat,
                        weak_import: false,
                    })
                    .is_none(),
                    "{name} is published here; it belongs to ROSETTA3_NAME_MISSING_CLASSES"
                );
            }
            assert_eq!(CLASS_REFERENCES_LEFT, 128);
        }

        /// The four awkward spellings, pinned twice each: as the image writes them
        /// and as [`crate::base_name`] reduces them.
        ///
        /// `exports!`'s `data` section takes the Mach-O spelling verbatim and the
        /// synthetic dylib resolves an import verbatim, so a row written
        /// `"_NSArray0__"` instead of `"___NSArray0__"` compiles, passes every
        /// other test in this file, and is invisible to everything except the
        /// unbound-import census. `base_name` is checked as well because it
        /// strips exactly **one** underscore, so these four reduce to a name that
        /// still starts with `__`.
        ///
        /// The personality is in the list even though it is a `functions` row
        /// now, because the *image's* spelling is what is being pinned and it
        /// has not moved. The `functions` section's own spelling rule - bare C,
        /// with the underscore added by the macro - is
        /// [`the_personality_is_a_function_and_not_a_cell`]'s job.
        #[test]
        fn names_are_the_image_spellings() {
            for (image, reduced) in [
                ("___NSArray0__", "__NSArray0__"),
                ("___NSDictionary0__", "__NSDictionary0__"),
                ("___objc_personality_v0", "__objc_personality_v0"),
                (
                    "__kLSOpenOptionBackgroundLaunchKey",
                    "_kLSOpenOptionBackgroundLaunchKey",
                ),
            ] {
                assert_eq!(crate::base_name(image), reduced, "{image}");
                assert!(image.starts_with("__"), "{image}");
                let mut lib = SyntheticDylib::new("<test>");
                provide(&mut lib);
                assert!(
                    lib.resolve(&SymbolRef {
                        name: image,
                        library: LibraryRef::Flat,
                        weak_import: false,
                    })
                    .is_some(),
                    "{image} is not published under the image's own spelling"
                );
                // And the one-leading-underscore spelling of the same name is
                // *not* what the image uses, so it must not resolve either.
                let wrong = format!("_{}", image.trim_start_matches('_'));
                assert_ne!(wrong, image);
                assert!(
                    lib.resolve(&SymbolRef {
                        name: &wrong,
                        library: LibraryRef::Flat,
                        weak_import: false,
                    })
                    .is_none(),
                    "{wrong} resolved; that is not the image's spelling"
                );
            }
        }

        /// The personality is a **function**, and its address is not any cell.
        ///
        /// This is the test the whole change exists for, and every part of it is
        /// a way the fix could silently not be one:
        ///
        /// * **it is published at all**, under the image's own three-underscore
        ///   spelling. `exports!`'s `functions` section writes the *bare C* name
        ///   and prepends the Mach-O underscore, so the row has to be
        ///   `"__objc_personality_v0"` - two - to publish `___objc_personality_v0`
        ///   - three. Written the other way round it publishes
        ///   `____objc_personality_v0`, which resolves nothing, is reported
        ///   **unbound**, and the slot stays whatever it was.
        /// * **it is a function and not data**, because the kind is what tells
        ///   the loader this slot is a place a guest arrives at.
        /// * **its address is not a cell**, so it cannot be eight zero bytes in a
        ///   read-only page. A positive control: every one of the seven cells is
        ///   in this module's `.data` and is zero, and this address is neither
        ///   zero nor any of them.
        /// * **`darwin::symbol` answers it**, so a guest `dlsym` and a guest
        ///   import reach the same function rather than one of them reaching
        ///   nothing - the split brain `crates/darwin/tests/published_census.rs`
        ///   exists to catch, and this row is now in both of its lookups.
        #[test]
        fn the_personality_is_a_function_and_not_a_cell() {
            let mut lib = SyntheticDylib::new("<test>");
            let mut sink = Collector::default();
            provide(&mut lib);
            provide(&mut sink);

            let published = sink
                .functions
                .iter()
                .find(|(n, _)| n == "___objc_personality_v0")
                .unwrap_or_else(|| {
                    panic!(
                        "the personality is not published as a function: {:#?}",
                        sink.functions
                    )
                });
            let (_, at) = published;
            assert_ne!(*at, 0, "a zero address is the zero cell again");
            assert_eq!(which_cell(*at), None, "the personality is a cell");
            assert!(
                !sink.data.iter().any(|(n, _)| n == "___objc_personality_v0"),
                "the personality is published as data as well, so it has two answers"
            );
            assert_eq!(sink.functions.len(), FUNCTIONS);

            // The positive control: the seven cells are zero, so "not a cell"
            // above is a statement and not a tautology.
            for cell in [
                Cell::Ptr,
                Cell::F32,
                Cell::F64,
                Cell::Rect,
                Cell::Affine,
                Cell::Metaclass,
                Cell::Ehtype,
            ] {
                let cell_at = cell_address(cell);
                assert_ne!(cell_at, *at, "{cell:?} is the personality");
                // SAFETY: a `&'static Refused` of eight live bytes.
                let word = unsafe { std::ptr::read_volatile(cell_at as *const u64) };
                assert_eq!(word, 0, "{cell:?} is not zero");
            }

            // The two lookups agree, which is what `published_census.rs` checks
            // for every published function and what this row could break by
            // being in `names()` but not in `symbol()`.
            assert_eq!(
                crate::symbol("___objc_personality_v0"),
                Some(*at),
                "a guest import and a guest dlsym of the personality disagree"
            );
            // And the one-leading-underscore spelling, which the image does not
            // use, resolves to nothing either way.
            assert_eq!(crate::symbol("__objc_personality_v0"), None);
        }

        /// Each of the seven cells is its own address, and each reads as zero.
        ///
        /// Distinct addresses are what makes [`which_cell`] able to answer, and
        /// are what keeps one family's read from being reported as another's.
        #[test]
        fn the_documented_families_are_all_published() {
            let cells = [
                Cell::Ptr,
                Cell::F32,
                Cell::F64,
                Cell::Rect,
                Cell::Affine,
                Cell::Metaclass,
                Cell::Ehtype,
            ];
            let mut seen = Vec::new();
            for cell in cells {
                let at = cell_address(cell);
                assert!(!seen.contains(&at), "{cell:?} shares an address");
                seen.push(at);
                assert_eq!(which_cell(at), Some(cell));
                // SAFETY: `cell_address` only answers with a `&'static Refused`.
                let word = unsafe { std::ptr::read_volatile(at as *const u64) };
                assert_eq!(word, 0, "{cell:?} is not zero");
            }
            assert_eq!(which_cell(0x1000), None);
            assert_eq!(which_cell(0), None);
        }

        /// The twenty SystemConfiguration and CFNetwork names the proxy
        /// The twenty-one names the proxy configuration read is made of, and
        /// **which of them this module still answers for**.
        ///
        /// It used to be `the_proxy_configuration_keys_are_still_refused` and it
        /// listed all twenty-one, because all twenty-one were zeroed cells. The
        /// seventeen `_kSCPropNetProxies*` are answered now, by
        /// `corefoundation::sysconfig`, with the string values transcribed from
        /// Apple's `SCSchemaDefinitions.h`; the four `_kCFProxyType*` are still
        /// refused, and the reason they are a *different* kind of refusal is
        /// this test's subject: their values are not in any header Apple
        /// publishes, so there is nothing to transcribe.
        ///
        /// The split is asserted rather than described, because the two halves
        /// are indistinguishable from the outside: both are `SymbolKind::Data`,
        /// both are a pointer the guest dereferences, and the difference between
        /// "a real key string" and "a null" is exactly the difference a
        /// `CFDictionaryGetValue` turns into a proxy decision nobody can see.
        ///
        /// So each name is resolved and its **address** is looked up in this
        /// module's cells. A name this module publishes answers with a cell of
        /// its own; a name `corefoundation` publishes does not appear here at
        /// all, so `resolve` must not answer for it - and that is the assertion
        /// that catches a row accidentally left behind, which would win the
        /// lookup and silently restore the null.
        #[test]
        fn the_proxy_configuration_keys_split_between_answered_and_refused() {
            for name in [
                "_kSCPropNetProxiesHTTPEnable",
                "_kSCPropNetProxiesHTTPProxy",
                "_kSCPropNetProxiesHTTPPort",
                "_kSCPropNetProxiesHTTPSEnable",
                "_kSCPropNetProxiesHTTPSProxy",
                "_kSCPropNetProxiesHTTPSPort",
                "_kSCPropNetProxiesSOCKSEnable",
                "_kSCPropNetProxiesSOCKSProxy",
                "_kSCPropNetProxiesSOCKSPort",
                "_kSCPropNetProxiesFTPEnable",
                "_kSCPropNetProxiesFTPProxy",
                "_kSCPropNetProxiesFTPPort",
                "_kSCPropNetProxiesProxyAutoConfigEnable",
                "_kSCPropNetProxiesProxyAutoConfigURLString",
                "_kSCPropNetProxiesProxyAutoDiscoveryEnable",
                "_kSCPropNetProxiesExceptionsList",
                "_kSCPropNetProxiesExcludeSimpleHostnames",
            ] {
                assert!(
                    the_loader_does_not_answer_with_one_of_our_cells(name),
                    "{name} is answered by corefoundation::sysconfig; a row left here \
                     would win `darwin::symbol` and put the null back"
                );
            }
            for name in [
                "_kCFProxyTypeHTTP",
                "_kCFProxyTypeHTTPS",
                "_kCFProxyTypeSOCKS",
                "_kCFProxyTypeNone",
                "_kCFProxyTypeKey",
                "_kCFProxyHostNameKey",
                "_kCFProxyPortNumberKey",
            ] {
                assert!(
                    resolve(name).kind == SymbolKind::Data,
                    "{name} is still a refusal"
                );
                assert_eq!(
                    which_cell(resolve(name).addr),
                    Some(Cell::Ptr),
                    "{name} is one of this module's cells"
                );
            }
        }

        /// Whether *this* module publishes `name`, read from the rows rather
        /// than from [`resolve`], which would answer for a framework's row too.
        ///
        /// The **loader** is the thing that has to agree, though, and
        /// `darwin::symbol` is `implemented().or_else(host)`, where
        /// `implemented` is a `find_map` over `MODULES` - so a row here would
        /// *win* against `corefoundation`'s and put the null back. This
        /// therefore asks the loader the question the loader asks, and compares
        /// the address against this module's cells, which is the only form of
        /// the claim that cannot be satisfied by a name merely existing
        /// somewhere.
        fn the_loader_does_not_answer_with_one_of_our_cells(name: &str) -> bool {
            let Some(address) = crate::symbol(name) else {
                // Nothing answers at all. That is not what this asserts, but it
                // is not a failure either: the name is simply not published
                // here, which is the claim.
                return true;
            };
            which_cell(address).is_none()
        }

        #[derive(Default)]
        struct Collector {
            data: Vec<(String, u64)>,
            /// The one function this module publishes, and the reason
            /// [`the_personality_is_a_function_and_not_a_cell`] has a `functions`
            /// list to look at.
            functions: Vec<(String, u64)>,
        }

        impl exports::Exports for Collector {
            fn function(&mut self, name: &str, address: u64) {
                self.functions.push((name.to_string(), address));
            }

            fn data(&mut self, name: &str, address: u64) {
                self.data.push((name.to_string(), address));
            }
        }
    }
}

/// The one row [`crate::lib`] holds for this file.
///
/// The table is a list of `exports::Module` values and every one of them is
/// declared by the macro inside `imp`, so this is a re-export rather than a
/// second declaration and the two cannot disagree about which names the module
/// answers for.
#[cfg(not(target_os = "macos"))]
pub use imp::{Cell, MODULE, published, which_cell};

/// Nothing of this file's on Darwin: the host has every one of these frameworks,
/// and shadowing its own constants with a zero would be the worst of both
/// worlds. See the `exports!` at the foot of the file.
#[cfg(target_os = "macos")]
exports::exports! {}
