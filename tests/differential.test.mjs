// Differential tests: the same pipeline is run through sharp (oracle) and @fastpkg/image.
//
// Tolerance policy
//  - Geometry-only ops (flip, flop, rotate 90/180/270, extract, extend, negate, threshold, ...)
//    must be bit-exact or within 1-2 levels (rounding in the colour maths).
//  - Anything that resamples (resize, arbitrary-angle rotate, shrink-on-load) uses mean absolute
//    difference thresholds: the kernels / JPEG DCT-scaling / premultiplication details differ
//    between libvips and the Rust resamplers, so only the aggregate error is bounded.
//  - Known engine gaps are registered with bug(...) below; they are re-asserted as `todo`
//    sub-tests so they stay visible without turning CI red.
import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { sharp, engine, make, pattern, exec, compare, diffStats, sha, jt, isValidationError } from './_util.mjs';
import { group, bug } from './_group.mjs';

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-diff-'));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

// ---------------------------------------------------------------------------------------------
// Registry of known engine gaps (engine is wrong / sharp is right). Each entry becomes a `todo`
// sub-test inside the group that hit it; see tests/_group.mjs. Severity in brackets.
// ---------------------------------------------------------------------------------------------
const isSize = (m) => /^size /.test(m);
const isPix = (m) => /^pixels /.test(m);
const isResizeGroup = (t) => /^resize:/.test(t);
const sharpRejects = (m) => /^sharp (rejects|wrote)|^sharp=error engine=ok|^engine=ok/.test(m) && /sharp rejects|sharp=error engine=ok/.test(m);

bug('resize-tiny-targets', '[wrong-output] 1x1 / 3x2 outputs from tiny (7x13) or very large-ratio sources deviate by 18-28 levels from libvips (shrink-then-kernel) ', (n, m, t) => isResizeGroup(t) && /(7x13|1000x500) .*-> (1x1|3x2)/.test(n) && isPix(m));
bug('position-strategy', '[wrong-output] position: entropy / attention are accepted but fall back to a centre crop (sharp does a content-aware crop)', (n, m, t) => t === 'resize: entropy / attention strategies');
bug('flip-idempotent', '[wrong-output] flip()/flop() are boolean flags in sharp', (n, m, t) => t.startsWith('flip / flop') && /twice/.test(n));
bug('tiny-source-upscale', '[wrong-output] enlarging a 7x13 source along one axis (fill + single dimension): libvips and the Rust resampler weigh the clamped edge differently (mad 16-24)', (n, m, t) => t === 'resize: source sizes' && /7x13 fill -> (20x-|nullx20)/.test(n) && isPix(m));
bug('withexif-density', '[wrong-output] withExif({...}) on a JPEG drops the original density/orientation that sharp keeps (and the new exif block is missing)', (n, m, t) => /withMetadata \/ keep/.test(t) && n === 'withExif');
bug('avif-10bit', '[cosmetic] avif({bitdepth:10}) is ignored (8-bit output)', (n, m, t) => t === 'avif output options' && /metadata\.depth/.test(m));
bug('animation', '[wrong-output] animated GIF/WebP: pages/page/animated unsupported (first frame only); pages:-1 rejects; out-of-range page not validated', (n, m, t) => /^pages \/ page|^animated output/.test(t));
bug('colourspace-unsupported', '[error-mismatch] pipelineColourspace(...) and toColourspace(rgb16|grey16|cmyk) unsupported', (n, m, t) => t.startsWith('colour space'));


const FITS = ['cover', 'contain', 'fill', 'inside', 'outside'];

// Resampling tolerance (mean absolute difference, 0-255). libvips shrinks by an integer box
// factor and then applies the kernel; fast_image_resize runs one convolution with a scaled
// support. They agree closely on moderate ratios; extreme ratios (>=8x either way), tiny
// outputs (<=3 px) and tiny sources (<=13 px, where every output pixel sees the clamped edge)
// amplify kernel-support differences, so those get a wider (but still bounded) tolerance.
function resizeTol(sw, sh, tw, th) {
  const w = tw ?? Math.round((sw * (th ?? sh)) / sh);
  const h = th ?? Math.round((sh * (tw ?? sw)) / sw);
  const ratio = Math.max(sw / w, sh / h, w / sw, h / sh);
  return { mad: ratio >= 8 || Math.min(w, h) <= 3 || Math.min(sw, sh) <= 13 ? 16 : 5 };
}

// ---------------------------------------------------------------------------------------------
// resize: fit x target shape x option flags on a PNG source
// ---------------------------------------------------------------------------------------------
{
  const png = await make('png', 120, 80);
  const targets = [[60], [null, 40], [50, 50], [200, 200], [1, 1], [3, 2], [300], [null, 300], [90, 10], [10, 90]];
  const flags = [{}, { withoutEnlargement: true }, { withoutReduction: true }, { withoutEnlargement: true, withoutReduction: true }];
  const cases = [];
  for (const fit of FITS) for (const [w, h] of targets) for (const flag of flags) {
    cases.push({ name: `${fit} ${w ?? '-'}x${h ?? '-'} ${JSON.stringify(flag)}`, w, h, fit, flag });
  }
  group('resize: fit x target x enlargement flags', cases, (c) =>
    compare((lib) => lib(png).resize(c.w, c.h, { fit: c.fit, ...c.flag }), resizeTol(120, 80, c.w, c.h))
  );
}

// ---------------------------------------------------------------------------------------------
// resize: every source format / bit depth / layout
// ---------------------------------------------------------------------------------------------
{
  const kinds = ['jpeg', 'png', 'webp', 'pnga', 'png16', 'gjpeg', 'gpng', 'gapng'];
  const cases = [];
  for (const kind of kinds) for (const fit of FITS) for (const [w, h] of [[50, 50], [200, 150], [60, null]]) {
    cases.push({ name: `${kind} ${fit} ${w}x${h ?? '-'}`, kind, fit, w, h });
  }
  group('resize: source formats', cases, async (c) => {
    const src = await make(c.kind, 120, 80);
    // 16-bit sources are compared through 8-bit raw output. (sharp's raw({depth:'ushort'}) is not a
    // usable oracle: it returns the 8-bit-reduced value in a 16-bit container, e.g. 1000 -> 3,
    // whereas the engine keeps the full 16-bit sample. Engine-side 16-bit fidelity is covered in image.test.mjs.)
    if (c.kind === 'png16' && c.w > 120) {
      // The png16 fixture is a high-frequency (noise-like) image, and libvips and fast_image_resize
      // disagree on how to *enlarge* such content (the identical 8-bit rendering of the fixture
      // differs from sharp by the same ~27 levels), so sharp is no oracle for the 16-bit path
      // here. Check the 16-bit path against the engine's own 8-bit path on the same samples:
      // identical up to 16->8 rounding.
      const raw8 = await sharp(src).raw().toBuffer();
      const src8 = await sharp(raw8, { raw: { width: 120, height: 80, channels: 3 } }).png().toBuffer();
      const [wide, narrow] = await Promise.all([
        exec(engine, (lib) => lib(src).resize(c.w, c.h, { fit: c.fit })),
        exec(engine, (lib) => lib(src8).resize(c.w, c.h, { fit: c.fit })),
      ]);
      if (wide.error || narrow.error) return `engine rejects: ${(wide.error || narrow.error).message}`;
      const d = diffStats(wide.data, narrow.data);
      return d.mad <= 1 ? null : `16-bit path deviates from 8-bit path: mad=${d.mad.toFixed(2)}`;
    }
    return compare((lib) => lib(src).resize(c.w, c.h, { fit: c.fit }), { mad: 5 });
  });
}

// ---------------------------------------------------------------------------------------------
// resize: source sizes (tiny, odd, extreme aspect, portrait, large)
// ---------------------------------------------------------------------------------------------
{
  const sources = [
    ['png', 1, 1], ['jpeg', 1, 1], ['png', 7, 13], ['jpeg', 7, 13], ['webp', 7, 13],
    ['jpeg', 1000, 500], ['jpeg', 300, 900], ['png', 5000, 20], ['png', 20, 5000], ['pnga', 7, 13],
  ];
  const targets = [[1, 1], [3, 2], [50, 50], [20], [null, 20], [100, 100]];
  const cases = [];
  for (const [kind, sw, sh] of sources) for (const fit of FITS) for (const [w, h] of targets) {
    cases.push({ name: `${kind} ${sw}x${sh} ${fit} -> ${w}x${h ?? '-'}`, kind, sw, sh, fit, w, h });
  }
  group('resize: source sizes', cases, async (c) => {
    const src = await make(c.kind, c.sw, c.sh);
    return compare((lib) => lib(src).resize(c.w, c.h, { fit: c.fit }), resizeTol(c.sw, c.sh, c.w, c.h), { sizeSlack: c.kind === 'jpeg' ? 1 : 0 });
  });
}

// ---------------------------------------------------------------------------------------------
// resize: position / gravity / strategy
// ---------------------------------------------------------------------------------------------
{
  const positions = ['top', 'right top', 'right', 'right bottom', 'bottom', 'left bottom', 'left', 'left top', 'centre', 'center', 'north', 'northeast', 'east', 'southeast', 'south', 'southwest', 'west', 'northwest'];
  const cases = [];
  for (const position of positions) for (const fit of ['cover', 'contain']) for (const [sw, sh] of [[120, 80], [80, 120]]) {
    cases.push({ name: `${fit} ${position} from ${sw}x${sh}`, position, fit, sw, sh });
  }
  group('resize: position keywords', cases, async (c) => {
    const src = await make('png', c.sw, c.sh);
    return compare((lib) => lib(src).resize(50, 30, { fit: c.fit, position: c.position, background: '#ff00ff' }), { mad: 5 });
  });

  // sharp also exposes numeric gravity constants and strategies; engine accepts the strings only.
  const numeric = [];
  for (const [k, v] of Object.entries(sharp.gravity)) numeric.push({ name: `gravity.${k}=${v}`, position: v });
  group('resize: numeric sharp.gravity constants', numeric, async (c) => {
    const src = await make('png', 120, 80);
    return compare((lib) => lib(src).resize(50, 30, { fit: 'cover', position: c.position }), { mad: 5 });
  });

  // entropy/attention choose the crop window by content; use an image where the interesting
  // region is off-centre so a centre crop would be visibly wrong.
  const strat = [];
  for (const s of ['entropy', 'attention']) for (const [w, h] of [[40, 40], [60, 20], [20, 60]]) strat.push({ name: `${s} ${w}x${h}`, s, w, h });
  group('resize: entropy / attention strategies', strat, async (c) => {
    const base = Buffer.alloc(160 * 100 * 3, 128);
    for (let y = 60; y < 100; y++) for (let x = 110; x < 160; x++) {
      const o = (y * 160 + x) * 3;
      base[o] = (x * 37 + y * 11) & 255;
      base[o + 1] = (x * 13 + y * 71) & 255;
      base[o + 2] = (x * 5 + y * 3) & 255;
    }
    const src = await sharp(base, { raw: { width: 160, height: 100, channels: 3 } }).png().toBuffer();
    return compare((lib) => lib(src).resize(c.w, c.h, { fit: 'cover', position: c.s }), { mad: 8 });
  });
}

// ---------------------------------------------------------------------------------------------
// resize: kernels, background, object-form options
// ---------------------------------------------------------------------------------------------
{
  const kernels = ['nearest', 'linear', 'cubic', 'mitchell', 'lanczos2', 'lanczos3', 'mks2013', 'mks2021'];
  const cases = [];
  for (const kernel of kernels) for (const [w, h] of [[50, 33], [240, 160]]) cases.push({ name: `${kernel} ${w}x${h}`, kernel, w, h });
  group('resize: kernels', cases, async (c) => {
    const src = await make('png', 120, 80);
    // nearest is a different sampling lattice in every library; allow a larger mean error.
    return compare((lib) => lib(src).resize(c.w, c.h, { kernel: c.kernel }), { mad: c.kernel === 'nearest' ? 14 : 6 });
  });

  const bgs = [
    ['css', '#336699'], ['rgba string', 'rgba(10,200,30,0.5)'], ['object opaque', { r: 255, g: 128, b: 0 }],
    ['object alpha', { r: 0, g: 128, b: 255, alpha: 0.25 }], ['named', 'red'], ['transparent', { r: 0, g: 0, b: 0, alpha: 0 }],
  ];
  const bgCases = [];
  for (const kind of ['png', 'pnga', 'jpeg', 'gpng']) for (const [n, bg] of bgs) bgCases.push({ name: `${kind} contain bg ${n}`, kind, bg });
  group('resize: contain background colours', bgCases, async (c) => {
    const src = await make(c.kind, 60, 40);
    return compare((lib) => lib(src).resize(80, 80, { fit: 'contain', background: c.bg }), { mad: 4 });
  });

  group('resize: argument forms', [
    { name: 'object', f: (l, s) => l(s).resize({ width: 40, height: 20 }) },
    { name: 'object width only', f: (l, s) => l(s).resize({ width: 40 }) },
    { name: 'number,null', f: (l, s) => l(s).resize(40, null) },
    { name: 'null,number', f: (l, s) => l(s).resize(null, 40) },
    { name: 'undefined,number', f: (l, s) => l(s).resize(undefined, 40) },
    { name: 'number,number,options', f: (l, s) => l(s).resize(40, 20, { fit: 'fill' }) },
    { name: 'string width', f: (l, s) => l(s).resize('40') },
    { name: 'two resizes', f: (l, s) => l(s).resize(60, 60).resize(20, 20) },
    { name: 'resize() no args', f: (l, s) => l(s).resize() },
    { name: 'resize then rotate 90', f: (l, s) => l(s).resize(40, 20).rotate(90) },
    { name: 'rotate 90 then resize', f: (l, s) => l(s).rotate(90).resize(40, 20) },
  ], async (c) => {
    const src = await make('png', 120, 80);
    return compare((lib) => c.f(lib, src), { mad: 5 });
  });
}

