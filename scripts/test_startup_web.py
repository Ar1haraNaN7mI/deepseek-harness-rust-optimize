"""Exercise the built loopback host against isolated fixtures, never user settings."""
import argparse
import io
import json
import re
import subprocess
import tempfile
import urllib.error
import urllib.request
import wave
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', default='target/debug/dsh.exe')
    parser.add_argument('--voice', action='store_true')
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix='dsh-web-test-') as temporary:
        root = Path(temporary)
        outer = root / 'home'
        configuration = Path('config/default.toml').read_text(encoding='utf-8')
        configuration = re.sub(r'^outer_home\s*=.*$', 'outer_home = ' + json.dumps(outer.as_posix()), configuration, flags=re.M)
        config = root / 'config.toml'
        config.write_text(configuration, encoding='utf-8')
        skill = root / '.dsh-rust/skills/fixture-skill/SKILL.md'
        skill.parent.mkdir(parents=True)
        skill.write_text('---\nname: fixture-skill\ndescription: real local test\n---\nFixture instruction.', encoding='utf-8')
        plugin = root / 'outer/plugins/fixture-plugin'
        plugin.mkdir(parents=True)
        (plugin / 'plugin.json').write_text(json.dumps({'id': 'fixture-plugin', 'name': 'Fixture Plugin', 'version': '1.0.0', 'tools': []}), encoding='utf-8')
        broken = root / 'outer/plugins/broken-plugin'
        broken.mkdir()
        (broken / 'plugin.json').write_text('{ broken', encoding='utf-8')
        command = [binary, '--workspace', str(root), '--config', str(config), 'startup']
        process = subprocess.Popen(command + ['web', '--port', '0', '--silent'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, encoding='utf-8')
        try:
            line = process.stdout.readline()
            match = re.search(r'(http://127\.0\.0\.1:\d+)', line)
            assert match, f'Host failed to start: {line}'
            base = match[1]

            def request(path, body=None, token=None, headers=None):
                h = dict(headers or {})
                if body is not None:
                    h['Content-Type'] = 'application/json'
                if token:
                    h['X-DSH-Token'] = token
                data = None if body is None else json.dumps(body).encode('utf-8')
                return urllib.request.urlopen(urllib.request.Request(base + path, data=data, headers=h), timeout=30)

            profile = json.load(request('/api/profile'))
            token = profile['token']
            assert profile['mode'] == 'local'
            assert profile['sound'] is False, 'global --silent must apply to the web preview'
            assert b'startup-local.js' in request('/startup-preview.html').read()
            assert b'DSHVoice' in request('/startup-voice.js').read()
            for asset in ['startup-preview.html', 'startup-preview.js', 'startup-local.js', 'startup-voice.js', 'startup-sequence.js', 'startup-identity.js', 'startup-visuals.js', 'startup-embed.js']:
                assert request('/' + asset).read() == (Path('docs') / asset).read_bytes(), f'Embedded asset is stale: {asset}'
            font = request('/assets/fonts/dsh-industrial-sc.woff2?v=1')
            assert font.headers['Content-Type'] == 'font/woff2'
            assert font.read() == Path('docs/assets/fonts/dsh-industrial-sc.woff2').read_bytes()
            assert b'SIL OPEN FONT LICENSE' in request('/assets/fonts/OFL-NotoSansSC.txt').read()
            for path, body, headers in [('/api/profile', {'username': 'bad'}, {}), ('/api/profile', None, {'Origin': 'https://example.com'})]:
                try:
                    request(path, body, headers=headers)
                    raise AssertionError('Cross-origin/tokenless request unexpectedly accepted')
                except urllib.error.HTTPError as error:
                    assert error.code == 403
            saved = json.load(request('/api/profile', {'username': 'CatShark', 'badge_id': 'DSH-QA'}, token))
            assert saved['username'] == 'CatShark'
            again = json.loads(subprocess.check_output(command + ['profile'], encoding='utf-8'))
            assert again == saved, 'Web and CLI must share persistent identity'
            try:
                request('/api/profile', {'username': '\x1b[31m', 'badge_id': 'DSH-QA'}, token)
                raise AssertionError('Control characters accepted')
            except urllib.error.HTTPError as error:
                assert error.code == 400
            events = [json.loads(line) for line in request('/api/load', {}, token).read().splitlines()]
            assert events[0] == {'type': 'stage', 'stage': 'skills', 'status': 'loading'}
            result = events[-1]
            assert result['type'] == 'complete'
            assert any(s['name'] == 'fixture-skill' for s in result['skills'])
            assert any(p['id'] == 'fixture-plugin' for p in result['plugins'])
            assert any(i['kind'] == 'plugin' and i['name'] == 'broken-plugin' for i in result['issues'])
            assert isinstance(result['elapsed_ms'], int)
            assert not (outer / 'startup-next.txt').exists(), 'Preview must not affect one-shot preference'
            assert not (outer / 'sessions').exists(), 'Preview must not boot a session'
            if args.voice:
                wav = request('/assets/voice/phase-5.wav').read()
                with wave.open(io.BytesIO(wav)) as audio:
                    duration = audio.getnframes() / audio.getframerate()
                    assert .5 < duration < 20
                    assert len(set(audio.readframes(audio.getnframes()))) > 20
                print(f'PASS fixed English voice: {duration:.2f}s, non-silent WAV')
            print('PASS local host: real catalog fixtures, failures, profile persistence, origin/token checks, no session')
        finally:
            process.terminate()
            process.wait(timeout=10)


if __name__ == '__main__':
    main()
