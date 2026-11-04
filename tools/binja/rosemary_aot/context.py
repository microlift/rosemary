"""Microlift's x86-64 guest context and the types around it, as plain data.

Mirrors `src/arch/x86_64/context.rs`, `core/exit.rs`, `core/link.rs` and `core/callret.rs` at the Microlift
revision in `Cargo.lock`; offsets follow `#[repr(C)]`. Independent of Binary Ninja so the layout can be
checked with plain Python.
"""

import struct
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple, Union


@dataclass(frozen=True)
class Int:
    size: int
    signed: bool = False


@dataclass(frozen=True)
class Float:
    size: int


@dataclass(frozen=True)
class Named:
    """A struct, union or enum defined in `TYPES` or `ENUMS`."""

    name: str


@dataclass(frozen=True)
class Pointer:
    # None is `void*`.
    target: Optional["Spec"] = None


@dataclass(frozen=True)
class FunctionPointer:
    """`void (*)(params...)`."""

    params: Tuple[Tuple["Spec", str], ...]


@dataclass(frozen=True)
class Array:
    element: "Spec"
    count: int


Spec = Union[Int, Float, Named, Pointer, FunctionPointer, Array]


@dataclass(frozen=True)
class Field:
    name: str
    spec: Spec
    # Overrides the natural alignment, as `Align16` does for `xmm`.
    align: Optional[int] = None


@dataclass(frozen=True)
class Aggregate:
    fields: Tuple[Field, ...]
    union: bool = False
    align: Optional[int] = None


@dataclass(frozen=True)
class Enum:
    size: int
    members: Tuple[Tuple[str, int], ...]


U8, U16, U32, U64 = Int(1), Int(2), Int(4), Int(8)
POINTER_SIZE = 8

REGISTER_CONTEXT = "RegisterContext"
LINK_ENTRY = "BlockLinkEntry"
XMM_REGISTER = "XmmRegister"

GPR_NAMES = (
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
    "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15",
)  # fmt: skip
XMM_COUNT = 16
X87_COUNT = 8

ENUMS: Dict[str, Enum] = {
    # Discriminants 2 and 4 are retired upstream and must not be reused.
    "ExitReason": Enum(
        4,
        (
            ("ExitNone", 0),
            ("ExitReturn", 1),
            ("ExitUnknownJump", 3),
            ("ExitInvalidInstruction", 5),
            ("ExitImportedFunctionCall", 6),
            ("ExitIndirectCall", 7),
            ("ExitUd2", 8),
            ("ExitBreakpoint", 9),
            ("ExitCpuid", 10),
            ("ExitXgetbv", 11),
            ("ExitInterrupt", 12),
            ("ExitPortInput", 13),
            ("ExitPortOutput", 14),
            ("ExitRdtsc", 15),
            ("ExitDivideError", 16),
        ),
    ),
    # `ControlHeader::exit_code` for `ExitUnknownJump` only, so `exit_code` itself stays an integer.
    "TransferKind": Enum(
        4,
        (
            ("TransferUnknown", 0),
            ("TransferDirectJump", 1),
            ("TransferIndirectJump", 2),
            ("TransferConditionalJump", 3),
            ("TransferReturn", 4),
            ("TransferDirectCall", 5),
            ("TransferRegionTail", 6),
            ("TransferNoResumePoint", 7),
            ("TransferIndirectCall", 8),
        ),
    ),
}

EXIT_REASON = "ExitReason"
TRANSFER_KIND = "TransferKind"


def enum_member(enum: str, value: int) -> Optional[str]:
    return next((name for name, v in ENUMS[enum].members if v == value), None)


def _exit(name: str) -> int:
    return next(v for n, v in ENUMS[EXIT_REASON].members if n == name)


EXIT_UNKNOWN_JUMP = _exit("ExitUnknownJump")
EXIT_INTERRUPT = _exit("ExitInterrupt")
EXIT_PORT_IO = (_exit("ExitPortInput"), _exit("ExitPortOutput"))


@dataclass(frozen=True)
class ExitCode:
    """What `ControlHeader::exit_code` means under one exit reason."""

    text: str
    # The enum the constant is a member of, if any.
    enum: Optional[str] = None


