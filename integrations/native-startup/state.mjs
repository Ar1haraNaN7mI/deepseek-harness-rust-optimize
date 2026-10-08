import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from 'node:fs';
import { homedir, userInfo } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';

export function dataDirectory() {
  const configured = process.env.DSH_HOME?.trim();
  const home = configured ? configured.replace(/^~(?=$|[\\/])/, homedir()) : join(homedir(), '.dsh');
  return join(resolve(home), 'startup-animation');
}
export function readJson(path, fallback) {
  try { return JSON.parse(readFileSync(path, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return fallback; throw error; }
}
export function writeJson(path, value) {
  mkdirSync(dataDirectory(), { recursive: true });
  const temporary = `${path}.${randomUUID()}.tmp`;
  try { writeFileSync(temporary, JSON.stringify(value, null, 2) + '\n', { encoding: 'utf8', mode: 0o600 }); renameSync(temporary, path); }
  finally { if (existsSync(temporary)) unlinkSync(temporary); }
}
export function settings() {
  const value = readJson(join(dataDirectory(), 'settings.json'), {});
  let username = 'OPERATOR';
  try { username = userInfo().username || username; } catch { /* No OS user database in some containers. */ }
  return { enabled: false, sound: true, username, badge_id: 'DSH-0001', ...value };
}
export function saveSettings(update) {
  const value = { ...settings(), ...update };
  writeJson(join(dataDirectory(), 'settings.json'), value);
  return value;
}
export function chooseNext(enabled) { writeJson(join(dataDirectory(), 'next.json'), { enabled }); }
// A rename claims a choice once across concurrent native processes. A preview,
// help/config dump, or failed boot never reaches this browser launch endpoint.
export function consumeNext() {
  const path = join(dataDirectory(), 'next.json'), claimed = `${path}.${randomUUID()}.claimed`;
  try { renameSync(path, claimed); } catch (error) { if (error.code === 'ENOENT') return undefined; throw error; }
  try { const value = readJson(claimed, {}); return typeof value.enabled === 'boolean' ? value.enabled : undefined; }
  finally { unlinkSync(claimed); }
}
export function profileInput(value) {
  const result = {};
  for (const key of ['username', 'badge_id']) {
    const text = value?.[key];
    if (typeof text !== 'string' || !text.trim() || text.length > (key === 'username' ? 64 : 40) || /[\x00-\x1f\x7f]/.test(text)) throw new Error('Username / badge must be nonempty printable text (64 / 40 characters maximum).');
    result[key] = text.trim();
  }
  return result;
}
