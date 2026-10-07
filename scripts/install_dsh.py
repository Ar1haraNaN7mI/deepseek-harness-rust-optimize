#!/usr/bin/env python3
"""Build and install DSH plus its web interface, independent of the caller's cwd.

Requirements: Python 3.9+, Node.js 22.12+, npm, and the Rust/Cargo toolchain.

Usage (the script path may be absolute and the current directory may be anything):
    python scripts/install_dsh.py
    python scripts/install_dsh.py --root /path/to/isolated/install --debug

The default install root is CARGO_HOME, or ~/.cargo when it is unset. The real
binary is installed into <root>/bin, and web assets into <root>/share/dsh/web.
Release is the default; --debug selects Cargo's faster development build.
No launcher or shim points back to this checkout. Installed `dsh` therefore uses
the directory from which the user runs it as the workspace.

On Windows, a missing bin directory is appended to the user's persisted PATH
without rewriting any existing entries. Other platforms receive shell setup
instructions. This installer never changes the invoking shell's directory.

Tests: python -m unittest discover -s scripts -p test_install_dsh.py
"""

from __future__ import annotations

import argparse
import ctypes
import ntpath
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
from typing import Mapping, MutableMapping, Optional, Sequence
import uuid


REPOSITORY = Path(__file__).resolve().parents[1]
STAGE_PREFIX = ".dsh-web-stage-"
BACKUP_PREFIX = ".dsh-web-backup-"


def install_root(
    explicit: Optional[str],
    environment: Optional[Mapping[str, str]] = None,
    home: Optional[Path] = None,
) -> Path:
    environment = os.environ if environment is None else environment
    chosen = explicit or environment.get("CARGO_HOME") or str((home or Path.home()) / ".cargo")
    return Path(chosen).expanduser().resolve()


def find_program(name: str) -> str:
    # npm's Windows entry point is a batch file, not a CreateProcess executable.
    candidates = ["npm.cmd", "npm"] if name == "npm" and os.name == "nt" else [name]
    for candidate in candidates:
        executable = shutil.which(candidate)
        if executable:
            return executable
    raise RuntimeError(f"Required program not found on PATH: {name}")


def build_steps(repo: Path, root: Path, npm: str, cargo: str, debug: bool = False):
    cargo_command = [
        cargo, "install", "--path", str(repo / "crates" / "dsh-cli"),
        "--locked", "--root", str(root), "--force",
    ]
    if debug:
        cargo_command.append("--debug")
    return [
        ("Install locked web dependencies", [npm, "ci"], repo / "web"),
        ("Build web interface", [npm, "run", "build"], repo / "web"),
        ("Install DSH binary", cargo_command, repo),
    ]


def validate_destination(root: Path) -> Path:
    """Resolve the exact owned asset directory before creating or replacing it."""
    root = root.resolve()
    parent = root / "share" / "dsh"
    for part in (root / "share", parent, parent / "web"):
        if part.is_symlink():
            raise RuntimeError(f"Refusing to replace an install path containing a symlink: {part}")
        resolved = part.resolve()
        try:
            relative = resolved.relative_to(root)
        except ValueError as error:
            raise RuntimeError(f"Install asset path escapes its root: {resolved}") from error
        if not relative.parts:
            raise RuntimeError(f"Invalid install asset path: {resolved}")
    target = parent / "web"
    if target.exists() and not target.is_dir():
        raise RuntimeError(f"The web install target is not a directory: {target}")
    return target


def remove_owned_tree(path: Path, parent: Path, prefix: str) -> None:
    """Only remove a generated, direct child of the checked asset parent."""
    if path.is_symlink() or not path.name.startswith(prefix):
        raise RuntimeError(f"Refusing to remove unexpected install staging path: {path}")
    expected_parent = parent.resolve()
    resolved = path.resolve()
    if resolved.parent != expected_parent or resolved == expected_parent:
        raise RuntimeError(f"Refusing to remove a path outside the install asset parent: {resolved}")
    if path.exists():
        shutil.rmtree(path)


def publish_web_assets(source: Path, root: Path) -> Path:
    """Stage first, swap the complete directory, and restore on a failed swap."""
    if not source.is_dir() or not (source / "index.html").is_file():
        raise RuntimeError(f"Built web interface is missing index.html: {source}")
    target = validate_destination(root)
    parent = target.parent
    parent.mkdir(parents=True, exist_ok=True)
    # Recheck after creation before any directory move or recursive removal.
    target = validate_destination(root)
    stage = Path(tempfile.mkdtemp(prefix=STAGE_PREFIX, dir=parent))
    backup = parent / (BACKUP_PREFIX + uuid.uuid4().hex)
    moved_old = False
    try:
        shutil.copytree(source, stage, dirs_exist_ok=True)
        if not (stage / "index.html").is_file():
            raise RuntimeError("The staged web interface is incomplete")
        validate_destination(root)
        if target.exists():
            target.rename(backup)
            moved_old = True
        try:
            stage.rename(target)
        except OSError:
            if moved_old:
                backup.rename(target)
                moved_old = False
            raise
    finally:
        remove_owned_tree(stage, parent, STAGE_PREFIX)
    if moved_old:
        try:
            remove_owned_tree(backup, parent, BACKUP_PREFIX)
        except OSError as error:
            # Publication succeeded. An old file held open by Windows should
            # not misreport the new installation as failed.
            print(f"Note: old web assets remain at {backup}: {error}", file=sys.stderr)
    return target


