// WebP decode goes through libwebp (exact fancy upsampling, like sharp) with libwebp's built-in
// scaled decode as shrink-on-load when a resize comes first. sharp is the oracle.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, pattern, prng } from './_util.mjs';

const W = 640;
const H = 480;
const rawOpts = (w, h, channels) => ({ raw: { width: w, height: h, channels } });

function photoish(w, h, channels = 3) {
  const rnd = prng(21);
  const base = pattern(w, h, channels);
  for (let i = 0; i < base.length; i++) if (channels !== 4 || i % 4 !== 3) base[i] = Math.max(0, Math.min(255, base[i] + (rnd() - 0.5) * 16));
  return base;
}
function withAlpha(w, h) {
  const px = photoish(w, h, 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) px[(y * w + x) * 4 + 3] = Math.max(0, Math.min(255, Math.round(255 * (1 - Math.hypot(x - w / 2, y - h / 2) / (h / 2)))));
  return px;
}

const sources = {
  'lossy rgb': await sharp(photoish(W, H), rawOpts(W, H, 3)).webp({ quality: 80 }).toBuffer(),
  'lossy rgba': await sharp(withAlpha(W, H), rawOpts(W, H, 4)).webp({ quality: 75, alphaQuality: 90 }).toBuffer(),
  'lossless rgb': await sharp(photoish(W, H), rawOpts(W, H, 3)).webp({ lossless: true }).toBuffer(),
  'lossless rgba': await sharp(withAlpha(W, H), rawOpts(W, H, 4)).webp({ lossless: true }).toBuffer(),
};

const meanAbs = (a, b) => {
  assert.equal(a.length, b.length);
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += Math.abs(a[i] - b[i]);
  return sum / a.length;
};

test('full decode is identical to sharp (lossy, lossless, with and without alpha)', async () => {
  for (const [name, buf] of Object.entries(sources)) {
    const ours = await engine(buf).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
    const theirs = await sharp(buf).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
    assert.deepEqual([ours.info.width, ours.info.height], [theirs.info.width, theirs.info.height], name);
    assert.ok(ours.data.equals(theirs.data), `${name} differs from sharp`);
    const meta = await engine(buf).metadata();
    assert.equal(meta.hasAlpha, name.endsWith('rgba'), name);
  }
});

test('resize from WebP uses scaled decode and matches sharp closely', async () => {
  const big = await sharp(photoish(1600, 1200), rawOpts(1600, 1200, 3)).webp({ quality: 85 }).toBuffer();
  const bigAlpha = await sharp(withAlpha(1600, 1200), rawOpts(1600, 1200, 4)).webp({ lossless: true, effort: 1 }).toBuffer();
  for (const [name, buf] of [['lossy', big], ['lossless alpha', bigAlpha]]) {
    for (const opts of [{ width: 300 }, { width: 700 }, { width: 120, height: 120, fit: 'cover' }, { width: 400, height: 200, fit: 'contain' }, { width: 1000 }]) {
      for (const fastShrinkOnLoad of [true, false]) {
        const o = { ...opts, fastShrinkOnLoad };
        const ours = await engine(buf).resize(o).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
        const theirs = await sharp(buf).resize(o).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
        const label = `${name} ${JSON.stringify(o)}`;
        assert.deepEqual([ours.info.width, ours.info.height], [theirs.info.width, theirs.info.height], label);
        // Compare premultiplied: the colour under (near-)transparent pixels is arbitrary.
        const pm = (d) => { const out = Buffer.alloc(d.length); for (let i = 0; i < d.length; i += 4) { const a = d[i + 3] / 255; out[i] = d[i] * a; out[i + 1] = d[i + 1] * a; out[i + 2] = d[i + 2] * a; out[i + 3] = d[i + 3]; } return out; };
        const err = meanAbs(pm(ours.data), pm(theirs.data));
        assert.ok(err <= 1.5, `${label}: mean abs error ${err.toFixed(3)} vs sharp`);
      }
    }
  }
});

test('shrink-on-load keeps the final size computed from the full resolution', async () => {
  const buf = await sharp(photoish(1000, 777), rawOpts(1000, 777, 3)).webp({ quality: 80 }).toBuffer();
  for (const resize of [{ width: 101 }, { height: 33 }, { width: 250, height: 250, fit: 'inside' }, { width: 77, height: 300, fit: 'cover' }, { width: 5000, withoutEnlargement: true }]) {
    const a = await engine(buf).resize(resize).toBuffer({ resolveWithObject: true });
    const b = await sharp(buf).resize(resize).toBuffer({ resolveWithObject: true });
    assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height], JSON.stringify(resize));
  }
  // rotate / orientation before a resize must not break the target
  const rot = await engine(buf).rotate(90).resize(100).toBuffer({ resolveWithObject: true });
  const rotS = await sharp(buf).rotate(90).resize(100).toBuffer({ resolveWithObject: true });
  assert.deepEqual([rot.info.width, rot.info.height], [rotS.info.width, rotS.info.height]);
});

test('truncated and corrupt WebP files fail like sharp', async () => {
  const buf = sources['lossy rgb'];
  const cut = buf.subarray(0, buf.length >> 1);
  await assert.rejects(engine(cut).raw().toBuffer());
  await assert.rejects(sharp(cut).raw().toBuffer());
  const bad = Buffer.from(buf);
  bad.fill(0xff, 40, 200);
  const results = await Promise.allSettled([engine(bad).raw().toBuffer(), sharp(bad).raw().toBuffer()]);
  assert.equal(results[0].status === 'fulfilled', results[1].status === 'fulfilled');
});

test('animated WebP still decodes (first frame) through the fallback decoder', async () => {
  const w = 40;
  const h = 30;
  const frames = Buffer.alloc(w * h * 3 * 3);
  for (let f = 0; f < 3; f++) for (let i = 0; i < w * h; i++) frames.set([f * 100, i % 256, 60], (f * w * h + i) * 3);
  const animated = await sharp(frames, { raw: { width: w, height: h * 3, channels: 3, pageHeight: h } }).webp({ loop: 0, delay: [50, 50, 50] }).toBuffer();
  assert.ok(animated.includes('ANMF'));
  const ours = await engine(animated).raw().toBuffer({ resolveWithObject: true });
  assert.deepEqual([ours.info.width, ours.info.height], [w, h]);
  const resized = await engine(animated).resize(10).png().toBuffer();
  assert.equal((await sharp(resized).metadata()).width, 10);
});
