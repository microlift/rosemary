use std::cell::Cell;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicPtr, Ordering};

use microlift::cranelift_codegen;
use microlift::cranelift_frontend;

use cranelift_codegen::ir::{
    AbiParam, ArgumentPurpose, Function, InstBuilder, MemFlagsData, Signature, StackSlotData,
    StackSlotKind, UserFuncName, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use microlift::arch::x86_64::context::RegisterContext;
use microlift::backend::CraneliftBackend;
use microlift::os::get_host_calling_conv;

use crate::abi::ArgType as T;
use crate::abi::aggregate::{Aggregate, Eightbyte, GuestClass, HostClass, Shape};
use crate::vm::{Vm, VmError};

/// The result slot a trampoline gives [`typed_entry`] to fill: four doubles, the
/// widest thing AAPCS64 returns in registers - and the largest aggregate this
/// runtime will carry back through a SysV host's hidden buffer.
const RESULT_BYTES: usize = 32;

/// Integer argument registers the guest's convention uses.
const GUEST_INT_ARGS: usize = 6;
/// Vector argument registers the guest's convention uses.
const GUEST_FP_ARGS: usize = 8;

/// One compiled trampoline.
pub struct Thunk {
    /// The guest function it re-enters.
    pub guest: u64,
    /// Its own address - what the host is given.
    pub host: u64,
    /// Keeps the JIT module, and therefore `host`'s pages, mapped.
    _backend: CraneliftBackend,
    /// Typed callbacks keep their signature at a stable address until this thunk dies.
    _signature: Option<Box<ThunkSignature>>,
}

// SAFETY: as for `Translation` in `crate::cache`, and for the same reason. A
// thunk is immutable once compiled; `host` is an address the *host* calls, from
// whichever thread it likes, which is the entire point of it. `_backend` is held
// to keep that address mapped and is never read again.
unsafe impl Send for Thunk {}
unsafe impl Sync for Thunk {}

/// Every thunk handed out, both ways round.
pub struct ThunkTable {
    by_guest: BTreeMap<u64, Thunk>,
    // One guest entry can be registered with different callback prototypes.
    // Neither a generic thunk nor a differently typed one may be reused.
    typed_by_guest: BTreeMap<u64, Vec<(ThunkSignature, Thunk)>>,
    by_host: BTreeMap<u64, u64>,
}

impl ThunkTable {
    pub fn new() -> Self {
        ThunkTable {
            by_guest: BTreeMap::new(),
            typed_by_guest: BTreeMap::new(),
            by_host: BTreeMap::new(),
        }
    }

    /// The thunk for a guest address, if one has been built.
    pub fn host_for(&self, guest: u64) -> Option<u64> {
        self.by_guest.get(&guest).map(|t| t.host)
    }

    /// A typed thunk for the exact guest address and callback signature.
    pub fn typed_host_for(&self, guest: u64, signature: &ThunkSignature) -> Option<u64> {
        self.typed_by_guest
            .get(&guest)?
            .iter()
            .find(|(s, _)| s == signature)
            .map(|(_, t)| t.host)
    }

    /// The guest address behind a thunk, if this is one.
    pub fn guest_for(&self, host: u64) -> Option<u64> {
        self.by_host.get(&host).copied()
    }

    /// How many native thunks this table owns, including typed variants.
    pub fn len(&self) -> usize {
        self.by_host.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_host.is_empty()
    }

    /// Add a typed thunk, unless one for the same guest and signature already exists.
    ///
    /// Returns the host address to hand out and whether `thunk` was the one kept. The
    /// first thunk wins: its address may already be in host hands, so it must stay
    /// mapped, and a racing duplicate that never left this table is the one dropped.
    pub fn insert_typed(&mut self, signature: ThunkSignature, thunk: Thunk) -> (u64, bool) {
        if let Some(existing) = self.typed_host_for(thunk.guest, &signature) {
            return (existing, false);
        }
        let (guest, host) = (thunk.guest, thunk.host);
        self.by_host.insert(host, guest);
        self.typed_by_guest
            .entry(guest)
            .or_default()
            .push((signature, thunk));
        (host, true)
    }

    /// Add a generic thunk, unless the guest address already has one. As for
    /// [`ThunkTable::insert_typed`], the first thunk wins.
    pub fn insert(&mut self, thunk: Thunk) -> (u64, bool) {
        if let Some(existing) = self.host_for(thunk.guest) {
            return (existing, false);
        }
        let (guest, host) = (thunk.guest, thunk.host);
        self.by_host.insert(host, guest);
        self.by_guest.insert(guest, thunk);
        (host, true)
    }

    /// A snapshot of both directions, for [`ThunkView`].
    pub fn view(&self) -> ThunkView {
        ThunkView {
            by_guest: self.by_guest.iter().map(|(g, t)| (*g, t.host)).collect(),
            by_host: self.by_host.iter().map(|(h, g)| (*h, *g)).collect(),
            count: self.by_host.len(),
        }
    }
}

impl Default for ThunkTable {
    fn default() -> Self {
        Self::new()
    }
}

/// The read-only half of a [`ThunkTable`], published for lookups that must not take a
/// lock.
///
/// A stale snapshot answers "no" for a thunk another thread created a moment ago, and
/// unlike a code translation that answer is not merely older - it is wrong, because
/// unwrapping a thunk is what keeps a guest pointer comparing equal to itself.
pub struct ThunkView {
    by_guest: Vec<(u64, u64)>,
    by_host: Vec<(u64, u64)>,
    /// How many thunks existed when this was built.
    pub count: usize,
}

impl ThunkView {
    /// The thunk for a guest address, if this snapshot has one.
    pub fn host_for(&self, guest: u64) -> Option<u64> {
        Self::find(&self.by_guest, guest)
    }

    /// The guest address behind a thunk, if this snapshot knows it.
    pub fn guest_for(&self, host: u64) -> Option<u64> {
        Self::find(&self.by_host, host)
    }

    fn find(pairs: &[(u64, u64)], key: u64) -> Option<u64> {
        pairs
            .binary_search_by_key(&key, |(k, _)| *k)
            .ok()
            .map(|i| pairs[i].1)
    }
}

// The VM the current thread is executing, for the duration of a guest call.
thread_local! {
    static CURRENT: Cell<*const Vm> = const { Cell::new(std::ptr::null()) };
}

// The VM to fall back on when the thread-local cannot answer. It exists for
// exactly one moment: process exit.
static PUBLISHED: AtomicPtr<Vm> = AtomicPtr::new(std::ptr::null_mut());

/// Run `f` with `vm` reachable from a thunk on this thread.
pub(crate) fn with_current<R>(vm: &Vm, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT.with(|c| c.replace(vm as *const Vm));
    let out = f();
    CURRENT.with(|c| c.set(previous));
    out
}

/// Publish `vm` for the rest of the process's life, and for every thread.
///
/// And a thread the guest spawned is started by the *host's* libpthread, which calls a
/// thunk on a thread that has never been inside [`with_current`].
pub fn publish(vm: &'static Vm) {
    PUBLISHED.store(vm as *const Vm as *mut Vm, Ordering::Release);
}

/// The VM this thread is executing, if any.
///
/// Also how [`crate::unwind`] reaches the guest's tables.
pub(crate) fn current() -> Option<&'static Vm> {
    // `try_with`, not `with`: at process exit this thread's TLS has been
    // destroyed and reading it would panic inside a host `atexit` handler.
    let p = match CURRENT.try_with(|c| c.get()) {
        Ok(p) if !p.is_null() => p,
        _ => PUBLISHED.load(Ordering::Acquire) as *const Vm,
    };
    // SAFETY: non-null only between `with_current`'s replace and restore, during
    // which the `&Vm` it was given is alive and borrowed immutably - which is
    // what makes re-entering it here legal. The `'static` is a lie the private
    // scope contains: the reference never escapes `thunk_entry`.
    (!p.is_null()).then(|| unsafe { &*p })
}

/// A callback whose argument and return types are known, as the emitted trampoline and
/// the Rust entry both need them.
#[derive(Clone, PartialEq, Eq)]
pub struct ThunkSignature {
    /// `self` and `_cmd` included: these are the arguments the guest really
    /// takes, in order.
    params: Vec<Shape>,
    /// `None` for `void`.
    returns: Option<Shape>,
    /// How each parameter arrives from the *host*, flattened to the scalars the
    /// trampoline's own signature declares.
    host_leaves: Vec<Vec<T>>,
}

