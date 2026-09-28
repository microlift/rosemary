//! `+[UTType typeWithMIMEType:]` is **refused**; the other three measured class
//! methods answer `nil`.
//!
//! # The census
//!
//! The classref slot is `0xe6e73c8`, and the loader's own unbound report names
//! it and where it comes from: `_OBJC_CLASS_$_UTType from
//! /System/Library/Frameworks/UniformTypeIdentifiers.framework/Versions/A/
//! UniformTypeIdentifiers`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **12** (exact - one classref slot) |
//! | `objc_msgSend`-family sends the queue's 96-byte window found | **13** |
//! | sends I could name, including hoisted selectors | **15** |
//! | loads that are followed by an `_objc_alloc*` | **0** |
//! | regions the 12 loads fall into | **4** |
//!
//! **Every one of the 12 is a *class* method.** Not one of the twelve loads is
//! followed by an allocator, so the class is never instantiated anywhere in
//! `__TEXT,__text`, and the only instance messages the guest sends to a `UTType`
//! are to the answer of one of the four class methods below. That single fact
//! decides the whole shape of this module; see "no instance ever exists".
//!
//! # The twelve loads, and which of them the branch survives
//!
//! Four regions, and the queue's `nil-checked` column is wrong about two of the
//! four for reasons that are worth writing down, because both are new ways for
//! that column to mislead.
//!
//! | region | file | loads | nil-tolerant? |
//! |---|---|--:|---|
//! | `0x3a91b90 .. 0x3a9222a` | `-[WebDragSource writableTypesForPasteboard:]` | 1 | **no** |
//! | `0x5af0cdf .. 0x5af1139` | unattributed; see "whichfile.py" | 2 | yes (1 of 2) |
//! | `0x67555c0 .. 0x6755d80` | unattributed; see "whichfile.py" | 7 | yes |
//! | `0x815f710 .. 0x8161e30` | `-[ExtensionDropdownHandler …]` | 2 | yes |
//!
//! ## 1. The one that is not nil-tolerant, in full
//!
//! ```text
//!  3a92037  mov  rdi,QWORD PTR [rbp-0x40]   ; the MIME type, a std::string
//!  3a9203b  mov  r13,QWORD PTR [rip+0xac55386]   # 0xe6e73c8  the class
//!  3a92042  call 0x5d99ad0                  ; std::string -> NSString
//!  3a9204a  call _objc_retainAutoreleasedReturnValue
//!  3a9204f  mov  rbx,rax
//!  3a92052  mov  rsi,QWORD PTR [rip+0xac570d7]   # 0xe6e9130  typeWithMIMEType:
//!  3a92059  mov  rdi,r13                     ; the class
//!  3a9205c  mov  rdx,rax                     ; the MIME type
//!  3a9205f  call objc_msgSend                ; +[UTType typeWithMIMEType:]
//!  3a92065  mov  rdi,rax
//!  3a92068  call _objc_retainAutoreleasedReturnValue
//!  3a9206d  mov  rdi,QWORD PTR [r15+0x3c0]   ; the old value of the ivar
//!  3a92074  mov  QWORD PTR [r15+0x3c0],rax   ; <-- THE ANSWER, INTO THE IVAR
//!  3a9207b  mov  r13,QWORD PTR [rip+0xa2bdfae]   # 0xdd50030  _objc_release
//!  3a92082  call r13                         ; release the old value
//!  ...
//!  3a920ab  mov  rdi,QWORD PTR [r15+0x3c0]   ; the ivar, again
//!  3a920b2  mov  rsi,QWORD PTR [rip+0xac5707f]   # 0xe6e9138  identifier
//!  3a920b9  call objc_msgSend                ; -[_fileType identifier]
//!  3a920c7  mov  rbx,rax
//!  3a920ca  mov  rdi,r14                     ; the NSMutableArray
//!  3a920cd  mov  rsi,r12                     ; addObject:
//!  3a920d0  mov  rdx,rax
//!  3a920d3  call objc_msgSend                ; -[writableTypes addObject:<identifier>]
//! ```
//!
//! `+0x3a92074` is the load-bearing instruction of this file: the answer goes
//! into the ivar and there is **no branch on it** between the send and the
//! `-identifier` message. The only `test` between them (`test rax,rax` at
//! `+0x3a92093`) is two instructions after a `movsx rax,BYTE PTR [r15+0x1f7]`,
//! so it is the guest's own test on a `std::string`'s long/short flag and has
//! nothing to do with the answer. **That is the third way the queue's
//! `nil-checked` column has been wrong in this project**, after
//! `NSAccessibilityRemoteUIElement` (a `test` on a different register) and
//! `NSSpellChecker` (a `test` 20 instructions later): here it is a `test` on the
//! *same* register, four instructions later, about a different value.
//!
//! The guest's own source settles what the ivar is. Fetched at the tag the
//! binary reports, `content/app_shim_remote_cocoa/web_drag_source_mac.mm`:
//!
//! ```objc
//!   UTType* __strong _fileType;          // the ivar at +0x3c0
//!   ...
//!     if (!mimeType.empty()) {
//!       _fileType = [UTType typeWithMIMEType:base::SysUTF8ToNSString(mimeType)];
//!       if (!_dropData.file_contents.empty()) {
//!         [writableTypes addObject:_fileType.identifier];   // no `if (_fileType)`
//!       }
//! ```
//!
//! and twice more further down with no test either:
//!
//! ```objc
//!   bool hasImageData = !_dropData.file_contents.empty() && _fileType &&
//!                       [_fileType conformsToType:UTTypeImage];
//!   ...
//!   if ([type isEqualToString:_fileType.identifier]) { … }
//!   ...
//!   if ([type isEqualToString:…kPasteboardTypeFilePromiseContent)]) {
//!     return _fileType.identifier;
//!   }
//! ```
//!
//! **The guard is on the MIME string being non-empty, never on the type.** The
//! guest is written on the invariant that a non-empty MIME type has a `UTType`,
//! which is true on macOS: `+[UTType typeWithMIMEType:]` is total there and
//! synthesises a type for a MIME type nothing declares.
//!
//! ### Why `nil` is refused here and not used
//!
//! Because this tree's `-[NSMutableArray addObject:]` does **not** raise on a
//! null argument. `crates/foundation/src/collections.rs:192` is
//!
//! ```text
//!     a.values.push(a.policy.retain(object as CFTypeRef));
//! ```
//!
//! with no null test above it, so a null answer to `+typeWithMIMEType:` becomes a
//! **null inside the array of pasteboard types the guest then hands to AppKit**.
//! On macOS that same line raises `NSInvalidArgumentException`; here it silently
//! produces a pasteboard type that is not a type. That is the failure this
//! project ranks first: a value that looks like a successful answer and is not,
//! in a surface the receiving application matches on by string equality.
//!
//! A stand-in `UTType` is worse, and the reason is specific to what the guest
//! does with the answer. `_fileType.identifier` is a **UTI string** compared by
//! `isEqualToString:` against the pasteboard types a destination advertises, and
//! `-[_fileType conformsToType:UTTypeImage]` needs the declared type hierarchy.
//! Both of those are Apple's data, in the `Info.plist` files of the system
//! frameworks, and neither is derivable from the image. A made-up identifier is
//! matched by nothing, so the drag would look like it carries a file and would
//! not - and `conformsToType:` would have to answer something invented.
//!
//! So this selector is **refused by name**. The refusal says which capability is
//! missing (the macOS UniformTypeIdentifiers declaration database) and which
//! caller needs it, and it respects `ROSETTA3_LOG_UNREGNIZED` so the queue
//! behind it stays readable in a measuring run.
//!
//! ## 2. The seven loads in `0x67555c0 .. 0x6755d80`, and the `isDynamic` test
//!
//! Three small functions. The first two are exact mirrors of each other:
//!
//! ```text
//!  6755625/54/65/77/82   r14 = the class; jmp 0x675566e   ; one hoisted load,
//!                                                         ; five spellings
//!  675568d  call 0x5d99ad0                ; std::string -> NSString
//!  67556a4  mov  rdi,r14 ; 67556a7 mov rdx,rax
//!  67556aa  call objc_msgSend            ; +[UTType typeWithFilenameExtension:]
//!  67556b8  mov  r14,rax                 ; the type
//!  67556c4  mov  rsi,[0xe6ea838] = isDynamic ; 67556cb mov rdi,r14
//!  67556ce  call objc_msgSend            ; -[type isDynamic]     <-- NO TEST on r14
//!  67556d4  test al,al ; je 0x67556dc    ; dynamic -> return false
//!  67556dc  mov  r15,[0xe6ea840] = preferredMIMEType
//!  67556e9  call objc_msgSend            ; -[type preferredMIMEType]
//!  6755703  test r12,r12 ; je 0x67556d8  ; nil   -> return false
//!  675570e  call objc_msgSend            ; the same message, for the value
//!  675571f  test rax,rax ; je 0x675577f  ; nil   -> return false
//! ```
//!
//! and the second is the same with `typeWithMIMEType:` /
//! `preferredFilenameExtension`.
//!
//! **`-[type isDynamic]` is sent to the answer with no test, and that is fine**,
//! and the reason is the same one that makes `NSSpellChecker`'s sends
//! survivable: on `objc_msgSend` the answer to a null receiver **is** the zero
//! the guest then tests, so there is no dereference to survive. `test al,al` at
//! `+0x67556d4` reads that zero, takes the "not dynamic" arm, asks
//! `preferredMIMEType` of the same null, gets nil, and returns **false** - "this
//! file has no MIME type".
//!
//! ## 3. `+typesWithTag:tagClass:conformingToType:`, and the tag class is null
//!
//! ```text
//!  675595e  mov  rax,[0xdd50b98] ; 6755965 mov rcx,[rax]   ; _UTTagClassMIMEType
//!  6755968  mov  rsi,[0xe6ea850] = typesWithTag:tagClass:conformingToType:
//!  6755972  mov  rdx,r15        ; the MIME type as an NSString
//!  6755975  xor  r8d,r8d        ; conformingToType: nil
//!  6755978  call objc_msgSend
//!  6755986  mov  rbx,rax
//!  6755992  test rbx,rbx ; je 0x6755cd2     ; <-- TESTED
//!  67559bd  call _objc_retain
//!  67559c6  mov  rsi,[0xe6e7d88] = countByEnumeratingWithState:objects:count:
//! ```
//!
//! Two things here. The answer **is** tested (`+0x6755992`), and the third
//! argument is `nil` by the guest's own code. And the second argument,
//! `_UTTagClassMIMEType`, is a **`__DATA_CONST,__got` slot this tree already
//! answers with a zeroed refusal** -
//! `crates/darwin/src/data_unavailable.rs:1232` has
//! `"_UTTagClassMIMEType" => refused(&PTR)` - so the tag class the guest passes
//! here is **null**, on this host, today, whatever `UTType` answers.
//!
//! ## 4. `select_file_dialog_bridge.mm`, and the last resort this host already takes
//!
//! ```text
//!  8160168  mov  rdx,[0xe6ea528] = typeWithFilenameExtension:  -> [rbp-0x100]
//!  8160176  mov  rdx,[0xe6e7e58] = localizedDescription         -> [rbp-0xf8]
//!  8160184  mov  rdx,[0xe6e7d68] = length                      -> [rbp-0xf0]
//!  ...
//!  8160245  mov  rbx,[0xe6e73c8]        ; the class
//!  816024c  call 0x5d99ad0              ; std::string -> NSString
//!  8160269  call objc_msgSend          ; +[UTType typeWithFilenameExtension:]
//!  8160277  mov  rbx,rax               ; the type
//!  816028d  call objc_msgSend          ; -[type localizedDescription]
//!  81602a8  call objc_msgSend          ; -[description length]
//!  81602ae  test rax,rax ; je 0x81602d0  ; empty -> no description
//!  ...
//!  816041a  mov  r13,[0xe6e73c8]        ; the class, the second loop
//!  816043a  call objc_msgSend          ; +[UTType typeWithFilenameExtension:]
//!  816045e  test r13,r13 ; je 0x81603f9  ; <-- TESTED: nil -> next extension
//! ```
//!
//! Both sites are tolerant, and each one's "no description" arm is what this host
//! should produce, because **the host has no localized description for a type it
//! does not declare**. This file's attribution is the one that is *positively*
//! established rather than bracketed: the `__FILE__` string
//! `../../components/remote_cocoa/app_shim/select_file_dialog_bridge.mm` at
//! `0x815f874` is referenced **from inside** the enclosing function
//! (`0x815f710 .. 0x8161e30`), read by `test/cstrings-in.py`.
//!
//! ## 5. `0x5af0cdf .. 0x5af1139`, and the arm this host is *already* in
//!
//! ```text
//!  5af0d5e  mov  rax,[0xdd50808] = _NSURLContentTypeKey
//!  5af0d68  mov  rsi,[0xe6e8630] = getResourceValue:forKey:error:
//!  5af0d7d  call objc_msgSend            ; ask the URL for its content type
//!  5af0d93  test r14b,r14b ; je 0x5af0f24 ; none -> fall through
//!  5af0d9c  mov  rsi,[0xe6e9138] = identifier
//!  5af0da6  call objc_msgSend            ; -[thatType identifier]
//!  5af0db7  test rax,rax ; je 0x5af0f2c  ; nil -> fall through
//!  ...
//!  5af0e35  mov  r14,[0xe6e73c8]          ; the class
//!  5af0e5c  call objc_msgSend            ; +[UTType typeWithFilenameExtension:]
//!  5af0e80  test r15,r15 ; je 0x5af0ec2  ; <-- TESTED
//!  5af0e8f  call objc_msgSend            ; -[type identifier]
//!  5af0ea0  test rax,rax ; je 0x5af0ef9
//!  ...
//!  5af0ec5  mov  rdi,[0xdd50800] = _UTTypeContent
//!  5af0ecf  mov  rsi,[0xe6e9138] = identifier
//!  5af0ed6  call objc_msgSend            ; -[_UTTypeContent identifier]
//!  ...
//!  5af0fb4  mov  rbx,[0xe6e73c8]          ; the class
//!  5af0ff9  call r12                      ; +[UTType typeWithIdentifier:]
//!  5af1017  mov  rdi,[0xe6e7390] = _OBJC_CLASS_$_NSWorkspace
//!  5af101e  mov  rsi,[0xe6e8f70] = sharedWorkspace
//!  5af1028  call rbx                      ; +[NSWorkspace sharedWorkspace]
//!  5af1035  mov  rsi,[0xe6ea538] = iconForContentType:
//!  5af1043  mov  rdx,r12 ; 5af1046 call r15
//! ```
//!
//! **The last resort at `+0x5af0ec5` is already a message to null on this
//! host.** `_UTTypeContent` is a `__DATA_CONST,__got` slot and
//! `crates/darwin/src/data_unavailable.rs:1233` refuses it:
//!
//! ```text
//!             "_UTTypeContent" => refused(&PTR),
//! ```
//!
//! so `[_UTTypeContent identifier]` sends `identifier` to zero and the guest
//! already gets nil there today, for a file whose `NSURLContentTypeKey` lookup
//! found nothing. **Answering `nil` to `+typeWithFilenameExtension:` puts this
//! caller in the arm it is already in.** The same is true of `_UTTypeImage` and
//! `_UTTagClassFilenameExtension` (`data_unavailable.rs:1231,1236`), which are
//! the other two UTType constants the guest imports.
//!
//! That is the whole reason `+typeWithFilenameExtension:` answers nil here and
//! `+typeWithMIMEType:` does not: for the first, every surrounding UTType
//! constant the guest uses is **already** null on this host, so nil is the
//! answer the guest is already receiving, and every one of its four sites has a
//! designed arm for it. For the second, one site stores the answer's
//! `identifier` in a real API surface with no arm, and null is not something
//! this host can put there.
//!
//! `+typeWithIdentifier:` is nil for the same reason as the first, with one
//! difference worth stating: at `+0x5af1043` the answer is passed straight to
//! `-[NSWorkspace iconForContentType:]` with no test, so the consequence of nil
//! is a file dialog with no icon on the extension row. That is the *absence* of a
//! thing, not a wrong thing, and the identifier it was asked for is one the
//! guest chose; nothing is claimed about the file that is not already claimed by
//! the `_UTTypeContent` refusal above.
//!
//! # Which file is `0x67555c0` and which is `0x5af0cdf`: not established
//!
//! `test/tools/whichfile.py` brackets `0x67555c0` between
//! `net/base/network_config_watcher_apple.cc` (`+0x3b6`) and
//! `net/cert/x509_util_apple.cc` (`-0x84f`), and `0x5af0cdf` between
//! `chrome/browser/file_select_helper.cc` (`+0x2b2`) and
//! `chrome/browser/ui/cocoa/icon_loader_mac.mm` (`-0x459`). **Both brackets are
//! wrong.** Fetched at tag `130.0.6723.91`, `net/base/network_config_watcher_apple.cc`
//! is 190 lines and contains no `UTType`, no `NSWorkspace` and no MIME-type
//! helper; `chrome/browser/file_select_helper.cc` is 700-odd lines and contains no
//! `UTType` either. The two functions have no `__FILE__` reference of their own -
//! `test/cstrings-in.py` over `0x67555c0:0x6755d80` returns **nothing**, and over
//! `0x5af0000:0x5af1400` returns only `ReadIcon` at `0x5af1132`, which belongs to
//! `icon_loader_mac.mm` - so there is nothing in the image to attribute them by.
//!
//! This is the **fourth** wrong `whichfile.py` attribution this project has
//! recorded (after the one in `AGENT-BRIEF.md` and the ones its callers flagged),
//! and the failure mode is the documented one: the bracket is bounded by
//! inlined `LOG()` calls, so a translation unit's marker can sit far from the
//! code that emitted it. **It is a hypothesis generator; the disassembly is the
//! authority.** The conclusions above rest only on the disassembly, and the one
//! file attribution that *is* claimed (`select_file_dialog_bridge.mm`) is claimed
//! because the `__FILE__` string is read by the function itself.
//!
//! # No `UTType` instance ever exists, so no instance method is installed
//!
//! With `+typeWithMIMEType:` refused and the other three answering null, the
//! class is never instantiated:
//!
//! - **no load is followed by an `_objc_alloc*`** - measured over all twelve,
//!   and it is why the queue's `inst` column is `-` for this class (see
//!   `test/callcensus.py` and the note at the end of this file);
//! - `+class` (sent at `0x3a920b9`'s neighbours and at two other sites in
//!   `0x675225c3`'s function) returns the class, and the guest uses it only as
//!   the argument of `-[other isKindOfClass:]`, which is a message to a
//!   *different* receiver;
//! - every instance message the guest sends to a `UTType` is sent to the answer
//!   of one of the four class methods, so its receiver is null or the refusal.
//!
//! So `-isDynamic`, `-preferredMIMEType`, `-preferredFilenameExtension`,
//! `-identifier`, `-localizedDescription` and `-conformsToType:` are
//! **deliberately not installed**. They would be unreachable surface pretending
//! otherwise, which is what `spell_checker.rs` calls out and what
//! `accessibility_remote_ui.rs` avoids by leaving `+class` alone.
//!
//! # The tool bug that made this module's `inst` column look empty on purpose
//!
//! `test/tools/classcensus.py:162` writes `stubnames[va] = nm.lstrip("_")` while
//! `ALLOCATORS` and `MSGSEND` (lines 102-104) keep the leading underscore, so
//! `name in ALLOCATORS` is `False` for every stub. Consequences, both verified by
//! running the constants:
//!
//! ```text
//! stubnames value     = 'objc_alloc'
//! 'objc_alloc' in ALLOCATORS  = False
//! 'objc_msgSend' in MSGSEND   = False
//! ```
//!
//! - the `inst` column of `test/logs/class-wall-queue.md` reads `-` for **all
//!   122 rows**, including the ones that are instance walls. `NSPopUpButton` is
//!   one: `0x815fb02` is `call _objc_alloc` on the classref, eleven bytes after
//!   the load, and that class is genuinely instantiated;
//! - the `sends` column misses every `call rel32`-to-stub `objc_msgSend` for the
//!   same reason. It also misses `call r12` / `call rbx` (a hoisted
//!   `objc_msgSend`), which is how `+[UTType typeWithMIMEType:]` is sent at
//!   `+0x6755811` - which is why this module counts 15 sends where the queue
//!   counts 13.
//!
//! The one-line fix is to stop stripping the underscore in `stubnames`, or to
//! strip it in the two sets. **The queue file itself was not regenerated** - other
//! agents are reading it.
//!
//! # Placement, and why it is `foundation` and not a new crate
//!
//! `UTType` is from `UniformTypeIdentifiers.framework`, which has no crate in
//! this tree, and the loader's report names that framework. This module lives in
//! `foundation` because `FRAMEWORKS` in `crates/rosetta3/src/frameworks.rs` is
//! `lib.flat(true)`: a data symbol published by any crate binds for any
//! framework, so which crate publishes `_OBJC_CLASS_$_UTType` is cosmetic as far
//! as the loader is concerned, and the module carries no state and no
//! framework-specific machinery. Moving it to a `crates/uniformtypeidentifiers`
//! is a `git mv` plus four registration lines and nothing here would change; the
//! orchestrator may prefer that and this says so rather than pretending the
//! placement is principled.
//!
//! # What a real answer needs
//!
//! The macOS UniformTypeIdentifiers declaration database: the `UTTypeDeclaration`
//! dictionaries inside every system framework's `Info.plist`, the tag-class
//! tables built from them, and the LaunchServices process that owns the merged
//! view. None of that is on this host, none of it is derivable from the image,
//! and the ten `_UTType*`/`_UTTagClass*` constants the guest imports are already
//! refused as named zeroed data in `darwin::data_unavailable`. A host with the
//! database and no LaunchServices would still not have `+typeWithMIMEType:`
//! total in the way the guest assumes.

