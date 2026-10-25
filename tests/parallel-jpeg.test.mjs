// Opt-in banded JPEG encoding: jpeg({ parallel: true }).
// The spliced stream must decode to exactly the pixels of a single-pass baseline encode with
// the same settings (optimiseCoding: false), and sharp must read it without warnings.
import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { sharp, engine, pattern, prng } from './_util.mjs';

const child = path.join(path.dirname(fileURLToPath(import.meta.url)), '_parallel-child.mjs');

// Photo-like content: smooth gradients plus mild noise so every block has AC coefficients.
function noisy(width, height, channels = 3) {
  const raw = pattern(width, height, channels);
  const rnd = prng(width * 31 + height);
  for (let i = 0; i < raw.length; i++) raw[i] = Math.max(0, Math.min(255, raw[i] + Math.round((rnd() - 0.5) * 14)));
  return { raw, input: { raw: { width, height, channels } } };
}

function markers(buf) {
  let restarts = 0;
  for (let i = 0; i + 1 < buf.length; i++) if (buf[i] === 0xff && buf[i + 1] >= 0xd0 && buf[i + 1] <= 0xd7) restarts++;
  const dri = buf.indexOf(Buffer.from([0xff, 0xdd, 0x00, 0x04]));
  return { restarts, dri: dri >= 0, bands: dri >= 0 ? restarts + 1 : 1 };
}

// sharp (libjpeg-turbo) decoding with its default failOn 'warning': any structural problem throws.
const decode = (buf) => sharp(buf).raw().toBuffer({ resolveWithObject: true });

async function checkIdentical(label, build, { expectBands = true } = {}) {
  const par = await build(engine).toBuffer();
  const ref = await build(engine, false).toBuffer();
  const a = await decode(par);
  const b = await decode(ref);
  assert.equal(a.info.width, b.info.width, label);
  assert.equal(a.info.height, b.info.height, label);
  assert.equal(a.info.channels, b.info.channels, label);
  assert.ok(a.data.equals(b.data), `${label}: decoded pixels differ from the single-pass encode`);
  // The engine's own decoder agrees too.
  assert.ok((await engine(par).raw().toBuffer()).equals(await engine(ref).raw().toBuffer()), `${label}: engine decode differs`);
  const m = markers(par);
  if (expectBands) assert.ok(m.bands >= 2, `${label}: expected a banded stream, got ${m.bands} band(s)`);
  return { par, ref, ...m };
}

const sizes = [
  [1280, 800],
  [1301, 777],
  [1001, 1001],
  [1003, 1021],
  [300, 3500], // tall and narrow
  [4001, 260], // wide and short
];

test('banded output decodes to the same pixels as a single-pass baseline encode', async (t) => {
  if (engine.concurrency() < 2) return t.skip('needs a multi-threaded pool');
  for (const [w, h] of sizes) {
    for (const subsampling of ['4:2:0', '4:4:4']) {
      const { raw, input } = noisy(w, h);
      for (const quality of [60, 85, 95]) {
        const opts = { quality, chromaSubsampling: subsampling };
        await checkIdentical(`${w}x${h} ${subsampling} q${quality}`, (lib, parallel = true) =>
          lib(raw, input).jpeg(parallel ? { ...opts, parallel: true } : { ...opts, optimiseCoding: false }));
      }
    }
  }
});

test('banded grayscale (1-channel raw, greyscale(), extractChannel, b-w)', async (t) => {
  if (engine.concurrency() < 2) return t.skip('needs a multi-threaded pool');
  for (const [w, h] of [[1301, 777], [1003, 1021], [300, 3500]]) {
    const one = noisy(w, h, 1);
    const three = noisy(w, h, 3);
    const variants = {
      'gray raw': (lib) => lib(one.raw, one.input),
      greyscale: (lib) => lib(three.raw, three.input).greyscale(),
      extractChannel: (lib) => lib(three.raw, three.input).extractChannel(1),
      'b-w': (lib) => lib(three.raw, three.input).toColourspace('b-w'),
    };
    for (const [name, make] of Object.entries(variants)) {
      const r = await checkIdentical(`${w}x${h} ${name}`, (lib, parallel = true) =>
        make(lib).jpeg(parallel ? { quality: 80, parallel: true } : { quality: 80, optimiseCoding: false }));
      // extractChannel and b-w produce true single-component files; the others follow whatever
      // the normal encoder does with them (checkIdentical compares against it).
      if (name === 'extractChannel' || name === 'b-w') assert.equal((await sharp(r.par).metadata()).channels, 1, name);
    }
  }
});

test('alpha input is flattened onto black before banding', async (t) => {
  if (engine.concurrency() < 2) return t.skip('needs a multi-threaded pool');
  const raw = pattern(1200, 900, 4, { alpha: true });
  const input = { raw: { width: 1200, height: 900, channels: 4 } };
  await checkIdentical('rgba', (lib, parallel = true) =>
    lib(raw, input).jpeg(parallel ? { parallel: true } : { optimiseCoding: false }));
});

test('toFormat("jpeg", { parallel: true }) is the same as jpeg({ parallel: true })', async (t) => {
  if (engine.concurrency() < 2) return t.skip('needs a multi-threaded pool');
  const { raw, input } = noisy(1280, 800);
  const a = await engine(raw, input).toFormat('jpeg', { parallel: true, quality: 70 }).toBuffer();
  const b = await engine(raw, input).jpeg({ parallel: true, quality: 70 }).toBuffer();
  assert.ok(a.equals(b));
  assert.ok(markers(a).dri);
});

