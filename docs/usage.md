# Rosemary CLI Reference

## Running applications

Pass an app bundle or a Mach-O executable to `run`:

```sh
rosemary /some/app.app
rosemary --emulated "/some/My App.app" [args...]
rosemary /some/app.app/Contents/MacOS/App [args...]
```

For a bundle, Rosemary reads `Contents/Info.plist` (XML or binary) and runs
the executable named by `CFBundleExecutable` in `Contents/MacOS`. An empty
or missing name falls back to `NSExecutable`, then the bundle's name without
`.app`. Arguments go to that executable, and the usual backend selection and
bundle resource lookup apply. Invalid bundles report an error.

Binaries and apps installed in the prefix take priority over host filesystem paths. Think of this as a sort of "overlay" - if `/Applications/MyApp.app` is present in your prefix, Rosemary prioritizes the prefix first. If not, it will search the host filesystem for that directory.

## Entering a shell

Rosemary ships a full MiniDarwin base system, including basic utilities like `bash` and `zsh`.

To enter a shell in the Rosemary prefix, first create one with `rosemary prefix create`, then simply run either:

```sh
rosemary /bin/bash
```

or

```sh
rosemary /bin/zsh
```

depending on your preference.

## The process server

On non-macOS, every guest is a host process of its own and none can see the
others by itself. A per-prefix **process server** tracks resources that `top`,
`ps` and `task_for_pid` read. It starts on demand and exits when idle. It can
also be run or stopped by hand:

```sh
rosemary server --foreground   # run this prefix's server in the terminal
rosemary server --stop         # ask this prefix's server to exit
```

## Environment variables


