"""Reader for Rosemary AOT cache files (`*.rmc`), independent of Binary Ninja.

Mirrors `crates/rosemary/src/aot/file.rs` (the `RMAOTv2` wrapper; `RMAOTv1` files, which lack the
source path, are still read) and Microlift's
`src/image.rs` (each `MLIMAGE` chunk). Little-endian throughout.
"""

import struct
from dataclasses import dataclass, field
from enum import IntEnum
from typing import List, Optional, Tuple, Union

CACHE_MAGIC = b"RMAOTv2\0"
# The previous wrapper, identical but for the missing source path.
CACHE_MAGIC_V1 = b"RMAOTv1\0"
IMAGE_MAGIC = b"MLIMAGE\0"
IMAGE_FORMAT_VERSION = 1

# Microlift `core::link::ENTRY_SIZE`: bytes per link cell.
LINK_ENTRY_SIZE = 32
# Microlift `image::TEXT_ALIGN`.
TEXT_ALIGN = 16
# Microlift `core::symbol::RUNTIME_PROVIDER` / `LIBCALL_PROVIDER`.
RUNTIME_PROVIDER = ""
LIBCALL_PROVIDER = "cranelift"

AARCH64_RET = 0xD65F03C0
# Encoded `GuestCodeRange`: u32 start, u32 end, u64 guest_va.
CODE_RANGE_SIZE = 16


class FormatError(Exception):
    pass


class Section(IntEnum):
    TEXT = 1
    RODATA = 2
    BSS = 3


class HostSymbolKind(IntEnum):
    FUNCTION = 1
    DATA = 2
    LIBCALL = 3


class LinkSiteKind(IntEnum):
    STATIC_EDGE = 0
    CALL_RETURN = 1


class DecisionKind(IntEnum):
    NOT_IMPORT = 0
    SLOW = 1
    FAST = 2
    BRIDGE = 3


@dataclass
class GuestSlide:
    def display_name(self) -> str:
        return "__guest_slide"


@dataclass
class HostSymbol:
    kind: HostSymbolKind
    provider: str
    name: str
    signature: Optional[str] = None

    def display_name(self) -> str:
        return self.name


ImageSymbol = Union[GuestSlide, HostSymbol]


@dataclass
class Reloc:
    """`*(text + offset) as u64 = target + addend`."""

    offset: int
    # Either `("symbol", index)` or `("local", Section, offset)`.
    target: Tuple
    addend: int


@dataclass
class ImportDecision:
    kind: DecisionKind
    provider: str = ""
    name: str = ""
    abi: str = ""

    def describe(self) -> str:
        if self.kind == DecisionKind.FAST:
            return f"fast {self.provider}!{self.name} ({self.abi})"
        if self.kind == DecisionKind.BRIDGE:
            return f"bridge {self.name}"
        return self.kind.name.lower().replace("_", " ")


@dataclass
class Region:
    lift_base: int
    entry: int
    link_target: int
    cells: Optional[int]
    resume_points: List[int]
    covered: List[Tuple[int, int]]
    data_dependencies: List[Tuple[int, int]]
    link_sites: List[Tuple[int, LinkSiteKind]]
    import_decisions: List[Tuple[int, ImportDecision]]
    # Packed `<IIQ` records; large files hold millions, so they are unpacked on demand.
    map_bytes: bytes

    @property
    def map(self) -> List[Tuple[int, int, int]]:
        """(start, end, guest_va) host runs; offsets relative to `link_target`."""
        return list(struct.iter_unpack("<IIQ", self.map_bytes))

    @property
    def map_len(self) -> int:
        return len(self.map_bytes) // CODE_RANGE_SIZE


@dataclass
class Image:
    isa_fingerprint: int
    config_fingerprint: int
    text: bytes
    rodata: bytes
    rodata_align: int
    bss_len: int
    symbols: List[ImageSymbol]
    relocs: List[Reloc]
    regions: List[Region]

    def section_len(self, section: Section) -> int:
        if section == Section.TEXT:
            return len(self.text)
        if section == Section.RODATA:
            return len(self.rodata)
        return self.bss_len