/// Why a callback's shape cannot be carried, in the register a refusal uses.
type Unsupported = &'static str;

impl ThunkSignature {
    /// Decide the host-side shape of every argument, or say what stopped it.
    ///
    /// An HFA occupies the same registers as its floating-point members and a
    /// composite of at most sixteen bytes as one or two integers. Larger AAPCS64
    /// composites arrive as pointers to a caller-owned copy; SysV composites
    /// instead occupy the incoming stack argument area.
    pub fn new(params: Vec<Shape>, returns: Option<Shape>) -> Result<Self, Unsupported> {
        let mut host_leaves = Vec::with_capacity(params.len());
        for shape in &params {
            host_leaves.push(match shape {
                Shape::Scalar(t) => vec![*t],
                Shape::Struct(agg) => match agg.host_class()? {
                    HostClass::Hfa { ty, count } => vec![ty; count],
                    HostClass::Integer { words } => {
                        vec![T::UnsignedDoubleword; words]
                    }
                    // One declared scalar per eightbyte, each the full width of
                    // the register its class names - so a packed pair of floats
                    // is one `f64` here and not two `f32`s.
                    class @ HostClass::Eightbytes { .. } => class
                        .slots()
                        .into_iter()
                        .map(|c| match c {
                            Eightbyte::Sse => T::DoublePrecisionFp,
                            Eightbyte::Integer => T::UnsignedDoubleword,
                        })
                        .collect(),
                    // Both hosts deliver an address for a large composite.
                    // AAPCS64 passes a pointer in the next integer argument
                    // slot; SysV uses the incoming stack argument area.
                    // These leaves describe the spill words, not ABI parameters.
                    HostClass::Indirect => vec![T::UnsignedDoubleword; agg.size.div_ceil(8)],
                },
            });
        }
        // A multi-value return lands where AAPCS64 puts an aggregate's members -
        // `v0`–`v3` for an HFA, `x0`/`x1` for a composite - which is what lets a
        // trampoline declare scalars and still return a struct. Larger than that
        // the callee fills a buffer through `x8`, a register Cranelift's return
        // lowering gives no way to declare.
        //
        // SysV hands the buffer over as an ordinary first argument instead, which
        // Cranelift *does* have a way to declare, so on that host the same shape
        // is carried rather than refused. See [`ThunkSignature::indirect_return`].
        if let Some(Shape::Struct(agg)) = &returns
            && agg.host_class()? == HostClass::Indirect
        {
            if cfg!(target_arch = "aarch64") {
                return Err("returns an aggregate too large for AAPCS64's registers");
            }
            if agg.size > RESULT_BYTES {
                return Err("returns an aggregate larger than this runtime buffers");
            }
        }
        Ok(ThunkSignature {
            params,
            returns,
            host_leaves,
        })
    }

    /// Size of the aggregate the *host* returns through a caller-supplied buffer,
    /// or `None` when the return travels in registers.
    ///
    /// `Some` only where the host's convention hands that buffer over as an argument;
    /// [`ThunkSignature::new`] has already refused the shape otherwise.
    fn indirect_return(&self) -> Option<usize> {
        match self.returns.as_ref()? {
            Shape::Struct(agg) if agg.host_class().ok()? == HostClass::Indirect => Some(agg.size),
            _ => None,
        }
    }

    /// What the trampoline declares it returns, and the offset in [`RESULT_BYTES`]
    /// each value is read from.
    ///
    /// Empty for an indirect return: nothing comes back in a register, and the buffer
    /// the caller passed is filled instead.
    fn return_leaves(&self) -> Result<Vec<(types::Type, i32)>, Unsupported> {
        if self.indirect_return().is_some() {
            return Ok(Vec::new());
        }
        Ok(match self.returns.as_ref() {
            None | Some(Shape::Scalar(T::None)) => Vec::new(),
            Some(Shape::Scalar(t)) => vec![(Self::cl_type(*t), 0)],
            Some(Shape::Struct(agg)) => match agg.host_class()? {
                HostClass::Hfa { ty, count } => {
                    let width = crate::abi::aggregate::width_of(ty) as i32;
                    (0..count as i32)
                        .map(|i| (Self::cl_type(ty), i * width))
                        .collect()
                }
                HostClass::Integer { words } => {
                    (0..words as i32).map(|i| (types::I64, i * 8)).collect()
                }
                // As above: one return value per eightbyte, read from that
                // eightbyte's own offset in the result buffer.
                class @ HostClass::Eightbytes { .. } => class
                    .slots()
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let ty = match c {
                            Eightbyte::Sse => types::F64,
                            Eightbyte::Integer => types::I64,
                        };
                        (ty, i as i32 * 8)
                    })
                    .collect(),
                HostClass::Indirect => {
                    return Err("returns an aggregate too large for AAPCS64's registers");
                }
            },
        })
    }

    /// The Cranelift type one host-side leaf arrives as.
    fn cl_type(t: T) -> types::Type {
        match t {
            T::SinglePrecisionFp => types::F32,
            T::DoublePrecisionFp => types::F64,
            _ => types::I64,
        }
    }
}

/// How one of a trampoline's declared parameters arrives, and where it is spilled.
///
/// Most parameters are one scalar in one register and one word in the spill slot.
/// An aggregate the host passes in memory is the exception: one declared parameter
/// carrying an address, and as many spill words as the aggregate has eightbytes.
enum Incoming {
    Leaf { word: usize, ty: T },
    Aggregate { word: usize, size: usize },
}

impl ThunkSignature {
    /// The parameters a trampoline for this signature declares, in order.
    fn incoming(&self) -> Vec<Incoming> {
        let mut out = Vec::new();
        let mut word = 0usize;
        for (shape, leaves) in self.params.iter().zip(&self.host_leaves) {
            let indirect = match shape {
                Shape::Struct(agg) => {
                    matches!(agg.host_class(), Ok(HostClass::Indirect)).then_some(agg.size)
                }
                Shape::Scalar(_) => None,
            };
            match indirect {
                Some(size) => {
                    out.push(Incoming::Aggregate { word, size });
                    word += leaves.len();
                }
                None => {
                    for &ty in leaves {
                        out.push(Incoming::Leaf { word, ty });
                        word += 1;
                    }
                }
            }
        }
        out
    }
}

/// Copy `size` bytes from the trampoline's result slot to the caller's buffer.
///
/// Widest-first down to a single byte, so a struct whose size is not a multiple of
/// eight - `{d f}` at twelve, say - is copied exactly and nothing past its end is
/// written. The caller's buffer is only guaranteed to be `size` bytes long.
fn copy_out(
    b: &mut FunctionBuilder,
    ptr: types::Type,
    out: cranelift_codegen::ir::StackSlot,
    dest: cranelift_codegen::ir::Value,
    size: usize,
) {
    let mut at = 0usize;
    for (width, ty) in [
        (8, types::I64),
        (4, types::I32),
        (2, types::I16),
        (1, types::I8),
    ] {
        while size - at >= width {
            let v = b.ins().stack_load(ptr, ty, out, at as i32);
            b.ins().store(MemFlagsData::trusted(), v, dest, at as i32);
            at += width;
        }
    }
}

/// Compile a trampoline that re-enters the guest at `guest_va`, placing arguments by
/// `signature` rather than by the generic shape.
///
/// The generic thunk takes six integer and eight floating-point registers because it
/// has no types to work from.
pub(crate) fn build_typed(guest_va: u64, signature: ThunkSignature) -> Result<Thunk, VmError> {
    build_typed_with_entry(
        guest_va,
        signature,
        typed_entry as *const () as usize as u64,
    )
}

