use super::consts::*;
use super::object::{Kind, Record, size};
use super::raw;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attributes {
    pub detached: bool,
    pub stack_size: Option<usize>,
    pub stack_top: Option<u64>,
    pub guard_size: Option<usize>,
    pub scope: i32,
    pub inherit_sched: i32,
    pub sched_policy: i32,
    pub sched_priority: i32,
    pub qos_class: u32,
    pub qos_relative_priority: i32,
}

impl Default for Attributes {
    /// `pthread_attr_init`
    fn default() -> Self {
        Attributes {
            detached: false,
            stack_size: None,
            stack_top: None,
            guard_size: None,
            scope: PTHREAD_SCOPE_SYSTEM,
            inherit_sched: PTHREAD_INHERIT_SCHED,
            sched_policy: SCHED_OTHER,
            sched_priority: DARWIN_PRIORITY_DEFAULT,
            qos_class: QOS_CLASS_UNSPECIFIED,
            qos_relative_priority: 0,
        }
    }
}

/// Slots within a `pthread_attr_t`, past the signature.
///
/// Sixty-four bytes to spend and seven settings to keep, so nothing is packed: a slot
/// each, which is one load per getter and legible in a dump.
mod slot {
    pub const DETACH: usize = 1;
    pub const STACK_SIZE: usize = 2;
    pub const STACK_TOP: usize = 3;
    pub const GUARD_SIZE: usize = 4;
    pub const SCOPE: usize = 5;
    pub const INHERIT: usize = 6;
    pub const SCHED: usize = 7;
}

/// A sentinel for "the guest did not say", distinct from a real zero.
///
/// A guest may legitimately ask for a zero guard size, which is not the same request
/// as never having asked.
const UNSET: u64 = u64::MAX;

fn get(r: &Record, index: usize) -> u64 {
    r.slot(index, size::ATTR)
        .load(std::sync::atomic::Ordering::Relaxed)
}

fn set(r: &Record, index: usize, value: u64) {
    r.slot(index, size::ATTR)
        .store(value, std::sync::atomic::Ordering::Relaxed)
}

impl Attributes {
    /// What the guest's `pthread_attr_t` at `at` says, or `None` if it is not one.
    ///
    /// A null `at` is the defaults, which is what `pthread_create` means by a null
    /// attribute pointer.
    ///
    /// # Safety
    ///
    /// `at` is null, or a guest `pthread_attr_t` - readable for [`size::ATTR`] bytes.
    pub unsafe fn of_guest(at: u64) -> Option<Attributes> {
        if at == 0 {
            return Some(Attributes::default());
        }
        #[cfg(target_os = "macos")]
        if !crate::libc_backend::using_builtin() {
            // SAFETY: native mode binds the guest's attribute calls to the host.
            return unsafe { Attributes::of_host_attr(at as usize as *const libc::pthread_attr_t) };
        }
        // SAFETY: builtin mode binds all guest attribute calls to this record.
        let r = unsafe { Record::at(at, Kind::Attr) };
        if !r.is_ours() {
            return None;
        }
        Some(Attributes {
            detached: get(&r, slot::DETACH) == PTHREAD_CREATE_DETACHED as u64,
            stack_size: unset_or(get(&r, slot::STACK_SIZE)).map(|n| n as usize),
            stack_top: unset_or(get(&r, slot::STACK_TOP)),
            guard_size: unset_or(get(&r, slot::GUARD_SIZE)).map(|n| n as usize),
            scope: get(&r, slot::SCOPE) as i32,
            inherit_sched: get(&r, slot::INHERIT) as i32,
            sched_policy: (get(&r, slot::SCHED) & 0xffff_ffff) as i32,
            sched_priority: (get(&r, slot::SCHED) >> 32) as i32,
            qos_class: (get(&r, slot::SCOPE) >> 32) as u32,
            qos_relative_priority: (get(&r, slot::INHERIT) >> 32) as i32,
        })
    }

