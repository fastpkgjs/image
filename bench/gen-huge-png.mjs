// Deterministic big PNGs for the streaming-decode memory cases in mem.mjs:
//
//   node bench/gen-huge-png.mjs [out.png] [--size N | --width W --height H] [--depth 8|16]
//        [--color gray|graya|rgb|rgba] [--interlace] [--content ramp|noise]
//
// Defaults: bench/.cache/mem/huge-256mp.png, 16384x16384 8-bit RGB, non-interlaced, 'ramp'
// (a blocky colour ramp that deflates to well under 1 MB, like the flat-colour/gradient files
// that trigger the problem). 'noise' is incompressible. Written row by row through zlib, so no
// raw frame is ever held (see png-fixtures.mjs).
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { writePng } from './png-fixtures.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const flag = (name) => args.includes(`--${name}`);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};
const out = process.argv[2] && !process.argv[2].startsWith('--') ? process.argv[2] : path.join(here, '.cache', 'mem', 'huge-256mp.png');
const size = Number(opt('size', 16384));
const bytes = await writePng(out, {
  width: Number(opt('width', size)),
  height: Number(opt('height', size)),
  depth: Number(opt('depth', 8)),
  color: opt('color', 'rgb'),
  interlace: flag('interlace'),
  content: opt('content', 'ramp'),
});
console.log(out, bytes, 'bytes');
