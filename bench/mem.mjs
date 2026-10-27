// Steady-state memory harness: engine vs sharp, default threads and concurrency(1).
//
//   node bench/mem.mjs                      # full matrix (1500 iterations per cell)
//   node bench/mem.mjs --iters 500 --engines fastpkg --scenarios atlas-png
//   FASTPKG_PKG=/path/to/package-dir node bench/mem.mjs   # A/B another build (dir with index.js + .node)
//   node bench/mem.mjs --json out.json
//
// Every cell runs in its own child process so RSS is not shared between cells. Inputs are
// generated once (with sharp) into bench/.cache/mem. The child warms up for 20% of the
// iterations, then samples process.memoryUsage().rss every 25 iterations; the table reports
// the median and max sample plus the median and mean wall time per image (throughput; the
// median is robust against other load on a shared machine). cpu/sys are process CPU ms per
// image (user+system, and system alone: page faults and munmap show up there), which stay
// comparable when the machine is busy.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const cache = path.join(here, '.cache', 'mem');
const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? fallback : argv[i + 1];
};

const SCENARIOS = {
  'jpeg-resize-jpeg': { file: 'photo-1024.jpg', parallel: 1 },
  'png-rgba-png': { file: 'rgba-1024.png', parallel: 1 },
  'atlas-png': { file: 'rgba-1024.png', overlay: 'sprite-256.png', parallel: 1 },
  'atlas-webp': { file: 'rgba-1024.png', overlay: 'sprite-256.png', parallel: 1 },
  'big-jpeg-1200': { file: 'photo-4000.jpg', parallel: 1 },
  'big-jpeg-1200-x8': { file: 'photo-4000.jpg', parallel: 8 },
  // 16384x16384 PNG (bench/gen-huge-png.mjs): few iterations, one sample each, peak RSS matters.
  // (More big fixtures: 16-bit and interlaced ones below.)
  'huge-png-800-png': { file: 'huge-256mp.png', parallel: 1, iters: 6, every: 1 },
  'huge-png-800-jpeg': { file: 'huge-256mp.png', parallel: 1, iters: 6, every: 1 },
  // The usual upload normalisation: auto-orient first, then resize.
  'huge-png-rotate-800-jpeg': { file: 'huge-256mp.png', parallel: 1, iters: 6, every: 1 },
  // Listing / background-removal normalisation: auto-orient, flatten, resize.
  'huge-png-rotate-flatten-800-jpeg': { file: 'huge-256mp.png', parallel: 1, iters: 6, every: 1 },
  // 16-bit RGB: 40 MP (a typical upload limit) and 31 MP (just under 32 MP), resized to 1200 px;
  // and 40 MP to 3000 px, a mild downscale that cannot shrink while decoding.
  'png16-40mp-rotate-1200-jpeg': { file: 'rgb16-40mp.png', parallel: 1, iters: 6, every: 1 },
  'png16-31mp-rotate-1200-jpeg': { file: 'rgb16-31mp.png', parallel: 1, iters: 6, every: 1 },
  'png16-40mp-rotate-3000-jpeg': { file: 'rgb16-40mp.png', parallel: 1, iters: 6, every: 1 },
  // 16384x16384 Adam7 (interlaced) 8-bit RGB.
  'huge-adam7-800-jpeg': { file: 'huge-256mp-adam7.png', parallel: 1, iters: 6, every: 1 },
};

// Big fixtures, written by gen-huge-png.mjs on first use (kept out of git under .cache).
const FIXTURES = {
  'huge-256mp.png': [],
  'rgb16-40mp.png': ['--size', '6325', '--depth', '16'],
  'rgb16-31mp.png': ['--size', '5568', '--depth', '16'],
  'huge-256mp-adam7.png': ['--interlace'],
};