    /// Read a host `pthread_attr_t`, also used by guests in macOS native mode.
    ///
    /// # Safety
    ///
    /// `at` is an initialised `pthread_attr_t`.
    #[cfg(target_os = "macos")]
    unsafe fn of_host_attr(at: *const libc::pthread_attr_t) -> Option<Attributes> {
        let mut detach = 0;
        // SAFETY: the caller's guarantee. This getter validates the structure's
        // signature and writes nothing when it refuses, which is how a byte copy of
        // the guest's attributes is checked before being read.
        if unsafe { crate::pthread::raw::pthread_attr_getdetachstate(at, &mut detach) } != 0 {
            return None;
        }
        let (mut size, mut guard) = (0usize, 0usize);
        let mut top: *mut core::ffi::c_void = core::ptr::null_mut();
        let (mut scope, mut inherit, mut policy) = (0, 0, 0);
        let mut param: libc::sched_param = unsafe { core::mem::zeroed() };
        // SAFETY: an initialised set, just established, and live locals.
        unsafe {
            libc::pthread_attr_getstacksize(at, &mut size);
            crate::pthread::raw::pthread_attr_getguardsize(at, &mut guard);
            libc::pthread_attr_getstackaddr(at, &mut top);
            libc::pthread_attr_getscope(at, &mut scope);
            libc::pthread_attr_getinheritsched(at, &mut inherit);
            libc::pthread_attr_getschedpolicy(at, &mut policy);
            libc::pthread_attr_getschedparam(at, &mut param);
        }
        Some(Attributes {
            detached: detach == PTHREAD_CREATE_DETACHED,
            stack_size: (size != 0).then_some(size),
            stack_top: (!top.is_null()).then_some(top as u64),
            guard_size: Some(guard),
            scope,
            inherit_sched: inherit,
            sched_policy: policy,
            sched_priority: param.sched_priority,
            qos_class: QOS_CLASS_UNSPECIFIED,
            qos_relative_priority: 0,
        })
    }

    /// A host `pthread_attr_t` saying what these attributes say.
    ///
    /// The stack is deliberately **not** carried across: what the guest asked for
    /// describes the *guest's* stack, and the host thread that runs its code gets one
    /// of this runtime's own. The caller states the host size it wants.
    ///
    /// Returns `EINVAL` for a setting with no host equivalent rather than dropping it,
    /// because a guest that asked for a first-fit mutex policy and did not get one is
    /// no worse off than a guest told no, while a guest that asked for a *detached*
    /// thread and got a joinable one leaks a thread per call.
    pub fn to_host_attr(&self, host_stack: usize) -> Result<HostAttr, i32> {
        let mut attr = HostAttr::new()?;
        // SAFETY: `attr` was just initialised by `pthread_attr_init`.
        unsafe {
            let a = attr.as_mut_ptr();
            host_result(libc::pthread_attr_setstacksize(a, host_stack))?;
            #[cfg(not(target_os = "macos"))]
            {
                let detach = match self.detached {
                    true => PTHREAD_CREATE_DETACHED,
                    false => PTHREAD_CREATE_JOINABLE,
                };
                let Some(detach) = detach_state_to_host(detach) else {
                    return Err(err::EINVAL);
                };
                host_result(libc::pthread_attr_setdetachstate(a, detach))?;
                if let Some(guard) = self.guard_size {
                    host_result(crate::pthread::raw::pthread_attr_setguardsize(a, guard))?;
                }
            }
            #[cfg(target_os = "macos")]
            {
                host_result(libc::pthread_attr_setdetachstate(
                    a,
                    match self.detached {
                        true => PTHREAD_CREATE_DETACHED,
                        false => PTHREAD_CREATE_JOINABLE,
                    },
                ))?;
                if let Some(guard) = self.guard_size {
                    host_result(crate::pthread::raw::pthread_attr_setguardsize(a, guard))?;
                }
            }
            let inherit = match self.inherit_sched {
                PTHREAD_INHERIT_SCHED => raw::PTHREAD_INHERIT_SCHED,
                PTHREAD_EXPLICIT_SCHED => raw::PTHREAD_EXPLICIT_SCHED,
                _ => return Err(err::EINVAL),
            };
            host_result(raw::pthread_attr_setinheritsched(a, inherit))?;
            if self.inherit_sched == PTHREAD_EXPLICIT_SCHED {
                let Some(policy) = sched_policy_to_host(self.sched_policy) else {
                    return Err(err::EINVAL);
                };
                let Some(priority) = sched_priority_to_host(self.sched_priority, policy) else {
                    return Err(err::EINVAL);
                };
                host_result(raw::pthread_attr_setschedpolicy(a, policy))?;
                let mut param: libc::sched_param = core::mem::zeroed();
                param.sched_priority = priority;
                host_result(raw::pthread_attr_setschedparam(a, &param))?;
            }
        }
        Ok(attr)
    }
}

