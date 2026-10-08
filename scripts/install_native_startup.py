#!/usr/bin/env python3
"""Package/install the native DSH addon without requiring Cargo or frontend tools.

python scripts/install_native_startup.py             # npm global installation
python scripts/install_native_startup.py --pack-only # portable npm tarball
python scripts/install_native_startup.py --prefix PATH # isolated installation
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def stage_package(destination: Path) -> None:
    source = ROOT / "integrations" / "native-startup"
    destination.mkdir(parents=True, exist_ok=True)
    for path in source.iterdir():
        if path.is_file() and path.name != "test.mjs":
            shutil.copy2(path, destination / path.name)
    assets = destination / "assets"
    assets.mkdir()
    for path in (ROOT / "docs").glob("startup-*"):
        if path.suffix in (".html", ".js"):
            shutil.copy2(path, assets / path.name)
    for name in ("voice", "fonts"):
        shutil.copytree(ROOT / "docs" / "assets" / name, assets / "assets" / name)
    shutil.copy2(ROOT / "docs" / "assets" / "dsh-emblem.svg", assets / "assets" / "dsh-emblem.svg")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pack-only", action="store_true")
    parser.add_argument("--prefix", type=Path, help="npm global installation prefix (does not modify PATH)")
    args = parser.parse_args()
    npm = shutil.which("npm.cmd") or shutil.which("npm")
    if not npm:
        parser.error("Node.js 22.19+ or 24+ and npm are required.")
    if not args.pack_only:
        node = shutil.which("node")
        if not node:
            parser.error("Node.js 22.19+ or 24+ is required.")
        version = subprocess.run([node, "--version"], check=True, capture_output=True, text=True, encoding="utf-8").stdout.strip()
        major, minor = [int(part) for part in version.lstrip("v").split(".")[:2]]
        if not ((major == 22 and minor >= 19) or major >= 24):
            parser.error(f"Official DSH needs Node.js 22.19+ (22.x) or 24+, but PATH selects {version}. Update Node before installing; --pack-only can still create the portable tarball.")
    output = ROOT / "target" / "native-startup-package"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="dsh-native-package-") as temp:
        staged = Path(temp) / "package"
        stage_package(staged)
        result = subprocess.run([npm, "pack", "--json", "--pack-destination", str(output)], cwd=staged, check=True, capture_output=True, text=True, encoding="utf-8")
        package = output / json.loads(result.stdout)[0]["filename"]
    print(f"Packaged native startup addon: {package}", flush=True)
    if args.pack_only:
        return
    command = [npm, "install", "--global", str(package)]
    if args.prefix:
        command += ["--prefix", str(args.prefix.expanduser().resolve())]
    subprocess.run(command, check=True)
    print("Installed. Run: dsh-native web --startup")
    if args.prefix:
        print(f"Add the npm executable directory under {args.prefix.expanduser().resolve()} to PATH, or use its absolute dsh-native launcher path.")


if __name__ == "__main__":
    main()
