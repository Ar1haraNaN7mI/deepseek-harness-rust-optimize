/* Fixed English narration, generated ahead of time from a reference voice.
   No OS speech engine, live TTS, dynamic names, or per-plugin announcements. */
(() => {
  'use strict';
  let unlocked = false, muted = false, active = false, generation = 0;
  let context, source, controller;
  const cache = new Map();
  function cancel() {
    generation++; active = false;
    controller?.abort(); controller = null;
    if (source) { const player = source; source = null; try { player.stop(); } catch (_) {} player.disconnect(); }
  }
  async function unlock() {
    unlocked = true;
    const Audio = globalThis.AudioContext || globalThis.webkitAudioContext;
    if (!context && Audio) context = new Audio();
    if (context?.state === 'suspended') { try { await context.resume(); } catch (_) {} }
  }
  function clip(index, state) {
    if (!Number.isInteger(index) || index < 0 || index > 5) return null;
    if (index === 3 && state.inventoryMode === 'mounted') return 'phase-3-mounted';
    if (index === 4) {
      if (state.inventory?.status !== 'complete') return 'load-unavailable';
      if (state.inventory.issues?.length) return 'load-warning';
    }
    return `phase-${index}`;
  }
  async function play(name, id) {
    try {
      if (!context || context.state !== 'running') return;
      let buffer = cache.get(name);
      if (!buffer) {
        controller = new AbortController(); const current = controller;
        const deadline = setTimeout(() => current.abort(), 10000);
        try {
          const response = await fetch(`/assets/voice/${name}.wav`, { signal: current.signal });
          if (!response.ok) throw new Error('Narration asset unavailable');
          buffer = await context.decodeAudioData(await response.arrayBuffer());
        } finally { clearTimeout(deadline); }
        if (id !== generation) return;
        cache.set(name, buffer);
      }
      if (id !== generation) return;
      await new Promise(resolve => {
        const player = context.createBufferSource(); player.buffer = buffer;
        player.connect(context.destination); source = player;
        player.onended = () => { player.disconnect(); if (source === player) source = null; resolve(); };
        player.start();
      });
    } catch (_) { /* Missing audio cannot trap startup; no system-TTS fallback. */ }
  }
  function phase(index, state = {}) {
    cancel();
    if (!unlocked || muted) return;
    const name = clip(index, state), id = generation;
    if (!name) return;
    active = true;
    play(name, id).finally(() => { if (id === generation) active = false; });
  }
  globalThis.DSHVoice = Object.freeze({
    unlock, phase, cancel,
    setMuted(value) { muted = Boolean(value); if (muted) cancel(); },
    get busy() { return active; },
  });
  globalThis.addEventListener('pagehide', () => { cancel(); if (context && context.state !== 'closed') context.close(); context = null; });
})();
