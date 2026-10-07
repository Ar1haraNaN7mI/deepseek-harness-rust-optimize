"""Render the real Ratatui framebuffer export, not a hand-built terminal mockup.

cargo test -p dsh-tui export_preview_frames -- --ignored
python -X utf8 scripts/render_startup_preview.py
Requires Pillow. Writes a PNG and animated GIF under target/startup-preview/.
"""
import json
import unicodedata
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
data = json.loads((ROOT / "target/startup-frames.json").read_text(encoding="utf-8"))
destination = ROOT / "target/startup-preview"
destination.mkdir(parents=True, exist_ok=True)
fonts = Path("C:/Windows/Fonts")
mono = ImageFont.truetype(str(fonts / "consola.ttf"), 17)
cjk = ImageFont.truetype(str(fonts / "msyh.ttc"), 16)
cw, ch = 11, 23
width, height = data["width"], data["height"]
frames = []
for cells in data["frames"]:
    img = Image.new("RGB", (width * cw, height * ch), (8, 12, 19))
    draw = ImageDraw.Draw(img)
    continuation = set()
    for i, (symbol, _, _) in enumerate(cells):
        if i in continuation:
            continue
        if any(unicodedata.east_asian_width(c) in "WF" for c in symbol):
            if (i % width) + 1 < width:
                continuation.add(i + 1)
    for i, (_, _, bg) in enumerate(cells):
        # Ratatui skips the reset cell behind a double-width glyph.
        if i in continuation:
            continue
        x, y = (i % width) * cw, (i // width) * ch
        glyph_width = 2 if i + 1 in continuation else 1
        draw.rectangle((x, y, x + cw * glyph_width - 1, y + ch - 1), fill=tuple(bg))
    for i, (symbol, fg, _) in enumerate(cells):
        if i in continuation:
            continue
        x, y = (i % width) * cw, (i // width) * ch
        fg = tuple(fg)
        if len(symbol) == 1 and 0x2800 <= ord(symbol) <= 0x28FF:
            dots = ord(symbol) - 0x2800
            for bit, (dx, dy) in enumerate(((0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3))):
                if dots & (1 << bit):
                    px, py = x + 2 + dx * 5, y + 2 + dy * 5
                    draw.ellipse((px, py, px + 2, py + 2), fill=fg)
        elif symbol.strip():
            font = cjk if any(unicodedata.east_asian_width(c) in "WF" for c in symbol) else mono
            draw.text((x, y + 1), symbol, font=font, fill=fg)
    frames.append(img)
frames[-1].save(destination / "terminal.png")
frame_duration = int(data.get("frame_duration_ms", 100))
frames[0].save(destination / "terminal.gif", save_all=True, append_images=frames[1:], duration=[frame_duration] * (len(frames) - 1) + [1500], loop=0)
print(destination)
