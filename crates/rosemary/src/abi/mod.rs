use core::fmt;

use machish::memory::AddressSpace;

use microlift::arch::x86_64::context::RegisterContext;
use microlift::core::abi::CallAbi;
pub use microlift::function_traits::{DynamicSymbol, FunctionArgumentType as ArgType};

pub mod aggregate;
pub mod marshal;
pub mod table;

use static_init::dynamic;
pub use table::{FunctionAbi, Lookup};

/// The convention the guest passes arguments in.
///
/// A property of the binary being translated - an Intel macOS executable is SysV AMD64
/// - and not of the machine this runs on.
pub fn guest_call_abi() -> CallAbi {
    guest_call_abi_ref().clone()
}

/// [`guest_call_abi`] without building one.
///
/// A [`CallAbi`] owns two `Vec`s - six integer argument registers and eight vector ones
/// - so constructing it is two heap allocations, and [`marshal::call_types`] did it
/// *per intercepted call* purely to ask where the arguments are.
pub fn guest_call_abi_ref() -> &'static CallAbi {
    #[dynamic]
    static ABI: CallAbi = CallAbi::sysv_amd64();
    &ABI
}

/// Anything that stops an intercepted call from being made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiError {
    /// Nothing in [`table`] describes this symbol.
    NoSignature { symbol: String },
    /// Known, and deliberately not callable - see the reason.
    Refused {
        symbol: String,
        reason: &'static str,
    },
    /// Variadic, and no shim implements it. Variadics cannot be forwarded generically.
    NoShim { symbol: String },
    /// The signature is known but this marshaller cannot express the call:
    /// more than eight integer or eight floating-point arguments.
    Unmarshallable {
        symbol: String,
        reason: &'static str,
    },
    /// The import resolved to nothing - an unsatisfied weak import, whose slot
    /// holds zero. Calling it would jump to null.
    Unsatisfied { symbol: String },
    /// A guest memory access failed: a stack argument or a string argument
    /// pointing outside anything mapped.
    Memory { symbol: String, addr: u64 },
    /// A shim called *into the guest* and that call failed. The payload is the inner
    /// failure, rendered.
    Nested { symbol: String, reason: String },
}

impl AbiError {
    /// The same failure, said about `symbol` instead.
    pub fn renaming(self, symbol: String) -> Self {
        match self {
            AbiError::NoSignature { .. } => AbiError::NoSignature { symbol },
            AbiError::Refused { reason, .. } => AbiError::Refused { symbol, reason },
            AbiError::NoShim { .. } => AbiError::NoShim { symbol },
            AbiError::Unmarshallable { reason, .. } => AbiError::Unmarshallable { symbol, reason },
            AbiError::Unsatisfied { .. } => AbiError::Unsatisfied { symbol },
            AbiError::Memory { addr, .. } => AbiError::Memory { symbol, addr },
            AbiError::Nested { reason, .. } => AbiError::Nested { symbol, reason },
        }
    }

    /// The symbol the failure is about. Always present, by construction.
    pub fn symbol(&self) -> &str {
        match self {
            AbiError::NoSignature { symbol }
            | AbiError::Refused { symbol, .. }
            | AbiError::NoShim { symbol }
            | AbiError::Unmarshallable { symbol, .. }
            | AbiError::Unsatisfied { symbol }
            | AbiError::Memory { symbol, .. }
            | AbiError::Nested { symbol, .. } => symbol,
        }
    }
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AbiError::NoSignature { symbol } => {
                write!(f, "{symbol}: no signature; add one to rosemary::abi::table")
            }
            AbiError::Refused { symbol, reason } => write!(f, "{symbol}: not callable: {reason}"),
            AbiError::NoShim { symbol } => {
                write!(f, "{symbol}: variadic, and no shim implements it")
            }
            AbiError::Unmarshallable { symbol, reason } => {
                write!(f, "{symbol}: cannot be marshalled: {reason}")
            }
            AbiError::Unsatisfied { symbol } => {
                write!(f, "{symbol}: unsatisfied import, resolved to null")
            }
            AbiError::Memory { symbol, addr } => {
                write!(f, "{symbol}: guest memory at {addr:#x} is not readable")
            }
            AbiError::Nested { symbol, reason } => write!(f, "{symbol}: {reason}"),
        }
    }
}

