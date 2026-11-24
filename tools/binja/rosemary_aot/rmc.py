"""Reader for Rosemary AOT cache files (`*.rmc`), independent of Binary Ninja.

Each file is one Microlift image (`src/image/file.rs`, `MLIMAGE2`) whose embedder section holds
Rosemary's data (`crates/rosemary/src/aot/file.rs`, `RMAOTv3`). An image's cache can span several
files, `<image-sha256>.<part>.rmc`; each part stands alone.

Only the header and the tables after the code are read up front. Region records are split into
columns in one pass, and a region's variable-length metadata is decoded only when asked for, as
Microlift's loader does. Little-endian throughout.
"""

import sys
from array import array
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Callable, Dict, List, Optional, Tuple, Union

IMAGE_MAGIC = b"MLIMAGE2"
IMAGE_FORMAT_VERSION = 2
CACHE_MAGIC = b"RMAOTv3\0"

# Microlift `image::file`.
PAGE = 16 * 1024
GOT_SPAN = 256 << 20
MAX_TEXT = 2 << 30
REGION_SLOTS = 16
HEADER_LEN = 512
RECORD_LEN = 40
ENTRY_LEN = 8
NO_CELLS = 0xFFFF_FFFF
GOT_ENTRY = 8
# Microlift `core::link::ENTRY_SIZE`: bytes per link cell.
LINK_ENTRY_SIZE = 32
# Microlift `core::symbol::RUNTIME_PROVIDER` / `LIBCALL_PROVIDER`.
RUNTIME_PROVIDER = ""
LIBCALL_PROVIDER = "cranelift"

AARCH64_RET = 0xD65F03C0
# Regions sampled to tell the host ISA from the code.
ARCH_SAMPLE = 64

U64_MASK = (1 << 64) - 1

# `ReadAt(offset, length) -> bytes`: how an image reaches the file.
ReadAt = Callable[[int, int], bytes]


class FormatError(Exception):
    pass


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


class Tier(IntEnum):
    FINAL = 0
    PROVISIONAL = 1


_SHARED_GUEST_SLIDE = 0
_SHARED_TEXT = 4


@dataclass(frozen=True)
class GuestSlide:
    pass


@dataclass(frozen=True)
class HostSymbol:
    kind: HostSymbolKind
    provider: str
    name: str
    signature: Optional[str] = None


@dataclass(frozen=True)
class TextEntry:
    """A helper or table, by its offset in the code."""

    offset: int


SharedEntry = Union[GuestSlide, HostSymbol, TextEntry]


@dataclass(frozen=True)
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
class RegionMeta:
    """What a region was lifted from, in lift-time VAs. `map` holds (start, end, guest_va) host
    runs, offsets relative to the body."""

    lift_base: int
    shared: List[int] = field(default_factory=list)
    resume_points: List[int] = field(default_factory=list)
    covered: List[Tuple[int, int]] = field(default_factory=list)
    data_dependencies: List[Tuple[int, int]] = field(default_factory=list)
    link_sites: List[Tuple[int, LinkSiteKind]] = field(default_factory=list)
    import_decisions: List[Tuple[int, ImportDecision]] = field(default_factory=list)
    map: List[Tuple[int, int, int]] = field(default_factory=list)


@dataclass(frozen=True)
class Section:
    offset: int
    len: int

    @property
    def end(self) -> int:
        return self.offset + self.len


_SECTIONS = ("shared", "sites", "regions", "entries", "meta", "signature", "user")
_HEADER_WORDS = (
    "isa_fingerprint",
    "config_fingerprint",
    "text_offset",
    "text_len",
    "bss_len",
    "shared_count",
    "region_count",
    "entry_count",
    "entry_base",
)


