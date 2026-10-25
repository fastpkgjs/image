// PNG encoder: pinned bytes, thread-count independence, level ladder and palette semantics.
//
// The byte output of the PNG encoder is NOT libvips-identical (own filter choice, zlib-rs,
// block-parallel deflate, own quantiser) but is stable within a release line: the golden hashes
// below pin it, so any change to the encoded bytes is a deliberate edit of this file (and a
// CHANGELOG entry).
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import zlib from 'node:zlib';
import { sharp, engine } from './_util.mjs';
import { images, cases, sha256 } from './_png-cases.mjs';

const GOLDEN = {
  "flatRgba/level0": "6e05b5505b85ed7b08203fbecac8ea4c80728feb9a81ff93e39721661d24d1f2",
  "flatRgba/level1": "8917853c44e44fdeeb91c68249c09120e1be125969b98773789f7d4f5c9ceedd",
  "flatRgba/level6": "5bd58ccaafc207f2940df2dbb2456e7df92ff0bdbe78a5ecdc0637f50410562f",
  "flatRgba/level9": "500629ec9c024522c62004a379e452c9a4e980b8528814cb0ea429a6cb3fe609",
  "flatRgb/level0": "48457903f8ea7046f185068669ab30027943f183209b5b568ab3716cf4718279",
  "flatRgb/level1": "065498b0cf4028972a0279d0f6b8c4ac4cf2c8d9b10bd738d6c1838607d5bc28",
  "flatRgb/level6": "f7df39d1d66421c5f54e13cc2bbc8a6348cf161c991caf785c90485c2b2d289a",
  "flatRgb/level9": "732e021d227fb7eb7c96ff0a9caa49c1db3906a4783f8b95f42b3809e738b5f9",
  "gradientRgb/level0": "8a0038eaef5b44fe42cb6b2db657f2a3e6b4e4fff4a4fc2c92ae21259a1eebe6",
  "gradientRgb/level1": "a6f115947cbde6fde91c04db3426f0840e7919d93eb45765e0640f983d13f391",
  "gradientRgb/level6": "3c8930a19b452776d661667b832c37722dc8038157d435a79dc29e3f9b643004",
  "gradientRgb/level9": "e4042b38d022ed30014d5de2661c1aaf2e1e1af6f7e54661a54160a25719820e",
  "noisyRgb/level0": "5d0fcb48d89a3c4b70ca82dbfecab78648bd6291212531ed9ccea2400828c347",
  "noisyRgb/level1": "69844570d345159f3c1dc896e2fd654276c9074758aec7815c305e48ab4fa52e",
  "noisyRgb/level6": "90687ee03faf475a7c507a715d4168799594cc652f567d64bba5e00a841220ba",
  "noisyRgb/level9": "8ca225e3fe3bd6d9658e7ba022fda2b27d857bb4c3c6b9aeb1aeda7fda7d9e27",
  "gray/level0": "03d64e4637325649d01de12896508465176c003a93f3bae71c59f4fdf73042b2",
  "gray/level1": "3127f9147e628ffd9e5a686829acbfcaa0a178d2a5bb1e386ff98e6256ce2ba6",
  "gray/level6": "06d28fce03da98f9fb7c98cfeb54da83f36f9abbe822a27e3a2039885e2859fc",
  "gray/level9": "9e1b658f2cb7368ce889bd52c44218f6f337b86dbd072d8d524c02b0791b51ad",
  "noisyRgba/default": "96a5f96bdfe79d10a58bfc958e190f1bae6bab21df80f4b2555674f51d89d521",
  "largeRgba/default": "9eeb5e694a77659740dfa2b00c30fe985146ec6f49774d50f86a3d824fbc30bf",
  "largeRgba/level1": "840b4c1c111397f59238fac1c5e7c76fdd717c6ecb295be633f9111b9b2aefce",
  "largeRgba/level9": "2ea8ed230b538ff3e6ac786e1d69fe880e916153c97962503d03653a004a35d2",
  "gradientRgb/adaptiveFiltering=false": "0376105f51b1b0b4b50ef7ed85ad2a066c9d3c49fe400d7be9e381d8873e1ada",
  "flatRgb/adaptiveFiltering=true": "e3048e9a9127e022e4bbe3cbb9e76735d1ff3a455f6599993fbfdad5ffd3e837",
  "noisyRgb/progressive": "988a11a34d1fb156c8f640ca40aeb738588807fa390839e063afcbe2521f1932",
  "flatRgba/palette": "f27fe24317fcc06bd0d8a926b68e9314545883728250c94a1a4c7a117b220aae",
  "flatRgb/palette": "4a44d47353fdce40217e60c6a1c0a1af30686daecb175a1cb7ff7ad75c338a5e",
  "noisyRgb/palette": "594387b137f865369899654a99daf5bb201a10ea9f25f86e99a6607011a59fd5",
  "noisyRgba/palette": "ae4cdab8eed046be68b209c637f7a34042a440fc755b2aee9746a05d83a8014b",
  "noisyRgb/palette-colours16": "7eae7fbaf1a5a0bad2f34d670383e5e1f394d120b8c739d5898971f6bf860248",
  "noisyRgb/palette-dither0-level0": "6f9a1bde33e2b95b74479e0c5377d187bb9b510b53431d3701f905dd23122801",
  "noisyRgb/palette-quality40-effort1": "29716d36c66272e0d8d6071e6d04abd2faece05e3623c0c36aae27630a292a44",
  "flatRgba/palette-progressive": "6906c5ecd94dd87c0f1a44bba0727282ef62c580de74ab38c139a59024fb9de4",
  "noisyRgb/rgb16": "1fdeb271470ba2b0730d1236696bca086855244c9ae69a957d28cf3cbc583da9",
  "flatRgba/rgb16-level0": "aee323e65a8547fc747d08e55dd048517b0f5d417424a1d79fde335ff08877ab",
};

