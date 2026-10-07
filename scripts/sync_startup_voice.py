"""Validate and embed the reviewed fixed startup recordings in the TUI crate.

After generating and reviewing docs/assets/voice, run this script to copy the
native subset. Use --check to verify checked-in assets without modifying files.
Only Python's standard library is required; no model runs or audio is played.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
from pathlib import Path
import struct
import wave

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "docs" / "assets" / "voice"
DESTINATION = ROOT / "crates" / "dsh-tui" / "assets" / "voice"
NATIVE_FILES = (
    "phase-0.wav",
    "phase-1.wav",
    "phase-2.wav",
    "phase-3-mounted.wav",
    "phase-4.wav",
    "phase-5.wav",
    "load-unavailable.wav",
)


def reviewed_assets() -> tuple[dict, dict[str, bytes]]:
    manifest = json.loads((SOURCE / "manifest.json").read_text(encoding="utf-8"))
    clips = {clip["file"]: clip for clip in manifest["clips"]}
    assets = {}
    for name in NATIVE_FILES:
        clip = clips.get(name)
        if clip is None:
            raise ValueError(f"Generation manifest is incomplete: {name}")
        data = (SOURCE / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != clip["sha256"]:
            raise ValueError(f"Generated file differs from its manifest: {name}")
        with wave.open(io.BytesIO(data), "rb") as reader:
            if (
                reader.getnchannels(),
                reader.getsampwidth(),
                reader.getframerate(),
                reader.getcomptype(),
            ) != (1, 2, 24_000, "NONE"):
                raise ValueError(f"Native audio requires PCM16 mono 24 kHz: {name}")
            frames = reader.getnframes()
            pcm = reader.readframes(frames)
            if frames != clip["frames"] or len(pcm) != frames * 2:
                raise ValueError(f"Truncated or changed waveform: {name}")
            if frames == 0 or not any(sample[0] for sample in struct.iter_unpack("<h", pcm)):
                raise ValueError(f"Recording contains no audio: {name}")
            if abs(frames / 24_000 - clip["duration_seconds"]) > 0.000_001:
                raise ValueError(f"Recording duration differs from its manifest: {name}")
        assets[name] = data
    # Retain model and reference provenance alongside the packaged native subset.
    native_manifest = dict(manifest)
    native_manifest["clips"] = [clips[name] for name in NATIVE_FILES]
    native_manifest["embedded_for"] = "dsh-tui; generated source is docs/assets/voice"
    return native_manifest, assets


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Check without writing files")
    args = parser.parse_args()
    manifest, assets = reviewed_assets()
    if not args.check:
        DESTINATION.mkdir(parents=True, exist_ok=True)
        # Validate the complete source set first, then replace each packaged file.
        for name, data in assets.items():
            temporary = DESTINATION / f".{name}.tmp"
            temporary.write_bytes(data)
            temporary.replace(DESTINATION / name)
        (DESTINATION / "manifest.json").write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    for name, data in assets.items():
        if (DESTINATION / name).read_bytes() != data:
            raise ValueError(f"Embedded recording does not match generated source: {name}")
        print(f"{name}: {hashlib.sha256(data).hexdigest()}")
    packaged = json.loads((DESTINATION / "manifest.json").read_text(encoding="utf-8"))
    if packaged != manifest:
        raise ValueError("Embedded provenance manifest is stale")
    print(f"Verified {len(assets)} native recordings; no audio was played.")


if __name__ == "__main__":
    main()
