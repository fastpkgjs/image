import { fileURLToPath } from 'node:url';
// Focused regression tests for the "fixes B" batch: API validation parity, libvips-exact
// filters, composite semantics, output/metadata parity and input handling. sharp (devDependency)
// is the oracle wherever a behaviour is defined by it.
import test from 'node:test';
import assert from 'node:assert/strict';
import { sharp, engine, make, pattern, compare, exec, diffStats } from './_util.mjs';

const small = await make('png', 32, 24);
const smallA = await make('pnga', 32, 24);
const photo = await sharp(fileURLToPath(new URL('../fixtures/photo.jpg', import.meta.url))).resize(160).png().toBuffer();

const sync = (lib, build) => {
  try {
    build(lib);
    return null;
  } catch (error) {
    return error;
  }
};

// ---------------------------------------------------------------------------------------------
// 1. API validation parity
// ---------------------------------------------------------------------------------------------
const invalidCalls = {
  'resize(0)': (l) => l(small).resize(0),
  'resize(-1)': (l) => l(small).resize(-1),
  'resize(1.5)': (l) => l(small).resize(1.5),
  'resize(10, 0)': (l) => l(small).resize(10, 0),
  'resize fit bogus': (l) => l(small).resize(5, 5, { fit: 'bogus' }),
  'resize kernel bogus': (l) => l(small).resize(5, 5, { kernel: 'bogus' }),
  'resize position bogus': (l) => l(small).resize(5, 5, { position: 'bogus' }),
  'resize background bogus': (l) => l(small).resize(5, 5, { background: 'notacolour' }),
  'extend(0)': (l) => l(small).extend(0),
  'extend left -1': (l) => l(small).extend({ left: -1 }),
  'extend extendWith bogus': (l) => l(small).extend({ top: 1, extendWith: 'bogus' }),
  'extract missing': (l) => l(small).extract({ left: 0, top: 0, width: 5 }),
  'extract float': (l) => l(small).extract({ left: 0.5, top: 0, width: 5, height: 5 }),
  'trim threshold -1': (l) => l(small).trim({ threshold: -1 }),
  'rotate("x")': (l) => l(small).rotate('x'),
  'blur(0.1)': (l) => l(small).blur(0.1),
  'blur(1001)': (l) => l(small).blur(1001),
  'blur precision': (l) => l(small).blur({ sigma: 2, precision: 'x' }),
  'sharpen sigma 0': (l) => l(small).sharpen({ sigma: 0 }),
  'sharpen m1 -1': (l) => l(small).sharpen({ sigma: 1, m1: -1 }),
  'median(0)': (l) => l(small).median(0),
  'gamma(5)': (l) => l(small).gamma(5),
  'gamma(0.5)': (l) => l(small).gamma(0.5),
  'gamma out 4': (l) => l(small).gamma(2, 4),
  'normalise lower 100': (l) => l(small).normalise({ lower: 100 }),
  'normalise lower>=upper': (l) => l(small).normalise({ lower: 60, upper: 40 }),
  'clahe missing': (l) => l(small).clahe(),
  'clahe slope 101': (l) => l(small).clahe({ width: 3, height: 3, maxSlope: 101 }),
  'convolve bad': (l) => l(small).convolve({ width: 3, height: 3, kernel: [1] }),
  'threshold(256)': (l) => l(small).threshold(256),
  'threshold(1.5)': (l) => l(small).threshold(1.5),
  'boolean op': (l) => l(small).boolean(small, 'nand'),
  'linear mismatch': (l) => l(small).linear([1, 2], [1]),
  'recomb 2x2': (l) => l(small).recomb([[1, 0], [0, 1]]),
  'modulate brightness -1': (l) => l(small).modulate({ brightness: -1 }),
  'modulate hue 1.5': (l) => l(small).modulate({ hue: 1.5 }),
  'modulate array': (l) => l(small).modulate([]),
  'tint bogus': (l) => l(small).tint('notacolour'),
  'ensureAlpha(2)': (l) => l(small).ensureAlpha(2),
  'extractChannel("x")': (l) => l(small).extractChannel('x'),
  'bandbool op': (l) => l(small).bandbool('nand'),
  'jpeg quality 0': (l) => l(small).jpeg({ quality: 0 }),
  'jpeg quality 101': (l) => l(small).jpeg({ quality: 101 }),
  'jpeg quality "80"': (l) => l(small).jpeg({ quality: '80' }),
  'jpeg chroma 4:1:1': (l) => l(small).jpeg({ chromaSubsampling: '4:1:1' }),
  'jpeg mozjpeg "yes"': (l) => l(small).jpeg({ mozjpeg: 'yes' }),
  'png compressionLevel 10': (l) => l(small).png({ compressionLevel: 10 }),
  'png colours 1': (l) => l(small).png({ palette: true, colours: 1 }),
  'png dither 2': (l) => l(small).png({ palette: true, dither: 2 }),
  'webp quality 0': (l) => l(small).webp({ quality: 0 }),
  'webp preset bogus': (l) => l(small).webp({ preset: 'bogus' }),
  'webp effort 7': (l) => l(small).webp({ effort: 7 }),
  'gif effort 11': (l) => l(small).gif({ effort: 11 }),
  'gif loop 70000': (l) => l(small).gif({ loop: 70000 }),
  'avif quality 0': (l) => l(small).avif({ quality: 0 }),
  'avif bitdepth 9': (l) => l(small).avif({ bitdepth: 9 }),
  'raw depth bogus': (l) => l(small).raw({ depth: 'bogus' }),
  'toFormat bogus': (l) => l(small).toFormat('bogus'),
  'toColourspace(5)': (l) => l(small).toColourspace(5),
  'withDensity(-1)': (l) => l(small).withDensity(-1),
  'withMetadata orientation 9': (l) => l(small).withMetadata({ orientation: 9 }),
  'withXmp("")': (l) => l(small).withXmp(''),
  'composite non-array': (l) => l(small).composite({ input: smallA }),
  'composite blend bogus': (l) => l(small).composite([{ input: smallA, blend: 'bogus' }]),
  'composite gravity bogus': (l) => l(small).composite([{ input: smallA, gravity: 'upwards' }]),
  'composite top without left': (l) => l(small).composite([{ input: smallA, top: 1 }]),
  'composite tile "yes"': (l) => l(small).composite([{ input: smallA, tile: 'yes' }]),
  'tile size 0': (l) => l(small).tile({ size: 0 }),
  'tile layout bogus': (l) => l(small).tile({ layout: 'bogus' }),
  'timeout bogus': (l) => l(small).timeout('x'),
  'constructor failOn bogus': (l) => l(small, { failOn: 'x' }),
  'constructor density 0': (l) => l(small, { density: 0 }),
  'constructor page -1': (l) => l(small, { page: -1 }),
  'constructor raw without channels': (l) => l(Buffer.alloc(12), { raw: { width: 2, height: 2 } }),
  'constructor create channels 2': (l) => l({ create: { width: 2, height: 2, channels: 2, background: '#fff' } }),
  'constructor create missing background': (l) => l({ create: { width: 2, height: 2, channels: 3 } }),
  'constructor text empty': (l) => l({ text: { text: '' } }),
  'constructor sharp(undefined)': (l) => l(undefined),
  'constructor number': (l) => l(123),
};

