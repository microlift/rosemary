use iced_x86::Register;

use microlift::arch::x86_64::abi::x86_reg;
use microlift::arch::x86_64::context::RegisterContext;
use microlift::core::abi::{ArgLocation, GuestReg};

use super::aggregate::{self, Eightbyte, GuestClass, HostClass, Shape};
use super::{AbiError, ArgType, FunctionAbi, GuestMemory, Result, guest_call_abi_ref};

/// Host argument registers of each class, per AAPCS64. The count the prototypes below
/// are shaped around.
const MAX_INT_ARGS: usize = 8;

/// Integer argument registers SysV has, past which an argument is on the stack.
///
/// The prototypes below declare twenty-four `u64`, and the compiler places the first six
/// of them in registers and the rest on the stack - so an index into `ints` at or
/// above this is a stack slot, which is what lets an aggregate SysV sends to memory be
/// written there.
#[cfg(target_arch = "x86_64")]
const HOST_INT_REGS: usize = 6;
const MAX_FP_ARGS: usize = 8;

/// Integer arguments the over-wide prototype declares, registers and stack together.
const MAX_INT_ARGS_TOTAL: usize = 24;

/// Whether `ty` occupies a full eight bytes.
#[cfg(target_arch = "aarch64")]
fn is_doubleword(ty: ArgType) -> bool {
    matches!(
        ty,
        ArgType::Pointer | ArgType::SignedDoubleword | ArgType::UnsignedDoubleword
    )
}

/// How many bytes `ty` occupies in a stack slot, which is also its alignment.
#[cfg(target_arch = "aarch64")]
fn stack_width(ty: ArgType) -> u64 {
    match ty {
        ArgType::SignedByte | ArgType::UnsignedByte => 1,
        ArgType::SignedHalfword | ArgType::UnsignedHalfword => 2,
        ArgType::SignedWord | ArgType::UnsignedWord | ArgType::SinglePrecisionFp => 4,
        _ => 8,
    }
}

/// Whether declaring every overflowing argument `u64` puts each of them where Apple's
/// rule puts it.
///
/// Apple packs each stack argument at its own size *and its own alignment*, so a
/// four-byte argument followed by a pointer still leaves the pointer at the next
/// eight-byte boundary - exactly where a `u64` declaration would put it.
#[cfg(target_arch = "aarch64")]
fn widening_preserves_offsets(overflow: &[ArgType]) -> bool {
    let mut packed = 0u64;
    for (slot, ty) in overflow.iter().enumerate() {
        let width = stack_width(*ty);
        packed = packed.next_multiple_of(width);
        if packed != slot as u64 * 8 {
            return false;
        }
        packed += width;
    }
    true
}

/// Whether [`call`] can express this signature at all.
pub fn is_marshallable(abi: &FunctionAbi) -> bool {
    if abi.variadic {
        return false;
    }
    let Some(placed) = guest_call_abi_ref().place_args(abi.types) else {
        return false;
    };
    // Counted by argument *class*, not by where the guest put it.
    let integers: Vec<ArgType> = placed
        .iter()
        .map(|(ty, _)| *ty)
        .filter(|ty| !is_fp(*ty))
        .collect();
    let fps = placed.len() - integers.len();
    if integers.len() > MAX_INT_ARGS_TOTAL || fps > MAX_FP_ARGS {
        return false;
    }
    // Everything the host will pass on its own stack is placed at its own
    // width, by a prototype chosen for the pattern those widths make.
    let ret = abi.types.first().copied().unwrap_or(ArgType::None);
    NarrowTail::of(&integers[MAX_INT_ARGS.min(integers.len())..], is_fp(ret)).is_some()
}

/// The prototype that places a run of overflowing arguments at their own widths.
///
/// SysV gives every stack argument a whole eightbyte, so on that host only the first
/// variant is ever chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
enum NarrowTail {
    /// Every overflowing argument is a full doubleword: the original prototype.
    None,
    W4,
    W44,
    W48,
    W84,
    W444,
    W448,
    W484,
    W488,
    W844,
    W848,
    W884,
}

impl NarrowTail {
    /// Every stack argument on this host occupies a whole eightbyte, so the
    /// doubleword prototype already places them; the narrow tails spell AAPCS64's
    /// packing rule, which SysV does not have.
    #[cfg(target_arch = "x86_64")]
    fn of(_overflow: &[ArgType], _fp_return: bool) -> Option<Self> {
        Some(NarrowTail::None)
    }

    /// Which prototype places `overflow`, or `None` if nothing here can.
    ///
    /// `fp_return` refuses the whole question rather than answering it wrong.
    #[cfg(target_arch = "aarch64")]
    fn of(overflow: &[ArgType], fp_return: bool) -> Option<Self> {
        if widening_preserves_offsets(overflow) {
            return Some(NarrowTail::None);
        }
        if fp_return {
            return None;
        }
        match overflow
            .iter()
            .map(|ty| if is_doubleword(*ty) { 8u8 } else { 4 })
            .collect::<Vec<_>>()
            .as_slice()
        {
            [4] => Some(NarrowTail::W4),
            [4, 4] => Some(NarrowTail::W44),
            [4, 8] => Some(NarrowTail::W48),
            [8, 4] => Some(NarrowTail::W84),
            [4, 4, 4] => Some(NarrowTail::W444),
            [4, 4, 8] => Some(NarrowTail::W448),
            [4, 8, 4] => Some(NarrowTail::W484),
            [4, 8, 8] => Some(NarrowTail::W488),
            [8, 4, 4] => Some(NarrowTail::W844),
            [8, 4, 8] => Some(NarrowTail::W848),
            [8, 8, 4] => Some(NarrowTail::W884),
            _ => None,
        }
    }
}

/// Call `host` with the arguments `ctx` holds, and write the result back.
pub fn call(
    symbol: &str,
    abi: &FunctionAbi,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &dyn GuestMemory,
) -> Result<()> {
    call_with(symbol, abi, host, ctx, mem, &super::NoCallbacks)
}