fn host_result(rc: i32) -> Result<(), i32> {
    match rc {
        0 => Ok(()),
        rc => Err(err::from_host(rc)),
    }
}

fn unset_or(value: u64) -> Option<u64> {
    (value != UNSET).then_some(value)
}

/// A host `pthread_attr_t`, destroyed when it goes out of scope.
///
/// `pthread_create` copies what it needs, so the set only has to outlive the call -
/// and a guard is what makes that true on the error paths as well as the happy one.
pub struct HostAttr(libc::pthread_attr_t);

impl HostAttr {
    pub fn new() -> Result<HostAttr, i32> {
        // SAFETY: zeroed is a valid starting point for `pthread_attr_init`, which is
        // what makes the set initialised.
        let mut attr: libc::pthread_attr_t = unsafe { core::mem::zeroed() };
        // SAFETY: a live local.
        match unsafe { libc::pthread_attr_init(&mut attr) } {
            0 => Ok(HostAttr(attr)),
            e => Err(err::from_host(e)),
        }
    }

    pub fn as_ptr(&self) -> *const libc::pthread_attr_t {
        &self.0
    }

    pub fn as_mut_ptr(&mut self) -> *mut libc::pthread_attr_t {
        &mut self.0
    }
}

impl Drop for HostAttr {
    fn drop(&mut self) {
        // SAFETY: initialised in `new` and not used again.
        unsafe { libc::pthread_attr_destroy(&mut self.0) };
    }
}

/// Darwin's default thread stack, which a guest reading back a size it never set
/// must be told.
pub const DEFAULT_STACK_SIZE: usize = 512 * 1024;

/// The smallest stack `pthread_attr_setstacksize` takes.
pub const STACK_MIN: usize = 16 * 1024;

/// The page a stack size is a multiple of, and the default guard, as Darwin
/// reports them.
const PAGE_SIZE: usize = 4096;

/// Whether `pthread_attr_setstacksize` accepts `size`.
pub fn valid_stack_size(size: usize) -> bool {
    size >= STACK_MIN && size.is_multiple_of(PAGE_SIZE)
}

pub mod imp {
    use super::*;
    use crate::pthread::object::ours;

    /// # Safety
    ///
    /// Called only through an import slot this crate defines; `at` is the guest's own
    /// `pthread_attr_t *`.
    unsafe fn record(at: u64) -> Option<Record> {
        if at == 0 {
            return None;
        }
        let r = unsafe { Record::at(at, Kind::Attr) };
        r.is_ours().then_some(r)
    }