for (const [label, build] of Object.entries(invalidCalls)) {
  test(`validation: ${label} throws synchronously like sharp`, () => {
    const oracle = sync(sharp, build);
    assert.ok(oracle, 'oracle: sharp throws');
    const error = sync(engine, build);
    assert.ok(error instanceof Error, 'engine did not throw while chaining');
    assert.ok(!/^(InvalidArg|GenericFailure)/.test(error.message), error.message);
    assert.ok(error.message.length > 10);
  });
}

test('validation: messages follow sharp and name the option', () => {
  assert.throws(() => engine(small).jpeg({ quality: 0 }), /^TypeError: Expected integer between 1 and 100 for quality but received 0 of type number$/);
  assert.throws(() => engine(small).blur(0.1), /Expected number between 0\.3 and 1000 for sigma but received 0\.1/);
  assert.throws(() => engine(small).gamma(5), /Expected number between 1\.0 and 3\.0 for gamma but received 5/);
  assert.throws(() => engine(small).resize(0), /Expected positive integer between 1 and 100000000 for width but received 0/);
  assert.throws(() => engine(small).composite([{ input: smallA, blend: 'nope' }]), /Expected valid blend name for blend/);
});

test('validation: sharp-valid forms are accepted (and null behaves like undefined)', async () => {
  const valid = [
    (l) => l(small).resize(10, null, { fit: 'inside', position: 8 }),
    (l) => l(small).resize({ width: 10, kernel: null, fit: null }),
    (l) => l(small).blur(true),
    (l) => l(small).blur(false),
    (l) => l(small).sharpen(false),
    (l) => l(small).jpeg({ quality: 50, mozjpeg: true, optimizeScans: true, trellisQuantization: true }),
    (l) => l(small).png({ colors: 16, quality: null }),
    (l) => l(small).webp({ delay: null, loop: null }),
    (l) => l(small).toFormat('png', { compressionLevel: 3 }),
    (l) => l(small).linear(null, 5),
    (l) => l(small).negate(false),
    (l) => l(small).tint('#abc'),
    (l) => l(small).tint('hsl(120, 50%, 50%)'),
    (l) => l(small).tint('rgb(10 20 30 / 50%)'),
    (l) => l(small, { limitInputPixels: false, failOn: 'truncated', density: 150.5 }),
    (l) => l(small).composite([{ input: smallA, gravity: 3, blend: 'colour-dodge' }]),
  ];
  for (const build of valid) {
    const s = await build(sharp).toBuffer().then(() => 'ok', (e) => e.message);
    const e = await build(engine).toBuffer().then(() => 'ok', (err) => err.message);
    assert.equal(e, s === 'ok' ? 'ok' : e, `sharp: ${s}`);
    if (s === 'ok') assert.equal(e, 'ok', `engine rejected a valid form: ${e}`);
  }
});

