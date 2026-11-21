"""Cross-link the macOS Mixed CLI against Zig 0.14.1's public Darwin libSystem interface."""
import argparse
import hashlib
import os
from pathlib import Path
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
ARCHS = {"x86_64": (0x01000007, 3), "arm64": (0x0100000C, 0)}


def link(args):
    # Rust invokes a C compiler driver. Translate its arguments for LLVM's Mach-O
    # linker. libc and libm are part of libSystem; this program has no iconv imports.
    # -undefined error deliberately makes any dependency drift a link-time failure.
    command = [os.environ["CS2W_LLD"], "-flavor", "darwin", "-platform_version", "macos",
               "12.0", "15.1", "-undefined", "error", "-x", "-S", "-adhoc_codesign"]
    for arg in args:
        if arg in {"-liconv", "-lSystem", "-lc", "-lm", "-nodefaultlibs"} or arg.startswith("-mmacosx-version-min="):
            continue
        command.extend(arg[4:].split(",") if arg.startswith("-Wl,") else [arg])
    command.append(os.environ["CS2W_LIBSYSTEM"])
    # Give both ad-hoc code directories the stable output name, independent of Cargo's hash.
    output = command.index("-o") + 1
    cargo_output = Path(command[output])
    with tempfile.TemporaryDirectory(dir=cargo_output.parent) as tmp:
        staged = Path(tmp) / "cs2world"
        command[output] = staged.as_posix()
        subprocess.run(command, check=True)
        os.replace(staged, cargo_output)


def check_slice(data, cpu):
    magic, actual, _, filetype, ncmds, _, flags, _ = struct.unpack_from("<8I", data)
    if (magic, actual, filetype, flags & 1) != (0xfeedfacf, cpu, 2, 1):
        raise ValueError("invalid executable CPU, type or MH_NOUNDEFS")
    pos = 32
    libraries = []
    main = signature = platform = None
    for _ in range(ncmds):
        command, size = struct.unpack_from("<II", data, pos)
        if command == 0x0c:  # LC_LOAD_DYLIB
            name = pos + struct.unpack_from("<I", data, pos + 8)[0]
            libraries.append(data[name:data.index(0, name)])
        elif command in {0x80000018, 0x8000001f, 0x80000023}:  # weak, reexport, upward
            raise ValueError("unexpected dynamic dependency")
        elif command == 0x80000028:  # LC_MAIN
            main = struct.unpack_from("<Q", data, pos + 8)[0]
        elif command == 0x32:  # LC_BUILD_VERSION
            platform = struct.unpack_from("<II", data, pos + 8)
        elif command == 0x1d:  # LC_CODE_SIGNATURE
            signature = struct.unpack_from("<II", data, pos + 8)
        pos += size
    if libraries != [b"/usr/lib/libSystem.B.dylib"] or not main or platform != (1, 12 << 16) or not signature:
        raise ValueError(f"unexpected load commands: {libraries!r}, {platform}, main={main}")
    start, size = signature
    blob = data[start:start+size]
    magic, length, count = struct.unpack_from(">III", blob)
    if magic != 0xfade0cc0 or length > len(blob):
        raise ValueError("invalid code signature superblob")
    slots = dict(struct.unpack_from(">II", blob, 12+i*8) for i in range(count))
    directory = blob[slots[0]:]
    magic, length, version, flags, hashes, ident, _, pages, limit = struct.unpack_from(">9I", directory)
    hash_size, hash_type, _, page_bits = struct.unpack_from("4B", directory, 36)
    if (magic, flags & 2, limit, hash_type, hash_size) != (0xfade0c02, 2, start, 2, 32):
        raise ValueError("expected a SHA-256 ad-hoc CodeDirectory covering the executable")
    identifier = directory[ident:directory.index(0, ident)]
    if identifier != b"cs2world" or hashes + pages * hash_size > length:
        raise ValueError(f"invalid code directory identifier {identifier!r} or hash extent")
    page_size = 1 << page_bits
    if pages != (limit+page_size-1)//page_size:
        raise ValueError("incorrect code slot count")
    for i in range(pages):
        page = data[i*page_size:min(limit, (i+1)*page_size)]
        if hashlib.sha256(page).digest() != directory[hashes+i*hash_size:hashes+(i+1)*hash_size]:
            raise ValueError(f"invalid code signature page {i}")
    return pages


def build():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zig-lib", required=True, type=Path, help="Zig 0.14.1 lib directory")
    parser.add_argument("--llvm-lipo", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=ROOT / "dist/rust/macos/cs2world")
    options = parser.parse_args()
    tbd = (options.zig_lib / "libc/darwin/libSystem.tbd").resolve(strict=True)
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    host = next(line[6:] for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    linker = sysroot / "lib/rustlib" / host / "bin" / ("rust-lld.exe" if os.name == "nt" else "rust-lld")
    options.out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="cs2world-macos-cli-") as tmp:
        work = Path(tmp)
        targets = work / "target"
        # Strip local symbols and debug info during linking. A separate Cargo
        # post-link strip on Darwin can replace the linker's CodeDirectory.
        env = dict(os.environ, MACOSX_DEPLOYMENT_TARGET="12.0", CARGO_TARGET_DIR=str(targets),
                   CARGO_PROFILE_RELEASE_STRIP="none",
                   CS2W_LLD=str(linker), CS2W_LIBSYSTEM=str(tbd))
        script = Path(__file__).resolve()
        if os.name == "nt":
            wrapper = work / "darwin-link.cmd"
            wrapper.write_text(f'@echo off\n"{sys.executable}" "{script}" --link %*\nexit /b %ERRORLEVEL%\n', encoding="utf-8")
        else:
            wrapper = work / "darwin-link"
            wrapper.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} {shlex.quote(str(script))} --link "$@"\n', encoding="utf-8")
            wrapper.chmod(0o755)
        triples = ["x86_64-apple-darwin", "aarch64-apple-darwin"]
        command = ["cargo", "build", "--locked", "--offline", "--release", "-p", "cs2world-server"]
        for triple in triples:
            env["CARGO_TARGET_"+triple.upper().replace("-", "_")+"_LINKER"] = str(wrapper)
            command.extend(["--target", triple])
        subprocess.run(command, cwd=ROOT, env=env, check=True)
        paths = [targets / triple / "release/cs2world-server" for triple in triples]
        fat = work / "cs2world"
        subprocess.run([options.llvm_lipo, "-create", *paths, "-output", fat], check=True)
        data = fat.read_bytes()
        if struct.unpack_from(">II", data) != (0xcafebabe, 2):
            raise ValueError("invalid universal header")
        seen = set()
        for i in range(2):
            cpu, sub, offset, size, align = struct.unpack_from(">IIIII", data, 8+i*20)
            # The upper subtype byte contains capability flags (e.g. CPU_SUBTYPE_LIB64).
            arch = next(name for name, fields in ARCHS.items() if fields == (cpu, sub & 0x00ffffff))
            idx = 0 if arch == "x86_64" else 1
            thin = data[offset:offset+size]
            if offset % (1 << align) or thin != paths[idx].read_bytes():
                raise ValueError(f"invalid {arch} slice")
            pages = check_slice(thin, cpu)
            print(f"{arch}: MH_EXECUTE, macOS 12.0, libSystem, {pages} SHA-256 code pages", flush=True)
            seen.add(arch)
        if seen != set(ARCHS):
            raise ValueError("a required architecture is missing")
        shutil.copyfile(fat, options.out)
        options.out.chmod(0o755)
        print(f"{options.out}: {len(data)} bytes, SHA-256 {hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--link"]:
        link(sys.argv[2:])
    else:
        build()
