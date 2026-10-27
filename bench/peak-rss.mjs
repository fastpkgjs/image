// Peak RSS (VmHWM) of one pipeline over one file, in a fresh child process per cell.
//
//   node bench/peak-rss.mjs <file.png> [--engine fastpkg|sharp|both] [--pipeline NAME]
//        [--json]
//
// Pipelines (all `limitInputPixels: false`): rotate-1200-jpeg (default; the usual upload
// normalisation), rotate-3000-jpeg (a mild downscale: no decode-time shrink), resize-800-jpeg, resize-800-png, rotate-1200-png, greyscale-800-jpeg,
// resize-{1200,3000,4096}-jpeg, resize-{1024,3000,4096}-png. Reports wall ms, CPU ms (user+system) and peak RSS.
// FASTPKG_PKG=/path/to/package-dir A/Bs another build; env FASTPKG_PNG_SHRINK_* overrides pass
// through to the child.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? fallback : argv[i + 1];
};

const PIPELINES = {
  'rotate-1200-jpeg': (s) => s.rotate().resize({ width: 1200 }).jpeg({ quality: 80 }),
  'rotate-3000-jpeg': (s) => s.rotate().resize({ width: 3000 }).jpeg({ quality: 80 }),
  'rotate-1200-png': (s) => s.rotate().resize({ width: 1200 }).png(),
  'resize-800-jpeg': (s) => s.resize({ width: 800 }).jpeg({ quality: 80 }),
  'resize-800-png': (s) => s.resize({ width: 800 }).png(),
  'resize-1200-jpeg': (s) => s.resize({ width: 1200 }).jpeg({ quality: 80 }),
  'resize-1024-png': (s) => s.resize({ width: 1024 }).png(),
  'resize-3000-jpeg': (s) => s.resize({ width: 3000 }).jpeg({ quality: 80 }),
  'resize-3000-png': (s) => s.resize({ width: 3000 }).png(),
  'resize-4096-jpeg': (s) => s.resize({ width: 4096 }).jpeg({ quality: 80 }),
  'resize-4096-png': (s) => s.resize({ width: 4096 }).png(),
  'greyscale-800-jpeg': (s) => s.greyscale().resize({ width: 800 }).jpeg({ quality: 80 }),
};

async function child() {
  const [engineName, file, pipeline] = [opt('engine'), opt('file'), opt('pipeline')];
  const lib =
    engineName === 'sharp'
      ? (await import('sharp')).default
      : (await import(process.env.FASTPKG_PKG ? path.resolve(process.env.FASTPKG_PKG, 'index.js') : '../packages/image/index.js')).default;
  const input = fs.readFileSync(file);
  const cpu0 = process.cpuUsage();
  const t0 = performance.now();
  const out = await PIPELINES[pipeline](lib(input, { limitInputPixels: false })).toBuffer();
  const ms = performance.now() - t0;
  const cpu = process.cpuUsage(cpu0);
  const rss = Number(/VmHWM:\s+(\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024;
  console.log(JSON.stringify({ rssMb: Math.round(rss), inputMb: Math.round(input.length / 1048576), ms: Math.round(ms), cpuMs: Math.round((cpu.user + cpu.system) / 1000), bytes: out.length }));
}

if (argv.includes('--child')) {
  await child();
} else {
  const file = path.resolve(argv.find((a) => !a.startsWith('--')));
  const pipeline = opt('pipeline', 'rotate-1200-jpeg');
  const engines = opt('engine', 'both') === 'both' ? ['fastpkg', 'sharp'] : [opt('engine')];
  const results = {};
  for (const engine of engines) {
    const run = spawnSync(process.execPath, [fileURLToPath(import.meta.url), '--child', '--engine', engine, '--file', file, '--pipeline', pipeline], { encoding: 'utf8', timeout: 300_000 });
    results[engine] = run.status === 0 ? JSON.parse(run.stdout) : { error: (run.stderr || 'failed').trim().split('\n').pop() };
  }
  if (argv.includes('--json')) console.log(JSON.stringify(results));
  else for (const [engine, r] of Object.entries(results)) console.log(`${path.basename(file)} ${pipeline} ${engine.padEnd(7)}`, r.error ?? `${r.rssMb} MB peak (input ${r.inputMb} MB, ${r.ms} ms wall, ${r.cpuMs} ms cpu)`);
}