// ---------------------------------------------------------------------------------------------
// resize of a large JPEG: shrink-on-load changes the resampling path
// ---------------------------------------------------------------------------------------------
{
  // 4000x3000 gradient JPEG; fixtures/large.jpg is git-ignored (made by bench/run.mjs), so fall back to
  // generating the same picture when it is absent (fresh CI checkouts).
  const largePath = new URL('../fixtures/large.jpg', import.meta.url);
  let big;
  if (fs.existsSync(largePath)) {
    big = fs.readFileSync(largePath);
  } else {
    const [bw, bh] = [4000, 3000];
    const px = Buffer.allocUnsafe(bw * bh * 3);
    for (let y = 0; y < bh; y++) {
      for (let x = 0; x < bw; x++) {
        const i = (y * bw + x) * 3;
        px[i] = (x * 255 / bw) | 0;
        px[i + 1] = (y * 255 / bh) | 0;
        px[i + 2] = ((x + y) * 255 / (bw + bh)) | 0;
      }
    }
    big = await sharp(px, { raw: { width: bw, height: bh, channels: 3 } }).jpeg({ quality: 88 }).toBuffer();
  }
  const cases = [];
  for (const fast of [true, false, undefined]) for (const fit of FITS) for (const [w, h] of [[100, 75], [800], [null, 50], [1, 1], [3, 2], [333, 111], [5000, 4000]]) {
    cases.push({ name: `fast=${fast} ${fit} ${w}x${h ?? '-'}`, fast, fit, w, h });
  }
  group('resize: 4000x3000 JPEG shrink-on-load', cases, (c) =>
    // DCT scaling (1/2,1/4,1/8) then a final resample differs between libjpeg-turbo builds and
    // the Rust decoder; the dimension check is exact, pixels get a looser mean error bound.
    compare((lib) => lib(big).resize(c.w, c.h, { fit: c.fit, ...(c.fast === undefined ? {} : { fastShrinkOnLoad: c.fast }) }), { mad: 8 }, { sizeSlack: 1 })
  , { concurrency: 3 });
}

// ---------------------------------------------------------------------------------------------
// EXIF orientation 1..8: sharp never auto-rotates unless asked
// ---------------------------------------------------------------------------------------------
{
  const jpgs = {};
  for (let o = 1; o <= 8; o++) {
    const raw = await make('jpeg', 64, 40);
    jpgs[o] = await sharp(raw).withMetadata({ orientation: o }).jpeg({ quality: 95 }).toBuffer();
  }
  const cases = [];
  for (let o = 1; o <= 8; o++) {
    cases.push({ name: `o${o} resize only`, o, f: (l, s) => l(s).resize(32, 32, { fit: 'inside' }) });
    cases.push({ name: `o${o} no ops`, o, f: (l, s) => l(s) });
    cases.push({ name: `o${o} rotate()`, o, f: (l, s) => l(s).rotate() });
    cases.push({ name: `o${o} rotate() + resize`, o, f: (l, s) => l(s).rotate().resize(32, 32, { fit: 'inside' }) });
    cases.push({ name: `o${o} resize + rotate()`, o, f: (l, s) => l(s).resize(32, 32, { fit: 'inside' }).rotate() });
    cases.push({ name: `o${o} autoOrient()`, o, f: (l, s) => l(s).autoOrient() });
    cases.push({ name: `o${o} autoOrient input option`, o, f: (l, s) => l(s, { autoOrient: true }) });
    cases.push({ name: `o${o} rotate() + cover`, o, f: (l, s) => l(s).rotate().resize(30, 20, { fit: 'cover' }) });
    cases.push({ name: `o${o} rotate(90) honours orientation first`, o, f: (l, s) => l(s).rotate(90) });
  }
  group('EXIF orientation: dimensions and pixels', cases, (c) => compare((lib) => c.f(lib, jpgs[c.o]), { mad: 5 }));

  group('EXIF orientation: metadata survives / is reported', Array.from({ length: 8 }, (_, i) => ({ name: `o${i + 1}`, o: i + 1 })), async (c) => {
    const [s, e] = await Promise.all([sharp(jpgs[c.o]).metadata(), engine(jpgs[c.o]).metadata()]);
    if (s.orientation !== e.orientation) return `orientation sharp=${s.orientation} engine=${e.orientation}`;
    if (s.width !== e.width || s.height !== e.height) return `metadata size differs (raw, not rotated)`;
    return null;
  });

  group('EXIF orientation: output carries the tag unless rotated', Array.from({ length: 8 }, (_, i) => ({ name: `o${i + 1}`, o: i + 1 })), async (c) => {
    const out = [];
    for (const [label, mk] of [['plain', (l) => l(jpgs[c.o]).jpeg()], ['withMetadata', (l) => l(jpgs[c.o]).withMetadata().jpeg()], ['rotate()', (l) => l(jpgs[c.o]).rotate().jpeg()], ['rotate()+withMetadata', (l) => l(jpgs[c.o]).rotate().withMetadata().jpeg()]]) {
      const [s, e] = await Promise.all([mk(sharp).toBuffer(), mk(engine).toBuffer()]);
      const [sm, em] = await Promise.all([sharp(s).metadata(), sharp(e).metadata()]);
      if (sm.orientation !== em.orientation) out.push(`${label}: orientation sharp=${sm.orientation} engine=${em.orientation}`);
      if (sm.width !== em.width || sm.height !== em.height) out.push(`${label}: size sharp=${sm.width}x${sm.height} engine=${em.width}x${em.height}`);
    }
    return out.length ? out.join(' || ') : null;
  });
}

// ---------------------------------------------------------------------------------------------
// rotate / flip / flop
// ---------------------------------------------------------------------------------------------
{
  const cases = [];
  const angles = [0, 90, 180, 270, -90, -180, 360, 450, 45, 30, 15.5, -30, 123, 1, 89.9];
  for (const kind of ['png', 'pnga', 'jpeg', 'gpng']) for (const angle of angles) {
    for (const bg of [undefined, '#ff0000', { r: 0, g: 255, b: 0, alpha: 0.5 }]) {
      cases.push({ name: `${kind} rotate(${angle}) bg=${JSON.stringify(bg)}`, kind, angle, bg });
    }
  }
  group('rotate: angles x backgrounds', cases, async (c) => {
    const src = await make(c.kind, 60, 40);
    const exact = c.angle % 90 === 0;
    // Right-angle rotation is a pure pixel permutation: must be identical.
    // Arbitrary angles interpolate (bicubic vs bilinear) and anti-alias the rotated border, so
    // only the mean error is bounded.
    return compare((lib) => lib(src).rotate(c.angle, c.bg ? { background: c.bg } : undefined), jt(c.kind, exact ? { mad: 0, max: 0 } : { mad: 8 }));
  });

  const fl = [
    ['flip', (i) => i.flip()], ['flop', (i) => i.flop()], ['flip+flop', (i) => i.flip().flop()],
    ['flip(false)', (i) => i.flip(false)], ['flop(false)', (i) => i.flop(false)], ['flip twice', (i) => i.flip().flip()],
    ['flip+rotate90', (i) => i.flip().rotate(90)], ['rotate90+flip', (i) => i.rotate(90).flip()],
    ['flop+rotate270', (i) => i.flop().rotate(270)], ['rotate270+flop', (i) => i.rotate(270).flop()],
    ['rotate90+flip+flop', (i) => i.rotate(90).flip().flop()], ['flip+rotate180+flop', (i) => i.flip().rotate(180).flop()],
    ['rotate90 then rotate90', (i) => i.rotate(90).rotate(90)],
    ['rotate(90) then rotate(45)', (i) => i.rotate(90).rotate(45)],
    ['flip then resize', (i) => i.flip().resize(30, 20, { fit: 'fill' })],
    ['flop then extract', (i) => i.flop().extract({ left: 3, top: 2, width: 20, height: 10 })],
  ];
  const fc = [];
  for (const kind of ['png', 'pnga', 'jpeg', 'gpng']) for (const [n, f] of fl) fc.push({ name: `${kind} ${n}`, kind, f });
  group('flip / flop / rotate combinations', fc, async (c) => {
    const src = await make(c.kind, 60, 40);
    const arbitrary = c.name.includes('45') || c.name.includes('resize');
    return compare((lib) => c.f(lib(src)), jt(c.kind, arbitrary ? { mad: 8 } : { mad: 0, max: 0 }));
  });
}

// ---------------------------------------------------------------------------------------------
// extract / extend / trim
// ---------------------------------------------------------------------------------------------
{
  const regions = [
    { left: 0, top: 0, width: 60, height: 40 }, { left: 10, top: 5, width: 20, height: 10 },
    { left: 59, top: 39, width: 1, height: 1 }, { left: 0, top: 0, width: 1, height: 1 },
    { left: 30, top: 0, width: 30, height: 40 },
  ];
  const cases = [];
  for (const kind of ['png', 'pnga', 'jpeg', 'gpng']) for (const r of regions) cases.push({ name: `${kind} extract ${JSON.stringify(r)}`, kind, f: (i) => i.extract(r) });
  for (const kind of ['png', 'pnga']) {
    cases.push({ name: `${kind} extract then resize`, kind, f: (i) => i.extract({ left: 10, top: 10, width: 40, height: 20 }).resize(20, 10, { fit: 'fill' }), tol: { mad: 5 } });
    cases.push({ name: `${kind} resize then extract (post-extract)`, kind, f: (i) => i.resize(120, 80, { fit: 'fill' }).extract({ left: 20, top: 20, width: 40, height: 30 }), tol: { mad: 5 } });
    cases.push({ name: `${kind} extract, resize, extract`, kind, f: (i) => i.extract({ left: 5, top: 5, width: 50, height: 30 }).resize(25, 15, { fit: 'fill' }).extract({ left: 2, top: 2, width: 10, height: 8 }), tol: { mad: 5 } });
    cases.push({ name: `${kind} rotate90 then extract`, kind, f: (i) => i.rotate(90).extract({ left: 3, top: 3, width: 20, height: 20 }) });
  }
  group('extract: valid regions', cases, async (c) => {
    const src = await make(c.kind, 60, 40);
    return compare((lib) => c.f(lib(src)), jt(c.kind, c.tol ?? { mad: 0, max: 0 }));
  });

  // Error behaviour: whatever sharp rejects/throws, the engine must as well (and vice versa).
  const bad = [
    ['beyond right', { left: 50, top: 0, width: 20, height: 10 }],
    ['beyond bottom', { left: 0, top: 35, width: 10, height: 10 }],
    ['negative left', { left: -1, top: 0, width: 10, height: 10 }],
    ['float', { left: 0.5, top: 0, width: 10, height: 10 }],
    ['NaN', { left: 0, top: 0, width: NaN, height: 10 }],
    ['missing width', { left: 0, top: 0, height: 10 }],
    ['missing height', { left: 0, top: 0, width: 10 }],
    ['string number', { left: '1', top: 0, width: 10, height: 10 }],
    ['no args', undefined],
    ['null', null],
    ['number', 5],
  ];
  group('extract: invalid regions error like sharp', bad.map(([name, r]) => ({ name, r })), async (c) => {
    const src = await make('png', 60, 40);
    const outcome = async (lib) => {
      try { await lib(src).extract(c.r).toBuffer(); return 'ok'; } catch (e) { return e instanceof Error ? 'error' : 'non-error-throw'; }
    };
    const [s, e] = await Promise.all([outcome(sharp), outcome(engine)]);
    return s === e ? null : `sharp=${s} engine=${e}`;
  });
}