function run(engine, input, name, overlay) {
  const sharpLike = engine;
  switch (name) {
    case 'jpeg-resize-jpeg':
      return sharpLike(input).resize({ width: 512 }).jpeg({ quality: 80 }).toBuffer();
    case 'png-rgba-png':
      return sharpLike(input).png().toBuffer();
    case 'atlas-png':
      return sharpLike(input).composite([{ input: overlay, left: 100, top: 100 }]).png().toBuffer();
    case 'atlas-webp':
      return sharpLike(input).composite([{ input: overlay, left: 100, top: 100 }]).webp({ quality: 80 }).toBuffer();
    case 'huge-png-800-png':
      return sharpLike(input, { limitInputPixels: false }).resize({ width: 800 }).png().toBuffer();
    case 'huge-png-800-jpeg':
      return sharpLike(input, { limitInputPixels: false }).resize({ width: 800 }).jpeg({ quality: 80 }).toBuffer();
    case 'huge-png-rotate-flatten-800-jpeg':
      return sharpLike(input, { limitInputPixels: false }).rotate().flatten({ background: '#ffffff' }).resize({ width: 800 }).jpeg({ quality: 80 }).toBuffer();
    case 'huge-png-rotate-800-jpeg':
      return sharpLike(input, { limitInputPixels: false }).rotate().resize({ width: 800 }).jpeg({ quality: 80 }).toBuffer();
    case 'png16-40mp-rotate-1200-jpeg':
    case 'png16-31mp-rotate-1200-jpeg':
      return sharpLike(input, { limitInputPixels: false }).rotate().resize({ width: 1200 }).jpeg({ quality: 80 }).toBuffer();
    case 'png16-40mp-rotate-3000-jpeg':
      return sharpLike(input, { limitInputPixels: false }).rotate().resize({ width: 3000 }).jpeg({ quality: 80 }).toBuffer();
    case 'huge-adam7-800-jpeg':
      return sharpLike(input, { limitInputPixels: false }).resize({ width: 800 }).jpeg({ quality: 80 }).toBuffer();
    case 'big-jpeg-1200':
    case 'big-jpeg-1200-x8':
      return sharpLike(input).resize({ width: 1200 }).jpeg({ quality: 85 }).toBuffer();
    default:
      throw new Error(`unknown scenario ${name}`);
  }
}

async function child() {
  const [engineName, scenario, conc] = [opt('engine'), opt('scenario'), opt('conc')];
  const iters = SCENARIOS[scenario].iters ?? Number(opt('iters', '1500'));
  const engine = engineName === 'sharp' ? (await import('sharp')).default : (await import(process.env.FASTPKG_PKG ? path.resolve(process.env.FASTPKG_PKG, 'index.js') : '../packages/image/index.js')).default;
  if (engineName === 'sharp') {
    // Matches production use: sharp's default cache is kept.
    if (conc === '1') engine.concurrency(1);
  } else if (conc === '1') {
    engine.concurrency(1);
  }
  const spec = SCENARIOS[scenario];
  const input = fs.readFileSync(path.join(cache, spec.file));
  const overlay = spec.overlay ? fs.readFileSync(path.join(cache, spec.overlay)) : null;
  const warm = Math.floor(iters * 0.2);
  const samples = [];
  let start = 0;
  const times = [];
  let cpu0 = null;
  for (let i = 0; i < iters; i += spec.parallel) {
    if (i >= warm && start === 0) { start = performance.now(); cpu0 = process.cpuUsage(); }
    const t0 = performance.now();
    if (spec.parallel === 1) await run(engine, input, scenario, overlay);
    else await Promise.all(Array.from({ length: spec.parallel }, () => run(engine, input, scenario, overlay)));
    if (i >= warm) times.push((performance.now() - t0) / spec.parallel);
    if (i >= warm && (i / spec.parallel) % (spec.every ?? 25) === 0) samples.push(process.memoryUsage().rss);
  }
  const ms = (performance.now() - start) / (iters - warm);
  const cpu = process.cpuUsage(cpu0);
  samples.sort((a, b) => a - b);
  times.sort((a, b) => a - b);
  console.log(JSON.stringify({
    medianMs: times[times.length >> 1],
    cpuMs: (cpu.user + cpu.system) / 1000 / (iters - warm),
    sysMs: cpu.system / 1000 / (iters - warm),
    median: samples[samples.length >> 1] / 1048576,
    max: samples[samples.length - 1] / 1048576,
    peak: process.resourceUsage().maxRSS / 1024,
    msPerImage: ms,
  }));
}