/// [`call`], with a bridge for the arguments [`FunctionAbi::callbacks`] names.
pub fn call_with(
    symbol: &str,
    abi: &FunctionAbi,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &dyn GuestMemory,
    callbacks: &dyn super::Callbacks,
) -> Result<()> {
    call_types(
        symbol,
        abi.types,
        host,
        ctx,
        mem,
        &ForCallbacks { abi, callbacks },
    )
}

/// What a value means on the other side of the seam.
///
/// By index rather than by value, because *which* arguments are special is stated by a
/// signature and cannot be recovered from the bits.
pub trait Convert {
    /// The value the host should receive for parameter `index`.
    fn argument(&self, symbol: &str, index: usize, raw: u64) -> Result<u64>;

    /// The value the guest should receive back from an integer or pointer return.
    fn result(&self, raw: u64) -> u64 {
        raw
    }
}

/// Every argument crosses unchanged. The common case, and the marshaller's own
/// tests.
pub struct Verbatim;

impl Convert for Verbatim {
    fn argument(&self, _symbol: &str, _index: usize, raw: u64) -> Result<u64> {
        Ok(raw)
    }
}

/// [`Convert`] for the parameters [`FunctionAbi::callbacks`] names.
struct ForCallbacks<'a> {
    abi: &'a FunctionAbi,
    callbacks: &'a dyn super::Callbacks,
}

impl Convert for ForCallbacks<'_> {
    fn argument(&self, symbol: &str, index: usize, raw: u64) -> Result<u64> {
        if self.abi.callbacks.contains(&(index as u8)) {
            self.callbacks.substitute(symbol, raw)
        } else {
            Ok(raw)
        }
    }
}

/// [`call_with`], for a signature that is not in the table.
pub fn call_types(
    symbol: &str,
    types: &[ArgType],
    host: u64,
    ctx: &mut RegisterContext,
    mem: &dyn GuestMemory,
    convert: &dyn Convert,
) -> Result<()> {
    let guest = guest_call_abi_ref();
    let placed = guest
        .place_args(types)
        .ok_or_else(|| AbiError::Unmarshallable {
            symbol: symbol.to_string(),
            reason: "the guest ABI cannot place its arguments",
        })?;

    // Collected first, so a memory failure on the last stack argument still
    // leaves the guest context untouched.
    let mut ints = [0u64; MAX_INT_ARGS_TOTAL];
    let mut fps = [0f64; MAX_FP_ARGS];
    let (mut n_int, mut n_fp) = (0usize, 0usize);
    // The integer arguments past the eighth, in order.
    let mut overflow: Vec<ArgType> = Vec::new();

    for (index, (ty, loc)) in placed.iter().enumerate() {
        // A pointer that means something different on the far side - a guest
        // function address the host would call as arm64, or a guest class the
        // host runtime has never realized.
        let raw = convert.argument(symbol, index, read_location(symbol, ctx, mem, *loc)?)?;
        if is_fp(*ty) {
            if n_fp == MAX_FP_ARGS {
                return Err(AbiError::Unmarshallable {
                    symbol: symbol.to_string(),
                    reason: "more than eight floating-point arguments",
                });
            }
            fps[n_fp] = fp_arg(*ty, raw);
            n_fp += 1;
        } else {
            if n_int == MAX_INT_ARGS_TOTAL {
                return Err(AbiError::Unmarshallable {
                    symbol: symbol.to_string(),
                    reason: "more than twenty-four integer arguments",
                });
            }
            if n_int >= MAX_INT_ARGS {
                overflow.push(*ty);
            }
            ints[n_int] = int_arg(*ty, raw);
            n_int += 1;
        }
    }

    let ret = types.first().copied().unwrap_or(ArgType::None);
    // Chosen after every argument is read and before anything is called, so a
    // signature whose tail no prototype places costs a named refusal with the
    // guest context untouched.
    let tail = NarrowTail::of(&overflow, is_fp(ret)).ok_or_else(|| AbiError::Unmarshallable {
        symbol: symbol.to_string(),
        reason: "no prototype places this signature's overflowing arguments",
    })?;
    match ret {
        // SAFETY: `host` is a host function address a `SymbolProvider` resolved
        // via `dlsym`, i.e. a real native arm64 C function. The prototypes below
        // are wider than its own in the argument registers only, which AAPCS64
        // permits (see the module note); the return variant is chosen to match
        // the class the table declares. Signatures with stack arguments or
        // aggregates never reach here - `is_marshallable` and the loop above
        // reject them.
        ArgType::SinglePrecisionFp => {
            let bits = unsafe { call_f32(host, &ints, &fps) }.to_bits();
            // Only the low 32 bits of xmm0 are architecturally defined for a
            // float return; zeroing the rest is the tidier of two legal answers.
            ctx.xmm[0] = [u64::from(bits), 0];
        }
        ArgType::DoublePrecisionFp => {
            let v = unsafe { call_f64(host, &ints, &fps) };
            ctx.xmm[0] = [v.to_bits(), 0];
        }
        ArgType::None => {
            unsafe { call_int_tail(host, &ints, &fps, tail) };
        }
        _ => {
            let v = unsafe { call_int_tail(host, &ints, &fps, tail) };
            ctx.rax = int_ret(ret, convert.result(v));
        }
    }
    Ok(())
}

fn is_fp(ty: ArgType) -> bool {
    matches!(ty, ArgType::SinglePrecisionFp | ArgType::DoublePrecisionFp)
}

