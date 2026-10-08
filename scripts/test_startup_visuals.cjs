// Exercise the shipped renderer, not a duplicate animation or DOM-only stub.
// This validates drawing commands; browser screenshots still verify the design.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createHash } = require('node:crypto');

const sandbox = { window: {} };
vm.createContext(sandbox);
for (const name of ['startup-emblem.js', 'startup-text.js', 'startup-visuals.js']) {
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../docs', name), 'utf8'), sandbox, { filename: name });
}
const { draw, phaseCount } = sandbox.window.DSHVisuals;
assert.equal(phaseCount, 6);

function finite(values, operation) {
  for (const value of values) assert.ok(Number.isFinite(value), `${operation} received ${value}`);
}

function recordingContext() {
  const records = [], stack = [];
  let matrix = [1, 0, 0, 1, 0, 0], gradientId = 0;
  const state = {
    globalAlpha: 0.73, globalCompositeOperation: 'source-over', fillStyle: '#123456',
    strokeStyle: '#654321', lineWidth: 2, lineCap: 'round', lineJoin: 'round',
    font: '12px sans-serif', textAlign: 'left', textBaseline: 'alphabetic', lineDash: [2, 3],
  };
  const initial = JSON.stringify(state);
  function point(x, y) {
    return [matrix[0] * x + matrix[2] * y + matrix[4], matrix[1] * x + matrix[3] * y + matrix[5]];
  }
  function record(op, args = [], extra = {}) {
    records.push({ op, args, matrix: [...matrix], ...state, ...extra });
  }
  function multiply(next) {
    finite(next, 'transform');
    const [a, b, c, d, e, f] = matrix, [aa, bb, cc, dd, ee, ff] = next;
    matrix = [a * aa + c * bb, b * aa + d * bb, a * cc + c * dd, b * cc + d * dd, a * ee + c * ff + e, b * ee + d * ff + f];
    finite(matrix, 'composed transform');
  }
  const context = {
    save() { stack.push({ state: { ...state }, matrix: [...matrix] }); },
    restore() {
      assert.ok(stack.length, 'restore without matching save');
      const saved = stack.pop(); Object.assign(state, saved.state); matrix = saved.matrix;
    },
    translate(x, y) { multiply([1, 0, 0, 1, x, y]); },
    scale(x, y) { multiply([x, 0, 0, y, 0, 0]); },
    rotate(angle) { finite([angle], 'rotate'); multiply([Math.cos(angle), Math.sin(angle), -Math.sin(angle), Math.cos(angle), 0, 0]); },
    transform(...values) { multiply(values); },
    setLineDash(values) { finite(values, 'line dash'); state.lineDash = [...values]; },
    measureText(value) {
      const size = Number(state.font.match(/([\d.]+)px/)?.[1] || 12);
      return { width: [...String(value)].reduce((sum, char) => sum + size * (char.charCodeAt(0) > 255 ? 1 : 0.58), 0) };
    },
    fillText(value, x, y) { finite([x, y], 'fillText'); record('fillText', [String(value), x, y]); },
  };
  for (const property of Object.keys(state).filter(key => key !== 'lineDash')) {
    Object.defineProperty(context, property, {
      get: () => state[property],
      set(value) {
        if (typeof value === 'number') finite([value], property);
        if (property === 'globalAlpha') assert.ok(value >= 0 && value <= 1, 'alpha must be valid');
        if (property === 'lineWidth') assert.ok(value > 0, 'stroke width must be positive');
        state[property] = value;
      },
    });
  }
  for (const operation of ['beginPath', 'closePath', 'clip', 'fill', 'stroke', 'moveTo', 'lineTo', 'arc', 'rect', 'fillRect', 'strokeRect']) {
    context[operation] = (...args) => {
      finite(args.filter(value => typeof value === 'number'), operation);
      if (operation === 'arc') assert.ok(args[2] >= 0, 'arc radius must be nonnegative');
      const bounds = operation.endsWith('Rect') ? [point(args[0], args[1]), point(args[0] + args[2], args[1]), point(args[0] + args[2], args[1] + args[3]), point(args[0], args[1] + args[3])] : undefined;
      record(operation, args, bounds ? { bounds } : {});
    };
  }
  for (const operation of ['createLinearGradient', 'createRadialGradient']) {
    context[operation] = (...args) => {
      finite(args, operation);
      if (operation === 'createRadialGradient') assert.ok(args[2] >= 0 && args[5] >= 0, 'gradient radii must be valid');
      const gradient = { gradient: ++gradientId, type: operation, args, stops: [] };
      return Object.assign(gradient, {
        addColorStop(offset, color) {
          assert.ok(Number.isFinite(offset) && offset >= 0 && offset <= 1, 'gradient stop must be valid');
          gradient.stops.push([offset, color]);
        },
      });
    };
  }
  return {
    context, records,
    finish() {
      assert.equal(stack.length, 0, 'renderer leaked saved canvas state');
      assert.deepEqual(matrix, [1, 0, 0, 1, 0, 0], 'renderer leaked its transform');
      assert.equal(JSON.stringify(state), initial, 'renderer changed caller styles');
      return createHash('sha256').update(JSON.stringify(records)).digest('hex');
    },
  };
}