test('colour strings follow the CSS grammar sharp accepts', async () => {
  for (const colour of ['red', 'RED', '#f00', '#ff0000', '#ff000080', 'rgb(255, 0, 0)', 'rgba(255,0,0,0.5)', 'hsl(0, 100%, 50%)', 'transparent', { r: 255, g: 0, b: 0 }, { r: 255, g: 0, b: 0, alpha: 0.25 }]) {
    const [s, e] = await Promise.all(
      [sharp, engine].map((lib) => lib({ create: { width: 2, height: 2, channels: 4, background: colour } }).raw().toBuffer())
    );
    assert.deepEqual([...e], [...s], JSON.stringify(colour));
  }
  for (const bad of ['notacolour', '', 'ab', 5, [], {}, { r: 'x' }]) {
    assert.ok(sync(sharp, (l) => l({ create: { width: 2, height: 2, channels: 4, background: bad } })), `oracle ${JSON.stringify(bad)}`);
    assert.ok(sync(engine, (l) => l({ create: { width: 2, height: 2, channels: 4, background: bad } })), `engine ${JSON.stringify(bad)}`);
  }
});

test('runtime errors drop the native status prefix and never leak a panic', async () => {
  for (const op of [
    () => engine(Buffer.from('definitely not an image')).toBuffer(),
    () => engine(Buffer.from('definitely not an image')).metadata(),
    () => engine(small).toColourspace('bogus').toBuffer(),
    () => engine(small).extractChannel(3).toBuffer(),
    () => engine(small).median(99).toBuffer(),
    () => engine(small).clahe({ width: 99, height: 99 }).toBuffer(),
    () => engine(small).extract({ left: 20, top: 0, width: 20, height: 5 }).toBuffer(),
  ]) {
    const error = await op().then(() => null, (e) => e);
    assert.ok(error instanceof Error);
    assert.ok(!/^(InvalidArg|GenericFailure)/.test(error.message), error.message);
    assert.ok(!/panicked/.test(error.message), error.message);
  }
  await assert.rejects(engine(small).extract({ left: 20, top: 0, width: 20, height: 5 }).toBuffer(), /extract_area: bad extract area/);
});

test('callbacks and streams also receive clean errors', async () => {
  const viaCallback = await new Promise((resolve) => engine(Buffer.from('nope')).toBuffer((err) => resolve(err)));
  assert.ok(!/^(InvalidArg|GenericFailure)/.test(viaCallback.message), viaCallback.message);
  const viaStream = await new Promise((resolve) => {
    const t = engine().png();
    t.on('error', resolve);
    t.end(Buffer.from('nope'));
    t.resume();
  });
  assert.ok(!/^(InvalidArg|GenericFailure)/.test(viaStream.message), viaStream.message);
});

test('sharp constant tables are exported', () => {
  assert.deepEqual(engine.gravity, sharp.gravity);
  assert.deepEqual(engine.strategy, sharp.strategy);
  assert.deepEqual(engine.position, sharp.position);
  assert.deepEqual(engine.kernel, sharp.kernel);
  assert.deepEqual(engine.fit, sharp.fit);
  assert.deepEqual(engine.blend, sharp.blend);
  assert.deepEqual(engine.bool, sharp.bool);
});

test('numeric gravity / position constants work for resize and composite', async () => {
  for (const [name, value] of Object.entries(sharp.gravity)) {
    const verdict = await compare((l) => l(photo).resize(40, 20, { fit: 'cover', position: value }), { mad: 6 });
    assert.equal(verdict, null, `${name}: ${verdict}`);
  }
});

// ---------------------------------------------------------------------------------------------
// 2. libvips-exact filters
// ---------------------------------------------------------------------------------------------
const sources = { photo, smallA, grey: await make('gpng', 48, 32) };

test('greyscale uses the libvips B_W conversion', async () => {
  for (const [name, buf] of Object.entries(sources)) {
    assert.equal(await compare((l) => l(buf).greyscale(), { mad: 0.6, max: 2 }), null, name);
    assert.equal(await compare((l) => l(buf).toColourspace('b-w'), { mad: 0.6, max: 2 }), null, name);
  }
});