/// The raw 64 bits the guest left at `loc`.
fn read_location(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    loc: ArgLocation,
) -> Result<u64> {
    match loc {
        ArgLocation::IntReg(reg) => int_reg(symbol, ctx, reg),
        ArgLocation::FpReg(i) => {
            ctx.xmm
                .get(usize::from(i))
                .map(|x| x[0])
                .ok_or_else(|| AbiError::Unmarshallable {
                    symbol: symbol.to_string(),
                    reason: "vector argument register out of range",
                })
        }
        // See the module note: `rsp` is the pre-`CALL` value at an import exit,
        // which is the base these offsets are measured from.
        ArgLocation::Stack(off) => mem.read_u64(
            ctx.rsp
                .wrapping_add(stack_base())
                .wrapping_add(u64::from(off)),
            symbol,
        ),
    }
}

/// Where the guest's outgoing stack arguments begin, relative to `rsp`.
///
/// Reading the wrong one does not fault.
pub(crate) fn stack_base() -> u64 {
    match crate::thread::current() {
        Some(state) if state.entered_by_jump() => 8,
        _ => 0,
    }
}

/// Resolve a [`GuestReg`] to the field of the context that holds it.
///
/// A `GuestReg` payload *is* an `iced_x86::Register` discriminant, so this goes
/// through microlift's own accessor rather than matching raw numbers - the
/// numbering is iced's to change.
fn int_reg(symbol: &str, ctx: &RegisterContext, reg: GuestReg) -> Result<u64> {
    let unsupported = || AbiError::Unmarshallable {
        symbol: symbol.to_string(),
        reason: "argument in a register the context does not model",
    };
    let reg = x86_reg(reg).map_err(|_| unsupported())?;
    Ok(match reg {
        Register::RAX => ctx.rax,
        Register::RCX => ctx.rcx,
        Register::RDX => ctx.rdx,
        Register::RBX => ctx.rbx,
        Register::RSP => ctx.rsp,
        Register::RBP => ctx.rbp,
        Register::RSI => ctx.rsi,
        Register::RDI => ctx.rdi,
        Register::R8 => ctx.r8,
        Register::R9 => ctx.r9,
        Register::R10 => ctx.r10,
        Register::R11 => ctx.r11,
        Register::R12 => ctx.r12,
        Register::R13 => ctx.r13,
        Register::R14 => ctx.r14,
        Register::R15 => ctx.r15,
        _ => return Err(unsupported()),
    })
}

/// Narrow the guest's 64-bit register down to the declared argument type, then widen it
/// back the way the host expects to receive it.
///
/// Apple's arm64 ABI makes the *caller* extend arguments narrower than 32 bits, so this
/// is not cosmetic.
fn int_arg(ty: ArgType, raw: u64) -> u64 {
    match ty {
        ArgType::SignedByte => raw as u8 as i8 as i64 as u64,
        ArgType::UnsignedByte => u64::from(raw as u8),
        ArgType::SignedHalfword => raw as u16 as i16 as i64 as u64,
        ArgType::UnsignedHalfword => u64::from(raw as u16),
        ArgType::SignedWord => raw as u32 as i32 as i64 as u64,
        ArgType::UnsignedWord => u64::from(raw as u32),
        _ => raw,
    }
}

/// The bits to put in `v_n` for a floating-point argument. See the module note
/// on why `f32` is a bit-pattern move and not a cast.
fn fp_arg(ty: ArgType, raw: u64) -> f64 {
    match ty {
        ArgType::SinglePrecisionFp => f64::from_bits(u64::from(raw as u32)),
        _ => f64::from_bits(raw),
    }
}

/// What the guest should find in `rax`.
///
/// x86-64 leaves only the declared width defined (a function returning `int` defines
/// `eax`), and the host leaves only the declared width defined in `x0`.
fn int_ret(ty: ArgType, raw: u64) -> u64 {
    int_arg(ty, raw)
}

type IntFn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> u64;

type F64Fn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> f64;

type F32Fn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> f32;

macro_rules! call_through {
    ($f:expr, $i:expr, $v:expr) => {
        $f(
            $i[0], $i[1], $i[2], $i[3], $i[4], $i[5], $i[6], $i[7], $i[8], $i[9], $i[10], $i[11],
            $i[12], $i[13], $i[14], $i[15], $i[16], $i[17], $i[18], $i[19], $i[20], $i[21], $i[22],
            $i[23], $v[0], $v[1], $v[2], $v[3], $v[4], $v[5], $v[6], $v[7],
        )
    };
}

/// One prototype per overflow-width pattern.
///
/// Eight `u64` fill `x0`–`x7`; the tail types are what AAPCS64 then places on the
/// stack, each at its own width and alignment; the eight `f64` occupy `v0`–`v7` and
/// never reach the stack, which is why declaring them after the tail does not move it.
macro_rules! narrow_tail_call {
    ($name:ident, $(($t:ty, $at:expr)),+) => {
        /// # Safety `host` must be a native C function whose overflowing arguments have
        /// exactly the widths this variant names, returning an integer, a pointer or
        /// nothing.
        unsafe fn $name(
            host: u64,
            ints: &[u64; MAX_INT_ARGS_TOTAL],
            fps: &[f64; MAX_FP_ARGS],
        ) -> u64 {
            type F = unsafe extern "C" fn(
                u64, u64, u64, u64, u64, u64, u64, u64,
                $($t),+,
                f64, f64, f64, f64, f64, f64, f64, f64,
            ) -> u64;
            let f: F = unsafe { core::mem::transmute(host as usize as *const ()) };
            unsafe {
                f(
                    ints[0], ints[1], ints[2], ints[3],
                    ints[4], ints[5], ints[6], ints[7],
                    $(ints[$at] as $t),+,
                    fps[0], fps[1], fps[2], fps[3],
                    fps[4], fps[5], fps[6], fps[7],
                )
            }
        }
    };
}