test('small images silently use the normal encoder', async () => {
  const { raw, input } = noisy(1000, 900); // 0.9 MP
  const par = await engine(raw, input).jpeg({ parallel: true, quality: 80 }).toBuffer();
  const normal = await engine(raw, input).jpeg({ quality: 80 }).toBuffer();
  assert.ok(par.equals(normal));
  assert.equal(markers(par).dri, false);
});

test('parallel is a boolean and cannot be combined with progressive / mozjpeg / optimiseScans', () => {
  const img = () => engine(Buffer.alloc(12), { raw: { width: 2, height: 2, channels: 3 } });
  for (const bad of [{ progressive: true }, { mozjpeg: true }, { optimiseScans: true }, { optimizeScans: true }]) {
    assert.throws(() => img().jpeg({ parallel: true, ...bad }), {
      name: 'TypeError',
      message: 'parallel cannot be combined with progressive/mozjpeg encoding',
    });
    assert.throws(() => img().toFormat('jpeg', { parallel: true, ...bad }), TypeError);
  }
  // Explicit "off" values are fine, as is parallel: false with any of them.
  img().jpeg({ parallel: true, progressive: false, mozjpeg: false, optimiseScans: false });
  img().jpeg({ parallel: false, progressive: true });
  img().jpeg({ parallel: false, mozjpeg: true });
  for (const bad of ['yes', 1, {}]) {
    assert.throws(() => img().jpeg({ parallel: bad }), TypeError);
  }
  // optimiseCoding is accepted and ignored by the banded encoder.
  img().jpeg({ parallel: true, optimiseCoding: true });
});

test('metadata written after splicing still works (orientation, density, ICC)', async (t) => {
  if (engine.concurrency() < 2) return t.skip('needs a multi-threaded pool');
  const { raw, input } = noisy(1280, 800);
  const make = (parallel) => engine(raw, input)
    .withMetadata({ orientation: 6, density: 300 })
    .withIccProfile('p3')
    .jpeg(parallel ? { parallel: true, quality: 80 } : { optimiseCoding: false, quality: 80 });
  const par = await make(true).toBuffer();
  const ref = await make(false).toBuffer();
  assert.ok(markers(par).dri);
  for (const lib of [sharp, engine]) {
    const mp = await lib(par).metadata();
    const mr = await lib(ref).metadata();
    assert.equal(mp.orientation, 6);
    assert.equal(mp.density, 300);
    assert.equal(mp.width, 1280);
    assert.equal(mp.height, 800);
    assert.ok(mp.icc && mp.icc.length > 100, 'ICC profile kept');
    assert.equal(mp.icc.length, mr.icc.length);
    assert.equal(mp.orientation, mr.orientation);
  }
  const a = await decode(par);
  const b = await decode(ref);
  assert.ok(a.data.equals(b.data));
});

test('auto-orient + parallel on a real JPEG input', async (t) => {
  if (engine.concurrency() < 2) return t.skip('needs a multi-threaded pool');
  const { raw, input } = noisy(1400, 1000);
  const src = await sharp(raw, input).withMetadata({ orientation: 6 }).jpeg({ quality: 92 }).toBuffer();
  const par = await engine(src).rotate().jpeg({ parallel: true, quality: 80 }).toBuffer();
  const ref = await engine(src).rotate().jpeg({ optimiseCoding: false, quality: 80 }).toBuffer();
  const a = await decode(par);
  assert.equal(a.info.width, 1000);
  assert.equal(a.info.height, 1400);
  assert.ok(a.data.equals((await decode(ref)).data));
});

// The pool size is per process, so thread counts are exercised in child processes.
test('band counts 2..12 and the single-thread fallback (child processes)', () => {
  const run = (threads, w, h, channels = 3, quality = 85, subsampling = '4:2:0') => {
    const r = spawnSync(process.execPath, [child, threads, w, h, channels, quality, subsampling], { encoding: 'utf8' });
    assert.equal(r.status, 0, r.stderr);
    return JSON.parse(r.stdout.trim().split('\n').pop());
  };
  const seen = new Set();
  for (let n = 2; n <= 12; n++) {
    const r = run(n, 1301, 1500);
    assert.equal(r.concurrency, n);
    assert.ok(r.dri, `threads=${n}: banded`);
    assert.ok(r.bands >= 2 && r.bands <= n, `threads=${n}: bands=${r.bands}`);
    assert.ok(r.sameInfo && r.identical, `threads=${n}: decoded pixels differ`);
    seen.add(r.bands);
  }
  assert.ok(seen.size >= 8, `distinct band counts: ${[...seen]}`);
  // 4:4:4, gray, odd height with a short remainder band.
  for (const [w, h, c, q, s] of [[1200, 1051, 3, 60, '4:4:4'], [1500, 703, 1, 90, '4:2:0'], [1100, 1000 + 17, 3, 95, '4:2:0']]) {
    const r = run(7, w, h, c, q, s);
    assert.ok(r.dri && r.identical && r.sameInfo, `${w}x${h}x${c}`);
  }
  // One thread: normal path, byte-identical to jpeg() without the option.
  const one = run(1, 1301, 1500);
  assert.equal(one.dri, false);
  assert.equal(one.bands, 1);
  assert.ok(one.sameBytesAsDefault);
  assert.ok(one.identical);
});