@dataclass(frozen=True)
class Header:
    isa_fingerprint: int
    config_fingerprint: int
    text_offset: int
    text_len: int
    bss_len: int
    shared_count: int
    region_count: int
    entry_count: int
    entry_base: int
    sections: Dict[str, Section]

    @staticmethod
    def decode(data: bytes) -> "Header":
        if len(data) < HEADER_LEN:
            raise FormatError("truncated header")
        if data[:8] != IMAGE_MAGIC:
            raise FormatError("not a microlift image")
        version = int.from_bytes(data[8:12], "little")
        if version != IMAGE_FORMAT_VERSION:
            raise FormatError(f"image format version {version}, this plugin reads {IMAGE_FORMAT_VERSION}")
        words = _u64s(data[16 : 16 + 8 * (len(_HEADER_WORDS) + 2 * len(_SECTIONS))])
        fields = dict(zip(_HEADER_WORDS, words))
        rest = words[len(_HEADER_WORDS) :]
        sections = {name: Section(rest[2 * i], rest[2 * i + 1]) for i, name in enumerate(_SECTIONS)}
        return Header(sections=sections, **fields)

    def validate(self, file_len: int) -> None:
        inside = lambda offset, length: offset + length <= file_len
        if self.text_offset % PAGE or not inside(self.text_offset, self.text_len):
            raise FormatError("code outside the file")
        if self.text_len > MAX_TEXT:
            raise FormatError("more code than one file may hold")
        if any(not inside(s.offset, s.len) for s in self.sections.values()):
            raise FormatError("a table outside the file")
        if (
            self.sections["regions"].len != self.region_count * RECORD_LEN
            or self.sections["entries"].len != self.entry_count * ENTRY_LEN
        ):
            raise FormatError("table lengths disagree with their counts")
        if self.shared_count * GOT_ENTRY + self.region_count * REGION_SLOTS > GOT_SPAN:
            raise FormatError("more GOT entries than the span holds")


@dataclass(frozen=True)
class Embedded:
    """Rosemary's data in the image's embedder section."""

    lift_slide: int
    profiled: int
    tier: Tier
    source: str

    @staticmethod
    def decode(data: bytes) -> "Embedded":
        if data[: len(CACHE_MAGIC)] != CACHE_MAGIC:
            raise FormatError("not a Rosemary cache file")
        at = len(CACHE_MAGIC)
        if len(data) < at + 17:
            raise FormatError("truncated Rosemary data")
        lift_slide, profiled = _u64s(data[at : at + 16])
        if data[at + 16] not in (Tier.FINAL, Tier.PROVISIONAL):
            raise FormatError("unknown tier")
        try:
            source = data[at + 17 :].decode("utf-8")
        except UnicodeDecodeError as e:
            raise FormatError("source path is not UTF-8") from e
        return Embedded(lift_slide, profiled, Tier(data[at + 16]), source)


def _u64s(data: bytes) -> array:
    out = array("Q", data)
    if sys.byteorder != "little":
        out.byteswap()
    return out


def _u32s(data: bytes) -> array:
    out = array("I", data)
    if sys.byteorder != "little":
        out.byteswap()
    return out


class _Varints:
    """LEB128 integers, zigzag for signed ones, as Microlift's `image::varint` writes them."""

    __slots__ = ("data", "at", "end")

    def __init__(self, data: bytes, at: int = 0, end: Optional[int] = None):
        self.data = data
        self.at = at
        self.end = len(data) if end is None else end

    def get(self) -> int:
        data, at = self.data, self.at
        if at >= self.end:
            raise FormatError("truncated integer")
        byte = data[at]
        if byte < 0x80:
            self.at = at + 1
            return byte
        value = byte & 0x7F
        shift = 7
        while True:
            at += 1
            if at >= self.end:
                raise FormatError("truncated integer")
            if shift >= 64:
                raise FormatError("integer too long")
            byte = data[at]
            value |= (byte & 0x7F) << shift
            if byte < 0x80:
                self.at = at + 1
                return value & U64_MASK
            shift += 7

    def signed(self) -> int:
        value = self.get()
        return (value >> 1) ^ -(value & 1)

    def count(self) -> int:
        n = self.get()
        if n > self.end - self.at:
            raise FormatError("count exceeds the record")
        return n

    def byte(self) -> int:
        if self.at >= self.end:
            raise FormatError("truncated record")
        self.at += 1
        return self.data[self.at - 1]

    def str(self) -> str:
        n = self.count()
        raw = self.data[self.at : self.at + n]
        self.at += n
        try:
            return bytes(raw).decode("utf-8")
        except UnicodeDecodeError as e:
            raise FormatError("string is not UTF-8") from e

    def done(self) -> bool:
        return self.at == self.end