narrow_tail_call!(call_w4, (u32, 8));
narrow_tail_call!(call_w44, (u32, 8), (u32, 9));
narrow_tail_call!(call_w48, (u32, 8), (u64, 9));
narrow_tail_call!(call_w84, (u64, 8), (u32, 9));
narrow_tail_call!(call_w444, (u32, 8), (u32, 9), (u32, 10));
narrow_tail_call!(call_w448, (u32, 8), (u32, 9), (u64, 10));
narrow_tail_call!(call_w484, (u32, 8), (u64, 9), (u32, 10));
narrow_tail_call!(call_w488, (u32, 8), (u64, 9), (u64, 10));
narrow_tail_call!(call_w844, (u64, 8), (u32, 9), (u32, 10));
narrow_tail_call!(call_w848, (u64, 8), (u32, 9), (u64, 10));
narrow_tail_call!(call_w884, (u64, 8), (u64, 9), (u32, 10));

/// Call `host` through whichever prototype places its overflowing arguments.
///
/// # Safety
/// As [`call_int`], plus: `tail` must describe `host`'s real overflow widths.
unsafe fn call_int_tail(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
    tail: NarrowTail,
) -> u64 {
    unsafe {
        match tail {
            NarrowTail::None => call_int(host, ints, fps),
            NarrowTail::W4 => call_w4(host, ints, fps),
            NarrowTail::W44 => call_w44(host, ints, fps),
            NarrowTail::W48 => call_w48(host, ints, fps),
            NarrowTail::W84 => call_w84(host, ints, fps),
            NarrowTail::W444 => call_w444(host, ints, fps),
            NarrowTail::W448 => call_w448(host, ints, fps),
            NarrowTail::W484 => call_w484(host, ints, fps),
            NarrowTail::W488 => call_w488(host, ints, fps),
            NarrowTail::W844 => call_w844(host, ints, fps),
            NarrowTail::W848 => call_w848(host, ints, fps),
            NarrowTail::W884 => call_w884(host, ints, fps),
        }
    }
}

/// # Safety `host` must be a native C function of at most twenty-four integer and eight
/// floating-point scalar arguments returning an integer or pointer, with every integer
/// argument past the eighth a full doubleword.
unsafe fn call_int(host: u64, ints: &[u64; MAX_INT_ARGS_TOTAL], fps: &[f64; MAX_FP_ARGS]) -> u64 {
    let f: IntFn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

/// # Safety
/// As [`call_int`], for a callee returning `double`.
unsafe fn call_f64(host: u64, ints: &[u64; MAX_INT_ARGS_TOTAL], fps: &[f64; MAX_FP_ARGS]) -> f64 {
    let f: F64Fn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

/// # Safety
/// As [`call_int`], for a callee returning `float`.
unsafe fn call_f32(host: u64, ints: &[u64; MAX_INT_ARGS_TOTAL], fps: &[f64; MAX_FP_ARGS]) -> f32 {
    let f: F32Fn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_integer_arguments_are_extended_by_signedness() {
        assert_eq!(
            int_arg(ArgType::SignedByte, 0xffff_ffff_ffff_ff80),
            u64::MAX - 127
        );
        assert_eq!(int_arg(ArgType::UnsignedByte, 0xdead_beef_0000_0080), 0x80);
        assert_eq!(
            int_arg(ArgType::SignedWord, 0x1234_5678_ffff_fffe),
            u64::MAX - 1
        );
        assert_eq!(
            int_arg(ArgType::UnsignedWord, 0x1234_5678_ffff_fffe),
            0xffff_fffe
        );
        assert_eq!(
            int_arg(ArgType::Pointer, 0x1234_5678_9abc_def0),
            0x1234_5678_9abc_def0
        );
    }

    #[test]
    fn a_float_argument_moves_bits_rather_than_widening_the_value() {
        // The callee reads `s_n`, the low half of the register.
        let bits = 1.5f32.to_bits();
        let passed = fp_arg(ArgType::SinglePrecisionFp, u64::from(bits));
        assert_eq!(passed.to_bits() as u32, bits);
        assert_ne!(passed.to_bits(), (1.5f64).to_bits());
    }
}

// Aggregates

/// The most bytes an indirectly-returned aggregate may occupy.
const MAX_INDIRECT: usize = 256;

/// Where the guest left one argument, in SysV AMD64's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Where {
    /// Index into `rdi, rsi, rdx, rcx, r8, r9`.
    Int(usize),
    /// Index into `xmm0`...`xmm7`, of which only the low eight bytes are read.
    Sse(usize),
    /// Byte offset from `rsp`, which at an import exit is the pre-`CALL` value.
    Stack(u64),
}

/// Where the guest left one whole argument.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Scalar(Where),
    /// One [`Where`] per eightbyte, in order.
    Parts(Vec<Where>),
    /// `size` bytes at `rsp + offset`.
    Memory {
        offset: u64,
        size: usize,
    },
}

const SYSV_INT_REGS: usize = 6;
const SYSV_SSE_REGS: usize = 8;