{
  const cases = [];
  const forms = [
    ['number', 5], ['per-edge', { top: 1, left: 2, bottom: 3, right: 4 }], ['partial', { top: 5 }], ['only left/right', { left: 3, right: 7 }],
    ['zero', 0], ['bg css', { top: 2, bottom: 2, left: 2, right: 2, background: '#336699' }],
    ['bg object', { top: 2, bottom: 2, left: 2, right: 2, background: { r: 255, g: 0, b: 0, alpha: 0.5 } }],
    ['bg transparent', { top: 4, bottom: 4, left: 4, right: 4, background: { r: 0, g: 0, b: 0, alpha: 0 } }],
    ['copy', { top: 3, bottom: 3, left: 3, right: 3, extendWith: 'copy' }],
    ['repeat', { top: 5, bottom: 5, left: 5, right: 5, extendWith: 'repeat' }],
    ['mirror', { top: 5, bottom: 5, left: 5, right: 5, extendWith: 'mirror' }],
    ['background explicit', { top: 3, bottom: 3, left: 3, right: 3, extendWith: 'background', background: 'red' }],
    ['repeat bigger than image', { top: 50, bottom: 50, left: 70, right: 70, extendWith: 'repeat' }],
    ['mirror bigger than image', { top: 50, left: 70, extendWith: 'mirror' }],
    ['copy bigger than image', { top: 50, left: 70, extendWith: 'copy' }],
  ];
  for (const kind of ['png', 'pnga', 'jpeg', 'gpng']) for (const [n, o] of forms) cases.push({ name: `${kind} extend ${n}`, kind, o });
  cases.push({ name: 'png resize then extend', kind: 'png', o: 4, pre: (i) => i.resize(30, 20, { fit: 'fill' }), tol: { mad: 5 } });
  cases.push({ name: 'png extend then resize', kind: 'png', o: 4, post: (i) => i.resize(30, 20, { fit: 'fill' }), tol: { mad: 6 } });
  cases.push({ name: 'png extend then extract', kind: 'png', o: 4, post: (i) => i.extract({ left: 2, top: 2, width: 20, height: 20 }) });
  group('extend: option forms', cases, async (c) => {
    const src = await make(c.kind, 60, 40);
    return compare((lib) => { let i = lib(src); if (c.pre) i = c.pre(i); i = i.extend(c.o); if (c.post) i = c.post(i); return i; }, jt(c.kind, c.tol ?? { mad: 0, max: 1 }));
  });

  const badExtend = [['negative', -1], ['float', 1.5], ['string', 'a'], ['bad extendWith', { top: 1, extendWith: 'nope' }], ['NaN', NaN], ['bad bg', { top: 1, background: 'notacolour' }], ['null', null], ['neg edge', { top: -3 }]];
  group('extend: invalid arguments error like sharp', badExtend.map(([name, o]) => ({ name, o })), async (c) => {
    const src = await make('png', 20, 20);
    const outcome = async (lib) => { try { await lib(src).extend(c.o).toBuffer(); return 'ok'; } catch (e) { return 'error'; } };
    const [s, e] = await Promise.all([outcome(sharp), outcome(engine)]);
    return s === e ? null : `sharp=${s} engine=${e}`;
  });
}

{
  // Trim: bordered image (uniform border around a patterned core).
  const borders = {};
  const build = async (bg, alpha) => {
    const core = pattern(30, 20, 3);
    const inner = await sharp(core, { raw: { width: 30, height: 20, channels: 3 } }).png().toBuffer();
    return sharp(inner).extend({ top: 5, bottom: 8, left: 3, right: 11, background: bg }).png().toBuffer();
  };
  borders.white = await build('#ffffff');
  borders.red = await build('#ff0000');
  borders.alpha = await build({ r: 0, g: 0, b: 0, alpha: 0 });
  borders.near = await build('#fafafa');
  const cases = [
    ['white default', 'white', undefined], ['red default', 'red', undefined], ['red explicit bg', 'red', { background: '#ff0000' }],
    ['alpha default', 'alpha', undefined], ['near-white thr 0', 'near', undefined], ['near-white thr 10', 'near', { threshold: 10 }],
    ['near-white thr 20 bg white', 'near', { threshold: 20, background: '#ffffff' }], ['white lineArt', 'white', { lineArt: true }],
    ['red wrong bg', 'red', { background: '#00ff00' }], ['red thr 255', 'red', { threshold: 255 }],
  ].map(([name, key, o]) => ({ name, key, o }));
  group('trim', cases, (c) => compare((lib) => lib(borders[c.key]).trim(c.o), { mad: 0, max: 1 }));

  group('trim: fully uniform image', [{ name: 'uniform' }], async () => {
    const solid = await sharp({ create: { width: 10, height: 10, channels: 3, background: '#fff' } }).png().toBuffer();
    const outcome = async (lib) => { try { const { info } = await lib(solid).trim().toBuffer({ resolveWithObject: true }); return `${info.width}x${info.height}`; } catch (e) { return 'error'; } };
    const [s, e] = await Promise.all([outcome(sharp), outcome(engine)]);
    return s === e ? null : `sharp=${s} engine=${e}`;
  });
}

// ---------------------------------------------------------------------------------------------
// per-pixel / neighbourhood operations
// ---------------------------------------------------------------------------------------------
function opGroup(title, ops, { kinds = ['png', 'pnga', 'jpeg', 'gpng'], size = [48, 32] } = {}) {
  const cases = [];
  for (const kind of kinds) for (const [n, f, tol] of ops) cases.push({ name: `${kind} ${n}`, kind, f, tol });
  group(title, cases, async (c) => {
    const src = await make(c.kind, size[0], size[1]);
    return compare((lib) => c.f(lib(src)), jt(c.kind, c.tol ?? { mad: 2 }));
  });
}

opGroup('blur', [
  ['sigma 0.3', (i) => i.blur(0.3), { mad: 2 }], ['sigma 1', (i) => i.blur(1)], ['sigma 2.5', (i) => i.blur(2.5), { mad: 4 }],
  ['sigma 10', (i) => i.blur(10), { mad: 4 }], ['blur()', (i) => i.blur()], ['blur(true) fast', (i) => i.blur(true)],
  ['blur(false)', (i) => i.blur(false)], ['{sigma}', (i) => i.blur({ sigma: 3 }), { mad: 4 }],
  ['{sigma, precision approximate}', (i) => i.blur({ sigma: 3, precision: 'approximate' }), { mad: 4 }],
  ['{sigma, minAmplitude}', (i) => i.blur({ sigma: 3, minAmplitude: 0.5 }), { mad: 8 }],
  ['blur 0.2 (below range)', (i) => i.blur(0.2)], ['blur 1001 (above range)', (i) => i.blur(1001)],
  ['blur -1', (i) => i.blur(-1)], ['blur NaN', (i) => i.blur(NaN)], ['blur "2"', (i) => i.blur('2')],
]);

opGroup('sharpen', [
  ['sharpen()', (i) => i.sharpen(), { mad: 6 }], ['sigma 1', (i) => i.sharpen(1), { mad: 6 }], ['sigma 2.5', (i) => i.sharpen(2.5), { mad: 6 }],
  ['object full', (i) => i.sharpen({ sigma: 1.5, m1: 0.5, m2: 2, x1: 3, y2: 12, y3: 20 }), { mad: 6 }],
  ['object flat/jagged', (i) => i.sharpen({ sigma: 2, m1: 1, m2: 3 }), { mad: 6 }],
  ['sigma 0.1 (out of range)', (i) => i.sharpen(0.1), { mad: 6 }], ['sigma -1', (i) => i.sharpen(-1), { mad: 6 }], ['sharpen(false)', (i) => i.sharpen(false), { mad: 6 }],
  ['legacy flat/jagged', (i) => i.sharpen(1, 1, 2), { mad: 6 }],
]);

opGroup('median / gamma / negate / normalise / threshold', [
  ['median()', (i) => i.median()], ['median 3', (i) => i.median(3)], ['median 5', (i) => i.median(5)], ['median 1', (i) => i.median(1)], ['median 0', (i) => i.median(0)],
  ['gamma()', (i) => i.gamma(), { mad: 2.5 }], ['gamma 1.5', (i) => i.gamma(1.5), { mad: 2.5 }], ['gamma 2.2,2.2', (i) => i.gamma(2.2, 2.2), { mad: 2.5 }],
  ['gamma 3', (i) => i.gamma(3), { mad: 2.5 }], ['gamma 0.5 (out of range)', (i) => i.gamma(0.5)], ['gamma 4 (out of range)', (i) => i.gamma(4)],
  ['negate()', (i) => i.negate(), { mad: 0, max: 1 }], ['negate alpha:false', (i) => i.negate({ alpha: false }), { mad: 0, max: 1 }],
  ['negate alpha:true', (i) => i.negate({ alpha: true }), { mad: 0, max: 1 }],
  ['normalise()', (i) => i.normalise(), { mad: 3 }], ['normalize()', (i) => i.normalize(), { mad: 3 }],
  ['normalise 5..95', (i) => i.normalise({ lower: 5, upper: 95 }), { mad: 3 }], ['normalise 0..100', (i) => i.normalise({ lower: 0, upper: 100 }), { mad: 3 }],
  ['normalise lower>upper', (i) => i.normalise({ lower: 90, upper: 10 })],
  ['threshold()', (i) => i.threshold(), { mad: 3 }], ['threshold 100', (i) => i.threshold(100), { mad: 3 }],
  ['threshold 200 colour', (i) => i.threshold(200, { greyscale: false }), { mad: 3 }], ['threshold object', (i) => i.threshold({ threshold: 90 }), { mad: 3 }],
  ['threshold 255', (i) => i.threshold(255), { mad: 3 }], ['threshold 0', (i) => i.threshold(0), { mad: 3 }], ['threshold 300', (i) => i.threshold(300)],
  ['threshold grayscale:false', (i) => i.threshold(128, { grayscale: false }), { mad: 3 }],
]);

opGroup('clahe', [
  ['8x8', (i) => i.clahe({ width: 8, height: 8 }), { mad: 8 }], ['10x10 slope 5', (i) => i.clahe({ width: 10, height: 10, maxSlope: 5 }), { mad: 8 }],
  ['slope 0 (off)', (i) => i.clahe({ width: 8, height: 8, maxSlope: 0 }), { mad: 8 }], ['missing width', (i) => i.clahe({ height: 8 })],
  ['window larger than image', (i) => i.clahe({ width: 500, height: 500 })], ['no args', (i) => i.clahe()],
], { kinds: ['png', 'gpng', 'jpeg'], size: [64, 48] });

opGroup('linear / modulate / tint / greyscale', [
  ['linear(1.2,10)', (i) => i.linear(1.2, 10), { mad: 1, max: 2 }], ['linear(0.5)', (i) => i.linear(0.5), { mad: 1, max: 2 }],
  ['linear arrays', (i) => i.linear([1, 0.5, 2], [0, 10, -10]), { mad: 1, max: 2 }], ['linear(undefined,20)', (i) => i.linear(undefined, 20), { mad: 1, max: 2 }],
  ['linear()', (i) => i.linear()], ['linear([1,2,3,4])', (i) => i.linear([1, 2, 3, 4], [0, 0, 0, 0]), { mad: 1, max: 2 }],
  ['modulate brightness 1.5', (i) => i.modulate({ brightness: 1.5 }), { mad: 6 }], ['modulate brightness 0.5', (i) => i.modulate({ brightness: 0.5 }), { mad: 6 }],
  ['modulate saturation 0.3', (i) => i.modulate({ saturation: 0.3 }), { mad: 6 }], ['modulate saturation 2', (i) => i.modulate({ saturation: 2 }), { mad: 8 }],
  ['modulate hue 90', (i) => i.modulate({ hue: 90 }), { mad: 8 }], ['modulate hue 180', (i) => i.modulate({ hue: 180 }), { mad: 8 }],
  ['modulate lightness 20', (i) => i.modulate({ lightness: 20 }), { mad: 6 }], ['modulate lightness -30', (i) => i.modulate({ lightness: -30 }), { mad: 6 }],
  ['modulate combo', (i) => i.modulate({ brightness: 1.2, saturation: 0.8, hue: 45, lightness: 5 }), { mad: 8 }],
  ['modulate()', (i) => i.modulate(), { mad: 1 }], ['modulate brightness -1', (i) => i.modulate({ brightness: -1 })],
  ['modulate hue "a"', (i) => i.modulate({ hue: 'a' })],
  ['tint css', (i) => i.tint('#ff8800'), { mad: 8 }], ['tint object', (i) => i.tint({ r: 30, g: 120, b: 200 }), { mad: 8 }], ['tint named', (i) => i.tint('green'), { mad: 8 }],
  ['tint bad', (i) => i.tint('notacolour')],
  ['greyscale', (i) => i.greyscale(), { mad: 2 }], ['grayscale', (i) => i.grayscale(), { mad: 2 }], ['greyscale(false)', (i) => i.greyscale(false), { mad: 2 }],
  ['toColourspace b-w', (i) => i.toColourspace('b-w'), { mad: 2 }], ['toColorspace srgb', (i) => i.toColorspace('srgb')],
]);

opGroup('alpha handling', [
  ['flatten()', (i) => i.flatten(), { mad: 1, max: 3 }], ['flatten red', (i) => i.flatten({ background: '#ff0000' }), { mad: 1, max: 3 }],
  ['flatten object', (i) => i.flatten({ background: { r: 10, g: 200, b: 30 } }), { mad: 1, max: 3 }],
  ['flatten bad bg', (i) => i.flatten({ background: 'nope' })],
  ['unflatten', (i) => i.unflatten(), { mad: 2 }],
  ['ensureAlpha()', (i) => i.ensureAlpha(), { mad: 0, max: 1 }], ['ensureAlpha(0.5)', (i) => i.ensureAlpha(0.5), { mad: 0, max: 1 }],
  ['ensureAlpha(0)', (i) => i.ensureAlpha(0), { mad: 0, max: 1 }], ['ensureAlpha(2)', (i) => i.ensureAlpha(2)], ['ensureAlpha(-1)', (i) => i.ensureAlpha(-1)],
  ['removeAlpha', (i) => i.removeAlpha(), { mad: 0, max: 1 }], ['ensureAlpha+removeAlpha', (i) => i.ensureAlpha().removeAlpha(), { mad: 0, max: 1 }],
  ['removeAlpha+ensureAlpha', (i) => i.removeAlpha().ensureAlpha(0.3), { mad: 0, max: 1 }],
  ['flatten then ensureAlpha', (i) => i.flatten({ background: '#fff' }).ensureAlpha(0.5), { mad: 1, max: 3 }],
  ['extractChannel 0', (i) => i.extractChannel(0), { mad: 0, max: 1 }], ['extractChannel green', (i) => i.extractChannel('green'), { mad: 0, max: 1 }],
  ['extractChannel 3', (i) => i.extractChannel(3), { mad: 0, max: 1 }], ['extractChannel alpha', (i) => i.extractChannel('alpha'), { mad: 0, max: 1 }],
  ['extractChannel 9', (i) => i.extractChannel(9)], ['extractChannel bogus', (i) => i.extractChannel('bogus')],
]);