impl std::error::Error for AbiError {}

/// Result of an intercepted call.
pub type Result<T> = core::result::Result<T, AbiError>;

/// The guest's memory, as the marshaller and the shims need it.
pub trait GuestMemory {
    fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error>;
    fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error>;
}

impl<S: AddressSpace> GuestMemory for S {
    fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
        AddressSpace::read(self, va, out)
    }

    fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
        AddressSpace::write(self, va, data)
    }
}

/// Longest C string a shim will follow before giving up.
///
/// Totality, not policy.
pub const MAX_CSTR: usize = 64 * 1024;

impl dyn GuestMemory + '_ {
    /// Read one 64-bit little-endian word.
    pub fn read_u64(&self, va: u64, symbol: &str) -> Result<u64> {
        let mut buf = [0u8; 8];
        self.read(va, &mut buf).map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: va,
        })?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Read one 32-bit little-endian word - a `wchar_t`, on both sides.
    pub fn read_u32(&self, va: u64, symbol: &str) -> Result<u32> {
        let mut buf = [0u8; 4];
        self.read(va, &mut buf).map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: va,
        })?;
        Ok(u32::from_le_bytes(buf))
    }

    /// Write one 64-bit little-endian word.
    pub fn write_u64(&mut self, va: u64, value: u64, symbol: &str) -> Result<()> {
        self.write(va, &value.to_le_bytes())
            .map_err(|_| AbiError::Memory {
                symbol: symbol.to_string(),
                addr: va,
            })
    }

    /// Read `len` bytes, or report which address could not be followed.
    pub fn read_bytes(&self, va: u64, len: usize, symbol: &str) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        self.read(va, &mut out).map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: va,
        })?;
        Ok(out)
    }

    /// Write `data`, or report which address could not be followed.
    pub fn write_bytes(&mut self, va: u64, data: &[u8], symbol: &str) -> Result<()> {
        self.write(va, data).map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: va,
        })
    }

    /// Read a NUL-terminated string, capped at [`MAX_CSTR`] bytes.
    ///
    /// Reads a byte at a time rather than a block, because the string may end
    /// one byte before the end of a mapped region and a block read would fail
    /// on bytes the guest never asked about.
    pub fn read_cstr(&self, va: u64, symbol: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for i in 0..MAX_CSTR as u64 {
            let mut b = [0u8; 1];
            self.read(va.wrapping_add(i), &mut b)
                .map_err(|_| AbiError::Memory {
                    symbol: symbol.to_string(),
                    addr: va,
                })?;
            if b[0] == 0 {
                return Ok(out);
            }
            out.push(b[0]);
        }
        Ok(out)
    }
}

/// Service one intercepted call.
pub fn call_import(
    symbol: &str,
    tag: u64,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    call_import_with(symbol, tag, host, ctx, mem, &NoCallbacks)
}

/// [`call_import`], with somewhere for a guest function pointer to go.
pub fn call_import_with(
    symbol: &str,
    tag: u64,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    call_planned(plan(symbol, tag, host), symbol, host, ctx, mem, callbacks)
}

/// What a call to one import comes to.
///
/// Every question in it is asked of the symbol's *name* - a shim match, a table
/// hash, the trace list - and none of the answers change once start-up is over,
/// so a caller that services the same site repeatedly asks once and keeps this.
/// See `ImportSite::plan`.
#[derive(Clone, Copy)]
pub struct Plan {
    route: Route,
    /// `ROSEMARY_TRACE_CALL` names this symbol.
    traced: bool,
}

