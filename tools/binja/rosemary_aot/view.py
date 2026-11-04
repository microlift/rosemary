"""The `RosemaryAOT` BinaryView: a Rosemary cache file as relocated, symbolized host code."""

import traceback
from typing import Dict, List, Optional, Tuple

from binaryninja import (
    Architecture,
    BinaryView,
    Platform,
    SectionSemantics,
    SegmentFlag,
    Settings,
    Symbol,
    SymbolType,
    Type,
    log,
)
from binaryninja.enums import NamedTypeReferenceClass, StructureVariant
from binaryninja.types import FunctionParameter, StructureBuilder

from . import context as cx
from . import layout as lay
from . import rmc

SETTINGS_GROUP = "rosemaryAot"
SETTING_ARCH = "rosemaryAot.architecture"
SETTING_GUEST_ADDRESSES = "rosemaryAot.guestAddresses"
SETTING_ANNOTATION_LIMIT = "rosemaryAot.annotationLimit"

# Cranelift calling-convention names -> Binary Ninja platform, per architecture.
PLATFORMS = {
    ("aarch64", "apple_aarch64"): "mac-aarch64",
    ("aarch64", "system_v"): "linux-aarch64",
    ("x86_64", "system_v"): "linux-x86_64",
    ("x86_64", "windows_fastcall"): "windows-x86_64",
}
DEFAULT_PLATFORMS = {"aarch64": "linux-aarch64", "x86_64": "linux-x86_64"}



def register_settings() -> None:
    settings = Settings()
    settings.register_group(SETTINGS_GROUP, "Rosemary AOT")
    settings.register_setting(
        SETTING_ARCH,
        """{
            "title": "Host architecture",
            "type": "string",
            "default": "auto",
            "enum": ["auto", "aarch64", "x86_64"],
            "description": "ISA the cache was compiled for. Cache files record only a hash of it, so 'auto' guesses from the code.",
            "ignore": ["SettingsProjectScope"]
        }""",
    )
    settings.register_setting(
        SETTING_GUEST_ADDRESSES,
        """{
            "title": "Guest address space",
            "type": "string",
            "default": "file",
            "enum": ["file", "lift"],
            "enumDescriptions": [
                "Guest addresses are the image's file VAs, matching the Mach-O opened on its own.",
                "Guest addresses are run-time VAs at the slide the image was lifted at."
            ],
            "description": "What the GuestSlide relocation resolves to, and how guest addresses are named.",
            "ignore": ["SettingsProjectScope"]
        }""",
    )
    settings.register_setting(
        SETTING_ANNOTATION_LIMIT,
        """{
            "title": "Annotation limit",
            "type": "number",
            "default": 250000,
            "minValue": 0,
            "maxValue": 100000000,
            "description": "Per-instruction guest comments and relocation data variables are added on load only when a file has at most this many of them; use 'Rosemary AOT > Annotate guest addresses' for single functions otherwise.",
            "ignore": ["SettingsProjectScope"]
        }""",
    )


