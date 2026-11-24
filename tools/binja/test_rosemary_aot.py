"""Checks the cache-file reader and layout without Binary Ninja: `python3 -m unittest test_rosemary_aot`."""

import struct
import unittest

from rosemary_aot import context, layout, rmc


def u32(v):
    return struct.pack("<I", v)


def u64(v):
    return struct.pack("<Q", v & rmc.U64_MASK)


def varint(v):
    out = bytearray()
    while v >= 0x80:
        out.append(v & 0x7F | 0x80)
        v >>= 7
    out.append(v)
    return bytes(out)


def signed(v):
    return varint(((v << 1) ^ (v >> 63)) & rmc.U64_MASK)


def string(s):
    return varint(len(s)) + s.encode()


SAMPLE_SOURCE = "/Applications/Example.app/Contents/MacOS/Example"
LIFT_SLIDE = 0x800
MALLOC = rmc.HostSymbol(rmc.HostSymbolKind.FUNCTION, "/usr/lib/libSystem.B.dylib", "malloc", "(i64) -> i64 apple_aarch64")


def shared_entry(entry):
    if isinstance(entry, rmc.GuestSlide):
        return b"\x00"
    if isinstance(entry, rmc.TextEntry):
        return b"\x04" + varint(entry.offset)
    out = bytes([entry.kind])
    if entry.kind == rmc.HostSymbolKind.FUNCTION:
        out += string(entry.signature)
    return out + string(entry.provider) + string(entry.name)


def region_meta(meta, sites):
    base = meta.lift_base
    out = varint(len(meta.shared)) + b"".join(varint(s) for s in meta.shared)
    out += varint(len(meta.resume_points))
    previous = base
    for pc in meta.resume_points:
        out += signed(pc - previous)
        previous = pc
    for ranges in (meta.covered, meta.data_dependencies):
        out += varint(len(ranges))
        previous = base
        for start, end in ranges:
            out += signed(start - previous) + varint(end - start)
            previous = end
    out += varint(len(meta.link_sites))
    for target, kind in meta.link_sites:
        out += signed(target - base) + bytes([kind])
    out += varint(len(meta.import_decisions))
    for site in meta.import_decisions:
        if site not in sites:
            sites.append(site)
        out += varint(sites.index(site))
    out += varint(len(meta.map))
    end = 0
    for start, stop, va in meta.map:
        out += signed(start - end) + varint(stop - start) + signed(va - base)
        end = stop
    return out


def site(va, decision):
    out = varint(va) + bytes([decision.kind])
    if decision.kind == rmc.DecisionKind.FAST:
        out += string(decision.provider) + string(decision.name) + string(decision.abi)
    elif decision.kind == rmc.DecisionKind.BRIDGE:
        out += string(decision.name)
    return out


