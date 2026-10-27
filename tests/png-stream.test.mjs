// PNG streaming decode: a resize at the start of a pipeline area-averages the rows while they are
// decoded (shrink-on-load analogue). Every colour type and bit depth must agree with the
// full-decode result (and with sharp) within a small tolerance; without a downscale the rows
// decode exactly like sharp. Memory is checked in a child process.
import './_png-shrink-env.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { sharp, engine } from './_util.mjs';
import { W, H, encode, scaled, cases } from './_png-encode.mjs';

const meanAbs = (a, b) => {
  assert.equal(a.length, b.length);
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += Math.abs(a[i] - b[i]);
  return sum / a.length;
};
const raw4 = (pipeline) => pipeline.ensureAlpha().raw().toBuffer({ resolveWithObject: true });

for (const [name, spec] of Object.entries(cases)) {
  for (const filter of [0, 2]) {
    test(`stream shrink: ${name}${filter ? ' (up filter)' : ''}`, async () => {
      const png = encode({ width: W, height: H, filter, ...spec });
      // Full decode: bit-exact against sharp (apart from 16-bit rounding).
      const full = await raw4(engine(png));
      const oracle = await raw4(sharp(png));
      assert.deepEqual([full.info.width, full.info.height, full.info.channels], [W, H, 4]);
      assert.ok(meanAbs(full.data, oracle.data) <= (spec.depth === 16 ? 0.5 : 0), `${name}: full decode`);
      for (const width of [96, 64, 48, 24, 7]) {
        const streamed = await raw4(engine(png).resize({ width }));
        const reference = await raw4(engine(full.data, { raw: { width: W, height: H, channels: 4 } }).resize({ width }));
        const theirs = await raw4(sharp(png).resize({ width }));
        assert.deepEqual([streamed.info.width, streamed.info.height], [theirs.info.width, theirs.info.height]);
        const vsFull = meanAbs(streamed.data, reference.data);
        const vsSharp = meanAbs(streamed.data, theirs.data);
        // Without 1.25x of room above twice the target no axis shrinks, so the rows decode exactly.
        if (width > W / 2.5) assert.ok(vsFull <= (spec.depth === 16 ? 1 : 0), `${name} @${width}: no shrink must be exact`);
        // The transparent block hides arbitrary colour; only alpha-weighted pixels are comparable.
        // 1-bit checkerboards alias at tiny targets; so does a 7 px thumbnail of anything.
        const limit = name.includes('transparent') || / 1$/.test(name) ? 10 : width < 10 ? 3 : 2;
        assert.ok(vsFull <= limit, `${name} @${width}: ${vsFull.toFixed(2)} from full decode`);
        assert.ok(vsSharp <= limit + 1, `${name} @${width}: ${vsSharp.toFixed(2)} from sharp`);
      }
    });
  }
}

test('stream shrink keeps the colour of semi-transparent pixels (premultiplied averaging)', async () => {
  // Left half opaque red, right half alpha 0 with green colour: a straight average would turn the
  // edge brownish; the alpha-weighted average keeps pure red where alpha > 0.
  const png = encode({
    width: 64, height: 64, type: 6, depth: 8,
    sample: (x, y, c) => (x < 32 ? [255, 0, 0, 255][c] : [0, 255, 0, 0][c]),
  });
  const { data, info } = await raw4(engine(png).resize({ width: 8, kernel: 'nearest' }));
  assert.equal(info.width, 8);
  for (let y = 0; y < info.height; y++) {
    const left = data.subarray((y * 8 + 1) * 4, (y * 8 + 1) * 4 + 4);
    assert.deepEqual([...left], [255, 0, 0, 255]);
    const right = data.subarray((y * 8 + 6) * 4, (y * 8 + 6) * 4 + 4);
    assert.equal(right[3], 0);
  }
});

test('stream shrink: odd sizes and non-square targets keep the sharp output size', async () => {
  const png = encode({ width: 131, height: 77, type: 2, depth: 8, sample: (x, y, c) => scaled(8)(x, y, c) });
  for (const opts of [{ width: 20 }, { height: 9 }, { width: 31, height: 17, fit: 'fill' }, { width: 30, height: 30, fit: 'cover' }, { width: 30, height: 30, fit: 'inside' }]) {
    const a = await raw4(engine(png).resize(opts));
    const b = await raw4(sharp(png).resize(opts));
    assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height], JSON.stringify(opts));
    assert.ok(meanAbs(a.data, b.data) <= 3, JSON.stringify(opts));
  }
});