function render(width, height, state) {
  const recorder = recordingContext();
  draw(recorder.context, width, height, state);
  return { hash: recorder.finish(), records: recorder.records };
}

const inventory = { status: 'complete', skills: [{ name: 'real-review' }], plugins: [{ name: 'real-plugin' }], issues: [] };
const base = { identity: 'CatShark / 星海', inventory, choice: 1, pointer: { x: 0.83, y: 0.17 }, drag: 0.7, impulse: 0.35 };
const samples = [0, 0.001, 0.035, 0.16, 0.31, 0.42, 0.58, 0.82, 0.999, 1];
let rendered = 0;
for (const [width, height] of [[1920, 1080], [390, 844]]) {
  for (const light of [true, false]) {
    for (let phase = 0; phase < phaseCount; phase++) {
      for (const progress of samples) {
        const state = { ...base, phase, progress, light, time: phase * 2.3 + progress, ambientTime: 13.7 + progress };
        const first = render(width, height, state), second = render(width, height, state);
        assert.equal(first.hash, second.hash, `nondeterministic draw: ${width}×${height}, phase ${phase}, progress ${progress}`);
        assert.ok(first.records.some(record => record.op === 'fillText'), 'every scene retains its typography');
        rendered++;
      }
      const still = render(width, height, { ...base, phase, light, reducedMotion: true, progress: 0, time: 0, ambientTime: 0 });
      const later = render(width, height, { ...base, phase, light, reducedMotion: true, progress: 1, time: 99, ambientTime: 200, drag: -14, pointer: [0, 1], impulse: 1 });
      assert.equal(still.hash, later.hash, `reduced motion changed over time or input in phase ${phase}`);
    }
  }
}

// Readability gates must not leave any large opaque editorial cut across the
// central text corridor. Vignettes and the initial canvas background are exempt.
for (const [width, height] of [[1920, 1080], [390, 844]]) {
  for (const phase of [0, 2, 4]) {
    const { records } = render(width, height, { ...base, phase, progress: 0, waiting: true, impulse: 0, light: true });
    const corridor = [width * 0.3, height * 0.25, width * 0.7, height * 0.8];
    for (const record of records.slice(1)) {
      if (record.op !== 'fillRect' || typeof record.fillStyle !== 'string' || record.globalAlpha < 0.9) continue;
      // The very first solid rectangle clears the canvas, regardless of preceding state commands.
      if (record.args.join(',') === `0,0,${width},${height}`) continue;
      const xs = record.bounds.map(point => point[0]), ys = record.bounds.map(point => point[1]);
      const overlap = Math.max(0, Math.min(Math.max(...xs), corridor[2]) - Math.max(Math.min(...xs), corridor[0])) *
        Math.max(0, Math.min(Math.max(...ys), corridor[3]) - Math.max(Math.min(...ys), corridor[1]));
      const area = (corridor[2] - corridor[0]) * (corridor[3] - corridor[1]);
      assert.ok(overlap / area < 0.2, `phase ${phase} gate left a large opaque cut over the reading corridor`);
    }
  }
}

const resultText = status => render(1920, 1080, { ...base, phase: 4, progress: 1, reducedMotion: true, inventory: { ...inventory, status } }).records.filter(record => record.op === 'fillText').map(record => record.args[0]).join('');
assert.ok(resultText('complete').includes('RESULT / LOCAL'));
for (const status of ['idle', 'loading', 'error', 'unavailable']) {
  assert.ok(resultText(status).includes('RESULT / NOT READ'), `renderer claimed a local result for ${status}`);
  assert.ok(!resultText(status).includes('RESULT / LOCAL'));
}

