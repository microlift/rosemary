# CLAUDE.md

This file provides guidance to AI agents when working with code in this repository.

## What this is

Rosemary runs macOS applications across architectures and operating systems. It combines:

* a high-level x86-64 emulator built on the Microlift binary lifter/JIT, and
* a macOS compatibility layer that reimplements Darwin APIs and Apple frameworks on non-macOS hosts.

Both pieces share one Mach-O loader, runtime, ABI bridge, and framework implementation. There are two execution
backends: `native` (runs a host-architecture slice directly) and `emulated` (recompiles an x86-64 slice through
Microlift). Rosemary picks `native` automatically when a matching slice exists.

## Build, test, and run

Run the commands below with `nix develop --command ...` on NixOS hosts.

```sh
cargo build --release --bin rosemary                                  # optimized driver
cargo run --release --bin rosemary -- run --emulated <mach-o> [args]  # run a guest through the lifter explicitly
cargo run --release --bin rosemary -- run --native <mach-o> [args]
cargo test --release                                                  # full workspace test suite
cargo test --release --test objc -- msgsend                           # one integration-test target + filter
cargo check -p machish --no-default-features                          # verify the loader stays no_std
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

`tools/linux-sync.sh` builds and runs the project in its configured Debian container; read its header before first
use (`--vendor` on the first run or after dependency changes).

Debug builds are fine, and useful for iteration, when running a *native* guest with a host-matching slice — their
extra checks have caught real bugs. Avoid debug builds whenever emulation is in play (no native slice for the host
CPU), since that path is already slow and debug makes it much worse. Use release builds to judge performance.

Rust 2024 edition, requires Rust 1.96+. Microlift and Microlift-Wasmtime are Git dependencies, so the build
environment needs network access to fetch them. On Debian-based Linux, install `build-essential pkg-config
libcurl4-openssl-dev zlib1g-dev libsdl3-dev libglu1-mesa libxkbcommon-dev` first. On macOS, install the Xcode command-line tools.

Runtime environment variables (`ROSEMARY_OBJC`, `ROSEMARY_BUNDLED`, `ROSEMARY_STRICT_IMPORTS`, etc.) are documented
in `docs/usage.md` — check there before adding a new one or reasoning about existing runtime toggles.

## Debugging

Rosemary provides LLDB integration via `ROSEMARY_LLDB=1` in combination with standard `lldb` usage. LLDB is preferred over GDB
unless strictly necessary, as the extra integration with announcing Mach-O segments to LLDB makes debugging far more convenient.

## Workspace layout

All members live in `crates/*` (see `Cargo.toml` for the full member/dependency list).

* **`rosemary`** — the CLI driver and runtime: Mach-O execution (`exec/`), the ABI/marshaling bridge to guest
  code (`abi/`), imported-function shims (`shims/`), the JIT/trampoline glue (`jit.rs`, `trampoline.rs`,
  `thunk.rs`, `interp.rs`), dynamic linking (`dyld.rs`, `bridge.rs`, `interpose.rs`), and GUI/profiling overlay
  (`gui/`).
* **`machish`** — the `no_std` Mach-O loader and linker: fat binaries (`fat.rs`), symbol resolution
  (`resolve/`), relocations/fixups (`fixups/`), exports (`exports/`), and mapped memory (`memory/`). Must remain
  `std`-free; verify with `cargo check -p machish --no-default-features`.
* **Darwin/framework crates** — `darwin` (Darwin syscalls/POSIX shims), `objc` (Objective-C runtime, used by both
  `native` and `builtin` modes), `corefoundation`, `foundation`, `appkit`, `applicationservices`, `audiotoolbox`,
  `coreaudio`, `corevideo`, `contacts`, `security`, `iokit`, `gamecontroller`, `forcefeedback`, `webkit` (WebKitGTK
  behind a private Wayland compositor). These are cross-platform reimplementations of Apple
  frameworks; platform-specific behavior belongs here, not scattered through the runtime crate.
* **`server`** — the per-prefix process server (Rosemary's `wineserver`): which pids are guests, their
  executables and threads, answered over a private socket; started on demand as `rosemary server`. `darwin`'s
  `process` module and `mach::tasks` build Darwin's structures from it.
* **`starnix`** — what the host kernel says about processes, the machine, sockets and descriptors (`/proc` on
  Linux, `sysctl(KERN_PROC)` on FreeBSD, `libproc`/Mach on macOS), behind one host-neutral API. Host-info
  `#[cfg]`s belong in its `sys/` backends, not in `server` or `darwin`.
* **`sdl3`** — thin bindings used by the framework crates for windowing/input/audio on non-macOS hosts.
* **`metal_translate`** and **`metal`** — the Metal-on-Vulkan pipeline. `metal_translate` lowers AIR
  LLVM bitcode/text or source MSL into a Metal-semantics SSA IR and then to SPIR-V (`air/`, `ir/`, `msl/`,
  `cache/`); `metal` provides the Metal API surface as a Vulkan-backed runtime. Ground rules here:
  **no game-specific hacks**, and **rejection over approximation** — a feature that cannot be lowered is
  rejected with a diagnostic naming it, never silently approximated. A deliberate, documented semantic shortcut
  is marked with a comment starting `FIXME (ACCURACY): ...`.

### The Objective-C runtime split

There are two Objective-C runtime implementations, selected by `ROSEMARY_OBJC`:

* `native` (macOS only) — calls the real Apple Objective-C runtime; better compatibility/performance.
* `builtin` — the crate's own cross-platform Objective-C runtime (the only option on non-macOS hosts). It can
  differentially test against a real macOS install by forcing `ROSEMARY_OBJC=builtin` and
  `ROSEMARY_BUNDLED=1` on macOS.

`rosemary run --objc-debug` turns on expensive fail-fast checks in the `builtin` runtime (allocation tail guards,
finalized-object quarantine, refcount/weak-slot/autorelease/selector/class-handle validation); it requires
`ROSEMARY_OBJC=builtin` and is a diagnostic tool, not for normal use.

### ABI bridging

Guest imports are declared with an ABI signature in `crates/rosemary/src/abi/table.rs` — never guess an unknown
signature. Calls that need guest registers or guest memory access live in `crates/rosemary/src/shims/`. Rosemary
does not recompile x86-64 system libraries; instead it bridges directly to host ARM libraries through Microlift's
import system (libc, the Objective-C runtime, OpenGL, libdispatch). The one exception is `libc++`, whose x86-64
and AArch64 ABIs differ enough that bridging isn't attempted — most C++ calls fall through to host libc anyway.

## Coding conventions

* `rustfmt` defaults: 4-space indent, `snake_case` for modules/functions/variables, `PascalCase` for types,
  `SCREAMING_SNAKE_CASE` for constants.
* Put platform-specific behavior in `darwin`, not behind conditionals scattered through the runtime.
* Keep `machish` free of `std`.
* Comments should be short, ASCII-only, and explain *why* a constraint exists — not narrate what the code does.
* Semantic accuracy, simplicity, and self-explanatory code is prioritized over everything else. Do not write program-specific
  hacks, rejection over approximation. If you must have some inaccurate behavior, mark it as such with `FIXME (ACCURACY)`
  comments explaining the inaccuracy.
* Do not write entire paragraphs worth of comments — if code needs heavy commenting, consider if the approach could be more
  self-explanatory. Heavy commenting is permitted if the code is genuinely complex, if it's something simple like flag translation,
  it likely does not need entire paragraphs.
* Do not use "magic numbers". Assign constants or enums where macOS would, and in your own code where it is appropriate.

## Testing conventions

* Unit tests live beside the code (`#[cfg(test)]`); integration tests live in the owning crate's `tests/`
  directory.
* Name tests for observable behavior, e.g. `resolves_lazy_bind`.
* Guest fixtures compile at test time with `cc -arch x86_64`; a fixture test skipped for lack of that toolchain
  is not equivalent to real coverage — don't treat a green run on such a host as confirming the fixture path.
* Most LLDB tests need a Mach-O guest and skip on Linux. `lldb_symbolicates_a_published_region` is the exception
  (covers the ELF symbol file) and runs everywhere. On macOS, LLDB tests need
  `ROSEMARY_ENABLE_PRIVILEGED_TESTS` and will prompt for developer-tools access.
* Do not make giant "mega tests". Condense the tests down to just semantic accuracy — complex subsystem interaction
  tests are only permitted if it is to ensure semantic accuracy.

## Commits and PRs

Use the established imperative, scoped format: `darwin: fix clock translation`, `rosemary: implement
CFStringGetBytes`. Keep commits narrowly focused. PRs should explain the affected emulator layer, the behavior
change, and verification commands; link relevant issues and include a reproducer or trace for guest-specific
fixes. Include screenshots only for UI or debug-overlay changes.

ALWAYS ensure that you abide by the Rosemary AI policy documented in `AI_TOOL_POLICY.md`.

NEVER list any agent, model, or tool (AI-powered or otherwise) in a "co-authored-by" annotation in a commit message.
Only human contributors can be authors and accept the associated accountability.
