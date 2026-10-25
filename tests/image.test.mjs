import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { Readable } from 'node:stream';
import image from '../packages/image/index.js';

const jpg = 'fixtures/photo.jpg';
const png = 'fixtures/photo.png';
const webp = 'fixtures/photo.webp';
const png16Rgb = 'fixtures/png16-rgb.png';
const png16Rgba = 'fixtures/png16-rgba.png';
const png16Gray = 'fixtures/png16-gray.png';
const png16GrayAlpha = 'fixtures/png16-gray-alpha.png';

test('metadata is header-only and reports dimensions', async () => {
  const meta = await image(jpg).metadata();
  assert.equal(meta.format, 'jpeg');
  assert.equal(meta.width, 800);
  assert.equal(meta.height, 600);
  assert.equal(meta.channels, 3);
  assert.equal(meta.hasExif, false);
});

test('probe short-circuits to header information', async () => {
  const probe = await image(webp).probe();
  assert.equal(probe.format, 'webp');
  assert.equal(probe.width, 800);
  assert.equal(probe.height, 600);
});

test('png/jpeg/webp round trips', async () => {
  for (const format of ['png', 'jpeg', 'webp', 'qoi']) {
    const data = await image(png)[format]().toBuffer();
    assert.ok(data.length > 50, `${format} produced output`);
    const meta = await image(data).metadata();
    assert.equal(meta.width, 800);
    assert.equal(meta.height, 600);
  }
});

test('palette PNG reduces colours and stays decodable', async () => {
  // Compare against the stored (level 0) RGBA encode: the default encoder now compresses this
  // synthetic fixture to ~1% of its raw size, below what any dithered 8-bit palette can reach.
  const full = await image(png).png({ compressionLevel: 0 }).toBuffer();
  const indexed = await image(png).png({ palette: true, colors: 32, dither: 0.5 }).toBuffer();
  assert.equal(indexed[25], 3);
  assert.ok(indexed.length < full.length);
  const meta = await image(indexed).metadata();
  assert.equal(meta.width, 800);
  assert.equal(meta.height, 600);
});

test('16-bit PNG and raw ushort preserve wide samples', async () => {
  const width = 2;
  const height = 2;
  const samples = Uint16Array.from([
    0, 1000, 2000, 65535,
    12345, 23456, 34567, 40000,
    65535, 32768, 1, 0,
    257, 514, 771, 1028,
  ]);
  const input = Buffer.from(samples.buffer.slice(samples.byteOffset, samples.byteOffset + samples.byteLength));
  const encoded = await image(input, { raw: { width, height, channels: 4, depth: 'ushort' } })
    .png({ bitdepth: 16 })
    .toBuffer();
  assert.equal(encoded[24], 16);
  assert.equal(encoded[25], 6);
  const raw = await image(encoded).raw({ depth: 'ushort' }).toBuffer();
  assert.equal(raw.length, samples.length * 2);
  for (let i = 0; i < samples.length; i++) {
    assert.equal(raw.readUInt16LE(i * 2), samples[i]);
  }
});

test('toColourspace rgb16 upconverts 8-bit samples without truncation', async () => {
  const encoded = await image(png).toColourspace('rgb16').png().toBuffer();
  assert.equal(encoded[24], 16);
  const raw = await image(encoded).raw({ depth: 'ushort' }).toBuffer();
  const source = await image(png).raw().toBuffer();
  assert.equal(raw.readUInt16LE(0), source[0] * 257);
  assert.equal(raw.readUInt16LE(2), source[1] * 257);
});

test('external PNG16 fixtures report ushort metadata', async () => {
  const cases = [
    [png16Rgb, 3, 3, false],
    [png16Rgba, 3, 4, true],
    [png16Gray, 4, 1, false],
    [png16GrayAlpha, 3, 2, true],
  ];
  for (const [file, width, channels, hasAlpha] of cases) {
    const meta = await image(file).metadata();
    assert.equal(meta.format, 'png');
    assert.equal(meta.width, width);
    assert.equal(meta.height, 2);
    assert.equal(meta.channels, channels);
    assert.equal(meta.hasAlpha, hasAlpha);
    assert.equal(meta.depth, 'ushort');
    assert.equal(fs.readFileSync(file)[24], 16);
  }
});

