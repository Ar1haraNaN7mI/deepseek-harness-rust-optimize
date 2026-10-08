#!/usr/bin/env python3
"""Exercise real native WebView2 + React readiness and backend ownership.

Windows only. Opens two short-lived native windows using isolated DSH state;
never creates a chat/task or sends a model request. No browser automation API.
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def request(base, path, payload=None, token=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["X-DSH-Token"] = token
    req = urllib.request.Request(base + path, data=None if payload is None else json.dumps(payload).encode(), headers=headers)
    with urllib.request.urlopen(req, timeout=3) as response:
        return json.load(response)


def smoke(binary, workspace, *arguments):
    # WebView2 subprocesses may inherit pipes; wait for the DSH process itself,
    # not EOF from every browser subprocess associated with its persistent profile.
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as log:
        process = subprocess.Popen([str(binary), "app", "--workspace", str(workspace),
                                    "--no-startup", "--smoke-test", *arguments],
                                   cwd=ROOT, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            process.wait(timeout=90)
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
        log.seek(0)
        output = log.read()
    if process.returncode:
        raise RuntimeError(output or "Native desktop smoke failed")
    record = next(json.loads(line) for line in output.splitlines() if line.startswith('{"closed"'))
    assert record["page_loaded"] and record["frontend_ready"] and record["closed"], record
    return output, record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/dsh.exe")
    args = parser.parse_args()
    if os.name != "nt":
        print("Native desktop verification requires Windows / WebView2.")
        return
    binary = args.binary.resolve()
    assets = ROOT / "web/dist"
    with tempfile.TemporaryDirectory(prefix="dsh-desktop-qa-", ignore_cleanup_errors=True) as temp:
        base = Path(temp)
        workspace = base / "workspace"
        (workspace / ".dsh-rust").mkdir(parents=True)
        config = (ROOT / "config/default.toml").read_text(encoding="utf-8")
        config = re.sub(r"^outer_home\s*=.*$", "outer_home = " + json.dumps((base / "home").as_posix()), config, flags=re.M)
        (workspace / ".dsh-rust/config.toml").write_text(config, encoding="utf-8")
        # The second launch must focus the primary window, not borrow its server
        # in another window that breaks when the primary exits.
        primary_log = base / "primary.log"
        with primary_log.open("w", encoding="utf-8") as log:
            primary = subprocess.Popen([str(binary), "app", "--workspace", str(workspace),
                                        "--assets", str(assets), "--no-startup", "--smoke-test"],
                                       cwd=ROOT, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
            try:
                deadline = time.monotonic() + 15
                while "DSH Harness:" not in primary_log.read_text(encoding="utf-8"):
                    if time.monotonic() > deadline or primary.poll() is not None:
                        raise RuntimeError("Primary desktop service did not start")
                    time.sleep(.05)
                secondary = subprocess.run([str(binary), "app", "--workspace", str(workspace), "--no-startup"],
                                           cwd=ROOT, capture_output=True, encoding="utf-8", timeout=20)
                assert secondary.returncode == 0 and "已打开该工作区" in secondary.stdout
                assert primary.poll() is None, "Second launch terminated the primary"
                primary.wait(timeout=120)
                assert primary.returncode == 0
            finally:
                if primary.poll() is None:
                    primary.terminate()
                    primary.wait(timeout=5)
        output = primary_log.read_text(encoding="utf-8")
        owned = next(json.loads(line) for line in output.splitlines() if line.startswith('{"closed"'))
        assert owned["owned_service"]
        assert owned["frontend_ready"] and owned["page_loaded"]
        owned_url = re.search(r"DSH Harness: (http://127\.0\.0\.1:\d+)", output).group(1)
        try:
            request(owned_url, "/api/harness/bootstrap")
        except OSError:
            pass
        else:
            raise AssertionError("Owned service remained alive after desktop close")
        reopened, again = smoke(binary, workspace, "--assets", str(assets))
        reopened_url = re.search(r"DSH Harness: (http://127\.0\.0\.1:\d+)", reopened).group(1)
        assert reopened_url == owned_url and again["owned_service"], "Desktop origin changed on reopen"

        log_path = base / "service.log"
        with log_path.open("w", encoding="utf-8") as log:
            process = subprocess.Popen([str(binary), "web", "--workspace", str(workspace), "--port", "0",
                                        "--assets", str(assets), "--no-open", "--no-startup"],
                                       cwd=ROOT, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
            try:
                deadline = time.monotonic() + 15
                match = None
                while time.monotonic() < deadline:
                    match = re.search(r"DSH Harness: (http://127\.0\.0\.1:(\d+))", log_path.read_text(encoding="utf-8"))
                    if match:
                        break
                    time.sleep(.1)
                if not match:
                    raise RuntimeError("Isolated web service did not start")
                url, port = match.groups()
                before = request(url, "/api/harness/bootstrap")
                _, shared = smoke(binary, workspace, "--port", port)
                assert not shared["owned_service"]
                after = request(url, "/api/harness/bootstrap")
                assert before["token"] == after["token"] and process.poll() is None
                assert before["sessions"] == after["sessions"]
                identity = request(url, "/api/harness/service", token=after["token"])
                # Password protection must render a usable native entry without
                # attempting private bootstrap or requiring a terminal prompt.
                request(url, "/api/access/password", {"password": "Desktop-fixture-2026"}, after["token"])
                locked = request(url, "/api/access")
                assert locked["enabled"] and not locked["unlocked"]
                _, protected = smoke(binary, workspace, "--port", port)
                assert protected["frontend_ready"] and not protected["owned_service"]
                assert request(url, "/api/access")["unlocked"] is False
                control_path = Path(os.environ["LOCALAPPDATA"]) / "dsh-rust/web-services" / f"{port}.json"
                control = json.loads(control_path.read_text(encoding="utf-8"))
                assert control["instance_id"] == identity["instance_id"]
                request(url, "/api/harness/shutdown", {"instance_id": identity["instance_id"]}, control["token"])
                process.wait(timeout=5)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
        print(json.dumps({"native_window": "passed", "react_bootstrap": "passed",
                          "owned_service_cleanup": "passed", "shared_service_preserved": "passed",
                          "stable_origin_on_reopen": "passed", "second_launch_focuses_primary": "passed",
                          "password_protected_native_entry": "passed"}))


if __name__ == "__main__":
    main()