def _scalar(name: str) -> Optional[Type]:
    if name.startswith("i") and name[1:].isdigit():
        return Type.int(int(name[1:]) // 8, sign=True)
    if name in ("f32", "f64"):
        return Type.float(int(name[1:]) // 8)
    return None


def host_function_type(signature: str) -> Optional[Type]:
    parsed = lay.parse_signature(signature)
    if parsed is None:
        return None
    params, returns, _ = parsed
    if len(returns) > 1:
        return None
    param_types = [_scalar(p) for p in params]
    ret = _scalar(returns[0]) if returns else Type.void()
    if ret is None or any(t is None for t in param_types):
        return None
    return Type.function(ret, param_types)


def _calling_convention(cache: rmc.CacheFile) -> Optional[str]:
    for image in cache.chunks:
        for symbol in image.symbols:
            if isinstance(symbol, rmc.HostSymbol) and symbol.signature:
                parsed = lay.parse_signature(symbol.signature)
                if parsed:
                    return parsed[2]
    return None


def _ranges(ranges, guest) -> str:
    return ", ".join(f"{guest(a):#x}-{guest(b):#x}" for a, b in ranges)


class RosemaryAotView(BinaryView):
    name = "RosemaryAOT"
    long_name = "Rosemary AOT cache"

    @classmethod
    def is_valid_for_data(cls, data) -> bool:
        return rmc.is_cache_file(data.read(0, len(rmc.CACHE_MAGIC)))

    def __init__(self, data):
        settings = Settings()
        self.cache = rmc.decode(data.read(0, data.length))
        lift = settings.get_string(SETTING_GUEST_ADDRESSES, data) == "lift"
        slide = 0 if lift else lay.file_slide(self.cache)
        self.layout = lay.build(self.cache, guest_slide=slide)
        self.annotation_limit = settings.get_integer(SETTING_ANNOTATION_LIMIT, data)
        arch = settings.get_string(SETTING_ARCH, data)
        if arch == "auto":
            arch = rmc.guess_arch(self.cache.chunks[0]) if self.cache.chunks else "aarch64"
        self.arch_name = arch
        BinaryView.__init__(self, file_metadata=data.file, parent_view=data)
        # Entry and body address -> (chunk index, region index).
        self.function_starts: Dict[int, Tuple[int, int]] = {}
        self.body_types: Dict[cx.PinnedAbi, Optional[Tuple[Type, List[str]]]] = {}

    def guest(self, lift_va: int) -> int:
        return lay.guest_va(self.layout, lift_va)

    def init(self) -> bool:
        try:
            self.arch = Architecture[self.arch_name]
            platform = PLATFORMS.get((self.arch_name, _calling_convention(self.cache)))
            self.platform = Platform[platform or DEFAULT_PLATFORMS[self.arch_name]]
            self._add_segments()
            self._add_types()
            self._add_externs()
            self._add_regions()
            self._store_metadata()
            if self.cache.source:
                log.log_info(f"Rosemary AOT: cache of {self.cache.source}")
            return True
        except Exception:
            log.log_error(f"Rosemary AOT: failed to load cache file\n{traceback.format_exc()}")
            return False

    def _add_segments(self) -> None:
        code = SegmentFlag.SegmentReadable | SegmentFlag.SegmentExecutable | SegmentFlag.SegmentContainsCode
        data = SegmentFlag.SegmentReadable | SegmentFlag.SegmentContainsData
        bss = data | SegmentFlag.SegmentWritable
        blob = self.layout.blob
        memory = self.memory_map
        # The relocated bytes exist only here, not in the file, so each section is a
        # data-backed memory region rather than a segment over the parent view.
        for i, (image, placed) in enumerate(zip(self.cache.chunks, self.layout.chunks)):
            text = bytes(blob[placed.text_offset : placed.text_offset + len(image.text)])
            memory.add_memory_region(f"chunk{i}.text", placed.text, text, code)
            self.add_auto_section(
                f"chunk{i}.text", placed.text, len(image.text), SectionSemantics.ReadOnlyCodeSectionSemantics
            )
            if image.rodata:
                rodata = bytes(blob[placed.rodata_offset : placed.rodata_offset + len(image.rodata)])
                memory.add_memory_region(f"chunk{i}.rodata", placed.rodata, rodata, data)
                self.add_auto_section(
                    f"chunk{i}.rodata",
                    placed.rodata,
                    len(image.rodata),
                    SectionSemantics.ReadOnlyDataSectionSemantics,
                )
            if image.bss_len:
                memory.add_memory_region(f"chunk{i}.bss", placed.bss, None, bss, length=image.bss_len)
                self.add_auto_section(
                    f"chunk{i}.bss", placed.bss, image.bss_len, SectionSemantics.ReadWriteDataSectionSemantics
                )
        extern_len = self.layout.end - self.layout.extern_start
        memory.add_memory_region("extern", self.layout.extern_start, None, SegmentFlag.SegmentReadable, length=extern_len)
        self.add_auto_section("extern", self.layout.extern_start, extern_len, SectionSemantics.ExternalSectionSemantics)

    def _named(self, name: str) -> Type:
        if name in cx.ENUMS:
            enum = cx.ENUMS[name]
            enum_class = NamedTypeReferenceClass.EnumNamedTypeClass
            return Type.named_type_reference(enum_class, name, name, enum.size, enum.size)
        layout = cx.layout_of(name)
        variant = NamedTypeReferenceClass.StructNamedTypeClass
        if cx.TYPES[name].union:
            variant = NamedTypeReferenceClass.UnionNamedTypeClass
        return Type.named_type_reference(variant, name, name, layout.align, layout.size)

    def _type(self, spec: cx.Spec) -> Type:
        if isinstance(spec, cx.Int):
            return Type.int(spec.size, sign=spec.signed)
        if isinstance(spec, cx.Float):
            return Type.float(spec.size)
        if isinstance(spec, cx.Named):
            return self._named(spec.name)
        if isinstance(spec, cx.Pointer):
            return Type.pointer(self.arch, Type.void() if spec.target is None else self._type(spec.target))
        if isinstance(spec, cx.FunctionPointer):
            params = [FunctionParameter(self._type(t), name) for t, name in spec.params]
            return Type.pointer(self.arch, Type.function(Type.void(), params))
        return Type.array(self._type(spec.element), spec.count)

    def _add_types(self) -> None:
        for name, enum in cx.ENUMS.items():
            self.define_type(name, name, Type.enumeration(self.arch, list(enum.members), enum.size))
        for name, aggregate in cx.TYPES.items():
            layout = cx.layout_of(name)
            builder = StructureBuilder.create(
                type=StructureVariant.UnionStructureType if aggregate.union else StructureVariant.StructStructureType
            )
            for field, offset in layout.offsets:
                builder.insert(offset, self._type(field.spec), field.name, overwrite_existing=False)
            builder.width = layout.size
            builder.alignment = layout.align
            self.define_type(name, name, Type.structure_type(builder))
        self.context_pointer = self._type(cx.Pointer(cx.Named(cx.REGISTER_CONTEXT)))

    def _add_externs(self) -> None:
        for extern in self.layout.externs:
            host = extern.symbol
            if host.kind == rmc.HostSymbolKind.DATA:
                sym = Symbol(SymbolType.ExternalSymbol, extern.addr, extern.display)
                self.define_auto_symbol_and_var_or_function(sym, Type.int(8, sign=False), self.platform)
            else:
                sym = Symbol(SymbolType.ExternalSymbol, extern.addr, extern.display)
                func_type = host_function_type(host.signature) if host.signature else None
                self.define_auto_symbol_and_var_or_function(sym, func_type, self.platform)
            provider = host.provider or "rosemary runtime"
            if host.kind == rmc.HostSymbolKind.LIBCALL:
                provider = "Cranelift libcall"
            note = f"{provider}" + (f"\n{host.signature}" if host.signature else "")
            self.set_comment_at(extern.addr, note)

    def _add_regions(self) -> None:
        annotations = sum(len(image.relocs) + sum(r.map_len for r in image.regions) for image in self.cache.chunks)
        annotate = annotations <= self.annotation_limit
        if not annotate:
            log.log_info(
                f"Rosemary AOT: {annotations} annotations exceed the limit of {self.annotation_limit}; "
                "use 'Rosemary AOT > Annotate guest addresses' per function"
            )
        entry_type = Type.function(Type.void(), [FunctionParameter(self.context_pointer, "ctx")])
        link_entry = self._named(cx.LINK_ENTRY)
        for c, (image, placed) in enumerate(zip(self.cache.chunks, self.layout.chunks)):
            for r, region in enumerate(image.regions):
                name = f"region_{self.guest(region.lift_base):x}"
                entry = placed.text + region.entry
                body = placed.text + region.link_target
                self.define_auto_symbol_and_var_or_function(
                    Symbol(SymbolType.FunctionSymbol, entry, name), entry_type, self.platform
                )
                if body != entry:
                    self._add_body(image, region, body, f"{name}_body")
                self.set_comment_at(body, self.describe_region(region))
                self.function_starts[entry] = self.function_starts[body] = (c, r)
                if region.cells is not None:
                    cells = placed.bss + region.cells
                    self.define_auto_symbol(Symbol(SymbolType.DataSymbol, cells, f"cells_{self.guest(region.lift_base):x}"))
                    self.define_data_var(cells, Type.array(link_entry, len(region.link_sites)))
                    for i, (target, kind) in enumerate(region.link_sites):
                        self.set_comment_at(
                            cells + i * rmc.LINK_ENTRY_SIZE, f"-> guest {self.guest(target):#x} ({kind.name.lower()})"
                        )
                if annotate:
                    self.annotate_region(c, r)
            if annotate:
                pointer = Type.int(8, sign=False)
                for reloc in image.relocs:
                    self.define_data_var(placed.text + reloc.offset, pointer)
        if self.function_starts:
            entry = self.perform_get_entry_point()
            self.add_entry_point(entry)
            if self.cache.source:
                self.set_comment_at(entry, f"compiled from {self.cache.source}")

    def _add_body(self, image: rmc.Image, region: rmc.Region, body: int, name: str) -> None:
        """Adds a body called through an entry thunk. Its context and pinned guest registers arrive,
        and leave, in the registers the thunk loads them into rather than where the platform ABI says."""
        typed = None
        abi = cx.decode_aarch64_thunk(image.text, region.entry) if self.arch_name == "aarch64" else None
        if abi is not None:
            if abi not in self.body_types:
                self.body_types[abi] = self._pinned_body_type(abi)
            typed = self.body_types[abi]
        symbol = Symbol(SymbolType.FunctionSymbol, body, name)
        self.define_auto_symbol_and_var_or_function(symbol, typed[0] if typed else None, self.platform)
        function = self.get_function_at(body, self.platform)
        if function is not None and typed:
            function.set_auto_return_regs(typed[1])

    def _pinned_body_type(self, abi: cx.PinnedAbi) -> Optional[Tuple[Type, List[str]]]:
        params = [f"{cx.REGISTER_CONTEXT}* ctx @ {abi.context}"]
        params += [f"{cx.XMM_REGISTER if s.vector else 'uint64_t'} {s.guest} @ {s.register}" for s in abi.slots]
        try:
            parsed, _ = self.parse_type_string(f"void body({', '.join(params)})")
        except SyntaxError as error:
            log.log_warn(f"Rosemary AOT: cannot type region bodies for pinned ABI {abi}: {error}")
            return None
        # FIXME (ACCURACY): a body returns its pinned registers in the registers they arrived in.
        # Binary Ninja cannot type that, so they are only marked as return registers: callers see
        # them come back, but the body's own final values for them are not shown as results.
        return parsed, [s.register for s in abi.slots]

    def describe_region(self, region: rmc.Region) -> str:
        guest = self.guest
        lines = [f"guest {guest(region.lift_base):#x}", f"covered: {_ranges(region.covered, guest)}"]
        if region.resume_points:
            lines.append("resume points: " + ", ".join(f"{guest(p):#x}" for p in region.resume_points))
        if region.data_dependencies:
            lines.append(f"data dependencies: {_ranges(region.data_dependencies, guest)}")
        for i, (target, kind) in enumerate(region.link_sites):
            lines.append(f"link cell {i}: {guest(target):#x} ({kind.name.lower()})")
        for va, decision in region.import_decisions:
            lines.append(f"import at {guest(va):#x}: {decision.describe()}")
        return "\n".join(lines)

    def annotate_region(self, chunk: int, index: int) -> int:
        """Comments each host run with the guest instruction it came from. Returns the count."""
        region = self.cache.chunks[chunk].regions[index]
        body = self.layout.chunks[chunk].text + region.link_target
        previous = None
        count = 0
        for start, _end, va in sorted(region.map):
            if va == previous:
                continue
            previous = va
            addr = body + start
            if addr != body:
                self.set_comment_at(addr, f"guest {self.guest(va):#x}")
                count += 1
        return count

    def region_at(self, addr: int) -> Optional[Tuple[int, int]]:
        """The (chunk, region) whose entry or body function holds `addr`."""
        for function in self.get_functions_containing(addr) or []:
            found = self.function_starts.get(function.start)
            if found is not None:
                return found
        return None

    def regions_covering(self, guest_va: int) -> List[Tuple[int, int]]:
        found = []
        for c, image in enumerate(self.cache.chunks):
            for r, region in enumerate(image.regions):
                if any(self.guest(a) <= guest_va < self.guest(b) for a, b in region.covered):
                    found.append((c, r))
        return found

    def _store_metadata(self) -> None:
        if self.cache.source:
            self.store_metadata("rosemary_aot.source", self.cache.source, isAuto=True)
        self.store_metadata("rosemary_aot.lift_slide", self.cache.lift_slide, isAuto=True)
        self.store_metadata("rosemary_aot.guest_slide", self.layout.guest_slide, isAuto=True)
        fingerprints = sorted({(i.isa_fingerprint, i.config_fingerprint) for i in self.cache.chunks})
        self.store_metadata(
            "rosemary_aot.fingerprints", [f"isa {a:016x} config {b:016x}" for a, b in fingerprints], isAuto=True
        )

    def perform_is_executable(self) -> bool:
        return True

    def perform_get_address_size(self) -> int:
        return 8

    def perform_get_entry_point(self) -> int:
        if self.cache.chunks and self.cache.chunks[0].regions:
            return self.layout.chunks[0].text + self.cache.chunks[0].regions[0].entry
        return self.layout.base