test('external PNG16 fixtures decode losslessly to raw ushort', async () => {
  const rgb = await image(png16Rgb).raw({ depth: 'ushort' }).toBuffer({ resolveWithObject: true });
  assert.equal(rgb.info.channels, 3);
  assert.equal(rgb.info.depth, 'ushort');
  assert.deepEqual([...readU16(rgb.data, 0, 6)], [0, 1000, 2000, 3000, 4000, 5000]);

  const rgbaExpected = [
    1000, 2000, 3000, 65535, 4000, 5000, 6000, 32768, 7000, 8000, 9000, 0,
    10000, 11000, 12000, 12345, 13000, 14000, 15000, 65534, 16000, 17000, 18000, 1,
  ];
  const rgba = await image(png16Rgba).raw({ depth: 'ushort' }).toBuffer({ resolveWithObject: true });
  assert.equal(rgba.info.channels, 4);
  assert.deepEqual([...readU16(rgba.data, 0, rgbaExpected.length)], rgbaExpected);

  const gray = await image(png16Gray).raw({ depth: 'ushort' }).toBuffer({ resolveWithObject: true });
  assert.equal(gray.info.channels, 3);
  assert.equal(readU16(gray.data, 0, 1)[0], 0);
  assert.deepEqual([...readU16(gray.data, 6, 3)], [1, 1, 1]);

  const grayAlpha = await image(png16GrayAlpha).raw({ depth: 'ushort' }).toBuffer({ resolveWithObject: true });
  assert.equal(grayAlpha.info.channels, 4);
  assert.deepEqual([...readU16(grayAlpha.data, 0, 8)], [1000, 1000, 1000, 65535, 2000, 2000, 2000, 32768]);
});

test('no-op geometry and srgb preserve wide samples', async () => {
  const expected = await image(png16Rgba).raw({ depth: 'ushort' }).toBuffer();
  const outputs = await Promise.all([
    image(png16Rgba).rotate(0).raw({ depth: 'ushort' }).toBuffer(),
    image(png16Rgba).resize().raw({ depth: 'ushort' }).toBuffer(),
    image(png16Rgba).toColourspace('srgb').raw({ depth: 'ushort' }).toBuffer(),
  ]);
  for (const output of outputs) {
    assert.deepEqual(output, expected);
  }
});

test('rgb16 removes alpha from data and wide samples', async () => {
  const result = await image(png16Rgba)
    .toColourspace('rgb16')
    .raw({ depth: 'ushort' })
    .toBuffer({ resolveWithObject: true });
  assert.equal(result.info.channels, 3);
  assert.equal(result.info.depth, 'ushort');
  assert.equal(result.data.length, 3 * 2 * 3 * 2);
  assert.deepEqual([...readU16(result.data, 0, 6)], [1000, 2000, 3000, 4000, 5000, 6000]);

  const png = await image(png16Rgba).toColourspace('rgb16').png().toBuffer({ resolveWithObject: true });
  assert.equal(png.data[24], 16);
  assert.equal(png.data[25], 2);
  assert.equal(png.info.depth, 'ushort');
});

test('wide geometry preserves 16-bit precision', async () => {
  const extracted = await image(png16Rgba)
    .extract({ left: 1, top: 0, width: 2, height: 1 })
    .raw({ depth: 'ushort' })
    .toBuffer();
  assert.deepEqual([...readU16(extracted, 0, 8)], [4000, 5000, 6000, 32768, 7000, 8000, 9000, 0]);

  const flipped = await image(png16Rgba).flip().raw({ depth: 'ushort' }).toBuffer();
  assert.deepEqual([...readU16(flipped, 0, 4)], [10000, 11000, 12000, 12345]);

  const flopped = await image(png16Rgba).flop().raw({ depth: 'ushort' }).toBuffer();
  assert.deepEqual([...readU16(flopped, 0, 4)], [7000, 8000, 9000, 0]);

  const rotated90 = await image(png16Rgba).rotate(90).raw({ depth: 'ushort' }).toBuffer({ resolveWithObject: true });
  assert.equal(rotated90.info.width, 2);
  assert.equal(rotated90.info.height, 3);
  assert.deepEqual([...readU16(rotated90.data, 0, 8)], [10000, 11000, 12000, 12345, 1000, 2000, 3000, 65535]);

  const rotated180 = await image(png16Rgba).rotate(180).raw({ depth: 'ushort' }).toBuffer();
  assert.deepEqual([...readU16(rotated180, 0, 4)], [16000, 17000, 18000, 1]);

  const rotated270 = await image(png16Rgba).rotate(270).raw({ depth: 'ushort' }).toBuffer();
  assert.deepEqual([...readU16(rotated270, 0, 4)], [7000, 8000, 9000, 0]);
});

