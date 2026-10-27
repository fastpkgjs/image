// EXIF orientation: WebP files carry it too (libvips writes the EXIF chunk with an `Exif\0\0`
// prefix, which broke the container reader), and a leading rotate()/autoOrient()/flip/flop must
// not disable shrink-on-load for the resize that follows.
import './_png-shrink-env.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { spawnSync } from 'node:child_process';
import { sharp, engine, pattern, diffStats } from './_util.mjs';

const W = 320;
const H = 200;
const raw = pattern(W, H, 3);
const tagged = async (format, orientation, options = {}) =>
  sharp(raw, { raw: { width: W, height: H, channels: 3 } })[format](options).withMetadata({ orientation }).toBuffer();
const sources = { jpeg: { quality: 92 }, png: {}, webp: { lossless: true } };
const pixels = async (lib, input, build) => {
  const { data, info } = await build(lib(input)).raw().toBuffer({ resolveWithObject: true });
  return { data, info };
};

for (const [format, options] of Object.entries(sources)) {
  test(`${format}: metadata orientation and rotate() follow the EXIF tag (1-8)`, async () => {
    for (let orientation = 1; orientation <= 8; orientation++) {
      const input = await tagged(format, orientation, options);
      assert.equal((await engine(input).metadata()).orientation, (await sharp(input).metadata()).orientation, `${format} ${orientation}`);
      const [s, e] = await Promise.all([pixels(sharp, input, (x) => x.rotate()), pixels(engine, input, (x) => x.rotate())]);
      assert.equal(e.info.width, s.info.width, `${format} ${orientation} width`);
      assert.equal(e.info.height, s.info.height, `${format} ${orientation} height`);
      const d = diffStats(e.data, s.data);
      assert.ok(d.mad < (format === 'jpeg' ? 3 : 0.5), `${format} ${orientation}: mad ${d.mad}`);
    }
  });

  test(`${format}: a leading rotate()/flip/flop/quarter turn before resize matches sharp`, async () => {
    const steps = {
      'rotate()': (x) => x.rotate(),
      'rotate(90)': (x) => x.rotate(90),
      'rotate(270)': (x) => x.rotate(270),
      'rotate(180)': (x) => x.rotate(180),
      flip: (x) => x.flip(),
      flop: (x) => x.flop(),
    };
    for (const orientation of [1, 3, 6, 8, 5]) {
      const input = await tagged(format, orientation, options);
      for (const [name, step] of Object.entries(steps)) {
        // 40 px is a 4x+ downscale: PNG box-shrinks while decoding, JPEG scales in the DCT.
        const build = (x) => step(x).resize(40);
        const [s, e] = await Promise.all([pixels(sharp, input, build), pixels(engine, input, build)]);
        const label = `${format} o${orientation} ${name}`;
        assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], label);
        const d = diffStats(e.data, s.data);
        assert.ok(d.mad < 3 && d.max < 80, `${label}: mad ${d.mad} max ${d.max}`);
      }
    }
  });
}

test('rotate().flatten().greyscale() before resize matches sharp on an RGBA PNG (shrink looks through them)', async () => {
  const alphaRaw = pattern(W, H, 4, { alpha: true });
  const base = await sharp(alphaRaw, { raw: { width: W, height: H, channels: 4 } }).png().withMetadata({ orientation: 6 }).toBuffer();
  for (const [name, build] of Object.entries({
    flatten: (x) => x.rotate().flatten({ background: '#336699' }).resize(40),
    'flatten + greyscale': (x) => x.rotate().flatten({ background: '#ff8800' }).greyscale().resize(40),
    greyscale: (x) => x.rotate().greyscale().resize(40),
  })) {
    const [s, e] = await Promise.all([pixels(sharp, base, build), pixels(engine, base, build)]);
    assert.deepEqual([e.info.width, e.info.height, e.info.channels], [s.info.width, s.info.height, s.info.channels], name);
    const d = diffStats(e.data, s.data);
    assert.ok(d.mad < 3 && d.max < 80, `${name}: mad ${d.mad} max ${d.max}`);
  }
});

test('a rotate() before a fit-constrained resize keeps the final size', async () => {
  const input = await tagged('png', 6);
  const build = (x) => x.rotate().resize({ width: 50, height: 50, fit: 'inside' });
  const [s, e] = await Promise.all([pixels(sharp, input, build), pixels(engine, input, build)]);
  assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height]);
});

// Spawned so VmHWM is the child's own peak.
test('rotate().flatten() before resize keeps the low-memory PNG path', { skip: process.platform !== 'linux' }, async () => {
  const size = 8192;
  const row = Buffer.alloc(1 + size * 3);
  const zlib = await import('node:zlib');
  const deflate = zlib.createDeflate({ level: 1 });
  const parts = [];
  deflate.on('data', (part) => parts.push(part));
  for (let y = 0; y < size; y++) {
    row[0] = 0;
    for (let x = 0; x < size; x++) row.writeUIntBE(((((y >> 6) & 255) << 16) | (((x >> 6) & 255) << 8) | 128) >>> 0, 1 + x * 3, 3);
    deflate.write(row);
  }
  await new Promise((resolve) => { deflate.end(resolve); });
  const idat = Buffer.concat(parts);
  const chunk = (type, data) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length, 0);
    head.write(type, 4, 'latin1');
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(zlib.crc32(data, zlib.crc32(head.subarray(4))), 0);
    return Buffer.concat([head, data, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const exif = Buffer.from('4d4d002a0000000800010112000300000001000600000000', 'hex'); // orientation 6
  const png = Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('eXIf', exif), chunk('IDAT', idat), chunk('IEND', Buffer.alloc(0))]);
  const script = `
    import fs from 'node:fs';
    import engine from ${JSON.stringify(new URL('../packages/image/index.js', import.meta.url).href)};
    const chunks = [];
    for await (const part of process.stdin) chunks.push(part);
    const out = await engine(Buffer.concat(chunks), { limitInputPixels: false }).rotate().flatten({ background: '#ffffff' }).resize(400).jpeg().toBuffer({ resolveWithObject: true });
    console.log(JSON.stringify({ info: out.info, rss: Number(/VmHWM:\\s+(\\d+)/.exec(fs.readFileSync('/proc/self/status', 'utf8'))[1]) / 1024 }));
  `;
  const run = spawnSync(process.execPath, ['--input-type=module', '-e', script], { input: png, encoding: 'utf8', maxBuffer: 1 << 26 });
  assert.equal(run.status, 0, run.stderr);
  const { info, rss } = JSON.parse(run.stdout);
  assert.deepEqual([info.width, info.height], [400, 400]);
  assert.ok(rss < 200, `peak RSS ${rss.toFixed(0)} MB`);
});
