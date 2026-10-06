"""Measure generated startup WAVs and independently transcribe fixed English lines.

Uses faster-whisper on CPU and an already downloaded local model. Never plays audio.
"""

import argparse
import json
from pathlib import Path
import re


def normalized(text):
    return re.sub(r"[^a-z0-9]", "", text.lower())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--voice", type=Path, default=Path("docs/assets/voice"))
    parser.add_argument("--model-cache", type=Path, default=Path("target/voice-generation/asr-model"))
    parser.add_argument("--report", type=Path, default=Path("target/voice-generation/verification.json"))
    args = parser.parse_args()

    import numpy as np
    import soundfile as sf
    from faster_whisper import WhisperModel

    manifest = json.loads((args.voice / "manifest.json").read_text(encoding="utf-8"))
    model = WhisperModel("small", device="cpu", compute_type="int8", cpu_threads=6,
                         download_root=str(args.model_cache), local_files_only=True)
    results = []
    for clip in manifest["clips"]:
        path = args.voice / clip["file"]
        info = sf.info(path)
        samples, sample_rate = sf.read(path, dtype="float64")
        active = np.flatnonzero(np.abs(samples) > 0.002)
        tail = (len(samples) - active[-1] - 1) / sample_rate if len(active) else info.duration
        segments, _ = model.transcribe(str(path), language="en", beam_size=5,
                                       condition_on_previous_text=False, temperature=0)
        segments = list(segments)
        transcript = " ".join(segment.text.strip() for segment in segments)
        result = {
            "file": clip["file"], "expected": clip["text"], "transcript": transcript,
            "text_matches": normalized(transcript) == normalized(clip["text"]),
            "sample_rate": sample_rate, "channels": info.channels, "subtype": info.subtype,
            "duration_seconds": round(info.duration, 6),
            "peak": round(float(np.max(np.abs(samples))), 6),
            "rms": round(float(np.sqrt(np.mean(samples * samples))), 6),
            "clipped_fraction": float(np.mean(np.abs(samples) >= 32767 / 32768)),
            "tail_silence_seconds": round(tail, 6),
            "asr_average_log_probability": round(float(np.mean([s.avg_logprob for s in segments])), 6) if segments else None,
        }
        result["signal_passed"] = bool(
            sample_rate == 24000 and info.channels == 1 and info.subtype == "PCM_16"
            and 0.35 < info.duration < 8 and result["clipped_fraction"] == 0
            and result["rms"] > 0.002 and tail < 0.6
        )
        results.append(result)
        print(json.dumps(result, ensure_ascii=False), flush=True)
    report = {
        "asr_model": "Systran/faster-whisper-small", "asr_device": "cpu", "asr_compute_type": "int8",
        "subjective_listening_performed": False,
        "passed": all(result["signal_passed"] and result["text_matches"] for result in results),
        "clips": results,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if not report["passed"]:
        raise SystemExit("Voice verification found mismatches; inspect the report before publishing")


if __name__ == "__main__":
    main()