test('wide extend, resize and affine keep u16 samples', async () => {
  const extended = await image(png16Rgba)
    .extend({ width: 5, height: 4, background: '#112233' })
    .raw({ depth: 'ushort' })
    .toBuffer();
  assert.deepEqual([...readU16(extended, 0, 4)], [17 * 257, 34 * 257, 51 * 257, 65535]);
  assert.deepEqual([...readU16(extended, (1 * 5 + 1) * 8, 4)], [1000, 2000, 3000, 65535]);

  const resized = await image(png16Rgba)
    .resize(6, 4, { kernel: 'nearest' })
    .raw({ depth: 'ushort' })
    .toBuffer({ resolveWithObject: true });
  assert.equal(resized.info.width, 6);
  assert.equal(resized.info.height, 4);
  assert.equal(resized.info.depth, 'ushort');
  assert.ok(readU16(resized.data, 0, resized.data.length / 2).some((value) => value % 257 !== 0));

  const affine = await image(png16Rgba)
    .affine([[1, 0], [0, 1]])
    .raw({ depth: 'ushort' })
    .toBuffer({ resolveWithObject: true });
  assert.equal(affine.info.width, 3);
  assert.equal(affine.info.height, 2);
  const expected = await image(png16Rgba).raw({ depth: 'ushort' }).toBuffer();
  assert.deepEqual(affine.data, expected);
});

test('autoOrient keeps wide samples', async () => {
  const source = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .jpeg()
    .withMetadata({ orientation: 6 })
    .toBuffer();
  const exif = (await image(source).metadata()).exif;
  assert.ok(Buffer.isBuffer(exif));
  const tagged = await image(png16Rgba).withExif(exif).png({ bitdepth: 16 }).toBuffer();
  assert.equal((await image(tagged).metadata()).orientation, 6);
  const oriented = await image(tagged).autoOrient().raw({ depth: 'ushort' }).toBuffer({ resolveWithObject: true });
  assert.equal(oriented.info.width, 2);
  assert.equal(oriented.info.height, 3);
  assert.deepEqual([...readU16(oriented.data, 0, 4)], [10000, 11000, 12000, 12345]);
});

function readU16(buffer, offset, count) {
  const samples = new Array(count);
  for (let i = 0; i < count; i++) samples[i] = buffer.readUInt16LE(offset + i * 2);
  return samples;
}

test('lossy webp is smaller than lossless and decodes', async () => {
  const lossless = await image(jpg).webp({ lossless: true }).toBuffer();
  const lossy = await image(jpg).webp({ quality: 35, lossless: false }).toBuffer();
  assert.ok(lossy.length < lossless.length);
  const meta = await image(lossy).metadata();
  assert.equal(meta.format, 'webp');
  assert.equal(meta.width, 800);
  assert.equal(meta.height, 600);
});

test('resize changes dimensions and supports cover', async () => {
  const out = await image(jpg)
    .resize(200, 150)
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.width, 200);
  assert.equal(out.info.height, 150);

  const cover = await image(jpg)
    .resize(100, 100, { fit: 'cover' })
    .toBuffer({ resolveWithObject: true });
  assert.equal(cover.info.width, 100);
  assert.equal(cover.info.height, 100);
});