test('threshold keeps the alpha band semantics of sharp (alpha is thresholded too) and 0 is a no-op', async () => {
  for (const [name, buf] of Object.entries(sources)) {
    for (const t of [0, 1, 64, 128, 200, 255]) {
      assert.equal(await compare((l) => l(buf).threshold(t), { mad: 0.5 }), null, `${name} threshold ${t}`);
      assert.equal(await compare((l) => l(buf).threshold(t, { greyscale: false }), { mad: 0.5 }), null, `${name} threshold ${t} per band`);
    }
  }
  const out = await engine(smallA).threshold(100).raw().toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 4);
});

test('gamma replicates the two-stage 8-bit quantisation of libvips', async () => {
  for (const [g, go] of [[2.2, undefined], [1.5, undefined], [3, undefined], [2, 3], [1, 3], [1.2, 2.7]]) {
    assert.equal(await compare((l) => l(photo).gamma(g, go), { mad: 0.2, max: 1 }), null, `gamma(${g}, ${go})`);
  }
  const identity = await engine(photo).gamma(3).raw().toBuffer();
  const original = await engine(photo).raw().toBuffer();
  assert.ok(diffStats(identity, original).mad > 3, 'gamma(3) is visibly lossy in sharp and therefore here');
});

test('linear leaves alpha alone unless every band has a coefficient', async () => {
  for (const build of [(l) => l(smallA).linear(1.5, 10), (l) => l(smallA).linear([1, 0.5, 2], [0, 10, 20]), (l) => l(smallA).linear([1, 2, 3, 0.5], [0, 0, 0, 5]), (l) => l(photo).linear(0.5, 3.7)]) {
    assert.equal(await compare(build, { mad: 0, max: 0 }), null);
  }
});

test('convolve premultiplies RGBA input like sharp', async () => {
  const kernels = [
    { width: 3, height: 3, kernel: [1, 1, 1, 1, 1, 1, 1, 1, 1] },
    { width: 3, height: 3, kernel: [-1, -1, -1, -1, 8, -1, -1, -1, -1] },
    { width: 3, height: 3, kernel: [0, 0, 0, 0, 1, 0, 0, 0, 0], scale: 1 },
    { width: 3, height: 3, kernel: [-1, 0, 1, -2, 0, 2, -1, 0, 1], scale: 1, offset: 128 },
  ];
  for (const k of kernels) assert.equal(await compare((l) => l(smallA).convolve({ ...k }), { mad: 0.05, max: 1 }), null, JSON.stringify(k.kernel));
});

test('modulate matches the LCh implementation', async () => {
  const cases = [{ brightness: 1.5 }, { brightness: 0.5, saturation: 0.3 }, { saturation: 2 }, { hue: 90 }, { hue: 180, lightness: 10 }, { hue: -60 }, { lightness: -20 }, { brightness: 2, saturation: 1.5, hue: 45 }];
  for (const options of cases) {
    for (const [name, buf] of Object.entries(sources)) {
      const verdict = await compare((l) => l(buf).modulate(options), { mad: 2.5 });
      assert.equal(verdict, null, `${name} ${JSON.stringify(options)}: ${verdict}`);
    }
  }
});

test('tint maps B_W luminance through the Lab tint curve', async () => {
  for (const tint of ['#ff0000', '#336699', { r: 255, g: 240, b: 16 }, '#808080', 'white']) {
    for (const [name, buf] of Object.entries(sources)) {
      const verdict = await compare((l) => l(buf).tint(tint), { mad: 2 });
      assert.equal(verdict, null, `${name} ${JSON.stringify(tint)}: ${verdict}`);
    }
  }
});

test('normalise stretches Lab lightness between libvips percentiles', async () => {
  for (const options of [undefined, { lower: 5, upper: 95 }, { lower: 0, upper: 100 }, { lower: 10, upper: 90 }]) {
    for (const [name, buf] of Object.entries(sources)) {
      const verdict = await compare((l) => l(buf).normalise(options), { mad: 1.5 });
      assert.equal(verdict, null, `${name} ${JSON.stringify(options)}: ${verdict}`);
    }
  }
});

test('clahe reproduces libvips hist_local exactly', async () => {
  for (const options of [{ width: 8, height: 8 }, { width: 3, height: 3, maxSlope: 0 }, { width: 10, height: 6, maxSlope: 5 }, { width: 16, height: 16, maxSlope: 1 }]) {
    for (const [name, buf] of Object.entries(sources)) {
      const verdict = await compare((l) => l(buf).clahe(options), { mad: 0.1, max: 1 });
      assert.equal(verdict, null, `${name} ${JSON.stringify(options)}: ${verdict}`);
    }
  }
});

