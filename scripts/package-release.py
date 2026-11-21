"""Assemble the locally built platform products in dist/rust and verify their hashes."""

import hashlib
import json
import os
from pathlib import Path
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "dist" / "rust"

PRODUCTS = (
    "windows/CS2World-XP.exe",
    "windows-service/cs2world-server.exe",
    "linux-x86_64/cs2world-server",
    "linux-aarch64/cs2world-server",
    "macos/cs2world",
    "macos/CS2World-xcframework.zip",
    "android/CS2World-debug.apk",
    "android/CS2World-android-libs.zip",
    "android/CS2World-androidTest.apk",
    "android/device-test.json",
)
SUPPORT = {
    "README.md": "docs/rust-release.md",
    "VERIFICATION.md": "docs/rust-verification.md",
    "configs/cs2world_server.json": "configs/cs2world_server.json",
    "configs/cs2world_client.json": "configs/cs2world_client.json",
    "linux/deploy-linux.sh": "scripts/deploy-linux.sh",
    "licenses/Wintun.txt": "crates/cs2world-core/vendor/wintun/LICENSE.txt",
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def build():
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    entries = {name: DIST / name for name in PRODUCTS}
    for name in ("macos/CS2World-macOS.zip", "macos/CS2World-macOS-build-kit.zip"):
        if (DIST / name).is_file():
            entries[name] = DIST / name
    if not ({"macos/CS2World-macOS.zip", "macos/CS2World-macOS-build-kit.zip"} & entries.keys()):
        raise FileNotFoundError("Build the macOS app or its offline Xcode build kit first")
    entries.update({name: ROOT / path for name, path in SUPPORT.items()})
    report = json.loads((DIST / "android/device-test.json").read_text(encoding="utf-8"))
    digest = {}
    for name, path in entries.items():
        if not path.is_file():
            raise FileNotFoundError(path)
        digest[name] = sha(path.read_bytes())
    expected_android = {Path(name).name for name in PRODUCTS if name.startswith("android/") and not name.endswith(".json")}
    if set(report["artifacts"]) != expected_android:
        raise ValueError("Android device report does not identify all packaged binaries")
    for name, evidence in report["artifacts"].items():
        if digest["android/" + name] != evidence["sha256"]:
            raise ValueError(f"Android device evidence does not match {name}")
    if not all(report["instrumentation"][name]["passed"] for name in ("NativeBridgeTest", "VpnDeviceTest")):
        raise ValueError("Android instrumentation did not pass")
    if set(report["host_echo"]) != {"TCP", "UDP"}:
        raise ValueError("Android device report is incomplete")

    manifest = "".join(f"{digest[name]}  {name}\n" for name in sorted(digest))
    name = f"CS2World-rust-{version}.zip"
    with tempfile.TemporaryDirectory(dir=DIST) as tmp:
        packaged = Path(tmp) / name
        with zipfile.ZipFile(packaged, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for entry, path in entries.items():
                info = zipfile.ZipInfo(entry)
                info.create_system = 3
                mode = 0o100755 if entry.endswith(("cs2world-server", ".sh")) or entry == "macos/cs2world" else 0o100644
                info.external_attr = mode << 16
                archive.writestr(info, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED)
            archive.writestr(zipfile.ZipInfo("SHA256SUMS.txt"), manifest)
        with zipfile.ZipFile(packaged) as archive:
            if set(archive.namelist()) != set(entries) | {"SHA256SUMS.txt"}:
                raise ValueError("package inventory mismatch")
            for entry, checksum in digest.items():
                if sha(archive.read(entry)) != checksum:
                    raise ValueError(f"read-back verification failed for {entry}")
        final = DIST / name
        os.replace(packaged, final)
        checksums = Path(tmp) / "checksum"
        checksums.write_text(f"{sha(final.read_bytes())}  {name}\n", encoding="ascii")
        os.replace(checksums, DIST / (name + ".sha256"))
    print(f"{final} ({final.stat().st_size} bytes, {len(entries)} files plus manifest)")


if __name__ == "__main__":
    build()