test('extract, extend, rotate, flip and flop', async () => {
  const extracted = await image(png)
    .extract({ left: 10, top: 20, width: 100, height: 50 })
    .toBuffer({ resolveWithObject: true });
  assert.equal(extracted.info.width, 100);
  assert.equal(extracted.info.height, 50);

  const extended = await image(png)
    .extend({ width: 900, height: 700, background: '#112233' })
    .toBuffer({ resolveWithObject: true });
  assert.equal(extended.info.width, 900);
  assert.equal(extended.info.height, 700);

  const rotated = await image(png)
    .rotate(90)
    .toBuffer({ resolveWithObject: true });
  assert.equal(rotated.info.width, 600);
  assert.equal(rotated.info.height, 800);

  await image(png).flip().flop().toBuffer();
});

test('autoOrient honors EXIF orientation', async () => {
  const rotated = await image(jpg).rotate(90).jpeg().toBuffer();
  assert.ok(rotated.length > 0);
  const source = await image(png).jpeg().withMetadata({ orientation: 6 }).toBuffer();
  const meta = await image(source).metadata();
  assert.equal(meta.orientation, 6);
  const oriented = await image(source).autoOrient().toBuffer({ resolveWithObject: true });
  assert.equal(oriented.info.width, 600);
  assert.equal(oriented.info.height, 800);
});

test('filters and point operations execute', async () => {
  const out = await image(png)
    .blur(1.5)
    .sharpen({ sigma: 1 })
    .median(3)
    .linear(1.1, -5)
    .gamma(1.2)
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.width, 800);
  assert.ok(out.data.length > 0);

  const grey = await image(png).greyscale().toBuffer({ resolveWithObject: true });
  assert.ok(grey.info.channels >= 3);
});

test('cover resize crops before resampling', async () => {
  const raw = Buffer.alloc(4 * 2 * 4);
  for (let y = 0; y < 2; y++) {
    for (let x = 0; x < 4; x++) {
      const index = (y * 4 + x) * 4;
      raw[index] = x < 2 ? 255 : 0;
      raw[index + 1] = 0;
      raw[index + 2] = x < 2 ? 0 : 255;
      raw[index + 3] = 255;
    }
  }
  const out = await image(raw, { raw: { width: 4, height: 2, channels: 4 } })
    .resize({ width: 1, height: 1, fit: 'cover', position: 'left', kernel: 'nearest' })
    .raw()
    .toBuffer();
  assert.deepEqual([...out.subarray(0, 4)], [255, 0, 0, 255]);
});

test('median and morphology produce expected windows', async () => {
  const medianInput = Buffer.from([0, 1, 2, 3, 4, 5, 6, 7, 8]);
  const median = await image(medianInput, { raw: { width: 3, height: 3, channels: 1 } })
    .median(3)
    .raw()
    .toBuffer();
  assert.deepEqual([...median.subarray(12, 15)], [4, 4, 4]);

  const morphologyInput = Buffer.alloc(25);
  morphologyInput[12] = 255;
  const dilated = await image(morphologyInput, { raw: { width: 5, height: 5, channels: 1 } })
    .dilate(1)
    .raw()
    .toBuffer();
  assert.equal(dilated[(1 * 5 + 1) * 3], 255);
  assert.equal(dilated[0], 0);
});

test('create, raw and text inputs', async () => {
  const created = await image({
    create: { width: 40, height: 30, channels: 4, background: '#ff0000' },
  })
    .png()
    .toBuffer({ resolveWithObject: true });
  assert.equal(created.info.width, 40);
  assert.equal(created.info.height, 30);

  const raw = await image(Buffer.alloc(10 * 10 * 3, 128), {
    raw: { width: 10, height: 10, channels: 3 },
  })
    .png()
    .toBuffer({ resolveWithObject: true });
  assert.equal(raw.info.width, 10);

  const text = await image({
    text: { text: 'Fastpkg', width: 320, height: 100, align: 'center' },
  })
    .png()
    .toBuffer();
  assert.ok(text.length > 100);
});

test('svg input renders', async () => {
  const svg = Buffer.from(
    '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="48"><rect width="64" height="48" fill="#0af"/></svg>'
  );
  const out = await image(svg).png().toBuffer({ resolveWithObject: true });
  assert.equal(out.info.width, 64);
  assert.equal(out.info.height, 48);
});