def describe_exit_code(reason: int, code: int) -> Optional[ExitCode]:
    if reason == EXIT_UNKNOWN_JUMP:
        name = enum_member(TRANSFER_KIND, code)
        return ExitCode(name, TRANSFER_KIND) if name else None
    if reason == EXIT_INTERRUPT:
        return ExitCode(f"vector {code:#x}")
    if reason in EXIT_PORT_IO:
        return ExitCode(f"{code}-byte elements")
    return None


_CTX = Pointer(Named(REGISTER_CONTEXT))

TYPES: Dict[str, Aggregate] = {
    LINK_ENTRY: Aggregate(
        (
            Field("guest_pc", U64),
            Field("code", Pointer()),
            Field("image_base", U64),
            Field("reserved", U64),
        )
    ),
    "CallRetEntry": Aggregate((Field("guest_pc", U64), Field("cell", Pointer(Named(LINK_ENTRY))))),
    "ControlHeader": Aggregate(
        (
            Field("exit_reason", Named("ExitReason")),
            Field("exit_code", U32),
            Field("imported_function_va", U64),
            Field("image_base", U64),
        )
    ),
    "X86_64Callbacks": Aggregate(
        (
            Field("handle_syscall", FunctionPointer(((_CTX, "ctx"),))),
            Field("interpreter_fallback", FunctionPointer(((_CTX, "ctx"), (U64, "num_instructions")))),
        )
    ),
    # Upstream stores each XMM as `[lo_64, hi_64]`; the other views are for reading lane-wise code.
    XMM_REGISTER: Aggregate(
        (
            Field("q", Array(U64, 2)),
            Field("d", Array(U32, 4)),
            Field("w", Array(U16, 8)),
            Field("b", Array(U8, 16)),
            Field("pd", Array(Float(8), 2)),
            Field("ps", Array(Float(4), 4)),
        ),
        union=True,
    ),
    # FXSAVE `_st[i]`: an `[u64; 2]` whose second word holds only the sign and exponent.
    "X87Register": Aggregate((Field("mantissa", U64), Field("sign_exp", U16), Field("reserved", Array(U16, 3)))),
    REGISTER_CONTEXT: Aggregate(
        tuple(Field(name, U64) for name in GPR_NAMES)
        + (
            Field("rip", U64),
            Field("callbacks", Named("X86_64Callbacks")),
            Field("link_table", Pointer(Named(LINK_ENTRY))),
            Field("cf", U8),
            # A byte whose parity is PF, not PF itself.
            Field("pf_src", U8),
            # A byte whose bit 4 is AF.
            Field("af_src", U8),
            Field("zf", U8),
            Field("sf", U8),
            Field("of", U8),
            Field("df", U8),
            Field("control", Named("ControlHeader")),
            Field("xmm", Array(Named(XMM_REGISTER), XMM_COUNT), align=16),
            Field("fs_base", U64),
            Field("gs_base", U64),
            Field("fpr", Array(Named("X87Register"), X87_COUNT)),
            Field("fpu_top", U16),
            Field("fpu_sw", U16),
            Field("fpu_cw", U16),
            Field("fpu_tag", U16),
            Field("mxcsr", U32),
            Field("cs", U16),
            Field("ss", U16),
            Field("ds", U16),
            Field("es", U16),
            Field("fs", U16),
            Field("gs", U16),
            Field("link_hit", U64),
            Field("callret_base", Pointer(Named("CallRetEntry"))),
            Field("callret_off", U64),
            Field("host_frame_depth", U32),
            Field("_pad_host_frame", U32),
        ),
        # `Align16` on `xmm` raises the whole struct's alignment.
        align=16,
    ),
}


def _align_up(value: int, to: int) -> int:
    return (value + to - 1) // to * to


def size_align(spec: Spec) -> Tuple[int, int]:
    if isinstance(spec, (Int, Float)):
        return spec.size, spec.size
    if isinstance(spec, (Pointer, FunctionPointer)):
        return POINTER_SIZE, POINTER_SIZE
    if isinstance(spec, Array):
        size, align = size_align(spec.element)
        return size * spec.count, align
    if spec.name in ENUMS:
        size = ENUMS[spec.name].size
        return size, size
    layout = layout_of(spec.name)
    return layout.size, layout.align


