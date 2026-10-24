use std::sync::OnceLock;

use crate::objc::host::{self, Imp, Sel};

/// The methods whose implementation this module stands in front of, as
/// `(class, selector, whether it is a class method)`.
///
/// Each takes the format as the *first* message argument, which is what lets
/// one trampoline serve them.
const FORMATTERS: &[(&str, &str, bool)] = &[
    ("NSString", "stringWithFormat:", true),
    ("NSString", "localizedStringWithFormat:", true),
    ("NSString", "initWithFormat:", false),
    ("NSMutableString", "appendFormat:", false),
    ("NSString", "stringByAppendingFormat:", false),
];

/// The implementation each selector had before this module replaced it.
///
/// Keyed by selector alone. That is only correct while no two entries above
/// share one, which [`install`] checks rather than assumes: a second class
/// installing the same selector would otherwise be dispatched to the first's.
static ORIGINALS: OnceLock<Vec<(u64, u64)>> = OnceLock::new();

/// Swizzle each format method onto [`trampoline`].
///
/// A method this tree does not define is skipped rather than reported: which
/// classes exist is the frameworks' business, and they are allowed to grow.
pub fn install() {
    let mut originals: Vec<(u64, u64)> = Vec::new();
    for (class, selector, is_class_method) in FORMATTERS {
        let (Some(c), Some(s)) = (host::c_string(class), host::c_string(selector)) else {
            continue;
        };
        // SAFETY: both strings are NUL-terminated and outlive the calls.
        let (method, sel) = unsafe {
            let cls = host::objc_lookUpClass(c.as_ptr());
            if cls.is_null() {
                continue;
            }
            let sel = host::sel_registerName(s.as_ptr());
            let method = match is_class_method {
                true => host::class_getClassMethod(cls, sel),
                false => host::class_getInstanceMethod(cls, sel),
            };
            (method, sel)
        };
        if method.is_null() {
            continue;
        }
        // SAFETY: `method` is a `Method` the runtime just returned.
        let previous = unsafe { host::method_setImplementation(method, trampoline as Imp) };
        if previous.is_null() || previous == trampoline as Imp {
            continue;
        }
        let key = sel as u64;
        if let Some(clash) = originals
            .iter()
            .find(|(s, imp)| *s == key && *imp != previous as u64)
        {
            objc::fail!(
                "{selector} has two implementations ({:#x} and {:#x}), which the \
                 selector-keyed table here cannot tell apart",
                clash.1,
                previous as u64
            );
        }
        originals.push((key, previous as u64));
    }
    let _ = ORIGINALS.set(originals);
    install_predicate();
    install_object_lists();
}

/// `+predicateWithFormat:`'s selector, which [`service`] answers itself.
static PREDICATE: OnceLock<u64> = OnceLock::new();

/// Add `+[NSPredicate predicateWithFormat:]` in front of [`service`]: the
/// arguments its conversions name are read off the tail and parsed with it.
fn install_predicate() {
    // SAFETY: NUL-terminated strings; every pointer is checked before use.
    unsafe {
        let class = host::objc_lookUpClass(c"NSPredicate".as_ptr());
        if class.is_null() {
            return;
        }
        let sel = host::sel_registerName(c"predicateWithFormat:".as_ptr());
        host::class_addMethod(
            host::object_getClass(class),
            sel,
            trampoline as Imp,
            c"@24@0:8@16".as_ptr(),
        );
        let _ = PREDICATE.set(sel as u64);
    }
}

/// The implementation `sel` had before [`install`].
fn original(sel: Sel) -> Option<Imp> {
    let key = sel as u64;
    ORIGINALS
        .get()?
        .iter()
        .find(|(s, _)| *s == key)
        .map(|(_, imp)| *imp as usize as Imp)
}

/// Render the format against the tail and hand the original implementation the
/// result.
///
/// `tail` is a `va_list` on x86-64 and the address of the first stack argument
/// on AArch64, which is what Darwin's `va_list` is there.
unsafe extern "C" fn service(receiver: u64, sel: Sel, format: u64, tail: u64) -> u64 {
    if Some(sel as u64) == PREDICATE.get().copied() {
        // SAFETY: `+predicateWithFormat:`'s format is an `NSString`.
        let text = unsafe { foundation::string::bytes(format as usize as objc::object::Id) };
        let text = String::from_utf8_lossy(&text);
        let kinds = foundation::predicate::conversions(&text);
        let Some(arguments) =
            super::nslog::native::tail_arguments("predicateWithFormat:", tail, &kinds)
        else {
            objc::fail!("+[NSPredicate predicateWithFormat:]: unreadable arguments for {text:?}");
        };
        return foundation::predicate::with_arguments(&text, &arguments) as usize as u64;
    }
    let Some(original) = original(sel) else {
        objc::fail!("a format method was serviced before its implementation was recorded");
    };
    // SAFETY: `original` is the `IMP` the runtime held for this selector, and
    // all four of them take `(id, SEL, NSString *)`. The one returning void
    // leaves the return register undefined, which is what discarding it means.
    let call = |fmt: u64| unsafe {
        let f: unsafe extern "C" fn(u64, Sel, u64) -> u64 = core::mem::transmute(original);
        f(receiver, sel, fmt)
    };
    match super::nslog::native::render_escaped("stringWithFormat:", format, tail) {
        Some(rendered) => call(rendered),
        // An unreadable format is the caller's error to have, and passing it
        // through is what the emulated path does with one too.
        None => call(format),
    }
}

