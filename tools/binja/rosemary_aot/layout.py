"""Places a cache file's chunks in one address space and applies their relocations.

Independent of Binary Ninja so it can be checked with plain Python. Every relocation
Microlift emits is an absolute 8-byte store into `.text`, so sections may go anywhere;
what matters is that host code, host symbols and guest addresses never overlap.
"""

import re
import struct
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

from . import rmc

# Far above any Mach-O image, so a relocated guest address never names host code.
DEFAULT_BASE = 0x7F00_0000_0000
PAGE = 0x4000
# Each host symbol gets one pointer-sized slot in the synthetic extern segment.
EXTERN_SLOT = 8
U64_MASK = (1 << 64) - 1


def align(value: int, to: int) -> int:
    return (value + to - 1) & ~(to - 1)


@dataclass
class ChunkLayout:
    text: int
    rodata: int
    bss: int
    # Offsets of `.text` and `.rodata` in `Layout.blob`; `.bss` is not backed.
    text_offset: int
    rodata_offset: int

    def section_addr(self, section: rmc.Section) -> int:
        if section == rmc.Section.TEXT:
            return self.text
        if section == rmc.Section.RODATA:
            return self.rodata
        return self.bss


SymbolKey = Tuple


@dataclass
class ExternSymbol:
    symbol: rmc.HostSymbol
    addr: int
    # `name`, or `name@provider` when two providers export the same name.
    display: str


@dataclass
class Layout:
    base: int
    guest_slide: int
    chunks: List[ChunkLayout] = field(default_factory=list)
    externs: List[ExternSymbol] = field(default_factory=list)
    extern_start: int = 0
    end: int = 0
    blob: bytearray = field(default_factory=bytearray)


def symbol_key(symbol: rmc.HostSymbol) -> SymbolKey:
    return (symbol.kind, symbol.provider, symbol.name, symbol.signature)


def _provider_tag(provider: str) -> str:
    if provider == rmc.RUNTIME_PROVIDER:
        return "rosemary"
    tail = provider.rsplit("/", 1)[-1]
    return re.sub(r"\.(dylib|so(\.\d+)*|framework)$", "", tail) or provider


def build(cache: rmc.CacheFile, base: int = DEFAULT_BASE, guest_slide: int = 0) -> Layout:
    """Lays out every chunk and patches relocations.

    `guest_slide` is what `GuestSlide` resolves to. Its addends are lift-time VAs, so 0
    reproduces the run the image was lifted in and `-cache.lift_slide` gives file VAs.
    """
    out = Layout(base, guest_slide)
    blob = bytearray()
    addr = base
    for image in cache.chunks:
        text = align(addr, PAGE)
        rodata = align(text + len(image.text), max(PAGE, image.rodata_align))
        bss = align(rodata + len(image.rodata), PAGE)
        addr = bss + image.bss_len
        text_offset = len(blob)
        blob += image.text
        rodata_offset = len(blob)
        blob += image.rodata
        out.chunks.append(ChunkLayout(text, rodata, bss, text_offset, rodata_offset))

    out.extern_start = align(addr, PAGE)
    by_key: Dict[SymbolKey, ExternSymbol] = {}
    names: Dict[str, int] = {}
    for image in cache.chunks:
        for symbol in image.symbols:
            if isinstance(symbol, rmc.HostSymbol) and symbol_key(symbol) not in by_key:
                slot = out.extern_start + len(out.externs) * EXTERN_SLOT
                extern = ExternSymbol(symbol, slot, symbol.name)
                by_key[symbol_key(symbol)] = extern
                out.externs.append(extern)
                names[symbol.name] = names.get(symbol.name, 0) + 1
    for extern in out.externs:
        if names[extern.symbol.name] > 1:
            extern.display = f"{extern.symbol.name}@{_provider_tag(extern.symbol.provider)}"
    out.end = out.extern_start + max(1, len(out.externs)) * EXTERN_SLOT

    for image, placed in zip(cache.chunks, out.chunks):
        resolved: List[Optional[int]] = []
        for symbol in image.symbols:
            if isinstance(symbol, rmc.GuestSlide):
                resolved.append(guest_slide)
            else:
                resolved.append(by_key[symbol_key(symbol)].addr)
        for reloc in image.relocs:
            if reloc.target[0] == "symbol":
                target = resolved[reloc.target[1]]
            else:
                target = placed.section_addr(reloc.target[1]) + reloc.target[2]
            struct.pack_into("<Q", blob, placed.text_offset + reloc.offset, (target + reloc.addend) & U64_MASK)

    out.blob = blob
    return out


def file_slide(cache: rmc.CacheFile) -> int:
    """The `guest_slide` that maps lift-time VAs back to the image's file VAs."""
    return -cache.lift_slide & U64_MASK


def guest_va(layout: Layout, lift_va: int) -> int:
    """A lift-time guest VA in the address space the layout's relocations use."""
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