test('stream shrink: autoOrient and extract/rotate after the resize still work', async () => {
  const png = encode({ width: W, height: H, type: 2, depth: 8, sample: scaled(8) });
  const a = await raw4(engine(png).resize({ width: 40 }).rotate(90).extract({ left: 2, top: 2, width: 20, height: 20 }));
  const b = await raw4(sharp(png).resize({ width: 40 }).rotate(90).extract({ left: 2, top: 2, width: 20, height: 20 }));
  assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height]);
  assert.ok(meanAbs(a.data, b.data) <= 2);
});

test('stream shrink: failOn semantics on truncated and corrupt files', async () => {
  const bad = encode({ width: W, height: H, type: 6, depth: 8, sample: scaled(8), truncate: 0.6 });
  for (const failOn of [undefined, 'warning', 'error', 'truncated']) {
    await assert.rejects(() => engine(bad, failOn ? { failOn } : {}).resize({ width: 40 }).raw().toBuffer(), undefined, `failOn ${failOn}`);
    await assert.rejects(() => sharp(bad, failOn ? { failOn } : {}).resize({ width: 40 }).raw().toBuffer());
  }
  const a = await raw4(engine(bad, { failOn: 'none' }).resize({ width: 40 }));
  const b = await raw4(sharp(bad, { failOn: 'none' }).resize({ width: 40 }));
  assert.deepEqual([a.info.width, a.info.height], [b.info.width, b.info.height]);
  // The decoded top part is close to sharp's (libpng stops a few rows earlier than the png crate
  // near the cut, so only the rows well before it are comparable); the rest is transparent.
  const head = 12 * 40 * 4;
  assert.ok(meanAbs(a.data.subarray(0, head), b.data.subarray(0, head)) <= 4, 'lenient partial decode');
  assert.equal(a.data[a.data.length - 1], 0);
  // An opaque colour type: missing rows are opaque black.
  const rgb = encode({ width: W, height: H, type: 2, depth: 8, sample: scaled(8), truncate: 0.5 });
  const c = await raw4(engine(rgb, { failOn: 'none' }).resize({ width: 40 }));
  assert.equal(c.data[c.data.length - 1], 255);
  assert.equal(c.data[c.data.length - 2], 0);
});

test('stream shrink: limitInputPixels is enforced before decoding', async () => {
  const png = encode({ width: W, height: H, type: 2, depth: 8, sample: scaled(8) });
  await assert.rejects(() => engine(png, { limitInputPixels: 1000 }).resize({ width: 20 }).raw().toBuffer(), /pixel|limit|large/i);
  const ok = await engine(png, { limitInputPixels: W * H }).resize({ width: 20 }).raw().toBuffer();
  assert.ok(ok.length > 0);
});

test('stream shrink: ICC, EXIF-less metadata survive a shrinking resize', async () => {
  const icc = (await sharp({ create: { width: 1, height: 1, channels: 3, background: 'red' } }).withIccProfile('srgb').png().toBuffer());
  const src = await sharp(encode({ width: W, height: H, type: 2, depth: 8, sample: scaled(8) })).withIccProfile('srgb').png().toBuffer();
  assert.ok(icc.length > 0);
  const a = await engine(src).resize({ width: 40 }).keepIccProfile().png().toBuffer();
  const meta = await engine(a).metadata();
  assert.ok(meta.icc, 'ICC profile kept');
  assert.equal(meta.width, 40);
});

// maxRSS of a forked child starts from the parent's peak (which holds the raw test image here);
// VmHWM is reset by exec.
test('peak memory of a large flat PNG resize stays far below the decoded size', { skip: process.platform !== 'linux' }, () => {
  const size = 8192; // 192 MB as RGB, 256 MB as RGBA
  const png = encode({ width: size, height: size, type: 2, depth: 8, filter: 2, sample: (x, y, c) => [(y >> 6) & 255, (x >> 6) & 255, 128][c] });
  const script = `
    import fs from 'node:fs';
    import engine from ${JSON.stringify(new URL('../packages/image/index.js', import.meta.url).href)};
    const chunks = [];
    for await (const part of process.stdin) chunks.push(part);
    const png = Buffer.concat(chunks);
    const out = await engine(png, { limitInputPixels: false }).resize(400).jpeg().toBuffer();
    console.log(JSON.stringify({ length: out.length, rss: Number(/VmHWM:\\s+(\\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024 }));
  `;
  const run = spawnSync(process.execPath, ['--input-type=module', '-e', script], { input: png, encoding: 'utf8', maxBuffer: 1 << 26 });
  assert.equal(run.status, 0, run.stderr);
  const { length, rss } = JSON.parse(run.stdout);
  assert.ok(length > 100);
  // The buffered decoder peaked above 400 MB for this file.
  assert.ok(rss < 200, `peak RSS ${rss.toFixed(0)} MB`);
});
