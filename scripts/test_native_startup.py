#!/usr/bin/env python3
"""Boot the installed, real official DSH in an isolated home and test the addon.

python scripts/test_native_startup.py --package <installed addon directory> --node <node24>
Use --serve to keep the authenticated fixture available for browser inspection.
"""
from __future__ import annotations
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--node", default="node")
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--report", type=Path, default=Path(__file__).resolve().parents[1] / "target" / "native-startup-qa" / "fixture.json")
    args = parser.parse_args()
    package = args.package.resolve()
    native_cli = package / "cli.mjs"
    if not native_cli.is_file():
        parser.error("--package must contain the installed cli.mjs and node_modules")
    with tempfile.TemporaryDirectory(prefix="dsh-native-live-test-") as temp:
        root = Path(temp)
        home, workspace = root / "home", root / "workspace"
        home.mkdir(); workspace.mkdir(); (workspace / ".git").mkdir()
        skill = workspace / ".dsh" / "skills" / "native-fixture"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("---\nname: native-fixture\ndescription: A real isolated native startup test skill.\n---\nUse only in the integration test.\n", encoding="utf-8")
        env = {**os.environ, "DSH_HOME": str(home), "DSH_AGENTS_HOME": str(root / "agents"), "DSH_TELEMETRY_DISABLED": "1"}
        # The fixture must not inherit model credentials; no model request is needed.
        for key in list(env):
            if key.endswith("API_KEY") or key == "DSH_BUNDLED_SKILL_DIR":
                del env[key]
        command = [args.node, str(native_cli)]
        subprocess.run([*command, "startup", "next", "on"], env=env, cwd=workspace, check=True, capture_output=True)
        subprocess.run([*command, "web", "--help"], env=env, cwd=workspace, check=True, capture_output=True)
        assert (home / "startup-animation" / "next.json").is_file(), "Help consumed the one-shot choice"
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0)); port = listener.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        prefix = origin + "/__dsh_startup"
        log_path = root / "native.log"
        process = None
        with log_path.open("w", encoding="utf-8") as log:
            process = subprocess.Popen([*command, "web", "--no-open", "--port", str(port)], env=env, cwd=workspace, stdout=log, stderr=subprocess.STDOUT, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            try:
                url = None
                for _ in range(180):
                    output = log_path.read_text(encoding="utf-8", errors="replace")
                    match = re.search(re.escape(origin) + r"/\?token=[A-Za-z0-9_-]+", output)
                    if match: url = match.group(0); break
                    if process.poll() is not None: raise RuntimeError("Official DSH failed to boot:\n" + output[-6000:])
                    time.sleep(0.25)
                if not url: raise RuntimeError("Official DSH did not publish its login URL:\n" + output[-3000:])
                def unauth_status(path, headers=None):
                    try:
                        with urllib.request.urlopen(urllib.request.Request(prefix + path, headers=headers or {})) as response: return response.status
                    except urllib.error.HTTPError as error: return error.code
                assert unauth_status("/api/profile") == 401
                assert unauth_status("/launch", {"X-DSH-Startup": "launch"}) == 401
                assert (home / "startup-animation" / "next.json").is_file(), "Unauthenticated probe consumed the one-shot choice"
                cookies = http.cookiejar.CookieJar()
                client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))
                with client.open(url) as response:
                    page = response.read().decode("utf-8")
                assert "/__dsh_startup/overlay.js" in page, "Official HTML did not include the supported injection"
                def request(path, payload=None, token=None, launch=False):
                    headers = {"Origin": origin}
                    if token: headers["X-DSH-Token"] = token
                    if launch: headers["X-DSH-Startup"] = "launch"
                    if payload is not None: headers["Content-Type"] = "application/json"
                    data = json.dumps(payload).encode("utf-8") if payload is not None else None
                    with client.open(urllib.request.Request(prefix + path, data=data, headers=headers)) as response: return response.read()
                profile = json.loads(request("/api/profile"))
                assert profile["runtime"] == "native" and profile["inventory_mode"] == "mounted"
                assert profile["workspace"] == str(workspace)
                try:
                    request("/api/profile", {"username": "must-not-save", "badge_id": "BAD"}, "wrong-token")
                    raise AssertionError("A wrong mutation token was accepted")
                except urllib.error.HTTPError as error:
                    assert error.code == 403
                try:
                    client.open(urllib.request.Request(prefix + "/api/profile", headers={"Origin": "https://other.example"}))
                    raise AssertionError("A cross-origin request was accepted")
                except urllib.error.HTTPError as error:
                    assert error.code == 403
                launch = json.loads(request("/launch", launch=True))
                assert launch["enabled"] is True
                assert not (home / "startup-animation" / "next.json").exists()
                profile = json.loads(request("/api/profile", {"username": "NativeFixture", "badge_id": "DSH-QA-27"}, profile["token"]))
                assert profile["username"] == "NativeFixture"
                assert json.loads(request("/api/profile"))["badge_id"] == "DSH-QA-27"
                events = [json.loads(line) for line in request("/api/load", {}, profile["token"]).decode("utf-8").splitlines()]
                inventory = events[-1]
                assert inventory["type"] == "complete"
                assert any(item["name"] == "native-fixture" for item in inventory["skills"]), inventory["skills"]
                assert any("plugin.mjs" in item["name"] for item in inventory["plugins"]), "The addon was not mounted in the real official Loader"
                assert len(inventory["plugins"]) > 5
                html = request("/startup-preview.html").decode("utf-8")
                assert "/__dsh_startup/bridge.js" in html
                assert len(request("/assets/voice/phase-3-mounted.wav")) > 1000
                assert len(request("/assets/fonts/dsh-industrial-sc.woff2")) > 1000
                print(json.dumps({"passed": True, "official_version": "0.2.0-rc.2", "skills": len(inventory["skills"]), "mounted_plugins": len(inventory["plugins"]), "issues": len(inventory["issues"]), "fixture_workspace": str(workspace)}, ensure_ascii=False), flush=True)
                if args.serve:
                    args.report.parent.mkdir(parents=True, exist_ok=True)
                    args.report.write_text(json.dumps({"url": url, "workspace": str(workspace), "pid": process.pid}), encoding="utf-8")
                    print("Native browser fixture login URL saved to: " + str(args.report), flush=True)
                    print("Press Ctrl+C to close the isolated fixture.", flush=True)
                    while process.poll() is None: time.sleep(0.5)
            finally:
                if process is not None and process.poll() is None:
                    process.terminate()
                    try: process.wait(timeout=10)
                    except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)


if __name__ == "__main__":
    main()
