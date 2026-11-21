"""Cross-build the Rust-only macOS XCFramework using cargo and LLVM tools.

The native package.sh builds and verifies the Swift .app on macOS.
"""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[2]
ARCHS = {"x86_64": "x86_64", "arm64": "aarch64"}
LIB = "libcs2world_ffi.a"


def execute(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def prepare_workspace(work):
    for item in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(ROOT / item, work / item)
    shutil.copytree(ROOT / "crates", work / "crates")
    shutil.copytree(ROOT / "win32-xp", work / "win32-xp",
                    ignore=shutil.ignore_patterns("target", "data", "binaries"))
    locked = tomllib.loads((work / "Cargo.lock").read_text(encoding="utf-8"))["package"]
    versions = {p["name"]: p for p in locked if p["name"] in ("route_manager", "tun-rs")}
    registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/cache"
    deps = work / "vendor-static"
    deps.mkdir()
    patch = ["", "[patch.crates-io]"]
    for name in ("route_manager", "tun-rs"):
        dep = versions[name]
        stem = f"{name}-{dep['version']}"
        crate = next((path for path in registry.glob(f"*/{stem}.crate")
                      if digest(path) == dep["checksum"]), None)
        if not crate:
            raise RuntimeError(f"Run cargo fetch --locked to obtain the verified {stem}.crate")
        print(f"{stem} verified SHA-256 {dep['checksum']}", flush=True)
        with tarfile.open(crate, "r:gz") as archive:
            archive.extractall(deps, filter="data")
        manifest = deps / stem / "Cargo.toml"
        raw = manifest.read_text(encoding="utf-8")
        types = tomllib.loads(raw)["lib"]["crate-type"]
        if not {"staticlib", "cdylib"}.issubset(types):
            raise RuntimeError(f"{stem} crate types changed; recheck this cross-build workaround")
        new, count = re.subn(r"(?ms)^crate-type\s*=\s*\[[^\]]*\]", 'crate-type = ["rlib"]', raw)
        if count != 1:
            raise RuntimeError(f"{stem} manifest has unexpected library layout")
        manifest.write_text(new, encoding="utf-8")
        patch.append(f'{name} = {{ path = "vendor-static/{stem}" }}')
    with (work / "Cargo.toml").open("a", encoding="utf-8") as manifest:
        manifest.write("\n".join(patch) + "\n")
    execute(["cargo", "metadata", "--filter-platform", "aarch64-apple-darwin", "--format-version", "1"], cwd=work, stdout=subprocess.DEVNULL)
    before = {(p["name"], p["version"]): p for p in locked}
    after = {(p["name"], p["version"]): p for p in tomllib.loads((work/"Cargo.lock").read_text(encoding="utf-8"))["package"]}
    if before.keys() != after.keys():
        raise RuntimeError("unexpected dependency selection in temporary workspace")
    for key, original in before.items():
        revised = dict(after[key])
        if key[0] in versions:
            revised.update(source=original["source"], checksum=original["checksum"])
        if revised != original:
            raise RuntimeError(f"unexpected lockfile change in {key[0]}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--llvm-tools", type=Path, help="directory containing llvm-objdump, llvm-ar and llvm-nm (rustup component add llvm-tools-preview)")
    parser.add_argument("--out", type=Path, default=ROOT / "dist/rust/macos")
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    def llvm(name):
        tool = f"llvm-{name}{suffix}"
        return str(args.llvm_tools / tool) if args.llvm_tools else tool
    args.out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="cs2world-macos-static-") as tmp:
        work = Path(tmp)
        prepare_workspace(work)
        package(work, args.out, llvm)


def validate_objects(archive, cpu):
    # BSD ar stores extended names before the object body. Parse Mach-O's
    # mach_header_64 directly; objdump -f does not accept this format on Windows.
    data=archive.read_bytes()
    if not data.startswith(b"!<arch>\n"):
        raise RuntimeError("invalid static archive")
    pos=8
    seen=0
    while pos<len(data):
        header=data[pos:pos+60]
        if len(header)!=60 or header[58:60]!=b"`\n":
            raise RuntimeError("invalid ar member header")
        name=header[:16].strip()
        size=int(header[48:58])
        begin=pos+60
        end=begin+size
        if end>len(data):raise RuntimeError("truncated object")
        if name.startswith(b"#1/"):
            count=int(name[3:])
            name=data[begin:begin+count].rstrip(b"\0")
            begin+=count
        if not (name.startswith(b"__.SYMDEF") or name in (b"/", b"//")):
            magic,actual,sub,kind=struct.unpack_from("<IIII",data,begin)
            if magic!=0xfeedfacf or actual!=cpu or kind!=1:
                raise RuntimeError(f"invalid Mach-O member {name!r}: magic={magic:#x}, CPU={actual:#x}, filetype={kind}")
            seen+=1
        pos=end+size%2
    if not seen:
        raise RuntimeError("no Mach-O objects")


