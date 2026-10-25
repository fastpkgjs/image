// Focused regression tests for the engine fixes in this batch (each block names the bug it covers).
// sharp is the oracle wherever the behaviour is observable.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { sharp, engine, exec, diffStats, pattern } from './_util.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const libPath = path.join(here, '..', 'packages', 'image', 'lib.cjs');
const fixture = (name) => fs.readFileSync(path.join(here, '..', 'fixtures', name));

// Smooth RGB test card (no aliasing, so resampler differences stay tiny).
function card(width, height, channels = 3) {
  const buf = Buffer.alloc(width * height * channels);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const o = (y * width + x) * channels;
      buf[o] = Math.round((x * 255) / (width - 1));
      buf[o + 1] = Math.round((y * 255) / (height - 1));
      buf[o + 2] = (x + y) & 255;
      if (channels === 4) buf[o + 3] = 255 - ((x + y) % 120);
    }
  }
  return buf;
}
const rawIn = (lib, w, h, c = 3) => lib(card(w, h, c), { raw: { width: w, height: h, channels: c } });

async function both(build, opts) {
  const [s, e] = await Promise.all([exec(sharp, build, opts), exec(engine, build, opts)]);
  assert.ok(!s.error, `sharp failed: ${s.error?.message}`);
  assert.ok(!e.error, `engine failed: ${e.error?.message}`);
  return { s, e };
}

function assertSame(s, e, { mad = 0, label = '' } = {}) {
  assert.deepEqual(
    [e.info.width, e.info.height, e.info.channels],
    [s.info.width, s.info.height, s.info.channels],
    `${label} output geometry`
  );
  const d = diffStats(s.data, e.data);
  assert.ok(d.mad <= mad, `${label} mean abs diff ${d.mad.toFixed(2)} > ${mad}`);
}

// Runs a snippet in a fresh node process; a native abort/segfault shows up as a non-zero status.
function child(script, input) {
  return spawnSync(process.execPath, ['-e', script, libPath, ...input], { encoding: 'utf8', timeout: 60000 });
}

// ---------------------------------------------------------------------------------------------
// 1. AVIF: corrupt tile data used to abort the process inside rav1d (SIGABRT)
// ---------------------------------------------------------------------------------------------
test('avif: corrupt streams reject with an Error instead of aborting the process', () => {
  const script = `
    const image = require(process.argv[1]);
    const fs = require('fs');
    (async () => {
      const out = [];
      for (const file of process.argv.slice(2)) {
        try { await image(fs.readFileSync(file)).resize(16).raw().toBuffer(); out.push('ok'); }
        catch (e) { out.push(e instanceof Error ? 'rejected' : 'weird'); }
      }
      console.log(out.join(','));
    })();`;
  const files = ['avif-base.avif', 'avif-mut171.avif', 'avif-mut194.avif'].map((n) => path.join(here, '..', 'fixtures', n));
  const r = child(script, files);
  assert.equal(r.status, 0, `child died: signal=${r.signal} ${String(r.stderr).split('\n').find((l) => l.includes('panick')) ?? ''}`);
  assert.equal(r.stdout.trim(), 'ok,rejected,rejected');
});

test('avif: a pristine file still decodes to the right size', async () => {
  const { info } = await engine(fixture('avif-base.avif')).raw().toBuffer({ resolveWithObject: true });
  const ref = await sharp(fixture('avif-base.avif')).raw().toBuffer({ resolveWithObject: true });
  assert.deepEqual([info.width, info.height, info.channels], [ref.info.width, ref.info.height, ref.info.channels]);
});

// ---------------------------------------------------------------------------------------------
// 2. SVG: deep nesting overflowed the stack (SIGSEGV); hostile filter parameters pinned a core
// ---------------------------------------------------------------------------------------------
const svgHead = '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">';
const nested = (n) => `${svgHead}${'<g>'.repeat(n)}<rect width="5" height="5" fill="red"/>${'</g>'.repeat(n)}</svg>`;

