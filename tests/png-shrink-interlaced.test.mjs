// Interlaced (Adam7) PNGs take the decode-time shrink too: every pass row is accumulated into the
// shrunk image, so the result must equal the shrink of the same picture stored non-interlaced.
// Colour types, bit depths, tRNS/palette, failOn and limitInputPixels behave as for the
// row-by-row path.
import './_png-shrink-env.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine } from './_util.mjs';
import { W, H, encode, cases } from './_png-encode.mjs';

const raw4 = (pipeline) => pipeline.ensureAlpha().raw().toBuffer({ resolveWithObject: true });
const maxAbs = (a, b) => {
  assert.equal(a.length, b.length);
  let worst = 0;
  for (let i = 0; i < a.length; i++) worst = Math.max(worst, Math.abs(a[i] - b[i]));
  return worst;
};

for (const [name, spec] of Object.entries(cases)) {
  test(`interlaced shrink equals the non-interlaced shrink: ${name}`, async () => {
    const flat = encode({ width: W, height: H, ...spec });
    const adam7 = encode({ width: W, height: H, interlace: true, ...spec });
    for (const width of [64, 24, 7]) {
      const a = await raw4(engine(flat).resize({ width }));
      const b = await raw4(engine(adam7).resize({ width }));
      assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height]);
      // The two accumulate in different orders in f32; the shrunk rows may differ by a rounding
      // step, which the resampler after them can turn into one level.
      assert.ok(maxAbs(a.data, b.data) <= 1, `${name} @${width}: ${maxAbs(a.data, b.data)}`);
    }
  });
}

test('interlaced shrink: sizes that do not divide by the Adam7 grid, tiny and thin images', async () => {
  for (const [width, height] of [[131, 77], [67, 61], [200, 9], [9, 200], [1, 300], [300, 1]]) {
    const spec = { type: 6, depth: 8, sample: (x, y, c) => (c === 3 ? 40 + ((x + y) % 200) : (x * 3 + y * 7 + c * 50) & 255) };
    const flat = encode({ width, height, ...spec });
    const adam7 = encode({ width, height, interlace: true, ...spec });
    const target = Math.max(1, Math.floor(width / 4));
    const a = await raw4(engine(flat).resize({ width: target }));
    const b = await raw4(engine(adam7).resize({ width: target }));
    assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height], `${width}x${height}`);
    assert.ok(maxAbs(a.data, b.data) <= 1, `${width}x${height}`);
  }
});

test('interlaced shrink stays close to sharp', async () => {
  const png = encode({ width: W, height: H, interlace: true, ...cases['rgb 8'] });
  const a = await raw4(engine(png).resize({ width: 40 }));
  const b = await raw4(sharp(png).resize({ width: 40 }));
  assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height]);
  let sum = 0;
  for (let i = 0; i < a.data.length; i++) sum += Math.abs(a.data[i] - b.data[i]);
  assert.ok(sum / a.data.length <= 2);
});

test('interlaced shrink: autoOrient and rotate before the resize still look through', async () => {
  const png = encode({ width: W, height: H, interlace: true, ...cases['rgba 8'] });
  const flat = encode({ width: W, height: H, ...cases['rgba 8'] });
  const a = await raw4(engine(png).rotate(90).resize({ width: 30 }));
  const b = await raw4(engine(flat).rotate(90).resize({ width: 30 }));
  assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height]);
  assert.ok(maxAbs(a.data, b.data) <= 1);
});

test('interlaced shrink: failOn semantics on truncated and corrupt files', async () => {
  const bad = encode({ width: W, height: H, interlace: true, type: 6, depth: 8, sample: cases['rgba 8'].sample, truncate: 0.6 });
  for (const failOn of [undefined, 'warning', 'error', 'truncated']) {
    await assert.rejects(() => engine(bad, failOn ? { failOn } : {}).resize({ width: 40 }).raw().toBuffer(), undefined, `failOn ${failOn}`);
    await assert.rejects(() => sharp(bad, failOn ? { failOn } : {}).resize({ width: 40 }).raw().toBuffer());
  }
  // A damaged checksum inside the image data.
  const good = encode({ width: W, height: H, interlace: true, ...cases['rgb 8'] });
  const corrupt = Buffer.from(good);
  corrupt[corrupt.length - 30] ^= 0xff;
  await assert.rejects(() => engine(corrupt).resize({ width: 40 }).raw().toBuffer());
});

test('interlaced shrink: limitInputPixels is enforced before decoding', async () => {
  const png = encode({ width: W, height: H, interlace: true, ...cases['rgb 8'] });
  await assert.rejects(() => engine(png, { limitInputPixels: 1000 }).resize({ width: 20 }).raw().toBuffer(), /pixel|limit|large/i);
  const ok = await engine(png, { limitInputPixels: W * H }).resize({ width: 20 }).raw().toBuffer();
  assert.ok(ok.length > 0);
});

test('interlaced without a downscale decodes exactly like sharp', async () => {
  const png = encode({ width: W, height: H, interlace: true, ...cases['rgba 16'] });
  const a = await raw4(engine(png));
  const b = await raw4(sharp(png));
  assert.ok(maxAbs(a.data, b.data) <= 1);
});