const childPath = fileURLToPath(new URL('./_png-hash-child.mjs', import.meta.url));
const runChild = (threads) => {
  const run = spawnSync(process.execPath, [childPath, String(threads)], { encoding: 'utf8', maxBuffer: 1 << 24 });
  assert.equal(run.status, 0, run.stderr);
  return JSON.parse(run.stdout);
};

const prepare = (name, input) => (images[name].channels === 1 ? input.toColourspace('b-w') : input);
const make = (name) => {
  const { px, width, height, channels } = images[name];
  return prepare(name, engine(px, { raw: { width, height, channels } }));
};
const makeSharp = (name) => {
  const { px, width, height, channels } = images[name];
  return prepare(name, sharp(px, { raw: { width, height, channels } }));
};
const decode = (buf) => sharp(buf).raw().toBuffer({ resolveWithObject: true });

/** Chunks of a PNG as { type, data } in file order. */
function chunks(buf) {
  const out = [];
  for (let at = 8; at < buf.length; ) {
    const len = buf.readUInt32BE(at);
    out.push({ type: buf.toString('latin1', at + 4, at + 8), data: buf.subarray(at + 8, at + 8 + len) });
    at += 12 + len;
  }
  return out;
}
const idat = (buf) => Buffer.concat(chunks(buf).filter((c) => c.type === 'IDAT').map((c) => c.data));

test('golden hashes: PNG bytes are pinned for generated images', async () => {
  const { hashes } = runChild(0);
  assert.deepEqual(hashes, GOLDEN);
});

test('PNG bytes do not depend on the thread count', () => {
  const one = runChild(1);
  const many = runChild(8);
  assert.equal(one.threads, 1);
  assert.deepEqual(one.hashes, many.hashes);
  assert.deepEqual(one.hashes, GOLDEN);
});