def windows_path_contains(value: str, directory: str) -> bool:
    def normalize(part: str) -> str:
        return ntpath.normcase(ntpath.normpath(ntpath.expandvars(part.strip().strip('"'))))
    expected = normalize(directory)
    return any(part.strip() and normalize(part) == expected for part in value.split(";"))


def append_windows_path(value: str, directory: str) -> str:
    if windows_path_contains(value, directory):
        return value
    # Preserve the original value byte-for-byte, including ordering, case,
    # variable references, quoting, and existing trailing separators.
    return value + ("" if not value or value.endswith(";") else ";") + directory


def notify_windows_environment() -> None:
    try:
        from ctypes import wintypes
        result = ctypes.c_size_t()
        send = ctypes.windll.user32.SendMessageTimeoutW
        send.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPCWSTR,
                         wintypes.UINT, wintypes.UINT, ctypes.POINTER(ctypes.c_size_t)]
        send.restype = wintypes.LPARAM
        send(0xFFFF, 0x001A, 0, "Environment", 0x0002, 3000, ctypes.byref(result))
    except (AttributeError, OSError):
        pass  # New processes will still read the saved registry value.


def ensure_windows_path(
    bin_dir: Path,
    *,
    registry=None,
    environment: Optional[MutableMapping[str, str]] = None,
) -> bool:
    if registry is None:
        import winreg as registry
    environment = os.environ if environment is None else environment
    directory = str(bin_dir)
    if ";" in directory:
        raise RuntimeError("A Windows install root cannot contain ';' because it cannot form one PATH entry")
    with registry.CreateKeyEx(registry.HKEY_CURRENT_USER, "Environment", 0,
                              registry.KEY_READ | registry.KEY_WRITE) as key:
        try:
            user_path, value_type = registry.QueryValueEx(key, "Path")
        except FileNotFoundError:
            user_path, value_type = "", registry.REG_EXPAND_SZ
        if not isinstance(user_path, str) or value_type not in (registry.REG_SZ, registry.REG_EXPAND_SZ):
            raise RuntimeError("The user PATH registry value has an unsupported type; it was left unchanged")
        machine_path = ""
        try:
            with registry.OpenKey(registry.HKEY_LOCAL_MACHINE,
                                  r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment") as machine:
                candidate, _ = registry.QueryValueEx(machine, "Path")
                if isinstance(candidate, str):
                    machine_path = candidate
        except OSError:
            pass
        persisted = windows_path_contains(user_path, directory) or windows_path_contains(machine_path, directory)
        if not persisted:
            registry.SetValueEx(key, "Path", 0, value_type, append_windows_path(user_path, directory))
    environment["PATH"] = append_windows_path(environment.get("PATH", ""), directory)
    if not persisted:
        notify_windows_environment()
    return not persisted


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", help="Install root; defaults to CARGO_HOME or ~/.cargo")
    parser.add_argument("--debug", action="store_true", help="Install a development build instead of the default release build")
    arguments = parser.parse_args(argv)
    try:
        root = install_root(arguments.root)
        validate_destination(root)
        npm, cargo = find_program("npm"), find_program("cargo")
        for label, command, cwd in build_steps(REPOSITORY, root, npm, cargo, arguments.debug):
            print(f"\n{label}\n  {subprocess.list2cmdline(command) if os.name == 'nt' else shlex.join(command)}", flush=True)
            subprocess.run(command, cwd=cwd, check=True)
        assets = publish_web_assets(REPOSITORY / "web" / "dist", root)
        bin_dir = root / "bin"
        print(f"\nInstalled binary: {bin_dir / ('dsh.exe' if os.name == 'nt' else 'dsh')}")
        print(f"Installed web:    {assets}")
        if os.name == "nt":
            if ensure_windows_path(bin_dir):
                print("Added the install bin directory to your user PATH. Open a new terminal to use it.")
            else:
                print("The install bin directory is already on your persisted PATH; it was not changed.")
        else:
            current_entries = os.environ.get("PATH", "").split(os.pathsep)
            if str(bin_dir) not in current_entries:
                print("Add this line to your shell profile, then open a new terminal:")
                print(f'  export PATH={shlex.quote(str(bin_dir))}:"$PATH"')
        print("Next: run `dsh --help`, or run `dsh web` from the workspace you want to use.")
        return 0
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Installation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