test('composite and boolean operations', async () => {
  const overlay = await image({ create: { width: 100, height: 100, channels: 4, background: '#00ff0080' } }).png().toBuffer();
  const composited = await image(png)
    .composite([{ input: overlay, gravity: 'centre' }])
    .toBuffer({ resolveWithObject: true });
  assert.equal(composited.info.width, 800);

  const mask = await image({ create: { width: 800, height: 600, channels: 4, background: '#ffffff' } }).png().toBuffer();
  const boolean = await image(png).boolean(mask, 'and').toBuffer();
  assert.ok(boolean.length > 0);
});

test('stats and dominant color', async () => {
  const stats = await image(png).stats();
  assert.ok(stats.channels.length >= 3);
  assert.ok(stats.entropy > 0);
  const dominant = await image(png).dominant();
  assert.ok(dominant && dominant.r >= 0);
});

test('avif encode and decode', async () => {
  const avif = await image(png).resize(200, 150).avif({ quality: 45, effort: 8 }).toBuffer();
  const meta = await image(avif).metadata();
  assert.equal(meta.format, 'avif');
  const decoded = await image(avif).jpeg().toBuffer({ resolveWithObject: true });
  assert.equal(decoded.info.width, 200);
  assert.equal(decoded.info.height, 150);
});

test('stream pipeline works as a duplex', async () => {
  const pipeline = image().resize(120).jpeg();
  Readable.from([fs.readFileSync(png)]).pipe(pipeline);
  const chunks = [];
  for await (const chunk of pipeline) chunks.push(chunk);
  const out = Buffer.concat(chunks);
  assert.ok(out.length > 100);
  const meta = await image(out).metadata();
  assert.equal(meta.width, 120);
});

test('toFile infers output format from extension', async () => {
  const file = 'fixtures/tmp-output.webp';
  try {
    const info = await image(png).resize(64).toFile(file);
    assert.equal(info.format, 'webp');
    assert.ok(fs.existsSync(file));
  } finally {
    fs.rmSync(file, { force: true });
  }
});

test('joinChannel applies an alpha mask', async () => {
  const base = await image({ create: { width: 32, height: 32, channels: 4, background: '#ff0000' } }).png().toBuffer();
  const mask = await image({ create: { width: 32, height: 32, channels: 4, background: '#000000' } }).png().toBuffer();
  const joined = await image(base).joinChannel(mask).png().toBuffer();
  const raw = await image(joined).raw().toBuffer();
  assert.equal(raw[3], 0);
});

test('deep zoom tile output writes a dz pyramid', async () => {
  const base = 'fixtures/tiles.dz';
  try {
    const info = await image({ create: { width: 128, height: 128, channels: 4, background: '#336699' } })
      .png()
      .tile({ size: 64 })
      .toFile(base);
    assert.equal(info.tile.layout, 'dz');
    assert.ok(fs.existsSync('fixtures/tiles.dzi'));
    assert.ok(fs.existsSync('fixtures/tiles_files/7/0_0.png'));
  } finally {
    fs.rmSync('fixtures/tiles.dzi', { force: true });
    fs.rmSync('fixtures/tiles_files', { recursive: true, force: true });
  }
});

test('legacy formats fail explicitly', async () => {
  assert.throws(() => image(png).tiff(), /TIFF/);
  assert.throws(() => image(png).jp2(), /JPEG 2000/);
  assert.throws(() => image(png).heif(), /HEIF/);
});

test('static utilities exist', () => {
  assert.equal(typeof image.versions.fastpkg, 'string');
  assert.equal(image.format.png.input, true);
  assert.equal(typeof image.interpolators.lanczos3, 'string');
  assert.ok(image.concurrency() >= 1);
});

test('toColourspace b-w produces a one-channel greyscale output', async () => {
  const out = await image({ create: { width: 8, height: 8, channels: 3, background: '#ff0000' } })
    .toColourspace('b-w')
    .png()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 1);
  const meta = await image(out.data).metadata();
  assert.equal(meta.channels, 1);
  assert.equal(meta.format, 'png');
});

test('toColourspace b-w keeps alpha as a second channel', async () => {
  const out = await image({ create: { width: 8, height: 8, channels: 4, background: '#ff000080' } })
    .toColourspace('b-w')
    .png()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 2);
  assert.equal(out.info.hasAlpha, true);
  assert.equal((await image(out.data).metadata()).channels, 2);
});