// Host layout measurements change with zoom, CJK wrapping and the mobile
// single-column breakpoint. Registration ticks must follow actual row centers.
function screenPoint(record) {
  const [x, y] = record.args, [a, b, c, d, e, f] = record.matrix;
  return [a * x + c * y + e, b * x + d * y + f];
}
for (const [width, height, box] of [
  [1920, 1080, { x: 705, y: 554, width: 510, height: 171 }],
  [1280, 720, { x: 430, y: 350, width: 420, height: 203 }],
  [390, 844, { x: 31, y: 354, width: 328, height: 222 }],
]) {
  const rows = [0.25, 0.52, 0.82].map(offset => ({ y: box.y + box.height * offset - 14, height: 28 }));
  const state = { ...base, phase: 2, progress: 0, waiting: true, reducedMotion: true, layout: { identity: { ...box, rows } } };
  const { records } = render(width, height, state);
  for (const row of rows) {
    const center = row.y + row.height / 2;
    const aligned = records.filter(record => record.op === 'lineTo' && Math.abs(screenPoint(record)[1] - center) < 1e-7);
    assert.ok(aligned.some(record => screenPoint(record)[0] < box.x), 'left registration tick must follow the measured identity row');
    assert.ok(aligned.some(record => screenPoint(record)[0] > box.x + box.width), 'right registration tick must follow the measured identity row');
  }
  for (const [x,y] of [[box.x,box.y],[box.x+box.width,box.y],[box.x,box.y+box.height],[box.x+box.width,box.y+box.height]]) {
    assert.ok(records.some(record=>record.op==='moveTo'&&record.strokeStyle==='#29848e'&&Math.hypot(screenPoint(record)[0]-x,screenPoint(record)[1]-y)<1e-7), 'accent brackets must start on the actual measured card corners');
  }
  const first = render(width, height, { ...state, phase: 3, layout: { inventory: box } });
  const later = render(width, height, { ...state, phase: 3, time: 99, ambientTime: 99, layout: { inventory: box } });
  assert.equal(first.hash, later.hash, 'measured inventory decorations must respect reduced motion');
  const centerX = width / 2;
  const verticalCenterRule = first.records.some((record, index) => {
    const previous = first.records[index - 1];
    if (record.op !== 'lineTo' || previous?.op !== 'moveTo') return false;
    const from = screenPoint(previous), to = screenPoint(record);
    return Math.abs(from[0] - centerX) < 1e-7 && Math.abs(to[0] - centerX) < 1e-7 && Math.abs(to[1] - from[1]) > box.height / 2;
  });
  assert.equal(verticalCenterRule, false, 'the canvas must not duplicate the DOM inventory column divider');
}

// The outline must enclose the actual authored seal, including its bottom
// track, as pointer parallax and breathing scale change. Compare real drawn
// polygons with real guide vertices rather than a second layout formula.
for (const [width,height] of [[1920,1080],[947,730],[390,844]]) {
  for (const ambientTime of [0,1.2,4]) {
    const {records}=render(width,height,{...base,phase:0,waiting:true,ambientTime,textAge:2});
    const vertices=records.filter(r=>r.op==='arc'&&r.args[2]===2.5).map(screenPoint);
    assert.equal(vertices.length,3);
    const seal=[];let path=[];
    for(const record of records){
      if(record.op==='beginPath')path=[];
      if(record.op==='moveTo'||record.op==='lineTo')path.push(screenPoint(record));
      if(record.op==='fill'&&record.args[0]==='evenodd')seal.push(...path);
    }
    assert.ok(seal.length>70,'test must measure the real seal contours');
    const gaps=vertices.map((a,i)=>{
      const b=vertices[(i+1)%3],dx=b[0]-a[0],dy=b[1]-a[1];
      return Math.min(...seal.map(point=>(dx*(point[1]-a[1])-dy*(point[0]-a[0]))/Math.hypot(dx,dy)));
    });
    assert.ok(Math.min(...gaps)>0,'guide crossed into the seal');
    assert.ok(Math.max(...gaps)-Math.min(...gaps)<.01,'triangle guide edge gaps must remain equal');
  }
}
for (const phase of [2, 3]) {
  const state = { ...base, phase, progress: 0.5, reducedMotion: true };
  const baseline = render(1280, 720, state);
  for (const box of [{ x: NaN, y: 10, width: 10, height: 10 }, { x: 0, y: 0, width: 0, height: 30 }, { x: 0, y: 0, width: 100, height: Infinity }]) {
    assert.equal(render(1280, 720, { ...state, layout: { identity: box, inventory: box } }).hash, baseline.hash, 'unavailable measurements must use the composed fallback');
  }
}
for (const [width, height] of [[0, 100], [100, 0], [-1, 100]]) {
  assert.equal(render(width, height, base).records.length, 0, 'hidden or empty canvas must not draw');
}
draw(null, 1920, 1080, base);
console.log(`PASS: ${rendered} deterministic real-renderer samples; finite canvas geometry, balanced state, reduced-motion stability, readable gates, truthful inventory status, and measured layout alignment.`);
