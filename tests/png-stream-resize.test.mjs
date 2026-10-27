// A big PNG whose first resampling step is a plain downscale is resampled while it is decoded
// (rows through the box shrink, padding and kernel in bands), so the frame never exists. The
// result must be what the whole-frame resize gives (float rounding apart) for every colour type,
// depth, fit and kernel, with and without the decode-time area shrink in front, and the failure
// behaviour of truncated files must not change. Memory and CPU are checked in child processes.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { sharp } from './_util.mjs';
import { W, H, encode, cases } from './_png-encode.mjs';
import { writePng } from '../bench/png-fixtures.mjs';

const linux = process.platform === 'linux';
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-stream-resize-'));
test.after(() => fs.rmSync(dir, { recursive: true, force: true }));
const child = new URL('./_png-stream-resize-child.mjs', import.meta.url).pathname;
const enginePath = new URL('../packages/image/index.js', import.meta.url).href;

const ON = { FASTPKG_PNG_STREAM_MIN_BYTES: '0' };
const OFF = { FASTPKG_PNG_STREAM_MIN_BYTES: String(2 ** 50), FASTPKG_RESIZE_STREAM_MIN_BYTES: String(2 ** 50) };

function run(jobs, env) {
  const manifest = path.join(dir, `jobs-${Math.random().toString(36).slice(2)}.json`);
  fs.writeFileSync(manifest, JSON.stringify(jobs));
  const out = spawnSync(process.execPath, [child, manifest], { encoding: 'utf8', env: { ...process.env, ...env }, maxBuffer: 1 << 28 });
  assert.equal(out.status, 0, out.stderr);
  return JSON.parse(out.stdout);
}

/** Largest sample difference between two results (16-bit results are compared as 16-bit). */
function worst(a, b) {
  const x = Buffer.from(a.data, 'base64');
  const y = Buffer.from(b.data, 'base64');
  assert.equal(x.length, y.length, 'output size');
  const wide = a.depth === 'ushort';
  let max = 0;
  for (let i = 0; i < x.length; i += wide ? 2 : 1) {
    const d = wide ? Math.abs(x.readUInt16LE(i) - y.readUInt16LE(i)) : Math.abs(x[i] - y[i]);
    if (d > max) max = d;
  }
  return max;
}

function write(name, options) {
  const file = path.join(dir, `${name.replace(/\W+/g, '-')}.png`);
  fs.writeFileSync(file, encode({ width: W, height: H, ...options }));
  return file;
}

const files = Object.fromEntries(Object.entries(cases).map(([name, spec]) => [name, write(name, spec)]));

test('streamed resize equals the whole-frame resize for every colour type and depth', () => {
  const jobs = [];
  const labels = [];
  for (const [name, file] of Object.entries(files)) {
    for (const width of [150, 96, 31]) {
      jobs.push({ file, pipeline: `(s) => s.resize({ width: ${width} }).raw()` });
      labels.push(`${name} @${width}`);
    }
  }
  const on = run(jobs, ON);
  const off = run(jobs, OFF);
  on.forEach((result, i) => {
    assert.ok(!result.error && !off[i].error, `${labels[i]}: ${result.error ?? off[i].error}`);
    assert.deepEqual([result.width, result.height, result.channels], [off[i].width, off[i].height, off[i].channels], labels[i]);
    assert.ok(worst(result, off[i]) <= 1, `${labels[i]}: differs by ${worst(result, off[i])}`);
  });
});

test('streamed resize equals the whole-frame resize with the decode-time shrink in front', () => {
  const jobs = [];
  const labels = [];
  for (const name of ['rgb 8', 'rgba 8', 'gray 8', 'gray alpha 8', 'rgb 16', 'rgba 16', 'palette 8 + tRNS', 'gray 8 + tRNS']) {
    for (const width of [64, 24, 9]) {
      jobs.push({ file: files[name], pipeline: `(s) => s.resize({ width: ${width} }).raw()` });
      labels.push(`${name} @${width}`);
    }
  }
  const shrink = { FASTPKG_PNG_SHRINK_MIN_BYTES: '0' };
  const on = run(jobs, { ...shrink, ...ON });
  const off = run(jobs, { ...shrink, ...OFF });
  on.forEach((result, i) => {
    assert.ok(!result.error && !off[i].error, `${labels[i]}: ${result.error ?? off[i].error}`);
    assert.ok(worst(result, off[i]) <= 1, `${labels[i]}: differs by ${worst(result, off[i])}`);
  });
});