| Key | Default | Description |
| --- | ------- | ----------- |
| `ROSEMARY_OBJC` | `native` (macOS), `builtin` (other) | Set the Objective-C runtime implementation to use. On Linux, there is only `builtin`. On macOS, `native` uses the real Objective-C, and `builtin` uses the compatibility implementation which can only talk to cross platform reimplementations. `native` yields better compatibility and performance, so use it if you can. It is only recommended you use `builtin` if you are differentially testing, or working upon the compatibility layer from a real macOS system. Make sure that if you're on macOS you also set `ROSEMARY_BUNDLED=1`. |
| `ROSEMARY_BUNDLED` | `0` | (macOS only) - determines if Rosemary should re-execute from a virtual repackage of the application. Makes the real Objective-C runtime recognize plist registrations, unnecessary with `builtin` (and possibly problematic). |
| `ROSEMARY_LIBC` | `native` (macOS), `builtin` (other) | Select the libc shim implementation for guest imports. Guest-aware default modules remain active in both modes. |
| `ROSEMARY_DEAD_FLAGS` | `0` | Enable Microlift's dead flag elimination pass. Improves performance in some applications, but not by a wide enough margin. **May have semantic accuracy bugs, use with extreme caution**. |
| `ROSEMARY_TSO` | `1` (non-x86 hosts), `0` (x86 hosts) | Fence guest memory accesses so an AArch64 host keeps x86's total store order. Without it, lock-free guest code that publishes data with plain stores (Mono's class setup, for one) can see another thread's writes out of order; Subnautica aborts in Mono on `ac->rank` while loading the world. Costs a `dmb` on most guest memory accesses. |
| `ROSEMARY_CALL_RETURN` | `0` | Enable Microlift native call return support. Can improve performance in some applications - disabled by default as measurements were inconclusive and it is additional, complex code surface. |
| `ROSEMARY_LINK_ROWS` | `65536` | Rows in each guest thread's block-link table, rounded up to a power of two (at least 256). A row lets translated code continue straight into the next region instead of exiting to the run loop, so a working set larger than the table shows up as `window miss(es)` and `indirect call` / `ret out of region` exits under `ROSEMARY_STATS`. 32 bytes a row, committed only where rows are written. |
| `ROSEMARY_LINK_WAYS` | `4` | How many rows of the block-link table one guest address may occupy: `1`, `2` or `4`. Fewer ways let hot addresses that share a row evict each other on every transfer, which Mono's densely packed JIT code does as soon as ASLR overlaps it with another image modulo the table's span. Measure with `tools/unitybench`. |
| `ROSEMARY_STRICT_IMPORTS` | `0` | Refuse to run a program that has unbound imports, rather than announcing them on stderr and running anyway. Unbound imports are always announced; this only decides whether they are fatal. |
| `ROSEMARY_NO_PASSTHROUGH` | `0` | (non-macOS) Never answer a guest import with a host library's symbol. Only what Rosemary implements (Darwin, the frameworks, the Objective-C runtime, OpenGL/OpenAL bridges and other explicit shims) binds; everything else is reported unbound. Covers the program's link, `dlopen`ed images, `dlsym`, and `dlopen` of host libraries, which is refused. Use it to catch a guest silently bound to a coincidentally-named host function, such as the host's `libstdc++` in place of `libc++`. Combine with `ROSEMARY_STRICT_IMPORTS=1` to make any gap fatal. |
| `ROSEMARY_UNBOUND_DETAIL` | `40` | How many unbound imports are named one by one before the rest are only counted. Set to `all` for the whole list. A large application has hundreds, so the default keeps the interesting output - what comes *after* the list - on screen; raise it when a fault at address zero has to be matched against a symbol. |
| `ROSEMARY_NO_FOREIGN` | `0` | (macOS only - unsafe) Assume that we have reimplemented every framework this app asks of us, disabling foreign Objective-C support in the reimplementations for a slight performance boost. Assumed to be always on with non-macOS systems, since there are no native Objective C frameworks to conflict with. |
| `ROSEMARY_STEAM` | `1` | Bridge a guest's Steamworks calls to the host's own Steam client, and publish the Mach service `libsteam_api` asks where Steam is. Set to `0` to turn both off, so that a guest asking for Steam is told there is none. Has no effect unless a generated `steamclient.dylib` exists. |
| `ROSEMARY_STEAM_CLIENT` | (found) | The generated guest-facing `steamclient.dylib`. Looked for beside the `rosemary` binary, and then in `tools/steamclient/generated/` above it, which is where a checkout leaves it. |
| `ROSEMARY_STEAM_HOST_CLIENT` | (found) | The host's own `steamclient.so`. Looked for under `$STEAM_COMPAT_CLIENT_INSTALL_PATH`, `~/.steam/sdk64`, `~/.steam/root`, `~/.steam/steam` and `~/.local/share/Steam`. |
| `ROSEMARY_STEAM_TRACE` | unset | Report every bridged Steamworks call on stderr: the dispatch id, the host interface, and the vtable slot it landed on. |
| `ROSEMARY_SWAP_INTERVAL` | `none` | Force a specific swap interval on OpenGL applications. Allows enabling immediate (0), VSync (1), and -1 (Adaptive VSync), along with other V-blank values, allowing unlocked framerate on apps that use VSync. |
| `ROSEMARY_PREFIX` | the default prefix (`off` on macOS) | The prefix a guest runs in: a directory holding a Darwin filesystem, whose `root/` subdirectory is the guest's `/`. Unset, `rosemary run` uses `$XDG_DATA_HOME/rosemary/prefixes/default` (or `$HOME/.local/share/rosemary/prefixes/default`), creating it on first use. Set it to another path to keep a guest's files separate, or to `off` to leave guest paths alone entirely. |
| `ROSEMARY_PREFIX_HOME` | `prefix` | Where the guest's home directory is. `prefix` keeps it inside the prefix, so a prefix is one directory to copy, archive or delete. `host` mounts the guest's `/Users/<name>` onto the real `$HOME` instead, for a user who would rather keep one set of documents than two. |
| `ROSEMARY_PREFIX_SETUP` | `ask` | What to do when a prefix needs its Darwin base and it is not cached. `ask` asks - a dialog under `rosemary run` when there is a display, then the terminal - and declines when there is nobody to ask. `download` fetches it without asking. `fail` never downloads. The base is verified against hashes compiled into Rosemary whichever route it takes. |
| `ROSEMARY_ROOTFS_CACHE` | `$XDG_CACHE_HOME/rosemary/rootfs` | Where verified Darwin bases are cached, shared by every prefix. Falls back to `$HOME/.cache/rosemary/rootfs`. |
| `ROSEMARY_ENVIRON` | unset | (builtin libc) Variables the guest starts with, one `NAME=VALUE` per line. A line that is a bare `NAME` copies the host's value, if it has one. Nothing else of the host's environment reaches the guest: it gets only `HOME`, `USER`, `LOGNAME`, `SHELL`, `PATH`, `TMPDIR` and `PWD` with its own values, plus the host's `TERM` and `COLORTERM`, and `LANG` and `LC_*` unless the guest is an application bundle (an app launched from the Finder has no locale variables; a bare `LANG` line still copies it). This keeps a Darwin program from following host settings such as `DISPLAY` or `QT_PLUGIN_PATH`. Example: `ROSEMARY_ENVIRON=$'FOO=bar\nDISPLAY'`. |
| `ROSEMARY_FORWARD_ENV_VARS` | unset | (builtin libc) Comma-separated names of variables a guest may set for a Rosemary process it launches (another Mach-O it `exec`s or spawns); a name ending in `*` matches by prefix. Unset, nothing the guest sets reaches that process's own environment. Read from Rosemary's environment, never the guest's, unless the list names `ROSEMARY_FORWARD_ENV_VARS` itself. Example: `ROSEMARY_STATS,ROSEMARY_TRACE_PREFIX`. |
| `ROSEMARY_GUEST_ENVIRON`, `ROSEMARY_GUEST_ENVIRON_<n>` | unset | (internal) Set by a guest's `exec` of another Mach-O to carry the guest's environment block, one entry per variable, to the child runtime. The child guest starts with exactly that block, and `ROSEMARY_ENVIRON` is not applied again. Never visible to a guest. |
| `ROSEMARY_DYLD_INSERT_LIBRARIES` | unset | Colon-separated dylibs linked into the guest's program ahead of its own dependencies, like dyld's `DYLD_INSERT_LIBRARIES`. Read from the *host's* environment only: a guest that sets it changes nothing. A program that opted into the hardened runtime (without `allow-dyld-environment-variables`) or has a `__RESTRICT` segment ignores it. |
| `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`, `DYLD_FALLBACK_LIBRARY_PATH` | unset | Honored when present in the *guest's* environment block (`ROSEMARY_ENVIRON`, or what the guest set before an `exec`), never from the host's, so a macOS host's own dyld does not insert a library a second time. `DYLD_LIBRARY_PATH` and the fallback path apply to the program's link and to `dlopen`. |
| `ROSEMARY_TRACE_PREFIX` | unset | Report every guest-to-host path translation on stderr, as the pair of spellings. The namespace sits underneath every file call a guest makes, so this is the first thing to reach for when a program behaves differently inside a prefix. |
| `ROSEMARY_SERVER` | `on` | Whether guests join their prefix's process server, which is how one guest sees another: `top`, `ps`, `task_for_pid`, `proc_listpids`. The first guest starts the server as `rosemary server` and it exits a few seconds after the last guest. Set to `off` to keep every guest to itself, as if it were the only process on the system. |
| `ROSEMARY_SERVER_LINGER` | `5` | Seconds a process server stays up with no guest connected, so a shell running one short guest after another keeps one server rather than starting a new one each time. Fractions are accepted; `0` exits as soon as the last guest leaves. |
| `ROSEMARY_DECORATIONS` | `client` | Who draws a titled window's titlebar. `client` draws a Mac-style titlebar (window buttons, title, and the application's menus) in the window itself, and keeps SDL from loading libdecor, whose GTK 3 plugin cannot share a process with the GTK 4 that WebKit loads. `server` leaves the titlebar to the host's window manager. Applies to the `builtin` AppKit on every host, including macOS with `ROSEMARY_OBJC=builtin`. |
| `ROSEMARY_MENU` | `titlebar` (`client`), `strip` (`server`) | Where the application's main menu is shown. `titlebar` puts it in the titlebar after the window buttons; `strip` puts it in a row of its own under the titlebar, which is the only placement under `server` decorations; `none` shows no menus. Key equivalents work whichever is chosen. |
| `ROSEMARY_MENU_CTRL` | `1` | Let Control stand in for Command in menu key equivalents, so Ctrl+S reaches an item whose equivalent is Command-S. Only when nothing took the key as Control first. Set to `0` to leave Control to the program. |
| `ROSEMARY_SWAP_CTRL_CMD` | unset | Swap the Control and Command (Super) keys everywhere the guest reads them: key codes, `-modifierFlags` and Carbon's `GetCurrentKeyModifiers`, so the key a PC keyboard has where a Mac's Command is acts as Command. Any value but `0` turns it on. |
| `ROSEMARY_CONTROL_EDIT_KEYS` | unset | Let Control stand in for Command in the editing shortcuts only (A, C, V, X, Y and Z: select all, copy, paste, cut, redo, undo) and stay Control everywhere else, for a program that also reads Control as a key of its own. From the editing key until Control is let go, the guest sees Control released and Command held; any other key or a click pressed meanwhile gets Control back first. Meant for desktops that keep Super's letter chords for themselves, where Command-V never reaches the guest. Does nothing under `ROSEMARY_SWAP_CTRL_CMD`. Any value but `0` turns it on. |
| `ROSEMARY_CONFINE_POINTER` | unset | Keep the pointer inside the focused window's content, off the titlebar Rosemary draws; pushed against the edge it holds still and keeps reporting deltas. A workaround, not Mac behavior: some ports (Feral's, e.g. Life Is Strange) confine the cursor with a Win32 `ClipCursor` emulation that clamps only once and counts on warping the pointer back when it strays, which cannot reach a pointer that has left the window on Wayland. Their drag interactions stop short without it. |
| `ROSEMARY_GLIBC` | `auto` | (packaged builds) Which glibc the launcher runs Rosemary on: `host`, `bundled`, or `auto`, which uses the host's when it defines every symbol version the bundle's libraries need and the bundled one otherwise. |
| `ROSEMARY_EXE` | (set by the launcher) | The path that starts another copy of Rosemary. Under the bundled glibc the kernel reports the bundled dynamic loader as this process's executable, so the runtime re-launches itself (`rosemary server`, a guest spawning a Mach-O) through this instead. |
| `ROSEMARY_METAL_PRESENTATION_MODE` | unset | Set the presentation mode used by Vulkan in Rosemary's Metal translation. Valid values are `immediate`, `fifo`, `fifo_relaxed`, and `mailbox`. |
| `ROSEMARY_SCREEN_REFRESH_RATE` | unset | Hertz that guests are told the display refreshes at, in place of the host's mode: `NSScreen`/`UIScreen` `maximumFramesPerSecond` and the refresh-interval properties. |