@dataclass(frozen=True)
class Layout:
    offsets: Tuple[Tuple[Field, int], ...]
    size: int
    align: int


def layout_of(name: str) -> Layout:
    aggregate = TYPES[name]
    offsets = []
    end = 0
    align = aggregate.align or 1
    for field in aggregate.fields:
        size, natural = size_align(field.spec)
        field_align = field.align or natural
        offset = 0 if aggregate.union else _align_up(end, field_align)
        offsets.append((field, offset))
        end = max(end, offset + size)
        align = max(align, field_align)
    return Layout(tuple(offsets), _align_up(end, align), align)


def offset_of(name: str, field: str) -> int:
    return next(offset for f, offset in layout_of(name).offsets if f.name == field)


def control_offset(field: str) -> int:
    """Offset of a `ControlHeader` field from the start of `RegisterContext`."""
    return offset_of(REGISTER_CONTEXT, "control") + offset_of("ControlHeader", field)


def guest_register_at(offset: int, size: int) -> Optional[str]:
    """The guest register a `size`-byte load from `RegisterContext + offset` reads, if it is a whole one."""
    if size == 8 and offset % 8 == 0 and offset < len(GPR_NAMES) * 8:
        return GPR_NAMES[offset // 8]
    xmm = offset_of(REGISTER_CONTEXT, "xmm")
    if size == 16 and (offset - xmm) % 16 == 0 and 0 <= offset - xmm < XMM_COUNT * 16:
        return f"xmm{(offset - xmm) // 16}"
    return None


# AArch64 encodings the entry thunk uses to move the context into the pinned argument registers.
_LDR_X_UIMM = (0xFFC00000, 0xF9400000)  # ldr Xt, [Xn, #imm12 * 8]
_LDR_Q_UIMM = (0xFFC00000, 0x3DC00000)  # ldr Qt, [Xn, #imm12 * 16]
_LDR_X_UIMM_SP = 31
_LDUR_X = (0xFFE00C00, 0xF8400000)  # ldur Xt, [Xn, #simm9]
_BLR = (0xFFFFFC1F, 0xD63F0000)
_RET = 0xD65F03C0
# `extern "C"`: the host passes the context as the first argument.
_AARCH64_HOST_CTX = 0


@dataclass(frozen=True)
class PinnedSlot:
    register: str
    guest: str
    vector: bool


@dataclass(frozen=True)
class PinnedAbi:
    """How an AArch64 entry thunk calls its body: the context register and each pinned guest register."""

    context: str
    slots: Tuple[PinnedSlot, ...]


def _matches(word: int, encoding: Tuple[int, int]) -> bool:
    return word & encoding[0] == encoding[1]


def decode_aarch64_thunk(text: bytes, entry: int) -> Optional[PinnedAbi]:
    """Reads the pinned ABI off an entry thunk: the loads from the incoming context before the
    call to the body, and the register the context is reloaded into right before that call.

    None when the code does not have that shape, so callers type nothing rather than guess.
    """
    slots: List[PinnedSlot] = []
    previous = None
    for at in range(entry, len(text) - 3, 4):
        (word,) = struct.unpack_from("<I", text, at)
        if word == _RET:
            return None
        if _matches(word, _BLR):
            if previous is None or not _matches(previous, _LDUR_X) and not _matches(previous, _LDR_X_UIMM):
                return None
            if (previous >> 5) & 0x1F != _LDR_X_UIMM_SP:
                return None
            return PinnedAbi(f"x{previous & 0x1F}", tuple(slots))
        rn, rt, imm12 = (word >> 5) & 0x1F, word & 0x1F, (word >> 10) & 0xFFF
        if rn == _AARCH64_HOST_CTX:
            if _matches(word, _LDR_X_UIMM):
                guest = guest_register_at(imm12 * 8, 8)
                if guest is not None:
                    slots.append(PinnedSlot(f"x{rt}", guest, False))
            elif _matches(word, _LDR_Q_UIMM):
                guest = guest_register_at(imm12 * 16, 16)
                if guest is not None:
                    slots.append(PinnedSlot(f"q{rt}", guest, True))
        previous = word
    return None
