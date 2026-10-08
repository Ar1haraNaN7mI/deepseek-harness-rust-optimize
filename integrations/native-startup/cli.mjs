#!/usr/bin/env node
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chooseNext, profileInput, saveSettings, settings } from './state.mjs';

export function prepareArguments(input, patch) {
  const args = [...input];
  const explicitOn = args.includes('--startup'), explicitOff = args.includes('--no-startup');
  if (explicitOn && explicitOff) throw new Error('--startup and --no-startup are mutually exclusive.');
  const forwarded = args.filter(value => value !== '--startup' && value !== '--no-startup');
  const web = forwarded[0] === 'web' || (forwarded[0] === '--profile' && forwarded[1] === 'web');
  const inspect = forwarded.some(value => ['--help', '-h', '--dump-config', '--dump-default-config', '--dump-config-schema'].includes(value));
  if (web && !inspect) forwarded.splice(forwarded[0] === 'web' ? 1 : 2, 0, '--patch', patch);
  return { args: forwarded, override: explicitOn ? true : explicitOff ? false : null, web: web && !inspect };
}
export async function main(input = process.argv.slice(2)) {
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (!((major === 22 && minor >= 19) || major >= 24)) throw new Error(`Official DSH requires Node.js 22.19+ (22.x) or 24+; current runtime is ${process.versions.node}.`);
  if (input[0] === 'startup') {
    const [_, action, ...rest] = input;
    if (action === 'next' || action === 'enabled') {
      if (rest.length !== 1 || !['on', 'off'].includes(rest[0])) throw new Error(`Usage: dsh-native startup ${action} on|off`);
      const enabled = rest[0] === 'on';
      if (action === 'next') chooseNext(enabled); else saveSettings({ enabled });
      console.log(`Native DSH startup ${action}: ${enabled ? 'on' : 'off'}`);
      return;
    }
    if (action === 'profile') {
      const current = settings();
      if (!rest.length) { console.log(JSON.stringify(current, null, 2)); return; }
      const values = { username: current.username, badge_id: current.badge_id };
      for (let i = 0; i < rest.length; i += 2) {
        if (!['--name', '--badge'].includes(rest[i]) || !rest[i + 1]) throw new Error('Usage: dsh-native startup profile [--name NAME] [--badge ID]');
        values[rest[i] === '--name' ? 'username' : 'badge_id'] = rest[i + 1];
      }
      saveSettings(profileInput(values)); console.log('Native startup profile saved.'); return;
    }
    throw new Error('Usage: dsh-native startup enabled|next on|off; dsh-native startup profile [--name NAME] [--badge ID]');
  }
  if (!input.length || input[0] === '--help') {
    console.log('Official DeepSeek Harness with optional startup animation\n\n  dsh-native web --startup\n  dsh-native web --no-startup\n  dsh-native startup enabled on|off\n  dsh-native startup next on|off\n  dsh-native startup profile --name NAME\n\nOther arguments are forwarded to official DSH. Use dsh-native web --help for its options.');
    return;
  }
  const temp = mkdtempSync(join(tmpdir(), 'dsh-native-startup-'));
  const patch = join(temp, 'startup.patch.json');
  const cleanup = () => rmSync(temp, { recursive: true, force: true });
  process.once('exit', cleanup);
  try {
    const invocation = prepareArguments(input, patch);
    // JSON is valid YAML. Use a file URL so spaces/non-ASCII paths survive the
    // official Loader's module resolver without shell interpolation.
    writeFileSync(patch, JSON.stringify([{ insert: [{ id: 'dsh-original-startup', name: new URL('./plugin.mjs', import.meta.url).href, config: { override: invocation.override } }] }]), 'utf8');
    process.argv = [process.execPath, fileURLToPath(import.meta.url), ...invocation.args];
    const { runCli } = await import('@deepseek-ai/dsh/lib/bin.js');
    await runCli();
  } catch (error) { cleanup(); throw error; }
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch(error => { console.error(`dsh-native: ${error.message}`); process.exitCode = 1; });
}
