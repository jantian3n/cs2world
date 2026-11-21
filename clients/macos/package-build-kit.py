"""Bundle the Swift AppKit source and the prebuilt macOS XCFramework for offline builds."""
import argparse
import hashlib
from pathlib import Path
import tempfile
import zipfile

SOURCES = ("main.swift", "Info.plist", "build-app.sh", "verify-app.sh", "README.md")
NAME = "CS2World-macOS-build-kit"


def main():
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("framework", type=Path, help="path to CS2World-xcframework.zip")
    parser.add_argument("--out", type=Path, help="output directory (default: alongside the XCFramework ZIP)")
    args = parser.parse_args()
    output = args.out or args.framework.parent
    output.mkdir(parents=True, exist_ok=True)
    files = {name: (here / name).read_bytes() for name in SOURCES}
    files["CS2World-xcframework.zip"] = args.framework.read_bytes()
    manifest = "".join(f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in files.items())
    files["SHA256SUMS"] = manifest.encode("utf-8")
    with tempfile.TemporaryDirectory(dir=output) as tmp:
        path = Path(tmp) / f"{NAME}.zip"
        with zipfile.ZipFile(path, "w") as bundle:
            for name, data in files.items():
                info = zipfile.ZipInfo(f"{NAME}/{name}")
                info.create_system = 3
                info.external_attr = (0o100755 if name.endswith(".sh") else 0o100644) << 16
                method = zipfile.ZIP_STORED if name.endswith(".zip") else zipfile.ZIP_DEFLATED
                bundle.writestr(info, data, compress_type=method)
        with zipfile.ZipFile(path) as bundle:
            if bundle.testzip() is not None:
                raise RuntimeError("invalid build kit archive")
            for name, expected in files.items():
                if bundle.read(f"{NAME}/{name}") != expected:
                    raise RuntimeError(f"build kit content mismatch: {name}")
        result = output / path.name
        path.replace(result)
    print(f"{result}: {result.stat().st_size} bytes, SHA-256 {hashlib.sha256(result.read_bytes()).hexdigest()}")


if __name__ == "__main__":
    main()
