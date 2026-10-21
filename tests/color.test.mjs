import test from 'node:test';
import assert from 'node:assert/strict';
import image, { Gradient } from '../packages/image/index.js';

const create = (color, width = 8, height = 8) =>
  image({ create: { width, height, channels: 4, background: color } });

const raw = (color, width = 8, height = 8) => create(color, width, height).raw().toBuffer();
const pixel = (buf, index = 0) => [buf[index], buf[index + 1], buf[index + 2], buf[index + 3]];

const IDENTITY_CUBE = `LUT_3D_SIZE 2
0 0 0
1 0 0
0 1 0
1 1 0
0 0 1
1 0 1
0 1 1
1 1 1
`;

test('replaceColor replaces a matched colour', async () => {
  const out = await create('#ff0000').replaceColor('#ff0000', '#00ff00').raw().toBuffer();
  assert.deepEqual(pixel(out), [0, 255, 0, 255]);
});

test('replaceColor preserves opacity when requested', async () => {
  const kept = await create('rgba(255,0,0,0.5)')
    .replaceColor('#ff0000', '#0000ff')
    .raw()
    .toBuffer();
  assert.equal(pixel(kept)[3], 128);

  const replaced = await create('rgba(255,0,0,0.5)')
    .replaceColor('#ff0000', '#0000ff', { preserveOpacity: false })
    .raw()
    .toBuffer();
  assert.equal(pixel(replaced)[3], 255);
});

test('recolor changes hue while preserving luminance', async () => {
  const out = await create('#808080').recolor('#0000ff').raw().toBuffer();
  const [r, g, b] = pixel(out);
  assert.ok(b > r);
  assert.ok(b > g);
});

test('duotone maps black and white to the endpoint colours', async () => {
  const black = await create('#000000').duotone('#0000ff', '#ffff00').raw().toBuffer();
  assert.deepEqual(pixel(black), [0, 0, 255, 255]);

  const white = await create('#ffffff').duotone('#0000ff', '#ffff00').raw().toBuffer();
  assert.deepEqual(pixel(white), [255, 255, 0, 255]);
});

test('gradientMap maps luminance through the gradient', async () => {
  const gradient = new Gradient().addStop(0, '#000000').addStop(1, '#ff0000');
  const black = await create('#000000').gradientMap(gradient).raw().toBuffer();
  const white = await create('#ffffff').gradientMap(gradient).raw().toBuffer();
  assert.deepEqual(pixel(black), [0, 0, 0, 255]);
  assert.deepEqual(pixel(white), [255, 0, 0, 255]);
});

test('gradientFill creates a spatial linear gradient', async () => {
  const gradient = new Gradient().linear(0).addStop(0, '#000000').addStop(1, '#ffffff');
  const out = await create('#808080', 8, 1).gradientFill(gradient).raw().toBuffer();
  const left = pixel(out, 0);
  const right = pixel(out, 28);
  assert.ok(left[0] < 20);
  assert.ok(right[0] > 235);
});

test('radial gradients use the configured centre and radius', async () => {
  const gradient = new Gradient().radial(0.5, 0.5, 0.5).addStop(0, '#ffffff').addStop(1, '#000000');
  const out = await create('#000000', 8, 8).gradientFill(gradient).raw().toBuffer();
  const center = pixel(out, (4 * 8 + 4) * 4);
  const corner = pixel(out, 0);
  assert.ok(center[0] > corner[0]);
});

test('gradientRecolor uses luminance and preserves opacity', async () => {
  const gradient = new Gradient().addStop(0, '#0000ff').addStop(1, '#ffff00');
  const out = await create('rgba(255,255,255,0.5)')
    .gradientRecolor(gradient)
    .raw()
    .toBuffer();
  assert.deepEqual(pixel(out), [255, 255, 0, 128]);
});