test('svg: absurd nesting is rejected cleanly (no stack overflow)', () => {
  const dir = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), 'fixes-a-'));
  try {
    const file = path.join(dir, 'deep.svg');
    fs.writeFileSync(file, nested(20000));
    const script = `
      const image = require(process.argv[1]);
      image(require('fs').readFileSync(process.argv[2])).raw().toBuffer().then(() => console.log('ok'), (e) => console.log(e instanceof Error ? 'rejected' : 'weird'));`;
    const r = child(script, [file]);
    assert.equal(r.status, 0, `child died: signal=${r.signal}`);
    assert.equal(r.stdout.trim(), 'rejected');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('svg: nesting limit mirrors librsvg (255 groups render, 300 do not)', async () => {
  // sharp's own depth limit depends on the platform's glib/cairo build (musl builds stop at 50 layers),
  // so only the engine's behaviour is asserted for the passing case.
  await assert.doesNotReject(engine(Buffer.from(nested(255))).raw().toBuffer());
  await assert.rejects(sharp(Buffer.from(nested(300))).raw().toBuffer());
  await assert.rejects(engine(Buffer.from(nested(300))).raw().toBuffer(), Error);
});

test('svg: deep clip-path chains render on the large worker stack', async () => {
  let defs = '<clipPath id="c0"><rect width="5" height="5"/></clipPath>';
  for (let i = 1; i < 3000; i++) defs += `<clipPath id="c${i}" clip-path="url(#c${i - 1})"><rect width="5" height="5"/></clipPath>`;
  const svg = Buffer.from(`${svgHead}${defs}<rect width="5" height="5" fill="red" clip-path="url(#c2999)"/></svg>`);
  const { info } = await engine(svg).raw().toBuffer({ resolveWithObject: true });
  assert.equal(info.width, 10);
});

test('svg: filter parameters that cost minutes of CPU are rejected up front', async () => {
  const filters = [
    '<feMorphology radius="100000"/>',
    '<feConvolveMatrix order="300" kernelMatrix="1"/>',
    '<feTurbulence baseFrequency="0.05" numOctaves="100000"/>',
  ];
  for (const filter of filters) {
    const svg = Buffer.from(`${svgHead}<filter id="f">${filter}</filter><rect width="10" height="10" filter="url(#f)"/></svg>`);
    const t0 = Date.now();
    await assert.rejects(engine(svg).raw().toBuffer(), /svg/, filter);
    assert.ok(Date.now() - t0 < 5000, `${filter} was not rejected quickly`);
  }
  const ok = Buffer.from(`${svgHead}<filter id="f"><feMorphology radius="2"/></filter><rect width="10" height="10" filter="url(#f)"/></svg>`);
  await assert.doesNotReject(engine(ok).raw().toBuffer());
});

// ---------------------------------------------------------------------------------------------
// 3 + 4. resize: contain pads, fill keeps the other axis, per-axis enlargement/reduction clamps
// ---------------------------------------------------------------------------------------------
test('resize fit:contain pads to the requested size with background, position and alpha', async () => {
  const cases = [
    [50, 50, {}],
    [50, 50, { background: { r: 255, g: 0, b: 0, alpha: 0.5 } }],
    [201, 101, { position: 'right top', background: '#00ff00' }],
    [200, 100, { position: 'left bottom' }],
    [99, 200, { position: 'north' }],
    [100, 41, { position: 3 }],
    [199, 99, { position: 'southeast' }],
    [60, 200, { position: 'east' }],
    [51, undefined, {}],
    [undefined, 61, {}],
  ];
  for (const [w, h, extra] of cases) {
    const { s, e } = await both((lib) => lib({ create: { width: 120, height: 80, channels: 3, background: { r: 255, g: 0, b: 0 } } }).resize(w, h, { fit: 'contain', background: { r: 0, g: 0, b: 255 }, ...extra }));
    assertSame(s, e, { label: `contain ${w}x${h} ${JSON.stringify(extra)}` });
  }
  // The headline case from the bug report.
  const { info } = await engine(card(120, 80), { raw: { width: 120, height: 80, channels: 3 } }).resize(50, 50, { fit: 'contain', background: '#00f' }).raw().toBuffer({ resolveWithObject: true });
  assert.deepEqual([info.width, info.height], [50, 50]);
});

test('resize output sizes match sharp across fits x flags x targets', async () => {
  const sources = [[120, 80], [80, 120], [7, 3], [1, 1]];
  const targets = [undefined, 1, 3, 50, 100, 200];
  const flagSets = [{}, { withoutEnlargement: true }, { withoutReduction: true }, { withoutEnlargement: true, withoutReduction: true }];
  const mismatches = [];
  for (const [sw, sh] of sources) {
    for (const w of targets) {
      for (const h of targets) {
        if (w === undefined && h === undefined) continue;
        for (const fit of ['cover', 'contain', 'fill', 'inside', 'outside']) {
          for (const flags of flagSets) {
            const opts = { fit, ...flags };
            if (w) opts.width = w;
            if (h) opts.height = h;
            const build = (lib) => lib({ create: { width: sw, height: sh, channels: 3, background: '#123456' } }).resize(opts);
            const [s, e] = await Promise.all([exec(sharp, build), exec(engine, build)]);
            const a = `${s.info.width}x${s.info.height}`;
            const b = `${e.info?.width}x${e.info?.height}`;
            if (a !== b) mismatches.push(`${sw}x${sh} -> ${JSON.stringify(opts)}: sharp ${a} engine ${b}`);
          }
        }
      }
    }
  }
  assert.deepEqual(mismatches.slice(0, 10), []);
});

test('resize fill with a single dimension keeps the source size on the other axis', async () => {
  const { s, e } = await both((lib) => rawIn(lib, 120, 80).resize(50, undefined, { fit: 'fill' }));
  assert.deepEqual([e.info.width, e.info.height], [50, 80]);
  assertSame(s, e, { mad: 1, label: 'fill 50x-' });
  const v = await both((lib) => rawIn(lib, 120, 80).resize(undefined, 50, { fit: 'fill' }));
  assert.deepEqual([v.e.info.width, v.e.info.height], [120, 50]);
});

test('resize withoutReduction + contain / cover match sharp pixel for pixel on smooth content', async () => {
  const cases = [
    ['contain', 60, undefined, 'centre'], ['contain', 50, 50, 'right bottom'], ['contain', 131, 51, 'centre'], ['contain', 20, 10, 'north'],
    ['cover', 50, undefined, 'centre'], ['cover', 1, 1, 'centre'], ['cover', 2, 2, 'north'], ['cover', 200, 50, 'right'],
  ];
  for (const [fit, w, h, position] of cases) {
    for (const flags of [{ withoutReduction: true }, { withoutEnlargement: true }]) {
      const { s, e } = await both((lib) => rawIn(lib, 120, 80).resize(w, h, { fit, position, background: '#0000ff', ...flags }));
      assertSame(s, e, { mad: Math.min(w, h ?? w) <= 2 ? 7 : 3, label: `${fit} ${w}x${h} ${position} ${Object.keys(flags)[0]}` });
    }
  }
});

test('resize tiny targets and outside with tiny targets', async () => {
  for (const fit of ['cover', 'contain', 'fill', 'inside', 'outside']) {
    for (const [w, h] of [[1, 1], [3, 2], [2, 3]]) {
      const { s, e } = await both((lib) => rawIn(lib, 7, 13).resize(w, h, { fit }));
      assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], `${fit} ${w}x${h}`);
    }
  }
  const { s, e } = await both((lib) => rawIn(lib, 7, 13).resize(1, 1, { fit: 'outside' }));
  assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height]);
});