/// Three fixed arguments - receiver, selector, format - and the tail after them.
///
/// See `shims::nslog::native` for what each of the two ABIs needs; this is the
/// same capture with one more named parameter.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn trampoline() {
    core::arch::naked_asm!(
        "mov x3, sp",
        "b   {service}",
        service = sym service,
    )
}

#[cfg(not(target_arch = "aarch64"))]
#[unsafe(naked)]
unsafe extern "C" fn trampoline() {
    core::arch::naked_asm!(
        "push rbp",
        "mov  rbp, rsp",
        "sub  rsp, 208",
        "mov  [rsp + 24], rcx",
        "mov  [rsp + 32], r8",
        "mov  [rsp + 40], r9",
        "test al, al",
        "je   2f",
        "movaps [rsp + 48],  xmm0",
        "movaps [rsp + 64],  xmm1",
        "movaps [rsp + 80],  xmm2",
        "movaps [rsp + 96],  xmm3",
        "movaps [rsp + 112], xmm4",
        "movaps [rsp + 128], xmm5",
        "movaps [rsp + 144], xmm6",
        "movaps [rsp + 160], xmm7",
        "2:",
        // Three integer registers are the named parameters', so the tail starts
        // at the fourth.
        "mov  dword ptr [rsp + 176], 24",
        "mov  dword ptr [rsp + 180], 48",
        "lea  rax, [rbp + 16]",
        "mov  [rsp + 184], rax",
        "mov  [rsp + 192], rsp",
        "lea  rcx, [rsp + 176]",
        "call {service}",
        "mov  rsp, rbp",
        "pop  rbp",
        "ret",
        service = sym service,
    )
}

/// Nil-terminated object lists, as `(class, variadic, counted, pairs,
/// instance)`: the method a program sends, and the counted one that says the same thing
/// without an ellipsis. `pairs` reads the list as `object, key, object, key`.
///
/// The emulated backend rewrites these in `objc::dispatch` before any `IMP` is
/// reached; a native image reaches one, so it has to be a variadic one.
const OBJECT_LISTS: &[(&str, &str, &str, bool, bool)] = &[
    (
        "NSArray",
        "arrayWithObjects:",
        "arrayWithObjects:count:",
        false,
        false,
    ),
    (
        "NSDictionary",
        "dictionaryWithObjectsAndKeys:",
        "dictionaryWithObjects:forKeys:count:",
        true,
        false,
    ),
    // The initialiser spellings, as instance methods: `instance` is the last
    // field. The counted method is found on the receiver's class either way.
    (
        "NSArray",
        "initWithObjects:",
        "initWithObjects:count:",
        false,
        true,
    ),
    (
        "NSDictionary",
        "initWithObjectsAndKeys:",
        "initWithObjects:forKeys:count:",
        true,
        true,
    ),
    (
        "NSSet",
        "setWithObjects:",
        "setWithObjects:count:",
        false,
        false,
    ),
    (
        "NSSet",
        "initWithObjects:",
        "initWithObjects:count:",
        false,
        true,
    ),
];

/// `(variadic selector, counted selector, pairs)` for each installed list.
static LISTS: OnceLock<Vec<(u64, u64, bool)>> = OnceLock::new();

/// Add each object list as a class method answered by [`list_trampoline`].
///
/// Added rather than swizzled: an `IMP` cannot take an ellipsis, so a class that
/// already has one of these is left with what it has.
fn install_object_lists() {
    let mut lists = Vec::new();
    for (class, variadic, counted, pairs, instance) in OBJECT_LISTS {
        let (Some(c), Some(v), Some(n)) = (
            host::c_string(class),
            host::c_string(variadic),
            host::c_string(counted),
        ) else {
            continue;
        };
        // SAFETY: NUL-terminated strings that outlive the calls; every pointer
        // the runtime answers is checked before use.
        unsafe {
            let cls = host::objc_lookUpClass(c.as_ptr());
            if cls.is_null() {
                continue;
            }
            let variadic = host::sel_registerName(v.as_ptr());
            let counted = host::sel_registerName(n.as_ptr());
            let (found, owner) = match instance {
                true => (host::class_getInstanceMethod(cls, counted), cls),
                false => (
                    host::class_getClassMethod(cls, counted),
                    host::object_getClass(cls),
                ),
            };
            if found.is_null() {
                continue;
            }
            host::class_addMethod(
                owner,
                variadic,
                list_trampoline as Imp,
                c"@24@0:8@16".as_ptr(),
            );
            lists.push((variadic as u64, counted as u64, *pairs));
        }
    }
    let _ = LISTS.set(lists);
}