    pub unsafe extern "C" fn attr_init(at: u64) -> i32 {
        if at == 0 {
            return err::EINVAL;
        }
        // SAFETY: a guest `pthread_attr_t *`, writable for its own size.
        let r = unsafe { Record::at(at, Kind::Attr) };
        let d = Attributes::default();
        set(&r, slot::DETACH, PTHREAD_CREATE_JOINABLE as u64);
        set(&r, slot::STACK_SIZE, UNSET);
        set(&r, slot::STACK_TOP, UNSET);
        set(&r, slot::GUARD_SIZE, UNSET);
        set(&r, slot::SCOPE, d.scope as u64);
        set(&r, slot::INHERIT, d.inherit_sched as u64);
        set(
            &r,
            slot::SCHED,
            (d.sched_policy as u32 as u64) | ((d.sched_priority as u32 as u64) << 32),
        );
        // Last, and with release ordering: see `Record::publish`.
        r.slot(0, size::ATTR)
            .store(ours(Kind::Attr), std::sync::atomic::Ordering::Release);
        0
    }

    pub unsafe extern "C" fn attr_destroy(at: u64) -> i32 {
        // SAFETY: as above. Nothing is freed - an attribute set owns nothing - so
        // this only invalidates the signature.
        match unsafe { record(at) } {
            Some(r) => crate::pthread::object::destroy_plain(&r),
            None => err::EINVAL,
        }
    }

    /// The setters, each of which is "check the value, store it".
    macro_rules! setter {
        ($name:ident, $slot:expr, $check:expr) => {
            /// # Safety
            ///
            /// As [`attr_init`].
            pub unsafe extern "C" fn $name(at: u64, value: u64) -> i32 {
                let Some(r) = (unsafe { record(at) }) else {
                    return err::EINVAL;
                };
                let check: fn(u64) -> bool = $check;
                if !check(value) {
                    return err::EINVAL;
                }
                set(&r, $slot, value);
                0
            }
        };
    }

    /// The getters, each of which is "load it, write it through the out-parameter".
    macro_rules! getter {
        ($name:ident, $slot:expr, $ty:ty, $map:expr) => {
            /// # Safety
            ///
            /// As [`attr_init`], and `out` is a writable pointer of the stated width.
            pub unsafe extern "C" fn $name(at: u64, out: u64) -> i32 {
                let Some(r) = (unsafe { record(at) }) else {
                    return err::EINVAL;
                };
                if out == 0 {
                    return err::EINVAL;
                }
                let map: fn(u64) -> $ty = $map;
                // SAFETY: the caller states `out` is writable for this width, which is
                // what the declaration in `abi::table` says.
                unsafe { *(out as usize as *mut $ty) = map(get(&r, $slot)) };
                0
            }
        };
    }