@dataclass
class CacheFile:
    lift_slide: int
    # Where the image was read from when the file was built; None in `RMAOTv1` files.
    source: Optional[str] = None
    chunks: List[Image] = field(default_factory=list)


class _Reader:
    def __init__(self, data, at: int = 0, end: Optional[int] = None):
        self.data = data
        self.at = at
        self.end = len(data) if end is None else end

    def take(self, n: int) -> bytes:
        if n < 0 or self.at + n > self.end:
            raise FormatError("truncated")
        out = bytes(self.data[self.at : self.at + n])
        self.at += n
        return out

    def u8(self) -> int:
        return self.take(1)[0]

    def u32(self) -> int:
        return struct.unpack("<I", self.take(4))[0]

    def u64(self) -> int:
        return struct.unpack("<Q", self.take(8))[0]

    def i64(self) -> int:
        return struct.unpack("<q", self.take(8))[0]

    def length(self) -> int:
        n = self.u64()
        if n > self.end - self.at:
            raise FormatError("length exceeds the file")
        return n

    def blob(self) -> bytes:
        return self.take(self.length())

    def str(self) -> str:
        try:
            return self.blob().decode("utf-8")
        except UnicodeDecodeError as e:
            raise FormatError("string is not UTF-8") from e

    def records(self, fmt: str) -> list:
        """A counted list of fixed-size records, unpacked in one pass."""
        n = self.length()
        size = struct.calcsize(fmt)
        return list(struct.iter_unpack(fmt, self.take(n * size)))

    def symbol(self) -> ImageSymbol:
        tag = self.u8()
        if tag == 0:
            return GuestSlide()
        if tag not in (1, 2, 3):
            raise FormatError(f"unknown symbol kind {tag}")
        signature = self.str() if tag == HostSymbolKind.FUNCTION else None
        provider = self.str()
        name = self.str()
        return HostSymbol(HostSymbolKind(tag), provider, name, signature)

    def reloc_target(self) -> Tuple:
        tag = self.u8()
        if tag == 0:
            return ("symbol", self.u32())
        if tag not in (1, 2, 3):
            raise FormatError(f"unknown relocation target {tag}")
        return ("local", Section(tag), self.u64())

    def decision(self) -> ImportDecision:
        tag = self.u8()
        if tag == DecisionKind.FAST:
            return ImportDecision(DecisionKind.FAST, self.str(), self.str(), self.str())
        if tag == DecisionKind.BRIDGE:
            return ImportDecision(DecisionKind.BRIDGE, name=self.str())
        if tag in (DecisionKind.NOT_IMPORT, DecisionKind.SLOW):
            return ImportDecision(DecisionKind(tag))
        raise FormatError(f"unknown import decision {tag}")

    def region(self) -> Region:
        lift_base = self.u64()
        entry = self.u64()
        link_target = self.u64()
        cell_tag = self.u8()
        if cell_tag not in (0, 1):
            raise FormatError("bad cell tag")
        cells = self.u64() if cell_tag else None
        resume_points = [r[0] for r in self.records("<Q")]
        covered = self.records("<QQ")
        data_dependencies = self.records("<QQ")
        link_sites = [(t, LinkSiteKind(k)) for t, k in self.records("<QB")]
        import_decisions = [(self.u64(), self.decision()) for _ in range(self.length())]
        code_map = self.take(self.length() * CODE_RANGE_SIZE)
        return Region(
            lift_base,
            entry,
            link_target,
            cells,
            resume_points,
            covered,
            data_dependencies,
            link_sites,
            import_decisions,
            code_map,
        )


