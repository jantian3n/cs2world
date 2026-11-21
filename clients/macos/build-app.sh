#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
framework=${1:-"$here/CS2World-xcframework.zip"}
out=${2:-"$PWD/build-macos"}
if [[ $(uname -s) != Darwin ]]; then
    echo "Build the AppKit client on a Mac with Xcode or its command line tools." >&2
    exit 1
fi
if [[ -f "$here/SHA256SUMS" ]]; then
    (cd "$here" && /usr/bin/shasum -a 256 -c SHA256SUMS)
fi
export MACOSX_DEPLOYMENT_TARGET=12.0
sdk=$(xcrun --sdk macosx --show-sdk-path)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
if [[ -f "$framework" ]]; then
    /usr/bin/ditto -x -k "$framework" "$stage"
    framework="$stage/CS2World.xcframework"
fi
slice="$framework/macos-arm64_x86_64"
lib="$slice/libcs2world_ffi.a"
xcrun lipo "$lib" -verify_arch arm64 x86_64
/usr/bin/plutil -lint "$framework/Info.plist" "$here/Info.plist"
for arch in arm64 x86_64; do
    xcrun --sdk macosx swiftc -swift-version 5 -O -target "$arch-apple-macosx12.0" -sdk "$sdk" \
        -module-cache-path "$stage/modules-$arch" -I "$slice/Headers" "$here/main.swift" "$lib" \
        -framework AppKit -framework Security -framework SystemConfiguration -framework CoreFoundation \
        -liconv -lresolv -Xlinker -rpath -Xlinker /usr/lib/swift -o "$stage/CS2World-$arch"
done
app="$stage/CS2World.app"
mkdir -p "$app/Contents/MacOS" "$out"
cp "$here/Info.plist" "$app/Contents/Info.plist"
xcrun lipo -create "$stage/CS2World-arm64" "$stage/CS2World-x86_64" -output "$app/Contents/MacOS/CS2World"
chmod 755 "$app/Contents/MacOS/CS2World"
/usr/bin/codesign --force --options runtime --sign "${CS2W_SIGN_IDENTITY:--}" "$app"
/usr/bin/ditto -c -k --keepParent "$app" "$stage/CS2World-macOS.zip"
bash "$here/verify-app.sh" "$stage/CS2World-macOS.zip"
cp "$stage/CS2World-macOS.zip" "$out/CS2World-macOS.zip"
/usr/bin/shasum -a 256 "$out/CS2World-macOS.zip"