#[derive(Clone, Copy)]
enum Route {
    Unsatisfied,
    Shim(crate::shims::Resolved),
    Call(&'static FunctionAbi),
    /// Described, but variadic and with no shim to read its arguments.
    Variadic,
    Refused(&'static str),
    Unknown,
}

/// Work out how a call to `symbol`, resolved to `host`, is serviced.
pub fn plan(symbol: &str, tag: u64, host: u64) -> Plan {
    let route = if host == 0 {
        Route::Unsatisfied
    } else if let Some(shim) = crate::shims::resolve(symbol) {
        // Shims first, unconditionally: a shim exists precisely because passing the
        // call through would be wrong, so it must win over any table entry.
        Route::Shim(shim)
    } else {
        match table::lookup(tag, symbol) {
            Lookup::Known(abi) if abi.variadic => Route::Variadic,
            Lookup::Known(abi) => Route::Call(abi),
            Lookup::Refused(reason) => Route::Refused(reason),
            Lookup::Unknown => Route::Unknown,
        }
    };
    Plan {
        route,
        traced: crate::trace::watching(symbol),
    }
}

/// Service one call to `symbol` as `plan`, which [`plan`] made for the same
/// `symbol` and `host`.
pub fn call_planned(
    plan: Plan,
    symbol: &str,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    if !plan.traced {
        return call_route(plan.route, symbol, host, ctx, mem, callbacks);
    }
    crate::trace::entering(symbol, ctx);
    let result = call_route(plan.route, symbol, host, ctx, mem, callbacks);
    crate::trace::leaving(symbol, ctx, &result);
    result
}

/// [`call_planned`], without the trace around it.
fn call_route(
    route: Route,
    symbol: &str,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    match route {
        Route::Shim(shim) => shim.call(ctx, mem, callbacks),
        Route::Call(abi) => marshal::call_with(symbol, abi, host, ctx, mem, callbacks),
        Route::Unsatisfied => Err(AbiError::Unsatisfied {
            symbol: symbol.to_string(),
        }),
        Route::Variadic => Err(AbiError::NoShim {
            symbol: symbol.to_string(),
        }),
        Route::Refused(reason) => Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason,
        }),
        Route::Unknown => Err(AbiError::NoSignature {
            symbol: symbol.to_string(),
        }),
    }
}

/// How a guest function pointer becomes something the host can call.
pub trait Callbacks {
    /// What to pass the host in place of the guest's function-pointer argument `va`,
    /// while servicing `symbol`.
    fn substitute(&self, symbol: &str, va: u64) -> Result<u64>;

    /// Substitute a callback with an exact host/guest ABI signature.
    ///
    /// A generic thunk cannot carry by-value aggregates that live on the
    /// guest stack. Implementations without a typed trampoline must refuse
    /// this operation instead of losing callback arguments.
    fn substitute_typed(
        &self,
        symbol: &str,
        va: u64,
        _signature: crate::thunk::ThunkSignature,
    ) -> Result<u64> {
        if va == 0 {
            return Ok(0);
        }
        Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "takes a typed guest callback, and this caller has no typed thunk",
        })
    }

    /// The guest address behind `va`, if `va` is a thunk this runtime handed out; `va`
    /// otherwise.
    ///
    /// The direction [`Callbacks::substitute`] alone cannot express, and it is not
    /// symmetry for its own sake.
    fn unsubstitute(&self, va: u64) -> u64 {
        va
    }
}

/// The bridge for a caller that has no JIT to re-enter - the marshaller's own tests,
/// and [`call_import`] itself.
pub struct NoCallbacks;

impl Callbacks for NoCallbacks {
    fn substitute(&self, symbol: &str, va: u64) -> Result<u64> {
        if va == 0 {
            return Ok(0);
        }
        Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "takes a guest function pointer, and this caller has no JIT to re-enter",
        })
    }
}