def _decode_image(r: _Reader) -> Image:
    if r.take(len(IMAGE_MAGIC)) != IMAGE_MAGIC:
        raise FormatError("not a microlift image")
    version = r.u32()
    if version != IMAGE_FORMAT_VERSION:
        raise FormatError(f"image format version {version}, this plugin reads {IMAGE_FORMAT_VERSION}")
    isa_fingerprint = r.u64()
    config_fingerprint = r.u64()
    text = r.blob()
    rodata = r.blob()
    rodata_align = r.u64()
    bss_len = r.u64()
    symbols = [r.symbol() for _ in range(r.length())]
    relocs = [Reloc(r.u64(), r.reloc_target(), r.i64()) for _ in range(r.length())]
    regions = [r.region() for _ in range(r.length())]
    if r.at != r.end:
        raise FormatError("trailing bytes in image")
    image = Image(
        isa_fingerprint, config_fingerprint, text, rodata, rodata_align, bss_len, symbols, relocs, regions
    )
    _validate(image)
    return image


def _validate(image: Image) -> None:
    text = len(image.text)
    for reloc in image.relocs:
        if reloc.offset + 8 > text:
            raise FormatError("relocation outside .text")
        if reloc.target[0] == "local":
            _, section, offset = reloc.target
            if offset > image.section_len(section):
                raise FormatError("relocation target outside its section")
        elif reloc.target[1] >= len(image.symbols):
            raise FormatError("relocation names no symbol")
    if image.rodata_align == 0 or image.rodata_align & (image.rodata_align - 1):
        raise FormatError(".rodata alignment is not a power of two")
    for region in image.regions:
        if region.entry >= text or region.link_target >= text:
            raise FormatError("region entry outside .text")
        if (region.cells is None) != (not region.link_sites):
            raise FormatError("region cells disagree with its link sites")
        if region.cells is not None and region.cells + len(region.link_sites) * LINK_ENTRY_SIZE > image.bss_len:
            raise FormatError("region cells outside .bss")


def is_cache_file(data) -> bool:
    return bytes(data[: len(CACHE_MAGIC)]) in (CACHE_MAGIC, CACHE_MAGIC_V1)


def decode(data) -> CacheFile:
    """Decodes a whole cache file. `data` is any sliceable byte sequence."""
    if not is_cache_file(data):
        raise FormatError("not a Rosemary cache file")
    has_source = bytes(data[: len(CACHE_MAGIC)]) == CACHE_MAGIC
    r = _Reader(data, len(CACHE_MAGIC))
    lift_slide = r.u64()
    count = r.u64()
    out = CacheFile(lift_slide, r.str() if has_source else None)
    for _ in range(count):
        length = r.u64()
        if length > r.end - r.at:
            raise FormatError("truncated chunk")
        out.chunks.append(_decode_image(_Reader(data, r.at, r.at + length)))
        r.at += length
    if r.at != r.end:
        raise FormatError("trailing bytes after the last chunk")
    return out


def guess_arch(image: Image) -> str:
    """`"aarch64"` or `"x86_64"`, from the code itself.

    `isa_fingerprint` is a hash, so the ISA cannot be read from the file. AArch64 code is
    word-aligned and Cranelift ends nearly every function with `ret` (0xd65f03c0).
    """
    aligned = memoryview(image.text)[: len(image.text) & ~3]
    hits = sum(1 for (word,) in struct.iter_unpack("<I", aligned) if word == AARCH64_RET)
    return "aarch64" if hits * 2 >= max(1, len(image.regions)) else "x86_64"


if __name__ == "__main__":
    import sys

    for path in sys.argv[1:]:
        with open(path, "rb") as f:
            cache = decode(f.read())
        source = cache.source or "unknown source"
        print(f"{path}: {source}, lift slide {cache.lift_slide:#x}, {len(cache.chunks)} chunk(s)")
        for i, image in enumerate(cache.chunks):
            print(
                f"  chunk {i}: isa {image.isa_fingerprint:016x} config {image.config_fingerprint:016x} "
                f"arch~{guess_arch(image)} text {len(image.text):#x} rodata {len(image.rodata):#x} "
                f"bss {image.bss_len:#x} symbols {len(image.symbols)} relocs {len(image.relocs)} "
                f"regions {len(image.regions)} map ranges {sum(r.map_len for r in image.regions)}"
            )
