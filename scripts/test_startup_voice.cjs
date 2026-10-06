const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

async function flush() { for (let i = 0; i < 12; i++) await Promise.resolve(); }
async function run() {
  const fetched = [], players = [];
  class AudioContext {
    constructor() { this.state = 'running'; this.destination = {}; }
    async resume() { this.state = 'running'; }
    async decodeAudioData() { return {}; }
    createBufferSource() {
      const player = { connect() {}, disconnect() {}, start() {}, stop() { this.stopped = true; this.onended?.(); } };
      players.push(player); return player;
    }
  }
  const sandbox = {
    AudioContext, AbortController, setTimeout, clearTimeout, addEventListener() {},
    async fetch(url, options) {
      assert.equal(options.body, undefined, 'only fetch pre-generated audio; never send dynamic text');
      fetched.push(url);
      return { ok: true, arrayBuffer: async () => new ArrayBuffer(8) };
    },
  };
  vm.createContext(sandbox);
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../docs/startup-voice.js'), 'utf8'), sandbox);
  const voice = sandbox.DSHVoice;
  voice.phase(2, { username: 'CatShark' });
  assert.equal(voice.busy, false, 'no narration before a user gesture');
  await voice.unlock();
  voice.phase(2, { username: 'CatShark' });
  assert.equal(voice.busy, true);
  await flush();
  assert.equal(fetched.at(-1), '/assets/voice/phase-2.wav');
  players.at(-1).onended(); await flush(); assert.equal(voice.busy, false);
  voice.phase(4, { inventory: { status: 'complete', skills: [{ name: 'secret-name' }], plugins: [{ name: 'private-plugin' }, { name: 'another' }], issues: [] } });
  await flush();
  assert.equal(fetched.at(-1), '/assets/voice/phase-4.wav', 'same clip regardless of names/counts');
  voice.cancel(); assert.equal(voice.busy, false); assert.equal(players.at(-1).stopped, true);
  voice.phase(4, { inventory: { status: 'error', skills: [], plugins: [] } });
  await flush(); assert.equal(fetched.at(-1), '/assets/voice/load-unavailable.wav');
  voice.setMuted(true); assert.equal(voice.busy, false);
  const before = fetched.length; voice.phase(5, { username: 'CatShark' }); await flush(); assert.equal(fetched.length, before);
  voice.setMuted(false); voice.phase(5, { username: 'CatShark' }); voice.cancel(); await flush();
  assert.equal(voice.busy, false, 'cancelled async decode never resumes playback');
  voice.phase(4, { inventory: { status: 'complete', issues: [{}] } }); await flush();
  assert.equal(fetched.at(-1), '/assets/voice/load-warning.wav'); voice.cancel();
  voice.phase(3, { inventoryMode: 'mounted' }); await flush();
  assert.equal(fetched.at(-1), '/assets/voice/phase-3-mounted.wav', 'an already booted Harness reviews mounted resources'); voice.cancel();
  voice.phase(3, { inventoryMode: 'discover' }); await flush();
  assert.equal(fetched.at(-1), '/assets/voice/phase-3.wav'); voice.cancel();
  console.log('PASS fixed narration: gesture, fixed assets, no personal text, result-dependent clips, mute, cancellation');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
