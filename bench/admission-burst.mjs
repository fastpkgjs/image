// Bursts of parallel jobs: peak RSS and wall time of N images processed at once, for @fastpkg/image
// and sharp, each in a fresh child process (the peak belongs to a whole process).
//
//   node bench/admission-burst.mjs <fixture-dir> [--scenarios a,b] [--engines fastpkg,sharp] [--n 50]
//        [--input buffer|path] [--json]
//
// Scenarios (every job resizes to 1200 px wide and writes a JPEG; the PNG ones rotate() first):
//   png8-31mp      6200x5000 8-bit RGB PNG (p8-31mp-noise.png: incompressible, a photo-like load)
//   png8-31mp-flat 6200x5000 8-bit RGB PNG (p8-31mp.png: flat colour ramp, decodes fast)
//   png16-40mp     6325x6325 16-bit RGB PNG (p16-40mp.png)
//   jpeg-12mp      4000x3000 JPEG (j12.jpg)
//   jpeg-small     200 jobs over an 800x600 JPEG (j-small.jpg)
//   mixed          a tenth 31 MP PNGs, a fifth 12 MP JPEGs, the rest small JPEGs, shuffled (N jobs)
// FASTPKG_PKG=<package dir> runs another build of the library (A/B); FASTPKG_MEMORY_BUDGET_MB and the
// other FASTPKG_* variables pass through. Needs Linux (peak RSS from /proc/self/status).
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? fallback : argv[i + 1];
};

const FILES = {
  'png8-31mp': 'p8-31mp-noise.png',
  'png8-31mp-flat': 'p8-31mp.png',
  'png16-40mp': 'p16-40mp.png',
  'jpeg-12mp': 'j12.jpg',
  'jpeg-small': 'j-small.jpg',
};

const hwm = () => Number(/VmHWM:\s+(\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024;

async function child() {
  const [engineName, scenario, dir, n, inputKind] = [opt('engine'), opt('scenario'), opt('dir'), Number(opt('n')), opt('input', 'buffer')];
  let lib;
  if (engineName === 'sharp') {
    lib = createRequire(import.meta.url)('sharp');
    lib.cache(false);
  } else {
    const pkg = process.env.FASTPKG_PKG ? path.resolve(process.env.FASTPKG_PKG, 'index.js') : '../packages/image/index.js';
    lib = (await import(pkg)).default;
  }
  const load = (name) => (inputKind === 'path' ? path.join(dir, name) : fs.readFileSync(path.join(dir, name)));
  const jobs = [];
  if (scenario === 'mixed') {
    const [big, mid, small] = [load(FILES['png8-31mp']), load(FILES['jpeg-12mp']), load(FILES['jpeg-small'])];
    let seed = 7;
    const rand = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 2 ** 32);
    for (let i = 0; i < n; i++) {
      const r = rand();
      jobs.push(r < 0.1 ? [big, true] : r < 0.3 ? [mid, false] : [small, false]);
    }
  } else {
    const input = load(FILES[scenario]);
    const count = scenario === 'jpeg-small' ? Math.max(n, 200) : n;
    for (let i = 0; i < count; i++) jobs.push([input, scenario.startsWith('png')]);
  }
  // Warm the library on something tiny so the pools and tables are not part of the burst.
  await lib(fs.readFileSync(path.join(dir, 'j-small.jpg'))).resize(50).jpeg().toBuffer();
  const base = hwm();
  const t0 = performance.now();
  const results = await Promise.allSettled(
    jobs.map(([input, rotate]) => {
      const pipeline = lib(input, { limitInputPixels: false });
      return (rotate ? pipeline.rotate() : pipeline).resize({ width: 1200 }).jpeg({ quality: 80 }).toBuffer({ resolveWithObject: true });
    }),
  );
  const ms = performance.now() - t0;
  const failed = results.filter((r) => r.status === 'rejected');
  const widths = new Set(results.filter((r) => r.status === 'fulfilled').map((r) => r.value.info.width));
  const admission = typeof lib._admission === 'function' ? lib._admission() : null;
  console.log(
    JSON.stringify({
      jobs: jobs.length,
      failed: failed.length,
      firstError: failed[0]?.reason?.message,
      widths: [...widths].sort(),
      baseMb: Math.round(base),
      peakMb: Math.round(hwm()),
      ms: Math.round(ms),
      admission,
    }),
  );
}

if (argv.includes('--child')) {
  await child();
} else {
  const dir = path.resolve(argv.find((a) => !a.startsWith('--')));
  const scenarios = (opt('scenarios', null) ?? 'png8-31mp,png16-40mp,jpeg-12mp,jpeg-small,mixed').split(',');
  const engines = (opt('engines', 'fastpkg,sharp')).split(',');
  const n = opt('n', '50');
  const rows = [];
  for (const scenario of scenarios) {
    for (const engine of engines) {
      const run = spawnSync(
        process.execPath,
        [fileURLToPath(import.meta.url), '--child', '--engine', engine, '--scenario', scenario, '--dir', dir, '--n', n, '--input', opt('input', 'buffer')],
        { encoding: 'utf8', timeout: 600_000, env: process.env },
      );
      const row = { scenario, engine };
      if (run.status === 0) Object.assign(row, JSON.parse(run.stdout.trim().split('\n').pop()));
      else row.error = (run.stderr || run.signal || 'failed').toString().trim().split('\n').pop();
      rows.push(row);
      if (!argv.includes('--json'))
        console.log(
          scenario.padEnd(15),
          engine.padEnd(8),
          row.error ? row.error : `${String(row.jobs).padStart(4)} jobs  peak ${String(row.peakMb).padStart(5)} MB (base ${row.baseMb})  ${String(row.ms).padStart(6)} ms  failed ${row.failed}${row.admission ? `  budget ${row.admission.budgetMb} MB claimed-peak ${row.admission.peakInFlightMb} MB waits ${row.admission.waits}` : ''}`,
        );
    }
  }
  if (argv.includes('--json')) console.log(JSON.stringify(rows, null, 2));
}