class Records:
    """Every region's fixed-size record, one list per field."""

    def __init__(self, raw: bytes, count: int):
        words = _u64s(raw)
        halves = _u32s(raw)
        per_u64, per_u32 = RECORD_LEN // 8, RECORD_LEN // 4
        self.count = count
        self.lift_base = words[0::per_u64].tolist()
        self.meta_offset = words[1::per_u64].tolist()
        self.code = halves[4::per_u32].tolist()
        self.code_len = halves[5::per_u32].tolist()
        self.entry = halves[6::per_u32].tolist()
        self.link_target = halves[7::per_u32].tolist()
        self.cells = halves[8::per_u32].tolist()
        self.meta_len = halves[9::per_u32].tolist()

    def has_cells(self, index: int) -> bool:
        return self.cells[index] != NO_CELLS


class Image:
    """One cache file, read in place through `read`."""

    def __init__(self, read: ReadAt, file_len: int):
        self.read = read
        self.file_len = file_len
        self.header = header = Header.decode(read(0, HEADER_LEN))
        header.validate(file_len)
        # Everything but the code sits after it; one read brings in every table.
        tables = [s for s in header.sections.values() if s.len]
        self._tail_start = min((s.offset for s in tables), default=file_len)
        self._tail = read(self._tail_start, file_len - self._tail_start) if tables else b""
        self.embedded = Embedded.decode(self._section("user"))
        self.shared = self._decode_shared()
        self._sites: Optional[List[Tuple[int, ImportDecision]]] = None
        self.records = Records(self._section("regions"), header.region_count)
        self._validate_records()
        self.cell_counts = self._cell_counts()

    @property
    def lift_slide(self) -> int:
        return self.embedded.lift_slide

    @property
    def source(self) -> str:
        return self.embedded.source

    @property
    def region_count(self) -> int:
        return self.header.region_count

    def _section(self, name: str) -> bytes:
        s = self.header.sections[name]
        if not s.len:
            return b""
        start = s.offset - self._tail_start
        return self._tail[start : start + s.len]

    def _section_bounds(self, name: str) -> Tuple[int, int]:
        s = self.header.sections[name]
        start = s.offset - self._tail_start if s.len else 0
        return start, start + s.len

    def text(self, offset: int, length: int) -> bytes:
        if offset + length > self.header.text_len:
            raise FormatError("read past the code")
        return self.read(self.header.text_offset + offset, length)

    def _decode_shared(self) -> List[SharedEntry]:
        start, end = self._section_bounds("shared")
        r = _Varints(self._tail, start, end)
        out: List[SharedEntry] = []
        for _ in range(self.header.shared_count):
            tag = r.byte()
            if tag == _SHARED_GUEST_SLIDE:
                out.append(GuestSlide())
            elif tag == HostSymbolKind.FUNCTION:
                signature = r.str()
                out.append(HostSymbol(HostSymbolKind.FUNCTION, r.str(), r.str(), signature))
            elif tag in (HostSymbolKind.DATA, HostSymbolKind.LIBCALL):
                out.append(HostSymbol(HostSymbolKind(tag), r.str(), r.str()))
            elif tag == _SHARED_TEXT:
                offset = r.get()
                if offset >= self.header.text_len:
                    raise FormatError("a shared entry outside the code")
                out.append(TextEntry(offset))
            else:
                raise FormatError(f"unknown shared entry {tag}")
        return out

    @property
    def sites(self) -> List[Tuple[int, ImportDecision]]:
        """Every import site any region consulted, unslid, in the order regions name them."""
        if self._sites is None:
            start, end = self._section_bounds("sites")
            r = _Varints(self._tail, start, end)
            sites = []
            while not r.done():
                va = r.get()
                tag = r.byte()
                if tag == DecisionKind.FAST:
                    decision = ImportDecision(DecisionKind.FAST, r.str(), r.str(), r.str())
                elif tag == DecisionKind.BRIDGE:
                    decision = ImportDecision(DecisionKind.BRIDGE, name=r.str())
                elif tag in (DecisionKind.NOT_IMPORT, DecisionKind.SLOW):
                    decision = ImportDecision(DecisionKind(tag))
                else:
                    raise FormatError(f"unknown import decision {tag}")
                sites.append((va, decision))
            self._sites = sites
        return self._sites

    def _validate_records(self) -> None:
        h = self.header
        rec = self.records
        meta_len = h.sections["meta"].len
        if any(c + n > h.text_len for c, n in zip(rec.code, rec.code_len)):
            raise FormatError("region code outside the file")
        if any(e >= n for e, n in zip(rec.entry, rec.code_len)) or any(
            t >= n for t, n in zip(rec.link_target, rec.code_len)
        ):
            raise FormatError("region entry outside its code")
        if any(c != NO_CELLS and c > h.bss_len for c in rec.cells):
            raise FormatError("region cells outside the zeroed data")
        if any(o + n > meta_len for o, n in zip(rec.meta_offset, rec.meta_len)):
            raise FormatError("region metadata outside the file")

    def _cell_counts(self) -> List[int]:
        """Link cells per region. Writers hand out cell arrays in order and back to back, so each
        array runs to the next one (or to the end of the zeroed data)."""
        counts = [0] * self.region_count
        placed = sorted((c, i) for i, c in enumerate(self.records.cells) if c != NO_CELLS)
        ends = [c for c, _ in placed[1:]] + [self.header.bss_len]
        for (start, index), end in zip(placed, ends):
            counts[index] = (end - start) // LINK_ENTRY_SIZE
        return counts

    def _meta_reader(self, index: int) -> _Varints:
        rec = self.records
        start = self._section_bounds("meta")[0] + rec.meta_offset[index]
        return _Varints(self._tail, start, start + rec.meta_len[index])

    def covered(self, index: int) -> List[Tuple[int, int]]:
        """Region `index`'s covered ranges, decoding nothing after them."""
        r = self._meta_reader(index)
        for _ in range(r.count()):
            r.get()
        for _ in range(r.count()):
            r.signed()
        return _ranges(r, self.records.lift_base[index])

    def meta(self, index: int) -> RegionMeta:
        r = self._meta_reader(index)
        base = self.records.lift_base[index]
        meta = RegionMeta(base)
        for _ in range(r.count()):
            slot = r.get()
            if slot >= self.header.shared_count:
                raise FormatError("a region names no shared entry")
            meta.shared.append(slot)
        previous = base
        for _ in range(r.count()):
            previous = (previous + r.signed()) & U64_MASK
            meta.resume_points.append(previous)
        meta.covered = _ranges(r, base)
        meta.data_dependencies = _ranges(r, base)
        for _ in range(r.count()):
            target = (base + r.signed()) & U64_MASK
            kind = r.byte()
            if kind not in (LinkSiteKind.STATIC_EDGE, LinkSiteKind.CALL_RETURN):
                raise FormatError("unknown link site kind")
            meta.link_sites.append((target, LinkSiteKind(kind)))
        sites = self.sites
        for _ in range(r.count()):
            site = r.get()
            if site >= len(sites):
                raise FormatError("a region names no import site")
            meta.import_decisions.append(sites[site])
        end = 0
        for _ in range(r.count()):
            start = end + r.signed()
            end = start + r.get()
            if start < 0 or end > 0xFFFF_FFFF:
                raise FormatError("bad map range")
            meta.map.append((start, end, (base + r.signed()) & U64_MASK))
        if not r.done():
            raise FormatError("trailing bytes in a region's metadata")
        if self.records.has_cells(index) == (not meta.link_sites):
            raise FormatError("a region's cells disagree with its link sites")
        return meta

    def lookup(self, pc: int) -> Optional[int]:
        """The first region that can be entered at `pc`, a lift-time VA."""
        delta = pc - self.header.entry_base
        if not 0 <= delta <= 0xFFFF_FFFF:
            return None
        if not hasattr(self, "_entries"):
            pairs = _u32s(self._section("entries"))
            self._entries = (pairs[0::2], pairs[1::2])
        pcs, regions = self._entries
        lo, hi = 0, len(pcs)
        while lo < hi:
            mid = (lo + hi) // 2
            if pcs[mid] < delta:
                lo = mid + 1
            else:
                hi = mid
        return regions[lo] if lo < len(pcs) and pcs[lo] == delta else None

    def regions_covering(self, pc: int) -> List[int]:
        """Every region whose covered ranges hold `pc`, a lift-time VA."""
        return [i for i in range(self.region_count) if any(a <= pc < b for a, b in self.covered(i))]

    def guess_arch(self) -> str:
        """`"aarch64"` or `"x86_64"`, from the code itself.

        `isa_fingerprint` is a hash, so the ISA cannot be read from the file. AArch64 code is
        word-aligned and Cranelift ends nearly every function with `ret` (0xd65f03c0).
        """
        rec = self.records
        sample = range(min(ARCH_SAMPLE, self.region_count))
        hits = 0
        for i in sample:
            code = self.text(rec.code[i], rec.code_len[i] & ~3)
            hits += AARCH64_RET in _u32s(code)
        return "aarch64" if hits * 2 >= max(1, len(sample)) else "x86_64"


