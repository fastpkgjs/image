import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { engine, sharp, pattern } from './_util.mjs';

const pkg = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8'));

test('versions.fastpkg is the package version (the native binary is not stale)', () => {
  assert.equal(engine.versions.fastpkg, pkg.version);
  assert.equal(engine.versions.vips, `fastpkg ${pkg.version}`);
});

test('concurrency(n) resizes the pool even after it has been used', async () => {
  const before = engine.concurrency();
  try {
    const raw = pattern(1024, 768, 3);
    const run = () => engine(raw, { raw: { width: 1024, height: 768, channels: 3 } }).resize(300).jpeg().toBuffer();
    await run();
    assert.equal(await engine._jobPoolThreads(), before);
    for (const threads of [1, 3, 2, 5]) {
      assert.equal(engine.concurrency(threads), threads);
      assert.equal(engine.concurrency(), threads);
      await run();
      assert.equal(await engine._jobPoolThreads(), threads, `pool after concurrency(${threads})`);
    }
    engine.concurrency(0);
    await run();
    assert.equal(await engine._jobPoolThreads(), engine.concurrency());
    assert.ok(engine.concurrency() >= 1);
  } finally {
    engine.concurrency(before);
  }
});

test('output does not depend on the pool size', async () => {
  const before = engine.concurrency();
  try {
    const raw = pattern(900, 700, 3);
    const render = () => engine(raw, { raw: { width: 900, height: 700, channels: 3 } }).resize(333).blur(1.2).sharpen().raw().toBuffer();
    engine.concurrency(1);
    const one = await render();
    engine.concurrency(6);
    const six = await render();
    assert.ok(Buffer.compare(one, six) === 0);
  } finally {
    engine.concurrency(before);
  }
});

test('concurrent jobs still run in parallel with a small pool', async () => {
  const before = engine.concurrency();
  try {
    engine.concurrency(1);
    const raw = pattern(800, 600, 3);
    const run = () => engine(raw, { raw: { width: 800, height: 600, channels: 3 } }).resize(200).jpeg().toBuffer();
    const results = await Promise.all(Array.from({ length: 8 }, run));
    assert.equal(results.length, 8);
    for (const out of results) assert.ok((await sharp(out).metadata()).width === 200);
  } finally {
    engine.concurrency(before);
  }
});
