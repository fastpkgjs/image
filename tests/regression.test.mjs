import test from 'node:test';
import assert from 'node:assert/strict';
import image from '../packages/image/index.js';

// Gradient so DCT scaling and resampling see real data instead of a flat colour.
async function gradientJpeg(width, height, options = {}) {
  const data = Buffer.alloc(width * height * 3);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 3;
      data[i] = (x * 255 / width) | 0;
      data[i + 1] = (y * 255 / height) | 0;
      data[i + 2] = ((x + y) * 255 / (width + height)) | 0;
    }
  }
  return image(data, { raw: { width, height, channels: 3 } }).jpeg({ quality: 80, ...options }).toBuffer();
}

async function sizeOf(input, resize) {
  const { info } = await image(input).resize(resize).toBuffer({ resolveWithObject: true });
  return [info.width, info.height];
}

// Expected sizes are what sharp 0.35 produces for the same calls.
const INSIDE_CASES = [
  // source,           target box,        expected
  [[3000, 1500], [1200, 600], [1200, 600]],
  [[3000, 1500], [1200, 1200], [1200, 600]],
  [[4000, 2000], [1200, 600], [1200, 600]],
  [[4000, 2000], [1200, 1200], [1200, 600]],
  [[2000, 1000], [1200, 600], [1200, 600]],
  [[3001, 1501], [1200, 600], [1200, 600]],
  [[5000, 3333], [1200, 600], [900, 600]],
  [[5000, 3333], [1200, 1200], [1200, 800]],
  [[6000, 4000], [1200, 600], [900, 600]],
  [[6000, 4000], [1200, 1200], [1200, 800]],
  // smaller than the box: fit inside still enlarges unless withoutEnlargement is set
  [[1000, 500], [1200, 600], [1200, 600]],
  [[1000, 500], [1200, 1200], [1200, 600]],
];

for (const [[sw, sh], [tw, th], expected] of INSIDE_CASES) {
  test(`jpeg ${sw}x${sh} resize inside ${tw}x${th} -> ${expected.join('x')}`, async () => {
    const jpeg = await gradientJpeg(sw, sh);
    assert.deepEqual(await sizeOf(jpeg, { width: tw, height: th, fit: 'inside' }), expected);
    // PNG goes through no shrink-on-load, so it must agree with the JPEG result.
    const png = await image(jpeg).png().toBuffer();
    assert.deepEqual(await sizeOf(png, { width: tw, height: th, fit: 'inside' }), expected);
  });
}

test('jpeg width-only resize keeps the exact aspect ratio of the source', async () => {
  const jpeg = await gradientJpeg(3000, 1500);
  assert.deepEqual(await sizeOf(jpeg, { width: 1200 }), [1200, 600]);
  assert.deepEqual(await sizeOf(jpeg, { height: 300 }), [600, 300]);
  const odd = await gradientJpeg(5000, 3333);
  assert.deepEqual(await sizeOf(odd, { width: 1200 }), [1200, 800]);
});

test('jpeg withoutEnlargement never grows and never shrinks below the target', async () => {
  const jpeg = await gradientJpeg(1000, 500);
  assert.deepEqual(
    await sizeOf(jpeg, { width: 1200, height: 600, fit: 'inside', withoutEnlargement: true }),
    [1000, 500],
  );
  const big = await gradientJpeg(4000, 2000);
  assert.deepEqual(
    await sizeOf(big, { width: 1200, height: 600, fit: 'inside', withoutEnlargement: true }),
    [1200, 600],
  );
});

test('jpeg cover, fill and outside hit their exact target on large sources', async () => {
  const jpeg = await gradientJpeg(3000, 1500);
  assert.deepEqual(await sizeOf(jpeg, { width: 1200, height: 1200, fit: 'cover' }), [1200, 1200]);
  assert.deepEqual(await sizeOf(jpeg, { width: 1200, height: 1200, fit: 'fill' }), [1200, 1200]);
  assert.deepEqual(await sizeOf(jpeg, { width: 1200, height: 1200, fit: 'outside' }), [2400, 1200]);
  assert.deepEqual(await sizeOf(jpeg, { width: 300, height: 700, fit: 'cover' }), [300, 700]);
});