use std::collections::BTreeSet;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// Published so a guest binding `_OBJC_CLASS_$_UTType` gets a class that
/// **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["UTType"];

/// What is missing, said in the refusal so the line names the capability and
/// not merely the class.
const MISSING_UTI_DATABASE: &str = "The missing capability is Apple's UniformTypeIdentifiers \
    declaration database: UTType is a view over the UTTypeDeclaration dictionaries inside every \
    system framework's Info.plist, the tag-class tables built from them, and the LaunchServices \
    process that owns the merged view. There is none of that on this host, and none of it is \
    derivable from the guest image. A nil here would not be 'no such type' - the guest stores \
    this answer in an ivar and puts its -identifier into the array of pasteboard types it hands \
    to AppKit, with no test on it anywhere, and this tree's -[NSMutableArray addObject:] accepts \
    null rather than raising, so a null type would become a pasteboard type that is not a type.";

/// The callers, said in the refusal, by name and by address.
const CALLERS: &str = "The send sites are +[UTType typeWithMIMEType:] at 0x3a9205f, whose answer \
    goes into the WebDragSource ivar at +0x3a92074 and is messaged -identifier at +0x3a920b9 with \
    no test between them - the guest is -[WebDragSource writableTypesForPasteboard:] in \
    content/app_shim_remote_cocoa/web_drag_source_mac.mm, and its own source guards on the MIME \
    string being non-empty, never on the type. The same selector at 0x6755811 is the net MIME \
    helper and IS nil-tolerant (-isDynamic answers zero, -preferredFilenameExtension answers nil, \
    the function returns false), so this refusal is costing reach there and is made anyway: one \
    selector is one IMP and the site that cannot survive nil is the site that decides it. The \
    other measured sites are +typeWithFilenameExtension: at 0x5af0e5c, 0x67556aa, 0x8160269 and \
    0x816043a, +typeWithIdentifier: at 0x5af0ff9 and +typesWithTag:tagClass:conformingToType: at \
    0x6755978, and all of those answer nil rather than refusing - see this module's comment for \
    why that is a different answer and not an inconsistency.";