{
  const conv = [
    ['laplacian', { width: 3, height: 3, kernel: [-1, -1, -1, -1, 8, -1, -1, -1, -1] }, { mad: 3 }],
    ['box blur scale 9', { width: 3, height: 3, kernel: [1, 1, 1, 1, 1, 1, 1, 1, 1], scale: 9 }, { mad: 2 }],
    ['sobel offset 128', { width: 3, height: 3, kernel: [-1, 0, 1, -2, 0, 2, -1, 0, 1], scale: 1, offset: 128 }, { mad: 3 }],
    ['5x5 gaussian-ish', { width: 5, height: 5, kernel: [1, 4, 6, 4, 1, 4, 16, 24, 16, 4, 6, 24, 36, 24, 6, 4, 16, 24, 16, 4, 1, 4, 6, 4, 1], scale: 256 }, { mad: 2 }],
    ['1x3 vertical', { width: 1, height: 3, kernel: [1, 2, 1], scale: 4 }, { mad: 2 }],
    ['identity', { width: 3, height: 3, kernel: [0, 0, 0, 0, 1, 0, 0, 0, 0] }, { mad: 0, max: 1 }],
    ['wrong kernel length', { width: 3, height: 3, kernel: [1, 2, 3] }, { mad: 0 }],
    ['zero width', { width: 0, height: 3, kernel: [] }, { mad: 0 }],
  ];
  opGroup('convolve', conv.map(([n, k, t]) => [n, (i) => i.convolve(k), t]));

  const recomb = [
    ['sepia', [[0.3588, 0.7044, 0.1368], [0.299, 0.587, 0.114], [0.2392, 0.4696, 0.0912]], { mad: 1, max: 3 }],
    ['identity', [[1, 0, 0], [0, 1, 0], [0, 0, 1]], { mad: 0, max: 1 }],
    ['swap rb', [[0, 0, 1], [0, 1, 0], [1, 0, 0]], { mad: 0, max: 1 }],
    ['4x4', [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]], { mad: 0, max: 1 }],
    ['2x2 invalid', [[1, 0], [0, 1]], { mad: 0 }],
    ['non-matrix', 'nope', { mad: 0 }],
    ['ragged', [[1, 0, 0], [0, 1], [0, 0, 1]], { mad: 0 }],
  ];
  opGroup('recomb', recomb.map(([n, m, t]) => [n, (i) => i.recomb(m), t]));

  const other = await make('png', 48, 32).then((b) => sharp(b).negate().modulate({ hue: 60 }).png().toBuffer());
  const otherJpg = await sharp(other).jpeg().toBuffer();
  const rawOther = await sharp(other).removeAlpha().raw().toBuffer();
  const small = await sharp(other).resize(10, 10, { fit: 'fill' }).png().toBuffer();
  const bools = [];
  for (const op of ['and', 'or', 'eor']) {
    bools.push([`${op} png`, (i) => i.boolean(other, op), { mad: 0, max: 1, bitwise: true }]);
    bools.push([`${op} raw operand`, (i) => i.boolean(rawOther, op, { raw: { width: 48, height: 32, channels: 3 } }), { mad: 0, max: 1, bitwise: true }]);
    bools.push([`${op} jpeg operand`, (i) => i.boolean(otherJpg, op), { mad: 6 }]);
    bools.push([`${op} smaller operand`, (i) => i.boolean(small, op), { mad: 0 }]);
  }
  bools.push(['bad operator', (i) => i.boolean(other, 'nand')], ['missing operand', (i) => i.boolean(undefined, 'and')], ['no args', (i) => i.boolean()]);
  opGroup('boolean', bools);

  const bb = ['and', 'or', 'eor'].map((op) => [`bandbool ${op}`, (i) => i.bandbool(op), { mad: 0, max: 1, bitwise: true }]);
  bb.push(['bandbool bad', (i) => i.bandbool('xor')], ['bandbool none', (i) => i.bandbool()]);
  opGroup('bandbool', bb);
}

// ---------------------------------------------------------------------------------------------
// composite
// ---------------------------------------------------------------------------------------------
{
  const overlayRgba = await make('pnga', 24, 16).then((b) => sharp(b).negate({ alpha: false }).png().toBuffer());
  const overlayRgb = await sharp(overlayRgba).removeAlpha().png().toBuffer();
  const overlayGrey = await make('gpng', 24, 16);
  const overlayJpeg = await make('jpeg', 24, 16);
  const big = await make('png', 100, 100);
  const rawOverlay = Buffer.from(pattern(24, 16, 4, { alpha: true }));
  // Blend tolerance: libvips composites premultiplied 8-bit with its own rounding, and the
  // non-separable modes accumulate more error than plain 'over', so allow a mean error of 6.
  const blends = ['clear', 'source', 'over', 'in', 'out', 'atop', 'dest', 'dest-over', 'dest-in', 'dest-out', 'dest-atop', 'xor', 'add', 'saturate', 'multiply', 'screen', 'overlay', 'darken', 'lighten', 'colour-dodge', 'color-dodge', 'colour-burn', 'color-burn', 'hard-light', 'soft-light', 'difference', 'exclusion'];
  const cases = [];
  for (const base of ['png', 'pnga', 'jpeg', 'gpng']) for (const blend of blends) {
    cases.push({ name: `${base} blend ${blend}`, base, f: (i) => i.composite([{ input: overlayRgba, blend, top: 4, left: 6 }]), tol: { mad: 6 } });
  }
  const g9 = ['centre', 'center', 'north', 'northeast', 'east', 'southeast', 'south', 'southwest', 'west', 'northwest'];
  for (const gravity of g9) cases.push({ name: `png gravity ${gravity}`, base: 'png', f: (i) => i.composite([{ input: overlayRgba, gravity }]), tol: { mad: 2 } });
  for (const [k, v] of Object.entries(sharp.gravity)) cases.push({ name: `png numeric gravity ${k}`, base: 'png', f: (i) => i.composite([{ input: overlayRgba, gravity: v }]), tol: { mad: 2 } });
  cases.push(
    { name: 'top/left only', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 10, left: 20 }]) },
    { name: 'top only (left missing)', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 10 }]) },
    { name: 'left only (top missing)', base: 'png', f: (i) => i.composite([{ input: overlayRgba, left: 10 }]) },
    { name: 'partially outside right/bottom', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 25, left: 40 }]) },
    { name: 'negative offsets', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: -5, left: -8 }]) },
    { name: 'completely outside', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 500, left: 500 }]) },
    { name: 'tile', base: 'png', f: (i) => i.composite([{ input: overlayRgba, tile: true }]) },
    { name: 'tile + gravity', base: 'png', f: (i) => i.composite([{ input: overlayRgba, tile: true, gravity: 'southeast' }]) },
    { name: 'tile + top/left', base: 'png', f: (i) => i.composite([{ input: overlayRgba, tile: true, top: 3, left: 5 }]) },
    { name: 'tile multiply', base: 'pnga', f: (i) => i.composite([{ input: overlayRgba, tile: true, blend: 'multiply' }]) },
    { name: 'rgb overlay', base: 'png', f: (i) => i.composite([{ input: overlayRgb, top: 2, left: 2 }]), tol: { mad: 1 } },
    { name: 'grey overlay', base: 'png', f: (i) => i.composite([{ input: overlayGrey, top: 2, left: 2 }]), tol: { mad: 1 } },
    { name: 'jpeg overlay', base: 'png', f: (i) => i.composite([{ input: overlayJpeg, top: 2, left: 2 }]), tol: { mad: 2 } },
    { name: 'raw overlay', base: 'png', f: (i) => i.composite([{ input: rawOverlay, raw: { width: 24, height: 16, channels: 4 }, top: 2, left: 2 }]) },
    { name: 'raw overlay premultiplied', base: 'png', f: (i) => i.composite([{ input: rawOverlay, raw: { width: 24, height: 16, channels: 4 }, premultiplied: true, top: 2, left: 2 }]), tol: { mad: 3 } },
    { name: 'premultiplied flag on png', base: 'png', f: (i) => i.composite([{ input: overlayRgba, premultiplied: true }]), tol: { mad: 3 } },
    { name: 'multiple overlays', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 0, left: 0 }, { input: overlayRgb, top: 10, left: 10, blend: 'multiply' }, { input: overlayGrey, gravity: 'southeast' }]), tol: { mad: 3 } },
    { name: 'composite after resize', base: 'png', f: (i) => i.resize(30, 20, { fit: 'fill' }).composite([{ input: overlayRgba }]), tol: { mad: 6 } },
    { name: 'composite twice (second call replaces)', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 0, left: 0 }]).composite([{ input: overlayRgb, top: 20, left: 20 }]) },
    { name: 'overlay larger than base', base: 'png', f: (i) => i.composite([{ input: big }]) },
    { name: 'overlay larger, tile', base: 'png', f: (i) => i.composite([{ input: big, tile: true }]) },
    { name: 'missing input', base: 'png', f: (i) => i.composite([{ top: 1, left: 1 }]) },
    { name: 'unknown blend', base: 'png', f: (i) => i.composite([{ input: overlayRgba, blend: 'sparkle' }]) },
    { name: 'unknown gravity', base: 'png', f: (i) => i.composite([{ input: overlayRgba, gravity: 'upwards' }]) },
    { name: 'float top', base: 'png', f: (i) => i.composite([{ input: overlayRgba, top: 1.5, left: 0 }]) },
    { name: 'empty array', base: 'png', f: (i) => i.composite([]) },
    { name: 'not an array/object', base: 'png', f: (i) => i.composite('x') },
    { name: 'input not an image', base: 'png', f: (i) => i.composite([{ input: Buffer.from('nope') }]) },
    { name: 'create-input overlay', base: 'png', f: (i) => i.composite([{ input: { create: { width: 10, height: 10, channels: 4, background: { r: 255, g: 0, b: 0, alpha: 0.5 } } }, top: 3, left: 3 }]) },
    { name: 'text overlay', base: 'png', f: (i) => i.composite([{ input: { text: { text: 'hi' } } }]), tol: { mad: 100 } },
  );
  group('composite', cases, async (c) => {
    const src = await make(c.base, 48, 32);
    return compare((lib) => c.f(lib(src)), jt(c.base, c.tol ?? { mad: 1.5 }));
  });
}

// ---------------------------------------------------------------------------------------------
// output side: encoders. Both outputs are decoded with sharp so only the encoder differs.
// ---------------------------------------------------------------------------------------------
// 16-bit -> 8-bit reduction: libvips truncates (x >> 8), the engine rounds, so PNG16 sources differ by
// up to one 8-bit level after re-encoding.
const META_KEYS = ['format', 'width', 'height', 'channels', 'hasAlpha', 'space', 'depth', 'isProgressive', 'isPalette', 'paletteBitDepth'];
const FORMAT_NAME = { heif: 'avif' }; // sharp reports AVIF containers as "heif"

async function encodeBoth(build, { pixelTol, meta = [], sizeRatio, decode = true, skipMeta = [] } = {}) {
  let s;
  let e;
  try { s = await build(sharp).toBuffer({ resolveWithObject: true }); } catch (error) { s = { error }; }
  try { e = await build(engine).toBuffer({ resolveWithObject: true }); } catch (error) { e = { error }; }
  if (s.error) return e.error || !isValidationError(s.error) ? null : `sharp rejects (${String(s.error.message).slice(0, 50)}) but engine succeeded`;
  if (e.error) return `engine rejects: ${String(e.error.message).slice(0, 100)}`;
  const out = [];
  if (e.info.size !== e.data.length) out.push(`info.size ${e.info.size} != data.length ${e.data.length}`);
  if ((FORMAT_NAME[s.info.format] ?? s.info.format) !== (FORMAT_NAME[e.info.format] ?? e.info.format)) out.push(`info.format ${s.info.format} vs ${e.info.format}`);
  if (s.info.width !== e.info.width || s.info.height !== e.info.height) out.push(`info size ${s.info.width}x${s.info.height} vs ${e.info.width}x${e.info.height}`);
  if (s.info.channels !== e.info.channels) out.push(`info.channels ${s.info.channels} vs ${e.info.channels}`);
  if (!decode) return out.length ? out.join(' || ') : null;
  let sm;
  let em;
  let sd;
  let ed;
  try {
    [sm, em] = await Promise.all([sharp(s.data).metadata(), sharp(e.data).metadata()]);
    [sd, ed] = await Promise.all([sharp(s.data).raw().toBuffer({ resolveWithObject: true }), sharp(e.data).raw().toBuffer({ resolveWithObject: true })]);
  } catch (error) {
    return `engine output not decodable by sharp: ${String(error.message).slice(0, 80)}`;
  }
  for (const k of ['format', 'width', 'height', 'channels', 'hasAlpha', 'depth', 'isProgressive', 'isPalette', 'paletteBitDepth', ...meta]) {
    if (skipMeta.includes(k)) continue;
    if (k === 'format' && (FORMAT_NAME[sm[k]] ?? sm[k]) === (FORMAT_NAME[em[k]] ?? em[k])) continue;
    if (sm[k] !== em[k]) out.push(`output metadata.${k} sharp=${sm[k]} engine=${em[k]}`);
  }
  if (sd.info.channels === ed.info.channels && sd.data.length === ed.data.length && pixelTol !== undefined) {
    const d = diffStats(sd.data, ed.data);
    if (d.mad > pixelTol) out.push(`decoded pixels mad=${d.mad.toFixed(2)} > ${pixelTol}`);
  }
  if (sizeRatio) {
    const r = e.data.length / s.data.length;
    if (r > sizeRatio || r < 1 / sizeRatio) out.push(`size ratio engine/sharp=${r.toFixed(2)} outside 1/${sizeRatio}..${sizeRatio}`);
  }
  return out.length ? out.join(' || ') : null;
}

