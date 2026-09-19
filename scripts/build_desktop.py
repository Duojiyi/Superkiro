"""Build a self-contained native Superkiro application on the current OS."""
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
os.chdir(ROOT)
binary = "patch-cli.exe" if sys.platform == "win32" else "patch-cli"
subprocess.run(["cargo", "build", "--locked", "--release", "-p", "patch-engine", "--bin", "patch-cli"], check=True)
args = [sys.executable, "-m", "PyInstaller", "--noconfirm", "--clean", "--windowed", "--name", "Superkiro",
        "--add-data", f"apps/desktop-ui{os.pathsep}apps/desktop-ui",
        "--add-data", f"deploy/server-ca.pem{os.pathsep}deploy",
        "--add-binary", f"target/release/{binary}{os.pathsep}bin"]
if sys.platform == "darwin":
    args += ["--osx-bundle-identifier", "app.superkiro.desktop"]
subprocess.run(args + ["run_desktop.py"], check=True)
