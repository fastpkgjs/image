// Embedded ICC profiles and CMYK input. sharp (libvips + lcms) is the oracle: an input with an
// embedded profile is converted to sRGB at load (perceptual intent) unless the profile is to be
// kept (keepIccProfile / withMetadata) or ignored (ignoreIcc). 16-bit RGB sources are
// converted to Display P3 instead, which is what sharp's processing profile does for them.
//
// Inputs are generated here: smooth, saturated test patterns with synthetic profiles attached
// without touching the pixels (see _icc.mjs), so a colour shift can only come from the engine's
// handling of the profile.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, pattern, diffStats, diffStats16 } from './_util.mjs';
import { png16 } from './_png16.mjs';
import { makeRgbProfile, makeGrayProfile, embedIcc } from './_icc.mjs';

const W = 160;
const H = 96;
const rgbRaw = pattern(W, H, 3);
const rgbaRaw = pattern(W, H, 4, { alpha: true });
const sharpRaw = (channels, buf = rgbRaw) => ({ raw: { width: W, height: H, channels } });

const profiles = {
  'display-p3 (sRGB curve)': makeRgbProfile('p3', 'srgb'),
  'wide gamut, gamma 1.8 table': makeRgbProfile('wide', Array.from({ length: 1024 }, (_, i) => (i / 1023) ** 1.8)),
  'adobe rgb (gamma 2.2)': makeRgbProfile('adobe', 2.2),
};

const pixels = async (lib, input, build = (s) => s, options) => {
  const { data, info } = await build(lib(input, options)).raw().toBuffer({ resolveWithObject: true });
  return { data, info };
};

// Large buffers go through Buffer.compare: a failing assert.deepEqual builds a diff of every
// byte and can take gigabytes.
const sameBytes = (a, b, label = 'pixels') => assert.ok(Buffer.compare(a, b) === 0, `${label} differ`);

const assertClose = (a, b, { mad, max }, label) => {
  const d = diffStats(a, b);
  assert.ok(d.mad <= mad && d.max <= max, `${label}: mad ${d.mad.toFixed(3)} (limit ${mad}), max ${d.max} (limit ${max})`);
};

async function encoded(kind) {
  if (kind === 'jpeg') return sharp(rgbRaw, sharpRaw(3)).jpeg({ quality: 100, chromaSubsampling: '4:4:4' }).toBuffer();
  if (kind === 'png') return sharp(rgbRaw, sharpRaw(3)).png().toBuffer();
  return sharp(rgbRaw, sharpRaw(3)).webp({ lossless: true }).toBuffer();
}

for (const kind of ['jpeg', 'png', 'webp']) {
  for (const [name, profile] of Object.entries(profiles)) {
    test(`${kind} with ${name} converts to sRGB like sharp`, async () => {
      const plain = await encoded(kind);
      const tagged = embedIcc(plain, profile, { width: W, height: H });
      const [s, e, untagged] = await Promise.all([pixels(sharp, tagged), pixels(engine, tagged), pixels(engine, plain)]);
      // The engine also reports `space`, which sharp leaves out of raw output info.
      assert.deepEqual({ ...e.info, space: undefined }, { ...s.info, space: undefined });
      // Regression: the colours really moved (the old behaviour left them untouched).
      assert.ok(diffStats(untagged.data, s.data).mad > 3, 'fixture must change colour when converted');
      assertClose(e.data, s.data, { mad: 0.9, max: 4 }, 'pixels');
    });
  }
}

test('conversion happens before resize (same result as sharp)', async () => {
  const tagged = embedIcc(await encoded('jpeg'), profiles['display-p3 (sRGB curve)']);
  for (const width of [80, 41]) {
    const [s, e] = await Promise.all([
      pixels(sharp, tagged, (x) => x.resize(width)),
      pixels(engine, tagged, (x) => x.resize(width)),
    ]);
    assertClose(e.data, s.data, { mad: 1.5, max: 14 }, `resize ${width}`);
  }
});