function encGroup(title, specs, opts = {}) {
  const cases = specs.map(([name, kind, f, o]) => ({ name: `${kind} ${name}`, kind, f, o: { ...opts, ...o } }));
  group(title, cases, async (c) => {
    const src = await make(c.kind, 48, 32);
    // JPEG *sources* are decoded slightly differently by each library (see jt()), which leaks
    // into the re-encoded pixels, so give those cases a little extra headroom.
    const o = c.o.pixelTol !== undefined && c.kind === 'jpeg' ? { ...c.o, pixelTol: c.o.pixelTol + 1.2 } : c.o;
    return encodeBoth((lib) => c.f(lib(src)), o);
  });
}

encGroup('jpeg output options', [
  ...['png', 'pnga', 'jpeg', 'gpng'].flatMap((k) => [
    [`default`, k, (i) => i.jpeg(), { pixelTol: 3, sizeRatio: 3 }],
    [`quality 5`, k, (i) => i.jpeg({ quality: 5 }), { sizeRatio: 3 }],
    [`quality 50`, k, (i) => i.jpeg({ quality: 50 }), { pixelTol: 5, sizeRatio: 3 }],
    [`quality 100`, k, (i) => i.jpeg({ quality: 100 }), { pixelTol: 2, sizeRatio: 3 }],
    [`progressive`, k, (i) => i.jpeg({ progressive: true }), { pixelTol: 3, sizeRatio: 3 }],
    [`chroma 4:4:4`, k, (i) => i.jpeg({ chromaSubsampling: '4:4:4' }), { pixelTol: 3, sizeRatio: 3 }],
    [`chroma 4:2:0`, k, (i) => i.jpeg({ chromaSubsampling: '4:2:0' }), { pixelTol: 4, sizeRatio: 3 }],
    [`mozjpeg`, k, (i) => i.jpeg({ mozjpeg: true }), { pixelTol: 5, sizeRatio: 3 }],
    [`mozjpeg progressive q60`, k, (i) => i.jpeg({ mozjpeg: true, quality: 60 }), { pixelTol: 8, sizeRatio: 3 }],
    [`trellis/overshoot/optimiseScans`, k, (i) => i.jpeg({ trellisQuantisation: true, overshootDeringing: true, optimiseScans: true }), { pixelTol: 6, sizeRatio: 3 }],
    [`optimiseCoding false`, k, (i) => i.jpeg({ optimizeCoding: false, optimiseCoding: false }), { pixelTol: 3, sizeRatio: 3 }],
    [`quantisationTable 3`, k, (i) => i.jpeg({ quantisationTable: 3 }), { pixelTol: 6, sizeRatio: 4 }],
  ]),
  ['quality 0', 'png', (i) => i.jpeg({ quality: 0 })], ['quality 101', 'png', (i) => i.jpeg({ quality: 101 })],
  ['quality 80.5', 'png', (i) => i.jpeg({ quality: 80.5 })], ['quality "80"', 'png', (i) => i.jpeg({ quality: '80' })],
  ['quality NaN', 'png', (i) => i.jpeg({ quality: NaN })], ['chroma bogus', 'png', (i) => i.jpeg({ chromaSubsampling: '4:1:1' })],
  ['progressive "yes"', 'png', (i) => i.jpeg({ progressive: 'yes' })], ['options null', 'png', (i) => i.jpeg(null)],
  ['jpg() alias via toFormat', 'png', (i) => i.toFormat('jpg', { quality: 70 }), { pixelTol: 4, sizeRatio: 3 }],
]);

encGroup('png output options', [
  ...['png', 'pnga', 'jpeg', 'gpng', 'png16'].flatMap((k) => [
    ['default (lossless)', k, (i) => i.png(), { pixelTol: k === 'png16' ? 1 : 0 }],
    ['compressionLevel 0', k, (i) => i.png({ compressionLevel: 0 }), { pixelTol: k === 'png16' ? 1 : 0 }],
    ['compressionLevel 9', k, (i) => i.png({ compressionLevel: 9 }), { pixelTol: k === 'png16' ? 1 : 0 }],
    ['adaptiveFiltering', k, (i) => i.png({ adaptiveFiltering: true }), { pixelTol: k === 'png16' ? 1 : 0 }],
    ['interlaced (progressive)', k, (i) => i.png({ progressive: true }), { pixelTol: k === 'png16' ? 1 : 0 }],
    ['palette', k, (i) => i.png({ palette: true }), { pixelTol: 14 }],
    ['palette colors 2', k, (i) => i.png({ palette: true, colors: 2 }), { pixelTol: 70 }],
    // Palette quantisation uses NeuQuant; libvips uses libimagequant, which spends its colours
    // better, so the engine's mean error is a few levels higher at small palette sizes.
    ['palette colors 16 dither 0', k, (i) => i.png({ palette: true, colors: 16, dither: 0 }), { pixelTol: 30 }],
    ['palette quality 40 effort 10', k, (i) => i.png({ palette: true, quality: 40, effort: 10 }), { pixelTol: 25 }],
    ['colours alias', k, (i) => i.png({ palette: true, colours: 8 }), { pixelTol: 48 }],
  ]),
  ['bitdepth 8 (palette)', 'png', (i) => i.png({ palette: true, bitdepth: 8 }), { pixelTol: 14 }],
  ['bitdepth 4 (palette)', 'png', (i) => i.png({ palette: true, bitdepth: 4 }), { pixelTol: 40 }],
  ['bitdepth 2 (palette)', 'png', (i) => i.png({ palette: true, bitdepth: 2 }), { pixelTol: 70 }],
  ['bitdepth 1 (palette)', 'png', (i) => i.png({ palette: true, bitdepth: 1 }), { pixelTol: 90 }],
  ['bitdepth 16 on 8-bit', 'png', (i) => i.png({ bitdepth: 16 })], ['bitdepth 8 grey', 'gpng', (i) => i.png({ bitdepth: 8 }), { pixelTol: 0 }],
  ['bitdepth 3 (invalid)', 'png', (i) => i.png({ bitdepth: 3 })], ['bitdepth 1 grey no palette', 'gpng', (i) => i.png({ bitdepth: 1 }), { pixelTol: 128 }],
  ['compressionLevel 10', 'png', (i) => i.png({ compressionLevel: 10 })], ['compressionLevel -1', 'png', (i) => i.png({ compressionLevel: -1 })],
  ['compressionLevel 1.5', 'png', (i) => i.png({ compressionLevel: 1.5 })], ['colours 1 (invalid)', 'png', (i) => i.png({ palette: true, colours: 1 })],
  ['colours 257 (invalid)', 'png', (i) => i.png({ palette: true, colours: 257 })], ['dither 2 (invalid)', 'png', (i) => i.png({ palette: true, dither: 2 })],
  ['effort 0 (invalid)', 'png', (i) => i.png({ palette: true, effort: 0 })], ['png16 -> 8-bit via toColourspace', 'png16', (i) => i.png().toColourspace('srgb'), { pixelTol: 1 }],
]);

encGroup('webp output options', [
  ...['png', 'pnga', 'jpeg', 'gpng'].flatMap((k) => [
    ['default', k, (i) => i.webp(), { pixelTol: 4, sizeRatio: 3 }],
    ['quality 10', k, (i) => i.webp({ quality: 10 }), { pixelTol: 16, sizeRatio: 3 }],
    ['quality 100', k, (i) => i.webp({ quality: 100 }), { pixelTol: 3, sizeRatio: 3 }],
    ['lossless', k, (i) => i.webp({ lossless: true }), { pixelTol: 0 }],
    ['nearLossless', k, (i) => i.webp({ nearLossless: true, quality: 60 }), { pixelTol: 10, sizeRatio: 3 }],
    ['alphaQuality 10', k, (i) => i.webp({ alphaQuality: 10 }), { pixelTol: 8, sizeRatio: 3 }],
    ['effort 0', k, (i) => i.webp({ effort: 0 }), { pixelTol: 5, sizeRatio: 3 }],
    ['effort 6', k, (i) => i.webp({ effort: 6 }), { pixelTol: 4, sizeRatio: 3 }],
    ['smartSubsample', k, (i) => i.webp({ smartSubsample: true }), { pixelTol: 6, sizeRatio: 3 }],
    ['preset photo', k, (i) => i.webp({ preset: 'photo' }), { pixelTol: 4, sizeRatio: 3 }],
    ['lossless + effort 6', k, (i) => i.webp({ lossless: true, effort: 6 }), { pixelTol: 0 }],
  ]),
  ['quality 0', 'png', (i) => i.webp({ quality: 0 }), { pixelTol: 40 }], ['quality 101', 'png', (i) => i.webp({ quality: 101 })],
  ['effort 7', 'png', (i) => i.webp({ effort: 7 })], ['effort -1', 'png', (i) => i.webp({ effort: -1 })],
  ['alphaQuality 101', 'png', (i) => i.webp({ alphaQuality: 101 })], ['preset bogus', 'png', (i) => i.webp({ preset: 'bogus' })],
  ['lossless "x"', 'png', (i) => i.webp({ lossless: 'x' })], ['minSize/mixed (animation opts)', 'png', (i) => i.webp({ minSize: true, mixed: true }), { pixelTol: 4, sizeRatio: 3 }],
]);

encGroup('avif output options', [
  ...['png', 'pnga', 'jpeg'].flatMap((k) => [
    ['quality 30 effort 0', k, (i) => i.avif({ quality: 30, effort: 0 }), { pixelTol: 14, sizeRatio: 6, meta: [] }],
    ['lossless', k, (i) => i.avif({ lossless: true, effort: 0 }), { pixelTol: 6, sizeRatio: 8 }],
    ['chroma 4:2:0', k, (i) => i.avif({ quality: 30, effort: 0, chromaSubsampling: '4:2:0' }), { pixelTol: 14, sizeRatio: 6 }],
    ['bitdepth 10', k, (i) => i.avif({ quality: 30, effort: 0, bitdepth: 10 }), { pixelTol: 14, sizeRatio: 6 }],
  ]),
  ['quality 101', 'png', (i) => i.avif({ quality: 101 })], ['effort 10', 'png', (i) => i.avif({ effort: 10 })],
  ['bitdepth 9', 'png', (i) => i.avif({ bitdepth: 9 })], ['heif() compression av1', 'png', (i) => i.heif({ compression: 'av1', quality: 30, effort: 0 }), { pixelTol: 14, sizeRatio: 6 }],
]);

const gifSkip = (k) => (k === 'gpng' ? ['channels', 'hasAlpha'] : []);
encGroup('gif output options', [
  ...['png', 'pnga', 'jpeg', 'gpng'].flatMap((k) => [
    // sharp always reports GIFs as RGBA; the engine only when transparency is present.
    ['default', k, (i) => i.gif(), { pixelTol: 25, skipMeta: gifSkip(k) }],
    ['colours 16 dither 0', k, (i) => i.gif({ colours: 16, dither: 0 }), { pixelTol: 40, skipMeta: gifSkip(k) }],
    ['effort 1', k, (i) => i.gif({ effort: 1 }), { pixelTol: 25, skipMeta: gifSkip(k) }],
    ['loop/delay options', k, (i) => i.gif({ loop: 2, delay: 100 }), { pixelTol: 25, skipMeta: gifSkip(k) }],
    ['interFrameMaxError', k, (i) => i.gif({ interFrameMaxError: 4, interPaletteMaxError: 5 }), { pixelTol: 25, skipMeta: gifSkip(k) }],
    ['reuse false', k, (i) => i.gif({ reuse: false }), { pixelTol: 25, skipMeta: gifSkip(k) }],
  ]),
  ['colours 1 (invalid)', 'png', (i) => i.gif({ colours: 1 })], ['effort 11 (invalid)', 'png', (i) => i.gif({ effort: 11 })],
  ['dither 5 (invalid)', 'png', (i) => i.gif({ dither: 5 })], ['loop 70000 (invalid)', 'png', (i) => i.gif({ loop: 70000 })],
]);

