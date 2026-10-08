/* Uses the shared animation's authenticated parent completion protocol. */
(() => {
  const start = async () => {
    try {
      const response = await fetch('/__dsh_startup/launch', { cache: 'no-store', headers: { 'X-DSH-Startup': 'launch' } });
      if (!response.ok) return;
      const { enabled, instance } = await response.json();
      const key = `dsh-original-startup:${instance}`;
      let seen = false;
      try { seen = Boolean(sessionStorage.getItem(key)); } catch { /* Storage may be disabled. */ }
      if (!enabled || seen) return;
      const channel = crypto.randomUUID().replaceAll('-', '');
      const frame = document.createElement('iframe');
      frame.title = 'DSH startup animation';
      frame.setAttribute('allow', 'autoplay');
      frame.style.cssText = 'position:fixed;inset:0;width:100%;height:100%;border:0;z-index:2147483647;background:#eeeae2';
      frame.src = '/__dsh_startup/startup-preview.html?' + new URLSearchParams({ embed: '1', parentOrigin: location.origin, channel });
      const previousFocus = document.activeElement, inert = new Map();
      const isolate = node => {
        if (node.nodeType !== 1 || node === frame || inert.has(node)) return;
        inert.set(node, node.inert); node.inert = true;
      };
      for (const node of document.body.children) isolate(node);
      const observer = new MutationObserver(changes => {
        for (const change of changes) for (const node of change.addedNodes) isolate(node);
      });
      observer.observe(document.body, { childList: true });
      const containFocus = event => { if (event.target !== frame) frame.focus(); };
      window.addEventListener('focusin', containFocus, true);
      let deadline;
      const dismiss = () => {
        clearTimeout(deadline);
        observer.disconnect();
        window.removeEventListener('focusin', containFocus, true);
        for (const [node, wasInert] of inert) node.inert = wasInert;
        frame.remove(); window.removeEventListener('message', close);
        window.removeEventListener('keydown', escape);
        if (previousFocus?.isConnected && !previousFocus.inert) previousFocus.focus?.();
        try { sessionStorage.setItem(key, 'done'); } catch { /* Never block returning to Harness. */ }
      };
      const escape = event => { if (event.key === 'Escape') dismiss(); };
      const close = event => {
        if (event.origin !== location.origin || event.source !== frame.contentWindow || event.data?.source !== 'dsh-startup' || event.data.channel !== channel) return;
        if (event.data.type === 'ready') clearTimeout(deadline);
        if (['complete', 'skip'].includes(event.data.type)) dismiss();
      };
      window.addEventListener('message', close);
      window.addEventListener('keydown', escape);
      frame.addEventListener('error', dismiss, { once: true });
      // Fail open to the official UI if a missing asset prevents the shared
      // animation from announcing readiness. A ready sequence has no timeout.
      deadline = setTimeout(dismiss, 15000);
      document.body.append(frame); frame.focus();
    } catch (error) { console.warn('DSH startup animation unavailable:', error.message); }
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start, { once: true }); else start();
})();