test('pipelineColourspace accepts linear working space', async () => {
  const out = await image({ create: { width: 8, height: 8, channels: 4, background: '#808080' } })
    .pipelineColourspace('linear')
    .toColourspace('srgb')
    .png()
    .toBuffer();
  assert.ok(out.length > 0);
});

test('composite supports the full sharp blend mode set', async () => {
  const dest = await image({ create: { width: 8, height: 8, channels: 4, background: '#ff0000' } }).png().toBuffer();
  const src = await image({ create: { width: 8, height: 8, channels: 4, background: '#0000ff' } }).png().toBuffer();
  const cases = [
    ['multiply', [0, 0, 0, 255]],
    ['screen', [255, 0, 255, 255]],
    ['add', [255, 0, 255, 255]],
    ['source', [0, 0, 255, 255]],
    ['in', [0, 0, 255, 255]],
    ['out', [0, 0, 0, 0]],
    ['xor', [0, 0, 0, 0]],
  ];
  for (const [blend, expected] of cases) {
    const out = await image(dest).composite([{ input: src, blend, left: 0, top: 0 }]).raw().toBuffer();
    assert.deepEqual([out[0], out[1], out[2], out[3]], expected, blend);
  }
});

test('timeout aborts an over-budget pipeline', async () => {
  await assert.rejects(
    image('fixtures/photo.jpg').resize(400).timeout({ seconds: 0.000001 }).toBuffer(),
    /timed out/
  );
});

test('withDensity writes JPEG and PNG density metadata', async () => {
  const sharp = (await import('sharp')).default;
  const jpeg = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .jpeg()
    .withDensity(300)
    .toBuffer();
  assert.equal((await sharp(jpeg).metadata()).density, 300);

  const png = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .png()
    .withDensity(300)
    .toBuffer();
  assert.equal((await sharp(png).metadata()).density, 300);
});

test('keepExif preserves EXIF and metadata returns Buffers', async () => {
  const source = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .jpeg()
    .withMetadata({ orientation: 6 })
    .toBuffer();
  const meta = await image(source).metadata();
  assert.ok(Buffer.isBuffer(meta.exif));
  const output = await image(source).keepExif().jpeg().toBuffer();
  const outMeta = await image(output).metadata();
  assert.equal(outMeta.orientation, 6);
  assert.ok(Buffer.isBuffer(outMeta.exif));
});

test('withExif injects raw EXIF from a Buffer', async () => {
  const source = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .jpeg()
    .withMetadata({ orientation: 6 })
    .toBuffer();
  const exif = (await image(source).metadata()).exif;
  assert.ok(Buffer.isBuffer(exif));
  const output = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .jpeg()
    .withExif(exif)
    .toBuffer();
  assert.equal((await image(output).metadata()).orientation, 6);
});

test('keepExif works for PNG output', async () => {
  const source = await image({ create: { width: 8, height: 8, channels: 4, background: '#ffffff' } })
    .jpeg()
    .withMetadata({ orientation: 6 })
    .toBuffer();
  const output = await image(source).keepExif().png().toBuffer();
  assert.equal((await image(output).metadata()).orientation, 6);
});

test('tile layouts and zip containers', async () => {
  const source = await image({ create: { width: 64, height: 64, channels: 4, background: '#336699' } })
    .png()
    .toBuffer();
  for (const layout of ['google', 'zoomify', 'iiif', 'iiif3']) {
    const dir = `fixtures/tiles-${layout}`;
    try {
      await image(source).tile({ layout, size: 32 }).toFile(dir);
      assert.ok(fs.existsSync(dir));
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }
  const zip = await image(source).tile({ layout: 'dz', size: 32 }).toBuffer();
  assert.equal(zip.subarray(0, 2).toString(), 'PK');
  const file = 'fixtures/tiles.zip';
  try {
    await image(source).tile({ layout: 'dz', size: 32, container: 'zip' }).toFile(file);
    assert.ok(fs.existsSync(file));
    assert.equal(fs.readFileSync(file).subarray(0, 2).toString(), 'PK');
  } finally {
    fs.rmSync(file, { force: true });
  }
});