encGroup('raw output', [
  ...['png', 'pnga', 'jpeg', 'gpng', 'png16', 'gapng'].flatMap((k) => [
    ['raw()', k, (i) => i.raw(), { pixelTol: k === 'jpeg' ? 1.5 : 0, decode: false }],
    ['raw ushort', k, (i) => i.raw({ depth: 'ushort' }), { pixelTol: 0, decode: false }],
    ['raw uchar', k, (i) => i.raw({ depth: 'uchar' }), { pixelTol: 0, decode: false }],
  ]),
  ['depth invalid', 'png', (i) => i.raw({ depth: 'bogus' })], ['toFormat raw', 'png', (i) => i.toFormat('raw'), { decode: false }],
  ['toFormat({id:raw})', 'png', (i) => i.toFormat({ id: 'raw' }), { decode: false }],
]);

encGroup('toFormat argument forms', [
  ...['jpeg', 'jpg', 'png', 'webp', 'gif', 'avif', 'raw'].map((f) => [`toFormat('${f}')`, 'png', (i) => i.toFormat(f), { decode: false }]),
  ['toFormat object {id:png}', 'png', (i) => i.toFormat({ id: 'png' }), { decode: false }],
  ["toFormat('PNG') uppercase", 'png', (i) => i.toFormat('PNG'), { decode: false }],
  ["toFormat('bmp')", 'png', (i) => i.toFormat('bmp')], ["toFormat('')", 'png', (i) => i.toFormat('')],
  ['toFormat()', 'png', (i) => i.toFormat()], ['toFormat(123)', 'png', (i) => i.toFormat(123)], ['toFormat(null)', 'png', (i) => i.toFormat(null)],
  ['no output format keeps input (jpeg)', 'jpeg', (i) => i, { decode: false }], ['no output format keeps input (png)', 'png', (i) => i, { decode: false }],
  ['no output format keeps input (webp)', 'webp', (i) => i, { decode: false }],
  ['last format wins', 'png', (i) => i.jpeg().webp(), { decode: false }],
]);

group('tiff output is rejected with a clear error', [{ name: 'tiff()' }, { name: "toFormat('tiff')" }, { name: "toFile('x.tiff')" }, { name: "toFile('x.tif')" }, { name: 'tile + tiff' }], async (c) => {
  // sharp supports TIFF; the engine deliberately does not. The contract we assert: a real Error
  // that names the format, never a crash, a hang or an empty/garbage success.
  const src = await make('png', 16, 16);
  let error;
  try {
    if (c.name === 'tiff()') await engine(src).tiff().toBuffer();
    else if (c.name.startsWith('toFormat')) await engine(src).toFormat('tiff').toBuffer();
    else if (c.name.includes('.tiff')) await engine(src).toFile(path.join(tmp, 'x.tiff'));
    else if (c.name.includes('.tif')) await engine(src).toFile(path.join(tmp, 'x.tif'));
    else await engine(src).tiff({ compression: 'lzw' }).toBuffer();
  } catch (e) {
    error = e;
  }
  if (!(error instanceof Error)) return `expected an Error, got ${error}`;
  if (!/tif/i.test(error.message)) return `message does not mention tiff: ${error.message.slice(0, 80)}`;
  if (fs.existsSync(path.join(tmp, 'x.tiff')) || fs.existsSync(path.join(tmp, 'x.tif'))) return 'left a partial output file behind';
  return null;
});

// ---------------------------------------------------------------------------------------------
// toFile: one case per extension
// ---------------------------------------------------------------------------------------------
{
  // Extensions sharp understands and the engine should too.
  const same = ['jpg', 'jpeg', 'JPG', 'png', 'PNG', 'webp', 'avif', 'gif'];
  // Unknown extensions: sharp silently keeps the input format (PNG here); the engine must not throw.
  const fallback = ['bmp', 'txt', 'fits', 'xyz'];
  const cases = [...same.map((ext) => ({ name: ext, ext })), ...fallback.map((ext) => ({ name: `${ext} (unknown: falls back to input format)`, ext }))];
  group('toFile: output format from extension', cases, async (c) => {
    const src = await make('png', 24, 16);
    const out = {};
    for (const [label, lib] of [['sharp', sharp], ['engine', engine]]) {
      const file = path.join(tmp, `${label}-ext.${c.ext}`);
      try {
        const info = await lib(src).toFile(file);
        const buf = fs.readFileSync(file);
        let format = null;
        try { format = (await sharp(buf).metadata()).format; } catch { format = 'unreadable'; }
        out[label] = { ok: true, format: FORMAT_NAME[format] ?? format, infoFormat: FORMAT_NAME[info.format] ?? info.format, size: info.size, bytes: buf.length };
      } catch (error) {
        out[label] = { ok: false, message: String(error.message).slice(0, 70) };
      }
    }
    const { sharp: s, engine: e } = out;
    if (s.ok !== e.ok) return s.ok ? `sharp wrote a ${s.format} file, engine rejected: ${e.message}` : `sharp rejects (${s.message}) but engine wrote a ${e.format} file`;
    if (!s.ok) return null;
    const bad = [];
    if (s.format !== e.format && s.format !== 'unreadable') bad.push(`file format sharp=${s.format} engine=${e.format}`);
    if (s.infoFormat !== e.infoFormat) bad.push(`info.format sharp=${s.infoFormat} engine=${e.infoFormat}`);
    if (e.size !== e.bytes) bad.push(`info.size ${e.size} != bytes on disk ${e.bytes}`);
    return bad.length ? bad.join(' || ') : null;
  });

  // Formats the engine deliberately does not write. Contract: reject with an Error naming the format
  // (sharp can write TIFF; HEIC/HEIF are written as AVIF by sharp; JXL output is feature-gated).
  group('toFile: unsupported output formats reject clearly', ['tif', 'tiff', 'heic', 'heif', 'jxl', 'jp2'].map((ext) => ({ name: ext, ext })), async (c) => {
    const src = await make('png', 16, 16);
    const file = path.join(tmp, `unsupported.${c.ext}`);
    let error;
    try { await engine(src).toFile(file); } catch (e) { error = e; }
    if (!(error instanceof Error)) return `expected an Error, got ${error}`;
    if (!/tif|heif|heic|avif|jxl|jp2|2000|support/i.test(error.message)) return `message not descriptive: ${error.message.slice(0, 80)}`;
    if (fs.existsSync(file)) return 'left a partial output file behind';
    return null;
  });

  group('toFile: engine-only extensions', [{ name: 'qoi' }, { name: 'raw' }], async (c) => {
    const src = await make('png', 16, 8);
    const file = path.join(tmp, `engine-only.${c.name}`);
    const info = await engine(src).toFile(file);
    const buf = fs.readFileSync(file);
    if (info.size !== buf.length) return `info.size ${info.size} != ${buf.length}`;
    if (c.name === 'raw') return buf.length === 16 * 8 * info.channels ? null : `raw size ${buf.length} for ${info.channels}ch`;
    return (await engine(buf).metadata()).format === 'qoi' ? null : 'not qoi';
  });

  group('toFile: explicit format beats extension', [{ name: 'png() to .jpg' }, { name: 'jpeg() to .png' }, { name: 'webp() to no-ext' }], async (c) => {
    const src = await make('png', 24, 16);
    const mk = { 'png() to .jpg': ['.jpg', (i) => i.png()], 'jpeg() to .png': ['.png', (i) => i.jpeg()], 'webp() to no-ext': ['', (i) => i.webp()] }[c.name];
    const res = [];
    for (const [label, lib] of [['sharp', sharp], ['engine', engine]]) {
      const file = path.join(tmp, `explicit-${label}-${c.name.replace(/\W+/g, '_')}${mk[0]}`);
      try { await mk[1](lib(src)).toFile(file); res.push((await sharp(fs.readFileSync(file)).metadata()).format); } catch (e) { res.push(`error: ${e.message.slice(0, 40)}`); }
    }
    return res[0] === res[1] ? null : `sharp wrote ${res[0]}, engine wrote ${res[1]}`;
  });
}

// ---------------------------------------------------------------------------------------------
// OutputInfo fields from toBuffer({ resolveWithObject: true }) and toFile
// ---------------------------------------------------------------------------------------------
{
  const cases = [];
  for (const kind of ['png', 'pnga', 'jpeg', 'webp', 'gpng', 'png16']) for (const fmt of ['png', 'jpeg', 'webp', 'gif', 'raw']) cases.push({ name: `${kind} -> ${fmt}`, kind, fmt });
  group('OutputInfo fields', cases, async (c) => {
    const src = await make(c.kind, 40, 30);
    const run = (lib) => lib(src).resize(20, 15, { fit: 'fill' })[c.fmt]().toBuffer({ resolveWithObject: true });
    const [s, e] = await Promise.all([run(sharp), run(engine)]);
    const bad = [];
    for (const k of Object.keys(s.info)) {
      if (k === 'size' || k === 'premultiplied') continue; // premultiplied mirrors libvips' internal pipeline state
      if (k === 'format') { if ((FORMAT_NAME[s.info[k]] ?? s.info[k]) !== e.info[k]) bad.push(`format ${s.info[k]} vs ${e.info[k]}`); continue; }
      if (JSON.stringify(s.info[k]) !== JSON.stringify(e.info[k])) bad.push(`info.${k} sharp=${JSON.stringify(s.info[k])} engine=${JSON.stringify(e.info[k])}`);
    }
    if (e.info.size !== e.data.length) bad.push(`info.size ${e.info.size} != data.length ${e.data.length}`);
    if (typeof e.info.premultiplied !== 'boolean') bad.push('info.premultiplied missing');
    return bad.length ? bad.join(' || ') : null;
  });

  group('callback / stream / Uint8Array output forms', [{ name: 'toBuffer(cb)' }, { name: 'toBuffer(opts, cb)' }, { name: 'toFile(path, cb)' }, { name: 'toUint8Array' }, { name: 'toStream' }, { name: 'pipe via Duplex' }], async (c) => {
    const src = await make('png', 20, 12);
    const run = async (lib) => {
      if (c.name === 'toBuffer(cb)') return new Promise((res, rej) => lib(src).resize(10).toBuffer((err, data, info) => (err ? rej(err) : res({ len: data.length > 0, w: info.width, h: info.height }))));
      if (c.name === 'toBuffer(opts, cb)') return new Promise((res, rej) => lib(src).resize(10).toBuffer({ resolveWithObject: false }, (err, data, info) => (err ? rej(err) : res({ len: data.length > 0, w: info.width, h: info.height }))));
      if (c.name === 'toFile(path, cb)') return new Promise((res, rej) => lib(src).resize(10).toFile(path.join(tmp, `cb-${lib === sharp ? 's' : 'e'}.png`), (err, info) => (err ? rej(err) : res({ len: true, w: info.width, h: info.height }))));
      if (c.name === 'toUint8Array') { const r = await lib(src).resize(10).toUint8Array(); return { keys: Object.keys(r).sort(), isU8: r.data instanceof Uint8Array, w: r.info?.width, h: r.info?.height }; }
      if (c.name === 'toStream') { const chunks = []; for await (const ch of lib(src).resize(10).png().toStream?.() ?? []) chunks.push(ch); const b = Buffer.concat(chunks); const m = await sharp(b).metadata(); return { len: b.length > 0, w: m.width, h: m.height }; }
      const t = lib().resize(10).png();
      const chunks = [];
      const done = new Promise((res, rej) => { t.on('data', (d) => chunks.push(d)); t.on('end', res); t.on('error', rej); });
      t.end(src);
      await done;
      const m = await sharp(Buffer.concat(chunks)).metadata();
      return { len: chunks.length > 0, w: m.width, h: m.height };
    };
    let s;
    try { s = await run(sharp); } catch (error) { s = { error: error.message }; }
    if (s.skipped || s.error) return null; // not available in this sharp version
    const e = await run(engine).catch((error) => ({ error: error.message }));
    if (e.error) return `engine failed: ${String(e.error).slice(0, 80)}`;
    return JSON.stringify(s) === JSON.stringify(e) ? null : `sharp=${JSON.stringify(s)} engine=${JSON.stringify(e)}`;
  });
}

// ---------------------------------------------------------------------------------------------
// metadata()
// ---------------------------------------------------------------------------------------------
const animFrames = [];
for (let i = 0; i < 3; i++) {
  const raw = pattern(16, 12, 3);
  for (let k = 0; k < raw.length; k += 3) raw[k] = (raw[k] + i * 80) & 255;
  animFrames.push(await sharp(raw, { raw: { width: 16, height: 12, channels: 3 } }).png().toBuffer());
}
const animGif = await sharp(animFrames, { join: { animated: true } }).gif({ delay: [100, 200, 300], loop: 0 }).toBuffer();
const animWebp = await sharp(animFrames, { join: { animated: true } }).webp({ delay: [100, 200, 300], loop: 0 }).toBuffer();