test('trim follows find_trim: max-band threshold, uniform images are returned unchanged', async () => {
  const framed = await sharp({ create: { width: 40, height: 30, channels: 3, background: '#ffffff' } })
    .composite([{ input: await sharp({ create: { width: 12, height: 8, channels: 3, background: '#505050' } }).png().toBuffer(), left: 10, top: 6 }])
    .png()
    .toBuffer();
  for (const options of [undefined, { threshold: 0 }, { threshold: 10 }, { threshold: 200 }, { threshold: 255 }, { threshold: 300 }, { background: '#ffffff' }, { background: '#000000' }, { margin: 2 }, { lineArt: true }]) {
    const [s, e] = await Promise.all([sharp(framed).trim(options).toBuffer({ resolveWithObject: true }), engine(framed).trim(options).toBuffer({ resolveWithObject: true })]);
    assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], JSON.stringify(options));
  }
  const uniform = await sharp({ create: { width: 7, height: 5, channels: 3, background: 'red' } }).png().toBuffer();
  const out = await engine(uniform).trim().toBuffer({ resolveWithObject: true });
  assert.deepEqual([out.info.width, out.info.height], [7, 5]);
});

test('boolean pads the smaller operand with zeros; bandbool matches', async () => {
  const operand = await sharp({ create: { width: 10, height: 6, channels: 3, background: { r: 255, g: 15, b: 170 } } }).png().toBuffer();
  const base = await sharp({ create: { width: 14, height: 5, channels: 3, background: { r: 100, g: 200, b: 50 } } }).png().toBuffer();
  for (const op of ['and', 'or', 'eor']) {
    const [s, e] = await Promise.all([sharp(base).boolean(operand, op).raw().toBuffer({ resolveWithObject: true }), engine(base).boolean(operand, op).raw().toBuffer({ resolveWithObject: true })]);
    assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height]);
    assert.deepEqual([...e.data], [...s.data], op);
    assert.equal(await compare((l) => l(smallA).bandbool(op), { mad: 0, max: 0 }), null);
  }
});

// ---------------------------------------------------------------------------------------------
// 3. composite
// ---------------------------------------------------------------------------------------------
const overlay = await sharp(smallA).resize(12, 8, { fit: 'fill' }).negate({ alpha: false }).png().toBuffer();

test('every sharp blend mode matches libvips composite arithmetic', async () => {
  for (const blend of Object.keys(sharp.blend)) {
    for (const base of [small, smallA]) {
      const verdict = await compare((l) => l(base).composite([{ input: overlay, blend, left: 5, top: 4 }]), { mad: 1.2 });
      assert.equal(verdict, null, `${blend}: ${verdict}`);
    }
  }
});

test('Porter-Duff operators act on the whole canvas, not just the overlay rectangle', async () => {
  for (const blend of ['clear', 'source', 'in', 'out', 'dest-in', 'dest-atop']) {
    const out = await engine(small).composite([{ input: overlay, blend, left: 5, top: 4 }]).raw().toBuffer({ resolveWithObject: true });
    const corner = out.data.subarray(0, out.info.channels);
    const oracle = await sharp(small).composite([{ input: overlay, blend, left: 5, top: 4 }]).raw().toBuffer({ resolveWithObject: true });
    assert.deepEqual([...corner], [...oracle.data.subarray(0, oracle.info.channels)], blend);
  }
});

test('a second composite() call replaces the first', async () => {
  const verdict = await compare((l) => l(small).composite([{ input: overlay, top: 0, left: 0 }]).composite([{ input: overlay, top: 12, left: 14 }]), { mad: 1 });
  assert.equal(verdict, null, verdict);
});

test('tile honours gravity and (clamped) top/left like sharp', async () => {
  for (const options of [{}, { gravity: 'southeast' }, { gravity: 'north' }, { left: 0, top: 0 }, { left: 1, top: 2 }, { left: 3, top: 5 }, { left: 40, top: 30 }]) {
    const verdict = await compare((l) => l(small).composite([{ input: overlay, tile: true, ...options }]), { mad: 1 });
    assert.equal(verdict, null, `${JSON.stringify(options)}: ${verdict}`);
  }
});

