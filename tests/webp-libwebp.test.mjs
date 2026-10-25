import { fileURLToPath } from 'node:url';
// Lossy / near-lossless WebP is encoded by libwebp (libwebp-sys). sharp is the oracle.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, diffStats } from './_util.mjs';

const photoPath = fileURLToPath(new URL('../fixtures/photo.jpg', import.meta.url));

async function rgbaSource() {
  const w = 240;
  const h = 160;
  const data = Buffer.alloc(w * h * 4);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      data[i] = (x * 255 / w) | 0;
      data[i + 1] = (y * 255 / h) | 0;
      data[i + 2] = ((x * 7 + y * 3) & 255);
      data[i + 3] = 40 + ((x * 215 / w) | 0); // horizontal alpha ramp
    }
  }
  return sharp(data, { raw: { width: w, height: h, channels: 4 } }).png().toBuffer();
}

const decode = (buf) => sharp(buf).ensureAlpha().raw().toBuffer({ resolveWithObject: true });

test('lossy webp decodes in sharp close to the source (photo.jpg, q80)', async () => {
  const src = await sharp(photoPath).resize(600).toBuffer();
  const out = await engine(src).webp({ quality: 80 }).toBuffer();
  assert.equal(out.subarray(12, 16).toString(), 'VP8 ');
  const [a, b] = await Promise.all([decode(out), decode(src)]);
  assert.equal(a.info.width, b.info.width);
  const d = diffStats(a.data, b.data);
  assert.ok(d.mad < 6, `mean abs error ${d.mad}`);
});

test('lossy webp sizes track sharp (same libwebp encoder) at q50/80/95', async () => {
  const src = await sharp(photoPath).resize(800).toBuffer();
  for (const quality of [50, 80, 95]) {
    const [s, e] = await Promise.all([sharp(src).webp({ quality }).toBuffer(), engine(src).webp({ quality }).toBuffer()]);
    // Both use libwebp method 4; only the libwebp version / libvips preprocessing differ.
    const ratio = e.length / s.length;
    assert.ok(ratio > 0.9 && ratio < 1.1, `q${quality}: engine ${e.length} vs sharp ${s.length}`);
  }
});

test('lossy webp with alpha keeps the alpha channel (sharp decodes it)', async () => {
  const src = await rgbaSource();
  const out = await engine(src).webp({ quality: 80 }).toBuffer();
  assert.equal((await sharp(out).metadata()).hasAlpha, true);
  const [a, b] = await Promise.all([decode(out), decode(src)]);
  const alphaOf = (buf) => Uint8Array.from({ length: buf.length / 4 }, (_, i) => buf[i * 4 + 3]);
  const d = diffStats(alphaOf(a.data), alphaOf(b.data));
  assert.ok(d.mad < 1, `alpha mad ${d.mad}`);
  // sharp drops RGB under fully transparent pixels but visible pixels must match closely.
  assert.ok(diffStats(a.data, b.data).mad < 6);
});

test('alphaQuality is honoured and matches sharp', async () => {
  const src = await rgbaSource();
  const [hi, lo] = await Promise.all([engine(src).webp({ alphaQuality: 100 }).toBuffer(), engine(src).webp({ alphaQuality: 10 }).toBuffer()]);
  assert.ok(lo.length < hi.length, `alphaQuality 10 (${lo.length}) is smaller than 100 (${hi.length})`);
  const s = await sharp(src).webp({ alphaQuality: 10 }).toBuffer();
  assert.ok(Math.abs(lo.length / s.length - 1) < 0.1, `engine ${lo.length} vs sharp ${s.length}`);
});

test('effort, smartSubsample and preset reach libwebp', async () => {
  const src = await sharp(photoPath).resize(500).toBuffer();
  const base = await engine(src).webp().toBuffer();
  const variants = await Promise.all([
    engine(src).webp({ effort: 0 }).toBuffer(),
    engine(src).webp({ smartSubsample: true }).toBuffer(),
    engine(src).webp({ preset: 'drawing' }).toBuffer(),
  ]);
  for (const v of variants) assert.notDeepEqual(v, base);
});

test('nearLossless uses the lossless coder, stays near the source and tracks sharp size', async () => {
  const src = await sharp(photoPath).resize(500).png().toBuffer();
  const e = await engine(src).webp({ nearLossless: true, quality: 60 }).toBuffer();
  const s = await sharp(src).webp({ nearLossless: true, quality: 60 }).toBuffer();
  assert.equal(e.subarray(12, 16).toString(), 'VP8L');
  const [a, b] = await Promise.all([decode(e), decode(src)]);
  const d = diffStats(a.data, b.data);
  assert.ok(d.mad < 1.5 && d.max <= 20, `mad=${d.mad} max=${d.max}`);
  // libwebp 1.3 (bundled) vs sharp's newer libwebp differ somewhat in near-lossless output size.
  assert.ok(e.length / s.length < 1.3 && e.length / s.length > 0.7, `engine ${e.length} vs sharp ${s.length}`);
});

test('exif, icc and xmp survive lossy webp encoding', async () => {
  const src = await sharp(photoPath).resize(200).withExif({ IFD0: { Copyright: 'libwebp-test' } }).withIccProfile('p3').jpeg().toBuffer();
  const out = await engine(src).keepMetadata().webp({ quality: 70 }).toBuffer();
  const m = await sharp(out).metadata();
  assert.ok(m.exif && m.exif.includes(Buffer.from('libwebp-test')), 'exif kept');
  assert.ok(m.icc && m.icc.length > 100, 'icc kept');
  assert.ok(out.subarray(12, 16).toString() === 'VP8X', 'extended container');
});

test('webp rejects dimensions beyond the format limit with a clear error', async () => {
  const wide = await sharp({ create: { width: 16384, height: 1, channels: 3, background: '#808080' } }).png().toBuffer();
  await assert.rejects(() => engine(wide).webp().toBuffer(), /16383|too large/i);
  const ok = await sharp({ create: { width: 16383, height: 1, channels: 3, background: '#808080' } }).png().toBuffer();
  const out = await engine(ok).webp().toBuffer();
  assert.equal((await sharp(out).metadata()).width, 16383);
});