test('jpeg shrink-on-load matches the unshrunk result closely', async () => {
  const jpeg = await gradientJpeg(4000, 2000);
  const shrunk = await image(jpeg).resize({ width: 800 }).raw().toBuffer({ resolveWithObject: true });
  // Decoding to full size first (a rotate op blocks shrink-on-load) must give the same pixels ±tolerance.
  const full = await image(jpeg).rotate(0).resize({ width: 800 }).raw().toBuffer({ resolveWithObject: true });
  assert.equal(shrunk.info.width, full.info.width);
  assert.equal(shrunk.info.height, full.info.height);
  let max = 0;
  let total = 0;
  for (let i = 0; i < shrunk.data.length; i++) {
    const d = Math.abs(shrunk.data[i] - full.data[i]);
    max = Math.max(max, d);
    total += d;
  }
  assert.ok(total / shrunk.data.length < 2, `mean abs difference ${total / shrunk.data.length}`);
  assert.ok(max < 24, `max difference ${max}`);
});

test('fastShrinkOnLoad false keeps exact sizes too', async () => {
  const jpeg = await gradientJpeg(4000, 2000);
  for (const fastShrinkOnLoad of [true, false]) {
    assert.deepEqual(
      await sizeOf(jpeg, { width: 1200, height: 600, fit: 'inside', fastShrinkOnLoad }),
      [1200, 600],
    );
  }
});

test('jpeg resize after autoOrient uses the oriented dimensions', async () => {
  const jpeg = await gradientJpeg(3000, 1500);
  // Orientation 6 = rotate 90 CW: the oriented image is 1500x3000.
  const tagged = await image(jpeg).jpeg().withMetadata({ orientation: 6 }).toBuffer();
  const meta = await image(tagged).metadata();
  assert.equal(meta.orientation, 6);
  const { info } = await image(tagged)
    .rotate()
    .resize({ width: 300, height: 300, fit: 'inside' })
    .toBuffer({ resolveWithObject: true });
  assert.deepEqual([info.width, info.height], [150, 300]);
});

test('jpeg resize after another geometry op still uses the post-op dimensions', async () => {
  const jpeg = await gradientJpeg(3000, 1500);
  const { info } = await image(jpeg)
    .extract({ left: 0, top: 0, width: 1500, height: 1500 })
    .resize({ width: 300, height: 300, fit: 'inside' })
    .toBuffer({ resolveWithObject: true });
  assert.deepEqual([info.width, info.height], [300, 300]);
});

test('extractChannel yields one channel in raw, png and jpeg output', async () => {
  const rgb = Buffer.from([10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);
  const source = () => image(rgb, { raw: { width: 2, height: 2, channels: 3 } });
  for (const [channel, expected] of [[0, [10, 40, 70, 100]], [1, [20, 50, 80, 110]], [2, [30, 60, 90, 120]]]) {
    const out = await source().extractChannel(channel).raw().toBuffer({ resolveWithObject: true });
    assert.equal(out.info.channels, 1, `channel ${channel}`);
    assert.equal(out.info.width, 2);
    assert.equal(out.info.height, 2);
    assert.deepEqual([...out.data], expected);
  }
  const named = await source().extractChannel('green').raw().toBuffer({ resolveWithObject: true });
  assert.deepEqual([...named.data], [20, 50, 80, 110]);

  const png = await source().extractChannel(0).png().toBuffer({ resolveWithObject: true });
  assert.equal(png.info.channels, 1);
  assert.equal((await image(png.data).metadata()).channels, 1);
  const jpg = await source().extractChannel(0).jpeg().toBuffer({ resolveWithObject: true });
  assert.equal(jpg.info.channels, 1);
  assert.equal((await image(jpg.data).metadata()).channels, 1);
});

test('extractChannel alpha returns the alpha plane', async () => {
  const rgba = Buffer.from([1, 2, 3, 200, 4, 5, 6, 100, 7, 8, 9, 50, 10, 11, 12, 0]);
  const out = await image(rgba, { raw: { width: 2, height: 2, channels: 4 } })
    .extractChannel(3)
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 1);
  assert.deepEqual([...out.data], [200, 100, 50, 0]);
});

test('extractChannel survives a resize and feeds a one-channel mask consumer', async () => {
  const width = 64;
  const height = 48;
  const rgb = Buffer.alloc(width * height * 3);
  for (let i = 0; i < width * height; i++) {
    rgb[i * 3] = i % width < width / 2 ? 255 : 0;
    rgb[i * 3 + 1] = 7;
    rgb[i * 3 + 2] = 9;
  }
  const out = await image(rgb, { raw: { width, height, channels: 3 } })
    .extractChannel(0)
    .resize({ width: 32, height: 24, kernel: 'nearest' })
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 1);
  assert.equal(out.data.length, 32 * 24);
  // Left half bright, right half dark, indexed as a single channel.
  assert.equal(out.data[0], 255);
  assert.equal(out.data[31], 0);
  const lit = out.data.reduce((n, v) => n + (v > 127 ? 1 : 0), 0);
  assert.equal(lit, 16 * 24);
});

