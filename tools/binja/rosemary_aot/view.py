"""The `RosemaryAOT` BinaryView: a Rosemary cache file as mapped, symbolized host code."""

import time
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
from binaryninja.enums import (
    InstructionTextTokenType,
    IntegerDisplayType,
    MediumLevelILOperation,
    NamedTypeReferenceClass,
    RegisterValueType,
    StructureVariant,
)
from binaryninja.types import FunctionParameter, NamedTypeReferenceType, PointerType, StructureBuilder

from . import context as cx
from . import layout as lay
from . import rmc

SETTINGS_GROUP = "rosemaryAot"
SETTING_ARCH = "rosemaryAot.architecture"
SETTING_GUEST_ADDRESSES = "rosemaryAot.guestAddresses"
SETTING_ANNOTATION_LIMIT = "rosemaryAot.annotationLimit"

# Enough of an entry thunk to reach its call to the body.
THUNK_READ = 512

EXIT_REASON_OFFSET = cx.control_offset("exit_reason")
EXIT_CODE_OFFSET = cx.control_offset("exit_code")
EXIT_FIELD_SIZE = EXIT_CODE_OFFSET - EXIT_REASON_OFFSET

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
            "default": 1000,
            "minValue": 0,
            "maxValue": 10000000,
            "description": "Region comments, per-instruction guest comments and exit names are added on load only when a file has at most this many regions; use 'Rosemary AOT > Annotate guest addresses and exits' for single functions otherwise.",
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