/// `(class, selector)` pairs already named, so the refusal speaks once each.
static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Say once that there is no UTI database, then stop.
fn refuse(class: &str, sel: Sel) -> usize {
    // SAFETY: `objc::sel::name` is the selector itself, which `sel_registerName`
    // made a NUL-terminated string, and this is the only read of it.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) };
    let selector = name.to_string_lossy().into_owned();
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert((class.to_string(), selector.clone())) {
        eprintln!(
            "rosetta3: foundation: {class} {selector}: refused by name. {MISSING_UTI_DATABASE} \
             {CALLERS}{switch}",
            switch = if objc::msg::logging_unrecognised() {
                " ROSETTA3_LOG_UNRECOGNIZED is set, so this refusal is answering nil instead of \
                 aborting; a run this way cannot be trusted."
            } else {
                ""
            },
        );
    }
    match objc::msg::logging_unrecognised() {
        true => 0,
        false => std::process::abort(),
    }
}

/// The IMP for a class method that returns an object.
///
/// The class name is read off the **receiver** rather than baked in at install
/// time, so the message cannot name a different class than the one the selector
/// was installed on: it does not remember a class at all, it asks. A class
/// object's `isa` is its metaclass, and `corefoundation::base::register` gives
/// the metaclass the same name.
unsafe extern "C" fn refusing_returning(receiver: Id, sel: Sel) -> usize {
    // SAFETY: a receiver here is the class object, which is live; and a null
    // receiver cannot reach an IMP at all, because `objc_msgSend` returns first.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel)
}

