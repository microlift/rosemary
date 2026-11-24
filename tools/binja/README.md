# Binary Ninja: Rosemary AOT cache loader

`rosemary_aot` opens the cache files `rosemary aot` writes
(`$XDG_CACHE_HOME/rosemary/aot/<build-id>/<machine>/<config>/<image-sha256>.<part>.rmc`, see `docs/aot.md`) as a
`Rosemary AOT cache` view. Each part is one Microlift image (`MLIMAGE2`) and opens on its own:

* the code is mapped straight from the file, where Microlift's loader would put it: `GOT_SPAN` above the GOT
  it loads every host reference from. The GOT is filled as the loader fills it, read-only, so calls through
  it resolve to their targets;
* host imports, bridges and Cranelift libcalls become typed externs (`name@provider` when two libraries
  export the same name), named on their GOT entries too; the guest slide entry is `__guest_slide`;
* each region gets `region_<guest va>` (the host entry, `void (RegisterContext* ctx)`) and
  `region_<guest va>_body` (its link target), and `cells_<guest va>` as a `BlockLinkEntry` array;
* `RegisterContext` has Microlift's full x86-64 layout (`context.py` mirrors `src/arch/x86_64/context.rs`), along
  with `ControlHeader`, `ExitReason`, `TransferKind`, `XmmRegister`, `X87Register`, `BlockLinkEntry` and
  `CallRetEntry`, so context accesses read as `ctx->rip` or `ctx->control.exit_reason`;
* on AArch64 hosts, region bodies are typed with the pinned ABI read off their entry thunk, following the
  context through stack spills and reloads: `ctx` and each pinned guest register (`rsp @ x3`, `rdi @ x4`, ...)
  arrive in the registers the thunk loads them into, and the
  thunk decompiles to `region_<va>_body(ctx, ctx->rsp, ...)` followed by the write-back. Binary Ninja cannot
  type a body that returns its pinned registers, so the body shows their incoming values but not the ones it
  hands back; read the disassembly before the `ret` for those;
* when annotated (below), each body is commented with its covered ranges, resume points, link cells and
  import decisions, and the guest code map becomes `guest 0x...` comments on the host code;
* once analysis finishes, each constant a region stores into `ctx->control.exit_reason` is shown as its
  `ExitReason` and commented (`exit_reason = ExitUnknownJump`). Each `exit_code` is read under the reason stored
  before it in the same block: a `TransferKind` for `ExitUnknownJump`, a vector for `ExitInterrupt`, or an
  element size for port I/O;
* the path of the Mach-O the cache was built from is logged, stored as the `rosemary_aot.source` metadata, and
  commented at the entry point; its tier and profile size are stored as `rosemary_aot.tier` and
  `rosemary_aot.profiled`.

Guest addresses are the image's file VAs by default, so they match the Mach-O opened on its own; the
`Rosemary AOT > Guest address space` setting switches to the run-time VAs the image was lifted at.
Region metadata is decoded only when needed, so a file loads in time proportional to its region count
rather than its size. Files with more than `Annotation limit` regions skip region comments, per-instruction
annotations and exit names on load; `Rosemary AOT > Annotate guest addresses and exits` adds them for one
region, and `Rosemary AOT > Find host code for guest address` jumps to the code compiled for a guest address
in this part.

The cache records only a hash of its ISA, so the architecture is guessed from the code (`Host architecture`
overrides it).

## Install

```sh
ln -s "$PWD/tools/binja/rosemary_aot" ~/Library/Application\ Support/Binary\ Ninja/plugins/   # macOS
ln -s "$PWD/tools/binja/rosemary_aot" ~/.binaryninja/plugins/                                 # Linux
```

## Test

`rmc.py`, `layout.py` and `context.py` do not need Binary Ninja:

```sh
cd tools/binja && python3 -m unittest test_rosemary_aot
python3 -m rosemary_aot.rmc <file.rmc>   # summarize a cache file
```
