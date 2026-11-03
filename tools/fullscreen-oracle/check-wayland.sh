#!/usr/bin/env bash
# Pixel-check immediate idle OpenGL clipping on a genuine Wayland compositor,
# nested on Xvfb solely to capture its displayed pixels reproducibly.
set -euo pipefail
cd "$(dirname "$0")/../.."
report=$(mktemp)
trap 'rm -f "$report"' EXIT
${CARGO:-cargo} test --offline --release -p sdl3 -p appkit --no-run --message-format=json > "$report"
mapfile -t bins < <(python3 - "$report" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    try: row=json.loads(line)
    except ValueError: continue
    target=row.get('target',{})
    if row.get('reason')=='compiler-artifact' and row.get('executable') and \
       target.get('kind')==['lib'] and target.get('name') in ('sdl3','appkit'):
        print(target.get('name')+' '+row['executable'])
PY
)
((${#bins[@]} == 2)) || { echo "Expected SDL3 and AppKit tests, found ${#bins[@]}" >&2; exit 1; }
sdl_bin=''; appkit_bin=''
for entry in "${bins[@]}"; do
    case "$entry" in
        sdl3\ *) sdl_bin=${entry#sdl3 };;
        appkit\ *) appkit_bin=${entry#appkit };;
    esac
done
[[ -x "$sdl_bin" && -x "$appkit_bin" ]] || { echo 'Missing executable' >&2; exit 1; }
xvfb-run -a -s '-screen 0 1440x900x24 -nolisten tcp' bash -euo pipefail -c '
    socket=rosemary-fullscreen-wayland-$$
    xfwm4 --compositor=off >/dev/null 2>&1 & wm=$!
    weston -B x11 --renderer=pixman --shell=kiosk --width=1440 --height=900 \
      --no-config -S "$socket" --log=/tmp/rosemary-fullscreen-weston.log >/dev/null 2>&1 & weston=$!
    trap "kill $weston $wm 2>/dev/null || true" EXIT
    ready=0
    for ((i=0;i<100;i++)); do
        if [[ -S "$XDG_RUNTIME_DIR/$socket" ]] && WAYLAND_DISPLAY="$socket" wayland-info >/dev/null 2>&1; then
            ready=1; break
        fi
        sleep .1
    done
    if (( !ready )); then cat /tmp/rosemary-fullscreen-weston.log >&2; exit 1; fi
    WAYLAND_DISPLAY="$socket" SDL_VIDEODRIVER=wayland ROSEMARY_WAYLAND_MASK_PIXELS=1 \
      timeout 30 "$1" --exact wayland_mask::tests::idle_opengl_wayland_mask_survives_real_compositor_roundtrips \
      --ignored --test-threads=1 --nocapture
    WAYLAND_DISPLAY="$socket" SDL_VIDEODRIVER=wayland \
      timeout 30 "$2" --exact window::fullscreen_tests::wayland_idle_fullscreen_opengl_frame_is_masked_from_nswindow_resize \
      --ignored --test-threads=1 --nocapture
' bash "$sdl_bin" "$appkit_bin"
