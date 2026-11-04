"""Checks the cache-file reader and layout without Binary Ninja: `python3 -m unittest test_rosemary_aot`."""

import struct
import unittest

from rosemary_aot import layout, rmc


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


def sample_file(lift_slide=0x800):
    image = sample_image()
    return rmc.CACHE_MAGIC + u64(lift_slide) + u64(1) + u64(len(image)) + image


class ReaderTests(unittest.TestCase):
    def test_reads_a_file_with_its_regions(self):
        cache = rmc.decode(sample_file())
        self.assertEqual(cache.lift_slide, 0x800)
        image = cache.chunks[0]
        self.assertEqual(image.isa_fingerprint, 1)
        self.assertIsInstance(image.symbols[0], rmc.GuestSlide)
        self.assertEqual(image.symbols[2].name, "malloc")
        region = image.regions[0]
        self.assertEqual(region.covered, [(0x1000, 0x1008), (0x100C, 0x1010)])
        self.assertEqual(region.link_sites, [(0x2000, rmc.LinkSiteKind.STATIC_EDGE)])
        self.assertEqual(region.import_decisions[0][1].kind, rmc.DecisionKind.SLOW)
        self.assertEqual(region.map, [(0, 4, 0x1000)])

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


if __name__ == "__main__":
    unittest.main()
