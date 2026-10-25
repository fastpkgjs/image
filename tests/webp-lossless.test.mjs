// Lossless WebP: pure-Rust fast coder by default, libwebp for graphic-like content and whenever
// `effort` is given. sharp (devDependency) is the oracle for decoding and for size comparisons.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, prng } from './_util.mjs';

const rawOpts = (w, h, channels) => ({ raw: { width: w, height: h, channels } });
const decode = async (buf) => {
  const { data, info } = await sharp(buf).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  return { data, info };
};

// Graphic: flat fills, a gradient band, hard-edged shapes; optional alpha (incl. a hidden-colour area).
function graphic(w, h, channels) {
  const out = Buffer.alloc(w * h * channels);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      let px = [240, 240, 250, 255];
      if (y > h * 0.3 && y < h * 0.55) px = [(x * 255 / w) | 0, 90, (y * 255 / h) | 0, 255];
      if ((x - w * 0.3) ** 2 + (y - h * 0.75) ** 2 < (h * 0.15) ** 2) px = [220, 30, 30, 255];
      if (x > w * 0.6 && y > h * 0.6) px = [20, 90, 20, 0]; // hidden colour under alpha 0
      if (x > w * 0.5 && x < w * 0.58 && y > h * 0.62) px = [20, 60, 200, 128];
      const o = (y * w + x) * channels;
      for (let c = 0; c < channels; c++) px[c] !== undefined && (out[o + c] = px[c]);
    }
  }
  return out;
}

// Photographic stand-in: smooth field plus heavy grain, nothing for a run/palette coder to exploit.
function noisy(w, h, seed = 7) {
  const rnd = prng(seed);
  const out = Buffer.alloc(w * h * 3);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const o = (y * w + x) * 3;
      const base = 128 + 60 * Math.sin(x / 23) * Math.cos(y / 17);
      for (let c = 0; c < 3; c++) out[o + c] = Math.max(0, Math.min(255, base + (rnd() - 0.5) * 70 + c * 5));
    }
  }
  return out;
}

const screenshotSvg = Buffer.from(
  `<svg xmlns="http://www.w3.org/2000/svg" width="900" height="600"><rect width="900" height="600" fill="#fff"/><rect width="900" height="50" fill="#2b5797"/>` +
    Array.from({ length: 16 }, (_, i) => `<text x="24" y="${90 + i * 32}" font-family="sans-serif" font-size="18" fill="#222">Row ${i}: the quick brown fox jumps over the lazy dog ${i * 37}</text>`).join('') +
    `<rect x="600" y="100" width="240" height="160" fill="#e8f0fe" stroke="#999"/></svg>`,
);

const W = 640;
const H = 480;
const gRgb = graphic(W, H, 3);
const gRgba = graphic(W, H, 4);
const photo = noisy(W, H);

const lossless = (buf, w, h, ch, opts = {}, pipe = (i) => i) => pipe(engine(buf, rawOpts(w, h, ch))).webp({ lossless: true, ...opts }).toBuffer();

test('opaque lossless output is pixel-exact (default and explicit effort)', async () => {
  for (const effort of [undefined, 0, 1, 4, 6]) {
    for (const [name, buf] of [['graphic', gRgb], ['photo', photo]]) {
      const out = await lossless(buf, W, H, 3, effort === undefined ? {} : { effort });
      const { data } = await decode(out);
      const expected = await sharp(buf, rawOpts(W, H, 3)).ensureAlpha().raw().toBuffer();
      assert.ok(data.equals(expected), `${name} effort=${effort} differs`);
    }
  }
});

test('RGBA lossless with exact keeps every channel including colour under alpha 0', async () => {
  for (const effort of [undefined, 1, 5]) {
    const out = await lossless(gRgba, W, H, 4, { exact: true, ...(effort === undefined ? {} : { effort }) });
    const { data } = await decode(out);
    assert.ok(data.equals(gRgba), `exact effort=${effort}`);
  }
});

test('RGBA lossless without exact keeps alpha and all visible colour', async () => {
  for (const effort of [undefined, 0, 1, 4]) {
    const out = await lossless(gRgba, W, H, 4, effort === undefined ? {} : { effort });
    const { data } = await decode(out);
    for (let i = 0; i < gRgba.length; i += 4) {
      assert.equal(data[i + 3], gRgba[i + 3], `alpha @${i / 4} effort=${effort}`);
      if (gRgba[i + 3] !== 0) {
        assert.ok(data[i] === gRgba[i] && data[i + 1] === gRgba[i + 1] && data[i + 2] === gRgba[i + 2], `rgb @${i / 4} effort=${effort}`);
      }
    }
  }
});

test('exact is validated like sharp', () => {
  const img = engine(gRgb, rawOpts(W, H, 3));
  assert.throws(() => img.webp({ lossless: true, exact: 'yes' }), /exact/i);
  assert.throws(() => img.webp({ lossless: true, effort: 7 }), /effort/i);
});

test('graphic-like content is routed to libwebp and lands near sharp size', async () => {
  const shot = await sharp(screenshotSvg).raw().toBuffer({ resolveWithObject: true });
  const { width, height, channels } = shot.info;
  const ours = await lossless(shot.data, width, height, channels);
  const theirs = await sharp(shot.data, rawOpts(width, height, channels)).webp({ lossless: true }).toBuffer();
  const e1 = await lossless(shot.data, width, height, channels, { effort: 1 });
  assert.ok(ours.length < 25000, `screenshot ${ours.length} B (pure-Rust coder alone is ~100 KB)`);
  assert.ok(ours.length <= theirs.length * 1.3, `${ours.length} vs sharp ${theirs.length}`);
  assert.ok(ours.length <= e1.length * 1.05, 'default for graphics is at least as small as effort 1');
  const g = await lossless(gRgba, W, H, 4);
  const gSharp = await sharp(gRgba, rawOpts(W, H, 4)).webp({ lossless: true }).toBuffer();
  assert.ok(g.length < 12000, `graphic ${g.length} B`);
  assert.ok(g.length <= gSharp.length * 1.3, `${g.length} vs sharp ${gSharp.length}`);
});