def image_file(text, shared, regions, bss_len, embedded):
    """An image as Microlift's `image::file::Writer` lays it out. `regions` holds
    (code, code_len, entry, link_target, cells, meta)."""
    text_len = -(-len(text) // rmc.PAGE) * rmc.PAGE
    sites = []
    metas = [region_meta(r[5], sites) for r in regions]
    records = b""
    meta_offset = 0
    entries = []
    for index, ((code, code_len, entry, link_target, cells, meta), encoded) in enumerate(zip(regions, metas)):
        records += u64(meta.lift_base) + u64(meta_offset)
        records += b"".join(u32(v) for v in (code, code_len, entry, link_target, rmc.NO_CELLS if cells is None else cells, len(encoded)))
        meta_offset += len(encoded)
        entries += [(pc, index) for pc in meta.resume_points]
    entries.sort()
    entry_base = entries[0][0] if entries else 0
    tables = [
        b"".join(shared_entry(e) for e in shared),
        b"".join(site(va, d) for va, d in sites),
        records,
        b"".join(u32(pc - entry_base) + u32(r) for pc, r in entries),
        b"".join(metas),
        b"",
        embedded,
    ]
    at = rmc.PAGE + text_len
    sections = b""
    for table in tables:
        sections += u64(at) + u64(len(table))
        at += len(table)
    words = (0x1111, 0x2222, rmc.PAGE, text_len, bss_len, len(shared), len(regions), len(entries), entry_base)
    header = rmc.IMAGE_MAGIC + u32(rmc.IMAGE_FORMAT_VERSION) + u32(0) + b"".join(u64(w) for w in words) + sections
    header = header.ljust(rmc.PAGE, b"\0")
    return header + text.ljust(text_len, b"\0") + b"".join(tables)


def embedded(source=SAMPLE_SOURCE, tier=rmc.Tier.FINAL):
    return rmc.CACHE_MAGIC + u64(LIFT_SLIDE) + u64(1234) + bytes([tier]) + source.encode()


SHARED = [rmc.GuestSlide(), MALLOC, rmc.TextEntry(0x40)]
FIRST = rmc.RegionMeta(
    lift_base=0x1000,
    shared=[0, 1],
    resume_points=[0x1000, 0x1004],
    covered=[(0x1000, 0x1008), (0x100C, 0x1010)],
    link_sites=[(0x2000, rmc.LinkSiteKind.STATIC_EDGE), (0x1010, rmc.LinkSiteKind.CALL_RETURN)],
    import_decisions=[(0x3000, rmc.ImportDecision(rmc.DecisionKind.SLOW))],
    map=[(0, 4, 0x1000), (8, 12, 0x1004)],
)
SECOND = rmc.RegionMeta(
    lift_base=0x1800,
    shared=[2],
    resume_points=[0x1800],
    covered=[(0x1800, 0x1810)],
    import_decisions=[
        (0x3000, rmc.ImportDecision(rmc.DecisionKind.SLOW)),
        (0x3008, rmc.ImportDecision(rmc.DecisionKind.FAST, "/usr/lib/libSystem.B.dylib", "free", "(i64) apple_aarch64")),
    ],
    map=[(0, 16, 0x1800)],
)


def sample_file(**kwargs):
    regions = [(0x80, 0x40, 0x20, 0, 0, FIRST), (0xC0, 0x10, 0, 0, None, SECOND)]
    return image_file(b"\xcc" * 0xD0, SHARED, regions, 2 * rmc.LINK_ENTRY_SIZE, embedded(**kwargs))


class ReaderTests(unittest.TestCase):
    def test_reads_a_file_with_its_regions(self):
        image = rmc.decode(sample_file())
        self.assertEqual(image.lift_slide, LIFT_SLIDE)
        self.assertEqual(image.source, SAMPLE_SOURCE)
        self.assertEqual(image.embedded.tier, rmc.Tier.FINAL)
        self.assertEqual(image.shared, SHARED)
        self.assertEqual(image.region_count, 2)
        self.assertEqual(image.records.entry, [0x20, 0])
        self.assertEqual(image.meta(0), FIRST)
        self.assertEqual(image.meta(1), SECOND)

    def test_decodes_covered_ranges_alone(self):
        image = rmc.decode(sample_file())
        self.assertEqual(image.covered(0), FIRST.covered)

    def test_counts_cells_from_the_records(self):
        self.assertEqual(rmc.decode(sample_file()).cell_counts, [2, 0])

    def test_finds_regions_by_entry_and_by_coverage(self):
        image = rmc.decode(sample_file())
        self.assertEqual(image.lookup(0x1004), 0)
        self.assertEqual(image.lookup(0x1800), 1)
        self.assertIsNone(image.lookup(0x1002))
        self.assertEqual(image.regions_covering(0x100C), [0])

    def test_recognizes_only_rosemary_images(self):
        data = sample_file()
        read = lambda offset, length: data[offset : offset + length]
        self.assertTrue(rmc.is_cache_file(read, len(data)))
        foreign = data.replace(rmc.CACHE_MAGIC, b"OTHERv1\0")
        read = lambda offset, length: foreign[offset : offset + length]
        self.assertFalse(rmc.is_cache_file(read, len(foreign)))

    def test_rejects_truncated_and_foreign_files(self):
        data = sample_file()
        for cut in (8, 600, len(data) - 1):
            with self.assertRaises(rmc.FormatError):
                rmc.decode(data[:cut])
        with self.assertRaises(rmc.FormatError):
            rmc.decode(b"not a cache file".ljust(rmc.HEADER_LEN, b"\0"))
        newer = bytearray(data)
        newer[8:12] = u32(rmc.IMAGE_FORMAT_VERSION + 1)
        with self.assertRaises(rmc.FormatError):
            rmc.decode(bytes(newer))

    def test_rejects_a_region_outside_the_code(self):
        regions = [(rmc.PAGE - 8, 0x40, 0, 0, None, SECOND)]
        with self.assertRaises(rmc.FormatError):
            rmc.decode(image_file(b"\xcc" * 16, SHARED, regions, 0, embedded()))


class LayoutTests(unittest.TestCase):
    def test_fills_the_got_as_the_loader_does(self):
        image = rmc.decode(sample_file())
        placed = layout.build(image, guest_slide=layout.file_slide(image))
        self.assertEqual(placed.text - placed.got, rmc.GOT_SPAN)
        shared = struct.unpack(f"<{len(SHARED)}Q", placed.shared_values)
        (malloc,) = placed.externs
        self.assertEqual(shared, (-LIFT_SLIDE & rmc.U64_MASK, malloc.addr, placed.text + 0x40))
        own = placed.own_values
        word = lambda addr: struct.unpack_from("<Q", own, addr - placed.own_got)[0]
        cells, body = placed.own_slots(0)
        self.assertEqual((word(cells), word(body)), (placed.bss, placed.text + 0x80))
        cells, body = placed.own_slots(1)
        self.assertEqual((word(cells), word(body)), (0, placed.text + 0xC0))
        self.assertEqual(placed.own_slots(1)[0], placed.own_got)

    def test_maps_lift_time_addresses_to_file_or_lift_addresses(self):
        image = rmc.decode(sample_file())
        self.assertEqual(layout.guest_va(layout.build(image, guest_slide=layout.file_slide(image)), 0x1000), 0x800)
        self.assertEqual(layout.guest_va(layout.build(image, guest_slide=0), 0x1000), 0x1000)

    def test_names_symbols_two_providers_export(self):
        other = rmc.HostSymbol(MALLOC.kind, "/usr/lib/libc++.1.dylib", "malloc", MALLOC.signature)
        data = image_file(b"\xcc" * 16, [MALLOC, other], [], 0, embedded())
        names = [e.display for e in layout.build(rmc.decode(data)).externs]
        self.assertEqual(names, ["malloc@libSystem.B", "malloc@libc++.1"])

    def test_splits_cranelift_signatures(self):
        self.assertEqual(layout.parse_signature("(i64, f64) -> i32 apple_aarch64"), (["i64", "f64"], ["i32"], "apple_aarch64"))
        self.assertEqual(layout.parse_signature("() system_v"), ([], [], "system_v"))


def ldr_x(rt, rn, offset):
    return 0xF9400000 | (offset // 8) << 10 | rn << 5 | rt


def ldr_q(rt, rn, offset):
    return 0x3DC00000 | (offset // 16) << 10 | rn << 5 | rt


def stur_x(rt, offset):
    return 0xF8000000 | offset << 12 | SP << 5 | rt


def ldur_x(rt, offset):
    return 0xF8400000 | offset << 12 | SP << 5 | rt


def adrp(rd, pages):
    return 0x90000000 | (pages & 3) << 29 | ((pages >> 2) & 0x7FFFF) << 5 | rd


SP = 31
STP_FP_LR = 0xA9BF7BFD
MOV_FP_SP = 0x910003FD
SUB_SP_16 = 0xD10043FF
MOV_X3_X5 = 0xAA0503E3
BLR_X0 = 0xD63F0000
BLR_X1 = 0xD63F0020
RET = 0xD65F03C0


def code(*words):
    return struct.pack(f"<{len(words)}I", *words)


def optimized_thunk(got_page=-1, got_offset=0xFF8):
    """The final tier's thunk: guest registers loaded from x0, then the body from the GOT into x0."""
    return code(
        STP_FP_LR,
        MOV_FP_SP,
        SUB_SP_16,
        ldr_x(3, 0, 32),
        ldr_x(4, 0, 56),
        ldr_q(8, 0, 192 + 2 * 16),
        stur_x(0, 0),
        adrp(0, got_page),
        ldr_x(0, 0, got_offset),
        ldur_x(2, 0),
        BLR_X0,
        RET,
    )


class ContextTests(unittest.TestCase):
    def test_register_context_matches_microlift_offsets(self):
        offset = lambda field: context.offset_of(context.REGISTER_CONTEXT, field)
        # The offsets Microlift's own `context::tests` pin.
        self.assertEqual(offset("link_table"), 152)
        self.assertEqual(offset("xmm"), 192)
        self.assertEqual(offset("link_hit"), 616)
        self.assertEqual((offset("cf"), offset("control"), offset("callret_base")), (160, 168, 624))
        whole = context.layout_of(context.REGISTER_CONTEXT)
        self.assertEqual((whole.size, whole.align), (656, 16))
        self.assertEqual(context.layout_of(context.LINK_ENTRY).size, rmc.LINK_ENTRY_SIZE)

    def test_control_fields_sit_where_regions_store_them(self):
        self.assertEqual(context.control_offset("exit_reason"), 168)
        self.assertEqual(context.control_offset("exit_code"), 172)

    def test_reads_an_exit_code_under_its_reason(self):
        reason = lambda name: next(v for n, v in context.ENUMS[context.EXIT_REASON].members if n == name)
        jump = context.describe_exit_code(reason("ExitUnknownJump"), 8)
        self.assertEqual((jump.text, jump.enum), ("TransferIndirectCall", context.TRANSFER_KIND))
        self.assertEqual(context.describe_exit_code(reason("ExitInterrupt"), 0x80).text, "vector 0x80")
        self.assertEqual(context.describe_exit_code(reason("ExitPortOutput"), 2).text, "2-byte elements")
        self.assertIsNone(context.describe_exit_code(reason("ExitReturn"), 1))
        self.assertIsNone(context.describe_exit_code(reason("ExitUnknownJump"), 99))

    def test_retired_exit_reasons_have_no_name(self):
        self.assertIsNone(context.enum_member(context.EXIT_REASON, 2))
        self.assertIsNone(context.enum_member(context.EXIT_REASON, 4))

    def test_names_whole_guest_registers_only(self):
        self.assertEqual(context.guest_register_at(32, 8), "rsp")
        self.assertEqual(context.guest_register_at(192 + 3 * 16, 16), "xmm3")
        self.assertIsNone(context.guest_register_at(128, 8))
        self.assertIsNone(context.guest_register_at(36, 8))

    def test_decodes_pinned_registers_from_an_entry_thunk(self):
        abi = context.decode_aarch64_thunk(b"\0" * 4 + optimized_thunk(), 4)
        self.assertEqual(abi.context, "x2")
        self.assertEqual(
            [(s.register, s.guest, s.vector) for s in abi.slots],
            [("x3", "rsp", False), ("x4", "rdi", False), ("q8", "xmm2", True)],
        )

    def test_follows_the_context_through_stack_reloads(self):
        # The provisional tier reloads the spilled context before each load, into a new register.
        thunk = code(
            STP_FP_LR,
            stur_x(0, 0x58),
            ldur_x(27, 0x58),
            ldr_x(3, 27, 32),
            ldur_x(1, 0x58),
            ldr_x(15, 1, 88),
            adrp(8, -2),
            ldr_x(8, 8, 0xFF8),
            ldur_x(2, 0x58),
            0xD63F0100,  # blr x8
        )
        abi = context.decode_aarch64_thunk(thunk, 0)
        self.assertEqual(abi.context, "x2")
        self.assertEqual([(s.register, s.guest) for s in abi.slots], [("x3", "rsp"), ("x15", "r11")])

    def test_loads_through_a_clobbered_context_register_are_not_guest_registers(self):
        thunk = code(stur_x(0, 0), adrp(0, -1), ldr_x(3, 0, 32), ldur_x(2, 0), BLR_X0)
        self.assertEqual(context.decode_aarch64_thunk(thunk, 0).slots, ())

    def test_types_nothing_for_a_thunk_of_another_shape(self):
        self.assertIsNone(context.decode_aarch64_thunk(code(ldr_x(3, 0, 32), RET), 0))
        self.assertIsNone(context.decode_aarch64_thunk(code(ldr_x(3, 0, 32), BLR_X1), 0))
        # An instruction the decoder does not model may move the context anywhere.
        self.assertIsNone(context.decode_aarch64_thunk(code(MOV_X3_X5, stur_x(0, 0), ldur_x(2, 0), BLR_X1), 0))

    def test_remembers_a_thunk_shape_across_got_entries(self):
        decoder = context.ThunkDecoder()
        first = decoder.decode(optimized_thunk())
        self.assertEqual(decoder.decode(optimized_thunk(got_page=-7, got_offset=0x10)), first)
        self.assertEqual(len(decoder._shapes), 1)
        # A different load is a different shape, never a match.
        other = optimized_thunk().replace(struct.pack("<I", ldr_x(4, 0, 56)), struct.pack("<I", ldr_x(4, 0, 64)))
        self.assertEqual(decoder.decode(other), context.decode_aarch64_thunk(other, 0))
        self.assertEqual(len(decoder._shapes), 2)


if __name__ == "__main__":
    unittest.main()
