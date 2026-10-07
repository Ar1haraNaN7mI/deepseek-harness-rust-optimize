"""Run real dsh web HTTP/chat tests against an isolated local OpenAI SSE fixture.

Build dsh first. Use --assets web/dist --serve to retain the tested fixture for
browser QA until Ctrl+C. No user sessions or paid model endpoint are touched.
"""

from __future__ import annotations

import argparse
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time


class ModelFixture(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests.append(body)
        prompts = [message.get("content", "") for message in body["messages"] if message.get("role") == "user"]
        answer = "Fixture reply: " + str(prompts[-1])[:400]
        # Multiple real SSE frames let the frontend exercise incremental text.
        chunks = [answer[index:index + 12] for index in range(0, len(answer), 12)]
        frames = ["data: " + json.dumps({"choices": [{"delta": {"content": chunk}, "finish_reason": None}]}) + "\n\n" for chunk in chunks]
        frames += ['data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\n', "data: [DONE]\n\n"]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(sum(len(frame.encode()) for frame in frames)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            for frame in frames:
                self.wfile.write(frame.encode())
                self.wfile.flush()
                time.sleep(0.025)
        except (BrokenPipeError, ConnectionResetError):
            pass


def available_port():
    with socket.socket() as temporary:
        temporary.bind(("127.0.0.1", 0))
        return temporary.getsockname()[1]


def fixture_config(endpoint: str, outer: Path) -> str:
    """Keep required application fields in sync with the checked-in defaults."""
    defaults = Path(__file__).resolve().parents[1] / "config" / "default.toml"
    overrides = {
        ("llm", "base_url"): endpoint,
        ("llm", "model"): "harness-fixture",
        ("llm", "backend"): "deepseek",
        ("llm", "thinking"): False,
        ("paths", "outer_home"): outer.as_posix(),
        ("agent", "scheduler_enabled"): False,
        ("agent", "retry_max_attempts"): 1,
        ("ctm", "enabled"): False,
        ("learn", "enabled"): False,
        ("tui.startup", "enabled"): False,
        ("tui.startup", "sound"): False,
    }
    lines = []
    section = ""
    for line in defaults.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped[1:-1]
        key = stripped.partition("=")[0].strip()
        setting = (section, key)
        if setting in overrides:
            line = f"{key} = {json.dumps(overrides.pop(setting))}"
        lines.append(line)
    if overrides:
        raise ValueError(f"Fixture settings missing from default config: {list(overrides)}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/dsh.exe" if os.name == "nt" else "target/debug/dsh"))
    assets_group = parser.add_mutually_exclusive_group()
    assets_group.add_argument("--assets", type=Path, help="Built frontend for browser QA; default is a minimal static fixture")
    assets_group.add_argument("--installed-assets", action="store_true", help="Test installed web discovery from an unrelated workspace without --assets")
    parser.add_argument("--output", type=Path, default=Path("target/harness-web-qa"))
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--serve", action="store_true", help="Keep the isolated tested host alive until Ctrl+C")
    parser.add_argument("--settings", action="store_true", help="Verify settings persistence, model personalization, and conversation data controls")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="dsh-harness-web-")).resolve()
    mock = ThreadingHTTPServer(("127.0.0.1", 0), ModelFixture)
    mock.requests = []
    mock.daemon_threads = True
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    process = None
    log = None
    try:
        workspace = root / "workspace"
        outer = root / "outer"
        skill = workspace / ".dsh" / "skills" / "harness-http-fixture" / "SKILL.md"
        skill.parent.mkdir(parents=True)
        skill.write_text("---\nname: harness-http-fixture\ndescription: Isolated browser test skill\n---\nThis is a local fixture.\n", encoding="utf-8")
        plugin = outer / "plugins" / "http-fixture"
        plugin.mkdir(parents=True)
        (plugin / "plugin.json").write_text(json.dumps({"id": "http-fixture", "name": "HTTP Fixture Plugin", "version": "1.0", "tools": [{"name": "echo", "description": "Fixture definition"}]}), encoding="utf-8")
        if args.installed_assets:
            assets = None
        elif args.assets:
            assets = args.assets.resolve(strict=True)
        else:
            assets = root / "dist"
            assets.mkdir()
            (assets / "index.html").write_text("<html><h1>Isolated Harness fixture</h1></html>", encoding="utf-8")
        (root / "secret.txt").write_text("must not be served", encoding="utf-8")
        endpoint = f"http://127.0.0.1:{mock.server_port}/v1"
        config = root / "config.toml"
        config.write_text(fixture_config(endpoint, outer), encoding="utf-8")
        environment = os.environ.copy()
        for key in ("DSH_LLM_FALLBACK_BASE_URLS", "DSH_LLM_EXTRA_BODY", "DSH_MODEL_OPTIMIZATION", "DSH_MODEL_SIZE_B"):
            environment.pop(key, None)
        environment.update({
            "DSH_LLM_BASE_URL": endpoint, "DEEPSEEK_BASE_URL": endpoint,
            "DSH_LLM_BACKEND": "deepseek", "DSH_LLM_MODEL": "harness-fixture", "DEEPSEEK_MODEL": "harness-fixture",
            "DSH_LLM_API_KEY": "local-fixture-key", "DEEPSEEK_API_KEY": "local-fixture-key", "OPENAI_API_KEY": "local-fixture-key",
            "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost",
        })
        port = args.port or available_port()
        log = (output / "host.log").open("w", encoding="utf-8")
        # Use the process working directory, as an installed `dsh web` does.
        command = [str(binary), "--config", str(config), "--silent", "web", "--port", str(port)]
        if assets is not None:
            command += ["--assets", str(assets)]
        process = subprocess.Popen(command, cwd=workspace, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        token = None

        def request(method, path, body=None, authenticated=True):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            headers = {"Origin": f"http://127.0.0.1:{port}"}
            if authenticated and token:
                headers["X-DSH-Token"] = token
            data = json.dumps(body).encode() if body is not None else None
            if data is not None:
                headers["Content-Type"] = "application/json"
            connection.request(method, path, body=data, headers=headers)
            response = connection.getresponse()
            raw = response.read().decode("utf-8")
            status = response.status
            connection.close()
            try:
                value = json.loads(raw)
            except json.JSONDecodeError:
                value = raw
            return status, value

        deadline = time.monotonic() + 20
        while True:
            if process.poll() is not None:
                log.flush()
                raise RuntimeError((output / "host.log").read_text(encoding="utf-8")[-5000:])
            try:
                status, bootstrap = request("GET", "/api/harness/bootstrap")
                if status == 200:
                    break
            except OSError:
                pass
            if time.monotonic() > deadline:
                raise TimeoutError("Harness bootstrap did not become available")
            time.sleep(0.1)
        token = bootstrap["token"]
        assert bootstrap["workspace"].replace("\\", "/").endswith("/workspace")
        assert bootstrap["model"]["ready"] and bootstrap["model"]["name"] == "harness-fixture"
        assert "local-fixture-key" not in json.dumps(bootstrap)
        assert any(skill["name"] == "harness-http-fixture" for skill in bootstrap["skills"])
        assert any(plugin["id"] == "http-fixture" and plugin["tool_count"] == 1 for plugin in bootstrap["plugins"])
        assert request("POST", "/api/harness/rpc", {"method": "sessions/create"}, authenticated=False)[0] == 403

        def rpc(method, params=None):
            status, value = request("POST", "/api/harness/rpc", {"method": method, "params": params or {}})
            assert status == 200 and "error" not in value, value
            return value["result"]

        if args.settings:
            settings = rpc("settings/get")
            assert "local-fixture-key" not in json.dumps(settings)
            assert settings["account"]["kind"] == "local"
            saved = rpc("settings/update", {"patch": {"custom_instructions": "DSH_SETTINGS_QA_CONTEXT", "personality": "concise", "memory_inject": False, "memory_generate": False}})
            assert saved["settings"]["custom_instructions"] == "DSH_SETTINGS_QA_CONTEXT"
            assert "DSH_SETTINGS_QA_CONTEXT" in (outer / "settings.toml").read_text(encoding="utf-8")
            assert rpc("settings/get")["settings"]["memory_inject"] is False
            _, invalid = request("POST", "/api/harness/rpc", {"method": "settings/update", "params": {"patch": {"unknown_field": True}}})
            assert "error" in invalid, invalid
            assert request("POST", "/api/harness/rpc", {"method": "settings/update", "params": {"patch": {"personality": "friendly"}}}, authenticated=False)[0] == 403
            assert rpc("memory/list")["total"] == 0
            assert rpc("memory/clear")["deleted_episodes"] == 0
            scratch_ids = [rpc("sessions/create", {"name": f"Disposable settings QA {n}"})["session"]["id"] for n in (1, 2)]
            assert rpc("sessions/archive_all")["count"] == 2
            assert not rpc("sessions/list")["sessions"]
            assert rpc("sessions/delete_all")["count"] == 2
            assert not any((outer / "sessions" / f"{sid}.json").exists() for sid in scratch_ids)

        session_id = rpc("sessions/create", {"name": "HTTP integration test"})["session"]["id"]
        cursor = bootstrap["latest_sequence"]
        received = []
        for turn in [1, 2]:
            prompt = f"e2e turn {turn}"
            assert rpc("agent/turn", {"session_id": session_id, "prompt": prompt, "wait": False})["accepted"]
            deadline = time.monotonic() + 15
            text = ""
            while True:
                assert time.monotonic() < deadline, "Timed out waiting for actual agent.done"
                batch = rpc("events/wait", {"sequence": cursor, "limit": 2, "timeout_ms": 1000})
                done = False
                for event in batch["events"]:
                    assert event["sequence"] > cursor
                    cursor = event["sequence"]
                    received.append(event)
                    if event["payload"].get("session_id") == session_id:
                        assert event["event_type"] not in ("agent.error", "agent.server_error"), event
                        if event["event_type"] == "agent.text_delta":
                            text += event["payload"]["text"]
                        done = done or event["event_type"] == "agent.done"
                if done:
                    break
            assert text == "Fixture reply: " + prompt, text
        session = rpc("sessions/get", {"id": session_id})["session"]
        assert len([event for event in session["events"] if event["type"] == "assistant_message"]) == 2
        persisted = json.loads((outer / "sessions" / f"{session_id}.json").read_text(encoding="utf-8"))
        assert len([event for event in persisted["events"] if event["type"] == "assistant_message"]) == 2
        assert "Fixture reply: e2e turn 1" in json.dumps(mock.requests[1])
        if args.settings:
            assert "DSH_SETTINGS_QA_CONTEXT" in json.dumps(mock.requests[0])
            assert rpc("sessions/rename", {"id": session_id, "name": "Settings integration verified"})
            assert rpc("sessions/archive", {"id": session_id})["archived"] is True
            assert not rpc("sessions/list")["sessions"]
            assert rpc("sessions/unarchive", {"id": session_id})["archived"] is False
            for export_format in ("json", "markdown"):
                exported = rpc("sessions/export", {"id": session_id, "format": export_format})
                assert exported["session_count"] == 1 and "Fixture reply" in exported["content"]
                assert "local-fixture-key" not in exported["content"]
            metrics = rpc("settings/get")
            assert metrics["usage"]["assistant_message_count"] == 2
            assert metrics["storage"]["session_bytes"] > 0
            assert metrics["usage"]["token_usage_available"] is False
            disposable = rpc("sessions/create")["session"]["id"]
            assert rpc("sessions/delete", {"id": disposable})["deleted"]
            assert not (outer / "sessions" / f"{disposable}.json").exists()
        assert request("POST", "/api/profile", {"username": "OPERATOR-QA", "badge_id": "DSH-QA"})[0] == 200
        assert request("POST", "/api/startup-next", {"enabled": True})[0] == 200
        assert request("GET", "/api/harness/bootstrap")[1]["startup"]["next_enabled"] is True
        assert request("GET", "/api/profile")[1]["inventory_mode"] == "mounted"
        inventory = request("POST", "/api/load", {})[1]
        assert inventory["inventory_mode"] == "mounted" and inventory["type"] == "complete"
        assert (outer / "startup-next.txt").read_text(encoding="utf-8").strip() == "on"
        page_status, page = request("GET", "/")
        assert page_status == 200
        if args.installed_assets:
            assert isinstance(page, str) and 'id="root"' in page, "Installed Harness index missing"
            scripts = re.findall(r'<script\b[^>]*\bsrc="([^"]+)"', page)
            assert scripts, "Installed frontend scripts missing"
            for asset in scripts:
                assert asset.startswith("/assets/") and request("GET", asset)[0] == 200, asset
        for path in ["/../secret.txt", "/%2e%2e/secret.txt", "/%252e%252e/secret.txt", "/assets/%5c..%5csecret.txt"]:
            assert request("GET", path)[0] == 403, path
        report = {"passed": True, "url": f"http://127.0.0.1:{port}/", "session_id": session_id, "mock_requests": len(mock.requests),
                  "stream_events": len(received), "fixture_workspace": str(workspace), "next_cli_marker_unconsumed": True,
                  "installed_assets": args.installed_assets, "settings_checked": args.settings,
                  "checks": ["token", "actual inventory", "two streamed turns", "event pagination", "persisted history", "profile", "next CLI", "static traversal"]}
        if args.settings:
            report["checks"] += ["settings persistence and validation", "custom instructions reach model", "memory management", "archive/unarchive", "rename", "single and bulk delete", "export JSON/Markdown", "actual storage and usage"]
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(report), flush=True)
        if args.serve:
            print(f"FIXTURE_READY {report['url']}  (local mock model; Ctrl+C to stop)", flush=True)
            while process.poll() is None:
                time.sleep(1)
    except KeyboardInterrupt:
        pass
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        mock.shutdown()
        mock.server_close()
        if log:
            log.close()
        assert root.parent == Path(tempfile.gettempdir()).resolve() and root.name.startswith("dsh-harness-web-")
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