#[cfg(test)]
mod callback_tests {
    use super::{AbiError, Callbacks, NoCallbacks};
    use crate::thunk::ThunkSignature;

    #[test]
    fn a_null_typed_callback_needs_no_jit() {
        let signature = ThunkSignature::new(vec![], None).unwrap();
        assert_eq!(
            NoCallbacks
                .substitute_typed("test", 0, signature.clone())
                .unwrap(),
            0
        );
        assert_eq!(NoCallbacks.substitute("test", 0).unwrap(), 0);
        assert!(matches!(
            NoCallbacks.substitute_typed("test", 0x1234, signature),
            Err(AbiError::Refused { .. })
        ));
    }
}

/// How an import will be reached, once classified.
///
/// The summary the driver prints, and the shape of the acceptance question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    /// Signature known and marshallable: inlined by the lifter, no VM exit.
    Fast,
    /// Serviced at an `ImportedFunctionCall` exit - a variadic with a shim, or
    /// a signature this marshaller cannot inline.
    Slow,
    /// Known and deliberately not callable: data read through its slot, or a
    /// callback whose shape the host->guest thunk cannot carry.
    Refused(&'static str),
    /// Nothing describes it. The one outcome the acceptance test forbids.
    Unknown,
}

/// Decide how `symbol` will be reached, without calling anything.
pub fn classify(tag: u64, symbol: &str) -> Classification {
    // A shimmed symbol is slow whatever its signature says, because
    // [`crate::ImportPolicy`] refuses to promote one.
    if crate::shims::is_shimmed(symbol) {
        let Some(abi) = promotable_abi(symbol) else {
            return Classification::Slow;
        };
        return match guest_call_abi().can_fast_call(abi.types) && marshal::is_marshallable(abi) {
            true => Classification::Fast,
            false => Classification::Slow,
        };
    }
    match table::lookup(tag, symbol) {
        Lookup::Known(abi) if abi.variadic => Classification::Slow,
        // A fast import is inlined by the lifter, which passes the guest's
        // registers straight through - including the function pointer that has
        // to become a thunk first.
        Lookup::Known(abi) if !abi.callbacks.is_empty() => Classification::Slow,
        Lookup::Known(abi) if abi.reenters_guest => Classification::Slow,
        Lookup::Known(abi) => {
            if guest_call_abi().can_fast_call(abi.types) && marshal::is_marshallable(abi) {
                Classification::Fast
            } else {
                Classification::Slow
            }
        }
        Lookup::Refused(reason) => Classification::Refused(reason),
        Lookup::Unknown => Classification::Unknown,
    }
}

/// An import as an inlined native call
#[derive(Clone, Copy, Debug)]
pub struct Promotion {
    /// The signature microlift reproduces, built from the entry's own row.
    pub abi: &'static FunctionAbi,
    /// The address to call: the host's own, or a substituting function of this
    /// runtime's.
    pub address: u64,
}

/// `name` as an inlined native call, if this host has a table that says so.
pub fn promotion(name: &str, resolved: u64) -> Option<Promotion> {
    crate::objc::fast::promotion(name, resolved)
}

/// The signature of a promotable entry, for a caller that only wants to classify it.
///
/// [`classify`] asks this so the audit reports how an import will really be reached;
/// [`promotion`] is what actually decides, because only it knows the resolved address.
pub fn promotable_abi(name: &str) -> Option<&'static FunctionAbi> {
    crate::objc::fast::abi_of(name)
}

/// The `DynamicSymbol` microlift wants for a fast import.
pub fn dynamic_symbol(abi: &FunctionAbi, host: u64) -> DynamicSymbol {
    DynamicSymbol {
        name: abi.name,
        value: host as usize as *mut core::ffi::c_void,
        arg_types: abi.types.to_vec(),
        is_ellipsis_function: abi.variadic,
        has_execution_side_effects: false,
    }
}