/// Lay a signature out the way SysV AMD64 does, aggregates included.
///
/// Written here rather than taken from microlift's `CallAbi::place_args` because that
/// one knows only scalars, and an aggregate does not merely occupy a slot - it consumes
/// *two* registers of possibly different classes, or none at all and a run of stack
/// instead.
fn place_shapes(shapes: &[Shape]) -> (Vec<Place>, Option<Where>) {
    let mut int = 0usize;
    let mut sse = 0usize;
    let mut stack = 0u64;

    // A MEMORY-class return takes the first integer register for its hidden
    // buffer pointer, before any argument gets one.
    let sret = match shapes.first() {
        Some(Shape::Struct(agg)) if agg.guest_class() == GuestClass::Memory => {
            int = 1;
            Some(Where::Int(0))
        }
        _ => None,
    };

    let mut places = Vec::with_capacity(shapes.len().saturating_sub(1));
    for shape in shapes.iter().skip(1) {
        places.push(match shape {
            Shape::Scalar(ty) if aggregate::is_fp(*ty) => {
                let w = match sse < SYSV_SSE_REGS {
                    true => Where::Sse(sse),
                    false => Where::Stack(take_stack(&mut stack, 8)),
                };
                sse += usize::from(sse < SYSV_SSE_REGS);
                Place::Scalar(w)
            }
            Shape::Scalar(_) => {
                let w = match int < SYSV_INT_REGS {
                    true => Where::Int(int),
                    false => Where::Stack(take_stack(&mut stack, 8)),
                };
                int += usize::from(int < SYSV_INT_REGS);
                Place::Scalar(w)
            }
            Shape::Struct(agg) => match agg.guest_class() {
                GuestClass::Memory => Place::Memory {
                    offset: take_stack(&mut stack, agg.size),
                    size: agg.size,
                },
                GuestClass::Registers(classes) => {
                    let need_int = classes.iter().filter(|c| **c == Eightbyte::Integer).count();
                    let need_sse = classes.len() - need_int;
                    // All or nothing: SysV puts the *whole* aggregate in memory
                    // when the registers its eightbytes want are not all free.
                    if int + need_int > SYSV_INT_REGS || sse + need_sse > SYSV_SSE_REGS {
                        Place::Memory {
                            offset: take_stack(&mut stack, agg.size),
                            size: agg.size,
                        }
                    } else {
                        let parts = classes
                            .iter()
                            .map(|c| match c {
                                Eightbyte::Integer => {
                                    int += 1;
                                    Where::Int(int - 1)
                                }
                                Eightbyte::Sse => {
                                    sse += 1;
                                    Where::Sse(sse - 1)
                                }
                            })
                            .collect();
                        Place::Parts(parts)
                    }
                }
            },
        });
    }
    (places, sret)
}

/// Reserve `size` bytes of the guest's outgoing stack area, eight-aligned.
fn take_stack(cursor: &mut u64, size: usize) -> u64 {
    let at = *cursor;
    *cursor += (size as u64).next_multiple_of(8);
    at
}

/// The bytes the guest left for one aggregate argument.
fn read_aggregate(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    place: &Place,
    size: usize,
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; size.next_multiple_of(8)];
    match place {
        Place::Memory { offset, .. } => {
            let at = ctx.rsp.wrapping_add(stack_base()).wrapping_add(*offset);
            mem.read(at, &mut buf[..size])
                .map_err(|_| AbiError::Memory {
                    symbol: symbol.to_string(),
                    addr: at,
                })?;
        }
        Place::Parts(parts) => {
            for (i, w) in parts.iter().enumerate() {
                let word = read_where(symbol, ctx, mem, *w)?;
                buf[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
            }
        }
        Place::Scalar(_) => {
            return Err(AbiError::Unmarshallable {
                symbol: symbol.to_string(),
                reason: "an aggregate placed as a scalar",
            });
        }
    }
    buf.truncate(size);
    Ok(buf)
}

fn read_where(symbol: &str, ctx: &RegisterContext, mem: &dyn GuestMemory, w: Where) -> Result<u64> {
    Ok(match w {
        Where::Int(i) => [ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx, ctx.r8, ctx.r9][i],
        Where::Sse(i) => ctx.xmm[i][0],
        Where::Stack(off) => {
            mem.read_u64(ctx.rsp.wrapping_add(stack_base()).wrapping_add(off), symbol)?
        }
    })
}

/// Read `width` bytes at `offset` as the scalar `ty`, zero-padded.
fn leaf_bits(buf: &[u8], offset: usize, width: usize) -> u64 {
    let mut word = [0u8; 8];
    let end = (offset + width).min(buf.len());
    if offset < end {
        word[..end - offset].copy_from_slice(&buf[offset..end]);
    }
    u64::from_le_bytes(word)
}

/// Where the guest leaves every argument of one signature, which depends on the
/// signature alone.
///
/// Worked out per call by [`call_shapes`]; a caller that calls through the same
/// signature repeatedly - an Objective-C message - keeps one beside the shapes
/// and hands it to [`call_laid_out`] instead.
#[derive(Clone, Debug)]
pub struct Layout {
    places: Vec<Place>,
    sret: Option<Where>,
}

impl Layout {
    pub fn of(shapes: &[Shape]) -> Self {
        let (places, sret) = place_shapes(shapes);
        Layout { places, sret }
    }
}

/// Call `host` with a signature that may contain aggregates.
pub fn call_shapes(
    symbol: &str,
    shapes: &[Shape],
    host: u64,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    convert: &dyn Convert,
) -> Result<()> {
    call_laid_out(symbol, shapes, &Layout::of(shapes), host, ctx, mem, convert)
}

/// [`call_shapes`], with `layout` already worked out - by [`Layout::of`], from
/// these same `shapes`.
pub fn call_laid_out(
    symbol: &str,
    shapes: &[Shape],
    layout: &Layout,
    host: u64,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    convert: &dyn Convert,
) -> Result<()> {
    static VOID: Shape = Shape::Scalar(ArgType::None);
    let unmarshallable = |reason: &'static str| AbiError::Unmarshallable {
        symbol: symbol.to_string(),
        reason,
    };
    // Borrowed, not cloned: an aggregate return owns its leaves.
    let ret = shapes.first().unwrap_or(&VOID);
    // Classified before anything is read, so an unsupported return costs a
    // diagnostic rather than a half-made call.
    let ret_host = match ret {
        Shape::Scalar(_) => None,
        Shape::Struct(agg) => {
            if agg.size > MAX_INDIRECT {
                return Err(unmarshallable(
                    "an aggregate return larger than this runtime buffers",
                ));
            }
            Some(agg.host_class().map_err(unmarshallable)?)
        }
    };

    let Layout { places, sret } = layout;
    let sret = *sret;

    let Gathered {
        ints,
        fps,
        overflow,
        copies,
    } = gather_args(symbol, shapes, places, sret.is_some(), ctx, &*mem, convert)?;

    // Only the integer-returning prototypes were given tails, so a narrow
    // overflow is refused for anything else - an aggregate return included,
    // since those go through their own prototypes and would silently take the
    // doubleword one.
    let plain_integer_return =
        ret_host.is_none() && !matches!(ret.scalar(), Some(t) if aggregate::is_fp(t));
    let tail = NarrowTail::of(&overflow, !plain_integer_return).ok_or_else(|| {
        unmarshallable("no prototype places this signature's overflowing arguments")
    })?;

    // Everything that can fail has failed by here.
    let (out, scalar_ret) = call_returning(host, &ints, &fps, ret_host, ret, tail);
    // Said rather than left to scope: `ints` holds addresses into these, and the
    // callee has finished reading through them only now.
    drop(copies);
    write_return(ctx, mem, symbol, ret, sret, &out, scalar_ret, convert)
}

