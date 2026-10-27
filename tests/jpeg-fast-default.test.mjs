// The default JPEG shrink-on-load uses libjpeg's finer n/8 DCT scales (faster between the powers
// of two than sharp's 1/2, 1/4, 1/8). The output SIZE must still be sharp's to the pixel (it comes
// from the image libvips would have decoded), and the pixels stay close to sharp's.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, pattern } from './_util.mjs';

const psnr = (a, b) => {
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += (a[i] - b[i]) ** 2;
  return sum === 0 ? Infinity : 10 * Math.log10((255 * 255 * a.length) / sum);
};

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

test('default shrink-on-load output stays close to sharp (and says so in PSNR terms)', async () => {
  const w = 1600;
  const h = 1200;
  const photo = await sharp(pattern(w, h, 3), { raw: { width: w, height: h, channels: 3 } }).jpeg({ quality: 90 }).toBuffer();
  for (const width of [1330, 1000, 640, 333, 120]) {
    const [s, e] = await Promise.all([
      sharp(photo).resize(width).raw().toBuffer({ resolveWithObject: true }),
      engine(photo).resize(width).raw().toBuffer({ resolveWithObject: true }),
    ]);
    assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height]);
    const db = psnr(s.data, e.data);
    assert.ok(db >= 30, `resize(${width}): ${db.toFixed(1)} dB`);
  }
});
