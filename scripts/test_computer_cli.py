"""Exercise the real DSH CLI against our own native fixture, without an LLM.

Build dsh-cli and `cargo build -p dsh-computer --example fixture_window` first.
The helper window is private to this test; no user window is acted on.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import tempfile
import threading
import time


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/dsh.exe"))
    parser.add_argument("--fixture", type=Path, default=Path("target/debug/examples/fixture_window.exe"))
    args = parser.parse_args()
    assert os.name == "nt", "native CLI integration currently requires Windows"
    binary, fixture_binary = args.binary.resolve(strict=True), args.fixture.resolve(strict=True)
    source = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="dsh-computer-cli-") as temporary:
        root = Path(temporary)
        workspace = root / "workspace"
        workspace.mkdir()
        config = root / "config.toml"
        config.write_text(
            (source / "config/default.toml").read_text(encoding="utf-8")
            .replace('outer_home = ""', f'outer_home = {json.dumps((root / "outer").as_posix())}')
            .replace("scheduler_enabled = true", "scheduler_enabled = false"),
            encoding="utf-8",
        )
        command = [str(binary), "--workspace", str(workspace), "--config", str(config), "computer"]

        def once(*arguments: str) -> dict:
            result = subprocess.run(command + list(arguments), capture_output=True, text=True, encoding="utf-8", timeout=30, check=True)
            return json.loads(result.stdout)

        assert once("status")["enabled"] is False
        status = once("enable")
        assert status["enabled"] is True
        assert status["cursor_mode"] == "desktop_overlay" and status["physical_pointer"] is False
        assert status["model_input"] == "uia_ocr_text"
        helper = subprocess.Popen([str(fixture_binary)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        session = None
        ocr_coordinate_checked = False
        try:
            selected = None
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                selected = next((window for window in once("windows")["windows"] if window["pid"] == helper.pid), None)
                if selected:
                    break
                time.sleep(0.1)
            assert selected, "owned fixture window was not discovered"
            observed = once("observe", selected["window_id"])
            assert observed["window_id"] == selected["window_id"]
            assert observed["nodes"]
            assert "data_url" not in (observed.get("screenshot") or {})
            assert Path(observed["screenshot"]["path"]).is_file()
            recognition = observed["recognition"]
            assert recognition["engine"] == "windows-ocr" and recognition["local"] is True
            assert recognition["status"] in {"ok", "unavailable", "error"}
            assert recognition["coordinate_space"] == "window_physical_pixels"
            assert isinstance(recognition["text"], str) and isinstance(recognition["lines"], list)
            if recognition["status"] != "ok":
                assert recognition["error"], "OCR unavailability must have an actual reason"

            with (root / "session.log").open("w", encoding="utf-8") as error_log:
                session = subprocess.Popen(command + ["session"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=error_log, text=True, encoding="utf-8", bufsize=1)
                responses: queue.Queue[str] = queue.Queue()

                def read_output() -> None:
                    assert session and session.stdout
                    for line in session.stdout:
                        responses.put(line)

                threading.Thread(target=read_output, daemon=True).start()

                def send(action: dict) -> dict:
                    assert session and session.stdin
                    session.stdin.write(json.dumps(action, ensure_ascii=False) + "\n")
                    session.stdin.flush()
                    result = json.loads(responses.get(timeout=30))
                    assert "error" not in result, result.get("error")
                    return result["result"]

                windows = send({"action": "list_windows"})["windows"]
                target = next(window for window in windows if window["pid"] == helper.pid)
                snapshot = send({"action": "snapshot", "window_id": target["window_id"]})
                editable = next(node for node in snapshot["nodes"] if "set_value" in node["patterns"] and not node["password"])
                value = "DSH background CLI integration"
                send({"action": "set_value", "window_id": target["window_id"], "snapshot_id": snapshot["snapshot_id"], "node_id": editable["node_id"], "text": value})
                refreshed = send({"action": "snapshot", "window_id": target["window_id"]})
                assert any(node.get("value") == value for node in refreshed["nodes"]), "UIA did not confirm the changed fixture value"
                # This label is rendered pixels only, without UIA text. Use its
                # actual OCR box to target our own fixture's custom canvas.
                if refreshed["recognition"]["status"] == "ok":
                    label = next(line for line in refreshed["recognition"]["lines"] if "VISUAL TARGET 42" in line["text"])
                    bounds = label["bounds"]
                    clicked = send({"action": "click", "window_id": target["window_id"], "snapshot_id": refreshed["snapshot_id"], "x": bounds["x"] + bounds["width"] / 2, "y": bounds["y"] + bounds["height"] / 2})
                    assert clicked["delivery"] == "unverified"
                    assert clicked["input_target"]["node_id"] == "background"
                    after_click = send({"action": "snapshot", "window_id": target["window_id"]})
                    assert re.search(r"Canvas\s+clicks:\s*1", after_click["recognition"]["text"], re.IGNORECASE), "fresh OCR did not confirm the custom canvas click"
                    typed = send({"action": "type_text", "window_id": target["window_id"], "snapshot_id": after_click["snapshot_id"], "node_id": "background", "text": "DSH42"})
                    assert typed["delivery"] == "unverified"
                    after_text = send({"action": "snapshot", "window_id": target["window_id"]})
                    assert "DSH42" in after_text["recognition"]["text"], "fresh OCR did not confirm background text delivery"
                    ocr_coordinate_checked = True
                # Invalid commands must fail without terminating the persistent session.
                assert session.stdin
                session.stdin.write('{"action":"key","key":"ENTER"}\n')
                session.stdin.flush()
                rejected = json.loads(responses.get(timeout=30))
                assert "error" in rejected
                assert send({"action": "list_windows"})["windows"]
                # A second CLI process disables the shared capability. The
                # still-running session must reject the next native operation.
                once("disable")
                session.stdin.write('{"action":"list_windows"}\n')
                session.stdin.flush()
                rejected = json.loads(responses.get(timeout=30))
                assert "error" in rejected and "关闭" in rejected["error"]["message"]
                session.stdin.close()
                assert session.wait(timeout=10) == 0
        finally:
            if session and session.poll() is None:
                session.terminate()
                session.wait(timeout=10)
            if helper.poll() is None:
                helper.terminate()
                helper.wait(timeout=10)
        assert once("status")["enabled"] is False
    print("PASS: native CLI opt-in, desktop overlay contract, cross-process UIA/OCR observation, persistent real control action, invalid-action rejection and external disable; no model request.")
    print("PASS: OCR-derived custom canvas coordinate click and background text verified through fresh OCR." if ocr_coordinate_checked else "SKIP: OCR coordinate feedback check; native OCR was unavailable (UIA flow passed).")


if __name__ == "__main__":
    main()