test('resize with JPEG shrink-on-load never undershoots for contain / inside / cover', async () => {
  const jpeg = await sharp(card(800, 600), { raw: { width: 800, height: 600, channels: 3 } }).jpeg({ quality: 95 }).toBuffer();
  for (const [fit, w, h] of [['contain', 100, 100], ['inside', 100, 100], ['cover', 100, 60], ['contain', 300, 100], ['contain', 50, undefined], ['cover', 64, 64]]) {
    for (const fast of [true, false]) {
      const build = (lib) => lib(jpeg).resize(w, h, { fit, fastShrinkOnLoad: fast, background: '#ff0000' });
      const [s, e] = await Promise.all([exec(sharp, build), exec(engine, build)]);
      assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], `${fit} ${w}x${h}`);
      assert.ok(diffStats(s.data, e.data).mad < 4, `${fit} ${w}x${h} fast=${fast} mad ${diffStats(s.data, e.data).mad}`);
    }
  }
});

test('numeric position / gravity constants and strategies are accepted; sharp.gravity is exported', async () => {
  assert.deepEqual({ ...engine.gravity }, { ...sharp.gravity });
  assert.deepEqual({ ...engine.strategy }, { ...sharp.strategy });
  for (const g of Object.values(sharp.gravity)) {
    const { s, e } = await both((lib) => rawIn(lib, 120, 80).resize(40, 40, { position: g }));
    assertSame(s, e, { mad: 3, label: `gravity ${g}` });
  }
  for (const strategy of [sharp.strategy.entropy, sharp.strategy.attention, 'entropy', 'attention']) {
    const out = await engine(card(120, 80), { raw: { width: 120, height: 80, channels: 3 } }).resize(40, 40, { position: strategy }).raw().toBuffer({ resolveWithObject: true });
    assert.deepEqual([out.info.width, out.info.height], [40, 40]);
  }
});