/// The prototype's arguments, ready to be handed to whichever variant is called.
struct Gathered {
    ints: [u64; MAX_INT_ARGS_TOTAL],
    fps: [f64; MAX_FP_ARGS],
    /// The integer arguments the prototype places on its stack, in order.
    overflow: Vec<ArgType>,
    /// The copies AAPCS64 B.4 says the *caller* makes for an aggregate too
    /// large for registers. `ints` holds their addresses, so they must outlive
    /// the call and not merely the gather.
    copies: Vec<Vec<u64>>,
}

/// Read each argument out of the guest and place it the way AAPCS64 would.
///
/// The classes are the host's, so an aggregate is re-decided here rather than taken
/// from where the guest left it: an eightbyte pair on one side can be a float
/// quadruple or a pointer to a copy on the other.
#[cfg(target_arch = "aarch64")]
fn gather_args(
    symbol: &str,
    shapes: &[Shape],
    places: &[Place],
    _sret: bool,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    convert: &dyn Convert,
) -> Result<Gathered> {
    let unmarshallable = |reason: &'static str| AbiError::Unmarshallable {
        symbol: symbol.to_string(),
        reason,
    };
    let mut ints = [0u64; MAX_INT_ARGS_TOTAL];
    let mut fps = [0f64; MAX_FP_ARGS];
    let (mut n_int, mut n_fp) = (0usize, 0usize);
    let mut copies: Vec<Vec<u64>> = Vec::new();

    // As in [`call_types`]: the integer arguments past the eighth decide which
    // prototype places them.
    let mut overflow: Vec<ArgType> = Vec::new();
    let mut push_int =
        |v: u64, ty: ArgType, n_int: &mut usize, overflow: &mut Vec<ArgType>| -> Result<()> {
            if *n_int == MAX_INT_ARGS_TOTAL {
                return Err(unmarshallable("more than twenty-four integer arguments"));
            }
            if *n_int >= MAX_INT_ARGS {
                overflow.push(ty);
            }
            ints[*n_int] = v;
            *n_int += 1;
            Ok(())
        };

    for (index, (shape, place)) in shapes.iter().skip(1).zip(places.iter()).enumerate() {
        match shape {
            Shape::Scalar(ty) => {
                let Place::Scalar(w) = place else {
                    return Err(unmarshallable("a scalar placed as an aggregate"));
                };
                let raw = convert.argument(symbol, index, read_where(symbol, ctx, mem, *w)?)?;
                if aggregate::is_fp(*ty) {
                    if n_fp == MAX_FP_ARGS {
                        return Err(unmarshallable("more than eight floating-point arguments"));
                    }
                    fps[n_fp] = fp_arg(*ty, raw);
                    n_fp += 1;
                } else {
                    push_int(int_arg(*ty, raw), *ty, &mut n_int, &mut overflow)?;
                }
            }
            Shape::Struct(agg) => {
                let bytes = read_aggregate(symbol, ctx, mem, place, agg.size)?;
                match agg.host_class().map_err(unmarshallable)? {
                    HostClass::Hfa { ty, count } => {
                        if n_fp + count > MAX_FP_ARGS {
                            return Err(unmarshallable("more than eight floating-point arguments"));
                        }
                        let width = aggregate::width_of(ty);
                        for i in 0..count {
                            fps[n_fp] = fp_arg(ty, leaf_bits(&bytes, i * width, width));
                            n_fp += 1;
                        }
                    }
                    HostClass::Integer { words } => {
                        for i in 0..words {
                            let w = leaf_bits(&bytes, i * 8, 8);
                            push_int(w, ArgType::UnsignedDoubleword, &mut n_int, &mut overflow)?;
                        }
                    }
                    // AAPCS64 B.4 passes a pointer to a copy the caller makes.
                    HostClass::Indirect => {
                        let mut copy = vec![0u64; agg.size.div_ceil(8)];
                        for (i, w) in copy.iter_mut().enumerate() {
                            *w = leaf_bits(&bytes, i * 8, 8);
                        }
                        let addr = copy.as_ptr() as usize as u64;
                        copies.push(copy);
                        push_int(addr, ArgType::Pointer, &mut n_int, &mut overflow)?;
                    }
                    HostClass::Eightbytes { .. } => {
                        return Err(unmarshallable("an aggregate classified for the wrong host"));
                    }
                }
            }
        }
    }

    Ok(Gathered {
        ints,
        fps,
        overflow,
        copies,
    })
}

