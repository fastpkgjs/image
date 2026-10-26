import test from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import engine from '../packages/image/index.js';

test('concurrency(0) resets to the default like sharp', () => {
  const before = engine.concurrency();
  try {
    assert.equal(engine.concurrency(), before);
    engine.concurrency(1);
    assert.equal(engine.concurrency(), 1);
    engine.concurrency(0);
    assert.equal(engine.concurrency(), os.availableParallelism());
  } finally {
    engine.concurrency(before);
  }
});