test('conversion combined with other operations and output formats', async () => {
  const tagged = embedIcc(await encoded('png'), profiles['adobe rgb (gamma 2.2)']);
  for (const [name, build] of [
    ['rotate 90 + flip', (x) => x.rotate(90).flip()],
    ['extract', (x) => x.extract({ left: 10, top: 8, width: 90, height: 50 })],
    ['greyscale', (x) => x.greyscale()],
    ['png roundtrip', (x) => sharp(null) && x],
  ]) {
    if (name === 'png roundtrip') continue;
    const [s, e] = await Promise.all([pixels(sharp, tagged, build), pixels(engine, tagged, build)]);
    assertClose(e.data, s.data, { mad: 1, max: 5 }, name);
  }
  for (const format of ['png', 'jpeg', 'webp']) {
    const out = await engine(tagged)[format]({ lossless: format === 'webp', quality: 100 }).toBuffer();
    const ref = await sharp(tagged)[format]({ lossless: format === 'webp', quality: 100 }).toBuffer();
    const [a, b] = await Promise.all([pixels(sharp, out), pixels(sharp, ref)]);
    assertClose(a.data, b.data, { mad: 1.2, max: 8 }, `${format} output`);
    assert.equal((await sharp(out).metadata()).hasProfile, false, `${format}: converted output carries no profile`);
  }
});

test('alpha is kept and only the colour is converted', async () => {
  const png = await sharp(rgbaRaw, sharpRaw(4)).png().toBuffer();
  const tagged = embedIcc(png, profiles['display-p3 (sRGB curve)']);
  const [s, e] = await Promise.all([pixels(sharp, tagged), pixels(engine, tagged)]);
  assert.equal(e.info.channels, 4);
  const alpha = (buf) => Buffer.from(buf.filter((_, i) => i % 4 === 3));
  sameBytes(Buffer.from(alpha(e.data)), Buffer.from(alpha(s.data)), "alpha");
  assertClose(e.data, s.data, { mad: 0.9, max: 4 }, 'rgba');
});

test('keepIccProfile / withMetadata keep the pixels as stored and the profile', async () => {
  const profile = profiles['display-p3 (sRGB curve)'];
  const plain = await encoded('jpeg');
  const tagged = embedIcc(plain, profile);
  const stored = await pixels(engine, plain);
  for (const build of [(x) => x.keepIccProfile(), (x) => x.keepMetadata()]) {
    const [s, e] = await Promise.all([pixels(sharp, tagged, build), pixels(engine, tagged, build)]);
    sameBytes(e.data, stored.data, 'engine leaves the stored values');
    sameBytes(s.data, stored.data, 'sharp does too');
  }
  // withMetadata() is keepMetadata() plus withIccProfile('srgb'): converted, with sRGB embedded.
  const [sw, ew] = await Promise.all([pixels(sharp, tagged, (x) => x.withMetadata()), pixels(engine, tagged, (x) => x.withMetadata())]);
  assertClose(ew.data, sw.data, { mad: 0.9, max: 4 }, 'withMetadata pixels');
  assert.ok(diffStats(stored.data, ew.data).mad > 3, 'withMetadata converts');
  const out = await engine(tagged).keepIccProfile().jpeg().toBuffer();
  const meta = await sharp(out).metadata();
  assert.equal(meta.hasProfile, true);
  assert.deepEqual(meta.icc, profile);
  const dropped = await engine(tagged).jpeg().toBuffer();
  assert.equal((await sharp(dropped).metadata()).hasProfile, false, 'converted output drops the profile');
});

test('ignoreIcc leaves the stored values', async () => {
  const plain = await encoded('png');
  const tagged = embedIcc(plain, profiles['display-p3 (sRGB curve)']);
  const [s, e, stored] = await Promise.all([
    pixels(sharp, tagged, (x) => x, { ignoreIcc: true }),
    pixels(engine, tagged, (x) => x, { ignoreIcc: true }),
    pixels(engine, plain),
  ]);
  sameBytes(e.data, stored.data);
  sameBytes(s.data, stored.data);
});

