/* DSH's frame-driven lettering: fixed layout, rolling ink, terminal cells and
 * withdrawing redaction bars. Choreography reference: RhineLabUI (MIT),
 * boot-lettering.ts / document-decryption.ts / main.ts at 12cc5e4.
 * Redaction easing adapted under MIT (Copyright 2026 LBEILC; see
 * assets/licenses/RhineLabUI-MIT.txt). No reference font, artwork or audio is used. */
(() => {
  'use strict';
  const clamp = n => Math.max(0, Math.min(1, n));
  const segmenter = typeof Intl?.Segmenter === 'function' ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;
  const characters = value => segmenter ? [...segmenter.segment(value)].map(x => x.segment) : Array.from(value);
  const hash = value => { let n = 2166136261; for (const c of value) n = Math.imul(n ^ c.codePointAt(0), 16777619); return n >>> 0; };
  // A short acceleration followed by a decisive, unbounced withdrawal.
  const departure = t => t < .2 ? .4 * (t / .2) ** 2 : 1 - .6 * ((1 - t) / .8) ** (16 / 3);
  function sample(value, elapsed, options = {}) {
    const target = String(value ?? ''), glyphs = characters(target);
    const kind = options.kind || 'roll', duration = options.duration || (kind === 'mask' ? .72 : kind === 'decode' ? .82 : kind === 'type' ? .66 : .46);
    const age = Math.max(0, Number.isFinite(elapsed) ? elapsed : 0);
    const progress = options.reducedMotion ? 1 : clamp((age - (options.delay || 0)) / duration);
    const eased = departure(progress), settled = progress >= 1;
    let text = target;
    if (!settled && kind === 'type') text = glyphs.slice(0, Math.floor(glyphs.length * progress)).join('');
    if (!settled && kind === 'decode') {
      const symbols = '0123456789/#<>+×[]', seed = hash(target), frame = Math.floor(age * 28);
      text = glyphs.map((c, i) => /^\s+$/u.test(c) || (i + .6) / Math.max(1, glyphs.length) <= progress ? c : symbols[((seed + Math.imul(frame + 1, 19 + i * 7)) >>> 0) % symbols.length]).join('');
    }
    return { text, progress, settled, offset: kind === 'roll' ? (1 - eased) * 1.12 : 0,
      reveal: kind === 'type' ? progress : eased, cover: kind === 'mask' ? 1 - eased : 0,
      blur: kind === 'roll' && !settled ? Math.sin(progress * Math.PI) * .065 : 0 };
  }
  class View {
    constructor(node, options = {}) {
      this.node = node; this.options = options; this.start = 0; this.value = '';
      this.ink = node.ownerDocument ? node.ownerDocument.createElement('span') : document.createElement('span');
      this.ink.className = 'dsh-text-ink';
      node.classList.add('dsh-lettering');
      this.set(node.textContent);
    }
    set(value) {
      const target = String(value ?? '');
      const changed = target !== this.value;
      this.value = target;
      if (changed) this.settled = false;
      // Keep real text in the DOM at all times: unknown plugin counts and live
      // names remain authoritative even while their ink is being revealed.
      if (this.ink.textContent !== target) this.ink.textContent = target;
      if (this.node.children[0] !== this.ink) this.node.replaceChildren(this.ink);
      return changed;
    }
    render(age, reducedMotion) {
      const state = sample(this.value, age, { ...this.options, reducedMotion });
      if (state.settled && this.settled) return;
      this.settled = state.settled;
      const style = this.ink.style;
      // No transforms on measured hosts: all motion stays inside fixed cells.
      style.transform = state.offset ? `translateY(${state.offset.toFixed(4)}em)` : 'none';
      style.filter = state.blur ? `blur(${state.blur.toFixed(4)}em)` : 'none';
      style.clipPath = state.settled ? 'none' : `inset(0 ${(100 * (1 - state.reveal)).toFixed(3)}% 0 0)`;
      this.node.style.setProperty('--dsh-ink-cover', state.cover.toFixed(5));
      this.node.setAttribute('data-lettering', state.settled ? 'settled' : this.options.kind || 'roll');
    }
  }
  class Stage {
    constructor(root) {
      this.views = new Map(); this.phase = -1; this.now = 0; this.globalTime = 0; this.reduced = false;
      for (const node of root.querySelectorAll('[data-motion]')) this.register(node, {
        kind: node.getAttribute('data-motion') || 'roll', delay: Number(node.getAttribute('data-motion-delay') || 0),
        scope: node.getAttribute('data-motion-scope') || 'phase'
      });
    }
    register(node, options = {}) {
      if (!node) return null;
      let view = this.views.get(node);
      if (!view) { view = new View(node, options); view.start = this.now; this.views.set(node, view); }
      return view;
    }
    set(node, value, options = {}) {
      const view = this.register(node, options);
      if (view?.set(value)) view.start = view.options.scope === 'global' ? this.globalTime : this.seeking ? 0 : this.now;
    }
    remove(node) { this.views.delete(node); }
    begin(phase, age, globalTime, reduced, seeking = false) {
      if (phase !== this.phase || age < this.now) for (const view of this.views.values()) {
        if (view.options.scope !== 'global') view.start = 0;
      }
      if (globalTime < this.globalTime) for (const view of this.views.values()) if (view.options.scope === 'global') view.start = 0;
      this.phase = phase; this.now = age; this.globalTime = globalTime; this.reduced = reduced; this.seeking = seeking;
    }
    replayScope(scope) { for (const view of this.views.values()) if (view.options.scope === scope) view.start = this.now; }
    render() {
      for (const view of this.views.values()) {
        const age = (view.options.scope === 'global' ? this.globalTime : this.now) - view.start;
        view.render(Math.max(0, age), this.reduced);
      }
    }
  }
  globalThis.DSHText = Object.freeze({ sample, characters, View, Stage });
})();
