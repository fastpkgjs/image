// Child process for job-limit.test.mjs: bursts of parallel jobs, measured from inside the
// process (thread count and peak RSS come from /proc, so the numbers are Linux-only).
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { engine, sharp, pattern } from './_util.mjs';

const mode = process.argv[2];
const cores = os.availableParallelism();
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const sha = (buf) => crypto.createHash('sha1').update(buf).digest('hex');

const threadNames = () => {
  try {
    return fs.readdirSync('/proc/self/task').map((id) => fs.readFileSync(`/proc/self/task/${id}/comm`, 'utf8').trim());
  } catch {
    return [];
  }
};
const rayonThreads = () => threadNames().filter((name) => /^fastpkg-\d+$/.test(name)).length;
const peakRssMB = () => {
  try {
    return Number(/VmHWM:\s+(\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024;
  } catch {
    return 0;
  }
};

// Samples the thread count while `run` is in flight.
async function watched(run) {
  let maxThreads = 0;
  let maxRayon = 0;
  const sample = () => {
    const names = threadNames();
    maxThreads = Math.max(maxThreads, names.length);
    maxRayon = Math.max(maxRayon, names.filter((name) => /^fastpkg-\d+$/.test(name)).length);
  };
  const timer = setInterval(sample, 2);
  try {
    const value = await run();
    sample();
    return { value, maxThreads, maxRayon };
  } finally {
    clearInterval(timer);
  }
}

const SOURCE = { width: 1600, height: 1200 };
const source = () =>
  engine(pattern(SOURCE.width, SOURCE.height, 3), { raw: { ...SOURCE, channels: 3 } }).jpeg({ quality: 88 }).toBuffer();

async function burst() {
  const input = await source();
  const jobs = Number(process.argv[3] || 200);
  await engine(input).resize(100).jpeg().toBuffer();
  const { value, maxThreads, maxRayon } = await watched(() =>
    Promise.all(
      Array.from({ length: jobs }, (_, i) =>
        i % 5 === 4
          ? engine(input).stats().then((stats) => ({ stats: stats.channels.length }))
          : engine(input)
              .resize(300 + i)
              .jpeg()
              .toBuffer()
              .then((data) => sharp(data).metadata().then((meta) => ({ width: meta.width, expected: 300 + i }))),
      ),
    ),
  );
  console.log(
    JSON.stringify({
      jobs: value.length,
      wrong: value.filter((r) => (r.stats !== undefined ? r.stats !== 3 : r.width !== r.expected)).length,
      maxThreads,
      maxRayon,
      peakRssMB: Math.round(peakRssMB()),
      cores,
    }),
  );
}

async function errors() {
  const limit = Math.max(4, cores);
  const garbage = Buffer.from('definitely not an image '.repeat(40));
  const good = await source();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-joblimit-'));
  const blocker = path.join(dir, 'blocker');
  fs.writeFileSync(blocker, 'a file, so nothing can be created below it');
  const failing = [];
  for (let i = 0; i < limit * 4; i++) {
    failing.push(engine(garbage).resize(50).jpeg().toBuffer());
    failing.push(engine(garbage).stats());
    failing.push(engine(good).resize(50).toFile(path.join(blocker, 'out.jpg')));
    failing.push(engine(path.join(dir, 'no-such-file.jpg')).resize(50).jpeg().toBuffer());
  }
  const settled = await Promise.allSettled(failing);
  const rejected = settled.filter((r) => r.status === 'rejected').length;
  // Every permit must be back: a burst as large as the limit still completes.
  const after = await Promise.all(Array.from({ length: limit * 2 }, () => engine(good).resize(80).jpeg().toBuffer()));
  const toFile = await engine(good).resize(60).toFile(path.join(dir, 'ok.jpg'));
  fs.rmSync(dir, { recursive: true, force: true });
  console.log(JSON.stringify({ failing: failing.length, rejected, after: after.length, toFileWidth: toFile.width }));
}

async function midburst() {
  const input = await source();
  const render = () => engine(input).resize(777).sharpen().blur(0.8).jpeg({ quality: 80 }).toBuffer();
  engine.concurrency(1);
  const reference = sha(await render());
  const pending = [];
  const pools = [];
  for (const threads of [4, 2, 7, 1, 3]) {
    engine.concurrency(threads);
    for (let i = 0; i < 24; i++) pending.push(render());
    await sleep(15);
  }
  engine.concurrency(5);
  const results = await Promise.all(pending);
  pools.push(await engine._jobPoolThreads());
  engine.concurrency(2);
  pools.push(await engine._jobPoolThreads());
  engine.concurrency(0);
  pools.push(await engine._jobPoolThreads());
  console.log(
    JSON.stringify({
      jobs: results.length,
      different: results.filter((data) => sha(data) !== reference).length,
      pools,
      defaultThreads: engine.concurrency(),
    }),
  );
}

// Idle pools are released: with a 300 ms idle timeout the rayon threads are gone soon after the burst.
async function idle() {
  const input = await source();
  const { maxRayon } = await watched(() =>
    Promise.all(Array.from({ length: 60 }, () => engine(input).resize(200).jpeg().toBuffer())),
  );
  const during = rayonThreads();
  const deadline = Date.now() + 5000;
  let left = rayonThreads();
  while (left > 0 && Date.now() < deadline) {
    await sleep(50);
    left = rayonThreads();
  }
  // And a later job works (and rebuilds a pool) after the release.
  const again = await engine(input).resize(120).jpeg().toBuffer();
  console.log(JSON.stringify({ maxRayon, during, left, againWidth: (await sharp(again).metadata()).width, cores }));
}

if (mode === 'burst') await burst();
else if (mode === 'errors') await errors();
else if (mode === 'midburst') await midburst();
else if (mode === 'idle') await idle();
else throw new Error(`unknown mode ${mode}`);
