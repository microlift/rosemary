# Native macOS window geometry probe

This measures real AppKit behavior to decide how Rosemary should implement
fullscreen sizing. It does not run Roblox or assume that the proposed fix is
correct. Source is included; the launcher compiles an arm64 macOS application
locally with Apple's Command Line Tools. Requires macOS 12 or later.

## Run

Unzip this folder on your Apple Silicon Mac, open Terminal, and run:

```bash
cd /path/to/rosemary-macos-window-probe
bash run.command
```

If the compiler is missing, install it with `xcode-select --install`, finish
the installation, then run again. Full Xcode is not required. Use a native
Terminal session rather than running Terminal under Rosetta.

The probe automatically opens windows and enters/leaves macOS fullscreen
Spaces. Leave its windows alone while it runs. The full run takes several
minutes; progress is printed in Terminal and in each window's title. Cmd-Q
or closing the probe window stops the run and saves partial results. A failed
fullscreen transition stops the run with an explicit reason; each case also
has a 35-second watchdog.

When finished, return the ZIP path printed as **Results archive to return**.
It lives inside `results/` beside the launcher. No screenshot, Accessibility,
or screen-recording permission is requested. The program does not use the
network, read Roblox data, change system preferences, or save window positions
to preferences.

## The 70-case matrix

Each combination gets a fresh window:

| Dimension | Cases |
|---|---|
| Window style | Standard titled/resizable; full-size content view with transparent titlebar |
| Operation | `setFrame:display:NO`, `setFrame:display:YES`, `setFrame:display:YES animate:YES`, `setContentSize:`, `setFrameOrigin:`, burst of four `setFrame:` requests, Roblox-shaped style/frame sequence |
| Timing | Settled windowed, zoomed, inside `windowWillEnterFullScreen:`, settled fullscreen, inside `windowWillExitFullScreen:` |

The ordinary frame request shortens the current height by 31 points and moves
its origin up by 31 points. The content-size request shortens content height
by 31 points. The burst requests heights of original minus 33, minus 31,
half the original, and original plus 71. These values reproduce the shape of
the observed Roblox calls without encoding a presumed correct result.

The `roblox-transition` operation removes the full-size-content-view style bit
while preserving the native fullscreen state bit, then submits two frames
derived from the display: height minus 33 at y plus 32, then height minus 31
at y plus 31. This includes the style change seen in the live trace, rather
than testing only isolated resize calls. It reproduces the observed shape in
points; it does not claim to replay every internal Roblox call.

Every case enters and exits fullscreen, records the restored frame, and then
requests an ordinary 640x420-point content resize. This checks whether requests
are ignored, applied immediately, deferred, or affect the restoration frame,
and whether later normal resizing still works. The zoomed cases also record
the actual `isZoomed` result: zoom is an AppKit operation, not an assumption
that macOS has identical semantics to a Linux maximize button.

Samples are taken before calls, immediately after return, after 100ms and
500ms, and 800–900ms after fullscreen completion notifications. Resize, move,
backing-scale, and fullscreen lifecycle callbacks are recorded as they occur.
Mutations in `will` callbacks are intentional transition tests. Objective-C
exceptions from the tested operations are recorded rather than classified as
incorrect native behavior.

## Reports

- `events.jsonl`: ordered, flushed observations, OS/process architecture,
  display geometry, backing scale, display safe-area insets, every requested
  rectangle and each window/content/subview/layer geometry readback. Also logs
  instance and class frame/content conversions for the current style mask.
- `summary.json`: completed/planned cases, interruption reason, caught
  exceptions, initial/pre-fullscreen/restored/final frames, and restored content geometry.
- `console.log`: progress, AppKit diagnostics, and fatal runtime errors.

Coordinates are **points**, except for the explicitly named `backingBounds`.
`rectInWindow` is produced by AppKit coordinate conversion; do not interpret a
view's superview-relative `frame` as a screen-relative rectangle.

There is deliberately no semantic PASS/FAIL verdict. AppKit is the oracle.
`reason: completed` means the experiment completed, not that the Rosemary patch
has been proven correct. Inspect `operationApplied`, exceptions, callback order,
and restored geometry when comparing results.

## Smaller runs and repeat experiments

```bash
# Ten cases: standard windows, frame/content-size setters, all five timings.
bash run.command --quick

# Show the exact planned cases without opening windows.
bash run.command --list

# Repeat only the core fullscreen request.
bash run.command --style standard --operation frame --phase fullscreen

# Examine restoration after a content-size request during exit.
bash run.command --operation content-size --phase will-exit

# Run the full matrix on the second connected display (zero-based index).
bash run.command --screen 1
```

Valid styles: `standard`, `full-size-content`.
Valid operations: `frame`, `frame-display`, `frame-animate`, `content-size`,
`origin`, `burst`, `roblox-transition`.
Valid phases: `windowed`, `zoomed`, `will-enter`, `fullscreen`, `will-exit`.

For broader host coverage, repeat the run on the built-in display and an
external display if available using `--screen INDEX` (default `0`). Indices
follow the `screens` array recorded in the first event. Every fresh window is
created and centered on that display. Display changes, Spaces preferences, display scaling, and OS
versions are separate host configurations; this finite matrix cannot exhaust
every possible desktop setup.

## Validation status

The launcher was prepared on Linux and then compiled and run by the reporter
on an Apple Silicon Mac running macOS 26.5 (25F71). The completed run contains
70 cases, zero caught exceptions, and 2542 ordered observations. The original
helper name `Rect` collided with a macOS typedef; this source uses the corrected
`ProbeRectJSON` name and retains `NSMakeRect` unchanged.

See `../../crates/appkit/tests/fixtures/fullscreen-native.md` for evidence
provenance, extracted observations, comparison coverage, and limitations.

API references: [NSWindowDelegate](https://developer.apple.com/documentation/appkit/nswindowdelegate),
[NSWindow frame setter](https://developer.apple.com/documentation/appkit/nswindow/setframe(_:display:animate:)).