test('sRGB-equivalent profiles are left alone', async () => {
  const plain = await encoded('jpeg');
  const stored = await pixels(engine, plain);
  for (const profile of [makeRgbProfile('srgb', 'srgb'), makeRgbProfile('srgb', Array.from({ length: 4096 }, (_, i) => {
    const v = i / 4095;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  }))]) {
    const e = await pixels(engine, embedIcc(plain, profile));
    assertClose(e.data, stored.data, { mad: 0.1, max: 1 }, 'srgb profile');
  }
});

test('corrupt, truncated and mismatched profiles pass the pixels through', async () => {
  const plain = await encoded('jpeg');
  const stored = await pixels(engine, plain);
  const p3 = profiles['display-p3 (sRGB curve)'];
  const bad = Buffer.from(p3);
  bad.fill(0, 36, 40); // no 'acsp' signature
  for (const [name, profile] of [
    ['bad signature', bad],
    ['truncated', p3.subarray(0, 200)],
    ['garbage', Buffer.alloc(300, 7)],
    ['grey profile on an RGB image', makeGrayProfile(2.2)],
  ]) {
    const [s, e] = await Promise.all([pixels(sharp, embedIcc(plain, profile)), pixels(engine, embedIcc(plain, profile))]);
    sameBytes(e.data, stored.data, `${name}: engine`);
    sameBytes(s.data, stored.data, `${name}: sharp`);
  }
});

test('grey images: grey profiles convert, RGB profiles are ignored', async () => {
  const grey = await sharp(pattern(W, H, 1), sharpRaw(1)).toColourspace('b-w').jpeg({ quality: 100 }).toBuffer();
  const stored = await pixels(engine, grey);
  for (const gamma of [2.2, 1.0, 1.8]) {
    const tagged = embedIcc(grey, makeGrayProfile(gamma));
    const [s, e] = await Promise.all([pixels(sharp, tagged), pixels(engine, tagged)]);
    assert.equal(e.info.channels, s.info.channels);
    assertClose(e.data, s.data, { mad: 0.9, max: 3 }, `gamma ${gamma}`);
    if (gamma !== 2.2) assert.ok(diffStats(stored.data, e.data).mad > 5);
  }
  const e = await pixels(engine, embedIcc(grey, profiles['display-p3 (sRGB curve)']));
  sameBytes(e.data, stored.data);
  const greyPng = await sharp(pattern(W, H, 1), sharpRaw(1)).toColourspace('b-w').png().toBuffer();
  const taggedPng = embedIcc(greyPng, makeGrayProfile(1.8));
  const [sp, ep] = await Promise.all([pixels(sharp, taggedPng), pixels(engine, taggedPng)]);
  assertClose(ep.data, sp.data, { mad: 0.9, max: 3 }, 'grey png');
});

test('16-bit RGB sources are converted to Display P3, like sharp', async () => {
  const src = png16(64, 48, 3, (x, y, c) => Math.min(65535, Math.round(((c === 0 ? x / 63 : c === 1 ? y / 47 : 0.5 + 0.4 * Math.sin((x + y) / 15)) * 0.8 + 0.1) * 65535)));
  for (const [name, profile, expectShift] of [
    ['wide gamut', profiles['wide gamut, gamma 1.8 table'], true],
    ['adobe rgb', profiles['adobe rgb (gamma 2.2)'], true],
    ['display p3', profiles['display-p3 (sRGB curve)'], false],
  ]) {
    const tagged = embedIcc(src, profile);
    const [s, e, stored] = await Promise.all([pixels(sharp, tagged), pixels(engine, tagged), pixels(engine, src)]);
    assertClose(e.data, s.data, { mad: 1, max: 5 }, `${name} 8-bit`);
    assert.equal(diffStats(stored.data, e.data).mad > 3, expectShift, `${name}: ${expectShift ? 'moves' : 'keeps'} the colours`);
    const [s16, e16] = await Promise.all([
      sharp(tagged).toColourspace('rgb16').raw({ depth: 'ushort' }).toBuffer(),
      engine(tagged).toColourspace('rgb16').raw({ depth: 'ushort' }).toBuffer(),
    ]);
    const d = diffStats16(e16, s16);
    assert.ok(d.mad < 300, `${name} 16-bit: mad ${d.mad.toFixed(1)}`);
  }
});

