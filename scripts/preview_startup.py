"""Silent Windows ConPTY smoke checks for the real startup preview.

Build first: cargo build -p dsh-cli
Run: python scripts/preview_startup.py
Direct CLI integration only: python scripts/preview_startup.py --case none --direct-startup
Requires pywinpty. Captures ANSI and a JSON report under target/startup-qa.
This never launches a visible console or enables audible playback.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

from winpty import PTY


# Run a parent inside the PTY so console modes can be compared before/after DSH.
PROBE = r'''
import ctypes, json, subprocess, sys, time
from ctypes import wintypes as w
k = ctypes.WinDLL("kernel32", use_last_error=True)
k.GetStdHandle.argtypes, k.GetStdHandle.restype = [w.DWORD], w.HANDLE
k.GetConsoleMode.argtypes = [w.HANDLE, ctypes.POINTER(w.DWORD)]
class Cursor(ctypes.Structure):
    _fields_ = [("size", w.DWORD), ("visible", w.BOOL)]
k.GetConsoleCursorInfo.argtypes = [w.HANDLE, ctypes.POINTER(Cursor)]
def state():
    modes = []
    for n in (-10, -11):
        value = w.DWORD()
        assert k.GetConsoleMode(k.GetStdHandle(n), ctypes.byref(value))
        modes.append(value.value)
    cursor = Cursor()
    assert k.GetConsoleCursorInfo(k.GetStdHandle(-11), ctypes.byref(cursor))
    return [modes[0], modes[1], bool(cursor.visible)]
before = state()
result = subprocess.run(sys.argv[1:])
print("\n__DSH_QA__" + json.dumps(dict(exit_code=result.returncode, before=before, after=state()), separators=(",", ":")), flush=True)
# Let ConPTY deliver its final frame before the probe process closes the pipe.
time.sleep(0.2)
sys.exit(result.returncode)
'''

CASES = {
    "complete": (["--auto", "--speed", "2.5"], []),
    "skip": ([], [(0.5, "key", "\x1b")]),
    "mute": ([], [(0.4, "key", "m"), (0.8, "key", "m"), (1.2, "key", "\x1b")]),
    "ctrl_c": ([], [(0.5, "key", "\x03")]),
    # Enter during playback emits a pulse without skipping. Escape exits afterward.
    "advance": ([], [(0.4, "key", "\r"), (0.6, "key", "\r"), (0.8, "key", "\r"), (1.2, "key", "\x1b")]),
    "interactive": (["--interactive", "--speed", "2.5"], [
        (0.3, "key", "2"), (0.5, "key", "\r"),
        (1.1, "key", "3"), (1.3, "key", "\r"),
        # Later confirmations wait for actual gate text below. Cold startup
        # and ConPTY throughput must not turn a confirmation into a pulse.
    ]),
    "mouse": (["--interactive", "--speed", "2.5"], [
        (0.5, "click", (2, 2)),
        (1.3, "click", (55, 16)),  # Pulse during playback, not an extra stage advance.
    ]),
    "resize": ([], [(0.4, "size", (18, 53)), (0.8, "size", (3, 8)), (1.2, "size", (32, 110)), (1.6, "key", "\x1b")]),
}


def run_case(
    executable, config, directory, output, name, *, tui=False,
    expected_animation=True, launch_arguments=None, natural_completion=False,
):
    if tui:
        arguments = list(launch_arguments or [])
        scheduled = [] if natural_completion else [(0.8, "key", "\x1b"), (1.4, "key", "\x03")]
    else:
        arguments, scheduled = CASES[name]
    environment = dict(os.environ)
    for variable in ("CI", "NO_COLOR", "DSH_NO_STARTUP"):
        environment.pop(variable, None)
    environment["TERM"] = "xterm-256color"
    process = PTY(110, 32)
    command = ["-u", "-c", PROBE, str(executable), "--config", str(config), *([] if tui else ["startup"]), "--silent", *arguments]
    process.spawn(
        sys.executable, cmdline=" " + subprocess.list2cmdline(command),
        cwd=str(directory), env="\0".join(f"{key}={value}" for key, value in environment.items()) + "\0",
    )
    start = time.monotonic()
    pending = list(scheduled)
    gate_prompts = ["查看本机清单", "完成接入"] if name in ("interactive", "mouse") else []
    confirmed_gates = 0
    gate_tail = ""
    captured = []
    eof = False
    live_tail = ""
    chat_after_seconds = None
    try:
        while time.monotonic() - start < 12:
            elapsed = time.monotonic() - start
            while pending and elapsed >= pending[0][0]:
                _, kind, value = pending.pop(0)
                if kind == "key":
                    process.write(value)
                elif kind == "click":
                    column, row = value
                    process.write(f"\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m")
                else:
                    process.set_size(value[1], value[0])
            try:
                chunk = process.read(65536, blocking=False)
            except Exception as error:
                if "EOF" not in str(error):
                    raise
                eof = True
                break
            if chunk:
                captured.append(chunk)
                if gate_prompts:
                    gate_tail = (gate_tail + chunk)[-65536:]
                    visible_gate = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", gate_tail)
                    if gate_prompts[0] in visible_gate:
                        gate_prompts.pop(0)
                        action = ("key", "\r") if name == "interactive" else ("click", (108, 29) if confirmed_gates == 0 else (55, 16))
                        pending.append((elapsed + 0.35, *action))
                        pending.sort(key=lambda item: item[0])
                        confirmed_gates += 1
                        gate_tail = ""
                if natural_completion and chat_after_seconds is None:
                    live_tail = (live_tail + chunk)[-65536:]
                    visible = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", live_tail)
                    if "Welcome to dsh-rust" in visible:
                        chat_after_seconds = time.monotonic() - start
                        # No skip/advance key was sent. Only exit after the real
                        # chat has appeared following natural animation completion.
                        process.write("\x03")
            if not process.isalive() and not chunk:
                # PTY.iseof() can block on a closed ConPTY pipe. Process exit
                # plus an empty read is enough; distinguish observed EOF below.
                break
            time.sleep(0.005)
    finally:
        finished = time.monotonic()
        if process.isalive():
            # Only our private probe and its children are terminated on timeout.
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], capture_output=True, creationflags=subprocess.CREATE_NO_WINDOW)
    text = "".join(captured)
    (output / f"{name}.ansi").write_text(text, encoding="utf-8")
    clean = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    marker = re.search(r"__DSH_QA__(\{[^\r\n]+\})", clean)
    if not marker:
        raise AssertionError(f"{name}: missing restoration probe; inspect {output / (name + '.ansi')}")
    result = json.loads(marker.group(1))
    for key in ("before", "after"):
        result[key] = dict(zip(("input_mode", "output_mode", "cursor_visible"), result[key]))
    result.update(case=name, eof_observed=eof, process_exited=not process.isalive(), elapsed_seconds=round(finished - start, 3), captured_characters=len(text))
    assert result["process_exited"], f"{name}: preview did not exit"
    assert result["exit_code"] == 0, result
    assert result["before"]["input_mode"] == result["after"]["input_mode"], result
    assert result["before"]["output_mode"] == result["after"]["output_mode"], result
    assert result["after"]["cursor_visible"], result
    assert ("DEEP DIVE" in clean) == expected_animation, f"{name}: unexpected animation state"
    if tui:
        assert "Welcome to dsh-rust" in clean, f"{name}: normal TUI did not appear"
    assert "\x1b[?1049l" in text, f"{name}: alternate screen not restored"
    if natural_completion:
        assert chat_after_seconds is not None, "natural animation completion never reached chat"
        assert chat_after_seconds >= 5.0, "startup was bypassed instead of playing to completion"
        assert "WELCOME TO DSH" in clean, "final animation phase never appeared"
        result["chat_after_seconds"] = round(chat_after_seconds, 3)
    elif name == "complete":
        assert result["elapsed_seconds"] >= 5.0, result
    elif name in ("interactive", "mouse"):
        assert confirmed_gates == 2, "Both real interaction gates must have appeared"
        assert result["elapsed_seconds"] >= 6.3, result
    elif name == "advance":
        assert result["elapsed_seconds"] < 3.5, result
        assert "WORKSPACE CONTEXT" not in clean, "mid-play Enter incorrectly skipped opening stage"
    else:
        assert result["elapsed_seconds"] < 3.5, result
    return result


def run_next_once(executable, root, output):
    with tempfile.TemporaryDirectory(prefix="dsh-next-startup-qa-") as temporary:
        directory = Path(temporary)
        outer = directory / "outer"
        workspace = directory / "workspace"
        workspace.mkdir()
        config = directory / "config.toml"
        source = (root / "config/default.toml").read_text(encoding="utf-8")
        source = re.sub(r"(?m)^outer_home\s*=.*$", f'outer_home = "{outer.as_posix()}"', source)
        source = re.sub(r"(?m)^volume\s*=.*$", "volume = 0.0", source)
        source = re.sub(r"(?m)^scheduler_enabled\s*=.*$", "scheduler_enabled = false", source)
        config.write_text(source, encoding="utf-8")
        subprocess.run([str(executable), "--config", str(config), "startup", "next", "on"], cwd=workspace, check=True, capture_output=True)
        marker = outer / "startup-next.txt"
        assert marker.exists(), "next on did not create its one-use preference"
        first = run_case(executable, config, workspace, output, "next_first", tui=True)
        assert not marker.exists(), "interactive TUI did not consume next on"
        second = run_case(executable, config, workspace, output, "next_second", tui=True, expected_animation=False)
        assert not marker.exists(), "second TUI recreated next on"
        return [first, second]


def run_direct_startup(executable, root, output):
    with tempfile.TemporaryDirectory(prefix="dsh-direct-startup-qa-") as temporary:
        directory = Path(temporary)
        outer = directory / "outer"
        workspace = directory / "workspace"
        workspace.mkdir()
        config = directory / "config.toml"
        source = (root / "config/default.toml").read_text(encoding="utf-8")
        source = re.sub(r"(?m)^outer_home\s*=.*$", f'outer_home = "{outer.as_posix()}"', source)
        source = re.sub(r"(?m)^volume\s*=.*$", "volume = 0.0", source)
        source = re.sub(r"(?m)^scheduler_enabled\s*=.*$", "scheduler_enabled = false", source)
        source = re.sub(r"(?m)^interactive\s*=.*$", "interactive = false", source)
        source = re.sub(r"(?m)^speed\s*=.*$", "speed = 2.5", source)
        # Keep the permanent preference off; this test must exercise --startup.
        source = re.sub(r"(?m)(^\[tui\.startup\]\s*\n)enabled\s*=.*$", r"\1enabled = false", source)
        config.write_text(source, encoding="utf-8")
        command = [str(executable), "--config", str(config)]
        subprocess.run([*command, "startup", "next", "off"], cwd=workspace, check=True, capture_output=True)
        marker = outer / "startup-next.txt"
        expected_marker = marker.read_bytes()
        for arguments in [
            ["--startup", "--no-startup"],
            ["--startup", "tui", "--no-startup"],
            ["--no-startup", "tui", "--startup"],
        ]:
            conflict = subprocess.run(
                [*command, *arguments], cwd=workspace,
                capture_output=True, encoding="utf-8", timeout=10,
            )
            assert conflict.returncode == 2, f"conflicting startup flags were accepted: {arguments}"
            assert "--startup" in conflict.stderr and "--no-startup" in conflict.stderr
            assert marker.read_bytes() == expected_marker, "parse failure consumed the pending choice"
        results = [{"case": "direct_startup_conflict", "variants": 3, "exit_code": 2, "expected_exit_code": 2}]

        # These redirected commands must leave the queued terminal preference intact.
        for arguments in [["doctor", "--startup"], ["startup", "--startup", "--silent"]]:
            subprocess.run([*command, *arguments], cwd=workspace, check=True, capture_output=True, timeout=15)
            assert marker.read_bytes() == expected_marker, f"non-TUI command consumed next off: {arguments}"

        complete = run_case(
            executable, config, workspace, output, "direct_startup_complete", tui=True,
            launch_arguments=["--startup"], natural_completion=True,
        )
        assert not marker.exists(), "--startup did not consume the overridden next off"
        results.append(complete)
        results.append(run_case(
            executable, config, workspace, output, "direct_startup_second", tui=True,
            expected_animation=False,
        ))
        assert not marker.exists(), "single-invocation startup became persistent"
        return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parents[1]
    parser.add_argument("--exe", type=Path, default=root / "target/debug/dsh.exe")
    parser.add_argument("--output", type=Path, default=root / "target/startup-qa")
    parser.add_argument("--case", choices=["all", "none", *CASES], default="all")
    parser.add_argument("--next-once", action="store_true", help="Also test one-use preference across two isolated TUI launches")
    parser.add_argument("--direct-startup", action="store_true", help="Test --startup overriding next off, natural completion into chat, and non-consumption by previews")
    args = parser.parse_args()
    help_result = subprocess.run([str(args.exe), "startup", "--help"], capture_output=True, encoding="utf-8")
    if "--auto" not in help_result.stdout or (args.direct_startup and "--startup" not in help_result.stdout):
        raise SystemExit("Build the current binary first: cargo build -p dsh-cli")
    args.output.mkdir(parents=True, exist_ok=True)
    source = (root / "config/default.toml").read_text(encoding="utf-8")
    # A zero-volume private config prevents even the mute-toggle test making sound.
    source = re.sub(r"(?m)^volume\s*=.*$", "volume = 0.0", source)
    reports = []
    with tempfile.TemporaryDirectory(prefix="dsh-startup-qa-") as temporary:
        config = Path(temporary) / "config.toml"
        config.write_text(source, encoding="utf-8")
        cases = CASES if args.case == "all" else ([] if args.case == "none" else [args.case])
        for name in cases:
            result = run_case(args.exe.resolve(), config, root, args.output, name)
            reports.append(result)
            print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.next_once:
        for result in run_next_once(args.exe.resolve(), root, args.output):
            reports.append(result)
            print(json.dumps(result, ensure_ascii=False), flush=True)
    if args.direct_startup:
        for result in run_direct_startup(args.exe.resolve(), root, args.output):
            reports.append(result)
            print(json.dumps(result, ensure_ascii=False), flush=True)
    (args.output / "report.json").write_text(json.dumps(reports, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
