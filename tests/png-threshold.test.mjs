// Below 32 MP a PNG downscale decodes at full size (exact, parallel resample); only very large
// files take the decode-time box shrink, which saves memory but costs a little accuracy.
// No env override here: this file checks the production threshold.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, pattern } from './_util.mjs';

test('a 5 MP PNG downscale equals the resample of the raw pixels (no pre-shrink)', async () => {
  const width = 4096;
  const height = 1240;
  const raw = pattern(width, height, 3);
  const png = await sharp(raw, { raw: { width, height, channels: 3 } }).png().toBuffer();
  for (const target of [300, 1024]) {
    const [fromPng, fromRaw] = await Promise.all([
      engine(png).resize(target).raw().toBuffer(),
      engine(raw, { raw: { width, height, channels: 3 } }).resize(target).raw().toBuffer(),
    ]);
    assert.ok(Buffer.compare(fromPng, fromRaw) === 0, `png and raw resample differ at ${target}`);
  }
});