test('metadata still reports the input profile', async () => {
  const tagged = embedIcc(await encoded('jpeg'), profiles['display-p3 (sRGB curve)']);
  const [s, e] = await Promise.all([sharp(tagged).metadata(), engine(tagged).metadata()]);
  assert.equal(e.hasProfile, true);
  assert.equal(e.hasProfile, s.hasProfile);
  assert.deepEqual(e.icc, s.icc);
});

// ---- CMYK ----

let cmykProfile;
const cmykJpeg = async (profile) => {
  const jpeg = await sharp(rgbRaw, sharpRaw(3)).toColourspace('cmyk').jpeg({ quality: 100 }).toBuffer();
  return profile ? embedIcc(jpeg, profile) : jpeg;
};

test('CMYK JPEG metadata matches sharp', async () => {
  const jpeg = await cmykJpeg();
  const [s, e] = await Promise.all([sharp(jpeg).metadata(), engine(jpeg).metadata()]);
  for (const key of ['format', 'width', 'height', 'space', 'channels', 'depth', 'hasProfile', 'hasAlpha', 'chromaSubsampling', 'isProgressive']) {
    assert.deepEqual(e[key], s[key], key);
  }
  assert.equal(e.space, 'cmyk');
  assert.equal(e.channels, 4);
});

test('CMYK JPEG (Adobe, inverted) converts to sRGB like sharp', async () => {
  const jpeg = await cmykJpeg();
  const [s, e] = await Promise.all([pixels(sharp, jpeg), pixels(engine, jpeg)]);
  assert.equal(e.info.channels, 3);
  // The engine also reports `space`, which sharp leaves out of raw output info.
      assert.deepEqual({ ...e.info, space: undefined }, { ...s.info, space: undefined });
  // Not the old behaviour: inverted or naively converted values are far off.
  assertClose(e.data, s.data, { mad: 1, max: 12 }, 'cmyk');
  // Round trip through CMYK stays near the original colours (inversion is right).
  assert.ok(diffStats(rgbRaw, e.data).mad < 20);
});

test('CMYK JPEG: resize, embedded CMYK profile, ignoreIcc, keepIccProfile', async () => {
  // The bundled CMYK profile of libvips, as exported by sharp, serves as the embedded profile.
  const probe = await sharp(rgbRaw, sharpRaw(3)).withIccProfile('cmyk').jpeg().toBuffer();
  cmykProfile = (await sharp(probe).metadata()).icc;
  assert.ok(cmykProfile && cmykProfile.length > 100000);
  const jpeg = await cmykJpeg();
  const tagged = await cmykJpeg(cmykProfile);
  const [plain, withProfile] = await Promise.all([pixels(sharp, jpeg), pixels(engine, tagged)]);
  assertClose(withProfile.data, plain.data, { mad: 1, max: 12 }, 'embedded profile');
  for (const [name, build, options] of [
    ['resize', (x) => x.resize(60), undefined],
    ['ignoreIcc', (x) => x, { ignoreIcc: true }],
    ['keepIccProfile', (x) => x.keepIccProfile(), undefined],
  ]) {
    const [s, e] = await Promise.all([pixels(sharp, tagged, build, options), pixels(engine, tagged, build, options)]);
    assertClose(e.data, s.data, { mad: name === 'resize' ? 1.5 : 1, max: 16 }, name);
  }
  const out = await engine(tagged).keepIccProfile().jpeg().toBuffer();
  assert.equal((await sharp(out).metadata()).hasProfile, true);
});
