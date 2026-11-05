"""Checks the cache-file reader and layout without Binary Ninja: `python3 -m unittest test_rosemary_aot`."""

import struct
import unittest

from rosemary_aot import context, layout, rmc


def u64(v):
    return struct.pack("<Q", v & layout.U64_MASK)


def blob(b):
    return u64(len(b)) + b


def string(s):
    return blob(s.encode())


def sample_image():
    """The same image Microlift's `image::tests::sample` encodes, plus a host function."""
    text = b"\xcc" * 32
    out = rmc.IMAGE_MAGIC + struct.pack("<I", rmc.IMAGE_FORMAT_VERSION) + u64(1) + u64(2)
    out += blob(text) + blob(b"\x01\x02\x03") + u64(16) + u64(32)
    out += u64(3)
    out += b"\x00"
    out += b"\x02" + string("") + string("slot")
    out += b"\x01" + string("(i64) -> i64 apple_aarch64") + string("/usr/lib/libSystem.B.dylib") + string("malloc")
    out += u64(3)
    out += u64(8) + b"\x00" + struct.pack("<I", 1) + u64(-4)
    out += u64(16) + b"\x03" + u64(0) + u64(0)
    out += u64(24) + b"\x00" + struct.pack("<I", 0) + u64(0x1000)
    out += u64(1)
    out += u64(0x1000) + u64(0) + u64(0) + b"\x01" + u64(0)
    out += u64(2) + u64(0x1000) + u64(0x1004)
    out += u64(2) + u64(0x1000) + u64(0x1008) + u64(0x100C) + u64(0x1010)
    out += u64(0)
    out += u64(1) + u64(0x2000) + b"\x00"
    out += u64(1) + u64(0x3000) + b"\x01"
    out += u64(1) + struct.pack("<IIQ", 0, 4, 0x1000)
    return out


SAMPLE_SOURCE = "/Applications/Example.app/Contents/MacOS/Example"


def sample_file(lift_slide=0x800):
    image = sample_image()
    return rmc.CACHE_MAGIC + u64(lift_slide) + u64(1) + string(SAMPLE_SOURCE) + u64(len(image)) + image


def sample_v1_file(lift_slide=0x800):
    image = sample_image()
    return rmc.CACHE_MAGIC_V1 + u64(lift_slide) + u64(1) + u64(len(image)) + image


class ReaderTests(unittest.TestCase):
    def test_reads_a_file_with_its_regions(self):
        cache = rmc.decode(sample_file())
        self.assertEqual(cache.lift_slide, 0x800)
        self.assertEqual(cache.source, SAMPLE_SOURCE)
        image = cache.chunks[0]
        self.assertEqual(image.isa_fingerprint, 1)
        self.assertIsInstance(image.symbols[0], rmc.GuestSlide)
        self.assertEqual(image.symbols[2].name, "malloc")
        region = image.regions[0]
        self.assertEqual(region.covered, [(0x1000, 0x1008), (0x100C, 0x1010)])
        self.assertEqual(region.link_sites, [(0x2000, rmc.LinkSiteKind.STATIC_EDGE)])
        self.assertEqual(region.import_decisions[0][1].kind, rmc.DecisionKind.SLOW)
        self.assertEqual(region.map, [(0, 4, 0x1000)])

    def test_reads_a_v1_file_without_a_source(self):
        cache = rmc.decode(sample_v1_file())
        self.assertIsNone(cache.source)
        self.assertEqual(len(cache.chunks), 1)

    def test_rejects_truncated_and_foreign_files(self):
        data = sample_file()
        for cut in (8, 24, len(data) - 1):
            with self.assertRaises(rmc.FormatError):
                rmc.decode(data[:cut])
        with self.assertRaises(rmc.FormatError):
            rmc.decode(b"not a cache file")

    def test_rejects_a_relocation_outside_text(self):
        data = bytearray(sample_file())
        at = data.index(u64(8) + b"\x00" + struct.pack("<I", 1))
        data[at : at + 8] = u64(28)
        with self.assertRaises(rmc.FormatError):
            rmc.decode(bytes(data))


class LayoutTests(unittest.TestCase):
    def test_applies_relocations_against_placed_sections_and_symbols(self):
        cache = rmc.decode(sample_file())
        placed = layout.build(cache, guest_slide=layout.file_slide(cache))
        chunk = placed.chunks[0]
        word = lambda off: struct.unpack_from("<Q", placed.blob, chunk.text_offset + off)[0]
        slot = next(e for e in placed.externs if e.symbol.name == "slot")
        self.assertEqual(word(8), slot.addr - 4)
        self.assertEqual(word(16), chunk.bss)
        # A lift-time guest VA comes back as its file VA.
        self.assertEqual(word(24), 0x1000 - 0x800)
        self.assertEqual(layout.guest_va(placed, 0x1000), 0x800)

    def test_lift_slide_mode_keeps_lift_time_addresses(self):
        cache = rmc.decode(sample_file())
        placed = layout.build(cache, guest_slide=0)
        self.assertEqual(struct.unpack_from("<Q", placed.blob, 24)[0], 0x1000)

    def test_splits_cranelift_signatures(self):
        self.assertEqual(layout.parse_signature("(i64, f64) -> i32 apple_aarch64"), (["i64", "f64"], ["i32"], "apple_aarch64"))
        self.assertEqual(layout.parse_signature("() system_v"), ([], [], "system_v"))


def ldr_x(rt, rn, offset):
    return 0xF9400000 | (offset // 8) << 10 | rn << 5 | rt


def ldr_q(rt, rn, offset):
    return 0x3DC00000 | (offset // 16) << 10 | rn << 5 | rt


STUR_X0_SP = 0xF80003E0
LDUR_X2_SP = 0xF84003E2
BLR_X1 = 0xD63F0020
RET = 0xD65F03C0


def code(*words):
    return struct.pack(f"<{len(words)}I", *words)


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
        thunk = code(
            ldr_x(3, 0, 32),
            ldr_x(4, 0, 56),
            ldr_q(8, 0, 192 + 2 * 16),
            ldr_x(5, 1, 0),
            STUR_X0_SP,
            LDUR_X2_SP,
            BLR_X1,
        )
        abi = context.decode_aarch64_thunk(b"\0" * 4 + thunk, 4)
        self.assertEqual(abi.context, "x2")
        self.assertEqual(
            [(s.register, s.guest, s.vector) for s in abi.slots],
            [("x3", "rsp", False), ("x4", "rdi", False), ("q8", "xmm2", True)],
        )

    def test_types_nothing_for_a_thunk_of_another_shape(self):
        self.assertIsNone(context.decode_aarch64_thunk(code(ldr_x(3, 0, 32), RET), 0))
        self.assertIsNone(context.decode_aarch64_thunk(code(ldr_x(3, 0, 32), BLR_X1), 0))


if __name__ == "__main__":
    unittest.main()
