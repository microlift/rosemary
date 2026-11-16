# VR for Roblox under Rosemary (Quadra)

Quadra gives Roblox's macOS client an OpenXR headset by injecting a library into
`RobloxPlayer` (`libQuadraVR.dylib`, built from Quadra's `vr/vr_device.c`,
`vr/vr_xr.c` and `vr/rosemary_glue.c`). This document is what Rosemary adds so
those sources run here, how Quadra builds and starts the library, and what has
and has not been checked. It documents the `quadra-vr` branch of this fork; none
of it is upstream. "Quadra" is the Rust launcher
([QuadraHQ/Quadra](https://github.com/QuadraHQ/Quadra)); the
Python launcher it replaced, which ran the same sources under Darling, is
discontinued.

## What the library does

`vr_device.c` finds the client's `DeviceGL` and `DebugDeviceVR` classes by RTTI,
patches `DeviceGL`'s vtable (`beginFrame`, `setVR`) and attaches a
`DebugDeviceVR` whose methods read poses from, and submit eyes to, OpenXR
(`vr_xr.c`). Roblox's own VR code does the rest. Quadra's side is
`src/backend/vr.rs`.

It needs four things from the host process, and Rosemary supplies each:

| Needs | Darling | Rosemary |
| --- | --- | --- |
| to be loaded into a hardened `RobloxPlayer` | `DYLD_INSERT_LIBRARIES` | `ROSEMARY_DYLD_INSERT_LIBRARIES` + `ROSEMARY_DYLD_INSERT_FORCE=1` |
| to find itself, by `dlsym(RTLD_DEFAULT, "_elfcalls")` | its own table | `dlsym(RTLD_DEFAULT)` searches the program's dylibs (inserted ones included) |
| host (Linux) libraries: the OpenXR loader, EGL, libc | `_elfcalls`: dlopen / dlsym of ELF | `quadra_host_dlopen` / `quadra_host_dlsym` / `quadra_host_dlerror` / `quadra_host_native`, behind a small `_elfcalls` table in the glue |
| a GL context it can bind OpenXR to | Darling's EGL context | SDL3's EGL context (see Graphics) |

## Changes in this fork

* **`ROSEMARY_DYLD_INSERT_FORCE=1`** (`crates/rosemary/src/dyld.rs`). A program
  with the hardened runtime or a `__RESTRICT` segment ignores inserted
  libraries, as dyld would, and RobloxPlayer is hardened. With this set in the
  *host's* environment the libraries are inserted anyway and a line says so on
  stderr. The default is unchanged and a guest cannot set it for itself
  (`ROSEMARY_ENVIRON` is ignored for this). Tests: `tests/dyld_insert.rs`.
* **Global `dlsym` covers the program's own dylibs** (`dyld.rs`,
  `program_image_symbol`). `dlsym(RTLD_DEFAULT, ...)` used to search `dlopen`ed
  images and the executable only, so a symbol exported by an inserted library
  was invisible. It is now the last thing consulted, so nothing Rosemary
  answers is displaced. Native backend only: the emulated backend does not
  record the program's dylibs. Test: `global_dlsym_finds_an_inserted_library_symbol`.
* **`quadra_host_*`** (`crates/rosemary/src/hostbridge.rs`). The host loader's
  own `dlopen` and `dlsym`, bound by any guest image through a flat lookup
  (`-undefined dynamic_lookup`):

  ```c
  int         quadra_host_native(void);                      /* 1: returned pointers are directly callable */
  void       *quadra_host_dlopen(const char *path, int mode); /* mode: Darwin's RTLD_LAZY/NOW/GLOBAL */
  void       *quadra_host_dlsym(void *handle, const char *name); /* handle NULL: the whole process */
  const char *quadra_host_dlerror(void);
  ```

  A guest `dlopen("libEGL.so.1")` already worked, but a guest `dlsym` returns
  Rosemary's implementation for names whose host and Darwin behavior differ
  (`getenv`, `setenv`, `getuid`, ...). The OpenXR loader reads the *process's*
  environment, so the library needs the host's `setenv` itself. These exports
  return the host's symbol unmodified. Libraries are never closed. They refuse
  under `ROSEMARY_NO_PASSTHROUGH` and under the emulated backend, with a
  `quadra_host_dlerror` message. Tests: `tests/hostbridge.rs`.

  Upstream has since added an opt-in API for the same job (`rosemary_host_dlopen`,
  `rosemary_host_dlsym`, `rosemary_host_dlclose`, `rosemary_get_host_rtld_default`
  and `rosemary_host_call_trampoline`, exported only under
  `ROSEMARY_EXPOSE_LIBSYSTEM_EXTENSIONS=1`; `crates/darwin/src/rosemary/`). The
  fork keeps `quadra_host_*` because Quadra's glue binds those names, and Quadra
  decides whether a Rosemary build can do VR by finding the string
  `ROSEMARY_DYLD_INSERT_FORCE` in its binary.

### Calling host functions from guest threads

In the native backend (a host-architecture slice, here x86-64) the returned
pointers are called directly; there is no marshaling, and a function needs no
signature row. That is safe because:

* Both sides use the System V AMD64 ABI. The structs OpenXR and EGL pass are
  fixed-width types and pointers, laid out identically.
* Thread-local state does not collide: the host keeps its thread pointer in
  `%fs`, the guest keeps Darwin's thread-specific data in `%gs`
  (`darwin::tls`), and host code never reads `%gs`. A guest thread is a host
  thread, so glibc's TLS and `errno` work as in any host call (the host's
  `errno`, not the guest's).
* A guest thread runs on its own stack, so a host library's frames sit on the
  stack the guest thread already has. Stack-hungry host code on a small guest
  thread stack is the one thing to watch; the OpenXR loader and the Monado
  client ran on Roblox's render thread without trouble.
* Host code may call guest function pointers (a comparator passed to `qsort`
  ran on a secondary guest thread in the test), because `%gs` stays installed
  for the thread.

The emulated backend (an x86-64 slice on a non-x86-64 host) is refused rather
than approximated: its calls go through a per-function signature table and
the libraries' callbacks and floating-point arguments would need rows for
every OpenXR and EGL function. VR therefore needs an x86-64 host today.

## Graphics

* Roblox must run on OpenGL, not Metal (Quadra's client settings):
  `FFlagDebugGraphicsPreferOpenGL=True` and `FFlagDebugGraphicsDisableMetal=True`
  in `Contents/MacOS/ClientSettings/ClientAppSettings.json`.
* `vr_xr.c` binds OpenXR to the game's context with `XR_MNDX_egl_enable`, so the
  context must be **EGL**. SDL3 makes an EGL context on its Wayland and
  offscreen video drivers. On X11 it makes a GLX one unless told otherwise:
  set `SDL_VIDEO_FORCE_EGL=1` there. With GLX the library logs
  `The game's EGL context has no config` and VR stays off (inferred from the
  code; not run, because the Xvfb used for testing cannot set a swap interval
  under GLX and Roblox then aborts).
* Roblox's render thread owns the context when `DeviceGL::beginFrame` runs, so
  `eglGetCurrentDisplay/Context` called through the bridge on that thread return
  the game's own objects.

## Building

```sh
# Rosemary (this branch)
cargo build --release --bin rosemary
```

Quadra builds the VR library itself, in `build.rs`, when clang and `ld64.lld`
(or Rust's `rust-lld`) are installed. Each source is compiled with
`clang -target x86_64-apple-macos11.0 -ffreestanding -O2` and the objects are
linked with `ld64.lld -dylib -undefined dynamic_lookup` against
`vr/libSystem.tbd`, so no Darwin SDK is needed. The library is embedded in
Quadra and written to `~/.cache/quadra/vr/libQuadraVR.dylib` before a VR launch.

`vr/rosemary_glue.c` is the only Rosemary-specific source. It defines
`_elfcalls`, a pointer to `{open, close, symbol}` of the shape Darling's loader
published, with the three functions forwarding to `quadra_host_*`.
`vr/vr_device.c` and `vr/vr_xr.c` are compiled unchanged.

## Running

Quadra's `src/backend/vr.rs` and `src/backend/launch.rs` set this (host
environment of the `rosemary` process unless noted):

| Setting | Value |
| --- | --- |
| `ROSEMARY_DYLD_INSERT_LIBRARIES` | absolute host path of `libQuadraVR.dylib` |
| `ROSEMARY_DYLD_INSERT_FORCE` | `1` |
| `ROSEMARY_ENVIRON` | newline-separated guest variables: `QUADRA_VR=openxr` (or `openxr-headset`, without the window mirror), `QUADRA_VR_SCALE=<percent>` and `QUADRA_XR_RUNTIME_JSON=<runtime manifest>`; `QUADRA_VR_FOLLOW_RATE=1` is optional |
| `ClientAppSettings.json` | the two OpenGL flags above |
| `SDL_VIDEO_FORCE_EGL=1` | always set; it matters only when SDL uses X11 |
| `MESA_LOADER_DRIVER_OVERRIDE=zink`, `__EGL_VENDOR_LIBRARY_FILENAMES=<Mesa's EGL vendor file>` | only with Quadra's OpenGL-on-Vulkan (zink) VR option |
| `XDG_RUNTIME_DIR` | must be the runtime's: its IPC socket lives there. The library falls back to `/run/user/<uid>` |

`QUADRA_XR_RUNTIME_JSON` is copied to the host's `XR_RUNTIME_JSON` through the
host's real `setenv`, which is why the bridge exists. Setting `XR_RUNTIME_JSON`
in the `rosemary` process's own environment works as well (run, with
`QUADRA_VR=openxr-headset`).

Every `QUADRA_*` variable must be in `ROSEMARY_ENVIRON`: nothing else of the
host's environment reaches the guest (see `usage.md`).

## Testing without a headset

Start the Monado emulator (`~/.local/share/quadrahq/vr-emulator/run-service.sh
start`: a headless null compositor with a simulated HMD and controllers; with
`IPC_LOG=trace` its log counts the runtime's side). Copy the installed
`RobloxPlayer.app` and make a new prefix in a temporary directory, so the live
prefix and its sign-in are not touched. Then start `rosemary <copy>.app` with
the settings above, `QUADRA_XR_RUNTIME_JSON` pointing at the emulator's
`monado_openxr.json` and `SDL_VIDEO_DRIVER=offscreen`, so nothing appears on the
desktop. The library's `[Quadra VR]` lines on stderr report each stage: the
`DeviceGL` slot, the attached device, the session's creation and states, and a
pacing report every 900 frames.

## What has been checked

### 0.5.0-quadra.2 (upstream 0b081874 merged)

Roblox 0.746.0.7461264 on x86-64 Linux with an NVIDIA RTX 3060 Ti (driver
620.70.06), SDL's offscreen driver and Monado 25.1.0's null compositor. The
library was built the way Quadra's `build.rs` builds it and started with the
environment `src/backend/vr.rs` sets (render scale 70 %, mirror on), in a new
prefix. Login screen only; no game was joined.

* Insertion into the hardened client is forced, `DeviceGL`'s VR slot is found
  (+0x268, enabled flag +0x270) and the client's `DebugDeviceVR` is attached.
  The game renders with OpenGL 4.1 through SDL's EGL.
* The OpenXR session is created at 1008x1120 per eye (70 % of the emulator's
  1440x1600) with the controllers bound, and goes idle, ready, synchronized,
  visible, focused. The first 900 frames ran at 42.8 fps while shaders
  compiled, the next 900 at 72.0 fps against 72 Hz with none late.
* The runtime dispatched 2371 `xrWaitFrame`, 2370 `xrBeginFrame` and 2369
  `xrEndFrame`, and 4738 swapchain acquires and releases (both eyes, every
  frame).
* Without VR the same client starts on Metal, through Vulkan on the NVIDIA GPU,
  and reaches its render loop. That held in a new prefix and in a prefix made by
  0.5.0-quadra.1 with MiniDarwin 15. A named prefix, as Quadra uses, keeps its
  base: Rosemary offers MiniDarwin updates only for the default prefix, and
  `rosemary prefix update` moves a named one to release 17.
* `tests/dyld_insert.rs` and `tests/hostbridge.rs` ran, with fixtures built by a
  `cc` stand-in around clang and `ld64.lld` against Darling's libSystem. The
  bridge's probe does not link without `-undefined dynamic_lookup`, so with a
  plain `cc -arch x86_64` its three tests passed without running; the test now
  links it that way.

The `getState` trace, controller input, the window-mirror screenshot, Xvfb and
`kwin_wayland` were not repeated for this release.

### 0.5.0-quadra.1

Roblox 0.745.0.7451137, Rosemary 0.5.0 + this branch, x86-64 Linux, NVIDIA
GPU, against Monado 25.1.0's headless null compositor (simulated HMD and
controllers). All of it on the login screen; no game was joined (that needs a
sign-in).

Run in Rosemary, evidence in the launch log and the runtime's trace:

* The inserted library loads into the hardened RobloxPlayer; every byte-pattern
  check in `vr_device.c` matches; `DeviceGL`'s vtable is patched.
* The OpenXR loader and the Monado client load through the bridge, find the
  runtime from `QUADRA_XR_RUNTIME_JSON`, and the session goes idle, ready,
  synchronized, visible, focused. One run: 900 frames at 70 fps against 72 Hz.
* The runtime's IPC trace of a 25 second run counts 1203 `xrWaitFrame`, 1202
  `xrBeginFrame` and 1201 `xrEndFrame`, with 2402 swapchain acquire, wait and
  release calls (both eyes, every frame) and 2404 view locates.
* Roblox reads the pose: a trace of `getState` shows the simulated head's
  position and rotation changing, hands valid, and the session state 2
  (focused). With controller presses injected into the emulator, the right
  trigger (input slot 12) goes 1.0 and 0.0 in `getState`.
* The eyes hold Roblox's rendering: with the window mirror on, a screenshot
  shows the login screen as a floating panel in both eyes, laser pointers from
  both controllers, drawn from the wobbling head pose.
* The same session starts under SDL's offscreen driver on the NVIDIA GPU, under
  a private Xvfb with `SDL_VIDEO_FORCE_EGL=1` and Mesa software GL, and under a
  private virtual `kwin_wayland` with SDL's Wayland driver and Mesa software GL.
* The three Rust-side changes have integration tests that run a Mach-O guest
  (`dyld_insert`, `hostbridge`). Those fixtures build with `cc -arch x86_64`,
  which this Linux host lacks; they were run with a `cc` wrapper around
  `clang -target x86_64-apple-darwin` and the Darwin SDK that Quadra installs.

## Not checked

* A real headset (Quest 2 through WiVRn). The emulator's controllers use the
  Khronos simple profile; the Touch bindings, thumbsticks, grips and haptics
  were not exercised. Pixels reaching a compositor were not seen either: the
  emulator discards them (the screenshot is of the window mirror, which shares
  the eye framebuffers the swapchain copy reads).
* Joining a game, or any frame after VR is on in a place.
* GLX (X11 without `SDL_VIDEO_FORCE_EGL`), the emulated backend, aarch64 hosts,
  and Rosemary running on its bundled glibc (`ROSEMARY_GLIBC=bundled`) against
  the host's OpenXR and EGL libraries.
* The packaged builds (the Steam-tool archive Quadra installs and the
  AppImage, both made by the release workflow with Nix) were not run locally;
  every check above used `cargo build --release`.
* One test run ended with `XR_ERROR_INSTANCE_LOST` late in its 30 seconds,
  while the emulator service was shared with another job; two repeats of the
  same run (30 and 40 seconds) were clean, and the service log shows only a
  normal shutdown. Treated as the service being stopped underneath the run,
  not reproduced.
