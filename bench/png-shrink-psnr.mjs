// Accuracy of PNG downscales against a float reference (see shrink-ref.mjs), in dB.
//
//   node bench/png-shrink-psnr.mjs [--size 3600x2400] [--ratios 4.5,9,18,36] [--old DIR]
//
// Columns: exact (whole image resampled, shrink off), new (decode-time shrink forced on),
// old (the 0.5.0 integer box shrink, needs --old DIR with a build of 0.5.0), sharp. Every engine
// run is a child process because the shrink cutoff is read once from the environment.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { photo, detail, graphics, referenceResize, psnr } from './shrink-ref.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i === -1 ? fallback : argv[i + 1];
};

if (argv.includes('--child')) {
  const [engineName, file, tw, th, out] = [opt('engine'), opt('file'), Number(opt('tw')), Number(opt('th')), opt('out')];
  const lib =
    engineName === 'sharp'
      ? createRequire(import.meta.url)('sharp')
      : (await import(path.resolve(opt('pkg', path.join(here, '..', 'packages', 'image')), 'index.js'))).default;
  const data = await lib(fs.readFileSync(file)).resize({ width: tw, height: th, fit: 'fill' }).raw().toBuffer();
  fs.writeFileSync(out, data);
  process.exit(0);
}

const [W, H] = opt('size', '3600x2400').split('x').map(Number);
const ratios = opt('ratios', '4.5,9,18,36').split(',').map(Number);
const oldPkg = opt('old', null);
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-psnr-'));
const sharp = createRequire(import.meta.url)('sharp');

const variants = {
  exact: { engine: 'fastpkg', env: { FASTPKG_PNG_SHRINK_MIN_BYTES: String(2 ** 50) } },
  new: { engine: 'fastpkg', env: { FASTPKG_PNG_SHRINK_MIN_BYTES: '0' } },
  ...(oldPkg ? { old: { engine: 'fastpkg', pkg: oldPkg, env: { FASTPKG_PNG_SHRINK_MIN_PIXELS: '0' } } } : {}),
  sharp: { engine: 'sharp', env: {} },
};

console.log(`${W}x${H} 8-bit RGB PNG, PSNR in dB against an f64 Lanczos3 reference of the full-resolution pixels`);
console.log('content'.padEnd(10), 'ratio'.padEnd(6), 'target'.padEnd(9), Object.keys(variants).map((k) => k.padStart(7)).join(' '));
for (const [name, make] of Object.entries({ photo, detail, graphics })) {
  const raw = make(W, H);
  const file = path.join(tmp, `${name}.png`);
  await sharp(Buffer.from(raw), { raw: { width: W, height: H, channels: 3 } }).png().toFile(file);
  for (const ratio of ratios) {
    const tw = Math.round(W / ratio);
    const th = Math.round(H / ratio);
    const reference = referenceResize(raw, W, H, 3, tw, th);
    const cells = [];
    for (const [key, v] of Object.entries(variants)) {
      const out = path.join(tmp, `${name}-${ratio}-${key}.raw`);
      const run = spawnSync(process.execPath, [fileURLToPath(import.meta.url), '--child', '--engine', v.engine, '--file', file, '--tw', tw, '--th', th, '--out', out, ...(v.pkg ? ['--pkg', v.pkg] : [])], {
        env: { ...process.env, ...v.env },
        encoding: 'utf8',
      });
      if (run.status !== 0) throw new Error(run.stderr);
      cells.push(psnr(fs.readFileSync(out), reference).toFixed(1).padStart(7));
    }
    console.log(name.padEnd(10), String(ratio).padEnd(6), `${tw}x${th}`.padEnd(9), cells.join(' '));
  }
}
fs.rmSync(tmp, { recursive: true, force: true });
