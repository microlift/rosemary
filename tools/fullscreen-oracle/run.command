#!/bin/bash
set -euo pipefail
cd -- "$(dirname -- "$0")"

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
    echo "Run this on your Apple Silicon Mac from a native Terminal session."
    exit 2
fi
if ! xcrun --find clang >/dev/null 2>&1; then
    echo "Apple Command Line Tools are required. Run: xcode-select --install"
    exit 2
fi

task_app="$PWD/build/RosemaryWindowProbe.app"
mkdir -p "$task_app/Contents/MacOS"
cat > "$task_app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>local.rosemary.window-probe</string>
  <key>CFBundleName</key><string>Rosemary Window Probe</string>
  <key>CFBundleExecutable</key><string>WindowProbe</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSSupportsAutomaticTermination</key><false/>
</dict></plist>
PLIST

echo "Building native arm64 AppKit probe..."
xcrun --sdk macosx clang -arch arm64 -mmacosx-version-min=12.0 \
    -fobjc-arc -fblocks -Wall -Wextra -O0 -g \
    -framework Cocoa -framework QuartzCore \
    WindowProbe.m -o "$task_app/Contents/MacOS/WindowProbe"
codesign --force --sign - "$task_app" >/dev/null

# Listing cases should not open a window or create a results folder.
for task_arg in "$@"; do
    if [[ "$task_arg" == --list ]]; then
        exec "$task_app/Contents/MacOS/WindowProbe" "$@"
    fi
done

mkdir -p "$PWD/results"
task_results="$(mktemp -d "$PWD/results/run-$(date +%Y%m%d-%H%M%S)-XXXXXX")"
task_archive="$task_results.zip"
task_package_results() {
    task_status=$?
    trap - EXIT
    # Preserve partial results after an application crash or user interruption.
    (cd -- "$(dirname -- "$task_results")" && /usr/bin/zip -q -r "$task_archive" "$(basename -- "$task_results")") || true
    echo
    echo "Results archive to return: $task_archive"
    exit "$task_status"
}
trap task_package_results EXIT

echo "The full matrix has 70 cases and takes several minutes."
echo "Leave the probe windows alone. Cmd-Q stops and saves partial results."
echo "Output: $task_results"
"$task_app/Contents/MacOS/WindowProbe" --output "$task_results" "$@" 2>&1 | tee "$task_results/console.log"
