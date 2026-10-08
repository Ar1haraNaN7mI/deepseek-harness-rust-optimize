/* Original deterministic archive-field decoder. No timers, randomness or fake data. */
(() => {
  'use strict';
  const alphabet = '0123456789/#<>+×[]';
  const segmenter = typeof Intl?.Segmenter === 'function' ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;
  const characters = text => segmenter ? Array.from(segmenter.segment(text), item => item.segment) : Array.from(text);
  const clamp = value => Math.max(0, Math.min(1, value));
  function hash(text) {
    let result = 2166136261;
    for (const char of text) result = Math.imul(result ^ char.codePointAt(0), 16777619);
    return result >>> 0;
  }
  function decode(value, elapsed, row = 0, reducedMotion = false) {
    const target = String(value ?? ''), glyphs = characters(target);
    const progress = reducedMotion ? 1 : clamp((Math.max(0, elapsed) - .08 - row * .14) / (.7 + row * .07));
    if (progress >= 1 || !glyphs.length) return { text: target, progress: 1, settled: true };
    const frame = Math.floor(Math.max(0, elapsed) * 26), seed = hash(target + ':' + row);
    const text = glyphs.map((glyph, index) => {
      if (/^\s+$/u.test(glyph)) return glyph;
      // A staggered left-to-right decode; the last glyph always resolves by 1.
      const threshold = (index + .35 + ((seed >>> (index % 20)) & 3) * .16) / Math.max(1, glyphs.length);
      if (progress >= threshold) return glyph;
      return alphabet[((seed + Math.imul(frame + 1, 17 + index * 7) + index * 13) >>> 0) % alphabet.length];
    }).join('');
    return { text, progress, settled: false };
  }
  class View {
    constructor(fields) { this.fields = fields; this.values = fields.map(() => ''); }
    set(values) {
      this.values = values.map(value => String(value ?? ''));
      this.fields.forEach((field, index) => {
        // Screen readers and text selection expose the real profile, never a cipher.
        if (field.value.textContent !== this.values[index]) field.value.textContent = this.values[index];
      });
    }
    render(elapsed, reducedMotion = false) {
      this.fields.forEach((field, index) => {
        const result = decode(this.values[index], elapsed, index, reducedMotion);
        if (field.visual.textContent !== result.text) field.visual.textContent = result.text;
        field.visual.setAttribute('data-decoding', result.settled ? 'false' : 'true');
      });
    }
  }
  globalThis.DSHIdentity = Object.freeze({ decode, View });
})();
