#!/usr/bin/env python3
"""Exercise the occupied-port launcher through a real Windows ConPTY.

Requires Windows, Python, and pywinpty. Build the new CLI and web frontend first:
    python scripts/test_web_launch.py --binary target/release/dsh.exe --assets web/dist
    python scripts/test_web_launch.py --binary target/release/dsh.exe --assets web/dist \
        --legacy-binary path/to/old/dsh.exe

Only test-owned processes on randomly selected loopback ports are stopped. Every
new CLI invocation receives --no-open; no browser tabs or real model requests are
created. The optional legacy host is spawned by this script, never discovered on
the user's normal 8770 port. Logs and the JSON report default to ignored target/.
"""

from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

from test_harness_web import available_port, fixture_config


REPO = Path(__file__).resolve().parents[1]
ANSI = re.compile(r"\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]")
PROMPT = "[y/N]:"


def request(port, path, token=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        headers = {"Origin": f"http://127.0.0.1:{port}"}
        if token:
            headers["X-DSH-Token"] = token
        connection.request("GET", path, headers=headers)
        response = connection.getresponse()
        raw = response.read().decode("utf-8")
        try:
            value = json.loads(raw)
        except json.JSONDecodeError:
            value = raw
        return response.status, value
    finally:
        connection.close()


def probe(port):
    status, bootstrap = request(port, "/api/harness/bootstrap")
    if status != 200 or not isinstance(bootstrap, dict) or "token" not in bootstrap:
        raise RuntimeError("Fixture did not return a Harness bootstrap")
    status, service = request(port, "/api/harness/service", bootstrap["token"])
    if status == 404:
        service = None
    elif status != 200 or not isinstance(service, dict):
        raise RuntimeError("Fixture did not return an authenticated service identity")
    return bootstrap, service


def wait_for_service(port, predicate, alive, timeout):
    deadline = time.monotonic() + timeout
    last_error = "not available"
    while time.monotonic() < deadline:
        if not alive():
            raise RuntimeError("Fixture process exited before the expected service became available")
        try:
            state = probe(port)
            if predicate(*state):
                return state
        except (OSError, http.client.HTTPException, RuntimeError) as error:
            last_error = str(error)
        time.sleep(0.05)
    raise TimeoutError(f"Timed out waiting for fixture service: {last_error}")


class PipeChild:
    def __init__(self, command, cwd, environment, log_path):
        self.log = log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            command, cwd=cwd, env=environment, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.reader = threading.Thread(target=self._drain, daemon=True)
        self.reader.start()

    def _drain(self):
        assert self.process.stdout is not None
        for line in iter(self.process.stdout.readline, b""):
            self.log.write(line.decode("utf-8", errors="replace"))
            self.log.flush()

    def alive(self):
        return self.process.poll() is None

    def close(self):
        if self.alive():
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.reader.join(timeout=3)
        if self.process.stdout:
            self.process.stdout.close()
        self.log.close()


class TerminalChild:
    """Use native ConPTY with nonblocking reads; answers follow actual prompts."""

    def __init__(self, command, cwd, environment, log_path):
        import winpty
        self.pty = winpty.PTY(140, 38, backend=winpty.Backend.ConPTY)
        self.output = ""
        self.answered = 0
        self.log = log_path.open("w", encoding="utf-8")
        env_block = "\0".join(f"{key}={value}" for key, value in sorted(environment.items())) + "\0"
        spawned = self.pty.spawn(command[0], cwd=str(cwd), env=env_block,
                                 cmdline=" " + subprocess.list2cmdline(command[1:]))
        if not spawned:
            self.log.close()
            raise RuntimeError("Could not create fixture ConPTY process")
        # Retain an exact process handle for emergency cleanup; never look up a
        # later owner of this port or use a broad process-name/tree termination.
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        self.kernel.TerminateProcess.restype = wintypes.BOOL
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.CloseHandle.restype = wintypes.BOOL
        self.handle = self.kernel.OpenProcess(0x00100001, False, self.pty.pid)

    def alive(self):
        return self.pty.isalive()

    def drain(self):
        for _ in range(100):
            try:
                text = self.pty.read(16384, blocking=False)
            except Exception:
                if not self.alive():
                    break
                raise
            if not text:
                break
            if isinstance(text, bytes):
                text = text.decode("utf-8", errors="replace")
            self.output += text
            self.log.write(text)
            self.log.flush()
        return ANSI.sub("", self.output)

    def answer(self, value, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            output = self.drain()
            if output.count(PROMPT) > self.answered:
                # Delay each answer until the real prompt is on screen, rather
                # than preloading stdin before is_terminal / read_line runs.
                time.sleep(0.12)
                self.pty.write(value + "\r")
                self.answered += 1
                return
            if not self.alive():
                raise RuntimeError(f"CLI exited before prompt {self.answered + 1}: {output[-1800:]}")
            time.sleep(0.03)
        raise TimeoutError(f"CLI did not display prompt {self.answered + 1}: {self.drain()[-1800:]}")

    def wait_exit(self, timeout):
        deadline = time.monotonic() + timeout
        while self.alive() and time.monotonic() < deadline:
            self.drain()
            time.sleep(0.03)
        if self.alive():
            raise TimeoutError(f"CLI did not exit: {self.drain()[-1800:]}")
        self.drain()
        return self.pty.get_exitstatus()

    def stop_with_ctrl_c(self, timeout):
        if self.alive():
            self.pty.write("\x03")
        return self.wait_exit(timeout)

    def close(self):
        try:
            if self.alive():
                try:
                    self.stop_with_ctrl_c(5)
                except TimeoutError:
                    if not self.handle or not self.kernel.TerminateProcess(self.handle, 1):
                        raise RuntimeError("Could not clean up the exact fixture terminal process")
                    self.wait_exit(5)
            self.drain()
        finally:
            if self.handle:
                self.kernel.CloseHandle(self.handle)
                self.handle = None
            self.log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, required=True, help="New binary with occupied-port dialog and --no-open")
    parser.add_argument("--assets", type=Path, default=REPO / "web" / "dist")
    parser.add_argument("--legacy-binary", type=Path, help="Optional old dsh.exe without /api/harness/service")
    parser.add_argument("--output", type=Path, default=REPO / "target" / "web-launch-qa")
    parser.add_argument("--timeout", type=float, default=45)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("This integration test requires Windows ConPTY")
    try:
        import winpty  # noqa: F401
    except ImportError:
        parser.error("Install pywinpty in this Python environment to run the ConPTY integration test")
    binary = args.binary.resolve(strict=True)
    legacy_binary = args.legacy_binary.resolve(strict=True) if args.legacy_binary else None
    assets = args.assets.resolve(strict=True)
    if not (assets / "index.html").is_file():
        parser.error("--assets must point to a built frontend containing index.html")
    if legacy_binary == binary:
        parser.error("--legacy-binary must be a distinct old executable")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="dsh-web-launch-")).resolve()
    children = []
    checks = []
    report = {"passed": False, "binary": str(binary), "checks": checks, "browser_opened": False}
    try:
        workspace_a, workspace_b = root / "workspace-a", root / "workspace-b"
        workspace_a.mkdir()
        workspace_b.mkdir()
        outer = root / "outer"
        # No inference is requested. Even an accidental request can only reach
        # this closed loopback fixture port, never a user's configured provider.
        endpoint = f"http://127.0.0.1:{available_port()}/v1"
        config = root / "config.toml"
        config.write_text(fixture_config(endpoint, outer), encoding="utf-8")
        environment = {key: value for key, value in os.environ.items() if not key.startswith("DSH_")}
        environment.update({
            "DSH_LLM_BASE_URL": endpoint, "DEEPSEEK_BASE_URL": endpoint, "OPENAI_BASE_URL": endpoint,
            "DSH_LLM_BACKEND": "deepseek", "DSH_LLM_MODEL": "harness-fixture", "DEEPSEEK_MODEL": "harness-fixture",
            "DSH_LLM_API_KEY": "local-fixture-key", "DEEPSEEK_API_KEY": "local-fixture-key", "OPENAI_API_KEY": "local-fixture-key",
            "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost",
        })

        def command(executable, port, *, legacy=False, startup=None):
            result = [str(executable), "--config", str(config), "--silent"]
            if startup is not None:
                result.append("--startup" if startup else "--no-startup")
            result += ["web", "--port", str(port), "--assets", str(assets)]
            if not legacy:
                result.append("--no-open")
            return result

        def terminal(name, port, startup=None):
            child = TerminalChild(command(binary, port, startup=startup), workspace_b, environment, output / f"{name}.log")
            children.append(child)
            return child

        port = available_port()
        assert port != 8770
        host = PipeChild(command(binary, port), workspace_a, environment, output / "original-host.log")
        children.append(host)
        _, identity = wait_for_service(port, lambda _b, s: s is not None, host.alive, args.timeout)
        assert identity and identity["pid"] == host.process.pid
        original_instance = identity["instance_id"]

        opened = terminal("open-existing", port, startup=True)
        opened.answer("y", args.timeout)
        assert opened.wait_exit(args.timeout) == 0
        assert "?dsh-startup=on" in opened.drain()
        assert host.alive() and probe(port)[1]["instance_id"] == original_instance
        checks.append("yes opens existing instance and carries startup=on without launching browser")

        looped = terminal("decline-restart-loop", port, startup=False)
        for answer in ("n", "n", "y"):
            looped.answer(answer, args.timeout)
        assert looped.wait_exit(args.timeout) == 0
        assert looped.drain().count(PROMPT) == 3 and "?dsh-startup=off" in looped.drain()
        assert host.alive() and probe(port)[1]["instance_id"] == original_instance
        checks.append("no then no returns to initial question; yes preserves original instance")

        cancelled = terminal("cancel-dialog", port)
        cancelled.answer("n", args.timeout)
        assert cancelled.stop_with_ctrl_c(8) != 0
        assert host.alive() and probe(port)[1]["instance_id"] == original_instance
        checks.append("Ctrl+C cancels the dialog without stopping the original service")

        # Existing service reuse needs no assets in this invocation, but a
        # confirmed replacement must validate its assets before closing it.
        for name, answers, expected_status in [
            ("reuse-with-missing-assets", ["y"], 0),
            ("restart-with-missing-assets", ["n", "y"], 1),
        ]:
            arguments = command(binary, port)
            arguments[arguments.index("--assets") + 1] = str(root / "missing-assets")
            child = TerminalChild(arguments, workspace_b, environment, output / f"{name}.log")
            children.append(child)
            for answer in answers:
                child.answer(answer, args.timeout)
            assert child.wait_exit(args.timeout) == expected_status
            assert host.alive() and probe(port)[1]["instance_id"] == original_instance
        checks.append("reuse works without new assets and invalid replacement leaves original service intact")

        started = time.monotonic()
        non_tty = subprocess.run(command(binary, port), cwd=workspace_b, env=environment, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=12, creationflags=subprocess.CREATE_NO_WINDOW)
        (output / "non-tty.log").write_bytes(non_tty.stdout)
        assert non_tty.returncode != 0 and time.monotonic() - started < 12
        assert PROMPT not in non_tty.stdout.decode("utf-8", errors="replace")
        assert host.alive() and probe(port)[1]["instance_id"] == original_instance
        checks.append("non-TTY occupied port returns promptly without asking or stopping")

        restarted = terminal("confirmed-restart", port)
        restarted.answer("n", args.timeout)
        restarted.answer("y", args.timeout)
        new_bootstrap, new_identity = wait_for_service(
            port, lambda _b, s: s is not None and s["instance_id"] != original_instance,
            restarted.alive, args.timeout,
        )
        assert new_identity and new_identity["pid"] == restarted.pty.pid
        assert Path(new_bootstrap["workspace"]).resolve() == workspace_b
        assert host.process.wait(timeout=8) == 0
        assert restarted.stop_with_ctrl_c(8) == 0
        checks.append("confirmed restart gracefully stops old process, changes identity/workspace, and Ctrl+C exits")

        unrelated_port = available_port()
        unrelated_source = (
            "from http.server import BaseHTTPRequestHandler,HTTPServer; import sys\n"
            "class H(BaseHTTPRequestHandler):\n"
            " def do_GET(self):\n"
            "  body=b'{\"service\":\"unrelated-fixture\"}'; self.send_response(200); self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)\n"
            "HTTPServer(('127.0.0.1',int(sys.argv[1])),H).serve_forever()\n"
        )
        unrelated = PipeChild([sys.executable, "-u", "-c", unrelated_source, str(unrelated_port)], workspace_a, environment, output / "unrelated-host.log")
        children.append(unrelated)
        deadline = time.monotonic() + args.timeout
        while True:
            try:
                assert request(unrelated_port, "/")[1]["service"] == "unrelated-fixture"
                break
            except OSError:
                if not unrelated.alive() or time.monotonic() > deadline:
                    raise RuntimeError("Unrelated fixture did not start")
                time.sleep(0.05)
        refused = terminal("unrelated-port-refused", unrelated_port)
        assert refused.wait_exit(args.timeout) != 0
        assert PROMPT not in refused.drain()
        assert unrelated.alive() and request(unrelated_port, "/")[1]["service"] == "unrelated-fixture"
        checks.append("unrelated listener is refused before prompts and remains alive")

        if legacy_binary:
            legacy_port = available_port()
            assert legacy_port != 8770
            legacy = PipeChild(command(legacy_binary, legacy_port, legacy=True), workspace_a, environment, output / "legacy-host.log")
            children.append(legacy)
            _, service = wait_for_service(legacy_port, lambda _b, s: s is None, legacy.alive, args.timeout)
            assert service is None
            replacement = terminal("legacy-confirmed-restart", legacy_port)
            replacement.answer("n", args.timeout)
            replacement.answer("y", args.timeout)
            legacy_bootstrap, replacement_id = wait_for_service(legacy_port, lambda _b, s: s is not None,
                                                                 replacement.alive, args.timeout)
            assert replacement_id and replacement_id["pid"] == replacement.pty.pid
            assert Path(legacy_bootstrap["workspace"]).resolve() == workspace_b
            legacy.process.wait(timeout=8)
            assert replacement.stop_with_ctrl_c(8) == 0
            checks.append("confirmed legacy restart terminates only the test-owned old dsh.exe and starts new service")
        report["passed"] = True
    except BaseException as error:
        report["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        cleanup_errors = []
        for child in reversed(children):
            try:
                child.close()
            except Exception as error:
                cleanup_errors.append(str(error))
        if cleanup_errors:
            report["cleanup_errors"] = cleanup_errors
            report["passed"] = False
        resolved = root.resolve()
        assert resolved.parent == Path(tempfile.gettempdir()).resolve() and resolved.name.startswith("dsh-web-launch-")
        try:
            shutil.rmtree(resolved)
        except OSError as error:
            report["fixture_cleanup_error"] = str(error)
            report["passed"] = False
        (output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(json.dumps(report, ensure_ascii=False), flush=True)
    if not report["passed"]:
        raise RuntimeError("Fixture cleanup failed; see report.json")


if __name__ == "__main__":
    main()