fn build_typed_with_entry(
    guest_va: u64,
    signature: ThunkSignature,
    host_entry: u64,
) -> Result<Thunk, VmError> {
    let conv = get_host_calling_conv();
    let fail = |reason: String| VmError::Thunk {
        guest: guest_va,
        reason,
    };

    // The host machine this trampoline is being built for. Cranelift's frontend
    // is told rather than asked; see [`crate::jit::frontend_config`].
    let frontend = crate::jit::frontend_config().map_err(|e| fail(e.to_string()))?;
    let ptr = frontend.pointer_type();

    let incoming = signature.incoming();
    let words: usize = signature.host_leaves.iter().map(Vec::len).sum();
    // The buffer a caller passes for a return its convention sends to memory.
    // First in the list, ahead of `self` - which is what makes it the hidden
    // argument the callee expects rather than a declared one.
    let indirect = signature.indirect_return();
    let mut sig = Signature::new(conv);
    if indirect.is_some() {
        sig.params
            .push(AbiParam::special(ptr, ArgumentPurpose::StructReturn));
    }
    for arrival in &incoming {
        sig.params.push(match arrival {
            Incoming::Leaf { ty, .. } => AbiParam::new(ThunkSignature::cl_type(*ty)),
            // Rounded up because the backend requires whole eightbytes, which is
            // also what the caller reserved: SysV pads a stack argument out.
            Incoming::Aggregate { size, .. } => {
                if cfg!(target_arch = "aarch64") {
                    // AAPCS64 passes composites larger than 16 bytes by
                    // pointer. Cranelift does not support StructArgument there.
                    AbiParam::new(ptr)
                } else {
                    AbiParam::special(
                        ptr,
                        ArgumentPurpose::StructArgument(size.next_multiple_of(8) as u32),
                    )
                }
            }
        });
    }
    let ret_leaves = signature.return_leaves().map_err(|e| fail(e.to_string()))?;
    for (ty, _) in &ret_leaves {
        sig.returns.push(AbiParam::new(*ty));
    }

    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig);
    let mut fn_ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fn_ctx);

    // One word per host-side leaf, in declaration order. The Rust entry knows
    // the type of each from the descriptor, so the slot carries no tags.
    let spill = b.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (words.max(1) * 8) as u32,
        3,
    ));
    // Where the entry writes the result, so a floating-point return does not
    // have to travel back through an integer register.
    let out = b.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        RESULT_BYTES as u32,
        3,
    ));

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);

    let params: Vec<_> = b.block_params(entry).to_vec();
    // The struct-return buffer is a parameter but not an argument, so it is
    // taken off the front before the rest are spilled in declaration order.
    let (sret, args) = match indirect {
        Some(_) => (Some(params[0]), &params[1..]),
        None => (None, &params[..]),
    };
    for (arrival, &v) in incoming.iter().zip(args) {
        match arrival {
            Incoming::Leaf { word, .. } => {
                b.ins().stack_store(ptr, v, spill, (word * 8) as i32);
            }
            // `v` is the address of the caller's copy, so the bytes are read
            // through it into the words the entry expects to find them in.
            Incoming::Aggregate { word, size } => {
                for i in 0..size.div_ceil(8) {
                    let offset = i * 8;
                    let bytes = (size - offset).min(8);
                    // A SysV caller reserved whole eightbytes (see the
                    // `StructArgument` above); an AAPCS64 caller's copy is only
                    // the object's own size, so its last partial eightbyte is
                    // read a byte at a time.
                    let w = if bytes == 8 || !cfg!(target_arch = "aarch64") {
                        b.ins()
                            .load(types::I64, MemFlagsData::trusted(), v, offset as i32)
                    } else {
                        let mut w = b.ins().iconst(types::I64, 0);
                        for j in 0..bytes {
                            let byte = b.ins().load(
                                types::I8,
                                MemFlagsData::trusted(),
                                v,
                                (offset + j) as i32,
                            );
                            let byte = b.ins().uextend(types::I64, byte);
                            let byte = b.ins().ishl_imm_u(byte, (j * 8) as i64);
                            w = b.ins().bor(w, byte);
                        }
                        w
                    };
                    b.ins().stack_store(ptr, w, spill, ((word + i) * 8) as i32);
                }
            }
        }
    }

    let descriptor = Box::new(signature);
    let desc = b.ins().iconst(
        types::I64,
        (&*descriptor as *const ThunkSignature) as usize as i64,
    );
    let spill_ptr = b.ins().stack_addr(types::I64, spill, 0);
    let out_ptr = b.ins().stack_addr(types::I64, out, 0);
    let guest = b.ins().iconst(types::I64, guest_va as i64);

    let mut callee_sig = Signature::new(conv);
    for _ in 0..4 {
        callee_sig.params.push(AbiParam::new(types::I64));
    }
    let callee_sig = b.import_signature(callee_sig);
    let callee = b.ins().iconst(types::I64, host_entry as i64);
    b.ins()
        .call_indirect(callee_sig, callee, &[guest, desc, spill_ptr, out_ptr]);

    // The entry filled `out` either way; an indirect return is that buffer's
    // bytes copied on to the caller's. Cranelift adds the return of the pointer
    // itself, so this returns nothing.
    if let (Some(sret), Some(size)) = (sret, indirect) {
        copy_out(&mut b, ptr, out, sret, size);
        b.ins().return_(&[]);
    } else {
        let rets: Vec<_> = ret_leaves
            .iter()
            .map(|(ty, offset)| b.ins().stack_load(ptr, *ty, out, *offset))
            .collect();
        b.ins().return_(&rets);
    }
    b.finalize(frontend);

    let mut backend = crate::jit::backend().map_err(|e| fail(e.to_string()))?;
    let code = backend.compile(func).map_err(|e| fail(e.to_string()))?;
    Ok(Thunk {
        guest: guest_va,
        host: code as usize as u64,
        _backend: backend,
        _signature: Some(descriptor),
    })
}

/// Compile a trampoline that re-enters the guest at `guest_va`.
pub(crate) fn build(guest_va: u64) -> Result<Thunk, VmError> {
    let conv = get_host_calling_conv();
    let fail = |reason: String| VmError::Thunk {
        guest: guest_va,
        reason,
    };

    let mut sig = Signature::new(conv);
    for _ in 0..GUEST_INT_ARGS {
        sig.params.push(AbiParam::new(types::I64));
    }
    for _ in 0..GUEST_FP_ARGS {
        sig.params.push(AbiParam::new(types::F64));
    }
    sig.returns.push(AbiParam::new(types::I64));

    let frontend = crate::jit::frontend_config().map_err(|e| fail(e.to_string()))?;
    let ptr = frontend.pointer_type();

    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig);
    let mut fn_ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fn_ctx);

    let ints = b.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (GUEST_INT_ARGS * 8) as u32,
        3,
    ));
    let floats = b.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (GUEST_FP_ARGS * 8) as u32,
        3,
    ));

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);

    let params: Vec<_> = b.block_params(entry).to_vec();
    for (i, &v) in params.iter().take(GUEST_INT_ARGS).enumerate() {
        b.ins().stack_store(ptr, v, ints, (i * 8) as i32);
    }
    for (i, &v) in params.iter().skip(GUEST_INT_ARGS).enumerate() {
        b.ins().stack_store(ptr, v, floats, (i * 8) as i32);
    }

    let int_ptr = b.ins().stack_addr(types::I64, ints, 0);
    let fp_ptr = b.ins().stack_addr(types::I64, floats, 0);
    let guest = b.ins().iconst(types::I64, guest_va as i64);

    let mut callee_sig = Signature::new(conv);
    callee_sig.params.push(AbiParam::new(types::I64));
    callee_sig.params.push(AbiParam::new(types::I64));
    callee_sig.params.push(AbiParam::new(types::I64));
    callee_sig.returns.push(AbiParam::new(types::I64));
    let callee_sig = b.import_signature(callee_sig);
    let callee = b
        .ins()
        .iconst(types::I64, thunk_entry as *const () as usize as i64);

    let call = b
        .ins()
        .call_indirect(callee_sig, callee, &[guest, int_ptr, fp_ptr]);
    let result = b.inst_results(call)[0];
    b.ins().return_(&[result]);
    b.finalize(frontend);

    let mut backend = crate::jit::backend().map_err(|e| fail(e.to_string()))?;
    let code = backend.compile(func).map_err(|e| fail(e.to_string()))?;
    Ok(Thunk {
        guest: guest_va,
        host: code as usize as u64,
        _backend: backend,
        _signature: None,
    })
}

/// The guest's argument registers, being filled.
///
/// SysV AMD64 allocates them in declaration order and - the rule that matters for
/// aggregates - moves the *whole* aggregate to memory if the registers it needs are not
/// all still free.
#[derive(Default)]
struct GuestArgs {
    ints: Vec<u64>,
    sse: Vec<u64>,
    stack: Vec<u8>,
}

impl GuestArgs {
    fn push_int(&mut self, v: u64) {
        match self.ints.len() < GUEST_INT_ARGS {
            true => self.ints.push(v),
            false => self.stack.extend_from_slice(&v.to_le_bytes()),
        }
    }

    fn push_sse(&mut self, bits: u64) {
        match self.sse.len() < GUEST_FP_ARGS {
            true => self.sse.push(bits),
            false => self.stack.extend_from_slice(&bits.to_le_bytes()),
        }
    }