test('fits, kernels, orientation and later operations see the streamed result', () => {
  const pipelines = [
    '(s) => s.resize({ width: 100, height: 70, fit: "cover" }).raw()',
    '(s) => s.resize({ width: 100, height: 70, fit: "contain", background: "#336699" }).raw()',
    '(s) => s.resize({ width: 100, height: 70, fit: "inside" }).raw()',
    '(s) => s.resize({ width: 100, height: 70, fit: "fill" }).raw()',
    '(s) => s.resize({ width: 100, height: 70, fit: "cover", position: "left top" }).raw()',
    '(s) => s.rotate().resize({ width: 100 }).raw()',
    '(s) => s.resize({ height: 50, kernel: "cubic" }).raw()',
    '(s) => s.resize({ width: 77, kernel: "mitchell" }).raw()',
    '(s) => s.resize({ width: 90 }).flatten({ background: "#ffffff" }).raw()',
    '(s) => s.resize({ width: 90 }).extend({ top: 3, left: 4, background: "#ff0000" }).raw()',
    '(s) => s.resize({ width: 90 }).sharpen().raw()',
    '(s) => s.resize({ width: 90 }).toColourspace("rgb16").raw()',
    '(s) => s.resize({ width: 1000 }).raw()',
    '(s) => s.resize({ width: 190 }).raw()',
  ];
  const jobs = [];
  const labels = [];
  for (const name of ['rgb 8', 'rgba 8', 'rgba 16', 'rgb 16', 'gray alpha 8']) {
    for (const pipeline of pipelines) {
      jobs.push({ file: files[name], pipeline });
      labels.push(`${name} ${pipeline}`);
    }
  }
  const on = run(jobs, ON);
  const off = run(jobs, OFF);
  on.forEach((result, i) => {
    assert.ok(!result.error && !off[i].error, `${labels[i]}: ${result.error ?? off[i].error}`);
    assert.deepEqual([result.width, result.height, result.channels], [off[i].width, off[i].height, off[i].channels], labels[i]);
    assert.ok(worst(result, off[i]) <= 2, `${labels[i]}: differs by ${worst(result, off[i])}`);
  });
});

test('thin, tiny and tall images and extreme targets agree with the whole-frame resize', () => {
  const jobs = [];
  const labels = [];
  const sample = (x, y, c) => (c === 3 ? 60 + ((x * 5 + y * 3) % 190) : (x * 37 + y * 11 + c * 90) & 255);
  for (const [width, height] of [[7, 500], [500, 5], [1, 1000], [333, 2], [64, 3000], [3000, 64], [97, 89]]) {
    for (const [type, depth] of [[2, 8], [6, 8], [6, 16]]) {
      const file = write(`odd-${width}x${height}-${type}-${depth}`, { width, height, type, depth, sample: (x, y, c) => (depth === 16 ? sample(x, y, c) * 257 : sample(x, y, c)) });
      for (const op of [
        '{ width: 3 }',
        '{ height: 2 }',
        `{ width: ${Math.max(1, width >> 1)} }`,
        '{ width: 1, height: 1, fit: "fill" }',
        '{ width: 5, height: 9, fit: "cover" }',
      ]) {
        jobs.push({ file, pipeline: `(s) => s.resize(${op}).raw()` });
        labels.push(`${width}x${height} type ${type} depth ${depth} ${op}`);
      }
    }
  }
  const on = run(jobs, ON);
  const off = run(jobs, OFF);
  on.forEach((result, i) => {
    assert.equal(Boolean(result.error), Boolean(off[i].error), `${labels[i]}: ${result.error} / ${off[i].error}`);
    if (!result.error) {
      assert.deepEqual([result.width, result.height, result.channels], [off[i].width, off[i].height, off[i].channels], labels[i]);
      assert.ok(worst(result, off[i]) <= 2, `${labels[i]}: differs by ${worst(result, off[i])}`);
    }
  });
});

test('truncated and damaged files behave the same streamed or not', () => {
  const jobs = [];
  const labels = [];
  for (const name of ['rgb 8', 'rgba 16']) {
    const full = fs.readFileSync(files[name]);
    for (const [kind, bytes] of [
      ['truncated 60%', full.subarray(0, Math.floor(full.length * 0.6))],
      ['truncated 95%', full.subarray(0, Math.floor(full.length * 0.95))],
      ['no IEND', full.subarray(0, full.length - 12)],
    ]) {
      const file = path.join(dir, `${name.replace(/\W+/g, '-')}-${kind.replace(/\W+/g, '-')}.png`);
      fs.writeFileSync(file, bytes);
      for (const failOn of ['none', 'warning', 'error']) {
        jobs.push({ file, failOn, pipeline: '(s) => s.resize({ width: 60 }).raw()' });
        labels.push(`${name} ${kind} failOn ${failOn}`);
      }
    }
  }
  const on = run(jobs, ON);
  const off = run(jobs, OFF);
  on.forEach((result, i) => {
    assert.equal(Boolean(result.error), Boolean(off[i].error), `${labels[i]}: ${result.error} / ${off[i].error}`);
    if (!result.error) assert.ok(worst(result, off[i]) <= 1, `${labels[i]}: differs by ${worst(result, off[i])}`);
  });
});

