#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
out=${1:-"$root/target/packages/macos"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
export MACOSX_DEPLOYMENT_TARGET=12.0
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$root/target"}
include="$root/crates/cs2world-ffi/include"
for target in x86_64-apple-darwin aarch64-apple-darwin; do
    cargo build -p cs2world-ffi --locked --release --target "$target"
    cargo build -p cs2world-server --locked --release --target "$target"
done
mkdir "$stage/cli" "$stage/verify-cli"
xcrun lipo -create "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/cs2world-server" \
    "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/cs2world-server" -output "$stage/cli/cs2world"
chmod 755 "$stage/cli/cs2world"
/usr/bin/codesign --force --options runtime --sign "${CS2W_SIGN_IDENTITY:--}" "$stage/cli/cs2world"
ditto -c -k "$stage/cli" "$stage/CS2World-macOS-CLI.zip"
ditto -x -k "$stage/CS2World-macOS-CLI.zip" "$stage/verify-cli"
cli="$stage/verify-cli/cs2world"
test -x "$cli"
xcrun lipo "$cli" -verify_arch arm64 x86_64
/usr/bin/codesign --verify --strict --all-architectures --verbose=2 "$cli"
"$cli" version
"$cli" check -c configs/cs2world_client.json
"$cli" check -c configs/cs2world_server.json

xcrun lipo -create "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/libcs2world_ffi.a" \
    "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/libcs2world_ffi.a" -output "$stage/libcs2world_ffi.a"
xcodebuild -create-xcframework -library "$stage/libcs2world_ffi.a" -headers "$include" -output "$stage/CS2World.xcframework"
ditto -c -k --keepParent "$stage/CS2World.xcframework" "$stage/CS2World-xcframework.zip"
bash clients/macos/build-app.sh "$stage/CS2World-xcframework.zip" "$out"
cp "$stage/CS2World-xcframework.zip" "$stage/CS2World-macOS-CLI.zip" "$out/"
shasum -a 256 "$out/CS2World-xcframework.zip" "$out/CS2World-macOS-CLI.zip"
