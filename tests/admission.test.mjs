// Memory-aware job admission: every job claims its estimated peak working set from a budget
// before it runs, so a burst of large images queues instead of holding all of them at once.
// These run in child processes because the figures (budget, peak RSS) belong to a whole process.
import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const child = path.join(path.dirname(fileURLToPath(import.meta.url)), '_admission-child.mjs');
const linux = process.platform === 'linux';

function run(mode, args = [], env = {}, { wrap = [] } = {}) {
  const command = [...wrap, process.execPath, child, mode, ...args];
  const r = spawnSync(command[0], command.slice(1), {
    encoding: 'utf8',
    timeout: 180_000,
    env: { ...process.env, FASTPKG_MEMORY_BUDGET_MB: '', ...env },
  });
  assert.equal(r.status, 0, `child ${mode} failed: ${r.error ?? ''}\n${r.stderr}`);
  return JSON.parse(r.stdout.trim().split('\n').pop());
}

test('FASTPKG_MEMORY_BUDGET_MB sets the budget; junk and unset fall back to the derived one', () => {
  const set = run('info', [], { FASTPKG_MEMORY_BUDGET_MB: '700' });
  assert.equal(set.budgetMb, 700);
  assert.equal(set.source, 'FASTPKG_MEMORY_BUDGET_MB');
  assert.equal(run('info', [], { FASTPKG_MEMORY_BUDGET_MB: ' 96 ' }).budgetMb, 96);
  for (const off of ['0', 'off', 'unlimited']) {
    assert.equal(run('info', [], { FASTPKG_MEMORY_BUDGET_MB: off }).budgetMb, null, off);
  }
  for (const junk of ['lots', '-5', '1.5', '12GB']) {
    const derived = run('info', [], { FASTPKG_MEMORY_BUDGET_MB: junk });
    assert.notEqual(derived.source, 'FASTPKG_MEMORY_BUDGET_MB', junk);
    assert.ok(derived.budgetMb >= 512, `${junk}: ${derived.budgetMb}`);
  }
  const unset = run('info');
  assert.ok(unset.budgetMb >= 512);
  assert.ok(unset.maxJobs >= 4);
});

test('the budget follows the cgroup limit of the process (half of it, at least 512 MB)', { skip: !linux && 'reads cgroups' }, (t) => {
  const probe = spawnSync('systemd-run', ['--user', '--scope', '--quiet', '-p', 'MemoryMax=3G', 'true'], { encoding: 'utf8', timeout: 20_000 });
  if (probe.status !== 0) return t.skip('no systemd user scope with a memory limit');
  const wrap = ['systemd-run', '--user', '--scope', '--quiet', '-p', 'MemoryMax=3G', '-p', 'MemorySwapMax=0'];
  const limited = run('info', [], {}, { wrap });
  assert.equal(limited.source, 'cgroup limit');
  assert.equal(limited.budgetMb, 1536);
  // The override still wins inside a container.
  assert.equal(run('info', [], { FASTPKG_MEMORY_BUDGET_MB: '800' }, { wrap }).budgetMb, 800);
});

test('estimates follow the pipeline: shrink-on-load is small, the whole frame is not, small jobs are free', () => {
  const e = run('estimates');
  const mb = (bytes) => bytes / 1048576;
  // 12 MP RGB: 48 MB of RGBA.
  assert.ok(mb(e.png12mp1200) > 50 && mb(e.png12mp1200) < 90, `${mb(e.png12mp1200)}`);
  assert.ok(mb(e.png12mp200) > 50, 'a 12 MP PNG is below the shrink threshold: decoded whole');
  assert.ok(mb(e.png12mpWhole) > 1.5 * mb(e.png12mp1200), 'encoding the whole frame needs more than a thumbnail');
  assert.ok(mb(e.smallThumb) < 8, `${mb(e.smallThumb)}`);
  assert.equal(e.pathInput, e.png12mp1200, 'a file path is estimated like its bytes');
  assert.equal(e.garbage, 'rejected');
});

test('a burst of large jobs queues against the budget; claims and RSS stay near it, results are right', { skip: !linux && 'reads /proc' }, () => {
  const out = run('burst', ['24'], { FASTPKG_MEMORY_BUDGET_MB: '130' });
  assert.equal(out.jobs, 24);
  assert.equal(out.wrong, 0);
  assert.equal(out.budgetMb, 130);
  assert.ok(out.peakInFlightMb <= 130, `claimed ${out.peakInFlightMb} MB of 130`);
  assert.ok(out.waits > 0, 'nothing queued');
  assert.equal(out.claims, 24);
  assert.equal(out.inFlightMb, 0);
  // Two jobs of about 60 MB each fit; the process holds those and little else (4 cores or 12,
  // the unbudgeted burst holds one per core: 240-720 MB).
  assert.ok(out.peakDeltaMB < 130 + 130, `peak RSS grew by ${out.peakDeltaMB} MB`);
});

test('with the memory limit off the same burst is only bounded by the job count', () => {
  const out = run('burst', ['16'], { FASTPKG_MEMORY_BUDGET_MB: 'off' });
  assert.equal(out.wrong, 0);
  assert.equal(out.budgetMb, null);
  assert.equal(out.peakInFlightMb, 0);
  assert.equal(out.waits, 0);
});

test('a job bigger than the whole budget runs alone, and nothing deadlocks', () => {
  const out = run('huge', [], { FASTPKG_MEMORY_BUDGET_MB: '100' });
  assert.ok(out.estimateMb > 100, `estimate ${out.estimateMb} MB is not above the budget`);
  assert.equal(out.budgetMb, 100);
  assert.equal(out.peakInFlightMb, 100, 'the claim is clamped to the whole budget');
  assert.ok(out.waits >= 3, `${out.waits} claims waited`);
  assert.equal(out.wholeWrong, 0);
  assert.equal(out.thumbWidth, 300);
  assert.equal(out.inFlightMb, 0);
});

test('failing jobs give their claim back (every entry point)', () => {
  const out = run('errors', [], { FASTPKG_MEMORY_BUDGET_MB: '130' });
  assert.equal(out.rejected, out.failing);
  assert.ok(out.claimedByFailures > 0, 'the failing jobs were admitted with a claim');
  assert.equal(out.inFlightAfterFailures, 0);
  assert.equal(out.after, 8);
  assert.equal(out.toFileWidth, 600);
  assert.equal(out.inFlightAtEnd, 0);
});

test('small jobs claim nothing', () => {
  const out = run('small', [], { FASTPKG_MEMORY_BUDGET_MB: '512' });
  assert.equal(out.jobs, 200);
  assert.equal(out.claims, 0);
  assert.equal(out.waits, 0);
});
