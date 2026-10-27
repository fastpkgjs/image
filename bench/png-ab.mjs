// A/B of one PNG pipeline: this build, another fastpkg build (--base DIR) and sharp, each run in
// a fresh child process (peak-rss.mjs --child), alternating, median of --runs (default 3).
//
//   node bench/png-ab.mjs <file.png> [--pipeline resize-1200-jpeg] [--base /path/to/package-dir]
//        [--runs 3] [--threads N]    # N sets RAYON_NUM_THREADS for the fastpkg builds
//
// Prints wall ms, CPU ms (user+system) and peak RSS (VmHWM) per engine.
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? fallback : argv[i + 1];
};
const file = path.resolve(argv.find((a, i) => !a.startsWith('--') && (i === 0 || !argv[i - 1].startsWith('--'))));
const pipeline = opt('pipeline', 'rotate-1200-jpeg');
const runs = Number(opt('runs', 3));
const base = opt('base', null);
const threads = opt('threads', null);
const here = path.dirname(fileURLToPath(import.meta.url));
const engines = [['new', 'fastpkg', null], ...(base ? [['base', 'fastpkg', path.resolve(base)]] : []), ['sharp', 'sharp', null]];
const results = Object.fromEntries(engines.map(([name]) => [name, []]));
for (let i = 0; i < runs; i++) {
  for (const [name, engine, pkg] of engines) {
    const env = { ...process.env, ...(pkg ? { FASTPKG_PKG: pkg } : { FASTPKG_PKG: '' }), ...(threads && engine === 'fastpkg' ? { RAYON_NUM_THREADS: threads } : {}) };
    if (!pkg) delete env.FASTPKG_PKG;
    const run = spawnSync(process.execPath, [path.join(here, 'peak-rss.mjs'), '--child', '--engine', engine, '--file', file, '--pipeline', pipeline], { encoding: 'utf8', env, timeout: 300_000 });
    results[name].push(run.status === 0 ? JSON.parse(run.stdout) : { error: (run.stderr || 'failed').trim().split('\n').pop() });
  }
}
const median = (list, key) => {
  const v = list.map((r) => r[key]).filter((x) => x !== undefined).sort((a, b) => a - b);
  return v.length ? v[v.length >> 1] : NaN;
};
console.log(`${path.basename(file)} ${pipeline} (median of ${runs}${threads ? `, ${threads} thread(s)` : ''})`);
for (const [name] of engines) {
  const list = results[name];
  if (list[0].error) console.log(`  ${name.padEnd(6)} ${list[0].error}`);
  else console.log(`  ${name.padEnd(6)} ${String(median(list, 'ms')).padStart(6)} ms wall ${String(median(list, 'cpuMs')).padStart(6)} ms cpu ${String(median(list, 'rssMb')).padStart(5)} MB peak`);
}