/// The shared body for the three selectors that answer null.
///
/// One body, and it is `unsafe` only because the ABI is: `objc_msgSend` writes
/// the argument registers this function ignores. Returning `0` **is** "answer
/// nil" - it is not a stand-in for an answer, and the module comment above is
/// the argument for why null is the whole truth for these three.
unsafe extern "C" fn null_returning(_class: Id, _sel: Sel, _argument: Id) -> usize {
    0
}

/// Three arguments, for `+typesWithTag:tagClass:conformingToType:`.
///
/// A separate body rather than a `...` varargs one, because the argument count
/// comes from the selector's own colon count and a varargs body cannot be
/// checked against it: `assert_row` checks the *declaration*, and the
/// declaration is what this signature has to agree with.
unsafe extern "C" fn null_returning_three(
    _class: Id,
    _sel: Sel,
    _tag: Id,
    _tag_class: Id,
    _conforming_to: Id,
) -> usize {
    0
}

/// The class methods this module installs, with the encoding each is declared
/// with. Written out rather than generated so `test/tools/objcenc.py diff` can
/// see the rows: it parses `class_methods!` bodies and reports a table-driven
/// install as silence.
///
/// `#[cfg(test)]` because only the tests read it, as `spell_checker.rs:152` does.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] = &[
    // One colon, so one argument: frame `8 * (2 + 1)` = 24, `self` at 0, the
    // argument at 16, and 8 is the unencoded `_cmd`.
    ("typeWithMIMEType:", c"@24@0:8@16"),
    ("typeWithFilenameExtension:", c"@24@0:8@16"),
    ("typeWithIdentifier:", c"@24@0:8@16"),
    // Three colons, three arguments, so four encoded words and a frame of
    // `8 * (2 + 3)` = 40. The guest passes `nil` for the third
    // (`xor r8d,r8d` at 0x6755975) and a **null** tag class, because
    // `_UTTagClassMIMEType` is one of `darwin::data_unavailable`'s named zeroed
    // refusals.
    ("typesWithTag:tagClass:conformingToType:", c"@40@0:8@16@24@32"),
];

