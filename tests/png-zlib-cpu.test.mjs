// PNG zlib end-of-stream check. The decoder stops after the last row, so a stream that was cut
// short is only noticed by looking at the end of the stream; the decoder does that itself where
// the rows end (no second inflate), and only an inconclusive tail falls back to inflating the
// whole IDAT run again. These tests pin the verdicts on the awkward tails and the CPU cost of
// the default failOn level against failOn 'none' (which never checks).
import test from 'node:test';
import assert from 'node:assert/strict';
import zlib from 'node:zlib';
import { spawnSync } from 'node:child_process';
import { sharp, engine, prng } from './_util.mjs';

const chunk = (type, data) => {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, 'latin1');
  data.copy(out, 8);
  out.writeUInt32BE(zlib.crc32(data, zlib.crc32(Buffer.from(type, 'latin1'))), 8 + data.length);
  return out;
};

/** RGB 8-bit PNG with unfiltered rows; `stream` may replace the zlib data, `split` cuts IDAT. */
function rgbPng(width, height, { extraRows = 0, stream, split = 0, level = 6 } = {}) {
  const next = prng(7);
  const rowBytes = width * 3;
  const raw = Buffer.alloc((rowBytes + 1) * (height + extraRows));
  for (let y = 0; y < height + extraRows; y++) {
    const o = y * (rowBytes + 1);
    for (let i = 0; i < rowBytes; i++) raw[o + 1 + i] = ((i >> 1) + y * 3 + Math.floor(next() * 6)) & 255;
  }
  const data = stream?.(raw) ?? zlib.deflateSync(raw, { level });
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  const idat = [];
  if (split) for (let at = 0; at < data.length; at += split) idat.push(chunk('IDAT', data.subarray(at, at + split)));
  else idat.push(chunk('IDAT', data));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), ...idat, chunk('IEND', Buffer.alloc(0))]);
}

const verdict = async (lib, png, failOn) => {
  try {
    const { data, info } = await lib(png, { failOn }).raw().toBuffer({ resolveWithObject: true });
    return { ok: true, data, info };
  } catch (error) {
    return { ok: false, error };
  }
};

async function sameAsSharp(name, png) {
  for (const failOn of ['none', 'truncated', 'error', 'warning']) {
    const [expected, actual] = await Promise.all([verdict(sharp, png, failOn), verdict(engine, png, failOn)]);
    assert.equal(actual.ok, expected.ok, `${name} / ${failOn}: sharp ${expected.ok ? 'accepts' : 'rejects'}`);
    if (expected.ok) {
      assert.equal(actual.info.width, expected.info.width, name);
      assert.equal(actual.info.height, expected.info.height, name);
    }
  }
}

test('png zlib tail: whole streams, IDAT split anywhere (even inside the checksum), long and short rasters', async () => {
  const W = 120;
  const H = 90;
  const whole = rgbPng(W, H);
  const reference = await engine(whole).raw().toBuffer();
  for (const split of [1, 3, 7, 4096]) {
    const png = rgbPng(W, H, { split });
    assert.equal(Buffer.compare(await engine(png).raw().toBuffer(), reference), 0, `split ${split}`);
    await sameAsSharp(`split ${split}`, png);
  }
  await sameAsSharp('whole', whole);
  // More rows than the header declares: the stream outlives the image and ends cleanly.
  await sameAsSharp('long raster', rgbPng(W, H, { extraRows: 40 }));
  await sameAsSharp('long raster, split', rgbPng(W, H, { extraRows: 40, split: 5 }));
  // Stored blocks, and a sync flush before the end of the stream.
  await sameAsSharp('stored', rgbPng(W, H, { level: 0 }));
  await sameAsSharp('sync flush', rgbPng(W, H, { stream: (raw) => zlib.deflateSync(raw, { finishFlush: zlib.constants.Z_SYNC_FLUSH }) }));
});

test('png zlib tail: a stream that is cut short or has a bad checksum is rejected like sharp', async () => {
  const W = 120;
  const H = 90;
  const cut = (n) => (raw) => zlib.deflateSync(raw).subarray(0, -n);
  for (const n of [1, 2, 4, 5, 6, 9]) await sameAsSharp(`cut ${n} bytes`, rgbPng(W, H, { stream: cut(n) }));
  const badSum = (raw) => {
    const data = zlib.deflateSync(raw);
    data[data.length - 1] ^= 0x55;
    return data;
  };
  await sameAsSharp('bad checksum', rgbPng(W, H, { stream: badSum }));
  const padded = (raw) => Buffer.concat([zlib.deflateSync(raw), Buffer.from('trailing bytes after the stream')]);
  await sameAsSharp('bytes after the stream', rgbPng(W, H, { stream: padded }));
  // A stream that is not final at all (a flush, no last block).
  const open = (raw) => zlib.deflateSync(raw, { finishFlush: zlib.constants.Z_SYNC_FLUSH }).subarray(0, -4);
  await sameAsSharp('no final block', rgbPng(W, H, { stream: open }));
});

test('png zlib tail: the default failOn costs about as much CPU as failOn none', { skip: process.platform === 'win32' }, () => {
  const png = rgbPng(4096, 1024, { level: 1 });
  const script = `
    import engine from ${JSON.stringify(new URL('../packages/image/index.js', import.meta.url).href)};
    const chunks = [];
    for await (const part of process.stdin) chunks.push(part);
    const png = Buffer.concat(chunks);
    const run = (failOn) => engine(png, { failOn }).resize(1024).png().toBuffer();
    const cpu = async (failOn) => {
      const before = process.cpuUsage();
      await run(failOn);
      const used = process.cpuUsage(before);
      return (used.user + used.system) / 1000;
    };
    await run('warning');
    await run('none');
    const best = { warning: Infinity, none: Infinity };
    for (let i = 0; i < 8; i++) for (const failOn of ['warning', 'none']) best[failOn] = Math.min(best[failOn], await cpu(failOn));
    console.log(JSON.stringify(best));
  `;
  const run = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    input: png,
    encoding: 'utf8',
    maxBuffer: 1 << 26,
    env: { ...process.env, RAYON_NUM_THREADS: '4' },
  });
  assert.equal(run.status, 0, `${run.signal} ${run.stderr}`);
  const { warning, none } = JSON.parse(run.stdout);
  // A second inflate of the IDAT run used to add about a third on top of failOn none.
  assert.ok(warning <= none * 1.25, `default ${warning.toFixed(0)} ms CPU vs failOn none ${none.toFixed(0)} ms`);
});