    unsafe fn set_int(at: u64, index: usize, value: u64, valid: fn(i32) -> bool) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        let value = value as i32;
        if !valid(value) {
            return err::EINVAL;
        }
        set(
            &r,
            index,
            (get(&r, index) & 0xffff_ffff_0000_0000) | value as u32 as u64,
        );
        0
    }

    pub unsafe extern "C" fn attr_setdetachstate(at: u64, value: u64) -> i32 {
        unsafe {
            set_int(at, slot::DETACH, value, |v| {
                matches!(v, PTHREAD_CREATE_JOINABLE | PTHREAD_CREATE_DETACHED)
            })
        }
    }
    setter!(attr_setstacksize, slot::STACK_SIZE, |v| {
        usize::try_from(v).is_ok_and(super::valid_stack_size)
    });
    setter!(attr_setstackaddr, slot::STACK_TOP, |v| {
        v != 0 && v.is_multiple_of(DEFAULT_GUARD)
    });
    setter!(attr_setguardsize, slot::GUARD_SIZE, |_| true);
    pub unsafe extern "C" fn attr_setscope(at: u64, value: u64) -> i32 {
        // Validate the attribute object before diagnosing its requested scope.
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        let value = value as i32;
        match value {
            PTHREAD_SCOPE_PROCESS => err::ENOTSUP,
            PTHREAD_SCOPE_SYSTEM => {
                set(
                    &r,
                    slot::SCOPE,
                    (get(&r, slot::SCOPE) & 0xffff_ffff_0000_0000) | value as u32 as u64,
                );
                0
            }
            _ => err::EINVAL,
        }
    }
    pub unsafe extern "C" fn attr_setinheritsched(at: u64, value: u64) -> i32 {
        unsafe {
            set_int(at, slot::INHERIT, value, |v| {
                matches!(v, PTHREAD_INHERIT_SCHED | PTHREAD_EXPLICIT_SCHED)
            })
        }
    }

    pub unsafe extern "C" fn attr_set_qos_class_np(at: u64, class: u32, relative: i32) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if !super::super::valid_qos(class, relative) {
            return err::EINVAL;
        }
        let scope = get(&r, slot::SCOPE) as u32;
        let inherit = get(&r, slot::INHERIT) as u32;
        set(&r, slot::SCOPE, u64::from(scope) | (u64::from(class) << 32));
        set(
            &r,
            slot::INHERIT,
            u64::from(inherit) | ((relative as u32 as u64) << 32),
        );
        0
    }

    pub unsafe extern "C" fn attr_get_qos_class_np(at: u64, class: u64, relative: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if class == 0 || relative == 0 {
            return err::EINVAL;
        }
        unsafe {
            *(class as usize as *mut u32) = (get(&r, slot::SCOPE) >> 32) as u32;
            *(relative as usize as *mut i32) = (get(&r, slot::INHERIT) >> 32) as i32;
        }
        0
    }

    getter!(attr_getdetachstate, slot::DETACH, i32, |v| v as i32);
    getter!(attr_getscope, slot::SCOPE, i32, |v| v as i32);
    getter!(attr_getinheritsched, slot::INHERIT, i32, |v| v as i32);

    /// The three getters whose "unset" answer is a real value rather than the
    /// sentinel stored for it.
    ///
    /// # Safety
    ///
    /// As [`attr_init`].
    pub unsafe extern "C" fn attr_getstacksize(at: u64, out: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if out == 0 {
            return err::EINVAL;
        }
        // What a guest reading back a size it never set must see: the size a thread
        // would actually get. Darwin's default is 512 KiB, and reporting anything
        // else makes a guest that sizes a buffer off it wrong.
        let size = unset_or(get(&r, slot::STACK_SIZE)).unwrap_or(DEFAULT_STACK);
        // SAFETY: `out` is a `size_t *`.
        unsafe { *(out as usize as *mut usize) = size as usize };
        0
    }

    /// # Safety
    ///
    /// As [`attr_init`].
    pub unsafe extern "C" fn attr_getguardsize(at: u64, out: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if out == 0 {
            return err::EINVAL;
        }
        let guard = unset_or(get(&r, slot::GUARD_SIZE)).unwrap_or(DEFAULT_GUARD);
        // SAFETY: `out` is a `size_t *`.
        unsafe { *(out as usize as *mut usize) = guard as usize };
        0
    }

    /// # Safety
    ///
    /// As [`attr_init`].
    pub unsafe extern "C" fn attr_getstackaddr(at: u64, out: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if out == 0 {
            return err::EINVAL;
        }
        // SAFETY: `out` is a `void **`.
        unsafe { *(out as usize as *mut u64) = unset_or(get(&r, slot::STACK_TOP)).unwrap_or(0) };
        0
    }

    /// `pthread_attr_setstack(attr, base, size)`, whose argument is the **low**
    /// address where `pthread_attr_setstackaddr`'s is the high one.
    ///
    /// # Safety
    ///
    /// As [`attr_init`].
    pub unsafe extern "C" fn attr_setstack(at: u64, base: u64, bytes: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if base == 0
            || bytes < 16 * 1024
            || !base.is_multiple_of(DEFAULT_GUARD)
            || !bytes.is_multiple_of(DEFAULT_GUARD)
        {
            return err::EINVAL;
        }
        let Some(top) = base.checked_add(bytes) else {
            return err::EINVAL;
        };
        set(&r, slot::STACK_TOP, top);
        set(&r, slot::STACK_SIZE, bytes);
        0
    }

    /// # Safety
    ///
    /// As [`attr_init`], with two writable out-parameters.
    pub unsafe extern "C" fn attr_getstack(at: u64, base_out: u64, size_out: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if base_out == 0 || size_out == 0 {
            return err::EINVAL;
        }
        let size = unset_or(get(&r, slot::STACK_SIZE)).unwrap_or(DEFAULT_STACK);
        let top = unset_or(get(&r, slot::STACK_TOP)).unwrap_or(0);
        // SAFETY: two writable out-parameters, as the declaration says.
        unsafe {
            *(base_out as usize as *mut u64) = top.saturating_sub(size);
            *(size_out as usize as *mut usize) = size as usize;
        }
        0
    }

    /// # Safety
    ///
    /// As [`attr_init`].
    pub unsafe extern "C" fn attr_setschedpolicy(at: u64, policy: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if sched_policy_to_host(policy as i32).is_none() {
            return err::EINVAL;
        }
        let priority = get(&r, slot::SCHED) >> 32;
        set(&r, slot::SCHED, (priority << 32) | (policy & 0xffff_ffff));
        0
    }

    getter!(attr_getschedpolicy, slot::SCHED, i32, |v| (v & 0xffff_ffff)
        as i32);

    /// `struct sched_param` is one `int` on Darwin, which is what
    /// `__SCHED_PARAM_SIZE__` says.
    ///
    /// # Safety
    ///
    /// As [`attr_init`], with `param` a writable `struct sched_param *`.
    pub unsafe extern "C" fn attr_setschedparam(at: u64, param: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if param == 0 {
            return err::EINVAL;
        }
        // SAFETY: the caller states `param` is a readable `sched_param`, whose first
        // member is `sched_priority`.
        let priority = unsafe { *(param as usize as *const i32) } as u32 as u64;
        let policy = get(&r, slot::SCHED) & 0xffff_ffff;
        set(&r, slot::SCHED, (priority << 32) | policy);
        0
    }

    /// # Safety
    ///
    /// As [`attr_setschedparam`].
    pub unsafe extern "C" fn attr_getschedparam(at: u64, param: u64) -> i32 {
        let Some(r) = (unsafe { record(at) }) else {
            return err::EINVAL;
        };
        if param == 0 {
            return err::EINVAL;
        }
        // SAFETY: as above, writing rather than reading.
        unsafe { *(param as usize as *mut i32) = (get(&r, slot::SCHED) >> 32) as i32 };
        0
    }

    const DEFAULT_STACK: u64 = super::DEFAULT_STACK_SIZE as u64;
    const DEFAULT_GUARD: u64 = super::PAGE_SIZE as u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are Darwin's, which is what a guest reading a freshly initialised
    /// attribute set sees. Checked against the host's own defaults on a Mac, where
    /// `pthread_attr_init` is the function this stands in for.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_defaults_match_a_freshly_initialised_host_set() {
        let mut attr = HostAttr::new().expect("a host attribute set");
        // SAFETY: just initialised.
        let read = unsafe { Attributes::of_host_attr(attr.as_mut_ptr()) }.expect("readable");
        let d = Attributes::default();
        assert_eq!(read.detached, d.detached);
        assert_eq!(read.scope, d.scope);
        assert_eq!(read.inherit_sched, d.inherit_sched);
        assert_eq!(read.sched_policy, d.sched_policy);
    }

    #[test]
    fn a_null_attribute_pointer_is_the_defaults() {
        // SAFETY: null is the one address this is documented to accept.
        assert_eq!(
            unsafe { Attributes::of_guest(0) },
            Some(Attributes::default())
        );
    }

    #[test]
    fn garbage_is_not_an_attribute_set() {
        let bytes = [0u8; size::ATTR];
        // SAFETY: `bytes` is a live, correctly sized and aligned block this test owns.
        assert_eq!(unsafe { Attributes::of_guest(bytes.as_ptr() as u64) }, None);
    }
}
