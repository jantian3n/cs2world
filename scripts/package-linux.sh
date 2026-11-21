#!/usr/bin/env bash
# Build release binaries with Rust's bundled musl CRT and LLVM linker.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# == 0 ]]; then set -- x86_64 aarch64; fi
case "$(uname -s)" in
    MINGW*|MSYS*) python_bin=${PYTHON:-python} ;;
    *) python_bin=${PYTHON:-python3} ;;
esac
for arch in "$@"; do
    case "$arch" in x86_64|aarch64) ;; *) echo "unsupported Linux architecture: $arch" >&2; exit 2;; esac
    target="$arch-unknown-linux-musl"
    rustup target add "$target"
    RUSTFLAGS="${RUSTFLAGS:-} -C linker=rust-lld -C linker-flavor=ld.lld -C link-self-contained=yes -C target-feature=+crt-static" \
        cargo build --locked --release --target "$target" -p cs2world-server
    binary="${CARGO_TARGET_DIR:-target}/$target/release/cs2world-server"
    "$python_bin" - "$binary" "$arch" <<'PY'
import hashlib
from pathlib import Path
import struct
import sys

path, arch = Path(sys.argv[1]), sys.argv[2]
data = path.read_bytes()
if data[:6] != b"\x7fELF\x02\x01":
    raise SystemExit("expected a little-endian 64-bit ELF")
kind, machine = struct.unpack_from("<HH", data, 16)
if kind not in (2, 3) or machine != {"x86_64": 62, "aarch64": 183}[arch]:
    raise SystemExit("unexpected ELF type or machine")
offset = struct.unpack_from("<Q", data, 32)[0]
size, count = struct.unpack_from("<HH", data, 54)
for index in range(count):
    off = offset + size * index
    typ, _, start, _, _, length, _, _ = struct.unpack_from("<IIQQQQQQ", data, off)
    if typ == 3:
        raise SystemExit("ELF requests a dynamic interpreter")
    if typ == 2:
        for tag, _ in struct.iter_unpack("<qQ", data[start:start + length]):
            if tag == 1:
                raise SystemExit("ELF has a DT_NEEDED dependency")
            if tag == 0:
                break
print(f"{arch} ELF64, no PT_INTERP or DT_NEEDED, {len(data)} bytes, sha256={hashlib.sha256(data).hexdigest()}")
PY
    if command -v readelf >/dev/null; then
        readelf -lW "$binary"
        readelf -dW "$binary"
    fi
    destination="dist/rust/linux-$arch"
    mkdir -p "$destination"
    install -m 0755 "$binary" "$destination/cs2world-server"
done
