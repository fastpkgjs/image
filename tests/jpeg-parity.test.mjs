// JPEG resizes must come out like sharp's, not just close: sharp shrinks a JPEG on load by 1, 2,
// 4 or 8 only, drops the partial last block, sizes the resize from that shrunk image and hands
// libvips the shrink factors themselves (which box-shrinks large residuals and spreads a rounded
// output size evenly over both ends of each axis). Anything finer, such as libjpeg's n/8 scales,
// or a resampler that works from the rounded sizes, costs 10-25 dB of PSNR against sharp on
// photographs and far more on hard edges (0.5.0 measured 42 dB on a rotate().resize()).
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, prng } from './_util.mjs';

const W = 1600;
const H = 1200;
// Plain and cover resizes must stay this close to sharp. They measure 52-76 dB (hard edges at the
// extreme 32x ratio are the lowest); the margin is for platform differences in the SIMD paths.
// A resize after rotate() is a little further out at the extreme ratios (49-54 dB at 24-32x, 58-76
// dB up to 8x) because sharp resizes before it turns the pixels and the engine after.
const MIN_PSNR = 50;
const MIN_PSNR_ROTATED = 48;

// Photograph-like: three octaves of smooth noise plus fine grain.
async function photo() {
  const rand = prng(0x9e3779b9);
  const octave = async (w, h, amp) => {
    const raw = Buffer.allocUnsafe(w * h * 3);
    for (let i = 0; i < raw.length; i++) raw[i] = (rand() * 255) | 0;
    const out = await sharp(raw, { raw: { width: w, height: h, channels: 3 } }).resize(W, H, { kernel: 'cubic' }).raw().toBuffer();
    return { out, amp };
  };
  const layers = [await octave(4, 3, 0.55), await octave(16, 12, 0.3), await octave(80, 60, 0.12)];
  const data = Buffer.allocUnsafe(W * H * 3);
  for (let i = 0; i < data.length; i++) {
    let v = (rand() - 0.5) * 3;
    for (const l of layers) v += (l.out[i] - 127.5) * l.amp;
    data[i] = Math.max(0, Math.min(255, 128 + v * 1.8));
  }
  return data;
}

// Hard edges: random rectangles, an 8 px checkerboard, thin diagonal lines and a long diagonal.
function edges() {
  const rand = prng(7);
  const data = Buffer.alloc(W * H * 3, 235);
  const rect = (x0, y0, x1, y1, c) => {
    for (let y = Math.max(0, y0); y < Math.min(H, y1); y++) {
      for (let x = Math.max(0, x0); x < Math.min(W, x1); x++) data.set(c, (y * W + x) * 3);
    }
  };
  for (let i = 0; i < 100; i++) {
    rect((rand() * W) | 0, (rand() * H) | 0, (rand() * W) | 0, (rand() * H) | 0, [rand() * 255, rand() * 255, rand() * 255]);
  }
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      if (x < W / 4 && y < H / 3 && (((x >> 3) & 1) ^ ((y >> 3) & 1))) data.set([20, 20, 20], (y * W + x) * 3);
      else if (x > W * 0.7 && y > H * 0.6 && (x + y) % 9 < 2) data.set([10, 10, 200], (y * W + x) * 3);
      else if (Math.abs(x * H - y * W) < 3 * H) data.set([0, 0, 0], (y * W + x) * 3);
    }
  }
  return data;
}

function psnr(a, b) {
  let squares = 0;
  for (let i = 0; i < a.length; i++) {
    const d = a[i] - b[i];
    squares += d * d;
  }
  return squares === 0 ? Infinity : 10 * Math.log10((255 * 255) / (squares / a.length));
}

const sources = {};
for (const [name, make] of [['photo', photo], ['edges', async () => edges()]]) {
  const raw = await make();
  for (const subsampling of ['4:2:0', '4:4:4']) {
    const base = sharp(raw, { raw: { width: W, height: H, channels: 3 } }).jpeg({ quality: 90, chromaSubsampling: subsampling });
    sources[`${name} ${subsampling}`] = {
      plain: await base.clone().toBuffer(),
      // EXIF orientation 6: the stored pixels are turned a quarter, rotate() undoes it.
      oriented: await base.clone().withExif({ IFD0: { Orientation: '6' } }).toBuffer(),
    };
  }
}

const run = (lib, input, build) => build(lib(input)).raw().toBuffer({ resolveWithObject: true });

