// Calibrates the memory estimate behind job admission: for each case, the working set the
// library estimates before it admits the job against the peak RSS the job really adds.
//
//   node bench/admission-calibrate.mjs <fixture-dir> [--only name,name] [--json]
//
// Every case runs in a fresh child process (the peak of one job is what is measured: the
// child resets VmHWM through /proc/self/clear_refs after loading the input and warming the
// library, then reports VmHWM minus the RSS it started the job with). Inputs are Buffers, so
// their bytes are in the baseline and not in the delta. Needs Linux. FASTPKG_PKG=<package dir>
// runs another build (builds without `_jobEstimate` report no estimate).
//
// Fixtures: bench/gen-huge-png.mjs writes the PNGs (p8-31mp.png: 6200x5000 8-bit RGB,
// p8-40mp.png: 6325x6325, p16-40mp.png, p16-31mp.png, p8i-31mp.png interlaced, p8a-31mp.png RGBA,
// p8-31mp-noise.png incompressible); the JPEG/WebP files are 4000x3000 / 7000x5700 gradients with
// texture noise (j12.jpg, j12p.jpg progressive, j40.jpg, w12.webp, j-small.jpg 800x600).
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? fallback : argv[i + 1];
};

const CASES = {
  'png8-31mp rotate-1200-jpeg': ['p8-31mp.png', (s) => s.rotate().resize({ width: 1200 }).jpeg({ quality: 80 })],
  'png8-31mp noise 1200-jpeg': ['p8-31mp-noise.png', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'png8-31mp 1200-png': ['p8-31mp.png', (s) => s.resize({ width: 1200 }).png()],
  'png8-31mp 3000-jpeg': ['p8-31mp.png', (s) => s.resize({ width: 3000 }).jpeg({ quality: 80 })],
  'png8-31mp no-resize-jpeg': ['p8-31mp.png', (s) => s.jpeg({ quality: 80 })],
  'png8-40mp 1200-jpeg (shrinks)': ['p8-40mp.png', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'png8-40mp rotate-1200-webp': ['p8-40mp.png', (s) => s.rotate().resize({ width: 1200 }).webp({ quality: 80 })],
  'png8-40mp 3000-jpeg': ['p8-40mp.png', (s) => s.resize({ width: 3000 }).jpeg({ quality: 80 })],
  'png8-rgba-31mp 1200-png': ['p8a-31mp.png', (s) => s.resize({ width: 1200 }).png()],
  'png8-interlaced-31mp 1200-jpeg': ['p8i-31mp.png', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'png16-31mp 1200-jpeg': ['p16-31mp.png', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'png16-40mp rotate-1200-jpeg (shrinks)': ['p16-40mp.png', (s) => s.rotate().resize({ width: 1200 }).jpeg({ quality: 80 })],
  'png16-40mp 3000-png': ['p16-40mp.png', (s) => s.resize({ width: 3000 }).png()],
  'png16-12mp 800-jpeg': ['p16-12mp.png', (s) => s.resize({ width: 800 }).jpeg({ quality: 80 })],
  'png16-12mp no-resize-png': ['p16-12mp.png', (s) => s.png()],
  'png16-12mp-interlaced 800-jpeg': ['p16i-12mp.png', (s) => s.resize({ width: 800 }).jpeg({ quality: 80 })],
  'png8-12mp-interlaced no-resize-png': ['p8i-12mp.png', (s) => s.png()],
  'jpeg-12mp 1200': ['j12.jpg', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'jpeg-12mp 200': ['j12.jpg', (s) => s.resize({ width: 200 }).jpeg({ quality: 80 })],
  'jpeg-12mp 3000': ['j12.jpg', (s) => s.resize({ width: 3000 }).jpeg({ quality: 80 })],
  'jpeg-12mp no-resize': ['j12.jpg', (s) => s.jpeg({ quality: 80 })],
  'jpeg-12mp 1200-webp': ['j12.jpg', (s) => s.resize({ width: 1200 }).webp({ quality: 80 })],
  'jpeg-12mp 1200-png': ['j12.jpg', (s) => s.resize({ width: 1200 }).png()],
  'jpeg-12mp 1200-avif': ['j12.jpg', (s) => s.resize({ width: 1200 }).avif({ quality: 50, effort: 2 })],
  'jpeg-12mp sharpen+blur 1200': ['j12.jpg', (s) => s.resize({ width: 1200 }).sharpen().blur(1).jpeg({ quality: 80 })],
  'jpeg-12mp greyscale+negate no-resize-png': ['j12.jpg', (s) => s.greyscale().negate().png()],
  'jpeg-12mp blur(5) no-resize': ['j12.jpg', (s) => s.blur(5).jpeg({ quality: 80 })],
  'jpeg-12mp sharpen no-resize': ['j12.jpg', (s) => s.sharpen().jpeg({ quality: 80 })],
  'jpeg-12mp rotate(37)+1200': ['j12.jpg', (s) => s.rotate(37).resize({ width: 1200 }).jpeg({ quality: 80 })],
  'jpeg-12mp 1200+extend': ['j12.jpg', (s) => s.resize({ width: 1200 }).extend({ top: 10, bottom: 10, left: 10, right: 10 }).jpeg({ quality: 80 })],
  'jpeg-12mp 1200x1200 cover': ['j12.jpg', (s) => s.resize({ width: 1200, height: 1200, fit: 'cover' }).jpeg({ quality: 80 })],
  'jpeg-12mp linear-pipeline 1200': ['j12.jpg', (s) => s.pipelineColourspace('linear').resize({ width: 1200 }).jpeg({ quality: 80 })],
  'avif-3mp 800-jpeg': ['a3.avif', (s) => s.resize({ width: 800 }).jpeg({ quality: 80 })],
  'avif-3mp no-resize-jpeg': ['a3.avif', (s) => s.jpeg({ quality: 80 })],
  'jpeg-12mp-progressive 1200': ['j12p.jpg', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'jpeg-40mp 1200': ['j40.jpg', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'jpeg-40mp 3000': ['j40.jpg', (s) => s.resize({ width: 3000 }).jpeg({ quality: 80 })],
  'webp-12mp 1200-jpeg': ['w12.webp', (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })],
  'webp-12mp no-resize-webp': ['w12.webp', (s) => s.webp({ quality: 80 })],
  'png8-3mp 800-jpeg': ['p8-3mp.png', (s) => s.resize({ width: 800 }).jpeg({ quality: 80 })],
  'jpeg-small 200': ['j-small.jpg', (s) => s.resize({ width: 200 }).jpeg({ quality: 80 })],
  'jpeg-small 800-png': ['j-small.jpg', (s) => s.resize({ width: 400 }).png()],
};

const rssParts = () => {
  const status = fs.readFileSync('/proc/self/status', 'utf8');
  const kb = (name) => Number(new RegExp(`${name}:\\s+(\\d+)`).exec(status)[1]);
  return { rss: kb('VmRSS') / 1024, hwm: kb('VmHWM') / 1024 };
};

async function child() {
  const [name, dir] = [opt('case'), opt('dir')];
  const [file, build] = CASES[name];
  const pkg = process.env.FASTPKG_PKG ? path.resolve(process.env.FASTPKG_PKG, 'index.js') : '../packages/image/index.js';
  const lib = (await import(pkg)).default;
  const input = fs.readFileSync(path.join(dir, file));
  // Warm the library (thread pools, tables) on something tiny so only the job itself is measured.
  await lib(fs.readFileSync(path.join(dir, 'j-small.jpg'))).resize(50).jpeg().toBuffer();
  const make = () => build(lib(input, { limitInputPixels: false }));
  const estimate = typeof make()._jobEstimate === 'function' ? await make()._jobEstimate() : null;
  fs.writeFileSync('/proc/self/clear_refs', '5');
  const before = rssParts();
  const t0 = performance.now();
  const out = await make().toBuffer();
  const ms = performance.now() - t0;
  const after = rssParts();
  console.log(
    JSON.stringify({
      estimateMb: estimate === null ? null : estimate / 1048576,
      measuredMb: after.hwm - before.rss,
      ms,
      outBytes: out.length,
    }),
  );
}

if (argv.includes('--child')) {
  await child();
} else {
  const dir = path.resolve(argv.find((a) => !a.startsWith('--')));
  const only = opt('only', null)?.split(',');
  const rows = [];
  for (const name of Object.keys(CASES)) {
    if (only && !only.some((o) => name.includes(o))) continue;
    const run = spawnSync(process.execPath, [fileURLToPath(import.meta.url), '--child', '--case', name, '--dir', dir], {
      encoding: 'utf8',
      timeout: 300_000,
      env: process.env,
    });
    if (run.status !== 0) {
      rows.push({ name, error: (run.stderr || 'failed').trim().split('\n').pop() });
      continue;
    }
    rows.push({ name, ...JSON.parse(run.stdout.trim().split('\n').pop()) });
  }
  if (argv.includes('--json')) console.log(JSON.stringify(rows, null, 2));
  else {
    console.log('case'.padEnd(46), 'estimate MB'.padStart(12), 'measured MB'.padStart(12), 'ratio'.padStart(7), 'ms'.padStart(7));
    for (const r of rows) {
      if (r.error) console.log(r.name.padEnd(46), r.error);
      else
        console.log(
          r.name.padEnd(46),
          (r.estimateMb === null ? '-' : r.estimateMb.toFixed(0)).padStart(12),
          r.measuredMb.toFixed(0).padStart(12),
          (r.estimateMb === null ? '-' : (r.estimateMb / Math.max(r.measuredMb, 1)).toFixed(2)).padStart(7),
          r.ms.toFixed(0).padStart(7),
        );
    }
  }
}