/// Integer registers left for the tail after receiver, selector and first
/// object. Darwin's AArch64 ABI puts every variadic argument on the stack.
#[cfg(target_arch = "aarch64")]
const TAIL_REGISTERS: usize = 0;
#[cfg(not(target_arch = "aarch64"))]
const TAIL_REGISTERS: usize = 3;

/// Collect the list up to its nil and send the counted method.
///
/// `registers` holds [`TAIL_REGISTERS`] saved integer registers and `stack` is
/// the first stack-passed argument; the tail is the one and then the other.
unsafe extern "C" fn list_service(
    receiver: u64,
    sel: Sel,
    first: u64,
    registers: *const u64,
    stack: *const u64,
) -> u64 {
    let Some(&(_, counted, pairs)) = LISTS
        .get()
        .and_then(|l| l.iter().find(|(v, _, _)| *v == sel as u64))
    else {
        objc::fail!("an object list was serviced before its counted method was recorded");
    };
    let mut at = 0;
    let mut next = || {
        // SAFETY: the caller passed a nil-terminated list, and this stops
        // reading at the nil.
        let value = unsafe {
            match at < TAIL_REGISTERS {
                true => *registers.add(at),
                false => *stack.add(at - TAIL_REGISTERS),
            }
        };
        at += 1;
        value
    };
    let (mut objects, mut keys) = (Vec::new(), Vec::new());
    let mut object = first;
    while object != 0 {
        objects.push(object);
        if pairs {
            keys.push(next());
        }
        object = next();
    }
    // SAFETY: the receiver is a class or an instance of one, and the counted
    // method was found on it at install time with the prototype each arm transmutes to.
    unsafe {
        let imp = host::class_getMethodImplementation(
            host::object_getClass(receiver as host::Class),
            counted as Sel,
        );
        match pairs {
            false => {
                let f: unsafe extern "C" fn(u64, Sel, *const u64, usize) -> u64 =
                    core::mem::transmute(imp);
                f(receiver, counted as Sel, objects.as_ptr(), objects.len())
            }
            true => {
                let f: unsafe extern "C" fn(u64, Sel, *const u64, *const u64, usize) -> u64 =
                    core::mem::transmute(imp);
                f(
                    receiver,
                    counted as Sel,
                    objects.as_ptr(),
                    keys.as_ptr(),
                    objects.len(),
                )
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn list_trampoline() {
    core::arch::naked_asm!(
        "mov x3, xzr",
        "mov x4, sp",
        "b   {service}",
        service = sym list_service,
    )
}

#[cfg(not(target_arch = "aarch64"))]
#[unsafe(naked)]
unsafe extern "C" fn list_trampoline() {
    core::arch::naked_asm!(
        "push rbp",
        "mov  rbp, rsp",
        "sub  rsp, 32",
        "mov  [rsp], rcx",
        "mov  [rsp + 8], r8",
        "mov  [rsp + 16], r9",
        "mov  rcx, rsp",
        "lea  r8, [rbp + 16]",
        "call {service}",
        "mov  rsp, rbp",
        "pop  rbp",
        "ret",
        service = sym list_service,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five objects put the tail past the three saved registers and onto the
    /// stack, which is the half of the walk a short list never reaches.
    ///
    /// x86-64 only: a Rust variadic call on another host follows that host's
    /// ABI rather than Darwin's.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_nil_terminated_list_reaches_the_counted_method() {
        foundation::bootstrap();
        install();
        let objects: Vec<u64> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| foundation::string::from_str(s) as u64)
            .collect();
        // SAFETY: NUL-terminated literals; the trampoline is installed above
        // and is `id (Class, SEL, id, ...)`.
        unsafe {
            let class = host::objc_lookUpClass(c"NSArray".as_ptr());
            let sel = host::sel_registerName(c"arrayWithObjects:".as_ptr());
            let imp = host::class_getMethodImplementation(host::object_getClass(class), sel);
            let f: unsafe extern "C" fn(u64, Sel, u64, ...) -> u64 = core::mem::transmute(imp);
            let [a, b, c, d, e] = objects[..] else {
                unreachable!()
            };
            let array = f(class as u64, sel, a, b, c, d, e, 0u64);
            let array = corefoundation::collections::array(array as *const std::ffi::c_void)
                .expect("an array");
            assert_eq!(array.values.len(), 5);
            assert_eq!(array.values[4] as u64, e);

            let one = f(class as u64, sel, a, 0u64);
            let one = corefoundation::collections::array(one as *const std::ffi::c_void)
                .expect("an array");
            assert_eq!(one.values.len(), 1);
        }
    }
}
