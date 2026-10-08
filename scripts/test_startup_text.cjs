// Execute the same lettering module used by DOM and Canvas. Assertions protect
// literal local data, stable layout, replay/seek and reduced-motion behavior.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
function node(value = '') {
  return {
    _text: value, children: [], attributes: new Map(), classList: { add() {} },
    style: { setProperty(key, value) { this[key] = value; } },
    get textContent() { return this._text + this.children.map(x => x.textContent).join(''); },
    set textContent(value) { this._text = String(value); this.children = []; },
    replaceChildren(...children) { this._text = ''; this.children = children; },
    setAttribute(key, value) { this.attributes.set(key, value); },
  };
}
const sandbox = { document: { createElement: () => node() } };
vm.createContext(sandbox);
vm.runInContext(fs.readFileSync(path.join(__dirname, '../docs/startup-text.js'), 'utf8'), sandbox);
const { sample, View, Stage, characters } = sandbox.DSHText;
const literal = 'CatShark 深潜 👩🏽‍💻 <plugin>&';
assert.equal(characters('👩🏽‍💻').length, 1, 'animation must keep grapheme clusters intact');
for (const kind of ['roll', 'mask', 'type', 'decode']) {
  for (const age of [0, .15, .4, .82, 2, -10, Infinity, NaN]) {
    const frame = sample(literal, age, { kind });
    for (const field of ['offset', 'reveal', 'cover', 'blur']) assert.ok(Number.isFinite(frame[field]));
    assert.equal(JSON.stringify(frame), JSON.stringify(sample(literal, age, { kind })), 'seek must be deterministic');
  }
  assert.equal(sample(literal, 2, { kind }).text, literal);
  const still = sample(literal, 0, { kind, delay: 30, reducedMotion: true });
  assert.equal(still.text, literal); assert.equal(still.settled, true);
  assert.equal(still.offset, 0); assert.equal(still.blur, 0); assert.equal(still.cover, 0);
}
const label = node(literal), view = new View(label, { kind: 'mask' });
view.render(.2, false);
assert.equal(label.textContent, literal, 'real data must remain accessible during its visual reveal');
assert.equal(label.style.transform, undefined, 'do not transform the measured host/card/row');
assert.notEqual(label.style['--dsh-ink-cover'], '0.00000');
view.render(1, false); assert.equal(label.style['--dsh-ink-cover'], '0.00000');
view.render(.2, true); assert.equal(view.ink.style.clipPath, 'none');
const stage = new Stage({ querySelectorAll: () => [] });
stage.begin(3, 0, 4, false); stage.set(label, '…', { kind: 'roll' }); stage.render();
assert.equal(label.textContent, '…', 'unknown inventory remains unknown during loading');
stage.begin(3, .7, 4.7, false); stage.set(label, '01'); stage.render();
assert.equal(label.textContent, '01', 'reels must never invent intermediate inventory counts');
stage.begin(3, 1.3, 5.3, false); stage.render();
assert.equal(label.attributes.get('data-lettering'), 'settled');
stage.begin(4, 0, 5.4, false); stage.render();
assert.equal(label.attributes.get('data-lettering'), 'roll', 'a new phase should restart visible lettering');
stage.begin(4, 0, 5.4, true); stage.render();
assert.equal(label.attributes.get('data-lettering'), 'settled');
stage.begin(2, 1.5, 5.4, false, true); stage.set(label, '访问身份确认'); stage.render();
assert.equal(label.attributes.get('data-lettering'), 'settled', 'seeking into a phase must show its actual timeline pose, not freeze a new title at age zero');
stage.begin(2, .2, 5.4, false, true); stage.render();
assert.equal(label.attributes.get('data-lettering'), 'roll', 'seeking backward must restore the earlier reveal pose');
stage.remove(label); assert.equal(stage.views.size, 0, 'removed inventory rows must release their animation state');
console.log('PASS: deterministic rolling/type/mask/decode lettering; real accessible data, fixed hosts, graphemes, seek, reduced motion.');