test('every golden case decodes to the source pixels (lossless cases)', async () => {
  for (const entry of cases) {
    if (entry.options.palette || entry.options.colours || entry.options.quality) continue;
    const { px, channels } = images[entry.name];
    const out = await make(entry.name).png(entry.options).toBuffer();
    const { data, info } = await decode(out);
    if (channels === 1) {
      // sharp decodes grey PNGs as 3-channel raw.
      for (let i = 0; i < px.length; i++) assert.equal(data[i * info.channels], px[i], entry.id);
      continue;
    }
    assert.equal(info.channels, channels, entry.id);
    assert.ok(Buffer.compare(data, px) === 0, `${entry.id}: pixels differ`);
  }
});

test('compressionLevel 0 writes stored deflate blocks, higher levels compress', async () => {
  const stored = await make('flatRgba').png({ compressionLevel: 0 }).toBuffer();
  const z = idat(stored);
  assert.equal(z[0], 0x78);
  // First deflate block header: BFINAL=0/1, BTYPE=00 (stored) -> low 3 bits are 000 or 001.
  assert.equal(z[2] & 0b110, 0);
  assert.deepEqual(zlib.inflateSync(z).length, images.flatRgba.height * (images.flatRgba.width * 4 + 1));
  assert.ok(stored.length >= images.flatRgba.px.length, 'stored output is not smaller than the raw pixels');
  for (const level of [1, 3, 6, 9]) {
    const packed = await make('flatRgba').png({ compressionLevel: level }).toBuffer();
    assert.ok(packed.length < stored.length / 4, `level ${level}: ${packed.length} vs stored ${stored.length}`);
  }
  const def = await make('flatRgba').png().toBuffer();
  const six = await make('flatRgba').png({ compressionLevel: 6 }).toBuffer();
  assert.ok(Buffer.compare(def, six) === 0, 'default level is 6');
});

test('default PNG is no larger than sharp (+2%) on flat, smooth and noisy images', async () => {
  for (const name of ['flatRgba', 'flatRgb', 'gradientRgb', 'noisyRgb', 'noisyRgba', 'gray', 'largeRgba']) {
    const ours = await make(name).png().toBuffer();
    const theirs = await makeSharp(name).png().toBuffer();
    assert.ok(ours.length <= theirs.length * 1.02, `${name}: ${ours.length} vs sharp ${theirs.length}`);
  }
});

test('adaptiveFiltering is honoured', async () => {
  const on = await make('gradientRgb').png({ adaptiveFiltering: true }).toBuffer();
  const off = await make('gradientRgb').png({ adaptiveFiltering: false }).toBuffer();
  assert.ok(on.length * 4 < off.length, `adaptive ${on.length} vs none ${off.length}`);
  // Forced "none" writes only filter type 0.
  const raw = zlib.inflateSync(idat(off));
  const stride = images.gradientRgb.width * 3 + 1;
  for (let y = 0; y < images.gradientRgb.height; y++) assert.equal(raw[y * stride], 0);
  assert.throws(() => engine(Buffer.alloc(3), { raw: { width: 1, height: 1, channels: 3 } }).png({ adaptiveFiltering: 'yes' }), TypeError);
});

test('16-bit output stays 16-bit and lossless', async () => {
  const out = await make('noisyRgb').toColourspace('rgb16').png().toBuffer();
  assert.equal(out[24], 16);
  assert.equal((await sharp(out).metadata()).depth, 'ushort');
  const reference = await makeSharp('noisyRgb').toColourspace('rgb16').png().toBuffer();
  const ours16 = await sharp(out).toColourspace('rgb16').raw().toBuffer();
  const reference16 = await sharp(reference).toColourspace('rgb16').raw().toBuffer();
  assert.ok(Buffer.compare(ours16, reference16) === 0);
});