test('applyLut applies a cube LUT and strength', async () => {
  const unchanged = await create('#336699').applyLut(IDENTITY_CUBE, { strength: 0 }).raw().toBuffer();
  assert.deepEqual(pixel(unchanged), [51, 102, 153, 255]);

  const identity = await create('#336699').applyLut(IDENTITY_CUBE).raw().toBuffer();
  assert.deepEqual(pixel(identity), [51, 102, 153, 255]);
});

test('curves applies a control point curve', async () => {
  const out = await create('#808080')
    .curves({ rgb: [[0, 0], [0.5, 0.8], [1, 1]] })
    .raw()
    .toBuffer();
  assert.ok(pixel(out)[0] > 190);
});

test('levels remaps the input range', async () => {
  const out = await create('#808080')
    .levels({ inputBlack: 16, inputWhite: 240, gamma: 1.2 })
    .raw()
    .toBuffer();
  assert.ok(pixel(out)[0] > 128);
});

test('whiteBalance warms and cools the image', async () => {
  const warm = await create('#808080').whiteBalance({ temperature: 60 }).raw().toBuffer();
  const cool = await create('#808080').whiteBalance({ temperature: -60 }).raw().toBuffer();
  assert.ok(pixel(warm)[0] > pixel(warm)[2]);
  assert.ok(pixel(cool)[2] > pixel(cool)[0]);
});

test('colourBalance applies tonal range shifts', async () => {
  const out = await create('#808080')
    .colourBalance({ shadows: [20, 0, -20], midtones: [0, 20, 0], highlights: [-20, 0, 20] })
    .raw()
    .toBuffer();
  const [r, g, b] = pixel(out);
  assert.ok(g > r);
  assert.ok(g > b);
});

test('posterize quantizes colour levels', async () => {
  const out = await create('#7f7f7f').posterize(2).raw().toBuffer();
  assert.equal(pixel(out)[0], 0);
});

test('solarize inverts values above the threshold', async () => {
  const out = await create('#c8c8c8').solarize(128).raw().toBuffer();
  assert.equal(pixel(out)[0], 55);
});

test('fill preserves opacity when requested', async () => {
  const kept = await create('rgba(255,0,0,0.5)').fill('#0000ff').raw().toBuffer();
  assert.deepEqual(pixel(kept), [0, 0, 255, 128]);

  const replaced = await create('rgba(255,0,0,0.5)')
    .fill('#0000ff', { preserveOpacity: false })
    .raw()
    .toBuffer();
  assert.deepEqual(pixel(replaced), [0, 0, 255, 255]);
});

test('fill accepts a gradient', async () => {
  const gradient = new Gradient().linear(90).addStop(0, '#ff0000').addStop(1, '#0000ff');
  const out = await create('#808080', 1, 8).fill(gradient).raw().toBuffer();
  assert.ok(pixel(out, 0)[0] > pixel(out, 28)[0]);
});

test('paletteSwap maps to the nearest palette colour', async () => {
  const out = await create('#ff0000').paletteSwap(['#000000', '#ffffff']).raw().toBuffer();
  assert.deepEqual(pixel(out), [0, 0, 0, 255]);
});

test('alphaFromLuma writes alpha from luminance', async () => {
  const white = await create('#ffffff').alphaFromLuma().raw().toBuffer();
  const black = await create('#000000').alphaFromLuma().raw().toBuffer();
  assert.equal(pixel(white)[3], 255);
  assert.equal(pixel(black)[3], 0);
});

test('gradient builder is reusable and serializable', () => {
  const gradient = image
    .gradient()
    .linear(45)
    .spread('reflect')
    .addStop(1, '#ffffff')
    .addStop(0, '#000000');
  const spec = gradient.toJSON();
  assert.equal(spec.type, 'linear');
  assert.equal(spec.angle, 45);
  assert.equal(spec.stops[0].offset, 0);
  assert.equal(spec.stops[1].offset, 1);
  assert.ok(gradient.clone().toJSON().stops.length === 2);
});
