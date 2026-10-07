// Rebuild native contours and standalone SVG from the browser's authored polygons.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.resolve(__dirname, '..');
const scope = { window: {} };
vm.runInNewContext(fs.readFileSync(path.join(root, 'docs/startup-emblem.js'), 'utf8'), scope);
const emblem = scope.window.DSHEmblem;
if (emblem.parts.length !== 5 || emblem.parts.some(part => part.some(contour =>
  contour.length < 3 || contour.some(point => point.length !== 2 || point.some(n => !Number.isFinite(n)))))) {
  throw new Error('The seal must contain five valid, finite compound contours.');
}
fs.writeFileSync(path.join(root, 'crates/dsh-tui/assets/dsh-emblem.json'), JSON.stringify(emblem, null, 2) + '\n', 'utf8');
const paths = emblem.parts.map(contours => '<path fill-rule="evenodd" d="' + contours.map(points =>
  'M' + points.map(point => point.join(',')).join('L') + 'Z').join('') + '"/>').join('\n');
fs.writeFileSync(path.join(root, 'docs/assets/dsh-emblem.svg'),
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="-118 -115 236 220" role="img" aria-labelledby="title desc">\n' +
  '<title id="title">DSH — Delta Circuit</title>\n' +
  '<desc id="desc">Original triangular department seal. The left D, lower S and right H tracks form its silhouette, with an engraved DSH signature and three scan strata inside.</desc>\n' +
  '<g fill="currentColor">\n' + paths + '\n</g></svg>\n', 'utf8');
console.log('Exported matching native contours and SVG for ' + emblem.name + '.');
