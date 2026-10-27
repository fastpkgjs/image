// Bursts of parallel jobs: the number of jobs that run at once, and with it the thread count and
// the working memory, stay bounded however many are queued. These run in child processes because
// the figures (threads, peak RSS) belong to a whole process.
import test from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const child = path.join(path.dirname(fileURLToPath(import.meta.url)), '_job-limit-child.mjs');
const cores = os.availableParallelism();
const linux = process.platform === 'linux';

function run(mode, args = [], env = {}) {
  const r = spawnSync(process.execPath, [child, mode, ...args], {
    encoding: 'utf8',
    timeout: 120_000,
    env: { ...process.env, ...env },
  });
  assert.equal(r.status, 0, `child ${mode} failed: ${r.error ?? ''}\n${r.stderr}`);
  return JSON.parse(r.stdout.trim().split('\n').pop());
}

test('200 parallel jobs: bounded threads and memory, every result right', { skip: !linux && 'reads /proc' }, () => {
  const out = run('burst', ['200']);
  assert.equal(out.jobs, 200);
  assert.equal(out.wrong, 0);
  // 0.5.0 gave every blocking-pool thread its own rayon pool: 1300-2300 threads for this burst.
  assert.ok(out.maxThreads < 100 + 4 * cores, `peak ${out.maxThreads} threads on ${cores} cores`);
  assert.ok(out.maxRayon <= 4 * cores + 8, `peak ${out.maxRayon} rayon threads on ${cores} cores`);
  // 650-700 MB before, about 280 MB now (sharp: about 210 MB) on this burst.
  assert.ok(out.peakRssMB < 450, `peak RSS ${out.peakRssMB} MB`);
});

test('failing jobs hand their permit back (every entry point)', () => {
  const out = run('errors');
  assert.equal(out.rejected, out.failing);
  assert.ok(out.after > 0);
  assert.equal(out.toFileWidth, 60);
});

test('concurrency(n) changed in the middle of a burst: same bytes, pools follow the setting', () => {
  const out = run('midburst');
  assert.equal(out.jobs, 120);
  assert.equal(out.different, 0, 'output depends on the pool size');
  assert.deepEqual(out.pools, [5, 2, out.defaultThreads]);
  assert.ok(out.defaultThreads >= 1);
});

test('idle pools release their threads and the next job rebuilds one', { skip: !linux && 'reads /proc' }, () => {
  const out = run('idle', [], { FASTPKG_POOL_IDLE_MS: '300' });
  assert.ok(out.maxRayon > 0);
  assert.equal(out.left, 0, `${out.left} rayon threads still alive after the idle timeout`);
  assert.equal(out.againWidth, 120);
});