    /// Place an aggregate whose bytes are `raw`, by SysV's classification.
    fn push_aggregate(&mut self, agg: &Aggregate, raw: &[u8]) {
        let eightbytes = |raw: &[u8], i: usize| {
            let mut word = [0u8; 8];
            let start = i * 8;
            let take = raw.len().saturating_sub(start).min(8);
            word[..take].copy_from_slice(&raw[start..start + take]);
            u64::from_le_bytes(word)
        };
        match agg.guest_class() {
            GuestClass::Registers(classes) => {
                let wanted_int = classes.iter().filter(|c| **c == Eightbyte::Integer).count();
                let wanted_sse = classes.len() - wanted_int;
                // All or nothing: see the type note.
                if self.ints.len() + wanted_int <= GUEST_INT_ARGS
                    && self.sse.len() + wanted_sse <= GUEST_FP_ARGS
                {
                    for (i, class) in classes.iter().enumerate() {
                        let word = eightbytes(raw, i);
                        match class {
                            Eightbyte::Integer => self.ints.push(word),
                            Eightbyte::Sse => self.sse.push(word),
                        }
                    }
                    return;
                }
            }
            GuestClass::Memory => {}
        }
        // On the stack, eight-byte aligned, padded to a whole number of words.
        while !self.stack.len().is_multiple_of(8) {
            self.stack.push(0);
        }
        self.stack.extend_from_slice(raw);
        while !self.stack.len().is_multiple_of(8) {
            self.stack.push(0);
        }
    }
}

/// Rebuild an aggregate's bytes from the scalars the host's convention delivered.
///
/// An HFA arrives as its members, one per `v` register, so each goes back at its own
/// offset; a SysV aggregate arrives as eightbytes, which go back as they came.
fn reassemble(agg: &Aggregate, host: HostClass, words: &[u64]) -> Vec<u8> {
    let mut raw = vec![0u8; agg.size];
    match host {
        HostClass::Hfa { ty, count } => {
            let width = crate::abi::aggregate::width_of(ty);
            for (leaf, word) in agg.leaves.iter().take(count).zip(words) {
                let end = (leaf.offset + width).min(raw.len());
                if leaf.offset < end {
                    raw[leaf.offset..end].copy_from_slice(&word.to_le_bytes()[..end - leaf.offset]);
                }
            }
        }
        HostClass::Integer { words: n } => {
            for i in 0..n {
                let start = i * 8;
                let end = (start + 8).min(raw.len());
                if start < end {
                    let word = words.get(i).copied().unwrap_or(0);
                    raw[start..end].copy_from_slice(&word.to_le_bytes()[..end - start]);
                }
            }
        }
        // One word per eightbyte, in order, whatever the members inside it are.
        class @ HostClass::Eightbytes { .. } => {
            for (i, _) in class.slots().into_iter().enumerate() {
                let start = i * 8;
                let end = (start + 8).min(raw.len());
                if start < end {
                    let word = words.get(i).copied().unwrap_or(0);
                    raw[start..end].copy_from_slice(&word.to_le_bytes()[..end - start]);
                }
            }
        }
        // Copied out of the caller's stack area, so the words are already the
        // aggregate's own eightbytes. Reached only where an indirect parameter
        // was accepted in the first place.
        HostClass::Indirect => {
            for (i, word) in words.iter().enumerate() {
                let start = i * 8;
                let end = (start + 8).min(raw.len());
                if start < end {
                    raw[start..end].copy_from_slice(&word.to_le_bytes()[..end - start]);
                }
            }
        }
    }
    raw
}

/// Rebuild the guest's ABI argument registers and stack from the exact
/// host-side words spilled by the typed trampoline, or say why they cannot be.
fn guest_arguments(
    signature: &ThunkSignature,
    spill: &[u64],
    result_buffer: &mut [u8; RESULT_BYTES],
) -> Result<GuestArgs, Unsupported> {
    let mut args = GuestArgs::default();
    if let Some(Shape::Struct(agg)) = signature.returns.as_ref()
        && agg.guest_class() == GuestClass::Memory
    {
        if agg.size > RESULT_BYTES {
            return Err("an aggregate return larger than this runtime buffers");
        }
        // The shared guest/host address space lets a guest return into this
        // local as though the caller had passed a hidden sret pointer.
        args.push_int(result_buffer.as_mut_ptr() as usize as u64);
    }
    let mut at = 0usize;
    for (shape, leaves) in signature.params.iter().zip(&signature.host_leaves) {
        let words = spill
            .get(at..at + leaves.len())
            .ok_or("fewer spilled words than the signature describes")?;
        at += leaves.len();
        match shape {
            Shape::Scalar(t) if crate::abi::aggregate::is_fp(*t) => args.push_sse(words[0]),
            Shape::Scalar(_) => args.push_int(words[0]),
            Shape::Struct(agg) => {
                let host = agg.host_class()?;
                let raw = reassemble(agg, host, words);
                args.push_aggregate(agg, &raw);
            }
        }
    }
    Ok(args)
}

/// The aggregate a returning guest call left behind, as its own bytes.
///
/// `buffer` is what a MEMORY-class return was given to fill; for the register
/// classes it is untouched and the eightbytes come back by class, `rax`/`rdx` for
/// the integer ones and `xmm0`/`xmm1` for the SSE ones.
fn returned_aggregate(agg: &Aggregate, regs: &crate::vm::Returned, buffer: &[u8]) -> Vec<u8> {
    let classes = match agg.guest_class() {
        GuestClass::Memory => return buffer[..agg.size.min(buffer.len())].to_vec(),
        GuestClass::Registers(classes) => classes,
    };
    let mut raw = vec![0u8; classes.len() * 8];
    let (mut ints, mut sse) = (0usize, 0usize);
    for (i, class) in classes.iter().enumerate() {
        let word = match class {
            Eightbyte::Integer => {
                ints += 1;
                match ints {
                    1 => regs.rax,
                    _ => regs.rdx,
                }
            }
            Eightbyte::Sse => {
                sse += 1;
                match sse {
                    1 => regs.xmm0,
                    _ => regs.xmm1,
                }
            }
        };
        raw[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    raw.truncate(agg.size);
    raw
}

/// What a typed trampoline calls. Not public: its only legitimate caller is code this
/// module emitted.
unsafe extern "C" fn typed_entry(
    guest_va: u64,
    desc: *const ThunkSignature,
    spill: *const u64,
    out: *mut u8,
) {
    // SAFETY: `out` is a `RESULT_BYTES` stack slot of the calling trampoline.
    unsafe { std::ptr::write_bytes(out, 0, RESULT_BYTES) };
    let Some(vm) = current() else {
        report_no_vm(guest_va);
        return;
    };
    if !vm.program.is_guest_address(guest_va) {
        return;
    }
    // SAFETY: `desc` is owned by the VM's thunk and remains valid while the
    // host can call this trampoline; `spill` points at exactly as many words as
    // `desc.host_leaves` flattens to.
    let (signature, spill) = unsafe {
        let signature = &*desc;
        let n: usize = signature.host_leaves.iter().map(Vec::len).sum();
        (signature, std::slice::from_raw_parts(spill, n))
    };

    let mut buffer = [0u8; RESULT_BYTES];
    // A refusal, not a zeroed return the host would read as the callback
    // having run.
    let args = match guest_arguments(signature, spill, &mut buffer) {
        Ok(args) => args,
        Err(reason) => {
            park_or_die(
                vm,
                VmError::Thunk {
                    guest: guest_va,
                    reason: format!("the callback's arguments cannot be placed: {reason}"),
                },
            );
            return;
        }
    };

    let mut ctx = RegisterContext::new();
    for (slot, &v) in [
        &mut ctx.rdi,
        &mut ctx.rsi,
        &mut ctx.rdx,
        &mut ctx.rcx,
        &mut ctx.r8,
        &mut ctx.r9,
    ]
    .into_iter()
    .zip(&args.ints)
    {
        *slot = v;
    }
    for (i, &bits) in args.sse.iter().enumerate() {
        ctx.xmm[i][0] = bits;
    }
    // `al` is the count of vector registers used, which a variadic callee reads
    // and a fixed one ignores. Costs nothing and is what the ABI says.
    ctx.rax = args.sse.len() as u64;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        vm.call_with_stack(guest_va, ctx, &args.stack)
    }));
    match result {
        Ok(Ok(regs)) => {
            // Which register holds the answer is the *return type's* question,
            // not something to infer from the value.
            let raw = match signature.returns.as_ref() {
                Some(Shape::Struct(agg)) => returned_aggregate(agg, &regs, &buffer),
                other => {
                    let value = match other.and_then(|s| s.scalar()) {
                        Some(t) if crate::abi::aggregate::is_fp(t) => regs.xmm0,
                        _ => regs.rax,
                    };
                    value.to_le_bytes().to_vec()
                }
            };
            let n = raw.len().min(RESULT_BYTES);
            // SAFETY: `out` is the calling trampoline's `RESULT_BYTES` slot, and
            // the members it loads sit at the offsets `return_leaves` named -
            // which are the aggregate's own, so its bytes go across unrearranged.
            unsafe { std::ptr::copy_nonoverlapping(raw.as_ptr(), out, n) };
        }
        Ok(Err(e)) => park_or_die(vm, in_callback(guest_va, e)),
        Err(_) => park_or_die(
            vm,
            VmError::Thunk {
                guest: guest_va,
                reason: "the guest callback panicked the runtime".into(),
            },
        ),
    }
}

