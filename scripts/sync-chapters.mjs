#!/usr/bin/env node
// Embed shared chapter CSS/motion. Rendered templates remain self-contained.
import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const check = process.argv.includes('--check');
const templates = [
  'chapter-title-card',
  'talking-head-horizontal',
  'talking-head-vertical',
  'screen-camera-vertical',
];
const sources = [
  { name: 'style', file: 'chapter.css', indent: '          ' },
  { name: 'motion', file: 'chapter-motion.js', indent: '            ' },
];
let stale = false;
for (const id of templates) {
  const file = path.join(root, 'components/compositions', id + '.html');
  const original = await readFile(file, 'utf8');
  let updated = original;
  for (const source of sources) {
    const start = '/* chapter-' + source.name + ':start */';
    const end = '/* chapter-' + source.name + ':end */';
    const first = updated.indexOf(start);
    const last = updated.indexOf(end);
    if (first < 0 || last < first ||
        updated.indexOf(start, first + start.length) !== -1 ||
        updated.indexOf(end, last + end.length) !== -1) {
      throw new Error(id + ': missing or duplicate ' + source.name + ' markers');
    }
    const shared = (await readFile(path.join(root, 'components/chapter', source.file), 'utf8')).trimEnd();
    const body = shared.split('\n').map(line => source.indent + line).join('\n');
    updated = updated.slice(0, first + start.length) + '\n' + body +
      '\n' + source.indent + updated.slice(last);
  }
  if (updated !== original) {
    stale = true;
    if (check) console.error(id + ': shared chapter styling/motion is out of sync');
    else await writeFile(file, updated);
  }
}
if (check && stale) process.exitCode = 1;
else console.log(check ? 'All four chapter templates are in sync.' : 'Chapter templates synchronized.');