test('premultiplied overlays, raw overlays and create/text overlays', async () => {
  const raw = Buffer.from(pattern(10, 6, 4, { alpha: true }));
  const cases = {
    premultiplied: (l) => l(small).composite([{ input: overlay, premultiplied: true }]),
    'raw overlay': (l) => l(small).composite([{ input: raw, raw: { width: 10, height: 6, channels: 4 }, top: 2, left: 3 }]),
    'raw premultiplied': (l) => l(small).composite([{ input: raw, raw: { width: 10, height: 6, channels: 4, premultiplied: true }, top: 2, left: 3 }]),
    'create overlay': (l) => l(small).composite([{ input: { create: { width: 9, height: 7, channels: 4, background: { r: 255, g: 0, b: 0, alpha: 0.5 } } }, gravity: 'southeast' }]),
    'negative offsets': (l) => l(small).composite([{ input: overlay, top: -3, left: -5 }]),
    'numeric gravity': (l) => l(small).composite([{ input: overlay, gravity: 7 }]),
  };
  for (const [name, build] of Object.entries(cases)) {
    const verdict = await compare(build, { mad: 2 });
    assert.equal(verdict, null, `${name}: ${verdict}`);
  }
  const text = await engine(small).composite([{ input: { text: { text: 'Hi', rgba: true } }, gravity: 'centre' }]).raw().toBuffer();
  const plain = await engine(small).raw().toBuffer();
  assert.ok(diffStats(text, plain).mad > 0, 'the text overlay changed pixels');
});

// ---------------------------------------------------------------------------------------------
// 4. output option parity
// ---------------------------------------------------------------------------------------------
const meta = (buf) => sharp(buf).metadata();

test('webp nearLossless writes lossless-coded output with quantised samples', async () => {
  const near = await engine(photo).webp({ nearLossless: true, quality: 60 }).toBuffer();
  const lossy = await engine(photo).webp({ quality: 60 }).toBuffer();
  assert.equal(near.subarray(12, 16).toString(), 'VP8L', 'near-lossless is VP8L');
  assert.equal(lossy.subarray(12, 16).toString(), 'VP8 ');
  const [a, b] = await Promise.all([sharp(near).raw().toBuffer(), sharp(photo).raw().toBuffer()]);
  const d = diffStats(a, b);
  assert.ok(d.max <= 4 && d.mad < 1.5, `near-lossless error mad=${d.mad} max=${d.max}`);
});

test('gif output keeps 1-bit transparency (alpha below 128 is transparent)', async () => {
  const gif = await engine(smallA).gif().toBuffer();
  const m = await meta(gif);
  assert.equal(m.hasAlpha, true);
  const opaque = await meta(await engine(small).gif().toBuffer());
  assert.equal(opaque.hasAlpha, false);
  const sharpAlpha = await sharp(await sharp(smallA).gif().toBuffer()).raw().toBuffer();
  const engineAlpha = await sharp(gif).raw().toBuffer();
  const count = (buf) => buf.filter((_, i) => i % 4 === 3 && buf[i] === 0).length;
  assert.equal(count(engineAlpha), count(sharpAlpha), 'same transparent pixel count');
});

test('png progressive writes Adam7 interlaced PNGs that decode identically', async () => {
  for (const [name, buf] of Object.entries({ photo, rgba: smallA, grey: sources.grey, tiny: await make('png', 3, 2) })) {
    const interlaced = await engine(buf).png({ progressive: true }).toBuffer();
    assert.equal(interlaced[28], 1, `${name}: IHDR interlace method`);
    const m = await meta(interlaced);
    assert.equal(m.isProgressive, true);
    const [a, b] = await Promise.all([sharp(interlaced).raw().toBuffer(), sharp(buf).raw().toBuffer()]);
    assert.deepEqual([...a], [...b], name);
    assert.equal((await engine(interlaced).metadata()).isProgressive, true);
  }
});

test('jpeg mozjpeg implies progressive output', async () => {
  for (const options of [{ mozjpeg: true }, { optimiseScans: true }, { progressive: true }]) {
    assert.equal((await meta(await engine(photo).jpeg(options).toBuffer())).isProgressive, true, JSON.stringify(options));
  }
  assert.equal((await meta(await engine(photo).jpeg().toBuffer())).isProgressive, false);
});

test('jpeg output flattens transparency onto black like libvips', async () => {
  const [a, b] = await Promise.all([sharp(await engine(smallA).jpeg({ quality: 95 }).toBuffer()).raw().toBuffer(), sharp(await sharp(smallA).jpeg({ quality: 95 }).toBuffer()).raw().toBuffer()]);
  assert.ok(diffStats(a, b).mad < 2.5);
});

test('extend with a translucent background adds an alpha channel', async () => {
  for (const background of [{ r: 255, g: 0, b: 0, alpha: 0.5 }, 'transparent', '#00000000']) {
    const [s, e] = await Promise.all([sharp(small).extend({ top: 3, left: 2, background }).raw().toBuffer({ resolveWithObject: true }), engine(small).extend({ top: 3, left: 2, background }).raw().toBuffer({ resolveWithObject: true })]);
    assert.equal(e.info.channels, s.info.channels);
    assert.deepEqual([...e.data], [...s.data]);
  }
  assert.equal((await engine(small).extend({ top: 3, background: '#ff0000' }).raw().toBuffer({ resolveWithObject: true })).info.channels, 3);
});

