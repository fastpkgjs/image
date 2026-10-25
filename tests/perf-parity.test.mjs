import test from 'node:test';
import assert from 'node:assert/strict';
import image from '../packages/image/index.js';

// Deterministic RGBA noise so every pixel is distinguishable.
function rgba(width, height) {
  const data = Buffer.alloc(width * height * 4);
  let seed = 7;
  for (let i = 0; i < data.length; i++) {
    seed = (seed * 1103515245 + 12345) >>> 0;
    data[i] = (seed >>> 16) & 255;
  }
  return data;
}

const W = 37;
const H = 23;
const source = rgba(W, H);
const raw = { raw: { width: W, height: H, channels: 4 } };

async function run(pipeline) {
  const { data, info } = await pipeline.raw().toBuffer({ resolveWithObject: true });
  return { data, info };
}

function pixel(buf, width, x, y) {
  const i = (y * width + x) * 4;
  return [...buf.subarray(i, i + 4)];
}

test('flip, flop and 90/180/270 rotations move pixels exactly', async () => {
  const cases = {
    flip: [() => image(source, raw).flip(), W, H, (x, y) => [x, H - 1 - y]],
    flop: [() => image(source, raw).flop(), W, H, (x, y) => [W - 1 - x, y]],
    r180: [() => image(source, raw).rotate(180), W, H, (x, y) => [W - 1 - x, H - 1 - y]],
    // 90 degrees clockwise: output (x, y) comes from source (y, H - 1 - x).
    r90: [() => image(source, raw).rotate(90), H, W, (x, y) => [y, H - 1 - x]],
    r270: [() => image(source, raw).rotate(270), H, W, (x, y) => [W - 1 - y, x]],
  };
  for (const [name, [make, ow, oh, from]] of Object.entries(cases)) {
    const { data, info } = await run(make());
    assert.equal(info.width, ow, name);
    assert.equal(info.height, oh, name);
    for (let y = 0; y < oh; y++) {
      for (let x = 0; x < ow; x++) {
        const [sx, sy] = from(x, y);
        assert.deepEqual(pixel(data, ow, x, y), pixel(source, W, sx, sy), `${name} (${x},${y})`);
      }
    }
  }
});

test('large rotate90 spans several row bands', async () => {
  const w = 50;
  const h = 70;
  const big = rgba(w, h);
  const { data, info } = await run(image(big, { raw: { width: w, height: h, channels: 4 } }).rotate(90));
  assert.equal(info.width, h);
  assert.equal(info.height, w);
  for (let y = 0; y < w; y++) {
    for (let x = 0; x < h; x++) {
      assert.deepEqual(pixel(data, h, x, y), pixel(big, w, y, h - 1 - x));
    }
  }
});

test('arbitrary-angle rotate keeps the centre pixel and fills corners with the background', async () => {
  const flat = Buffer.alloc(40 * 40 * 4, 200);
  const { data, info } = await run(
    image(flat, { raw: { width: 40, height: 40, channels: 4 } }).rotate(45, { background: '#336699' }),
  );
  assert.ok(info.width > 40);
  assert.deepEqual(pixel(data, info.width, info.width >> 1, info.height >> 1), [200, 200, 200, 200]);
  assert.deepEqual(pixel(data, info.width, 0, 0), [0x33, 0x66, 0x99, 255]);
});

test('median matches a brute-force window sort', async () => {
  for (const size of [3, 5]) {
    const { data } = await run(image(source, raw).median(size));
    const r = size >> 1;
    for (const [x, y] of [[0, 0], [W - 1, H - 1], [5, 7], [W - 1, 0], [0, H - 1], [18, 11]]) {
      const expected = [];
      for (let c = 0; c < 4; c++) {
        const values = [];
        for (let dy = -r; dy <= r; dy++) {
          for (let dx = -r; dx <= r; dx++) {
            const sx = Math.min(W - 1, Math.max(0, x + dx));
            const sy = Math.min(H - 1, Math.max(0, y + dy));
            values.push(source[(sy * W + sx) * 4 + c]);
          }
        }
        values.sort((a, b) => a - b);
        expected.push(values[values.length >> 1]);
      }
      assert.deepEqual(pixel(data, W, x, y), expected, `median ${size} at (${x},${y})`);
    }
  }
});