/// The status a callback failure nothing can raise ends the process with.
const CALLBACK_FAILURE_STATUS: i32 = 70;

/// Park a callback's failure - or, when nothing will ever take it, say so and stop the
/// process.
fn park_or_die(vm: &Vm, err: VmError) {
    if crate::thread::current().is_some_and(|s| s.depth.get() > 0) {
        vm.set_pending(err);
        return;
    }
    // Said here rather than through `set_pending`, which deduplicates and caps:
    // the last thing printed before the process ends may not be suppressed.
    eprintln!("rosemary: {err}");
    eprintln!(
        "rosemary: that was the outermost guest frame on this thread, so nothing \
         will raise it - the callback would return to the host and this thread \
         would go quiet. Exiting."
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // SAFETY: `_exit` ends the process immediately. No invariant of this
    // runtime has to survive it, which is the point of choosing it.
    unsafe { libc::_exit(CALLBACK_FAILURE_STATUS) };
}

/// Name a failure by the callback it happened in - once.
fn in_callback(guest_va: u64, e: VmError) -> VmError {
    match e {
        already @ VmError::Callback { .. } => already,
        e => VmError::Callback {
            guest: guest_va,
            reason: e.to_string(),
        },
    }
}

/// Said once, from either entry: a callback ran with no VM to enter.
fn report_no_vm(guest_va: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosemary: a guest callback at {guest_va:#x} ran with no VM reachable; \
             an embedder whose guest spawns threads or registers `atexit` handlers \
             must call `rosemary::thunk::publish`"
        );
    });
}

/// What every thunk calls. Not public: its only legitimate caller is code this module
/// emitted.
///
/// An error here cannot be returned - the host function in the middle (`qsort`, say)
/// has no way to express one - so it is parked on the VM and raised the moment control
/// is back in the runtime.
unsafe extern "C" fn thunk_entry(guest_va: u64, ints: *const u64, floats: *const f64) -> u64 {
    let Some(vm) = current() else {
        // Nowhere to report this - the caller is `qsort`, or libpthread's
        // thread start, and neither has an error channel - so it is said out
        // loud once and the callback returns zero.
        report_no_vm(guest_va);
        return 0;
    };

    // The VM that answered may not be the one that built this thunk.
    if !vm.program.is_guest_address(guest_va) {
        return 0;
    }

    // SAFETY: both point at stack slots the calling thunk just filled, sized
    // `GUEST_INT_ARGS` and `GUEST_FP_ARGS` words by the same code that emits
    // the call.
    let (ints, floats) = unsafe {
        (
            std::slice::from_raw_parts(ints, GUEST_INT_ARGS),
            std::slice::from_raw_parts(floats, GUEST_FP_ARGS),
        )
    };

    let mut ctx = RegisterContext::new();
    ctx.rdi = ints[0];
    ctx.rsi = ints[1];
    ctx.rdx = ints[2];
    ctx.rcx = ints[3];
    ctx.r8 = ints[4];
    ctx.r9 = ints[5];
    for (i, &f) in floats.iter().enumerate() {
        // The low half of `xmm_i`, which is where a `double` argument lives.
        ctx.xmm[i][0] = f.to_bits();
    }

    match std::panic::catch_unwind(AssertUnwindSafe(|| vm.call_with(guest_va, ctx))) {
        Ok(Ok(rax)) => rax,
        Ok(Err(e)) => {
            park_or_die(vm, in_callback(guest_va, e));
            0
        }
        Err(_) => {
            park_or_die(
                vm,
                VmError::Thunk {
                    guest: guest_va,
                    reason: "the guest callback panicked the runtime".into(),
                },
            );
            0
        }
    }
}

#[cfg(test)]
mod callback_abi_tests {
    use super::*;