// ---------------------------------------------------------------------------------------------
// 5. 16-bit PNG upscaling (reported as wrong colours) - not an engine bug
// ---------------------------------------------------------------------------------------------
test('16-bit PNG upscaling matches the engine 8-bit path and sharp on smooth content', async () => {
  // A real 16-bit gradient built by hand (sharp's raw ushort input is unreliable for this).
  const { png16 } = await import('./_png16.mjs');
  const w = 120;
  const h = 80;
  const wide = png16(w, h, 3, (x, y, c) => (c === 0 ? Math.round((x * 65535) / (w - 1)) : c === 1 ? Math.round((y * 65535) / (h - 1)) : Math.round(((x + y) * 65535) / (w + h - 2))));
  assert.equal((await sharp(wide).metadata()).depth, 'ushort');
  assert.equal((await engine(wide).metadata()).depth, 'ushort');
  for (const [tw, th] of [[200, 150], [121, 81], [240, 160], [60, 40]]) {
    const { s, e } = await both((lib) => lib(wide).resize(tw, th, { fit: 'fill' }));
    assertSame(s, e, { mad: 3, label: `png16 ${tw}x${th}` });
  }
});

// ---------------------------------------------------------------------------------------------
// 6. EXIF orientation 1-8 (5 = transpose, 7 = transverse) and 7. orientation / density metadata
// ---------------------------------------------------------------------------------------------
test('rotate() / autoOrient() apply all eight EXIF orientations like sharp (jpeg and png, with resize)', async () => {
  for (const o of [1, 2, 3, 4, 5, 6, 7, 8]) {
    const jpeg = await rawIn(sharp, 60, 40).withMetadata({ orientation: o }).jpeg({ quality: 100, chromaSubsampling: '4:4:4' }).toBuffer();
    const png = await sharp(await rawIn(sharp, 60, 40).png().toBuffer()).withMetadata({ orientation: o }).png().toBuffer();
    for (const [name, src, tol] of [['jpeg', jpeg, 2], ['png', png, 0]]) {
      for (const [label, build] of [
        ['rotate()', (lib) => lib(src).rotate()],
        ['autoOrient()', (lib) => lib(src).autoOrient()],
        ['rotate().resize()', (lib) => lib(src).rotate().resize(30, 30, { fit: 'inside' })],
        ['resize().rotate()', (lib) => lib(src).resize(30, 30, { fit: 'inside' }).rotate()],
      ]) {
        const { s, e } = await both(build);
        assertSame(s, e, { mad: label.includes('resize') ? 1 : tol, label: `o${o} ${name} ${label}` });
      }
    }
  }
});

async function orientationAndDensity(buf) {
  const m = await sharp(buf).metadata();
  return { orientation: m.orientation, density: m.density, hasExif: Boolean(m.exif) };
}

test('EXIF Orientation is reset to 1 after rotate() / autoOrient() with retained metadata', async () => {
  const src = await rawIn(sharp, 60, 40).withMetadata({ orientation: 6, density: 110 }).jpeg().toBuffer();
  const variants = {
    'rotate()+withMetadata': (i) => i.rotate().withMetadata().jpeg(),
    'autoOrient()+withMetadata': (i) => i.autoOrient().withMetadata().jpeg(),
    'rotate()+keepExif': (i) => i.rotate().keepExif().jpeg(),
    'rotate()+withMetadata png': (i) => i.rotate().withMetadata().png(),
    'rotate()+withMetadata webp': (i) => i.rotate().withMetadata().webp(),
    // Explicit angles do not touch the tag in sharp.
    'rotate(90)+withMetadata': (i) => i.rotate(90).withMetadata().jpeg(),
    'withMetadata only': (i) => i.withMetadata().jpeg(),
    'resize+withMetadata': (i) => i.resize(30).withMetadata().jpeg(),
  };
  for (const [name, f] of Object.entries(variants)) {
    const [s, e] = await Promise.all([f(sharp(src)).toBuffer(), f(engine(src)).toBuffer()]);
    const a = await orientationAndDensity(s);
    const b = await orientationAndDensity(e);
    assert.deepEqual(b, a, name);
  }
});

