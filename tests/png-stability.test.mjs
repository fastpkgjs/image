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
  "flatRgba/level1": "9fa5dd9b3c6689be13157a2993555d20f0e8e4b5a68869f9ea195bf3a2c1f1e9",
  "flatRgba/level6": "9fa5dd9b3c6689be13157a2993555d20f0e8e4b5a68869f9ea195bf3a2c1f1e9",
  "flatRgba/level9": "9fa5dd9b3c6689be13157a2993555d20f0e8e4b5a68869f9ea195bf3a2c1f1e9",
  "flatRgb/level0": "48457903f8ea7046f185068669ab30027943f183209b5b568ab3716cf4718279",
  "flatRgb/level1": "8ff86a640348734d83334700ef3192df6a1059d15e087d9f5d5021c1479d0d27",
  "flatRgb/level6": "8ff86a640348734d83334700ef3192df6a1059d15e087d9f5d5021c1479d0d27",
  "flatRgb/level9": "8ff86a640348734d83334700ef3192df6a1059d15e087d9f5d5021c1479d0d27",
  "gradientRgb/level0": "8a0038eaef5b44fe42cb6b2db657f2a3e6b4e4fff4a4fc2c92ae21259a1eebe6",
  "gradientRgb/level1": "22229c7cd83122a6a7513ca48ffc61d3126a72aaa39076723548dd0e86a3de96",
  "gradientRgb/level6": "3c8930a19b452776d661667b832c37722dc8038157d435a79dc29e3f9b643004",
  "gradientRgb/level9": "e4042b38d022ed30014d5de2661c1aaf2e1e1af6f7e54661a54160a25719820e",
  "noisyRgb/level0": "5d0fcb48d89a3c4b70ca82dbfecab78648bd6291212531ed9ccea2400828c347",
  "noisyRgb/level1": "b46528c4e73123fb1817257f71e16dcbf2a5d1211e005cd107308f889178f6d2",
  "noisyRgb/level6": "b46528c4e73123fb1817257f71e16dcbf2a5d1211e005cd107308f889178f6d2",
  "noisyRgb/level9": "b46528c4e73123fb1817257f71e16dcbf2a5d1211e005cd107308f889178f6d2",
  "gray/level0": "03d64e4637325649d01de12896508465176c003a93f3bae71c59f4fdf73042b2",
  "gray/level1": "7ddb272f69aff8e1caf60f1c70db445ae68ee0b05ba7be57e49fe26a912ebf14",
  "gray/level6": "7ddb272f69aff8e1caf60f1c70db445ae68ee0b05ba7be57e49fe26a912ebf14",
  "gray/level9": "7ddb272f69aff8e1caf60f1c70db445ae68ee0b05ba7be57e49fe26a912ebf14",
  "noisyRgba/default": "6f1bc20325e88fce0bfbfaa0fefa153484a973a8ff6fbdacd8fccefa95e3c7a8",
  "largeRgba/default": "a95b651dad4ae2c994bc4554fa62046abf8f233cda730338a527175285418519",
  "largeRgba/level1": "a95b651dad4ae2c994bc4554fa62046abf8f233cda730338a527175285418519",
  "largeRgba/level9": "a95b651dad4ae2c994bc4554fa62046abf8f233cda730338a527175285418519",
  "bigFlat/default": "048d3d27a9e9820d631974131330d0dd3bb5cead60028368599fe8bcad30e6de",
  "bigFlat/level1": "048d3d27a9e9820d631974131330d0dd3bb5cead60028368599fe8bcad30e6de",
  "tiled/default": "bcc26940958476189a8a8c97244543907863f58bb3e6dcb0cb226d95703d8cdf",
  "tiled/level1": "8c274163d709922494dfe0e787cbce18af6c72fd50cae8f6d617a87ffa2f8aee",
  "tiled/level3": "3a961c997493a00569f78bce17f187cac8c253febd353e191461dbaac5bebf4f",
  "tiled/level9": "68598b87538afcb9b9c4c046d80ad9b2a13e22dc2ddc28aa6b938a90339e81d0",
  "gradientRgb/adaptiveFiltering=false": "834b5f520fe73fce070ee44db547909331a1e1836aeb539cb3fe41e0f41b9052",
  "flatRgb/adaptiveFiltering=true": "8ff86a640348734d83334700ef3192df6a1059d15e087d9f5d5021c1479d0d27",
  "noisyRgb/progressive": "cc0a28350eba6c8698b1a5f1afbdbcfef6c0a82a3396e5691f777090fdcc0ce1",
  "flatRgba/palette": "f27fe24317fcc06bd0d8a926b68e9314545883728250c94a1a4c7a117b220aae",
  "flatRgb/palette": "4a44d47353fdce40217e60c6a1c0a1af30686daecb175a1cb7ff7ad75c338a5e",
  "noisyRgb/palette": "4c6432d0609479421868873feff701f89b1d1f83798b06bb127f374d41d54c34",
  "noisyRgba/palette": "a0de58e7d0a04e09d0cc6024e5ee461e8a01b3a6061fd764f9f0721b1c049c12",
  "noisyRgb/palette-colours16": "7eae7fbaf1a5a0bad2f34d670383e5e1f394d120b8c739d5898971f6bf860248",
  "noisyRgb/palette-dither0-level0": "6f9a1bde33e2b95b74479e0c5377d187bb9b510b53431d3701f905dd23122801",
  "noisyRgb/palette-quality40-effort1": "29bff59872d9bd6d500062b20931f0e1f9cd1205f24624adafce09af3d73ccbb",
  "flatRgba/palette-progressive": "3ca305686ec5237d95a24875837315a4383682814efc7f4f6e2c3031ac5ca0a3",
  "noisyRgb/rgb16": "64f30168da1262bad968eeb0d527dd08e6411ef16d0d22cbe162c1eb04a3fdd8",
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
  for (const name of ['flatRgba', 'flatRgb', 'gradientRgb', 'noisyRgb', 'noisyRgba', 'gray', 'largeRgba', 'bigFlat', 'tiled']) {
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