test('built-in ICC profiles for withIccProfile("srgb" | "p3")', async () => {
  for (const name of ['srgb', 'p3']) {
    for (const format of ['jpeg', 'png', 'webp']) {
      const out = await engine(small).withIccProfile(name)[format]().toBuffer();
      const m = await engine(out).metadata();
      assert.equal(m.hasProfile, true, `${name}/${format}`);
      assert.ok(Buffer.isBuffer(m.icc) && m.icc.length > 200);
      assert.equal(m.icc.subarray(36, 40).toString(), 'acsp');
      assert.equal(m.icc.readUInt32BE(0), m.icc.length);
      const sm = await meta(out);
      assert.equal(sm.hasProfile, true, 'sharp/libvips can read the generated profile');
    }
  }
});

test('png bitdepth:16 keeps 8-bit sources at 8 bits; heif av1 encodes avif', async () => {
  assert.equal((await meta(await engine(small).png({ bitdepth: 16 }).toBuffer())).depth, 'uchar');
  const avif = await engine(small).heif({ compression: 'av1', quality: 40, effort: 0 }).toBuffer();
  assert.equal((await engine(avif).metadata()).format, 'avif');
  assert.throws(() => engine(small).heif({ compression: 'hevc' }), /HEVC/);
});

test('opaque palette PNGs are written without alpha', async () => {
  const out = await meta(await engine(photo).png({ palette: true, colours: 32 }).toBuffer());
  assert.equal(out.channels, 3);
  assert.equal(out.hasAlpha, false);
});

// ---------------------------------------------------------------------------------------------
// 5. metadata parity
// ---------------------------------------------------------------------------------------------
test('metadata(): depth, density, chroma subsampling, progressive, alpha, 16-bit space', async () => {
  const jpeg = await sharp(photo).jpeg().toBuffer();
  const jpeg444 = await sharp(photo).jpeg({ chromaSubsampling: '4:4:4', progressive: true }).toBuffer();
  const png16 = await make('png16', 20, 10);
  const inputs = {
    jpeg,
    jpeg444,
    'jpeg density': await sharp(photo).withMetadata({ density: 300 }).jpeg().toBuffer(),
    'png density': await sharp(photo).withMetadata({ density: 144 }).png().toBuffer(),
    png: photo,
    png16,
    'png interlaced': await sharp(photo).png({ progressive: true }).toBuffer(),
    'webp alpha': await sharp(smallA).webp().toBuffer(),
    'webp lossless alpha': await sharp(smallA).webp({ lossless: true }).toBuffer(),
    webp: await sharp(photo).webp().toBuffer(),
    gif: await sharp(small).gif().toBuffer(),
    'gif alpha': await sharp(smallA).gif().toBuffer(),
    avif: await sharp(small).avif({ effort: 0 }).toBuffer(),
    'avif alpha': await sharp(smallA).avif({ effort: 0 }).toBuffer(),
  };
  for (const [name, buf] of Object.entries(inputs)) {
    const [s, e] = await Promise.all([sharp(buf).metadata(), engine(buf).metadata()]);
    for (const key of ['depth', 'density', 'chromaSubsampling', 'isProgressive', 'hasAlpha', 'channels', 'space', 'width', 'height']) {
      assert.equal(JSON.stringify(e[key]), JSON.stringify(s[key]), `${name}.${key}`);
    }
    assert.ok(!Object.values(e).includes(null), `${name}: no null fields`);
  }
});

test('metadata(): animated GIF / WebP report pages, loop and delay from the headers', async () => {
  const frames = await Promise.all([10, 80, 160].map((v) => sharp({ create: { width: 16, height: 12, channels: 3, background: { r: v, g: 20, b: 30 } } }).png().toBuffer()));
  for (const format of ['gif', 'webp']) {
    const animated = await sharp(frames, { join: { animated: true } })[format]({ delay: [100, 200, 300], loop: 2 }).toBuffer();
    const [s, e] = await Promise.all([sharp(animated).metadata(), engine(animated).metadata()]);
    assert.equal(e.pages, s.pages, `${format} pages`);
    assert.deepEqual(e.delay, s.delay, `${format} delay`);
    assert.equal(e.loop, s.loop, `${format} loop`);
  }
});

test('metadata(): PNG iCCP profiles are extracted', async () => {
  const withIcc = await sharp(small).withIccProfile('p3').png().toBuffer();
  const [s, e] = await Promise.all([sharp(withIcc).metadata(), engine(withIcc).metadata()]);
  assert.equal(e.hasProfile, s.hasProfile);
  assert.ok(Buffer.isBuffer(e.icc));
  assert.equal(e.icc.length, s.icc.length);
  assert.ok(e.icc.equals(s.icc));
});

