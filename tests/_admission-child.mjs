// Child process for admission.test.mjs: the memory-aware job admission, measured from inside the
// process (peak RSS comes from /proc, so the numbers are Linux-only). The budget under test
// arrives in FASTPKG_MEMORY_BUDGET_MB.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { engine, sharp, pattern } from './_util.mjs';
import { writePng } from '../bench/png-fixtures.mjs';

const mode = process.argv[2];
const sha = (buf) => crypto.createHash('sha1').update(buf).digest('hex');
const status = (key) => Number(new RegExp(`${key}:\\s+(\\d+)`).exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fastpkg-admission-'));
process.on('exit', () => fs.rmSync(dir, { recursive: true, force: true }));

// A flat-colour ramp: deflates to a few dozen KB and still decodes to a full frame.
async function bigPng(width, height, extra = {}) {
  const file = path.join(dir, `ramp-${width}x${height}.png`);
  await writePng(file, { width, height, depth: 8, color: 'rgb', content: 'ramp', ...extra });
  return fs.readFileSync(file);
}

const smallJpeg = () => engine(pattern(800, 600, 3), { raw: { width: 800, height: 600, channels: 3 } }).jpeg().toBuffer();
const thumb = (input) => engine(input).resize(300).jpeg({ quality: 80 }).toBuffer();
const resetPeak = () => fs.writeFileSync('/proc/self/clear_refs', '5');

async function info() {
  console.log(JSON.stringify(engine._admission()));
}

async function estimates() {
  const png = await bigPng(4000, 3000);
  const jpeg = await smallJpeg();
  const garbage = Buffer.from('not an image at all');
  const out = {
    png12mp1200: await engine(png).resize(1200).jpeg()._jobEstimate(),
    png12mp200: await engine(png).resize(200).jpeg()._jobEstimate(),
    png12mpWhole: await engine(png).jpeg()._jobEstimate(),
    smallThumb: await engine(jpeg).resize(100).jpeg()._jobEstimate(),
    pathInput: await engine(path.join(dir, 'ramp-4000x3000.png')).resize(1200).jpeg()._jobEstimate(),
  };
  out.garbage = await engine(garbage).resize(100).jpeg()._jobEstimate().then(
    () => 'estimated',
    () => 'rejected',
  );
  console.log(JSON.stringify(out));
}

// Many jobs of ~60 MB each against a budget of two of them: they queue, the claims never exceed
// the budget, the process stays near it, and every result is right.
async function burst() {
  const jobs = Number(process.argv[3] || 24);
  const png = await bigPng(4000, 3000);
  const render = () => engine(png).resize(1200).jpeg({ quality: 80 }).toBuffer({ resolveWithObject: true });
  const reference = await render();
  const before = engine._admission();
  await Promise.resolve();
  resetPeak();
  const base = status('VmRSS');
  const results = await Promise.all(Array.from({ length: jobs }, render));
  const peak = status('VmHWM') - base;
  const after = engine._admission();
  console.log(
    JSON.stringify({
      jobs: results.length,
      wrong: results.filter((r) => sha(r.data) !== sha(reference.data) || r.info.width !== 1200).length,
      peakDeltaMB: Math.round(peak),
      budgetMb: after.budgetMb,
      maxJobs: after.maxJobs,
      peakInFlightMb: after.peakInFlightMb,
      inFlightMb: after.inFlightMb,
      waits: after.waits - before.waits,
      claims: after.claims - before.claims,
    }),
  );
}

// Jobs that need more than the whole budget run one at a time, and finish.
async function huge() {
  const png = await bigPng(4000, 3000);
  const small = await smallJpeg();
  const whole = () => engine(png).jpeg({ quality: 70 }).toBuffer({ resolveWithObject: true });
  const estimate = await engine(png).jpeg()._jobEstimate();
  const reference = await whole();
  const before = engine._admission();
  const mixed = await Promise.all([
    whole(),
    whole(),
    thumb(small),
    whole(),
    thumb(small),
    engine(png).resize(1200).jpeg().toBuffer(),
    whole(),
  ]);
  const after = engine._admission();
  console.log(
    JSON.stringify({
      estimateMb: estimate / 1048576,
      budgetMb: after.budgetMb,
      peakInFlightMb: after.peakInFlightMb,
      inFlightMb: after.inFlightMb,
      waits: after.waits - before.waits,
      wholeWrong: [mixed[0], mixed[1], mixed[3], mixed[6]].filter((r) => sha(r.data) !== sha(reference.data)).length,
      thumbWidth: (await sharp(mixed[2]).metadata()).width,
    }),
  );
}

// Jobs that fail after they were admitted (a truncated file with a big header, an unwritable
// destination, a panic-free decode error) hand their claim back.
async function errors() {
  const png = await bigPng(4000, 3000);
  const truncated = png.subarray(0, Math.floor(png.length / 2));
  const blocker = path.join(dir, 'blocker');
  fs.writeFileSync(blocker, 'a file, so nothing can be created below it');
  const before = engine._admission();
  const failing = [];
  for (let i = 0; i < 12; i++) {
    failing.push(engine(truncated).resize(1200).jpeg().toBuffer());
    failing.push(engine(truncated).stats());
    failing.push(engine(png).resize(1200).jpeg().toFile(path.join(blocker, 'out.jpg')));
  }
  const settled = await Promise.allSettled(failing);
  const rejected = settled.filter((r) => r.status === 'rejected').length;
  const mid = engine._admission();
  // Every claim must be back: a burst as large as the budget still completes.
  const after = await Promise.all(Array.from({ length: 8 }, () => engine(png).resize(1200).jpeg().toBuffer()));
  const toFile = await engine(png).resize(600).jpeg().toFile(path.join(dir, 'ok.jpg'));
  const last = engine._admission();
  console.log(
    JSON.stringify({
      failing: failing.length,
      rejected,
      claimedByFailures: mid.claims - before.claims,
      inFlightAfterFailures: mid.inFlightMb,
      after: after.length,
      toFileWidth: toFile.width,
      inFlightAtEnd: last.inFlightMb,
    }),
  );
}

// Small jobs claim nothing and queue behind nothing.
async function small() {
  const input = await smallJpeg();
  const before = engine._admission();
  const results = await Promise.all(Array.from({ length: 200 }, (_, i) => engine(input).resize(100 + i).jpeg().toBuffer()));
  const after = engine._admission();
  console.log(JSON.stringify({ jobs: results.length, claims: after.claims - before.claims, waits: after.waits - before.waits }));
}

if (mode === 'info') await info();
else if (mode === 'estimates') await estimates();
else if (mode === 'burst') await burst();
else if (mode === 'huge') await huge();
else if (mode === 'errors') await errors();
else if (mode === 'small') await small();
else throw new Error(`unknown mode ${mode}`);
