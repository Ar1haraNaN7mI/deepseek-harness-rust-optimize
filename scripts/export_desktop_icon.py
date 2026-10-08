#!/usr/bin/env python3
"""Rasterize the repository's original vector contours for native Windows icons.

Requires Pillow; rerun when startup-emblem.js changes. No third-party artwork.
"""
import json
from pathlib import Path
from PIL import Image, ImageChops, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / "docs/startup-emblem.js").read_text(encoding="utf-8")
parts = json.loads(source.split("const parts = ", 1)[1].split(";", 1)[0])
size = 1024
image = Image.new("RGBA", (size, size), (10, 16, 19, 255))
mask = Image.new("1", (size, size))
for contours in parts:
    compound = Image.new("1", (size, size))
    for points in contours:
        ring = Image.new("1", (size, size))
        xy = [((x / 236 + .5) * size, (y / 236 + .5) * size) for x, y in points]
        ImageDraw.Draw(ring).polygon(xy, fill=1)
        compound = ImageChops.logical_xor(compound, ring)
    mask = ImageChops.logical_or(mask, compound)
image.paste((234, 238, 234, 255), mask=mask)
icon = image.resize((256, 256), Image.Resampling.LANCZOS)
destination = ROOT / "crates/dsh-desktop/assets"
destination.mkdir(parents=True, exist_ok=True)
(destination / "icon.rgba").write_bytes(icon.resize((128, 128), Image.Resampling.LANCZOS).tobytes())
icon.save(ROOT / "docs/assets/dsh-desktop.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
print("Exported native DSH window and shortcut icons.")