## Prefixes

A **prefix** is the Darwin filesystem a guest runs in - a directory whose
`root/` is the guest's `/`. Off macOS, `rosemary run` makes one at
`~/.local/share/rosemary/prefixes/default` the first time it is used and runs
every guest in it, because this host has no `/Users`, no `/Applications` and no
`/Library`, and a Mac program told otherwise writes its files into a shape
nothing else agrees about.

```sh
rosemary prefix create            # the default prefix, made on first run anyway
rosemary prefix create ~/prefixes/game
rosemary prefix info              # where it is, and what it says about itself
```

A guest does **not** have to be inside the prefix to run: the app stays wherever
it is and the guest sees it under `/Volumes/Host`, so
`rosemary run ~/games/Foo.app/Contents/MacOS/Foo` works with no setup.

To run a guest in a prefix of its own for separate saves/preferences:

```sh
ROSEMARY_PREFIX=~/prefixes/game rosemary run ~/games/Foo.app/Contents/MacOS/Foo
```

### The Darwin base

A prefix also holds a **base**: the [MiniDarwin](https://github.com/V3L0C1T13S/minidarwin)
userland - real `/usr/lib` dylibs, the BSD tools in `/bin` and `/usr/bin`, and a
terminfo database. The system libraries a guest *links against* are still this
runtime's own reimplementations; the base is there for a guest that stats,
reads or `dlopen`s a system file, and so that a prefix has a real Darwin
filesystem shape. A prefix without one still runs guests.

A new prefix gets its base when it is created, and nothing is downloaded
without consent: `rosemary run` asks with a dialog or on the terminal, and
`ROSEMARY_PREFIX_SETUP` answers in advance. The download (about 2 MB) is
checked against hashes compiled into this build of Rosemary, and kept in a
cache so that every other prefix, and every repair, works offline.

```sh
rosemary prefix create                        # shape, then base for this host's architecture
rosemary prefix create --arch x86_64          # the base for a given guest architecture
rosemary prefix create --bundle B.bundle.zip  # offline, from a MiniDarwin release bundle
rosemary prefix create --spec S.spec.yaml     # a MiniDarwin image this build does not pin
rosemary prefix create --system-files immutable
rosemary prefix create --no-base              # the shape only

rosemary prefix verify         # hash every system file against the manifest
rosemary prefix repair         # put back what differs, offline (--dry-run to preview)
rosemary prefix reset-system   # also moves aside what a user put in a system directory
rosemary prefix update         # move to the release this build pins (--allow-downgrade)
rosemary prefix cache list     # cached bases, and which prefixes use them
rosemary prefix cache gc       # remove the ones nothing uses
```

`--bundle` checks the bundle against the pinned spec (or `--spec`), never
against the spec inside it. That file travelled with what it vouches for.

`--system-files` decides what happens when a guest writes a file the base owns:

* `repair` (the default) - allowed, and restored at the guest's next start;
* `warn` - allowed, and reported at every start until `prefix repair`;
* `immutable` - refused with `EROFS`. The base's files are then hardlinked
  from the cache where the filesystem cannot clone them.

Nothing a user made is ever deleted. `repair` and `reset-system` move
anything in the way to `<prefix>/.rosemary/displaced/`.

When a guest behaves differently inside a prefix, `ROSEMARY_TRACE_PREFIX=1`
prints every path translation as a pair, which is almost always enough to
see which one is wrong.

## GUI

On macOS with native Objective C, Rosemary has a built in GUI that can be toggled with `SHIFT` + `F7`. This GUI shows the framerate of the application, and has various profiling utilities built in, useful for narrowing down performance bottlenecks.

## Execution modes

`rosemary run --objc-debug <mach-o> [args...]` enables expensive, fail-fast
checks in the built-in Objective-C runtime. It adds allocation tail guards,
quarantines finalized objects, and validates reference counting, weak slots,
autorelease pools, synchronization, selectors, and class handles. The option
requires `ROSEMARY_OBJC=builtin`; it is intended for diagnosis rather than
normal execution and can substantially increase memory use.

`rosemary prefix <create|info|verify|repair|reset-system|update> [PREFIX]`
manages a prefix, defaulting to the standard one when no path is given, and
`rosemary prefix cache <list|gc>` the cache of Darwin bases. See the Prefixes
section above.

Rosemary has two execution modes currently:

* native - which runs the app natively if it has a slice for your CPU
* emulated - which runs the app under Microlift's x86-64 recompilation.

If not specified at the CLI, Rosemary will automatically try to see if there is a slice available in the app for your native architecture and run it without getting Microlift involved. Otherwise, it will check if it has an emulated implementation for the architecture.

ARM emulation is not supported yet, but the groundwork is laid for it.

## Built-in OpenGL

`ROSEMARY_OBJC=builtin` uses the same SDL3-backed OpenGL and CGL compatibility
implementation on every host, including macOS. Linked imports and dynamic
lookups use that implementation; unsupported GL/CGL functions do not fall back
to Apple's OpenGL framework. Native Objective-C continues to use native frameworks.

CGL handles obtained from `NSOpenGLContext` belong to Rosemary. Querying a handle
does not create or bind a GL context. The Objective-C and CGL current-context
queries share thread-local state, and swap interval settings apply to the named
context. Contexts move between threads through a bounded handoff; an EGL backend
can fall back to a sibling sharing textures and buffers, but not vertex arrays,
framebuffers, or queries. Other backends report contention if handoff fails.

Direct CGL pixel-format/context creation and lifetime APIs, and explicit context
sharing, remain unsupported. The built-in GLU surface provides `gluOrtho2D`
through SDL-resolved `glOrtho`; other GLU functions remain unsupported. No host
GLU library is required. Driver capabilities still determine available profiles
and extensions.