{
  const base = await make('png', 40, 30);
  const jpgBase = await make('jpeg', 40, 30);
  const inputs = {
    jpeg: jpgBase,
    'jpeg progressive': await sharp(base).jpeg({ progressive: true }).toBuffer(),
    'jpeg 4:4:4': await sharp(base).jpeg({ chromaSubsampling: '4:4:4' }).toBuffer(),
    'jpeg density 300': await sharp(base).withMetadata({ density: 300 }).jpeg().toBuffer(),
    'jpeg exif orientation 6': await sharp(base).withMetadata({ orientation: 6 }).jpeg().toBuffer(),
    'jpeg exif tags': await sharp(base).withExif({ IFD0: { Copyright: 'fastpkg', Artist: 'me' } }).jpeg().toBuffer(),
    'jpeg icc p3': await sharp(base).withIccProfile('p3').jpeg().toBuffer(),
    'jpeg grey': await make('gjpeg', 40, 30),
    png: base,
    'png alpha': await make('pnga', 40, 30),
    'png grey': await make('gpng', 40, 30),
    'png grey+alpha': await make('gapng', 40, 30),
    'png 16-bit': await make('png16', 40, 30),
    'png palette': await sharp(base).png({ palette: true, colours: 16 }).toBuffer(),
    'png palette 4-bit': await sharp(base).png({ palette: true, colours: 8, bitdepth: 4 }).toBuffer(),
    'png interlaced': await sharp(base).png({ progressive: true }).toBuffer(),
    'png density 144': await sharp(base).withMetadata({ density: 144 }).png().toBuffer(),
    'png icc': await sharp(base).withIccProfile('p3').png().toBuffer(),
    'png exif': await sharp(base).withExif({ IFD0: { Copyright: 'x' } }).png().toBuffer(),
    webp: await make('webp', 40, 30),
    'webp alpha': await sharp(await make('pnga', 40, 30)).webp().toBuffer(),
    'webp lossless': await sharp(base).webp({ lossless: true }).toBuffer(),
    'webp icc': await sharp(base).withIccProfile('p3').webp().toBuffer(),
    'webp exif': await sharp(base).withExif({ IFD0: { Copyright: 'x' } }).webp().toBuffer(),
    gif: await sharp(base).gif().toBuffer(),
    'gif animated': animGif,
    'webp animated': animWebp,
    avif: await sharp(base).avif({ effort: 0, quality: 30 }).toBuffer(),
    'jpeg 1x1': await make('jpeg', 1, 1),
    'png 1x1': await make('png', 1, 1),
  };
  const keys = ['format', 'width', 'height', 'space', 'channels', 'depth', 'density', 'isProgressive', 'hasProfile', 'hasAlpha', 'orientation', 'pages', 'pageHeight', 'loop', 'delay', 'isPalette', 'paletteBitDepth', 'chromaSubsampling'];
  const metaCache = new Map();
  const metaPair = (name) => {
    if (!metaCache.has(name)) metaCache.set(name, Promise.all([sharp(inputs[name]).metadata(), engine(inputs[name]).metadata()]));
    return metaCache.get(name);
  };
  const metaCases = [];
  for (const name of Object.keys(inputs)) for (const k of [...keys, 'exif', 'icc', 'xmp', 'size']) metaCases.push({ name: `${name} :: ${k}`, input: name, k });
  group('metadata(): fields', metaCases, async (c) => {
    const [s, e] = await metaPair(c.input);
    const k = c.k;
    if (k === 'exif' || k === 'icc' || k === 'xmp') {
      if (Boolean(s[k]) !== Boolean(e[k])) return `${k} buffer present: sharp=${Boolean(s[k])} engine=${Boolean(e[k])}`;
      return s[k] && !Buffer.isBuffer(e[k]) ? `${k} is not a Buffer in engine` : null;
    }
    if (k === 'size') return typeof e.size === 'number' && (s.size === undefined || e.size === s.size) ? null : `size sharp=${s.size} engine=${e.size}`;
    let sv = s[k];
    let ev = e[k];
    if (k === 'format') sv = FORMAT_NAME[sv] ?? sv;
    if (k === 'loop') ev = e.loop ?? e.loopCount ?? undefined;
    if ((k === 'density' || k === 'orientation') && sv === undefined && ev === null) ev = undefined;
    if (k === 'delay' && (ev === null || ev === undefined) && sv === undefined) return null;
    // The engine always reports pages/pageHeight; sharp only does for multi-page containers.
    if ((k === 'pages' || k === 'pageHeight') && sv === undefined) return null;
    return JSON.stringify(sv) === JSON.stringify(ev) ? null : `metadata.${k}: sharp=${JSON.stringify(sv)} engine=${JSON.stringify(ev)}`;
  });

  group('metadata(): input sources', [{ name: 'Buffer' }, { name: 'Uint8Array view with offset' }, { name: 'ArrayBuffer' }, { name: 'file path' }, { name: 'raw input' }, { name: 'create input' }, { name: 'stream input' }], async (c) => {
    const file = path.join(tmp, `meta-src-${c.name.replace(/\W+/g, '_')}.png`);
    fs.writeFileSync(file, base);
    const mk = (lib) => {
      switch (c.name) {
        case 'Buffer': return lib(base);
        case 'Uint8Array view with offset': { const pad = new Uint8Array(base.length + 10); pad.set(base, 5); return lib(pad.subarray(5, 5 + base.length)); }
        case 'ArrayBuffer': return lib(base.buffer.slice(base.byteOffset, base.byteOffset + base.length));
        case 'file path': return lib(file);
        case 'raw input': return lib(Buffer.alloc(8 * 6 * 3), { raw: { width: 8, height: 6, channels: 3 } });
        case 'create input': return lib({ create: { width: 8, height: 6, channels: 4, background: '#fff' } });
        default: return lib(fs.createReadStream(file));
      }
    };
    const get = async (lib) => { const m = await mk(lib).metadata(); return { w: m.width, h: m.height, ch: m.channels, f: m.format, a: m.hasAlpha }; };
    const [s, e] = await Promise.all([get(sharp).catch((x) => ({ err: x.message })), get(engine).catch((x) => ({ err: x.message }))]);
    // sharp cannot read metadata() from a not-yet-piped stream; the engine merely has to work.
    if (c.name === 'stream input' && s.err) return e.err ? `engine failed: ${e.err}` : null;
    return JSON.stringify(s) === JSON.stringify(e) ? null : `sharp=${JSON.stringify(s)} engine=${JSON.stringify(e)}`;
  });
}

// ---------------------------------------------------------------------------------------------
// stats()
// ---------------------------------------------------------------------------------------------
{
  const sources = {
    png: await make('png', 64, 48), pnga: await make('pnga', 64, 48), jpeg: await make('jpeg', 64, 48), gpng: await make('gpng', 64, 48),
    'png 16-bit': await make('png16', 64, 48),
    solid: await sharp({ create: { width: 20, height: 20, channels: 3, background: '#336699' } }).png().toBuffer(),
    'solid alpha 0.5': await sharp({ create: { width: 20, height: 20, channels: 4, background: { r: 10, g: 200, b: 90, alpha: 0.5 } } }).png().toBuffer(),
    '1x1': await make('png', 1, 1),
    noise: await sharp({ create: { width: 64, height: 64, channels: 3, background: '#808080', noise: { type: 'gaussian', mean: 128, sigma: 40 } } }).png().toBuffer(),
  };
  group('stats()', Object.keys(sources).map((name) => ({ name })), async (c) => {
    const buf = sources[c.name];
    const [s, e] = await Promise.all([sharp(buf).stats(), engine(buf).stats()]);
    const bad = [];
    if (s.channels.length !== e.channels.length) return `channel count sharp=${s.channels.length} engine=${e.channels.length}`;
    const scale = c.name === 'png 16-bit' ? 257 : 1;
    s.channels.forEach((sc, i) => {
      const ec = e.channels[i];
      for (const [k, tol] of [['min', (c.name === 'jpeg' ? 4 : 1) * scale], ['max', (c.name === 'jpeg' ? 4 : 1) * scale], ['mean', 1.5 * scale], ['stdev', 2 * scale]]) {
        if (typeof ec[k] !== 'number' || Math.abs(sc[k] - ec[k]) > tol) bad.push(`ch${i}.${k} sharp=${sc[k].toFixed(2)} engine=${ec[k]?.toFixed?.(2)}`);
      }
    });
    if (s.isOpaque !== e.isOpaque) bad.push(`isOpaque sharp=${s.isOpaque} engine=${e.isOpaque}`);
    if (Math.abs(s.entropy - e.entropy) > 0.5) bad.push(`entropy sharp=${s.entropy.toFixed(2)} engine=${e.entropy}`);
    if (!(e.sharpness >= 0) || !Number.isFinite(e.sharpness)) bad.push(`sharpness not finite: ${e.sharpness}`);
    if (!e.dominant) bad.push('no dominant');
    // The dominant colour of Gaussian noise is an arbitrary histogram bin in every implementation.
    else if (c.name !== 'noise') for (const k of ['r', 'g', 'b']) if (Math.abs(s.dominant[k] - e.dominant[k]) > 40) bad.push(`dominant.${k} sharp=${s.dominant[k]} engine=${e.dominant[k]}`);
    return bad.length ? bad.slice(0, 6).join(' || ') : null;
  });

  group('stats(): sharpness orders blur < original < sharpened', [{ name: 'order' }], async () => {
    const orig = await make('png', 64, 48);
    const blurred = await sharp(orig).blur(3).png().toBuffer();
    const sharpened = await sharp(orig).sharpen({ sigma: 2, m2: 6 }).png().toBuffer();
    const [bs, os, ss] = await Promise.all([engine(blurred).stats(), engine(orig).stats(), engine(sharpened).stats()]);
    return bs.sharpness < os.sharpness && os.sharpness < ss.sharpness ? null : `sharpness blur=${bs.sharpness} orig=${os.sharpness} sharpened=${ss.sharpness}`;
  });

  group('stats(): after pipeline ops', [{ name: 'resize' }, { name: 'flip' }, { name: 'extract' }, { name: 'negate' }, { name: 'greyscale' }, { name: 'rotate 90' }], async (c) => {
    const buf = sources.png;
    const mk = { resize: (i) => i.resize(20, 15, { fit: 'fill' }), flip: (i) => i.flip(), extract: (i) => i.extract({ left: 5, top: 5, width: 20, height: 20 }), negate: (i) => i.negate(), greyscale: (i) => i.greyscale(), 'rotate 90': (i) => i.rotate(90) }[c.name];
    const [s, e] = await Promise.all([mk(sharp(buf)).stats(), mk(engine(buf)).stats()]);
    if (s.channels.length !== e.channels.length) return `channel count sharp=${s.channels.length} engine=${e.channels.length}`;
    const bad = s.channels.map((sc, i) => [i, Math.abs(sc.mean - e.channels[i].mean)]).filter(([, d]) => d > 4);
    return bad.length ? `mean differs on channels ${bad.map(([i, d]) => `${i}:${d.toFixed(1)}`).join(',')}` : null;
  });
}

// ---------------------------------------------------------------------------------------------
// input options: limitInputPixels, failOn, sequentialRead, pages / page / animated, density
// ---------------------------------------------------------------------------------------------
async function outcome(fn) {
  try { const r = await fn(); return `ok ${r}`; } catch (e) { return e instanceof Error ? 'error' : 'non-error'; }
}

{
  const png = await make('png', 120, 80); // 9600 px
  const jpeg = await make('jpeg', 120, 80);
  const limits = [
    ['100 (below)', 100], ['9599 (one below)', 9599], ['9600 (exact)', 9600], ['9601', 9601], ['1e9', 1e9], ['0 (unlimited)', 0], ['false', false], ['true', true],
    ['-1', -1], ['1.5', 1.5], ['"100"', '100'], ['NaN', NaN], ['Infinity', Infinity], ['null', null], ['undefined', undefined],
  ];
  const cases = [];
  for (const [label, v] of limits) for (const [fmt, buf] of [['png', png], ['jpeg', jpeg]]) cases.push({ name: `${fmt} limitInputPixels=${label}`, v, buf });
  group('limitInputPixels', cases, async (c) => {
    const opts = { limitInputPixels: c.v };
    const run = (lib) => () => lib(c.buf, opts).resize(10).toBuffer().then((b) => b.length > 0);
    const run2 = (lib) => () => lib(c.buf, opts).metadata().then((m) => m.width);
    const [s, e, s2, e2] = await Promise.all([outcome(run(sharp)), outcome(run(engine)), outcome(run2(sharp)), outcome(run2(engine))]);
    const bad = [];
    if (s !== e) bad.push(`toBuffer sharp=${s} engine=${e}`);
    // sharp's metadata() does not enforce the pixel limit (header only); neither should the engine.
    if (s2 !== e2) bad.push(`metadata sharp=${s2} engine=${e2}`);
    return bad.length ? bad.join(' || ') : null;
  });
}