test('photographic content keeps the fast pure-Rust coder by default', async () => {
  const t0 = performance.now();
  const ours = await lossless(photo, W, H, 3);
  const fastMs = performance.now() - t0;
  const e1 = await lossless(photo, W, H, 3, { effort: 1 });
  // image-webp is clearly larger than libwebp on grain; if the default had been escalated it would match.
  assert.ok(ours.length > e1.length * 1.1, `default ${ours.length} should be image-webp sized vs libwebp ${e1.length}`);
  const t1 = performance.now();
  await lossless(photo, W, H, 3, { effort: 1 });
  assert.ok(fastMs < (performance.now() - t1) * 2 + 50, 'default path is not slower than libwebp method 1');
  const { data } = await decode(ours);
  assert.equal(data.length, W * H * 4);
});

test('explicit effort maps to libwebp method: larger effort never much bigger, effort 0 is the loosest', async () => {
  const sizes = [];
  for (const effort of [0, 1, 2, 4, 6]) sizes.push((await lossless(gRgb, W, H, 3, { effort })).length);
  assert.ok(sizes[0] > sizes[1], `effort 0 (${sizes[0]}) larger than effort 1 (${sizes[1]})`);
  for (let i = 1; i < sizes.length; i++) assert.ok(sizes[i] <= sizes[i - 1] * 1.25, `effort steps ${sizes.join(',')}`);
  const sharpSizes = [];
  for (const effort of [0, 4]) sharpSizes.push((await sharp(gRgb, rawOpts(W, H, 3)).webp({ lossless: true, effort }).toBuffer()).length);
  assert.ok(sizes[3] <= sharpSizes[1] * 1.2 && sizes[0] <= sharpSizes[0] * 1.2, `ours ${sizes} vs sharp ${sharpSizes}`);
  const q = [];
  for (const quality of [10, 100]) q.push((await lossless(gRgb, W, H, 3, { effort: 3, quality })).length);
  assert.ok(q[1] <= q[0] * 1.02, `quality 100 (${q[1]}) not larger than quality 10 (${q[0]})`);
});

test('sharp decodes our lossless output (all paths) and reports lossless-compatible metadata', async () => {
  for (const [buf, ch] of [[gRgb, 3], [gRgba, 4], [photo, 3]]) {
    for (const effort of [undefined, 0, 3]) {
      const out = await lossless(buf, W, H, ch, effort === undefined ? {} : { effort });
      const meta = await sharp(out).metadata();
      assert.equal(meta.format, 'webp');
      assert.equal(meta.width, W);
      assert.equal(meta.height, H);
      assert.equal(meta.hasAlpha, ch === 4);
      assert.equal(out.subarray(0, 4).toString(), 'RIFF');
      assert.ok(out.includes('VP8L') || out.includes('VP8X'));
    }
  }
});

test('metadata (orientation EXIF, ICC) survives on every lossless path', async () => {
  for (const [name, buf, ch] of [['graphic', gRgb, 3], ['photo', photo, 3]]) {
    for (const effort of [undefined, 1]) {
      const out = await engine(buf, rawOpts(W, H, ch))
        .withMetadata({ orientation: 6, icc: 'p3' })
        .webp({ lossless: true, ...(effort === undefined ? {} : { effort }) })
        .toBuffer();
      const meta = await sharp(out).metadata();
      assert.equal(meta.orientation, 6, `${name} effort=${effort} orientation`);
      assert.ok(meta.icc && meta.icc.length > 100, `${name} effort=${effort} icc`);
    }
  }
  const xmp = '<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf/></x:xmpmeta>';
  for (const [buf, effort] of [[gRgb, undefined], [photo, undefined], [photo, 2]]) {
    const out = await engine(buf, rawOpts(W, H, 3)).withXmp(xmp).webp({ lossless: true, ...(effort === undefined ? {} : { effort }) }).toBuffer();
    assert.ok((await sharp(out).metadata()).xmp, `xmp effort=${effort}`);
  }
});

test('gray and 1-colour images, tiny and degenerate sizes round-trip', async () => {
  for (const [w, h] of [[1, 1], [3, 2], [64, 1], [1, 64], [33, 17]]) {
    const buf = gRgba.subarray(0, w * h * 4);
    const out = await lossless(Buffer.from(buf), w, h, 4, { exact: true });
    assert.ok((await decode(out)).data.equals(buf), `${w}x${h}`);
    const out2 = await lossless(Buffer.from(buf), w, h, 4, { exact: true, effort: 0 });
    assert.ok((await decode(out2)).data.equals(buf), `${w}x${h} effort 0`);
  }
  const flat = Buffer.alloc(200 * 200, 77);
  const out = await engine(flat, rawOpts(200, 200, 1)).webp({ lossless: true }).toBuffer();
  const px = await sharp(out).greyscale().raw().toBuffer();
  assert.ok(px.every((v) => v === 77));
});

test('oversized lossless WebP is rejected with an error, not a crash', async () => {
  const w = 16384;
  const buf = Buffer.alloc(w * 2 * 3, 5);
  for (const opts of [{}, { effort: 1 }]) {
    await assert.rejects(engine(buf, rawOpts(w, 2, 3)).webp({ lossless: true, ...opts }).toBuffer(), /16383|webp/i);
  }
});