async function prepare() {
  fs.mkdirSync(cache, { recursive: true });
  const wanted = new Set((opt('scenarios', Object.keys(SCENARIOS).join(',')) + '').split(',').map((name) => SCENARIOS[name]?.file));
  for (const [name, args] of Object.entries(FIXTURES)) {
    const file = path.join(cache, name);
    if (wanted.has(name) && !fs.existsSync(file)) {
      spawnSync(process.execPath, [path.join(here, 'gen-huge-png.mjs'), file, ...args], { stdio: 'inherit' });
    }
  }
  if (fs.existsSync(path.join(cache, 'sprite-256.png'))) return;
  const sharp = (await import('sharp')).default;
  const noise = (w, h, c, seed) => {
    const b = Buffer.allocUnsafe(w * h * c);
    let s = seed;
    for (let i = 0; i < b.length; i++) {
      s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
      b[i] = (s >>> 24) & 0x3f;
    }
    return b;
  };
  const photo = async (w, h, file, q) => {
    const small = await sharp(noise(w >> 4, h >> 4, 3, w), { raw: { width: w >> 4, height: h >> 4, channels: 3 } })
      .resize(w, h, { kernel: 'cubic' }).raw().toBuffer();
    const grain = noise(w, h, 3, h);
    for (let i = 0; i < small.length; i++) small[i] = Math.min(255, small[i] * 3 + grain[i] / 4);
    await sharp(small, { raw: { width: w, height: h, channels: 3 } }).jpeg({ quality: q }).toFile(path.join(cache, file));
    return small;
  };
  const rgb = await photo(1024, 1024, 'photo-1024.jpg', 85);
  await photo(4000, 3000, 'photo-4000.jpg', 85);
  const alpha = Buffer.alloc(1024 * 1024, 255);
  for (let y = 0; y < 1024; y++) for (let x = 0; x < 1024; x++) if ((x >> 6) % 3 === 0 && (y >> 6) % 2 === 0) alpha[y * 1024 + x] = 0;
  await sharp(rgb, { raw: { width: 1024, height: 1024, channels: 3 } })
    .joinChannel(alpha, { raw: { width: 1024, height: 1024, channels: 1 } })
    .png().toFile(path.join(cache, 'rgba-1024.png'));
  await sharp(noise(256, 256, 4, 7), { raw: { width: 256, height: 256, channels: 4 } }).png().toFile(path.join(cache, 'sprite-256.png'));
}

async function main() {
  await prepare();
  const iters = opt('iters', '1500');
  const engines = opt('engines', 'fastpkg,sharp').split(',');
  const scenarios = opt('scenarios', Object.keys(SCENARIOS).join(',')).split(',');
  const concs = opt('conc', 'default,1').split(',');
  const rows = [];
  for (const scenario of scenarios) {
    for (const conc of concs) {
      const row = { scenario, conc };
      for (const engine of engines) {
        const r = spawnSync(process.execPath, [
          fileURLToPath(import.meta.url), '--child', '--engine', engine, '--scenario', scenario,
          '--conc', conc, '--iters', iters,
        ], { cwd: root, encoding: 'utf8', env: process.env });
        if (r.status !== 0) { row[engine] = { error: (r.stderr || '').split('\n')[0] }; continue; }
        row[engine] = JSON.parse(r.stdout.trim().split('\n').pop());
      }
      rows.push(row);
      const fmt = (e) => (row[e] ? (row[e].error ? `ERR ${row[e].error}` : `${row[e].median.toFixed(0)} MB (max ${row[e].max.toFixed(0)}, peak ${row[e].peak.toFixed(0)}) ${row[e].medianMs.toFixed(1)} ms/img (mean ${row[e].msPerImage.toFixed(1)}, cpu ${row[e].cpuMs.toFixed(1)}, sys ${row[e].sysMs.toFixed(1)})`) : '-');
      console.log(`${scenario.padEnd(18)} conc=${conc.padEnd(7)} ${engines.map((e) => `${e}: ${fmt(e)}`).join(' | ')}`);
    }
  }
  const out = opt('json');
  if (out) fs.writeFileSync(out, JSON.stringify(rows, null, 2));
}

if (argv.includes('--child')) await child();
else await main();