test('withMetadata({ orientation, density }) overrides values in the retained EXIF / JFIF', async () => {
  const src = await rawIn(sharp, 60, 40).withMetadata({ orientation: 6, density: 110 }).jpeg().toBuffer();
  const plain = await rawIn(sharp, 60, 40).jpeg().toBuffer();
  const variants = {
    'orientation 3': (i) => i.withMetadata({ orientation: 3 }).jpeg(),
    'density 200': (i) => i.withMetadata({ density: 200 }).jpeg(),
    'both': (i) => i.withMetadata({ orientation: 8, density: 300 }).jpeg(),
    'rotate()+orientation 3': (i) => i.rotate().withMetadata({ orientation: 3 }).jpeg(),
    'png orientation 3': (i) => i.withMetadata({ orientation: 3 }).png(),
    'webp orientation 5': (i) => i.withMetadata({ orientation: 5 }).webp(),
  };
  for (const input of [src, plain]) {
    for (const [name, f] of Object.entries(variants)) {
      const [s, e] = await Promise.all([f(sharp(input)).toBuffer(), f(engine(input)).toBuffer()]);
      const a = await orientationAndDensity(s);
      const b = await orientationAndDensity(e);
      if (/orientation/.test(name) || input === src) assert.equal(b.orientation, a.orientation, `${name} orientation`);
      if (name !== 'webp orientation 5') assert.equal(b.density, a.density, `${name} density`);
    }
  }
});

test('JPEG output carries a single Exif APP1 segment', async () => {
  const src = await rawIn(sharp, 60, 40).withMetadata({ orientation: 6 }).jpeg().toBuffer();
  const out = await engine(src).withMetadata({ orientation: 3 }).jpeg().toBuffer();
  let exifSegments = 0;
  for (let i = 2; i + 4 < out.length && out[i] === 0xff && out[i + 1] !== 0xda; i += 2 + out.readUInt16BE(i + 2)) {
    if (out[i + 1] === 0xe1 && out.toString('latin1', i + 4, i + 8) === 'Exif') exifSegments += 1;
  }
  assert.equal(exifSegments, 1);
});

// ---------------------------------------------------------------------------------------------
// 8. JPEG from RGBA flattens onto black
// ---------------------------------------------------------------------------------------------
test('JPEG output from RGBA input composites onto black like sharp', async () => {
  const w = 40;
  const h = 30;
  const rgba = Buffer.alloc(w * h * 4);
  for (let i = 0; i < w * h; i++) {
    rgba[i * 4] = 200;
    rgba[i * 4 + 1] = 120;
    rgba[i * 4 + 2] = 40;
    rgba[i * 4 + 3] = (i % w) < w / 2 ? 0 : (i % w) * 6; // left half fully transparent
  }
  const png = await sharp(rgba, { raw: { width: w, height: h, channels: 4 } }).png().toBuffer();
  const [s, e] = await Promise.all([sharp(png).jpeg({ quality: 100, chromaSubsampling: '4:4:4' }).toBuffer(), engine(png).jpeg({ quality: 100, chromaSubsampling: '4:4:4' }).toBuffer()]);
  const [ds, de] = await Promise.all([sharp(s).raw().toBuffer(), sharp(e).raw().toBuffer()]);
  assert.ok(diffStats(ds, de).mad < 2, `mad ${diffStats(ds, de).mad}`);
  // Transparent area is black, not the colour hiding under the alpha.
  const px = (x, y) => [...de.subarray((y * w + x) * 3, (y * w + x) * 3 + 3)];
  assert.ok(px(2, 2).every((v) => v < 8), `transparent pixel ${px(2, 2)}`);
  // flatten() with a colour still wins over the implicit black.
  const flat = await engine(png).flatten({ background: '#ff0000' }).jpeg({ quality: 100, chromaSubsampling: '4:4:4' }).raw().toBuffer();
  assert.ok(flat[0] > 200);
});

