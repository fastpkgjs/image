// Child process for tests/robustness.test.mjs. Runs risky decodes outside the test runner so a
// crash / hang / runaway allocation only takes down this process, and reports what happened.
//   node _robust-child.mjs fuzz <base-file> <seed> <flips>
//   node _robust-child.mjs file <input-file>
//   node _robust-child.mjs rss <iterations>
// Protocol on stdout: "S <id>" before a case, "R <id> <ok|rej|nonerror>" after, "DONE <rssGrowthBytes>".
import fs from 'node:fs';
import engine from '../packages/image/index.js';
import { fuzzInputs } from './_util.mjs';

const out = (line) => fs.writeSync(1, `${line}\n`);
const MEM_LIMIT = Number(process.env.ROBUST_MEM_LIMIT || 1.5e9);
const OP_TIMEOUT = Number(process.env.ROBUST_OP_TIMEOUT || 10000);
const baseline = process.memoryUsage().rss;
let maxRss = baseline;

setInterval(() => {
  const rss = process.memoryUsage().rss;
  if (rss > maxRss) maxRss = rss;
  if (rss - baseline > MEM_LIMIT) {
    out(`MEM ${rss - baseline}`);
    process.exit(97);
  }
}, 20).unref();
process.on('unhandledRejection', (e) => { out(`UNHANDLED ${String(e && e.message).slice(0, 100)}`); process.exit(99); });
process.on('uncaughtException', (e) => { out(`UNCAUGHT ${String(e && e.message).slice(0, 100)}`); process.exit(99); });

function settle(promise, what) {
  return new Promise((resolve) => {
    const timer = setTimeout(() => { out(`HANG ${what}`); process.exit(98); }, OP_TIMEOUT);
    promise.then(
      () => { clearTimeout(timer); resolve('ok'); },
      (e) => { clearTimeout(timer); resolve(e instanceof Error ? 'rej' : 'nonerror'); },
    );
  });
}

async function runOne(id, buf) {
  out(`S ${id}`);
  const results = [];
  const ops = [
    ['metadata', () => engine(buf).metadata()],
    ['pipeline', () => engine(buf).resize(16, 16, { fit: 'inside' }).raw().toBuffer()],
    ['stats', () => engine(buf).stats()],
  ];
  for (const [name, fn] of ops) {
    let p;
    try { p = fn(); } catch (e) { results.push(e instanceof Error ? 'rej' : 'nonerror'); continue; }
    results.push(await settle(p, `${id}:${name}`));
  }
  out(`R ${id} ${results.join(',')}`);
}

const mode = process.argv[2];
if (mode === 'fuzz') {
  const base = fs.readFileSync(process.argv[3]);
  const skip = new Set((process.env.ROBUST_SKIP || '').split(',').filter(Boolean));
  for (const c of fuzzInputs(base, Number(process.argv[4]), Number(process.argv[5]))) if (!skip.has(c.id)) await runOne(c.id, c.buf);
} else if (mode === 'file') {
  await runOne('file', fs.readFileSync(process.argv[3]));
} else if (mode === 'rss') {
  const n = Number(process.argv[3]);
  const src = fs.readFileSync(process.argv[4]);
  const one = async (i) => {
    const kind = i % 4;
    const p = engine(src).resize(24, 18, { fit: 'cover' });
    if (kind === 0) await p.jpeg().toBuffer();
    else if (kind === 1) await p.png().toBuffer();
    else if (kind === 2) await p.webp().toBuffer();
    else await p.rotate(90).blur(1).raw().toBuffer();
  };
  const warm = Math.floor(n / 10);
  for (let i = 0; i < warm; i++) await one(i);
  if (global.gc) global.gc();
  const afterWarm = process.memoryUsage().rss;
  for (let i = warm; i < n; i++) await one(i);
  if (global.gc) global.gc();
  await new Promise((r) => setTimeout(r, 100));
  const end = process.memoryUsage().rss;
  out(`RSS warm=${afterWarm} end=${end} growth=${end - afterWarm} peak=${maxRss}`);
}
out(`DONE ${maxRss - baseline}`);
process.exit(0);
