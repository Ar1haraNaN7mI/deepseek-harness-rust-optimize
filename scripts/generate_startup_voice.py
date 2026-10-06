"""Generate fixed English startup narration with official Qwen3-TTS voice cloning.

Run from the repository root in an isolated environment, for example:
  python -m venv --system-site-packages target/voice-generation/venv
  target/voice-generation/venv/Scripts/python -m pip install qwen-tts==0.1.1
  target/voice-generation/venv/Scripts/python scripts/generate_startup_voice.py

This is inference from a reference voice, not fine-tuning. It does not use OS TTS,
upload local inventory, or speak names/counts from the user's computer. Weights,
original reference clips, and unprocessed outputs stay in ignored target/.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import time
import urllib.request

MODEL = "Qwen/Qwen3-TTS-12Hz-1.7B-Base"
MODEL_REVISION = "fd4b254389122332181a7c3db7f27e918eec64e3"
REFERENCE_REVISION = "4a2cfe642867fac013fdef90095981544a819a2a"
REFERENCE_BASE = f"https://raw.githubusercontent.com/6shenhonghong9/dsh-startup-screen/{REFERENCE_REVISION}/lib/assets"
REFERENCES = [
    ("Access permission required.", "ec3afe38d6df4664a8948b4046e4c124c534f27092e97dfc3948e11ff8212934"),
    ("ID confirmed.", "8d045168d150ff57c5f15af14644eaaba0d244694e3fae0ba31313a2c57c6d44"),
    ("Request received.", "c490d5218f36f51680e9b9e02bdb6ca285ef0e0ca13f04c491a53e1308470680"),
    ("Start processing.", "bbfa0d757843451df6e7d306630bf582657dea36f2c4952bb2773496dd5d4637"),
    ("Permission authorized.", "97be9c44591af6f3ef0b7b8788f960e07df684e41236661dd9d2294639f2ae31"),
]
LINES = {
    "phase-0": "D. S. H. Startup sequence initiated.",
    "phase-1": "Preparing the local workspace.",
    "phase-2": "Operator profile confirmed.",
    "phase-3": "Loading local skills and plugins.",
    "phase-3-mounted": "Reviewing local skills and plugins.",
    "phase-4": "Local resources loaded.",
    "phase-5": "Welcome, Operator.",
    "load-warning": "Some local resources require attention.",
    "load-unavailable": "Local resources could not be loaded.",
}
OUTPUT_SAMPLE_RATE = 24_000


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def reference_audio(work: Path):
    import numpy as np
    import soundfile as sf
    from scipy.signal import resample_poly

    directory = work / "reference"
    directory.mkdir(parents=True, exist_ok=True)
    audio = []
    provenance = []
    for index, (transcript, expected) in enumerate(REFERENCES, 1):
        path = directory / f"line-{index}.mp3"
        url = f"{REFERENCE_BASE}/{path.name}"
        if not path.exists():
            urllib.request.urlretrieve(url, path)
        if sha256(path) != expected:
            raise RuntimeError(f"Reference hash mismatch: {path}")
        samples, sample_rate = sf.read(path, dtype="float32", always_2d=True)
        samples = samples.mean(axis=1)
        if sample_rate != OUTPUT_SAMPLE_RATE:
            from math import gcd
            factor = gcd(sample_rate, OUTPUT_SAMPLE_RATE)
            samples = resample_poly(samples, OUTPUT_SAMPLE_RATE // factor, sample_rate // factor)
        audio.append(samples)
        provenance.append({"url": url, "sha256": expected, "transcript": transcript})
    combined = np.concatenate(audio)
    output = directory / "combined.wav"
    sf.write(output, combined, OUTPUT_SAMPLE_RATE, subtype="PCM_16")
    return output, " ".join(text for text, _ in REFERENCES), provenance


def export_pcm(raw, sample_rate: int, output: Path) -> dict:
    import numpy as np
    import soundfile as sf
    from scipy.signal import butter, resample_poly, sosfilt

    samples = np.asarray(raw, dtype=np.float64).reshape(-1)
    if samples.size == 0 or not np.isfinite(samples).all():
        raise RuntimeError(f"Invalid generated audio for {output.name}")
    if sample_rate != OUTPUT_SAMPLE_RATE:
        from math import gcd
        factor = gcd(sample_rate, OUTPUT_SAMPLE_RATE)
        samples = resample_poly(samples, OUTPUT_SAMPLE_RATE // factor, sample_rate // factor)
    samples = sosfilt(butter(2, 24, "highpass", fs=OUTPUT_SAMPLE_RATE, output="sos"), samples)
    peak = float(np.max(np.abs(samples)))
    if peak < 1e-5:
        raise RuntimeError(f"Silent generated audio for {output.name}")
    # Trim model padding while retaining plosives/breath tails. Do not time-stretch.
    active = np.flatnonzero(np.abs(samples) > max(0.002, peak * 0.007))
    if active.size:
        start = max(0, int(active[0]) - int(0.065 * OUTPUT_SAMPLE_RATE))
        end = min(samples.size, int(active[-1]) + int(0.14 * OUTPUT_SAMPLE_RATE))
        samples = samples[start:end]
    duration = samples.size / OUTPUT_SAMPLE_RATE
    if not 0.35 < duration < 8:
        raise RuntimeError(f"Unexpected generated duration {duration:.2f}s for {output.name}; raw clip retained for inspection")
    samples *= 0.78 / max(float(np.max(np.abs(samples))), 1e-5)
    fade = min(int(0.006 * OUTPUT_SAMPLE_RATE), samples.size // 2)
    samples[:fade] *= np.linspace(0, 1, fade)
    samples[-fade:] *= np.linspace(1, 0, fade)
    sf.write(output, samples, OUTPUT_SAMPLE_RATE, subtype="PCM_16")
    info = sf.info(output)
    return {
        "file": output.name, "sample_rate": info.samplerate, "channels": info.channels,
        "format": "PCM_16", "frames": info.frames, "duration_seconds": round(info.duration, 6),
        "peak": round(float(np.max(np.abs(samples))), 6), "sha256": sha256(output),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, default=Path("target/voice-generation"))
    parser.add_argument("--output", type=Path, default=Path("docs/assets/voice"))
    parser.add_argument("--only", nargs="+", choices=LINES, help="Generate selected clips for an initial quality check")
    parser.add_argument("--seed", type=int, default=20261007)
    parser.add_argument("--regenerate", action="store_true", help="Replace cached raw generated clips")
    args = parser.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)
    args.output.mkdir(parents=True, exist_ok=True)
    raw_directory = args.work / "raw"
    raw_directory.mkdir(exist_ok=True)
    os.environ.setdefault("HF_HUB_DISABLE_XET", "1")

    import numpy as np
    import soundfile as sf
    import torch
    from huggingface_hub import snapshot_download
    from qwen_tts import Qwen3TTSModel

    if not torch.cuda.is_available():
        raise RuntimeError("CUDA is unavailable; this generation workflow requires the local GPU")
    torch.manual_seed(args.seed)
    np.random.seed(args.seed)
    torch.set_num_threads(min(6, os.cpu_count() or 1))
    started = time.monotonic()
    reference, transcript, references = reference_audio(args.work)
    model_path = snapshot_download(MODEL, revision=MODEL_REVISION, local_dir=args.work / "model", max_workers=4)
    print(json.dumps({"event":"loading", "model":MODEL, "gpu":torch.cuda.get_device_name(0)}), flush=True)
    model = Qwen3TTSModel.from_pretrained(
        model_path, device_map="cuda:0", dtype=torch.bfloat16, attn_implementation="sdpa",
    )
    with torch.inference_mode():
        prompt = model.create_voice_clone_prompt(ref_audio=str(reference), ref_text=transcript, x_vector_only_mode=False)
        clips = []
        for index, (name, text) in enumerate(LINES.items()):
            if args.only and name not in args.only:
                continue
            raw_path = raw_directory / f"{name}.wav"
            raw_metadata = raw_path.with_suffix(".json")
            cache_key = {
                "model_revision": MODEL_REVISION, "reference_sha256": sha256(reference),
                "reference_transcript": transcript, "text": text, "seed": args.seed + index,
                "temperature": 0.7, "top_p": 0.9,
            }
            clip_started = time.monotonic()
            print(json.dumps({"event":"generating", "name":name, "text":text}), flush=True)
            cache_matches = (raw_metadata.exists()
                             and json.loads(raw_metadata.read_text(encoding="utf-8")) == cache_key)
            reused_cached_raw = raw_path.exists() and cache_matches and not args.regenerate
            if reused_cached_raw:
                samples, sample_rate = sf.read(raw_path, dtype="float32")
            else:
                torch.manual_seed(args.seed + index)
                waves, sample_rate = model.generate_voice_clone(
                    text=text, language="English", voice_clone_prompt=prompt,
                    max_new_tokens=256, do_sample=True, temperature=0.7, top_p=0.9,
                )
                samples = waves[0]
                sf.write(raw_path, samples, sample_rate, subtype="FLOAT")
                raw_metadata.write_text(json.dumps(cache_key, indent=2) + "\n", encoding="utf-8")
            metadata = export_pcm(samples, sample_rate, args.output / f"{name}.wav")
            metadata.update({"text":text, "seed":args.seed + index,
                             "processing_seconds":round(time.monotonic()-clip_started, 3),
                             "reused_cached_raw":reused_cached_raw})
            clips.append(metadata)
            print(json.dumps({"event":"complete", **metadata}), flush=True)
    manifest = {
        "method":"reference-conditioned voice cloning (inference, not fine-tuning)",
        "model":MODEL, "model_revision":MODEL_REVISION,
        "model_url":f"https://huggingface.co/{MODEL}/tree/{MODEL_REVISION}",
        "model_license":"Apache-2.0", "reference_repository":"https://github.com/6shenhonghong9/dsh-startup-screen",
        "reference_revision":REFERENCE_REVISION, "reference_assets":references,
        "reference_transcript":transcript, "reference_sha256":sha256(reference),
        "reference_transcript_check":"faster-whisper small, English, beam_size=5; actual reference audio transcription",
        "qwen_tts_version":importlib.metadata.version("qwen-tts"),
        "transformers_version":importlib.metadata.version("transformers"), "torch_version":torch.__version__,
        "python_version":platform.python_version(), "gpu":torch.cuda.get_device_name(0),
        "attention":"sdpa", "dtype":"bfloat16", "seed":args.seed,
        "postprocess":"24 kHz mono PCM16, 24 Hz high-pass, padding trim, -2.16 dBFS peak, 6 ms fades; no time stretching",
        "total_seconds":round(time.monotonic()-started, 3), "clips":clips,
    }
    manifest_path = args.output / "manifest.json"
    if args.only and manifest_path.exists():
        previous = json.loads(manifest_path.read_text(encoding="utf-8"))
        replaced = {clip["file"] for clip in clips}
        manifest["clips"] = [clip for clip in previous.get("clips", []) if clip["file"] not in replaced] + clips
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"event":"finished", "manifest":str(manifest_path), "seconds":manifest["total_seconds"]}), flush=True)


if __name__ == "__main__":
    main()