    #[test]
    fn video_toolbox_signature_has_two_indirect_times() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        assert_eq!(signature.params.len(), 7);
        assert!(signature.returns.is_none());
        assert_eq!(
            signature
                .host_leaves
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [1, 1, 1, 1, 1, 3, 3]
        );
        let incoming = signature.incoming();
        assert_eq!(incoming.len(), 7);
        assert!(matches!(
            incoming[5],
            Incoming::Aggregate { word: 5, size: 24 }
        ));
        assert!(matches!(
            incoming[6],
            Incoming::Aggregate { word: 8, size: 24 }
        ));
        for shape in &signature.params[5..] {
            let Shape::Struct(agg) = shape else {
                panic!("CMTime must be an aggregate")
            };
            assert_eq!(agg.size, 24);
            assert_eq!(agg.align, 8);
            assert_eq!(agg.guest_class(), GuestClass::Memory);
            assert_eq!(agg.host_class(), Ok(HostClass::Indirect));
        }
    }

    #[test]
    fn video_toolbox_two_times_reach_guest_stack_untruncated() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        let first = [
            0x0123_4567_89ab_cdef,
            0x1234_5678_9abc_def0,
            0xfedc_ba98_7654_3210,
        ];
        let second = [
            0x7fff_ffff_ffff_fffe,
            0xabcd_1234_0000_0001,
            0x8000_0000_0000_0001,
        ];
        let spill = [
            0x1001,
            0x2002,
            (-129i64) as u64,
            0x8000_0012,
            0x5005,
            first[0],
            first[1],
            first[2],
            second[0],
            second[1],
            second[2],
        ];
        let mut buffer = [0; RESULT_BYTES];
        let guest = guest_arguments(&signature, &spill, &mut buffer).expect("supported ABI");
        assert_eq!(guest.ints, spill[..5]);
        assert!(guest.sse.is_empty());
        let expected: Vec<u8> = first
            .into_iter()
            .chain(second)
            .flat_map(u64::to_le_bytes)
            .collect();
        assert_eq!(guest.stack.len(), 48);
        assert_eq!(guest.stack, expected);
    }

    #[test]
    fn a_register_after_two_stack_times_keeps_its_guest_slot() {
        let mut sig = crate::shims::coremedia::decompression_output_signature();
        sig.params.push(Shape::Scalar(T::UnsignedDoubleword));
        sig.host_leaves.push(vec![T::UnsignedDoubleword]);
        let spill = [10, 20, 30, 40, 50, 1, 2, 3, 4, 5, 6, 70];
        let mut buffer = [0; RESULT_BYTES];
        let guest = guest_arguments(&sig, &spill, &mut buffer).unwrap();
        assert_eq!(guest.ints, [10, 20, 30, 40, 50, 70]);
        assert_eq!(guest.stack.len(), 48);
    }

    #[test]
    fn a_short_spill_is_refused_rather_than_zeroed() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        let mut buffer = [0; RESULT_BYTES];
        let refused = guest_arguments(&signature, &[1, 2, 3], &mut buffer);
        assert!(refused.is_err());
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NativeTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    thread_local! {
        static CAPTURED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn capture_spill(
        _guest: u64,
        _desc: *const ThunkSignature,
        spill: *const u64,
        _out: *mut u8,
    ) {
        // SAFETY: this test constructs the trampoline with exactly eleven
        // spill words, and the callback runs synchronously before it returns.
        let words = unsafe { std::slice::from_raw_parts(spill, 11) };
        CAPTURED.with(|slot| *slot.borrow_mut() = words.to_vec());
    }

    #[test]
    fn native_c_callback_thunk_carries_both_times_in_full() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        let thunk = build_typed_with_entry(
            0x1234,
            signature.clone(),
            capture_spill as *const () as usize as u64,
        )
        .expect("native ABI supports the callback");
        let owned_descriptor = std::ptr::from_ref(thunk._signature.as_deref().unwrap());
        let mut table = ThunkTable::new();
        let (host, _) = table.insert_typed(signature.clone(), thunk);
        let stored = &table.typed_by_guest[&0x1234][0].1;
        assert_eq!(
            owned_descriptor,
            std::ptr::from_ref(stored._signature.as_deref().unwrap())
        );
        assert_eq!(table.typed_host_for(0x1234, &signature), Some(host));
        let first = NativeTime {
            value: 0x0123_4567_89ab_cdef,
            timescale: 600,
            flags: 0x1234_5678,
            epoch: -8,
        };
        let second = NativeTime {
            value: -0x0123_4567_890a_bcde,
            timescale: -300,
            flags: 0x9876_5432,
            epoch: 0x1122_3344_5566_7788,
        };
        assert_eq!(size_of::<NativeTime>(), 24);
        // SAFETY: the generated trampoline declares precisely this host
        // callback signature, with two native CMTime-layout arguments.
        let call: extern "C" fn(u64, u64, i32, u32, u64, NativeTime, NativeTime) =
            unsafe { std::mem::transmute(host as usize) };
        call(11, 22, -33, 44, 55, first, second);
        let captured = CAPTURED.with(|slot| slot.borrow().clone());
        assert_eq!(captured[0], 11);
        assert_eq!(captured[1], 22);
        assert_eq!(captured[3] as u32, 44);
        assert_eq!(captured[4], 55);
        // Signed 32-bit callback parameters only promise their low 32 bits.
        assert_eq!(captured[2] as u32 as i32, -33);
        let mut expected = Vec::new();
        for time in [first, second] {
            expected.extend_from_slice(&[
                time.value as u64,
                u64::from(time.timescale as u32) | (u64::from(time.flags) << 32),
                time.epoch as u64,
            ]);
        }
        assert_eq!(&captured[5..], expected.as_slice());
        let mut buffer = [0; RESULT_BYTES];
        let guest = guest_arguments(&signature, &captured, &mut buffer).unwrap();
        assert_eq!(guest.ints.len(), 5);
        let expected_bytes: Vec<u8> = expected.into_iter().flat_map(u64::to_le_bytes).collect();
        assert_eq!(guest.stack, expected_bytes);
    }

    #[test]
    fn typed_and_generic_thunks_cannot_be_confused_in_the_table() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        let typed = build_typed(0x1234, signature.clone()).unwrap();
        let generic = build(0x1234).unwrap();
        let mut table = ThunkTable::new();
        let (typed_host, _) = table.insert_typed(signature.clone(), typed);
        let (generic_host, _) = table.insert(generic);
        assert_ne!(typed_host, generic_host);
        assert_eq!(table.typed_host_for(0x1234, &signature), Some(typed_host));
        assert_eq!(table.host_for(0x1234), Some(generic_host));
        assert_eq!(table.guest_for(typed_host), Some(0x1234));
        assert_eq!(table.guest_for(generic_host), Some(0x1234));
        assert_eq!(table.len(), 2);
        assert_eq!(table.view().count, 2);
    }

    #[test]
    fn typed_cache_distinguishes_exact_signatures_for_one_guest_address() {
        let original = crate::shims::coremedia::decompression_output_signature();
        let mut changed_params = original.params.clone();
        // A different signedness is still a different callback prototype even
        // if the two machine register layouts happen to be identical.
        changed_params[3] = Shape::Scalar(T::SignedWord);
        let changed = ThunkSignature::new(changed_params, None).unwrap();
        let mut table = ThunkTable::new();
        let (first, _) = table.insert_typed(
            original.clone(),
            build_typed(0x1234, original.clone()).unwrap(),
        );
        assert_eq!(table.typed_host_for(0x1234, &original), Some(first));
        assert_eq!(table.typed_host_for(0x1234, &original.clone()), Some(first));
        assert_eq!(table.typed_host_for(0x1234, &changed), None);
        let (second, _) = table.insert_typed(
            changed.clone(),
            build_typed(0x1234, changed.clone()).unwrap(),
        );
        assert_ne!(first, second);
        assert_eq!(table.typed_host_for(0x1234, &changed), Some(second));
        assert_eq!(table.guest_for(first), Some(0x1234));
        assert_eq!(table.guest_for(second), Some(0x1234));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn a_racing_duplicate_thunk_does_not_replace_the_first() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        let mut table = ThunkTable::new();
        let (generic, kept) = table.insert(build(0x1234).unwrap());
        assert!(kept);
        let (again, kept) = table.insert(build(0x1234).unwrap());
        assert!(!kept);
        assert_eq!(again, generic);
        let (typed, kept) = table.insert_typed(
            signature.clone(),
            build_typed(0x1234, signature.clone()).unwrap(),
        );
        assert!(kept);
        let (again, kept) =
            table.insert_typed(signature.clone(), build_typed(0x1234, signature).unwrap());
        assert!(!kept);
        assert_eq!(again, typed);
        // Only the kept thunks are reachable from their host addresses.
        assert_eq!(table.len(), 2);
        assert_eq!(table.view().count, 2);
    }

    thread_local! {
        static REENTER_THUNK: Cell<u64> = const { Cell::new(0) };
        static REENTER_DEPTH: Cell<usize> = const { Cell::new(0) };
        static REENTER_SPILLS: std::cell::RefCell<Vec<(usize, Vec<u64>)>> = const {
            std::cell::RefCell::new(Vec::new())
        };
    }

    unsafe extern "C" fn capture_reentrant(
        _guest: u64,
        _desc: *const ThunkSignature,
        spill: *const u64,
        _out: *mut u8,
    ) {
        // SAFETY: the seven-argument callback always has eleven spill words.
        let words = unsafe { std::slice::from_raw_parts(spill, 11) }.to_vec();
        let depth = REENTER_DEPTH.with(|slot| slot.replace(slot.get() + 1));
        if depth == 0 {
            let host = REENTER_THUNK.with(Cell::get);
            // SAFETY: this is the same typed trampoline currently executing.
            let call: extern "C" fn(u64, u64, i32, u32, u64, NativeTime, NativeTime) =
                unsafe { std::mem::transmute(host as usize) };
            let time = NativeTime {
                value: 111,
                timescale: 222,
                flags: 333,
                epoch: 444,
            };
            call(10, 20, 30, 40, 50, time, time);
        }
        REENTER_SPILLS.with(|slot| slot.borrow_mut().push((depth, words)));
        REENTER_DEPTH.with(|slot| slot.set(depth));
    }

    #[test]
    fn typed_callback_trampoline_can_reenter_itself_without_clobbering_spills() {
        let signature = crate::shims::coremedia::decompression_output_signature();
        let thunk = build_typed_with_entry(
            0x1234,
            signature,
            capture_reentrant as *const () as usize as u64,
        )
        .unwrap();
        REENTER_THUNK.with(|slot| slot.set(thunk.host));
        REENTER_SPILLS.with(|slot| slot.borrow_mut().clear());
        // SAFETY: the generated thunk declares this exact native callback ABI.
        let call: extern "C" fn(u64, u64, i32, u32, u64, NativeTime, NativeTime) =
            unsafe { std::mem::transmute(thunk.host as usize) };
        let outer = NativeTime {
            value: 555,
            timescale: 666,
            flags: 777,
            epoch: 888,
        };
        call(1, 2, 3, 4, 5, outer, outer);
        let samples = REENTER_SPILLS.with(|slot| slot.borrow().clone());
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].0, 1);
        assert_eq!(samples[1].0, 0);
        assert_eq!(&samples[0].1[..5], &[10, 20, 30, 40, 50]);
        assert_eq!(&samples[1].1[..5], &[1, 2, 3, 4, 5]);
        assert_eq!(samples[0].1[5], 111);
        assert_eq!(samples[0].1[7], 444);
        assert_eq!(samples[1].1[5], 555);
        assert_eq!(samples[1].1[7], 888);
        REENTER_THUNK.with(|slot| slot.set(0));
    }

    // This fixture needs no Apple SDK or imported libraries. On macOS, the
    // system compiler can link it; on Linux, Clang and ld64.lld can do so.
    /// A scratch directory removed when the test ends, pass or fail.
    struct FixtureDir(std::path::PathBuf);

    impl FixtureDir {
        fn new() -> Option<Self> {
            let dir =
                std::env::temp_dir().join(format!("rosemary-callback-abi-{}", std::process::id()));
            std::fs::create_dir_all(&dir).ok()?;
            Some(FixtureDir(dir))
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn macho_callback_fixture(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        use std::process::Command;

        const SOURCE: &str = r#"
            typedef unsigned long long U64;
            typedef struct { long long value; int timescale; unsigned flags; long long epoch; } Time;
            typedef void (*Callback)(void*, void*, int, unsigned, void*, Time, Time);
            static volatile U64 observed[2][13];
            __attribute__((used,noinline)) U64 guest_callback(void* refcon, void* frame, int status,
                                                unsigned flags, void* image, Time pts, Time duration) {
                if ((U64)refcon == 0xfeedfaceULL)
                    return observed[(U64)frame / 13][(U64)frame % 13];
                volatile U64* row = observed[(U64)refcon == 101 ? 1 : 0];
                row[0] = (U64)refcon;
                row[1] = (U64)frame;
                row[2] = (U64)(unsigned)status;
                row[3] = flags;
                row[4] = (U64)image;
                row[5] = (U64)pts.value;
                row[6] = (U64)(unsigned)pts.timescale;
                row[7] = pts.flags;
                row[8] = (U64)pts.epoch;
                row[9] = (U64)duration.value;
                row[10] = (U64)(unsigned)duration.timescale;
                row[11] = duration.flags;
                row[12] = (U64)duration.epoch;
                if ((U64)refcon == 11) {
                    Time a = {-999, 123, 0xff000011U, 4444};
                    Time b = {4321, -432, 0x12345678U, -123};
                    ((Callback)frame)((void*)101, (void*)202, -303, 404, (void*)505, a, b);
                }
                return 0;
            }
        "#;
        let src = dir.join("callback.c");
        let obj = dir.join("callback.o");
        let exe = dir.join("callback");
        std::fs::write(&src, SOURCE).ok()?;
        if cfg!(target_os = "macos") {
            let status = Command::new("cc")
                .args(["-arch", "x86_64", "-O1", "-Wl,-e,_guest_callback", "-o"])
                .arg(&exe)
                .arg(&src)
                .status()
                .ok()?;
            return status.success().then_some(exe);
        }
        let compiled = Command::new("clang")
            .args([
                "-target",
                "x86_64-apple-macos10.15",
                "-O1",
                "-fno-stack-protector",
                "-c",
                "-o",
            ])
            .arg(&obj)
            .arg(&src)
            .status()
            .ok()?;
        if !compiled.success() {
            return None;
        }
        let linker =
            std::env::var_os("ROSEMARY_TEST_LD64_LLD").unwrap_or_else(|| "ld64.lld".into());
        let linked = Command::new(linker)
            .args([
                "-arch",
                "x86_64",
                "-platform_version",
                "macos",
                "10.15",
                "10.15",
                "-e",
                "_guest_callback",
                "-o",
            ])
            .arg(&exe)
            .arg(&obj)
            .status()
            .ok()?;
        linked.success().then_some(exe)
    }

    // A Clang-compiled caller, rather than Rust's extern "C" lowering,
    // validates the host macOS ABI at the generated callee boundary.
    #[cfg(target_os = "macos")]
    fn native_c_caller_fixture(dir: &std::path::Path) -> Option<(*mut libc::c_void, usize)> {
        use std::ffi::CString;
        use std::process::Command;

        const SOURCE: &str = r#"
            typedef struct { long long value; int timescale; unsigned flags; long long epoch; } Time;
            typedef void (*Callback)(void*, void*, int, unsigned, void*, Time, Time);
            __attribute__((visibility("default"))) void invoke_guest_callback(void *thunk, int which) {
                Time first = { 0x0123456789abcdefLL, -360, 0xabcd1234U, -555 };
                Time second = { -0x1234567890abcdeLL, 777, 0x80000001U, 0x1122334455667788LL };
                Callback cb = (Callback)thunk;
                if (!which) {
                    cb((void*)11, thunk, -33, 0x87654321U, (void*)55, first, second);
                } else {
                    cb((void*)12, (void*)23, -34, 0x13572468U, (void*)56, second, first);
                }
            }
        "#;
        let source = dir.join("native-c-caller.c");
        let library = dir.join("native-c-caller.dylib");
        std::fs::write(&source, SOURCE).expect("write C callback caller");
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x86_64",
            other => panic!("unsupported macOS callback test host architecture: {other}"),
        };
        let status = Command::new("cc")
            .args(["-arch", arch, "-dynamiclib", "-o"])
            .arg(&library)
            .arg(&source)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let dylib_path = CString::new(library.to_str().unwrap()).unwrap();
        // SAFETY: path and symbol are valid NUL-terminated strings.
        let handle = unsafe { libc::dlopen(dylib_path.as_ptr(), libc::RTLD_NOW) };
        assert!(!handle.is_null(), "load native macOS C callback caller");
        let symbol = CString::new("invoke_guest_callback").unwrap();
        // SAFETY: the library owns this exported function until dlclose below.
        let entry = unsafe { libc::dlsym(handle, symbol.as_ptr()) };
        assert!(!entry.is_null(), "find native macOS callback caller entry");
        Some((handle, entry as usize))
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "requires Clang and ld64.lld: run explicitly with --ignored and ROSEMARY_TEST_LD64_LLD"
    )]
    fn native_callback_enters_a_real_emulated_macho_guest_with_full_times() {
        // Runs by default only on macOS, where a missing x86_64 toolchain is the
        // host's limit and not a failure. Elsewhere it runs only when asked for.
        let dir = FixtureDir::new();
        let Some(path) = dir.as_ref().and_then(|d| macho_callback_fixture(&d.0)) else {
            if !cfg!(target_os = "macos") {
                panic!(
                    "Mach-O fixture compilation failed: provide Clang and ROSEMARY_TEST_LD64_LLD"
                );
            }
            eprintln!("skipping: cannot build an x86_64 Mach-O fixture");
            return;
        };
        let path = path.to_str().unwrap();
        let program = crate::Program::load_executable(path, &[]).expect("load guest Mach-O");
        let entry = program.report.root().unwrap().entry.expect("Mach-O entry");
        let vm = crate::Vm::new(program, path, &[]).expect("guest VM");
        // Counted from here, so thunks the VM makes for itself do not matter.
        let thunks_before = vm.thunks();
        let signature = crate::shims::coremedia::decompression_output_signature();
        let host = vm
            .typed_thunk_for(entry, signature.clone())
            .expect("typed guest callback");

        // Guest-visible callback values may contain a native thunk created
        // earlier for a generic or incompatible signature. Re-substituting
        // them must recover the same guest entry and use the requested ABI.
        use crate::abi::Callbacks as _;
        let generic = vm.thunk_for(entry).expect("generic guest callback");
        assert_ne!(generic, host);
        assert_eq!(
            vm.substitute_typed("roundtrip", generic, signature.clone())
                .unwrap(),
            host
        );
        assert_eq!(
            vm.substitute_typed("roundtrip", host, signature.clone())
                .unwrap(),
            host
        );
        let mut alternate_params = signature.params.clone();
        alternate_params[3] = Shape::Scalar(T::SignedWord);
        let alternate = ThunkSignature::new(alternate_params, None).unwrap();
        let other_host = vm
            .substitute_typed("roundtrip", host, alternate.clone())
            .expect("alternate typed guest callback");
        assert_ne!(other_host, host);
        assert_ne!(other_host, generic);
        assert_eq!(
            vm.substitute_typed("roundtrip", generic, alternate.clone())
                .unwrap(),
            other_host
        );
        assert_eq!(
            vm.substitute_typed("roundtrip", other_host, signature.clone())
                .unwrap(),
            host
        );
        assert_eq!(
            vm.substitute_typed("roundtrip", other_host, alternate.clone())
                .unwrap(),
            other_host
        );
        assert_eq!(vm.guest_behind_thunk(other_host), Some(entry));
        // And back: a typed thunk handed to a generic callback slot becomes
        // the generic thunk, never a typed one called with generic registers.
        assert_eq!(vm.substitute("roundtrip", host).unwrap(), generic);
        assert_eq!(vm.substitute("roundtrip", other_host).unwrap(), generic);
        assert_eq!(vm.substitute("roundtrip", generic).unwrap(), generic);
        assert_eq!(vm.thunks() - thunks_before, 3);
        assert_eq!(
            vm.substitute_typed("roundtrip", 0, alternate.clone())
                .unwrap(),
            0
        );
        let native = std::ptr::from_ref(&vm) as u64;
        assert_eq!(
            vm.substitute_typed("roundtrip", native, alternate).unwrap(),
            native
        );

        // SAFETY: the C caller has exactly the trampoline's native signature.
        #[cfg(not(target_os = "macos"))]
        let callback: extern "C" fn(u64, u64, i32, u32, u64, NativeTime, NativeTime) =
            unsafe { std::mem::transmute(host as usize) };
        #[cfg(target_os = "macos")]
        let Some((native_c_handle, native_c_addr)) =
            native_c_caller_fixture(std::path::Path::new(path).parent().unwrap())
        else {
            eprintln!("skipping: cannot build the native C callback caller");
            return;
        };
        #[cfg(target_os = "macos")]
        let native_c_call: unsafe extern "C" fn(u64, i32) =
            unsafe { std::mem::transmute(native_c_addr) };
        let first = NativeTime {
            value: 0x0123_4567_89ab_cdef,
            timescale: -360,
            flags: 0xabcd_1234,
            epoch: -555,
        };
        let second = NativeTime {
            value: -0x0123_4567_890a_bcde,
            timescale: 777,
            flags: 0x8000_0001,
            epoch: 0x1122_3344_5566_7788,
        };
        #[cfg(target_os = "macos")]
        with_current(&vm, || unsafe { native_c_call(host, 0) });
        #[cfg(not(target_os = "macos"))]
        with_current(&vm, || {
            callback(11, host, -33, 0x8765_4321, 55, first, second)
        });
        let expected = [
            11,
            host,
            (-33i32 as u32) as u64,
            0x8765_4321,
            55,
            first.value as u64,
            (first.timescale as u32) as u64,
            first.flags as u64,
            first.epoch as u64,
            second.value as u64,
            (second.timescale as u32) as u64,
            second.flags as u64,
            second.epoch as u64,
        ];
        for (index, want) in expected.iter().enumerate() {
            let got = vm
                .call(entry, &[0xfeedface, index as u64])
                .expect("read guest capture");
            assert_eq!(got, *want, "outer guest CMTime word {index}");
        }
        let nested = [
            101,
            202,
            (-303i32 as u32) as u64,
            404,
            505,
            (-999i64) as u64,
            123,
            0xff00_0011,
            4444,
            4321,
            (-432i32 as u32) as u64,
            0x1234_5678,
            (-123i64) as u64,
        ];
        for (index, want) in nested.iter().enumerate() {
            let got = vm
                .call(entry, &[0xfeedface, (13 + index) as u64])
                .expect("read nested guest capture");
            assert_eq!(got, *want, "nested guest CMTime word {index}");
        }

        // A host callback thread has no preexisting guest stack or VM TLS.
        // Enter the same trampoline from a second native thread and check
        // every field again after that thread has exited.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    #[cfg(target_os = "macos")]
                    with_current(&vm, || unsafe { native_c_call(host, 1) });
                    #[cfg(not(target_os = "macos"))]
                    with_current(&vm, || {
                        callback(12, 23, -34, 0x1357_2468, 56, second, first)
                    });
                })
                .join()
                .expect("native callback worker");
        });
        let threaded = [
            12,
            23,
            (-34i32 as u32) as u64,
            0x1357_2468,
            56,
            second.value as u64,
            (second.timescale as u32) as u64,
            second.flags as u64,
            second.epoch as u64,
            first.value as u64,
            (first.timescale as u32) as u64,
            first.flags as u64,
            first.epoch as u64,
        ];
        for (index, want) in threaded.iter().enumerate() {
            let got = vm
                .call(entry, &[0xfeedface, index as u64])
                .expect("read threaded guest capture");
            assert_eq!(got, *want, "threaded guest CMTime word {index}");
        }
        #[cfg(target_os = "macos")]
        assert_eq!(unsafe { libc::dlclose(native_c_handle) }, 0);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NativeTwentyFive([u8; 25]);

    unsafe extern "C" fn capture_partial(
        _guest: u64,
        desc: *const ThunkSignature,
        spill: *const u64,
        _out: *mut u8,
    ) {
        // SAFETY: the trampoline owns the descriptor and the synchronous
        // spill slot it passes here has exactly the declared word count.
        let words = unsafe {
            let count: usize = (*desc).host_leaves.iter().map(Vec::len).sum();
            std::slice::from_raw_parts(spill, count)
        };
        CAPTURED.with(|slot| *slot.borrow_mut() = words.to_vec());
    }

    #[test]
    fn partial_final_aggregate_word_does_not_copy_padding() {
        let signature = ThunkSignature::new(
            vec![Shape::Struct(Aggregate {
                size: 25,
                align: 1,
                leaves: vec![crate::abi::aggregate::Leaf {
                    ty: T::UnsignedByte,
                    offset: 0,
                }],
            })],
            None,
        )
        .unwrap();
        let thunk = build_typed_with_entry(
            0x5678,
            signature.clone(),
            capture_partial as *const () as usize as u64,
        )
        .unwrap();
        let mut bytes = [0u8; 25];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = i as u8 + 1;
        }
        // SAFETY: the generated function declares one 25-byte by-value C
        // aggregate, which the host ABI passes in memory.
        let call: extern "C" fn(NativeTwentyFive) =
            unsafe { std::mem::transmute(thunk.host as usize) };
        call(NativeTwentyFive(bytes));
        let captured = CAPTURED.with(|slot| slot.borrow().clone());
        assert_eq!(captured.len(), 4);
        // A SysV host loads the whole padded eightbyte, so only the object's
        // own byte is meaningful there.
        assert_eq!(captured[3] & 0xff, 25);
        if cfg!(target_arch = "aarch64") {
            assert_eq!(captured[3], 25);
        }
        let mut buffer = [0u8; RESULT_BYTES];
        let guest = guest_arguments(&signature, &captured, &mut buffer).unwrap();
        assert_eq!(&guest.stack[..25], &bytes);
        assert_eq!(&guest.stack[25..], &[0; 7]);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NativeMixed {
        a: i64,
        b: f64,
        c: i32,
        d: i64,
    }

    // ObjC methods reach `build_typed` through their decoded type encoding, and
    // one taking a large non-HFA struct is passed by pointer on AAPCS64 and on
    // the stack on SysV. The argument after it must keep its own slot either way.
    #[test]
    fn an_objc_method_with_a_large_struct_argument_reaches_the_guest_intact() {
        let method = machish::extensions::objc::ObjcMethod {
            name: "takeMixed:after:".into(),
            types: "v48@0:8{Mixed=qdiq}16q48".into(),
            imp: 0x9abc,
        };
        let signature = crate::objc::thunk_signature(&method).expect("supported encoding");
        let thunk = build_typed_with_entry(
            0x9abc,
            signature.clone(),
            capture_partial as *const () as usize as u64,
        )
        .unwrap();
        let mixed = NativeMixed {
            a: -0x0102_0304_0506_0708,
            b: 1.5,
            c: -9,
            d: 0x1122_3344_5566_7788,
        };
        // SAFETY: the trampoline was built from exactly this method's encoding.
        let call: extern "C" fn(u64, u64, NativeMixed, i64) =
            unsafe { std::mem::transmute(thunk.host as usize) };
        call(0x5e1f, 0xc0de, mixed, -77);
        let captured = CAPTURED.with(|slot| slot.borrow().clone());
        let mut buffer = [0u8; RESULT_BYTES];
        let guest = guest_arguments(&signature, &captured, &mut buffer).unwrap();
        assert_eq!(guest.ints, [0x5e1f, 0xc0de, (-77i64) as u64]);
        assert!(guest.sse.is_empty());
        let mut expected = Vec::new();
        expected.extend_from_slice(&mixed.a.to_le_bytes());
        expected.extend_from_slice(&mixed.b.to_le_bytes());
        expected.extend_from_slice(&mixed.c.to_le_bytes());
        expected.extend_from_slice(&[0; 4]);
        expected.extend_from_slice(&mixed.d.to_le_bytes());
        assert_eq!(guest.stack.len(), 32);
        // The padding after `c` is the caller's; only the members must match.
        assert_eq!(&guest.stack[..20], &expected[..20]);
        assert_eq!(&guest.stack[24..], &expected[24..]);
    }
}
