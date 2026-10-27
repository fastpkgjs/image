// Peak memory of big PNG downscales, measured in a child process (VmHWM, reset by exec) with the
// production shrink cutoff (128 MB decoded as RGBA: 32 MP of 8-bit, 16 MP of 16-bit pixels).
// The bounds sit well below what the earlier decoders needed, which is noted next to each case.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { writePng } from '../bench/png-fixtures.mjs';

const linux = process.platform === 'linux';
const enginePath = new URL('../packages/image/index.js', import.meta.url).href;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-shrink-mem-'));
test.after(() => fs.rmSync(dir, { recursive: true, force: true }));

function peak(file, pipeline) {
  const script = `
    import fs from 'node:fs';
    import engine from ${JSON.stringify(enginePath)};
    const png = fs.readFileSync(${JSON.stringify(file)});
    const out = await ${pipeline}(engine(png, { limitInputPixels: false })).toBuffer();
    console.log(JSON.stringify({ length: out.length, rss: Number(/VmHWM:\\s+(\\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024 }));
  `;
  const run = spawnSync(process.execPath, ['--input-type=module', '-e', script], { encoding: 'utf8', maxBuffer: 1 << 26 });
  assert.equal(run.status, 0, run.stderr);
  const { length, rss } = JSON.parse(run.stdout);
  assert.ok(length > 100);
  return rss;
}

const upload = '(s) => s.rotate().resize({ width: 1200 }).jpeg({ quality: 80 })';
const thumb = '(s) => s.resize({ width: 400 }).jpeg({ quality: 80 })';
const mild = '(s) => s.rotate().resize({ width: 3000 }).jpeg({ quality: 80 })';

async function fixture(name, opts) {
  const file = path.join(dir, `${name}.png`);
  await writePng(file, opts);
  return file;
}

// 25 MP, 200 MB decoded: under the old 32 MP pixel cutoff, so it was decoded whole and every
// full-size copy counted (about 950 MB before).
test('16-bit RGB below the old pixel cutoff downscales in a few hundred MB', { skip: !linux }, async () => {
  const file = await fixture('rgb16', { width: 5000, height: 5000, depth: 16, color: 'rgb' });
  const rss = peak(file, upload);
  assert.ok(rss < 260, `peak RSS ${rss.toFixed(0)} MB`);
});

test('16-bit RGBA downscales in a few hundred MB', { skip: !linux }, async () => {
  const file = await fixture('rgba16', { width: 4500, height: 4500, depth: 16, color: 'rgba' });
  const rss = peak(file, thumb);
  assert.ok(rss < 260, `peak RSS ${rss.toFixed(0)} MB`);
});

// A mild downscale cannot shrink at decode time, so the 16-bit frame is resident once: 8 bytes
// per pixel plus its 8-bit twin. Resizing it used to clone and repack it as well (about 28
// bytes per pixel: 460 MB for this 16 MP file, now about 250).
test('16-bit mild downscale does not copy the frame again', { skip: !linux }, async () => {
  const file = await fixture('rgb16-mild', { width: 4000, height: 4000, depth: 16, color: 'rgb' });
  const rss = peak(file, mild);
  assert.ok(rss < 330, `peak RSS ${rss.toFixed(0)} MB`);
});

// 49 MP interlaced: the buffered decoder held the whole frame (about 600 MB).
test('interlaced 8-bit RGB downscales without the whole frame', { skip: !linux }, async () => {
  const file = await fixture('adam7', { width: 7000, height: 7000, depth: 8, color: 'rgb', interlace: true });
  const rss = peak(file, thumb);
  assert.ok(rss < 200, `peak RSS ${rss.toFixed(0)} MB`);
});

test('interlaced 16-bit RGBA downscales without the whole frame', { skip: !linux }, async () => {
  const file = await fixture('adam7-16', { width: 4400, height: 4400, depth: 16, color: 'rgba', interlace: true });
  const rss = peak(file, thumb);
  assert.ok(rss < 200, `peak RSS ${rss.toFixed(0)} MB`);
});
