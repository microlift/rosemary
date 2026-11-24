"""Places an image where Microlift's loader would, and fills in its GOT.

Independent of Binary Ninja so it can be checked with plain Python. Code loads every host
reference from a GOT entry `GOT_SPAN` below it and is never written at load, so the code maps
straight from the file; only the GOT, which the file leaves to the loader, is synthesized here.
"""

import re
import sys
from array import array
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

from . import rmc

# Far above any Mach-O image, so a relocated guest address never names host code.
DEFAULT_BASE = 0x7F00_0000_0000
# Each host symbol gets one pointer-sized slot in the synthetic extern segment.
EXTERN_SLOT = 8
U64_MASK = rmc.U64_MASK


def align(value: int, to: int) -> int:
    return (value + to - 1) & ~(to - 1)


@dataclass
class ExternSymbol:
    symbol: rmc.HostSymbol
    addr: int
    # `name`, or `name@provider` when two providers export the same name.
    display: str


@dataclass
class Layout:
    """Addresses as Microlift's `MappedImage` reserves them: the shared GOT entries at `got`,
    each region's own two counted down from `text`, and the zeroed data after the code."""

    base: int
    guest_slide: int
    got: int = 0
    text: int = 0
    bss: int = 0
    own_got: int = 0
    extern_start: int = 0
    end: int = 0
    externs: List[ExternSymbol] = field(default_factory=list)
    # Extern per shared slot, None for the slide and text entries.
    slot_externs: List[Optional[ExternSymbol]] = field(default_factory=list)
    shared_values: bytes = b""
    own_values: bytes = b""

    def shared_slot(self, index: int) -> int:
        return self.got + index * rmc.GOT_ENTRY

    def own_slots(self, region: int) -> Tuple[int, int]:
        """The GOT entries holding region `region`'s cells and body."""
        at = self.text - rmc.REGION_SLOTS * (region + 1)
        return at, at + rmc.GOT_ENTRY


def _provider_tag(provider: str) -> str:
    if provider == rmc.RUNTIME_PROVIDER:
        return "rosemary"
    tail = provider.rsplit("/", 1)[-1]
    return re.sub(r"\.(dylib|so(\.\d+)*|framework)$", "", tail) or provider


def _words(values: List[int]) -> bytes:
    out = array("Q", (v & U64_MASK for v in values))
    if sys.byteorder != "little":
        out.byteswap()
    return out.tobytes()


def build(image: rmc.Image, base: int = DEFAULT_BASE, guest_slide: int = 0) -> Layout:
    """Lays the image out at `base` and fills every GOT entry.

    `guest_slide` is what `GuestSlide` resolves to. Code and metadata hold lift-time VAs, so 0
    reproduces the run the image was lifted in and `file_slide` gives file VAs.
    """
    h = image.header
    out = Layout(base, guest_slide & U64_MASK)
    out.got = base
    out.text = base + rmc.GOT_SPAN
    out.own_got = out.text - rmc.REGION_SLOTS * h.region_count
    out.bss = align(out.text + h.text_len, rmc.PAGE)
    out.extern_start = align(out.bss + h.bss_len, rmc.PAGE)

    names: Dict[str, int] = {}
    shared = []
    for entry in image.shared:
        extern = None
        if isinstance(entry, rmc.HostSymbol):
            extern = ExternSymbol(entry, out.extern_start + len(out.externs) * EXTERN_SLOT, entry.name)
            out.externs.append(extern)
            names[entry.name] = names.get(entry.name, 0) + 1
            shared.append(extern.addr)
        elif isinstance(entry, rmc.TextEntry):
            shared.append(out.text + entry.offset)
        else:
            shared.append(out.guest_slide)
        out.slot_externs.append(extern)
    for extern in out.externs:
        if names[extern.symbol.name] > 1:
            extern.display = f"{extern.symbol.name}@{_provider_tag(extern.symbol.provider)}"
    out.end = out.extern_start + max(1, len(out.externs)) * EXTERN_SLOT
    out.shared_values = _words(shared)

    rec = image.records
    own = [0] * (2 * h.region_count)
    # Region i's pair sits i + 1 pairs below the code, so the table runs in reverse.
    own[0::2] = [0 if c == rmc.NO_CELLS else out.bss + c for c in reversed(rec.cells)]
    own[1::2] = [out.text + c + t for c, t in zip(reversed(rec.code), reversed(rec.link_target))]
    out.own_values = _words(own)
    return out


def file_slide(image: rmc.Image) -> int:
    """The `guest_slide` that maps lift-time VAs back to the image's file VAs."""
    return -image.lift_slide & U64_MASK


def guest_va(layout: Layout, lift_va: int) -> int:
    """A lift-time guest VA in the address space the layout's GOT uses."""
    return (lift_va + layout.guest_slide) & U64_MASK


_SIGNATURE = re.compile(r"^\((?P<params>.*)\)(?: -> (?P<ret>.+?))? (?P<cc>\S+)$")


def parse_signature(signature: str) -> Optional[Tuple[List[str], List[str], str]]:
    """Splits a Cranelift signature (`(i64, f64) -> i64 apple_aarch64`) into
    parameter types, return types and calling convention."""
    match = _SIGNATURE.match(signature)
    if not match:
        return None
    params = [p.strip() for p in match["params"].split(",") if p.strip()]
    ret = (match["ret"] or "").strip("() ")
    returns = [r.strip() for r in ret.split(",") if r.strip()]
    return params, returns, match["cc"]