// ---------------------------------------------------------------------------------------------
// 6. input handling
// ---------------------------------------------------------------------------------------------
test('raw input with two channels is grey + alpha', async () => {
  const data = Buffer.from([10, 255, 120, 128, 250, 0, 60, 255]);
  const [s, e] = await Promise.all([sharp(data, { raw: { width: 4, height: 1, channels: 2 } }).raw().toBuffer({ resolveWithObject: true }), engine(data, { raw: { width: 4, height: 1, channels: 2 } }).raw().toBuffer({ resolveWithObject: true })]);
  assert.equal(e.info.channels, s.info.channels);
  assert.deepEqual([...e.data], [...s.data]);
});

test('failOn: damaged JPEG / WebP are rejected unless failOn is none', async () => {
  const jpeg = await sharp(photo).jpeg().toBuffer();
  const webp = await sharp(photo).webp().toBuffer();
  const riff = Buffer.from(webp);
  riff.writeUInt32LE(0xfffffff0, 4);
  const damaged = {
    'jpeg truncated': jpeg.subarray(0, Math.floor(jpeg.length * 0.6)),
    'jpeg missing EOI': jpeg.subarray(0, jpeg.length - 2),
    'webp bad RIFF length': riff,
  };
  for (const [name, buf] of Object.entries(damaged)) {
    for (const failOn of [undefined, 'warning', 'error', 'truncated']) {
      const options = failOn ? { failOn } : {};
      const outcome = (lib) => lib(buf, options).raw().toBuffer().then(() => 'ok', () => 'error');
      assert.equal(await outcome(engine), await outcome(sharp), `${name} failOn=${failOn}`);
    }
    if (name.startsWith('webp')) {
      // libvips opens WebP through the demuxer, so a damaged container fails at every level.
      const outcome = (lib) => lib(buf, { failOn: 'none' }).raw().toBuffer().then(() => 'ok', () => 'error');
      assert.equal(await outcome(engine), await outcome(sharp), `${name} failOn=none`);
      continue;
    }
    const lenient = await engine(buf, { failOn: 'none' }).raw().toBuffer({ resolveWithObject: true });
    assert.ok(lenient.info.width > 0, `${name} failOn=none decodes`);
  }
});

test('failOn none returns the decodable part of truncated PNG and GIF files', async () => {
  const png = await sharp(photo).png().toBuffer();
  const gif = await sharp(photo).gif().toBuffer();
  for (const [name, buf] of Object.entries({ png: png.subarray(0, Math.floor(png.length * 0.6)), gif: gif.subarray(0, Math.floor(gif.length * 0.6)) })) {
    const [s, e] = await Promise.all([sharp(buf, { failOn: 'none' }).raw().toBuffer({ resolveWithObject: true }), engine(buf, { failOn: 'none' }).raw().toBuffer({ resolveWithObject: true })]);
    assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], name);
    await assert.rejects(engine(buf).raw().toBuffer(), Error, `${name} rejects by default`);
  }
});

test('SVG density rasterises at density / 72', async () => {
  const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#3366cc"/></svg>');
  for (const density of [undefined, 72, 144, 300, 36]) {
    const options = density ? { density } : {};
    const [s, e] = await Promise.all([sharp(svg, options).raw().toBuffer({ resolveWithObject: true }), engine(svg, options).raw().toBuffer({ resolveWithObject: true })]);
    assert.deepEqual([e.info.width, e.info.height], [s.info.width, s.info.height], `density ${density}`);
    assert.deepEqual([...(await engine(svg, options).metadata().then((m) => [m.width, m.height]))], [s.info.width, s.info.height]);
  }
});

test('stats() of 16-bit images use the 0..65535 range', async () => {
  const png16 = await make('png16', 40, 30);
  const [s, e] = await Promise.all([sharp(png16).stats(), engine(png16).stats()]);
  s.channels.forEach((channel, i) => {
    for (const key of ['min', 'max', 'mean', 'stdev']) {
      assert.ok(Math.abs(channel[key] - e.channels[i][key]) <= 400, `ch${i}.${key} sharp=${channel[key]} engine=${e.channels[i][key]}`);
    }
  });
  assert.ok(e.channels[0].max > 255);
});

test('create noise input is supported', async () => {
  const { data, info } = await engine({ create: { width: 64, height: 64, channels: 3, noise: { type: 'gaussian', mean: 128, sigma: 30 } } }).raw().toBuffer({ resolveWithObject: true });
  assert.equal(info.channels, 3);
  const mean = data.reduce((a, b) => a + b, 0) / data.length;
  const variance = data.reduce((a, b) => a + (b - mean) ** 2, 0) / data.length;
  assert.ok(Math.abs(mean - 128) < 3, `mean ${mean}`);
  assert.ok(Math.abs(Math.sqrt(variance) - 30) < 3, `stdev ${Math.sqrt(variance)}`);
});