pub(crate) fn install() {
    // `register`, not `abstract_class`: the guest does `_objc_alloc` on classes
    // it instantiates, and a class nothing can allocate from would report a
    // different failure from the one this module is about. This one is
    // **not** allocated from in the guest (measured: no `_objc_alloc*` after any
    // of the twelve loads), so the difference does not arise here - but
    // `register` is the honest choice, and it is what makes `+class` answer with
    // the class rather than being absent.
    //
    // `register` returns the class already in the table when the name is taken,
    // so whether this runs before or after `rosetta3::program::
    // name_missing_classes` there is one class object per name.
    let class =
        corefoundation::base::register(c"UTType", None, size_of::<usize>(), false);

    class_methods!(class, {
        "typeWithMIMEType:" => refusing_returning, c"@24@0:8@16";
        "typeWithFilenameExtension:" => null_returning, c"@24@0:8@16";
        "typeWithIdentifier:" => null_returning, c"@24@0:8@16";
        "typesWithTag:tagClass:conformingToType:" => null_returning_three,
            c"@40@0:8@16@24@32";
    });
}

/// The IMP each row is installed with, so a test can check the pairing.
///
/// Not a `for` loop over `MEASURED` inside `install`, and not a `const` slice
/// beside it either: the two have to be *the same* list, and the way to make
/// that structural rather than editorial is for the test to read the table and
/// the runtime, as it does below.
#[cfg(test)]
fn expected_body(selector: &str) -> *const std::ffi::c_void {
    use std::ffi::c_void;
    match selector {
        "typeWithMIMEType:" => refusing_returning as *const c_void,
        "typeWithFilenameExtension:" | "typeWithIdentifier:" => {
            null_returning as *const c_void
        }
        "typesWithTag:tagClass:conformingToType:" => null_returning_three as *const c_void,
        other => unreachable!("{other} is not in MEASURED"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared encoding has the selector's own colon count, every
    /// argument at `8 * i`, and the frame reaching the last of them - and the
    /// encoding **that reached the runtime** is the one declared, not a copy.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            crate::encoding_check::assert_row(selector, types);
            crate::encoding_check::assert_installed(c"UTType", selector, types);
        }
    }

    /// The refusal is on `+typeWithMIMEType:` and on nothing else, and the three
    /// null answers are on the three selectors measured.
    ///
    /// This is the property the module comment argues for and it is a *negative*
    /// as much as a positive one: if `+class` were refused, the two
    /// `+[VNDetectBarcodesRequest class]`-shaped type tests elsewhere in the
    /// guest would abort for a reason that has nothing to do with a UTI
    /// database.
    #[test]
    fn exactly_the_measured_selectors_are_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"UTType").expect("installed by this module");
        for (selector, _) in MEASURED {
            assert!(
                class.metaclass().own_method(objc::sel::get(selector)).is_some(),
                "+[{selector}] is measured and must be installed"
            );
        }
        for unmeasured in ["class", "identifier", "conformsToType:", "isDynamic"] {
            assert!(
                class.own_method(objc::sel::get(unmeasured)).is_none(),
                "-{unmeasured} must NOT be installed: with the four class methods above, no \
                 UTType instance can exist, so an IMP for it would be unreachable surface"
            );
        }
    }

    /// Each row got the body its **declared shape** calls for, read back out of
    /// the class's own method list rather than out of the install.
    ///
    /// The pairing matters because the three null rows share one body and the
    /// refusal has its own: a swap would leave `+typeWithMIMEType:` answering
    /// null - the exact silent wrong answer this module refuses to give - and
    /// every test above would still pass.
    #[test]
    fn each_row_has_the_body_its_shape_calls_for() {
        crate::bootstrap();
        let class = objc::class::lookup(c"UTType").expect("installed by this module");
        for (selector, _) in MEASURED {
            let installed = class
                .metaclass()
                .method(objc::sel::get(selector))
                .expect("measured selector is installed")
                .implementation();
            assert_eq!(
                installed,
                expected_body(selector),
                "+[{selector}] got the wrong IMP"
            );
        }
        // And the refusal is on the one selector the module says it is on.
        assert_eq!(
            class
                .metaclass()
                .method(objc::sel::get("typeWithMIMEType:"))
                .expect("measured")
                .implementation(),
            refusing_returning as *const std::ffi::c_void
        );
    }

    /// Registered **and** published, so the guest's classref lands on a class
    /// whose methods name themselves rather than on an anonymous empty one.
    #[test]
    fn the_class_is_published() {
        crate::bootstrap();
        for name in CLASSES {
            let cname = std::ffi::CString::new(*name).unwrap();
            assert!(objc::class::lookup(&cname).is_some(), "{name} registered");
            assert!(
                crate::class_data()
                    .iter()
                    .any(|(symbol, _)| symbol == &format!("_OBJC_CLASS_$_{name}")),
                "{name} is published as data, not only registered"
            );
        }
    }

    /// The three null rows answer **null**, and null as an `id`: not a tagged
    /// null, not a class object, not a stand-in. A test that only checked "it
    /// returns something falsy" would pass for a stand-in too.
    #[test]
    fn the_three_null_rows_answer_null() {
        crate::bootstrap();
        // SAFETY: each row is `id (Class, SEL, id)` as declared in `MEASURED`
        // and installed by `install`.
        let f: unsafe extern "C" fn(Id, Sel, Id) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let class = objc::class::ptr(objc::class::lookup(c"UTType").unwrap()) as Id;
        for selector in ["typeWithFilenameExtension:", "typeWithIdentifier:"] {
            let answer =
                unsafe { f(class, objc::sel::get(selector), std::ptr::null_mut()) };
            assert_eq!(answer, 0, "+[{selector}] must answer null");
        }
        // SAFETY: `id (Class, SEL, id, id, id)`, as declared for the three-colon
        // row and installed by `install`.
        let g: unsafe extern "C" fn(Id, Sel, Id, Id, Id) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let answer = unsafe {
            g(
                class,
                objc::sel::get("typesWithTag:tagClass:conformingToType:"),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(answer, 0, "+typesWithTag:... must answer null");
    }

    /// The consequence the census called the wall, measured the way the guest
    /// measures it: `-isDynamic` sent to the null the class method answered
    /// gives the guest the zero it then tests.
    ///
    /// This is the check the queue's `nil-checked` column could not make. It
    /// reported "no" for the `+0x67556ce` site, and that is right about the
    /// absence of a test and wrong about the consequence: on `objc_msgSend` the
    /// answer to a null receiver *is* the zero this asserts, so there is no
    /// dereference to survive.
    #[test]
    fn the_follow_on_instance_message_answers_zero_on_the_null() {
        crate::bootstrap();
        // SAFETY: `-isDynamic` is `BOOL (id, SEL)`. It is deliberately **not**
        // installed on `UTType` (see `exactly_the_measured_selectors_are_
        // installed`), so this is a message to null and the runtime's own
        // contract for one is the answer.
        let f: unsafe extern "C" fn(Id, Sel) -> u8 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let dynamic = unsafe { f(std::ptr::null_mut(), objc::sel::get("isDynamic")) };
        assert_eq!(dynamic, 0, "a null type is not a dynamic type");
    }

    /// The refusal text names the missing capability, and says why nil was
    /// refused rather than only what is missing.
    #[test]
    fn the_missing_capability_is_named_rather_than_declared_unsupported() {
        assert!(MISSING_UTI_DATABASE.contains("UniformTypeIdentifiers"));
        assert!(
            MISSING_UTI_DATABASE.contains("would not be"),
            "the line has to say why nil was refused, not only what is missing"
        );
        // And the callers are named with addresses, because an address is the
        // part a reader can check against the binary.
        for site in ["0x3a9205f", "0x3a92074", "0x3a920b9", "0x6755811"] {
            assert!(
                CALLERS.contains(site),
                "the refusal names no address for {site}, and the address is the checkable part"
            );
        }
    }
}
