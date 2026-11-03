# Fullscreen work areas and native pixels

The regression fixture `fullscreen-workarea-pixels.tsv` selects observations from a native arm64 AppKit/Metal program run on macOS 26.5 (25F71), with a 1440x900-point display at 2x scale and zero safe-area insets. The run completed five cases, 503 ordered observations and 50 PNG screenshots.

Capture archive SHA-256: `6b06f30b71dd32ad63e71ca25e8751e55b844a9c88595edb3574065f3637b235`.

Native source SHA-256: `4122a6396ebd3f6d3e1ab77a56746f0ec2415c471d6fea019811f1927076d393`.

The source, launcher, full native captures and analysis are preserved locally in the investigation evidence. Full-display screenshots are not checked in because the windowed baselines contain unrelated desktop content. Fixture sequence numbers identify the screenshot-saved events. Edge rows were counted as black where at least 99% of pixels have all RGB channels below 8, and converted from pixels to points using the captured 2x scale.

## What native macOS established

- A settled native fullscreen window fills the 1440x900 display. Its fresh `NSScreen.visibleFrame` is (0,0,1440,870), retaining a 30-point top area while removing the ordinary desktop's bottom Dock reservation.
- Setting the native window to that fresh visible frame is accepted; the screenshot has 60 black pixels at the top.
- An explicit frame (0,31,1440,869) is accepted; the screenshot has 62 black pixels at the bottom.
- Those pixels agree for AppKit drawing, active Metal with resized drawables, active Metal retaining the full drawable, and an idle already-presented Metal image.
- A separate borderless/auto-hide control reports a full-display visible frame. It exposes desktop content around a shortened window and is not evidence that native fullscreen stretches pixels.

The 30-point top reservation is measured from the fresh screen observations; the probe did not independently measure NSMenu.menuBarHeight. The fixture is evidence for preserving a global top area, not an exhaustive specification of macOS presentation options, notched screens or multiple displays.

## Rosemary's SDL policy

Host desktop panel reservations constrain ordinary windows. A shown, non-minimized fullscreen surface covers those panels on its display. Its visible frame removes those desktop reservations while retaining the framework's own global menu-bar height. Rosemary currently places menus inside each window and reports a zero-height global menu bar, so this backend has no global top area to reserve. That host-chrome policy explains why the SDL result can fill its display while the captured Mac retained 30 points; this is not a claim that macOS always returns a full-display visible frame.

Explicit guest frame changes and presentation clipping are preserved. A smaller fullscreen window can still leave a black band, matching the native pixel reference. Other displays and hidden/minimized windows must not acquire a fullscreen work area.

## Linux reproducer and checks

On X11/xfwm4, a 31-pixel bottom Xfce panel advertises a work area of 2560x1409 on a 2560x1440 display. Unmodified Rosemary passes it through during Roblox's join/leave resize and ends with frame (0,31,2560,1409). Temporarily changing only `_NET_WORKAREA` to 2560x1440 removes the gap; restoring the property in the same client and leaving the experience restores the 31-pixel gap. The desktop property was restored after the experiment. This identifies work-area dependence, not a defect unique to xfwm4 or a GPU driver.

Run `CARGO='cargo +1.99.0' bash tools/fullscreen-oracle/check-linux.sh` (plus any host-specific link configuration). It reserves 31 bottom pixels inside an isolated 1440x900 Xvfb/Xfwm4 desktop, checks ordinary/fullscreen/hidden/restored screen queries, and replays all twelve native pixel bounds. Existing fullscreen/OpenGL regressions continue to verify framebuffer clipping and restoration. The fixture's geometry comparisons do not replace those renderer tests.
