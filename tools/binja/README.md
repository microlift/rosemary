# Binary Ninja: Rosemary AOT cache loader

`rosemary_aot` opens the cache files `rosemary aot` writes
(`$XDG_CACHE_HOME/rosemary/aot/<build-id>/<machine>/<config>/<image-sha256>.rmc`, see `docs/aot.md`) as a
`Rosemary AOT cache` view:

* each Microlift chunk's `.text`, `.rodata` and `.bss` are mapped with every relocation applied;
* host imports, bridges and Cranelift libcalls become typed externs (`name@provider` when two libraries
  export the same name);
* each region gets `region_<guest va>` (the host entry, `void (RegisterContext*)`) and
  `region_<guest va>_body` (its link target), with covered ranges, resume points, link cells and import
  decisions in a comment, and `cells_<guest va>` as a `BlockLinkEntry` array;
* the guest code map becomes `guest 0x...` comments on the host code;
* the path of the Mach-O the cache was built from is logged, stored as the `rosemary_aot.source` metadata, and
  commented at the entry point (files written before it was recorded, `RMAOTv1`, still open without it).

Guest addresses are the image's file VAs by default, so they match the Mach-O opened on its own; the
`Rosemary AOT > Guest address space` setting switches to the run-time VAs the image was lifted at.
Files with more than `Annotation limit` code-map ranges and relocations skip per-instruction annotations on
load; `Rosemary AOT > Annotate guest addresses` adds them for one function, and `Rosemary AOT > Find host code
for guest address` jumps to the code compiled for a guest address.

The cache records only a hash of its ISA, so the architecture is guessed from the code (`Host architecture`
overrides it).

## Install

```sh
ln -s "$PWD/tools/binja/rosemary_aot" ~/Library/Application\ Support/Binary\ Ninja/plugins/   # macOS
ln -s "$PWD/tools/binja/rosemary_aot" ~/.binaryninja/plugins/                                 # Linux
```

## Test

`rmc.py` and `layout.py` do not need Binary Ninja:

```sh
cd tools/binja && python3 -m unittest test_rosemary_aot
python3 -m rosemary_aot.rmc <file.rmc>   # summarize a cache file
```
