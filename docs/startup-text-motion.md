# Startup lettering

The DSH opening uses one deterministic lettering clock for the canvas, archive
titles, identity labels, local inventory and footer captions. The source is
`startup-text.js`; no animation dependency or external font request is required.

The choreography follows the techniques in [RhineLabUI](https://github.com/LBEILC/RhineLabUI)
at commit `12cc5e4013acb9408f753ff71de6b8492299b7c8`:

- `src/boot-lettering.ts`: fixed phrase cells, so revealing a letter cannot move
  the rest of a line.
- `src/main.ts`: direct upward rolling transitions, without bouncing or stagger
  between letters, with a 460 ms settling time.
- `src/document-decryption.ts`: opaque redaction strips retract with a short
  acceleration and a long deceleration. DSH adapts its departure easing, while
  keeping the actual layout boxes and text unchanged.

DSH provides its own implementation, layout, emblem, fonts and audio. The
reference's source license is included in `assets/licenses/RhineLabUI-MIT.txt`.
Its third-party phrase artwork, commercial fonts and sampled PV sound are not
included.

Dynamic names, counts and errors remain real text in the document throughout
their reveal. The animation never counts through fictional skills or plugins.
Identity field ciphers retain the real profile as their accessible text. Reduced
motion displays the final text immediately. Seeking, replaying and waiting at
interactive checkpoints all use the same clock; text can settle while a gate
awaits confirmation.

Run `node scripts/test_startup_text.cjs` and
`node scripts/test_startup_browser.cjs` to verify these contracts. The native
official DSH add-on packs the same files from `docs/startup-*`.
