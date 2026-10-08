"""Rebuild the shipped CJK face. Requires fonttools[woff] and brotli.

The original font is downloaded into ignored target/, never required at runtime.
Only Chinese copy and CJK punctuation are included; Latin keeps the DIN-like UI
fallback and arbitrary profile/extension names retain the operating-system font.
"""
from __future__ import annotations

import hashlib
from pathlib import Path
import urllib.request

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parents[1]
REVISION = "a85815a42757630ce188fdad368c2dfc444d4773"
SOURCE_URL = f"https://raw.githubusercontent.com/google/fonts/{REVISION}/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf"
SOURCE_SHA256 = "a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da"


def main() -> None:
    source = ROOT / "target/startup-font-source/NotoSansSC.ttf"
    source.parent.mkdir(parents=True, exist_ok=True)
    if not source.exists():
        urllib.request.urlretrieve(SOURCE_URL, source)
    if hashlib.sha256(source.read_bytes()).hexdigest() != SOURCE_SHA256:
        raise ValueError("The cached font does not match the pinned upstream source")
    copy = "".join(path.read_text(encoding="utf-8") for path in (ROOT / "docs").glob("startup-*") if path.suffix in {".html", ".js"})
    codepoints = {ord(char) for char in copy if 0x3400 <= ord(char) <= 0x9FFF}
    codepoints.update(range(0x3000, 0x3040))
    codepoints.update(range(0xFF00, 0xFFF0))
    options = subset.Options()
    options.hinting = False
    options.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14, 16, 17]
    options.name_legacy = True
    font = TTFont(source, recalcTimestamp=False)
    worker = subset.Subsetter(options=options)
    worker.populate(unicodes=codepoints)
    worker.subset(font)
    # Rename the derivative while retaining upstream copyright and license.
    names = {1: "DSH Industrial SC", 3: "DSHIndustrialSC-20261009", 4: "DSH Industrial SC", 6: "DSHIndustrialSC", 16: "DSH Industrial SC"}
    for record in font["name"].names:
        if record.nameID in names:
            record.string = names[record.nameID].encode(record.getEncoding())
    font.flavor = "woff2"
    destination = ROOT / "docs/assets/fonts/dsh-industrial-sc.woff2"
    destination.parent.mkdir(parents=True, exist_ok=True)
    font.save(destination)
    print(f"Created {destination.name}: {destination.stat().st_size:,} bytes, {len(codepoints)} requested glyphs")
    print(f"Upstream SHA-256: {hashlib.sha256(source.read_bytes()).hexdigest()}")


if __name__ == "__main__":
    main()
