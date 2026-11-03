# Native fullscreen frame reference

The reporter compiled and ran `tools/fullscreen-oracle/run.command` on an arm64
Mac: macOS 26.5 (25F71), 1440x900 points, backing scale 2, reported safe-area
insets zero. Run `run-20261022-113906-UeucC7` completed 70 cases with zero caught
exceptions and 2542 ordered observations. The returned ZIP SHA-256 is:

```
90eb9b191ddec2ad7f797681da517b7ed5ec87b508735a83ae7125b480608c7a
```

Run `node tools/fullscreen-oracle/verify-capture.mjs /path/to/unpacked/run`
to validate the fixture against the raw capture and check completion/restoration.

The ZIP is retained in the local review evidence rather than adding report logs
with machine-specific data to Git. `fullscreen-macos-26.5.tsv` contains 22
observations from the 14 settled fullscreen cases: two styles times seven
operations, retaining every burst/Roblox sequence step. Its last column is the
one-based `events.jsonl` sequence number; dimensions and origins are points.
This is measured expected output, not generated from Rosemary's implementation.

Native settled fullscreen accepts a frame shortened by 31 points with its
origin raised 31 points: frame/content become 1440x869, top edge remains 900,
and the fullscreen style bit remains set. A standalone origin request is
ignored. Oversized height is constrained to 900, origin zero. In will-enter
callbacks style/frame are still windowed; in will-exit they are still fullscreen,
but origin moves and oversized frames are accepted without settled constraints.
All 42 native entry/fullscreen/exit cases restore their pre-fullscreen frames.
All 70 cases accept a subsequent ordinary 640x420 content resize.

## Rosemary comparison

`tools/fullscreen-oracle/check-linux.sh` builds AppKit tests, starts an isolated
Xvfb/xfwm4 desktop, runs the unit suite, then runs the normally ignored
`native_fullscreen_oracle` alone so it owns SDL's video thread. Requires Xvfb,
xvfb-run, xfwm4, wmctrl, Python 3 and a working Rosemary build environment.
Set `CARGO`, `CARGO_TARGET_DIR` and ordinary build/link environment as needed.

The comparison checks all 22 fixture observations, synchronous resize callback
readback, logical content size, Metal presentation coordinates, software backing
placement, IME placement, inverse pointer mapping, all 14 restored frames and
subsequent ordinary resizes. Four additional callback cases cover will-enter
frame changes and will-exit frame shortening, origin moves, and oversized frames.
A separate isolated lifecycle regression verifies that replacing the SDL window
inside will-enter uses the replacement, retaining the NSWindow across callbacks
prevents deallocation during the transition, failed entry/exit attempts notify
the correct delegate with an NSWindow argument, and guest confinement follows
logical fullscreen resizing and window restoration. SDL transition failures are
injected only by the test binary.

These are selected transition behaviors from native cases 3, 5, 25 and 30;
the entire 70-case native matrix is not replayed by the Linux test.

Fixture widths map to the host display width; heights use the captured distance
from 900, except the 450-point burst which maps to half the host height. Origins
are translated into the host display's coordinate space. This lets the test run
on other display dimensions without confusing physical pixels with points.

Negative controls substituting the original frame setter and the earlier
ignore-fullscreen-requests policy both fail the comparison. The former misses
the synchronous native resize observation; the latter returns the wrong frame.

## Limits

The Mac probe measures geometry and callbacks, not WindowServer bounds or pixels
outside a shortened window. Black outside Rosemary's logical window is a host
container policy, not a measured native color. Horizontal shrink constraints
are inferred by symmetry; only the captured full-width/vertical cases have
native evidence. This does not establish behavior on other macOS versions,
notched/external displays, Wayland, all scale factors, or direct OpenGL rendering.
Animation endpoints are covered, not native interpolation/timing. General zoom
semantics are outside this patch. Failure of SDL fullscreen requests is not
covered by the native run, which completed without transition failures.