/// Read each argument out of the guest and place it where SysV puts it.
///
/// The host's convention is the guest's, so every argument's destination is the place
/// [`place_shapes`] already computed for it. Re-deciding it from the argument's class
/// would get SysV's all-or-nothing rule wrong once the registers run out, and would
/// leave an argument *after* an aggregate SysV sent to memory with nowhere to go - it
/// still takes the register that aggregate skipped.
///
/// A memory-class return shifts the registers down one on both sides: the prototype's
/// hidden buffer pointer takes the first, as the guest's does.
#[cfg(target_arch = "x86_64")]
fn gather_args(
    symbol: &str,
    shapes: &[Shape],
    places: &[Place],
    sret: bool,
    ctx: &RegisterContext,
    mem: &dyn GuestMemory,
    convert: &dyn Convert,
) -> Result<Gathered> {
    /// The prototype's stack slots, indexed by eightbyte of the outgoing area.
    fn put(stack: &mut Vec<u64>, slot: usize, word: u64) {
        if stack.len() <= slot {
            stack.resize(slot + 1, 0);
        }
        stack[slot] = word;
    }

    let unmarshallable = |reason: &'static str| AbiError::Unmarshallable {
        symbol: symbol.to_string(),
        reason,
    };
    let mut ints = [0u64; MAX_INT_ARGS_TOTAL];
    let mut fps = [0f64; MAX_FP_ARGS];
    let mut stack: Vec<u64> = Vec::new();
    let shift = usize::from(sret);

    for (index, (shape, place)) in shapes.iter().skip(1).zip(places.iter()).enumerate() {
        match shape {
            Shape::Scalar(ty) => {
                let Place::Scalar(w) = place else {
                    return Err(unmarshallable("a scalar placed as an aggregate"));
                };
                let raw = convert.argument(symbol, index, read_where(symbol, ctx, mem, *w)?)?;
                match w {
                    Where::Int(i) => ints[i - shift] = int_arg(*ty, raw),
                    Where::Sse(i) => fps[*i] = fp_arg(*ty, raw),
                    // The guest wrote a whole slot and the callee reads only the
                    // declared width back out of it, so the bits cross as they are.
                    Where::Stack(off) => put(&mut stack, (off / 8) as usize, raw),
                }
            }
            Shape::Struct(agg) => {
                let bytes = read_aggregate(symbol, ctx, mem, place, agg.size)?;
                match place {
                    Place::Parts(parts) => {
                        for (i, w) in parts.iter().enumerate() {
                            let word = leaf_bits(&bytes, i * 8, 8);
                            match w {
                                Where::Int(r) => ints[r - shift] = word,
                                Where::Sse(v) => fps[*v] = f64::from_bits(word),
                                Where::Stack(off) => put(&mut stack, (off / 8) as usize, word),
                            }
                        }
                    }
                    Place::Memory { offset, size } => {
                        let base = (offset / 8) as usize;
                        for i in 0..size.div_ceil(8) {
                            put(&mut stack, base + i, leaf_bits(&bytes, i * 8, 8));
                        }
                    }
                    Place::Scalar(_) => {
                        return Err(unmarshallable("an aggregate placed as a scalar"));
                    }
                }
            }
        }
    }

    // The prototype declares twenty-four `u64`; the compiler puts the first of them
    // in the registers the hidden pointer left free, and the rest on the stack
    // in order - which is the area the offsets above index.
    let regs = HOST_INT_REGS - shift;
    if regs + stack.len() > MAX_INT_ARGS_TOTAL {
        return Err(unmarshallable("more than twenty-four integer arguments"));
    }
    ints[regs..regs + stack.len()].copy_from_slice(&stack);

    Ok(Gathered {
        ints,
        fps,
        overflow: Vec::new(),
        copies: Vec::new(),
    })
}

/// Make the call, choosing the prototype the *host's* return class needs.
fn call_returning(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
    ret_host: Option<HostClass>,
    ret: &Shape,
    tail: NarrowTail,
) -> (Vec<u8>, u64) {
    // SAFETY: `host` is a native arm64 C function - a `dlsym` result or an
    // Objective-C `IMP` the host runtime owns. The prototypes are wider than
    // its own in the argument registers only, which AAPCS64 permits (see the
    // module note), and the return variant matches the class computed from the
    // callee's own type encoding. An HFA prototype wider than the callee's own
    // return reads `v` registers the callee did not write, which is why only
    // the first `count` members are taken.
    unsafe {
        match ret_host {
            None => match ret.scalar() {
                Some(ArgType::SinglePrecisionFp) => {
                    (Vec::new(), u64::from(call_f32(host, ints, fps).to_bits()))
                }
                Some(ArgType::DoublePrecisionFp) => {
                    (Vec::new(), call_f64(host, ints, fps).to_bits())
                }
                _ => (Vec::new(), call_int_tail(host, ints, fps, tail)),
            },
            Some(HostClass::Hfa { ty, count }) => {
                let width = aggregate::width_of(ty);
                let mut out = Vec::with_capacity(count * width);
                if ty == ArgType::SinglePrecisionFp {
                    let v = call_f32x4(host, ints, fps);
                    for m in v.0.iter().take(count) {
                        out.extend_from_slice(&m.to_le_bytes());
                    }
                } else {
                    let v = call_f64x4(host, ints, fps);
                    for m in v.0.iter().take(count) {
                        out.extend_from_slice(&m.to_le_bytes());
                    }
                }
                (out, 0)
            }
            Some(HostClass::Integer { words }) => {
                let v = call_u64x2(host, ints, fps);
                let mut out = Vec::with_capacity(words * 8);
                for w in v.0.iter().take(words) {
                    out.extend_from_slice(&w.to_le_bytes());
                }
                (out, 0)
            }
            // Each eightbyte comes back in one register, and the prototype is
            // whichever pair of registers the classes name. A one-eightbyte
            // aggregate borrows the two-eightbyte prototype and reads only the
            // first: the callee wrote one register, and the second is simply
            // not looked at.
            Some(HostClass::Eightbytes { first, second }) => {
                let mut out = Vec::with_capacity(16);
                let mut take = |bytes: [u8; 8], keep: bool| {
                    if keep {
                        out.extend_from_slice(&bytes);
                    }
                };
                let want_second = second.is_some();
                match (first, second.unwrap_or(first)) {
                    (Eightbyte::Integer, Eightbyte::Integer) => {
                        let v = call_u64x2(host, ints, fps);
                        take(v.0[0].to_le_bytes(), true);
                        take(v.0[1].to_le_bytes(), want_second);
                    }
                    (Eightbyte::Sse, Eightbyte::Sse) => {
                        let v = call_f64x2(host, ints, fps);
                        take(v.0[0].to_le_bytes(), true);
                        take(v.0[1].to_le_bytes(), want_second);
                    }
                    (Eightbyte::Integer, Eightbyte::Sse) => {
                        let v = call_int_sse(host, ints, fps);
                        take(v.0.to_le_bytes(), true);
                        take(v.1.to_le_bytes(), want_second);
                    }
                    (Eightbyte::Sse, Eightbyte::Integer) => {
                        let v = call_sse_int(host, ints, fps);
                        take(v.0.to_le_bytes(), true);
                        take(v.1.to_le_bytes(), want_second);
                    }
                }
                (out, 0)
            }
            Some(HostClass::Indirect) => {
                let v = call_indirect(host, ints, fps);
                (v.0.to_vec(), 0)
            }
        }
    }
}

