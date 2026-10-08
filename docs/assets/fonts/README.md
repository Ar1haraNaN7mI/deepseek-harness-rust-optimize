# DSH Industrial SC

`dsh-industrial-sc.woff2` is a renamed, subset derivative of **Noto Sans SC**
(variable weight 100–900), by Adobe / the Noto project, licensed under SIL OFL 1.1.
The unmodified license is included as `OFL-NotoSansSC.txt` and must accompany
redistributed animation assets. The font is separate from the project's code license.

Source: [google/fonts / Noto Sans SC](https://github.com/google/fonts/tree/a85815a42757630ce188fdad368c2dfc444d4773/ofl/notosanssc).

The shipped subset covers the startup film's Chinese copy and CJK punctuation.
Latin uses the interface's existing geometric / monospaced fallback. Usernames,
plugin names and other Chinese characters remain legible through system fallback.
The file is served locally: the animation never contacts a font CDN.

To rebuild after changing Chinese startup copy, install `fonttools[woff]` and
`brotli`, then run `python scripts/subset_startup_font.py` from the repository.
The script downloads the pinned upstream font into ignored `target/` and rewrites
the compact subset; the full upstream font is not shipped.
