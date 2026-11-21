#!/usr/bin/env bash
set -euo pipefail
artifact=${1:?Usage: bash verify-app.sh /path/to/CS2World-macOS.zip-or-app}
stage=$(mktemp -d)
launcher=
cleanup() {
    if [[ -n "$launcher" ]]; then kill "$launcher" 2>/dev/null || true; fi
    rm -rf "$stage"
}
trap cleanup EXIT
if [[ -f "$artifact" ]]; then
    /usr/bin/ditto -x -k "$artifact" "$stage"
    app="$stage/CS2World.app"
else
    app="$artifact"
fi
binary="$app/Contents/MacOS/CS2World"
/usr/bin/plutil -lint "$app/Contents/Info.plist"
test "$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$app/Contents/Info.plist")" = CS2World
test -x "$binary"
xcrun lipo "$binary" -verify_arch x86_64 arm64
/usr/bin/codesign --verify --strict --all-architectures --verbose=2 "$app"
for arch in arm64 x86_64; do
    libs=$(xcrun otool -arch "$arch" -L "$binary")
    printf '%s\n' "$libs"
    if ! grep -Fq '/AppKit.framework/' <<<"$libs"; then
        echo "Missing AppKit dependency in $arch" >&2
        exit 1
    fi
done
# Launch through macOS Launch Services in the current login session.
receipt="$stage/launch-result"
/usr/bin/open -n -W "$app" --args --verify-launch "$receipt" &
launcher=$!
for ((attempt = 0; attempt < 150; attempt++)); do
    if ! kill -0 "$launcher" 2>/dev/null; then break; fi
    sleep 0.2
done
if kill -0 "$launcher" 2>/dev/null; then
    echo "App did not finish the launch check within 30 seconds." >&2
    exit 1
fi
wait "$launcher"
launcher=
if [[ ! -f "$receipt" ]] || [[ $(cat "$receipt") != 'window,abi,start,stop,destroy' ]]; then
    echo "The app did not confirm a visible window, local listener, stop callback and shutdown." >&2
    exit 1
fi
printf 'Passed AppKit launch and local core lifecycle on %s.\n' "$(uname -m)"
