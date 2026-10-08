import { randomBytes } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, sep, extname } from 'node:path';
import { consumeNext, profileInput, saveSettings, settings } from './state.mjs';

export const name = 'dsh-original-startup';
export const inject = ['webServer', 'skills', 'loader', 'connection', 'agentPresets'];
export const PREFIX = '/__dsh_startup';
const assetRoot = fileURLToPath(new URL('./assets/', import.meta.url));
// Cordis publishes FiberState as an erased TypeScript const enum, not a JS
// export. These values are verified against the pinned official release.
const ACTIVE = 2, FAILED = 3;
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.svg': 'image/svg+xml', '.json': 'application/json', '.wav': 'audio/wav', '.mp3': 'audio/mpeg', '.woff2': 'font/woff2', '.txt': 'text/plain; charset=utf-8' };
const json = (res, status, value) => { res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' }); res.end(JSON.stringify(value)); };

export function localRequest(req, port, token, mutation = false) {
  const hosts = [`127.0.0.1:${port}`, `localhost:${port}`, `[::1]:${port}`];
  if (!hosts.includes(req.headers.host)) return false;
  if (req.socket?.remoteAddress && !['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(req.socket.remoteAddress)) return false;
  if (req.headers.origin && req.headers.origin !== `http://${req.headers.host}`) return false;
  return !mutation || (req.headers.origin === `http://${req.headers.host}` && req.headers['x-dsh-token'] === token);
}
async function body(req) {
  let content = '';
  for await (const chunk of req) { content += chunk; if (Buffer.byteLength(content) > 8192) throw new Error('Profile is too large.'); }
  return JSON.parse(content);
}
export async function inventory(ctx, emit, signal) {
  const started = performance.now(), skills = [], plugins = [], issues = [];
  emit({ type: 'stage', stage: 'skills', status: 'loading' });
  let lease;
  try {
    // Official Web's filesystem providers live in the selected default preset,
    // not the host layer. Borrow that actual revision without creating an Agent
    // or session, and release it after discovery.
    lease = await ctx.agentPresets?.acquireScope();
    const snapshot = await ctx.skills.snapshot({ cwd: process.cwd(), signal, ...(lease ? { scope: lease.key } : {}) });
    for (const skill of snapshot.skills) {
      const item = { name: skill.name, source: skill.path || `${skill.provider} / ${skill.source}`, status: 'loaded' };
      skills.push(item); emit({ type: 'item', kind: 'skill', ...item });
    }
    if (!snapshot.complete) issues.push({ kind: 'skill', name: 'catalog', message: 'The official registry reported an incomplete observation; retry after its providers settle.' });
  } catch (error) { issues.push({ kind: 'skill', name: 'catalog', message: error.message }); }
  finally { if (lease) await lease[Symbol.asyncDispose](); }
  emit({ type: 'stage', stage: 'skills', status: 'complete' });
  emit({ type: 'stage', stage: 'plugins', status: 'loading' });
  try {
    // These are live Loader fibers, not dependency manifests. Client-only rows,
    // disabled entries and pending service consumers are not reported as mounted.
    for (const entry of ctx.loader.entries()) {
      if (entry.options.group || entry.subtree || entry.subgroup || entry.disabled) continue;
      const label = entry.options.name || entry.id;
      if (entry.fiber?.state === ACTIVE) {
        const item = { id: entry.id, name: label, source: entry.id, status: 'loaded' };
        plugins.push(item); emit({ type: 'item', kind: 'plugin', ...item });
      } else if (entry.fiber?.state === FAILED) {
        const issue = { kind: 'plugin', name: label, message: 'The official Loader reports this plugin failed.' };
        issues.push(issue); emit({ type: 'item', ...issue, source: entry.id, status: 'error' });
      }
    }
    for (const preset of await ctx.agentPresets?.compositionInventory() ?? []) {
      if (!preset.isDefault) continue;
      if (preset.broken) issues.push({ kind: 'plugin', name: `preset:${preset.id}`, message: preset.broken });
      for (const row of preset.rows) {
        if (row.enabled !== true || row.fiberState !== ACTIVE) continue;
        const item = { id: `preset:${preset.id}/${row.entryId}`, name: row.moduleName, source: `preset:${preset.id}/${row.entryId}`, status: 'loaded' };
        plugins.push(item); emit({ type: 'item', kind: 'plugin', ...item });
      }
    }
  } catch (error) { issues.push({ kind: 'plugin', name: 'registry', message: error.message }); }
  emit({ type: 'stage', stage: 'plugins', status: 'complete' });
  const result = { type: 'complete', skills, plugins, issues, elapsed_ms: Math.round(performance.now() - started) };
  emit(result); return result;
}

export function apply(ctx, config = {}) {
  const token = randomBytes(32).toString('hex'), instance = randomBytes(16).toString('hex');
  let launchChoice;
  const register = route => ctx.effect(() => ctx.webServer.register(route), `startup ${route.path}`);
  register({ kind: 'prefix', path: PREFIX, async handler(req, res) {
    if (!localRequest(req, ctx.webServer.port, token)) return json(res, 403, { error: 'Startup is available only on this local machine.' });
    const path = new URL(req.url, `http://${req.headers.host}`).pathname.slice(PREFIX.length);
    if (path === '/launch' || path.startsWith('/api/')) {
      const rejection = ctx.connection.requestRejection(req);
      if (rejection !== undefined) return json(res, rejection, { error: 'Open the official DSH login URL before reading startup data.' });
    }
    try {
      if (path === '/launch' && req.method === 'GET') {
        if (req.headers['x-dsh-startup'] !== 'launch') return json(res, 403, { error: 'Launch must originate from the local startup overlay.' });
        if (launchChoice === undefined) {
          const next = consumeNext();
          launchChoice = typeof config.override === 'boolean' ? config.override : next ?? settings().enabled;
        }
        return json(res, 200, { enabled: launchChoice, instance });
      }
      if (path === '/api/profile') {
        if (req.method === 'POST') {
          if (!localRequest(req, ctx.webServer.port, token, true)) return json(res, 403, { error: 'Invalid local request.' });
          saveSettings(profileInput(await body(req)));
        } else if (req.method !== 'GET') return json(res, 405, { error: 'Method not allowed.' });
        const value = settings();
        return json(res, 200, { mode: 'local', token, username: value.username, badge_id: value.badge_id, sound: value.sound, workspace: process.cwd(), inventory_mode: 'mounted', runtime: 'native', runtime_label: 'NATIVE' });
      }
      if (path === '/api/load' && req.method === 'POST') {
        if (!localRequest(req, ctx.webServer.port, token, true)) return json(res, 403, { error: 'Invalid local request.' });
        res.writeHead(200, { 'Content-Type': 'application/x-ndjson; charset=utf-8', 'Cache-Control': 'no-store' });
        const controller = new AbortController();
        res.on('close', () => controller.abort());
        await inventory(ctx, event => { if (!res.destroyed) res.write(JSON.stringify(event) + '\n'); }, controller.signal);
        res.end(); return;
      }
      if (req.method !== 'GET' && req.method !== 'HEAD') return json(res, 405, { error: 'Method not allowed.' });
      let relative = decodeURIComponent(path).replace(/^\//, '');
      if (relative === 'overlay.js' || relative === 'bridge.js') {
        const script = await readFile(new URL(`./${relative.replace('.js', '.mjs')}`, import.meta.url));
        res.writeHead(200, { 'Content-Type': MIME['.js'], 'Cache-Control': 'no-store' }); return res.end(req.method === 'HEAD' ? undefined : script);
      }
      const absolute = resolve(assetRoot, relative);
      if (!absolute.startsWith(assetRoot + (assetRoot.endsWith(sep) ? '' : sep)) || !MIME[extname(absolute)]) return json(res, 404, { error: 'Asset not found.' });
      let content = await readFile(absolute);
      if (relative === 'startup-preview.html') {
        content = Buffer.from(content.toString('utf8').replace('<head>', `<head><script src="${PREFIX}/bridge.js"></script>`));
      }
      res.writeHead(200, { 'Content-Type': MIME[extname(absolute)], 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
      res.end(req.method === 'HEAD' ? undefined : content);
    } catch (error) {
      if (res.headersSent) { res.end(); return; }
      json(res, error.code === 'ENOENT' ? 404 : 400, { error: error.code === 'ENOENT' ? 'Asset not found.' : error.message });
    }
  } });
  ctx.on('webserver/index-inject', table => {
    table.push({ kind: 'script-src', placement: 'head', src: `${PREFIX}/overlay.js` });
  });
}