test('extend modes replicate edge, mirror and background content', async () => {
  const pad = { top: 3, bottom: 5, left: 4, right: 2 };
  const out = (data, info) => (x, y) => pixel(data, info.width, x, y);
  const mod = (n, m) => ((n % m) + m) % m;
  const mirror = (n, len) => {
    const v = mod(n, len * 2);
    return v < len ? v : len * 2 - v - 1;
  };
  const modes = {
    copy: (n, len) => Math.min(len - 1, Math.max(0, n)),
    mirror,
    repeat: mod,
  };
  for (const [mode, map] of Object.entries(modes)) {
    const { data, info } = await run(image(source, raw).extend({ ...pad, extendWith: mode }));
    assert.equal(info.width, W + pad.left + pad.right);
    assert.equal(info.height, H + pad.top + pad.bottom);
    const at = out(data, info);
    for (let y = 0; y < info.height; y++) {
      for (let x = 0; x < info.width; x++) {
        assert.deepEqual(
          at(x, y),
          pixel(source, W, map(x - pad.left, W), map(y - pad.top, H)),
          `${mode} (${x},${y})`,
        );
      }
    }
  }
  const { data, info } = await run(image(source, raw).extend({ ...pad, background: '#102030' }));
  assert.deepEqual(pixel(data, info.width, 0, 0), [0x10, 0x20, 0x30, 255]);
  assert.deepEqual(pixel(data, info.width, pad.left, pad.top), pixel(source, W, 0, 0));
  assert.deepEqual(
    pixel(data, info.width, info.width - 1, info.height - 1),
    [0x10, 0x20, 0x30, 255],
  );
});

test('trim finds the bounding box of content in parallel', async () => {
  const w = 60;
  const h = 45;
  const data = Buffer.alloc(w * h * 4);
  for (let i = 0; i < w * h; i++) data.set([255, 255, 255, 255], i * 4);
  for (const [x, y] of [[10, 8], [41, 30], [25, 12]]) data.set([0, 0, 0, 255], (y * w + x) * 4);
  const { info } = await run(
    image(data, { raw: { width: w, height: h, channels: 4 } }).trim({ background: '#ffffff', lineArt: true }),
  );
  assert.equal(info.width, 41 - 10 + 1);
  assert.equal(info.height, 30 - 8 + 1);
});

test('extractChannel copies the selected band', async () => {
  for (const channel of [0, 1, 2]) {
    const { data, info } = await run(image(source, raw).extractChannel(channel));
    assert.equal(info.channels, 1);
    for (let i = 0; i < W * H; i++) assert.equal(data[i], source[i * 4 + channel]);
  }
});

test('PNG round trips keep every colour layout', async () => {
  const cases = [
    { channels: 1, w: 9, h: 5 },
    { channels: 3, w: 9, h: 5 },
    { channels: 4, w: 9, h: 5 },
    { channels: 3, w: 1, h: 1 },
    { channels: 4, w: 300, h: 2 },
  ];
  for (const { channels, w, h } of cases) {
    const pixels = Buffer.alloc(w * h * channels);
    for (let i = 0; i < pixels.length; i++) pixels[i] = (i * 37 + 11) & 255;
    const png = await image(pixels, { raw: { width: w, height: h, channels } }).png().toBuffer();
    const { data, info } = await image(png).raw().toBuffer({ resolveWithObject: true });
    assert.equal(info.width, w);
    assert.equal(info.height, h);
    const outChannels = info.channels;
    for (let p = 0; p < w * h; p++) {
      const src = pixels.subarray(p * channels, (p + 1) * channels);
      const dst = data.subarray(p * outChannels, (p + 1) * outChannels);
      if (channels === 1) {
        assert.deepEqual([dst[0], dst[1], dst[2]].every((v) => v === src[0]), true);
      } else {
        assert.deepEqual([...dst.subarray(0, 3)], [...src.subarray(0, 3)]);
        if (channels === 4) assert.equal(dst[3], src[3]);
      }
    }
  }
});

test('compressed PNG levels stay decodable and ordered by size', async () => {
  const w = 128;
  const h = 96;
  const pixels = Buffer.alloc(w * h * 3);
  for (let i = 0; i < pixels.length; i++) pixels[i] = ((i * 7) >> 3) & 255;
  const input = { raw: { width: w, height: h, channels: 3 } };
  const fast = await image(pixels, input).png({ compressionLevel: 6 }).toBuffer();
  const best = await image(pixels, input).png({ compressionLevel: 9 }).toBuffer();
  assert.ok(best.length <= fast.length * 1.02);
  const back = await image(best).raw().toBuffer();
  assert.deepEqual(back, pixels);
});

test('JPEG decodes at full size without a resize hint', async () => {
  const w = 64;
  const h = 48;
  const pixels = Buffer.alloc(w * h * 3);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) pixels.set([x * 4, y * 5, (x + y) * 2], (y * w + x) * 3);
  }
  const jpeg = await image(pixels, { raw: { width: w, height: h, channels: 3 } })
    .jpeg({ quality: 95 })
    .toBuffer();
  const { data, info } = await image(jpeg).raw().toBuffer({ resolveWithObject: true });
  assert.equal(info.width, w);
  assert.equal(info.height, h);
  assert.equal(info.channels, 3);
  let worst = 0;
  for (let i = 0; i < pixels.length; i++) worst = Math.max(worst, Math.abs(data[i] - pixels[i]));
  assert.ok(worst < 12, `max channel error ${worst}`);
});