test('palette: exact for few colours, with tRNS, and much smaller than RGBA', async () => {
  const out = await make('flatRgba').png({ palette: true }).toBuffer();
  assert.equal(out[25], 3);
  const kinds = chunks(out).map((c) => c.type);
  assert.ok(kinds.includes('PLTE') && kinds.includes('tRNS'));
  assert.ok(kinds.indexOf('PLTE') < kinds.indexOf('tRNS') && kinds.indexOf('tRNS') < kinds.indexOf('IDAT'));
  const { px } = images.flatRgba;
  const { data } = await sharp(out).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  for (let i = 0; i < px.length; i += 4) {
    assert.equal(data[i + 3], px[i + 3]);
    if (px[i + 3] !== 0) assert.deepEqual([...data.subarray(i, i + 3)], [...px.subarray(i, i + 3)]);
  }
  const rgba = await make('flatRgba').png().toBuffer();
  assert.ok(out.length < rgba.length, `${out.length} vs ${rgba.length}`);
});

test('palette: quality/effort/colours/dither alone switch palette output on', async () => {
  for (const options of [{ quality: 80 }, { effort: 3 }, { colours: 64 }, { colors: 64 }, { dither: 0.5 }]) {
    const out = await make('noisyRgb').png(options).toBuffer();
    assert.equal(out[25], 3, JSON.stringify(options));
  }
  const off = await make('noisyRgb').png({ palette: false, quality: 80 }).toBuffer();
  assert.equal(off[25], 2);
});

test('palette: colours selects 1/2/4/8-bit depth like libvips', async () => {
  const entries = (buf) => chunks(buf).find((c) => c.type === 'PLTE').data.length / 3;
  for (const [colours, depth, max] of [[2, 1, 2], [4, 2, 4], [10, 4, 16], [16, 4, 16], [100, 8, 256]]) {
    const out = await make('noisyRgb').png({ palette: true, colours }).toBuffer();
    const theirs = await makeSharp('noisyRgb').png({ palette: true, colours }).toBuffer();
    assert.equal(out[24], theirs[24], `colours ${colours} bit depth`);
    assert.equal(out[24], depth);
    assert.ok(entries(out) <= max);
  }
});

test('palette: quality 100 stays close to sharp in size and error on photo-like content', async () => {
  const src = images.noisyRgb;
  const mad = async (buf) => {
    const { data } = await sharp(buf).raw().toBuffer({ resolveWithObject: true });
    let sum = 0;
    for (let i = 0; i < data.length; i++) sum += Math.abs(data[i] - src.px[i]);
    return sum / data.length;
  };
  for (const options of [{ palette: true }, { palette: true, dither: 0 }, { quality: 60 }]) {
    const ours = await make('noisyRgb').png(options).toBuffer();
    const theirs = await makeSharp('noisyRgb').png(options).toBuffer();
    assert.ok(ours.length <= theirs.length * 1.1, `${JSON.stringify(options)} size ${ours.length} vs ${theirs.length}`);
    assert.ok((await mad(ours)) <= (await mad(theirs)) * 1.15 + 0.3, `${JSON.stringify(options)} error`);
  }
});

test('palette + progressive writes a valid interlaced indexed PNG', async () => {
  const out = await make('flatRgba').png({ palette: true, progressive: true }).toBuffer();
  assert.equal(out[25], 3);
  assert.equal(out[28], 1);
  const meta = await sharp(out).metadata();
  assert.equal(meta.isProgressive, true);
});

test('metadata chunks keep PNG ordering with a palette (iCCP before PLTE)', async () => {
  const out = await make('noisyRgb').png({ palette: true }).withIccProfile('srgb').withMetadata({ density: 144 }).toBuffer();
  const order = chunks(out).map((c) => c.type);
  assert.ok(order.includes('iCCP'), 'iCCP embedded');
  assert.ok(order.indexOf('iCCP') < order.indexOf('PLTE'));
  assert.ok(order.indexOf('pHYs') < order.indexOf('IDAT'));
  assert.ok(order.indexOf('PLTE') < order.indexOf('IDAT'));
});