def _ranges(r: _Varints, base: int) -> List[Tuple[int, int]]:
    out = []
    previous = base
    for _ in range(r.count()):
        start = (previous + r.signed()) & U64_MASK
        previous = (start + r.get()) & U64_MASK
        out.append((start, previous))
    return out


def is_cache_file(read: ReadAt, file_len: int) -> bool:
    """A Microlift image carrying Rosemary's data, judged from its header alone."""
    if read(0, len(IMAGE_MAGIC)) != IMAGE_MAGIC:
        return False
    try:
        header = Header.decode(read(0, HEADER_LEN))
        header.validate(file_len)
    except FormatError:
        return False
    user = header.sections["user"]
    return read(user.offset, min(user.len, len(CACHE_MAGIC))) == CACHE_MAGIC


def open_path(path: str) -> Image:
    """Maps the file at `path` and reads it in place."""
    import mmap

    with open(path, "rb") as f:
        mapped = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    return Image(lambda offset, length: mapped[offset : offset + length], len(mapped))


def decode(data: bytes) -> Image:
    """Reads a whole file already in memory."""
    view = memoryview(data)
    return Image(lambda offset, length: bytes(view[offset : offset + length]), len(data))


if __name__ == "__main__":
    import time

    for path in sys.argv[1:]:
        started = time.perf_counter()
        image = open_path(path)
        opened = time.perf_counter() - started
        h, e = image.header, image.embedded
        print(f"{path}: {e.source}, lift slide {e.lift_slide:#x}, {e.tier.name.lower()} tier, profiled {e.profiled}")
        print(
            f"  isa {h.isa_fingerprint:016x} config {h.config_fingerprint:016x} arch~{image.guess_arch()} "
            f"text {h.text_len:#x} bss {h.bss_len:#x} shared {h.shared_count} regions {h.region_count} "
            f"entries {h.entry_count} meta {h.sections['meta'].len:#x}; opened in {opened:.2f}s"
        )
