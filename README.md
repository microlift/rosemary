# Rosemary

<a href="https://discord.gg/HUaPNwFwae">
  <img alt="Discord" src="https://img.shields.io/discord/1558958766211338350?label=discord&cacheSeconds=3600">
</a>

Rosemary runs macOS applications across architectures and operating systems. It
combines two closely related projects:

* a high-level x86-64 emulator built on the [Microlift](https://github.com/microlift/microlift) binary lifter and JIT
* a macOS compatibility layer that reimplements Darwin APIs and Apple frameworks on non-macOS hosts.

Rosemary aims to make macOS software portable across macOS and non-macOS systems while
sharing one Mach-O loader, runtime, ABI bridge, and framework implementation. It
is an experimental compatibility project rather than a drop-in replacement for
Apple's Rosetta 2, and application coverage is still incomplete.

The runtime uses [`machish`](crates/machish) to load and link Mach-O images. Its
framework crates provide cross-platform implementations of APIs such as
Objective-C, Foundation, AppKit, CoreAudio, and IOKit. Together these pieces let
the same runtime execute a host-architecture Mach-O directly or recompile an
x86-64 image through Microlift.

Rosemary is designed with the following goals in mind:

* Run x86-64 macOS applications through Microlift on supported host
  architectures.
* Run macOS applications on non-macOS hosts by translating Darwin behavior to
  portable Rust, libc, and SDL3 implementations.
* Share the loader, ABI definitions, shims, and framework implementations between
  native and emulated execution instead of maintaining separate runtimes.
* Prefer behavioral compatibility and correctness first, then improve application
  coverage and performance incrementally.
* Replace Rosetta 2 on macOS when Apple removes its general availability.

Rosemary currently has two execution backends:

* `native` loads a slice matching the host CPU, links it to the available native
  or reimplemented libraries, and calls it directly.
* `emulated` recompiles an x86-64 slice through Microlift. On an x86-64 host,
  native execution is normally selected because recompiling the same instruction
  set provides no benefit.

## Installing

For non-development purposes, we recommend using our management application,
[Butter](https://github.com/microlift/butter). It can automatically download
prebuilt Rosemary binaries from our Github releases, and even build new commits
from source if desired.

For supported distribution packages without using Butter, see [our installation guide](docs/installing.md).

## Building

Rosemary uses the Rust 2024 edition and currently requires Rust 1.99 or newer.
The Microlift and Microlift Wasmtime repositories are Git dependencies, so the
build environment must be able to fetch them.

Additionally, SDL 3.4.0 is required for GUI applications to run. Lower SDL
versions may partially work, but are completely unsupported otherwise

For a release build on the current host:

```sh
cargo build --release --bin rosemary
```

The resulting executable is `target/release/rosemary`. Release builds are
strongly recommended for emulation; debug builds are substantially slower.

### Nix development environment

The repository includes a development flake for AArch64 and x86-64 hosts on both
macOS and Linux. With Nix and flakes enabled, enter the complete development
environment with:

```sh
nix develop
```

The shell provides the Rust toolchain and the build, runtime, and development
dependencies needed to build Rosemary and its workspace components. From inside
the shell, the normal Cargo commands work without any additional setup:

```sh
cargo build --release --bin rosemary
cargo test --release
```

### Platform setup

On macOS, install the Xcode command-line tools before building.

On Alpine Linux, install the following dependencies first:

```sh
sudo apk add sdl3-dev zlib-dev curl-dev glu-dev webkit2gtk-6.0 mesa rust cargo
```

musl support is experimental, and not all applications may run underneath it,
though we encourage you to report issues faced with it anyway, as they likely
are a sign of an incorrectly translated Darwin API.

On Debian-based Linux systems, install the native build and runtime dependencies
first:

```sh
sudo apt-get install build-essential pkg-config libcurl4-openssl-dev \
    zlib1g-dev libsdl3-dev
cargo build --release --bin rosemary
```

Note that Rosemary may not run properly on Debian stable since we require
SDL 3.4.0 minimum.

On FreeBSD, install the native build and runtime dependencies first:

```sh
sudo pkg install curl sdl3 mesa-libs
cargo build --release --bin rosemary
```

FreeBSD support is newer than the Linux port, and might not be at parity.
In particular, some code paths are not updated to use our `starnix` abstraction,
and may result in inconsistencies. Bug reports are welcome, they are generally
a sign of unportable code.

Development from a macOS host can also use the configured Debian container:

```sh
tools/linux-sync.sh --vendor  # first run, or after dependencies change
tools/linux-sync.sh
```

Read the header of [`tools/linux-sync.sh`](tools/linux-sync.sh) for the one-time
container setup and dependency details.

## Running

Rosemary automatically uses a native Mach-O slice when one matches the host and
falls back to an available emulated slice. A backend can also be selected
explicitly:

```sh
target/release/rosemary /path/to/application [args...]
target/release/rosemary --emulated /path/to/application [args...]
target/release/rosemary --native /path/to/application [args...]
```

See [`docs/usage.md`](docs/usage.md) for runtime options and environment
variables.