/// Put the result where the *guest's* convention says it goes.
#[expect(clippy::too_many_arguments)]
fn write_return(
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    symbol: &str,
    ret: &Shape,
    sret: Option<Where>,
    bytes: &[u8],
    scalar: u64,
    convert: &dyn Convert,
) -> Result<()> {
    let agg = match ret {
        Shape::Scalar(ArgType::SinglePrecisionFp) => {
            ctx.xmm[0] = [scalar & 0xffff_ffff, 0];
            return Ok(());
        }
        Shape::Scalar(ArgType::DoublePrecisionFp) => {
            ctx.xmm[0] = [scalar, 0];
            return Ok(());
        }
        Shape::Scalar(ArgType::None) => return Ok(()),
        Shape::Scalar(ty) => {
            ctx.rax = int_ret(*ty, convert.result(scalar));
            return Ok(());
        }
        Shape::Struct(agg) => agg,
    };

    match agg.guest_class() {
        // The hidden buffer the guest allocated, whose address it passed in the
        // first integer register and expects back in `rax`.
        GuestClass::Memory => {
            let Some(w) = sret else {
                return Err(AbiError::Unmarshallable {
                    symbol: symbol.to_string(),
                    reason: "a memory-class return with no buffer register",
                });
            };
            let dest = read_where(symbol, ctx, mem, w)?;
            let n = bytes.len().min(agg.size);
            mem.write(dest, &bytes[..n]).map_err(|_| AbiError::Memory {
                symbol: symbol.to_string(),
                addr: dest,
            })?;
            ctx.rax = dest;
        }
        GuestClass::Registers(classes) => {
            let (mut n_int, mut n_sse) = (0usize, 0usize);
            for (i, class) in classes.iter().enumerate() {
                let word = leaf_bits(bytes, i * 8, 8);
                match class {
                    Eightbyte::Integer => {
                        match n_int {
                            0 => ctx.rax = word,
                            _ => ctx.rdx = word,
                        }
                        n_int += 1;
                    }
                    Eightbyte::Sse => {
                        ctx.xmm[n_sse] = [word, 0];
                        n_sse += 1;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Four doubles in `v0`–`v3`: what AAPCS64 returns a homogeneous float aggregate of up
/// to four `double`s in.
#[repr(C)]
#[derive(Clone, Copy)]
struct F64x4([f64; 4]);

/// As [`F64x4`], for `float` members, which occupy `s0`–`s3`.
#[repr(C)]
#[derive(Clone, Copy)]
struct F32x4([f32; 4]);

/// Two doublewords in `x0`/`x1`: a composite of at most sixteen bytes that is
/// not a homogeneous float aggregate.
#[repr(C)]
#[derive(Clone, Copy)]
struct U64x2([u64; 2]);

/// A buffer the callee fills through `x8`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Indirect([u8; MAX_INDIRECT]);

type F64x4Fn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> F64x4;

/// # Safety
/// As [`call_int`], for a callee whose return AAPCS64 places in `v0`–`v3`.
unsafe fn call_f64x4(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> F64x4 {
    let f: F64x4Fn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

type F32x4Fn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> F32x4;

/// # Safety
/// As [`call_int`], for a callee whose return AAPCS64 places in `s0`–`s3`.
unsafe fn call_f32x4(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> F32x4 {
    let f: F32x4Fn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

type U64x2Fn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> U64x2;

/// # Safety
/// As [`call_int`], for a callee whose return AAPCS64 places in `x0` and `x1`.
unsafe fn call_u64x2(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> U64x2 {
    let f: U64x2Fn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

/// Two SSE eightbytes, in `xmm0` and `xmm1`.
///
/// Sixteen bytes, so SysV returns it in registers - which is the whole reason this
/// exists next to [`F64x4`]: that one is thirty-two and would come back through a
/// hidden pointer the callee was never given.
#[repr(C)]
#[derive(Clone, Copy)]
struct F64x2([f64; 2]);

/// An integer eightbyte then an SSE one: `rax` and `xmm0`.
#[repr(C)]
#[derive(Clone, Copy)]
struct IntSse(u64, f64);

/// An SSE eightbyte then an integer one: `xmm0` and `rax`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SseInt(f64, u64);

type F64x2Fn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> F64x2;

/// # Safety
/// As [`call_int`], for a callee returning two SSE eightbytes.
unsafe fn call_f64x2(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> F64x2 {
    let f: F64x2Fn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

type IntSseFn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> IntSse;

/// # Safety
/// As [`call_int`], for a callee returning an integer eightbyte then an SSE one.
unsafe fn call_int_sse(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> IntSse {
    let f: IntSseFn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

type SseIntFn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> SseInt;

/// # Safety
/// As [`call_int`], for a callee returning an SSE eightbyte then an integer one.
unsafe fn call_sse_int(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> SseInt {
    let f: SseIntFn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}

type IndirectFn = unsafe extern "C" fn(
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
) -> Indirect;

/// # Safety
/// As [`call_int`], for a callee whose return AAPCS64 places in memory addressed by `x8`.
unsafe fn call_indirect(
    host: u64,
    ints: &[u64; MAX_INT_ARGS_TOTAL],
    fps: &[f64; MAX_FP_ARGS],
) -> Indirect {
    let f: IndirectFn = unsafe { core::mem::transmute(host as usize as *const ()) };
    unsafe { call_through!(f, ints, fps) }
}
