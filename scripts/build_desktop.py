"""Build the Windows native single EXE. Python is build-time only.

Requires Rust/MSVC and Node/npm on the build machine. End users need WebView2,
not Python, Node, patch-cli, or files next to Superkiro.exe.

  python scripts/build_desktop.py --release-version 0.1.1

builds a release: the client knows its version and updates itself to later ones. Without
it the build never updates itself. Publish it as exactly that version.
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def main(release_version=""):
    if release_version and not re.fullmatch(r"(0|[1-9][0-9]{0,8})(\.(0|[1-9][0-9]{0,8})){2}", release_version):
        raise SystemExit("Release version must be MAJOR.MINOR.PATCH, e.g. 0.1.1")
    if sys.platform != "win32":
        raise SystemExit("This portable EXE build targets Windows; run it on Windows with MSVC.")
    npm = shutil.which("npm.cmd") or shutil.which("npm")
    if not npm:
        raise SystemExit("Node.js/npm is required on the build machine.")
    ui = ROOT / "apps" / "desktop-ui"
    subprocess.run([npm, "ci"], cwd=ui, check=True)
    subprocess.run([npm, "run", "build"], cwd=ui, check=True)
    # cargo build embeds frontendDist through generate_context!. Unlike cargo-tauri,
    # cargo itself does not run beforeBuildCommand, so build the UI above.
    target = "x86_64-pc-windows-msvc"
    env = os.environ.copy()
    env["CARGO_BUILD_JOBS"] = "2"
    env["SUPERKIRO_RELEASE_VERSION"] = release_version
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C target-feature=+crt-static").strip()
    subprocess.run(["cargo", "build", "--locked", "--release", "--target", target,
                    "-p", "desktop-host", "--bin", "Superkiro"], cwd=ROOT, env=env, check=True)
    target_dir = Path(env.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target_dir.is_absolute():
        target_dir = ROOT / target_dir
    destination = ROOT / "dist" / "Superkiro.exe"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(target_dir / target / "release" / "Superkiro.exe", destination)
    print(f"Native standalone executable: {destination}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--release-version", default="",
                        help="MAJOR.MINOR.PATCH, the version in tauri.conf.json, e.g. 0.1.1")
    main(parser.parse_args().release_version)