test('an image with an embedded profile is converted before the resize, streamed or not', async () => {
  const profiled = path.join(dir, 'p3.png');
  const raw = Buffer.alloc(W * H * 3);
  for (let i = 0; i < raw.length; i++) raw[i] = (i * 7) & 255;
  fs.writeFileSync(profiled, await sharp(raw, { raw: { width: W, height: H, channels: 3 } }).withIccProfile('p3').png().toBuffer());
  const jobs = [{ file: profiled, pipeline: '(s) => s.resize({ width: 80 }).raw()' }];
  const [on] = run(jobs, ON);
  const [off] = run(jobs, OFF);
  assert.ok(worst(on, off) <= 1);
});

// --- memory and CPU, in child processes -------------------------------------------------------

function measure(file, pipeline, lib = 'engine') {
  const script = `
    import fs from 'node:fs';
    const lib = ${lib === 'engine' ? `(await import(${JSON.stringify(enginePath)})).default` : `(await import('sharp')).default`};
    const png = fs.readFileSync(${JSON.stringify(file)});
    const cpu0 = process.cpuUsage();
    const t0 = performance.now();
    const out = await (${pipeline})(lib(png, { limitInputPixels: false })).toBuffer();
    const cpu = process.cpuUsage(cpu0);
    console.log(JSON.stringify({ length: out.length, ms: performance.now() - t0, cpuMs: (cpu.user + cpu.system) / 1000, rss: Number(/VmHWM:\\s+(\\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024 }));
  `;
  const out = spawnSync(process.execPath, ['--input-type=module', '-e', script], { encoding: 'utf8', cwd: path.dirname(new URL(import.meta.url).pathname), maxBuffer: 1 << 26 });
  assert.equal(out.status, 0, out.stderr);
  return JSON.parse(out.stdout);
}

const median = (list) => list.slice().sort((a, b) => a - b)[list.length >> 1];

// 8000x8000 RGBA is 256 MB decoded. The whole-frame decode needed it plus the padded copy and the
// output (about 460 MB in 0.6.0, 3.4x sharp; 8000x8000 JPEG now about 270 MB, PNG 160 MB, of which the flattened copy for JPEG is 64); the rows are resampled as they arrive.
test('a mild RGBA downscale never holds the frame', { skip: !linux }, async () => {
  const file = path.join(dir, 'rgba8000.png');
  await writePng(file, { width: 8000, height: 8000, depth: 8, color: 'rgba', level: 1 });
  const jpeg = measure(file, '(s) => s.resize({ width: 4000 }).jpeg({ quality: 80 })');
  assert.ok(jpeg.rss < 340, `jpeg: peak RSS ${jpeg.rss.toFixed(0)} MB`);
  const png = measure(file, '(s) => s.resize({ width: 4000 }).png()');
  assert.ok(png.rss < 230, `png: peak RSS ${png.rss.toFixed(0)} MB`);
});

// 16-bit 5600x5600 is 250 MB decoded (a mild downscale cannot shrink while decoding).
test('a mild 16-bit downscale never holds the frame', { skip: !linux }, async () => {
  const file = path.join(dir, 'rgb16-5600.png');
  await writePng(file, { width: 5600, height: 5600, depth: 16, color: 'rgb', level: 1 });
  const rss = measure(file, '(s) => s.rotate().resize({ width: 3000 }).jpeg({ quality: 80 })').rss;
  assert.ok(rss < 190, `peak RSS ${rss.toFixed(0)} MB`);
});

// The decode-time shrink feeds the same stream: 100 MP (400 MB decoded) to 1000 px.
test('a big downscale holds neither the frame nor the shrunk image', { skip: !linux }, async () => {
  const file = path.join(dir, 'rgb-10000.png');
  await writePng(file, { width: 10000, height: 10000, depth: 8, color: 'rgb', level: 1 });
  const rss = measure(file, '(s) => s.resize({ width: 1000 }).png()').rss;
  assert.ok(rss < 130, `peak RSS ${rss.toFixed(0)} MB`);
});

// Accumulating the shrink must stay cheap next to decoding (it was 45% slower than 0.5.0), and
// the resize of a 4096x4096 file must not cost more CPU than sharp's by a wide margin.
test('big PNG downscales cost about what sharp costs', { skip: !linux }, async () => {
  const big = path.join(dir, 'rgb-10000-cpu.png');
  await writePng(big, { width: 10000, height: 10000, depth: 8, color: 'rgb', level: 1 });
  const mid = path.join(dir, 'rgb-4096.png');
  await writePng(mid, { width: 4096, height: 4096, depth: 8, color: 'rgb', content: 'noise', level: 1 });
  for (const [file, pipeline, bound] of [
    [big, '(s) => s.resize({ width: 1200 }).jpeg({ quality: 80 })', 1.8],
    [mid, '(s) => s.resize({ width: 1024 }).png()', 2.2],
  ]) {
    const ours = [];
    const theirs = [];
    for (let i = 0; i < 3; i++) {
      ours.push(measure(file, pipeline).cpuMs);
      theirs.push(measure(file, pipeline, 'sharp').cpuMs);
    }
    assert.ok(median(ours) <= bound * median(theirs) + 60, `${path.basename(file)}: ${median(ours).toFixed(0)} ms CPU against sharp's ${median(theirs).toFixed(0)}`);
  }
});