test('toColourspace b-w gives one channel in raw output', async () => {
  const out = await image({ create: { width: 4, height: 4, channels: 3, background: '#336699' } })
    .toColourspace('b-w')
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 1);
  assert.equal(out.data.length, 16);
});

test('greyscale keeps three identical channels in png output, like sharp', async () => {
  const out = await image({ create: { width: 4, height: 4, channels: 3, background: '#336699' } })
    .greyscale()
    .png()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 3);
});

test('extending a one-channel image keeps one channel with a grey background', async () => {
  const out = await image({ create: { width: 4, height: 4, channels: 3, background: '#336699' } })
    .extractChannel(0)
    .extend({ left: 2, background: '#ff0000' })
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 1);
  assert.equal(out.info.width, 6);
  assert.equal(out.data.length, 6 * 4);
});

test('a later op that introduces colour does not collapse to one channel', async () => {
  const gray = await image({ create: { width: 4, height: 4, channels: 3, background: '#336699' } })
    .extractChannel(0)
    .png()
    .toBuffer();
  const overlay = await image({ create: { width: 2, height: 2, channels: 3, background: '#ff0000' } })
    .png()
    .toBuffer();
  const out = await image(gray)
    .composite([{ input: overlay, left: 0, top: 0 }])
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.data.length, out.info.width * out.info.height * out.info.channels);
  // sharp's composite adds an alpha plane, so accept RGB or RGBA here.
  assert.ok(out.info.channels === 3 || out.info.channels === 4);
  assert.deepEqual([...out.data.subarray(0, 3)], [255, 0, 0]);
});

for (const [name, apply] of [
  ['resize', (i) => i.resize(300)],
  ['blur', (i) => i.blur(2)],
  ['extend', (i) => i.extend({ left: 3 })],
  ['extract', (i) => i.extract({ left: 0, top: 0, width: 50, height: 50 })],
  ['sharpen', (i) => i.sharpen()],
  ['greyscale', (i) => i.greyscale()],
  ['flip', (i) => i.flip()],
]) {
  test(`keepExif survives ${name}`, async () => {
    const source = await image('fixtures/photo.jpg').jpeg().withMetadata({ orientation: 6 }).toBuffer();
    const out = await apply(image(source).keepExif()).jpeg().toBuffer();
    const meta = await image(out).metadata();
    assert.equal(meta.orientation, 6);
    assert.ok(meta.exif && meta.exif.length > 0);
  });
}

test('arbitrary-angle rotate keeps the channel count of an opaque RGB source', async () => {
  const rgb = Buffer.alloc(16 * 16 * 3, 120);
  const out = await image(rgb, { raw: { width: 16, height: 16, channels: 3 } })
    .rotate(45)
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 3);
  assert.equal(out.data.length, out.info.width * out.info.height * 3);
  assert.deepEqual([...out.data.subarray(0, 3)], [0, 0, 0]);
});

test('rotate with a transparent background adds an alpha channel', async () => {
  const rgb = Buffer.alloc(16 * 16 * 3, 120);
  const out = await image(rgb, { raw: { width: 16, height: 16, channels: 3 } })
    .rotate(45, { background: { r: 0, g: 0, b: 0, alpha: 0 } })
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.equal(out.info.channels, 4);
  assert.equal(out.data[3], 0);
});

test('extend accepts per-edge padding and a single number', async () => {
  const src = () => image({ create: { width: 4, height: 4, channels: 3, background: '#336699' } });
  const edges = await src()
    .extend({ top: 1, bottom: 2, left: 3, right: 4, background: '#ff0000' })
    .raw()
    .toBuffer({ resolveWithObject: true });
  assert.deepEqual([edges.info.width, edges.info.height], [11, 7]);
  assert.deepEqual([...edges.data.subarray(0, 3)], [255, 0, 0]);
  const at = (x, y) => [...edges.data.subarray((y * 11 + x) * 3, (y * 11 + x) * 3 + 3)];
  assert.deepEqual(at(3, 1), [0x33, 0x66, 0x99]);
  assert.deepEqual(at(2, 1), [255, 0, 0]);
  const uniform = await src().extend(2).toBuffer({ resolveWithObject: true });
  assert.deepEqual([uniform.info.width, uniform.info.height], [8, 8]);
  const one = await src().extend({ left: 2 }).toBuffer({ resolveWithObject: true });
  assert.deepEqual([one.info.width, one.info.height], [6, 4]);
  assert.throws(() => src().extend({ top: -1 }), TypeError);
});