def _calling_convention(image: rmc.Image) -> Optional[str]:
    for entry in image.shared:
        if isinstance(entry, rmc.HostSymbol) and entry.signature:
            parsed = lay.parse_signature(entry.signature)
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
        return rmc.is_cache_file(data.read, data.length)

    def __init__(self, data):
        settings = Settings()
        self.image = rmc.Image(data.read, data.length)
        lift = settings.get_string(SETTING_GUEST_ADDRESSES, data) == "lift"
        slide = 0 if lift else lay.file_slide(self.image)
        self.layout = lay.build(self.image, guest_slide=slide)
        self.annotation_limit = settings.get_integer(SETTING_ANNOTATION_LIMIT, data)
        arch = settings.get_string(SETTING_ARCH, data)
        if arch == "auto":
            arch = self.image.guess_arch()
        self.arch_name = arch
        BinaryView.__init__(self, file_metadata=data.file, parent_view=data)
        # Entry and body address -> region index.
        self.function_starts: Dict[int, int] = {}
        self.body_types: Dict[cx.PinnedAbi, Optional[Tuple[Type, List[str]]]] = {}

    def guest(self, lift_va: int) -> int:
        return lay.guest_va(self.layout, lift_va)

    def lift(self, guest_va: int) -> int:
        return (guest_va - self.layout.guest_slide) & lay.U64_MASK

    def region_name(self, index: int) -> str:
        return f"region_{self.guest(self.image.records.lift_base[index]):x}"

    def body(self, index: int) -> int:
        rec = self.image.records
        return self.layout.text + rec.code[index] + rec.link_target[index]

    def init(self) -> bool:
        try:
            started = time.perf_counter()
            self.arch = Architecture[self.arch_name]
            platform = PLATFORMS.get((self.arch_name, _calling_convention(self.image)))
            self.platform = Platform[platform or DEFAULT_PLATFORMS[self.arch_name]]
            self._add_segments()
            self._add_types()
            with self.bulk_modify_symbols():
                self._add_got()
                self._add_regions()
            self._store_metadata()
            if self.annotate:
                for index in range(self.image.region_count):
                    self.annotate_region(index)
                self._exits_event = self.add_analysis_completion_event(self._annotate_all_exits)
            embedded = self.image.embedded
            log.log_info(
                f"Rosemary AOT: {self.image.region_count} regions of {embedded.source} "
                f"({embedded.tier.name.lower()} tier), loaded in {time.perf_counter() - started:.1f}s"
            )
            return True
        except Exception:
            log.log_error(f"Rosemary AOT: failed to load cache file\n{traceback.format_exc()}")
            return False

    def _add_segments(self) -> None:
        code = SegmentFlag.SegmentReadable | SegmentFlag.SegmentExecutable | SegmentFlag.SegmentContainsCode
        data = SegmentFlag.SegmentReadable | SegmentFlag.SegmentContainsData
        bss = data | SegmentFlag.SegmentWritable
        h, layout = self.image.header, self.layout
        read_only = SectionSemantics.ReadOnlyDataSectionSemantics
        # The code is never written at load, so it maps straight from the file. The GOT is the
        # loader's, so it is synthesized; read-only, so analysis folds what each entry holds.
        self.add_auto_segment(layout.text, h.text_len, h.text_offset, h.text_len, code)
        self.add_auto_section("text", layout.text, h.text_len, SectionSemantics.ReadOnlyCodeSectionSemantics)
        memory = self.memory_map
        if layout.shared_values:
            memory.add_memory_region("got.shared", layout.got, layout.shared_values, data)
            self.add_auto_section("got.shared", layout.got, len(layout.shared_values), read_only)
        if layout.own_values:
            memory.add_memory_region("got.regions", layout.own_got, layout.own_values, data)
            self.add_auto_section("got.regions", layout.own_got, len(layout.own_values), read_only)
        if h.bss_len:
            self.add_auto_segment(layout.bss, h.bss_len, 0, 0, bss)
            self.add_auto_section("cells", layout.bss, h.bss_len, SectionSemantics.ReadWriteDataSectionSemantics)
        extern_len = layout.end - layout.extern_start
        memory.add_memory_region("extern", layout.extern_start, None, SegmentFlag.SegmentReadable, length=extern_len)
        self.add_auto_section("extern", layout.extern_start, extern_len, SectionSemantics.ExternalSectionSemantics)

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

    def _add_got(self) -> None:
        """Names each shared GOT entry after what it holds, so calls through it read as calls to
        the host symbol."""
        word = Type.int(8, sign=False)
        for slot, (entry, extern) in enumerate(zip(self.image.shared, self.layout.slot_externs)):
            addr = self.layout.shared_slot(slot)
            if extern is None:
                if isinstance(entry, rmc.GuestSlide):
                    self.define_auto_symbol(Symbol(SymbolType.DataSymbol, addr, "__guest_slide"))
                    self.define_data_var(addr, word)
                else:
                    self.define_data_var(addr, Type.pointer(self.arch, Type.void()))
                continue
            host = extern.symbol
            func_type = None
            if host.kind != rmc.HostSymbolKind.DATA and host.signature:
                func_type = host_function_type(host.signature)
            target_type = word if host.kind == rmc.HostSymbolKind.DATA else func_type
            self.define_auto_symbol_and_var_or_function(
                Symbol(SymbolType.ExternalSymbol, extern.addr, extern.display), target_type, self.platform
            )
            self.define_auto_symbol(Symbol(SymbolType.ImportAddressSymbol, addr, extern.display))
            pointee = target_type if target_type is not None else Type.void()
            self.define_data_var(addr, Type.pointer(self.arch, pointee))
            provider = host.provider or "rosemary runtime"
            if host.kind == rmc.HostSymbolKind.LIBCALL:
                provider = "Cranelift libcall"
            self.set_comment_at(extern.addr, provider + (f"\n{host.signature}" if host.signature else ""))

    def _add_regions(self) -> None:
        image, layout, rec = self.image, self.layout, self.image.records
        self.annotate = image.region_count <= self.annotation_limit
        if not self.annotate:
            log.log_info(
                f"Rosemary AOT: {image.region_count} regions exceed the annotation limit of "
                f"{self.annotation_limit}; use 'Rosemary AOT > Annotate guest addresses and exits' per function"
            )
        entry_type = Type.function(Type.void(), [FunctionParameter(self.context_pointer, "ctx")])
        link_entry = self._named(cx.LINK_ENTRY)
        thunks = cx.ThunkDecoder() if self.arch_name == "aarch64" else None
        for index in range(image.region_count):
            name = self.region_name(index)
            code = layout.text + rec.code[index]
            entry = code + rec.entry[index]
            body = code + rec.link_target[index]
            self.define_auto_symbol_and_var_or_function(
                Symbol(SymbolType.FunctionSymbol, entry, name), entry_type, self.platform
            )
            if body != entry:
                abi = None
                if thunks is not None:
                    length = min(THUNK_READ, rec.code_len[index] - rec.entry[index])
                    abi = thunks.decode(image.text(rec.code[index] + rec.entry[index], length))
                self._add_body(body, f"{name}_body", abi)
            self.function_starts[entry] = self.function_starts[body] = index
            cells = image.cell_counts[index]
            if cells:
                at = layout.bss + rec.cells[index]
                self.define_auto_symbol(Symbol(SymbolType.DataSymbol, at, f"cells_{name[len('region_'):]}"))
                self.define_data_var(at, Type.array(link_entry, cells))
        if image.region_count:
            entry = self.perform_get_entry_point()
            self.add_entry_point(entry)
            self.set_comment_at(entry, f"compiled from {image.source}")

    def _add_body(self, body: int, name: str, abi: Optional[cx.PinnedAbi]) -> None:
        """Adds a body called through an entry thunk. Its context and pinned guest registers arrive,
        and leave, in the registers the thunk loads them into rather than where the platform ABI says."""
        typed = None
        if abi is not None:
            if abi not in self.body_types:
                self.body_types[abi] = self._pinned_body_type(abi)
            typed = self.body_types[abi]
        symbol = Symbol(SymbolType.FunctionSymbol, body, name)
        self.define_auto_symbol_and_var_or_function(symbol, typed[0] if typed else None, self.platform)
        if typed:
            function = self.get_function_at(body, self.platform)
            if function is not None:
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

    def describe_region(self, meta: rmc.RegionMeta) -> str:
        guest = self.guest
        lines = [f"guest {guest(meta.lift_base):#x}", f"covered: {_ranges(meta.covered, guest)}"]
        if meta.resume_points:
            lines.append("resume points: " + ", ".join(f"{guest(p):#x}" for p in meta.resume_points))
        if meta.data_dependencies:
            lines.append(f"data dependencies: {_ranges(meta.data_dependencies, guest)}")
        for i, (target, kind) in enumerate(meta.link_sites):
            lines.append(f"link cell {i}: {guest(target):#x} ({kind.name.lower()})")
        for va, decision in meta.import_decisions:
            lines.append(f"import at {guest(va):#x}: {decision.describe()}")
        return "\n".join(lines)

    def annotate_region(self, index: int) -> int:
        """Describes region `index` at its body, its link cells, and each host run with the guest
        instruction it came from. Returns the number of runs commented."""
        meta = self.image.meta(index)
        body = self.body(index)
        self.set_comment_at(body, self.describe_region(meta))
        if meta.link_sites:
            cells = self.layout.bss + self.image.records.cells[index]
            for i, (target, kind) in enumerate(meta.link_sites):
                comment = f"-> guest {self.guest(target):#x} ({kind.name.lower()})"
                self.set_comment_at(cells + i * rmc.LINK_ENTRY_SIZE, comment)
        previous = None
        count = 0
        for start, _end, va in sorted(meta.map):
            if va == previous:
                continue
            previous = va
            if start:
                self.add_comment(body + start, f"guest {self.guest(va):#x}")
                count += 1
        return count

    def add_comment(self, addr: int, line: str) -> None:
        """Adds a line to the comment at `addr`, keeping the other annotations there."""
        existing = self.get_comment_at(addr)
        if line not in existing.splitlines():
            self.set_comment_at(addr, f"{existing}\n{line}" if existing else line)

    def annotate_exits(self, function) -> int:
        """Names the constants a region stores into `ctx->control.exit_reason` and `exit_code`.

        An exit code is read under the reason stored before it in the same block. Returns the count."""
        mlil = None if function.analysis_skipped else function.mlil
        if mlil is None:
            return 0
        count = 0
        for block in mlil:
            reason = None
            for instr in block:
                for offset, value in self._control_stores(instr):
                    if offset == EXIT_REASON_OFFSET:
                        reason = value
                        name = cx.enum_member(cx.EXIT_REASON, value)
                        if name is not None:
                            self._name_exit_constant(function, instr, value, f"exit_reason = {name}", cx.EXIT_REASON)
                            count += 1
                    elif offset == EXIT_CODE_OFFSET and reason is not None:
                        code = cx.describe_exit_code(reason, value)
                        if code is not None:
                            self._name_exit_constant(function, instr, value, f"exit_code = {code.text}", code.enum)
                            count += 1
        if count:
            # Display types only reach the decompiler once the function is analyzed again.
            function.reanalyze()
        return count

    def _control_stores(self, instr) -> List[Tuple[int, int]]:
        """(context offset, constant) for each control field `instr` stores a known constant into."""
        if instr.operation != MediumLevelILOperation.MLIL_STORE_STRUCT or not self._is_context(instr.dest.expr_type):
            return []
        value = instr.src.value
        if value.type != RegisterValueType.ConstantValue:
            return []
        stores = []
        mask = (1 << (EXIT_FIELD_SIZE * 8)) - 1
        # A wide store can set both fields at once.
        for field in range(instr.size // EXIT_FIELD_SIZE):
            offset = instr.offset + field * EXIT_FIELD_SIZE
            if offset in (EXIT_REASON_OFFSET, EXIT_CODE_OFFSET):
                stores.append((offset, (value.value >> (field * EXIT_FIELD_SIZE * 8)) & mask))
        return stores

    @staticmethod
    def _is_context(pointer) -> bool:
        if not isinstance(pointer, PointerType):
            return False
        target = pointer.target
        # A resolved structure names itself through its registered reference.
        if not isinstance(target, NamedTypeReferenceType):
            target = target.registered_name
        return isinstance(target, NamedTypeReferenceType) and str(target.name) == cx.REGISTER_CONTEXT

    def _name_exit_constant(self, function, instr, value: int, comment: str, enum: Optional[str]) -> None:
        self.add_comment(instr.address, comment)
        hlil = instr.hlil
        if enum is None or hlil is None:
            return
        for token in hlil.tokens:
            if token.type == InstructionTextTokenType.IntegerToken and token.value == value:
                function.set_int_display_type(
                    token.address or hlil.address,
                    value,
                    token.operand,
                    IntegerDisplayType.EnumerationDisplayType,
                    enum_display_typeid=enum,
                )

    def annotate_region_exits(self, index: int) -> int:
        """`annotate_exits` for a region's entry and body. Returns the count."""
        rec = self.image.records
        code = self.layout.text + rec.code[index]
        return sum(self._annotate_exits_at(start) for start in {code + rec.entry[index], code + rec.link_target[index]})

    def _annotate_exits_at(self, start: int) -> int:
        function = self.get_function_at(start, self.platform)
        return self.annotate_exits(function) if function is not None else 0

    def _annotate_all_exits(self) -> None:
        count = sum(self._annotate_exits_at(start) for start in self.function_starts)
        log.log_info(f"Rosemary AOT: named {count} exit reasons and codes")

    def region_at(self, addr: int) -> Optional[int]:
        """The region whose entry or body function holds `addr`."""
        for function in self.get_functions_containing(addr) or []:
            found = self.function_starts.get(function.start)
            if found is not None:
                return found
        return None

    def regions_covering(self, guest_va: int) -> List[int]:
        """Regions entered at `guest_va`, from the image's entry index, or failing that every
        region whose covered ranges hold it."""
        lift_va = self.lift(guest_va)
        entered = self.image.lookup(lift_va)
        return [entered] if entered is not None else self.image.regions_covering(lift_va)

    def host_code_for(self, index: int, guest_va: int) -> int:
        """The first host address compiled for `guest_va` in region `index`, or its body."""
        body = self.body(index)
        meta = self.image.meta(index)
        return min((body + start for start, _end, va in meta.map if self.guest(va) == guest_va), default=body)

    def _store_metadata(self) -> None:
        embedded, header = self.image.embedded, self.image.header
        self.store_metadata("rosemary_aot.source", embedded.source, isAuto=True)
        self.store_metadata("rosemary_aot.lift_slide", embedded.lift_slide, isAuto=True)
        self.store_metadata("rosemary_aot.guest_slide", self.layout.guest_slide, isAuto=True)
        self.store_metadata("rosemary_aot.tier", embedded.tier.name.lower(), isAuto=True)
        self.store_metadata("rosemary_aot.profiled", embedded.profiled, isAuto=True)
        fingerprints = f"isa {header.isa_fingerprint:016x} config {header.config_fingerprint:016x}"
        self.store_metadata("rosemary_aot.fingerprints", fingerprints, isAuto=True)

    def perform_is_executable(self) -> bool:
        return True

    def perform_get_address_size(self) -> int:
        return 8

    def perform_get_entry_point(self) -> int:
        rec = self.image.records
        if self.image.region_count:
            return self.layout.text + rec.code[0] + rec.entry[0]
        return self.layout.text