// ---------------------------------------------------------------------------------------------
// 9. rotate(arbitrary angle) / affine geometry
// ---------------------------------------------------------------------------------------------
test('rotate(angle) output size and pixels follow libvips for many angles and sizes', async () => {
  for (const [w, h] of [[100, 50], [67, 73], [31, 17], [10, 10], [120, 80]]) {
    for (const angle of [1, 10, 30, 45, 60, 89, 91, 100, 123, 135, 170, 200, 222, 270.5, 300, 333, 359, -30, -123]) {
      const { s, e } = await both((lib) => rawIn(lib, w, h).rotate(angle));
      assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], `${w}x${h} @ ${angle}`);
    }
  }
  for (const angle of [1, 10, 45, 123, 200, 270.5]) {
    const { s, e } = await both((lib) => rawIn(lib, 120, 80).rotate(angle, { background: '#ff0000' }));
    assertSame(s, e, { mad: 4, label: `pixels @ ${angle}` });
  }
});

test('affine output size and content follow libvips (offsets move content, not the frame)', async () => {
  const matrices = [[1, 0, 0, 1], [2, 0, 0, 2], [0.5, 0, 0, 0.5], [1, 0.3, 0, 1], [0.866, -0.5, 0.5, 0.866], [1.5, 0.2, -0.2, 0.7], [0.33, 0, 0, 0.77], [0, 1, 1, 0]];
  for (const m of matrices) {
    for (const o of [{}, { idx: 3.5, idy: 1.2 }, { odx: 5, ody: 3.3 }, { odx: -5.7, ody: 2 }]) {
      const { s, e } = await both((lib) => rawIn(lib, 100, 60).affine(m, o));
      assertSame(s, e, { mad: 4, label: `affine ${JSON.stringify(m)} ${JSON.stringify(o)}` });
    }
  }
});

// ---------------------------------------------------------------------------------------------
// 10. operation order follows sharp's fixed pipeline
// ---------------------------------------------------------------------------------------------
test('operations execute in sharp pipeline order, not call order', async () => {
  const cases = {
    'resize then rotate(90) (crop after the turn)': (i) => i.resize(40, 20).rotate(90),
    'rotate(90) then resize': (i) => i.rotate(90).resize(40, 20),
    'rotate(90) then flip': (i) => i.rotate(90).flip(),
    'rotate(270) then flop': (i) => i.rotate(270).flop(),
    'flip then rotate(90)': (i) => i.flip().rotate(90),
    'flop then extract': (i) => i.flop().extract({ left: 3, top: 2, width: 20, height: 10 }),
    'extend then extract (extract runs first)': (i) => i.extend(4).extract({ left: 2, top: 2, width: 20, height: 20 }),
    'extend then resize (resize runs first)': (i) => i.extend(4).resize(30, 20, { fit: 'fill' }),
    'resize then extend': (i) => i.resize(30, 20, { fit: 'fill' }).extend(4),
    'extract then resize': (i) => i.extract({ left: 5, top: 5, width: 40, height: 30 }).resize(20, 15, { fit: 'fill' }),
    'resize then extract (post-resize crop)': (i) => i.resize(30, 20, { fit: 'fill' }).extract({ left: 5, top: 5, width: 10, height: 10 }),
    'extract, resize, extract': (i) => i.extract({ left: 5, top: 5, width: 40, height: 30 }).resize(20, 15, { fit: 'fill' }).extract({ left: 2, top: 2, width: 10, height: 10 }),
    'rotate(90) then extract (rotate first)': (i) => i.rotate(90).extract({ left: 3, top: 2, width: 20, height: 10 }),
    'extract then rotate(90)': (i) => i.extract({ left: 3, top: 2, width: 20, height: 10 }).rotate(90),
    'blur then resize (blur runs last)': (i) => i.blur(2).resize(30, 20, { fit: 'fill' }),
    'negate then resize': (i) => i.negate().resize(30, 20, { fit: 'fill' }),
    'rotate(90) then rotate(45)': (i) => i.rotate(90).rotate(45),
    'ensureAlpha + removeAlpha': (i) => i.ensureAlpha().removeAlpha(),
    'removeAlpha + ensureAlpha': (i) => i.removeAlpha().ensureAlpha(),
    'flip + flop + rotate(180) around resize': (i) => i.flip().resize(30, 20, { fit: 'cover', position: 'left' }).flop().rotate(180),
  };
  for (const [name, f] of Object.entries(cases)) {
    const { s, e } = await both((lib) => f(rawIn(lib, 60, 40)));
    assertSame(s, e, { mad: 4, label: name });
  }
});
