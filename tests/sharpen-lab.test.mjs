// sharpen({ sigma, ... }) is libvips' LAB-lightness unsharp mask. Its pixel output is pinned here
// (hashes taken from the scalar reference implementation before the blocked/vectorised rewrite),
// so any speed work on it has to stay bit-identical; thread count must not matter either.
import test from 'node:test';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import { engine } from './_util.mjs';

const hash = (buf) => crypto.createHash('sha256').update(buf).digest('hex').slice(0, 16);

const ops = {
  def: (i) => i.sharpen(),
  s1: (i) => i.sharpen({ sigma: 1 }),
  s2: (i) => i.sharpen({ sigma: 2, m1: 0.5, m2: 3, x1: 1, y2: 20, y3: 30 }),
  s05: (i) => i.sharpen({ sigma: 0.6, m1: 2, m2: 0.2 }),
  blur_s1: (i) => i.blur(2).sharpen({ sigma: 1 }),
};

const PINS = {
  'jpg/def': '7d908b07bdd0d4eb',
  'jpg/s1': 'cb15b6492a71e0b3',
  'jpg/s2': 'bff29748317c4f48',
  'jpg/s05': '44ff9cddc37c49b6',
  'jpg/blur_s1': '4f71c8d228458c26',
  'png/def': 'e74fdeae90c1a62b',
  'png/s1': '9782b20f32644e4e',
  'png/s2': '6f796fa373330cb9',
  'png/s05': '879b74f2c1710fe7',
  'png/blur_s1': 'f9285202f4bc0463',
};

const inputs = { jpg: 'fixtures/photo.jpg', png: 'fixtures/photo.png' };

test('LAB sharpen output is bit-identical to the reference implementation', async () => {
  for (const [name, file] of Object.entries(inputs)) {
    for (const [op, fn] of Object.entries(ops)) {
      const raw = await fn(engine(file)).raw().toBuffer();
      assert.equal(hash(raw), PINS[`${name}/${op}`], `${name}/${op}`);
    }
  }
});

test('LAB sharpen does not depend on the thread count', async () => {
  const before = engine.concurrency();
  try {
    const out = [];
    for (const threads of [1, 3, before]) {
      engine.concurrency(threads);
      out.push(hash(await ops.s1(engine('fixtures/photo.jpg')).raw().toBuffer()));
    }
    assert.deepEqual(new Set(out).size, 1);
    assert.equal(out[0], PINS['jpg/s1']);
  } finally {
    engine.concurrency(before);
  }
});

test('LAB sharpen keeps alpha and colour under it', async () => {
  const { data, info } = await engine('fixtures/png16-rgba.png').resize({ width: 40 }).raw().toBuffer({ resolveWithObject: true });
  const out = await engine('fixtures/png16-rgba.png').resize({ width: 40 }).sharpen({ sigma: 1 }).raw().toBuffer();
  assert.equal(out.length, data.length);
  for (let i = 3; i < out.length; i += info.channels) assert.equal(out[i], data[i], `alpha ${i}`);
});
