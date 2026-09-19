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
        "--add-binary", f"target/release/{binary}{os.pathsep}bin"]
# Bundle only runtime assets, not design references, backups, or launch logs.
for name in ("index.html", "desktop.css", "desktop.js"):
    resource = ROOT / "apps" / "desktop-ui" / name
    if not resource.is_file():
        raise FileNotFoundError(f"Missing desktop resource: {resource}")
    args += ["--add-data", f"{resource}{os.pathsep}apps/desktop-ui"]
if sys.platform == "win32":
    args += ["--hidden-import", "keyring.backends.Windows"]
if sys.platform == "darwin":
    args += ["--osx-bundle-identifier", "app.superkiro.desktop", "--hidden-import", "keyring.backends.macOS"]
subprocess.run(args + ["run_desktop.py"], check=True)
