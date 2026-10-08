/* A narrow completion bridge for the React Harness startup gate.
   The standalone preview keeps its replay behavior. No parent commands execute. */
(() => {
  'use strict';
  let target = null, channel = '', finished = false;
  try {
    const params = new URLSearchParams(location.search);
    const origin = new URL(params.get('parentOrigin'));
    const candidate = params.get('channel') || '';
    if (parent !== window && params.get('embed') === '1' &&
        ['http:', 'https:'].includes(origin.protocol) &&
        origin.origin === params.get('parentOrigin') && /^[a-zA-Z0-9_-]{16,128}$/.test(candidate)) {
      target = origin.origin;
      channel = candidate;
    }
  } catch (_) { /* Invalid or missing embedding options keep standalone behavior. */ }
  function send(type) {
    if (target) parent.postMessage({ source: 'dsh-startup', channel, type }, target);
  }
  globalThis.DSHEmbed = Object.freeze({
    get embedded() { return target !== null; },
    ready() { send('ready'); },
    // The parent rechecks its own backend cookie before opening the workbench.
    unlocked() { if (!finished) send('unlocked'); },
    finish(reason) {
      if (!target || finished || !['complete', 'skip'].includes(reason)) return false;
      finished = true;
      send(reason);
      return true;
    },
  });
})();
