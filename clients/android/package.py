"""Build the two native Android ABIs and assemble a debug-signed installable APK."""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import zipfile

PROJECT = Path(__file__).resolve().parent
ROOT = PROJECT.parents[1]
ABIS = {"arm64-v8a": "aarch64-linux-android", "x86_64": "x86_64-linux-android"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / "target/packages/android")
    parser.add_argument("--gradle", default=shutil.which("gradle"))
    parser.add_argument("--libs-only", action="store_true", help="Build JNI libraries without invoking Gradle")
    parser.add_argument("--with-tests", action="store_true", help="Include the instrumentation test APK")
    args = parser.parse_args()
    sdk = Path(os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT") or "")
    ndk = os.environ.get("ANDROID_NDK_HOME") or os.environ.get("ANDROID_NDK_ROOT")
    if not ndk:
        candidates = [d for d in (sdk / "ndk").glob("*") if d.is_dir() and d.name[0].isdigit()]
        if not candidates:
            parser.error("Set ANDROID_NDK_HOME or install an NDK under ANDROID_HOME/ndk")
        ndk = max(candidates, key=lambda d: tuple(int(x) for x in d.name.split(".") if x.isdigit()))
    host = {"Windows": "windows-x86_64", "Darwin": "darwin-x86_64", "Linux": "linux-x86_64"}[platform.system()]
    tools = Path(ndk) / "toolchains/llvm/prebuilt" / host / "bin"
    targets = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    libs = PROJECT / "app/src/main/jniLibs"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    for abi, target in ABIS.items():
        compiler = tools / (target + "26-clang" + (".cmd" if os.name == "nt" else ""))
        ar = tools / ("llvm-ar.exe" if os.name == "nt" else "llvm-ar")
        if not compiler.exists():
            parser.error(f"Missing NDK compiler {compiler}")
        env = dict(os.environ, CARGO_TARGET_DIR=str(targets))
        prefix = target.replace("-", "_")
        env["CARGO_TARGET_" + prefix.upper() + "_LINKER"] = str(compiler)
        env["CC_" + prefix] = str(compiler)
        env["AR_" + prefix] = str(ar)
        subprocess.run(["cargo", "build", "--locked", "--release", "--target", target, "-p", "cs2world-ffi"],
                       cwd=ROOT, env=env, check=True)
        (libs / abi).mkdir(parents=True, exist_ok=True)
        shutil.copy2(targets / target / "release/libcs2world_ffi.so", libs / abi / "libcs2world_ffi.so")

    artifacts = [output / "CS2World-android-libs.zip"]
    with zipfile.ZipFile(artifacts[0], "w", zipfile.ZIP_DEFLATED) as archive:
        for abi in ABIS:
            lib = libs / abi / "libcs2world_ffi.so"
            archive.write(lib, f"jniLibs/{abi}/{lib.name}")
        archive.write(ROOT / "crates/cs2world-ffi/include/cs2world.h", "include/cs2world.h")
    if not args.libs_only:
        if not args.gradle:
            parser.error("Gradle 8.11.1 is required on PATH or via --gradle")
        tasks = [":app:assembleDebug"]
        if args.with_tests:
            tasks.append(":app:assembleDebugAndroidTest")
        subprocess.run([args.gradle, "--no-daemon", *tasks], cwd=PROJECT, check=True)
        apk = output / "CS2World-debug.apk"
        shutil.copy2(PROJECT / "app/build/outputs/apk/debug/app-debug.apk", apk)
        artifacts.append(apk)
        if args.with_tests:
            test_apk = output / "CS2World-androidTest.apk"
            shutil.copy2(PROJECT / "app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk", test_apk)
            artifacts.append(test_apk)
    for artifact in artifacts:
        with artifact.open("rb") as file:
            print(hashlib.file_digest(file, "sha256").hexdigest(), artifact)


if __name__ == "__main__":
    main()
