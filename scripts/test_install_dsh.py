"""Isolated installer checks; never invoke Cargo/npm or change the real PATH."""

import contextlib
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import install_dsh as installer


class FakeRegistry:
    HKEY_CURRENT_USER = "user"
    HKEY_LOCAL_MACHINE = "machine"
    KEY_READ = 1
    KEY_WRITE = 2
    REG_SZ = 1
    REG_EXPAND_SZ = 2

    def __init__(self, user_path, machine_path="", value_type=REG_EXPAND_SZ):
        self.user_path = user_path
        self.machine_path = machine_path
        self.value_type = value_type
        self.writes = []

    def CreateKeyEx(self, *_args):
        return contextlib.nullcontext("user")

    def OpenKey(self, *_args):
        return contextlib.nullcontext("machine")

    def QueryValueEx(self, key, _name):
        if key == "machine":
            return self.machine_path, self.REG_EXPAND_SZ
        return self.user_path, self.value_type

    def SetValueEx(self, key, name, reserved, value_type, value):
        self.writes.append((key, name, reserved, value_type, value))


class InstallRootTests(unittest.TestCase):
    def test_explicit_root_then_cargo_home_then_user_home(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            self.assertEqual(installer.install_root(str(base / "explicit"), {"CARGO_HOME": str(base / "env")}), base / "explicit")
            self.assertEqual(installer.install_root(None, {"CARGO_HOME": str(base / "env")}), base / "env")
            self.assertEqual(installer.install_root(None, {}, base), base / ".cargo")

    def test_build_steps_use_repository_paths_and_locked_real_binary_install(self):
        repo = Path(installer.__file__).resolve().parents[1]
        root = Path(tempfile.gettempdir()) / "isolated dsh test"
        steps = installer.build_steps(repo, root, "npm.cmd", "cargo.exe", True)
        self.assertEqual(steps[0][1:], (["npm.cmd", "ci"], repo / "web"))
        self.assertEqual(steps[1][1:], (["npm.cmd", "run", "build"], repo / "web"))
        self.assertEqual(steps[2][1], ["cargo.exe", "install", "--path", str(repo / "crates" / "dsh-cli"), "--locked", "--root", str(root), "--force", "--debug"])
        self.assertNotIn("--debug", installer.build_steps(repo, root, "npm", "cargo")[2][1])

    def test_main_uses_subprocess_cwd_without_changing_callers_directory(self):
        original_cwd = Path.cwd()
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            repo = base / "checkout"
            dist = repo / "web" / "dist"
            dist.mkdir(parents=True)
            (dist / "index.html").write_text("built interface", encoding="utf-8")
            root = base / "install"
            with mock.patch.object(installer, "REPOSITORY", repo), \
                 mock.patch.object(installer, "find_program", side_effect=lambda name: name), \
                 mock.patch.object(installer.subprocess, "run") as run, \
                 mock.patch.object(installer, "ensure_windows_path", return_value=False), \
                 contextlib.redirect_stdout(io.StringIO()):
                result = installer.main(["--root", str(root), "--debug"])
            self.assertEqual(result, 0)
            self.assertEqual(Path.cwd(), original_cwd)
            self.assertEqual([call.kwargs["cwd"] for call in run.call_args_list], [repo / "web", repo / "web", repo])
            self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))
            self.assertEqual((root / "share" / "dsh" / "web" / "index.html").read_text(encoding="utf-8"), "built interface")


class AssetPublicationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.source = self.base / "dist"
        self.source.mkdir()
        (self.source / "index.html").write_text("new", encoding="utf-8")
        self.root = self.base / "install"
        self.target = self.root / "share" / "dsh" / "web"

    def test_replacement_removes_stale_assets_and_preserves_adjacent_data(self):
        self.target.mkdir(parents=True)
        (self.target / "obsolete.js").write_text("old", encoding="utf-8")
        adjacent = self.target.parent / "keep.txt"
        adjacent.write_text("keep", encoding="utf-8")
        self.assertEqual(installer.publish_web_assets(self.source, self.root), self.target)
        self.assertFalse((self.target / "obsolete.js").exists())
        self.assertEqual((self.target / "index.html").read_text(encoding="utf-8"), "new")
        self.assertEqual(adjacent.read_text(encoding="utf-8"), "keep")
        self.assertEqual(sorted(path.name for path in self.target.parent.iterdir()), ["keep.txt", "web"])

    def test_failed_swap_restores_previous_assets(self):
        self.target.mkdir(parents=True)
        (self.target / "index.html").write_text("previous", encoding="utf-8")
        original_rename = Path.rename
        def fail_stage(path, destination):
            if path.name.startswith(installer.STAGE_PREFIX):
                raise OSError("simulated file lock")
            return original_rename(path, destination)
        with mock.patch.object(Path, "rename", fail_stage), self.assertRaises(OSError):
            installer.publish_web_assets(self.source, self.root)
        self.assertEqual((self.target / "index.html").read_text(encoding="utf-8"), "previous")
        self.assertEqual([path.name for path in self.target.parent.iterdir()], ["web"])

    def test_recursive_cleanup_rejects_paths_outside_exact_owned_parent(self):
        outside = self.base / (installer.STAGE_PREFIX + "outside")
        outside.mkdir()
        with self.assertRaises(RuntimeError):
            installer.remove_owned_tree(outside, self.root / "share" / "dsh", installer.STAGE_PREFIX)
        self.assertTrue(outside.is_dir())
        with self.assertRaises(RuntimeError):
            installer.remove_owned_tree(self.base, self.base.parent, installer.STAGE_PREFIX)

    def test_bad_source_or_non_directory_target_does_not_replace_old_data(self):
        (self.source / "index.html").unlink()
        with self.assertRaises(RuntimeError):
            installer.publish_web_assets(self.source, self.root)
        self.target.parent.mkdir(parents=True)
        self.target.write_text("not an assets directory", encoding="utf-8")
        with self.assertRaises(RuntimeError):
            installer.validate_destination(self.root)
        self.assertEqual(self.target.read_text(encoding="utf-8"), "not an assets directory")


class WindowsPathTests(unittest.TestCase):
    def test_preserves_original_path_and_type_while_appending_only_missing_entry(self):
        original = r'%USERPROFILE%\Tools;"C:\Some Tools";;'
        registry = FakeRegistry(original, value_type=FakeRegistry.REG_SZ)
        environment = {"PATH": r"C:\Windows;"}
        directory = Path(r"C:\DSH Install\bin")
        with mock.patch.object(installer, "notify_windows_environment") as notify:
            changed = installer.ensure_windows_path(directory, registry=registry, environment=environment)
        self.assertTrue(changed)
        self.assertEqual(registry.writes, [("user", "Path", 0, FakeRegistry.REG_SZ, original + str(directory))])
        self.assertEqual(environment["PATH"], r"C:\Windows;" + str(directory))
        notify.assert_called_once()

    def test_existing_user_entry_is_not_rewritten_despite_case_or_variable_spelling(self):
        original = r'"%USERPROFILE%\.cargo\BIN";C:\Other;;'
        registry = FakeRegistry(original)
        process_path = r"C:\Tools;C:\Users\Example\.cargo\bin;"
        environment = {"PATH": process_path}
        with mock.patch.dict(os.environ, {"USERPROFILE": r"C:\Users\Example"}), \
             mock.patch.object(installer, "notify_windows_environment") as notify:
            changed = installer.ensure_windows_path(Path(r"C:\Users\Example\.cargo\bin"), registry=registry, environment=environment)
        self.assertFalse(changed)
        self.assertEqual(registry.writes, [])
        self.assertEqual(environment["PATH"], process_path)
        notify.assert_not_called()

    def test_existing_machine_entry_does_not_redundantly_change_user_path(self):
        registry = FakeRegistry(r"C:\User Tools", r"C:\DSH\bin;C:\Windows")
        environment = {"PATH": r"C:\Windows"}
        changed = installer.ensure_windows_path(Path(r"C:\DSH\bin"), registry=registry, environment=environment)
        self.assertFalse(changed)
        self.assertEqual(registry.writes, [])
        self.assertEqual(environment["PATH"], r"C:\Windows;C:\DSH\bin")


if __name__ == "__main__":
    unittest.main()