// Target widths for 1600 px: shrink factors 1.3 to 32 (the box shrink libvips adds on top of the
// DCT scale starts at about 18x).
const WIDTHS = [1230, 1000, 800, 667, 533, 400, 320, 267, 200, 133, 80, 50];
const pipelines = [
  ['resize(w)', 'plain', (w) => (x) => x.resize(w)],
  ['resize(w, h) cover', 'plain', (w) => (x) => x.resize(w, Math.round(w * 0.6), { fit: 'cover' })],
  ['resize(w, h) inside fastShrinkOnLoad:false', 'plain', (w) => (x) => x.resize(w, w, { fit: 'inside', fastShrinkOnLoad: false })],
  // The oriented source is 1200x1600 once turned; scale the targets to keep the same ratios.
  ['rotate().resize(w)', 'oriented', (w) => (x) => x.rotate().resize(Math.round((w * H) / W))],
];

for (const [label, source] of Object.entries(sources)) {
  for (const [name, kind, build] of pipelines) {
    const floor = kind === 'oriented' ? MIN_PSNR_ROTATED : MIN_PSNR;
    test(`jpeg ${label}: ${name} matches sharp in size and PSNR >= ${floor} dB`, async () => {
      const worst = { psnr: Infinity, width: 0 };
      for (const width of WIDTHS) {
        const [s, e] = await Promise.all([run(sharp, source[kind], build(width)), run(engine, source[kind], build(width))]);
        assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], `${width}: size`);
        const db = psnr(s.data, e.data);
        if (db < worst.psnr) Object.assign(worst, { psnr: db, width });
      }
      assert.ok(worst.psnr >= floor, `worst PSNR ${worst.psnr.toFixed(1)} dB at width ${worst.width}`);
    });
  }
}

test('jpeg sizes match sharp whatever the shrink, fit, orientation and fastShrinkOnLoad', async () => {
  const sizes = [[1000, 500], [1003, 507], [2003, 1001], [300, 900], [777, 1234]];
  const targets = [[50, 50], [20, undefined], [undefined, 33], [333, 222], [100, 300], [640, undefined]];
  const mismatches = [];
  for (const [w, h] of sizes) {
    const raw = Buffer.alloc(w * h * 3, 128);
    const jpeg = await sharp(raw, { raw: { width: w, height: h, channels: 3 } }).jpeg().withExif({ IFD0: { Orientation: '6' } }).toBuffer();
    for (const fit of ['cover', 'contain', 'fill', 'inside', 'outside']) {
      for (const [tw, th] of targets) {
        for (const fastShrinkOnLoad of [true, false]) {
          for (const turn of [false, true]) {
            const build = (x) => (turn ? x.rotate() : x).resize(tw, th, { fit, fastShrinkOnLoad });
            const [s, e] = await Promise.all([
              build(sharp(jpeg)).toBuffer({ resolveWithObject: true }),
              build(engine(jpeg)).toBuffer({ resolveWithObject: true }),
            ]);
            if (s.info.width !== e.info.width || s.info.height !== e.info.height) {
              mismatches.push(`${w}x${h} ${fit} ${tw}x${th} turn=${turn} fsl=${fastShrinkOnLoad}: sharp ${s.info.width}x${s.info.height}, engine ${e.info.width}x${e.info.height}`);
            }
          }
        }
      }
    }
  }
  assert.deepEqual(mismatches, []);
});

test('jpeg -> png and jpeg -> webp resizes decode like sharp', async () => {
  const input = sources['photo 4:2:0'].plain;
  for (const [format, options] of [['png', {}], ['webp', { lossless: true }]]) {
    for (const width of [1230, 533, 133]) {
      const encode = (lib) => lib(input).resize(width)[format](options).toBuffer();
      const [s, e] = await Promise.all([encode(sharp), encode(engine)]);
      const [sd, ed] = await Promise.all([run(sharp, s, (x) => x), run(sharp, e, (x) => x)]);
      assert.deepEqual([ed.info.width, ed.info.height], [sd.info.width, sd.info.height]);
      assert.ok(psnr(sd.data, ed.data) >= MIN_PSNR, `${format} ${width}: ${psnr(sd.data, ed.data).toFixed(1)} dB`);
    }
  }
});

test('jpeg rotate(90).resize() still shrinks on load and keeps sharp\'s output size', async () => {
  // sharp decodes such a file at full size (the quarter turn comes first); the engine looks
  // through the turn and shrinks, which is why its pixels are further from sharp's than the
  // pipelines above (35-60 dB), but the size is derived from the full-size source as in sharp.
  const input = sources['photo 4:2:0'].plain;
  for (const width of [1000, 400, 133]) {
    const build = (x) => x.rotate(90).resize(width);
    const [s, e] = await Promise.all([run(sharp, input, build), run(engine, input, build)]);
    assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height]);
    assert.ok(psnr(s.data, e.data) >= 35, `rotate(90).resize(${width}): ${psnr(s.data, e.data).toFixed(1)} dB`);
  }
});