def package(work,out,llvm):
    target = work / "target"
    env = dict(os.environ, MACOSX_DEPLOYMENT_TARGET="12.0", CARGO_TARGET_DIR=str(target))
    framework = work / "CS2World.xcframework"
    slice_dir = framework / "macos-arm64_x86_64"
    headers = slice_dir / "Headers"
    shutil.copytree(work / "crates/cs2world-ffi/include", headers)
    required = set(re.findall(r"\b(cs2w_\w+)\s*\(", (headers/"cs2world.h").read_text(encoding="utf-8")))
    thin = []
    for apple, rust in ARCHS.items():
        triple = f"{rust}-apple-darwin"
        execute(["cargo","rustc","--locked","--offline","--release","-p","cs2world-ffi",
                 "--lib","--crate-type","staticlib","--target",triple],env=env,cwd=work)
        lib = target / triple / "release" / LIB
        validate_objects(lib, {"x86_64": 0x01000007, "arm64": 0x0100000C}[apple])
        members = subprocess.check_output([llvm("ar"),"t",str(lib)],text=True).splitlines()
        if not members:
            raise RuntimeError("empty static library")
        nm = subprocess.check_output([llvm("nm"),"--defined-only","--extern-only",str(lib)],text=True,stderr=subprocess.DEVNULL)
        symbols = set(re.findall(r"\b[Tt]\s+_(cs2w_\w+)$", nm, flags=re.M))
        if not required <= symbols:
            raise RuntimeError(f"{apple} is missing {required-symbols}")
        print(f"{apple}: {len(members)} archive objects, {len(required)} public C functions verified", flush=True)
        thin.append(lib)
    universal = slice_dir / LIB
    # Apple mach-o/fat.h: big-endian fat_header and five-word fat_arch
    # records. Align archive slices to 16 KiB (valid for both CPUs).
    page=1 << 14
    next_offset=page
    entries=[]
    for cpu,subcpu,lib in zip((0x01000007,0x0100000C),(3,0),thin):
        size=lib.stat().st_size
        entries.append((cpu,subcpu,next_offset,size,14))
        next_offset=(next_offset+size+page-1)//page*page
    with universal.open("wb") as fat:
        fat.write(struct.pack(">II",0xCAFEBABE,len(entries)))
        for entry in entries:
            fat.write(struct.pack(">IIIII",*entry))
        for (_,_,offset,size,_),lib in zip(entries,thin):
            fat.seek(offset)
            with lib.open("rb") as archive:shutil.copyfileobj(archive,fat)
    headers_dump=subprocess.check_output([llvm("objdump"),"--macho","--universal-headers",str(universal)],text=True)
    if "x86_64" not in headers_dump or "arm64" not in headers_dump:
        raise RuntimeError(f"LLVM did not recognize both universal archive slices: {headers_dump[:1000]}")
    with universal.open("rb") as fat:
        if struct.unpack(">II",fat.read(8))!=(0xCAFEBABE,2):raise RuntimeError("invalid universal header")
        checked=[struct.unpack(">IIIII",fat.read(20)) for _ in entries]
        for (_,_,offset,size,align),lib in zip(checked,thin):
            fat.seek(offset)
            if offset%(1<<align) or hashlib.sha256(fat.read(size)).hexdigest()!=digest(lib):
                raise RuntimeError("universal archive slice hash/alignment mismatch")
    info = dict(CFBundlePackageType="XFWK", XCFrameworkFormatVersion="1.0", AvailableLibraries=[
        dict(LibraryIdentifier=slice_dir.name, LibraryPath=LIB, HeadersPath="Headers",
             SupportedArchitectures=["arm64","x86_64"], SupportedPlatform="macos")])
    (framework / "Info.plist").write_bytes(plistlib.dumps(info))
    result = out / "CS2World-xcframework.zip"
    staged = work / result.name
    with zipfile.ZipFile(staged, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as package:
        for file in sorted(framework.rglob("*")):
            if file.is_file():package.write(file, file.relative_to(work).as_posix())
    with zipfile.ZipFile(staged) as package:
        if package.testzip() is not None:raise RuntimeError("invalid XCFramework ZIP")
    shutil.copy2(staged,result)
    print(f"{result} ({result.stat().st_size} bytes, SHA-256 {digest(result)})",flush=True)


if __name__ == "__main__":
    main()