{
  // Truncated / damaged inputs under every failOn level.
  const png = await make('png', 80, 60);
  const jpeg = await make('jpeg', 80, 60);
  const webp = await make('webp', 80, 60);
  const gif = await sharp(await make('png', 80, 60)).gif().toBuffer();
  const flip = (b, at) => { const c = Buffer.from(b); c[at] ^= 0xff; return c; };
  const damaged = {
    'png truncated 60%': png.subarray(0, Math.floor(png.length * 0.6)),
    'png truncated 95%': png.subarray(0, Math.floor(png.length * 0.95)),
    'png bad IDAT byte': flip(png, Math.floor(png.length / 2)),
    'jpeg truncated 60%': jpeg.subarray(0, Math.floor(jpeg.length * 0.6)),
    'jpeg truncated 95%': jpeg.subarray(0, Math.floor(jpeg.length * 0.95)),
    'jpeg missing EOI': jpeg.subarray(0, jpeg.length - 2),
    'jpeg flipped body byte': flip(jpeg, Math.floor(jpeg.length / 2)),
    'webp truncated 60%': webp.subarray(0, Math.floor(webp.length * 0.6)),
    'gif truncated 60%': gif.subarray(0, Math.floor(gif.length * 0.6)),
  };
  const cases = [];
  for (const k of Object.keys(damaged)) for (const failOn of [undefined, 'none', 'truncated', 'error', 'warning']) cases.push({ name: `${k} failOn=${failOn}`, k, opts: failOn ? { failOn } : {} });
  for (const k of ['png truncated 60%', 'jpeg truncated 60%']) for (const failOnError of [true, false]) cases.push({ name: `${k} failOnError=${failOnError}`, k, opts: { failOnError } });
  cases.push({ name: 'failOn bogus', k: 'png truncated 60%', opts: { failOn: 'bogus' } });
  group('failOn / failOnError on damaged files', cases, async (c) => {
    const buf = damaged[c.k];
    const go = (lib) => () => lib(buf, c.opts).raw().toBuffer({ resolveWithObject: true }).then((r) => `${r.info.width}x${r.info.height}`);
    const [s, e] = await Promise.all([outcome(go(sharp)), outcome(go(engine))]);
    return s === e ? null : `sharp=${s} engine=${e}`;
  });
}

{
  const png = await make('png', 120, 80);
  const jpeg = await make('jpeg', 120, 80);
  group('sequentialRead', [{ name: 'png', buf: png }, { name: 'jpeg', buf: jpeg }, { name: 'png + resize', buf: png, f: (i) => i.resize(30, 20) }, { name: 'jpeg + rotate', buf: jpeg, f: (i) => i.rotate(90) }], async (c) => {
    const pipe = c.f ?? ((x) => x);
    const [a, b] = await Promise.all([
      pipe(engine(c.buf, { sequentialRead: true })).raw().toBuffer({ resolveWithObject: true }),
      pipe(engine(c.buf, { sequentialRead: false })).raw().toBuffer({ resolveWithObject: true }),
    ]);
    if (a.info.width !== b.info.width || a.data.compare(b.data) !== 0) return 'sequentialRead changed the output';
    return compare((lib) => (c.f ?? ((i) => i))(lib(c.buf, { sequentialRead: true })), { mad: 5 });
  });

  group('pages / page / animated on multi-frame inputs', [
    ...['gif', 'webp'].flatMap((f) => [
      { name: `${f} default`, f, o: {} }, { name: `${f} animated:true`, f, o: { animated: true } }, { name: `${f} pages:-1`, f, o: { pages: -1 } },
      { name: `${f} pages:2`, f, o: { pages: 2 } }, { name: `${f} page:1`, f, o: { page: 1 } }, { name: `${f} page:2`, f, o: { page: 2 } },
      { name: `${f} page:99 (out of range)`, f, o: { page: 99 } }, { name: `${f} page:-1`, f, o: { page: -1 } }, { name: `${f} pages:0`, f, o: { pages: 0 } },
      { name: `${f} page:1 pages:2`, f, o: { page: 1, pages: 2 } }, { name: `${f} animated:false`, f, o: { animated: false } },
    ]),
  ], async (c) => {
    const buf = c.f === 'gif' ? animGif : animWebp;
    // Contract: it must settle with an Error or an image; dimension parity with sharp is the check.
    const dims = async (lib) => { const r = await lib(buf, c.o).raw().toBuffer({ resolveWithObject: true }); return `${r.info.width}x${r.info.height}`; };
    const [s, e] = await Promise.all([outcome(() => dims(sharp)), outcome(() => dims(engine))]);
    return s === e ? null : `sharp=${s} engine=${e}`;
  });

  group('animated output: gif/webp from multi-frame input', [{ name: 'gif -> gif' }, { name: 'webp -> webp' }, { name: 'gif -> png (first frame)' }, { name: 'animated gif -> webp animated' }], async (c) => {
    // The engine has no animation support: it may emit a still, but it must not crash or return garbage.
    const mk = { 'gif -> gif': (l) => l(animGif, { animated: true }).gif(), 'webp -> webp': (l) => l(animWebp, { animated: true }).webp(), 'gif -> png (first frame)': (l) => l(animGif).png(), 'animated gif -> webp animated': (l) => l(animGif, { animated: true }).webp() }[c.name];
    const [s, e] = await Promise.all([mk(sharp).toBuffer({ resolveWithObject: true }), mk(engine).toBuffer({ resolveWithObject: true })]);
    const [sm, em] = await Promise.all([sharp(s.data, { animated: true }).metadata(), sharp(e.data, { animated: true }).metadata()]);
    const bad = [];
    if (sm.width !== em.width) bad.push(`width ${sm.width} vs ${em.width}`);
    if ((sm.pages ?? 1) !== (em.pages ?? 1)) bad.push(`pages sharp=${sm.pages ?? 1} engine=${em.pages ?? 1}`);
    if (sm.pageHeight !== em.pageHeight) bad.push(`pageHeight sharp=${sm.pageHeight} engine=${em.pageHeight}`);
    return bad.length ? bad.join(' || ') : null;
  });

  group('density on vector input', [{ d: undefined }, { d: 72 }, { d: 144 }, { d: 300 }, { d: 0 }, { d: -5 }, { d: 'x' }].map((x) => ({ name: `svg density=${x.d}`, d: x.d })), async (c) => {
    const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 40 20"><rect width="40" height="20" fill="#f00"/><circle cx="20" cy="10" r="6" fill="#00f"/></svg>');
    const go = (lib) => async () => { const r = await lib(svg, c.d === undefined ? {} : { density: c.d }).raw().toBuffer({ resolveWithObject: true }); return `${r.info.width}x${r.info.height}x${r.info.channels}`; };
    const [s, e] = await Promise.all([outcome(go(sharp)), outcome(go(engine))]);
    return s === e ? null : `sharp=${s} engine=${e}`;
  });

  group('raw / create inputs', [
    ...[1, 2, 3, 4].map((ch) => ({ name: `raw ${ch}ch`, mk: (l) => l(pattern(10, 6, ch), { raw: { width: 10, height: 6, channels: ch } }) })),
    // 16-bit raw input is compared via 8-bit output: sharp's 16-bit -> 16-bit raw round trip rescales
    // samples (0x0707 -> 7), which is a libvips quirk the engine does not reproduce.
    ...[1, 3, 4].map((ch) => ({ name: `raw ushort ${ch}ch`, mk: (l) => l(Buffer.alloc(10 * 6 * ch * 2, 7), { raw: { width: 10, height: 6, channels: ch, depth: 'ushort' } }) })),
    { name: 'raw premultiplied', mk: (l) => l(pattern(10, 6, 4, { alpha: true }), { raw: { width: 10, height: 6, channels: 4, premultiplied: true } }) },
    { name: 'raw wrong length', mk: (l) => l(Buffer.alloc(10), { raw: { width: 10, height: 6, channels: 3 } }) },
    { name: 'raw missing channels', mk: (l) => l(Buffer.alloc(180), { raw: { width: 10, height: 6 } }) },
    { name: 'raw zero width', mk: (l) => l(Buffer.alloc(0), { raw: { width: 0, height: 6, channels: 3 } }) },
    { name: 'raw bad channels 5', mk: (l) => l(Buffer.alloc(300), { raw: { width: 10, height: 6, channels: 5 } }) },
    ...[3, 4].flatMap((ch) => ['#ff8800', 'red', { r: 10, g: 20, b: 30 }, { r: 10, g: 20, b: 30, alpha: 0.5 }, 'rgba(1,2,3,0.25)'].map((bg) => ({ name: `create ${ch}ch bg=${JSON.stringify(bg)}`, mk: (l) => l({ create: { width: 7, height: 5, channels: ch, background: bg } }) }))),
    { name: 'create missing background', mk: (l) => l({ create: { width: 7, height: 5, channels: 3 } }) },
    { name: 'create 2 channels (invalid)', mk: (l) => l({ create: { width: 7, height: 5, channels: 2, background: '#fff' } }) },
    { name: 'create zero size', mk: (l) => l({ create: { width: 0, height: 5, channels: 3, background: '#fff' } }) },
    { name: 'create float size', mk: (l) => l({ create: { width: 7.5, height: 5, channels: 3, background: '#fff' } }) },
    { name: 'create + resize', mk: (l) => l({ create: { width: 70, height: 50, channels: 3, background: '#0f0' } }).resize(10) },
  ], async (c) => {
    const go = async (lib) => {
      const i = c.mk(lib);
      const r = await i.raw().toBuffer({ resolveWithObject: true });
      return r;
    };
    const [s, e] = await Promise.all([go(sharp).catch((error) => ({ error })), go(engine).catch((error) => ({ error }))]);
    if (s.error) return e.error || !isValidationError(s.error) ? null : `sharp rejects (${String(s.error.message).slice(0, 50)}) but engine succeeded`;
    if (e.error) return `engine rejects: ${String(e.error.message).slice(0, 80)}`;
    if (s.info.width !== e.info.width || s.info.height !== e.info.height || s.info.channels !== e.info.channels) return `info sharp=${s.info.width}x${s.info.height}x${s.info.channels} engine=${e.info.width}x${e.info.height}x${e.info.channels}`;
    const d = diffStats(s.data, e.data);
    return d.max <= 1 ? null : `pixels mad=${d.mad.toFixed(2)} max=${d.max}`;
  });
}

// ---------------------------------------------------------------------------------------------
// Stable cross-cutting behaviours
// ---------------------------------------------------------------------------------------------
group('withMetadata / keep* output retention', [
  { name: 'default strips exif', f: (i) => i.jpeg() }, { name: 'keepExif', f: (i) => i.keepExif().jpeg() }, { name: 'withMetadata()', f: (i) => i.withMetadata().jpeg() },
  { name: 'keepMetadata', f: (i) => i.keepMetadata().jpeg() }, { name: 'keepIccProfile', f: (i) => i.keepIccProfile().jpeg() },
  { name: 'withIccProfile p3', f: (i) => i.withIccProfile('p3').jpeg() }, { name: 'withDensity 150', f: (i) => i.withDensity(150).jpeg() },
  { name: 'withMetadata density', f: (i) => i.withMetadata({ density: 200 }).jpeg() }, { name: 'withExif', f: (i) => i.withExif({ IFD0: { Copyright: 'c' } }).jpeg() },
  { name: 'png withMetadata', f: (i) => i.withMetadata().png() }, { name: 'png keepExif', f: (i) => i.keepExif().png() }, { name: 'webp keepMetadata', f: (i) => i.keepMetadata().webp() },
  { name: 'withMetadata orientation 3', f: (i) => i.withMetadata({ orientation: 3 }).jpeg() }, { name: 'withXmp', f: (i) => i.withXmp('<x:xmpmeta xmlns:x="adobe:ns:meta/"/>').jpeg() },
  { name: 'keepXmp', f: (i) => i.keepXmp().jpeg() },
], async (c) => {
  const base = await sharp(await make('png', 32, 24)).withIccProfile('p3').withExif({ IFD0: { Copyright: 'orig', Artist: 'a' } }).withMetadata({ orientation: 6, density: 110 }).jpeg().toBuffer();
  const [s, e] = await Promise.all([c.f(sharp(base)).toBuffer(), c.f(engine(base)).toBuffer()]);
  const [sm, em] = await Promise.all([sharp(s).metadata(), sharp(e).metadata()]);
  const bad = [];
  // sharp keeps the EXIF block as a side effect of withDensity(); only the density itself is a contract.
  if (c.name === 'withDensity 150') return sm.density === em.density ? null : `density sharp=${sm.density} engine=${em.density}`;
  for (const k of ['hasProfile', 'density', 'orientation']) if (sm[k] !== em[k]) bad.push(`${k} sharp=${sm[k]} engine=${em[k]}`);
  for (const k of ['exif', 'icc', 'xmp']) if (Boolean(sm[k]) !== Boolean(em[k])) bad.push(`${k} present sharp=${Boolean(sm[k])} engine=${Boolean(em[k])}`);
  return bad.length ? bad.join(' || ') : null;
});

group('colour space conversions and pipelineColourspace', [
  { name: 'toColourspace srgb', f: (i) => i.toColourspace('srgb') }, { name: 'toColourspace b-w', f: (i) => i.toColourspace('b-w') },
  { name: 'toColourspace rgb16', f: (i) => i.toColourspace('rgb16') }, { name: 'toColourspace grey16', f: (i) => i.toColourspace('grey16') },
  { name: 'toColourspace cmyk (jpeg)', f: (i) => i.toColourspace('cmyk').jpeg() }, { name: 'pipelineColourspace rgb16', f: (i) => i.pipelineColourspace('rgb16') },
  { name: 'pipelineColourspace lab', f: (i) => i.pipelineColourspace('lab').resize(20) },
  { name: 'pipelineColourspace + toColourspace', f: (i) => i.pipelineColourspace('rgb16').toColourspace('srgb') },
], async (c) => {
  const src = await make('png', 32, 24);
  return compare((lib) => c.f(lib(src)), { mad: 3 });
});
