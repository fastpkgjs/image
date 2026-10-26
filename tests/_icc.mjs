// Builds small ICC profiles for tests: RGB matrix/TRC and grey TRC profiles, v2 layout.
// Colorants are Bradford-adapted to D50 as the ICC spec requires.

import zlib from 'node:zlib';

const D50 = [0.9642, 1.0, 0.8249];

const s15 = (v) => {
  const b = Buffer.alloc(4);
  b.writeInt32BE(Math.round(v * 65536));
  return b;
};
const sig = (s) => Buffer.from(s, 'latin1');
const u32 = (v) => {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(v >>> 0);
  return b;
};
const pad4 = (b) => (b.length % 4 ? Buffer.concat([b, Buffer.alloc(4 - (b.length % 4))]) : b);

const xyz = (v) => Buffer.concat([sig('XYZ '), u32(0), ...v.map(s15)]);
const text = (s) => Buffer.concat([sig('text'), u32(0), Buffer.from(`${s}\0`, 'latin1')]);
const desc = (s) =>
  Buffer.concat([
    sig('desc'),
    u32(0),
    u32(s.length + 1),
    Buffer.from(`${s}\0`, 'latin1'),
    u32(0),
    u32(0),
    Buffer.alloc(2 + 1 + 67),
  ]);

const mul = (m, v) => m.map((row) => row[0] * v[0] + row[1] * v[1] + row[2] * v[2]);
const matMul = (a, b) => a.map((row) => [0, 1, 2].map((j) => row[0] * b[0][j] + row[1] * b[1][j] + row[2] * b[2][j]));
function inv3(m) {
  const [[a, b, c], [d, e, f], [g, h, i]] = m;
  const det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
  return [
    [(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det],
    [(f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det],
    [(d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det],
  ];
}
const xy2xyz = ([x, y]) => [x / y, 1, (1 - x - y) / y];

// Bradford adaptation from `white` (XYZ) to D50.
function adapt(white) {
  const bfd = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
  ];
  const src = mul(bfd, white);
  const dst = mul(bfd, D50);
  const scale = [
    [dst[0] / src[0], 0, 0],
    [0, dst[1] / src[1], 0],
    [0, 0, dst[2] / src[2]],
  ];
  return matMul(inv3(bfd), matMul(scale, bfd));
}

export const PRIMARIES = {
  srgb: { r: [0.64, 0.33], g: [0.3, 0.6], b: [0.15, 0.06], white: [0.3127, 0.329] },
  p3: { r: [0.68, 0.32], g: [0.265, 0.69], b: [0.15, 0.06], white: [0.3127, 0.329] },
  // A deliberately wide gamut (ProPhoto-like primaries, D50 white).
  wide: { r: [0.7347, 0.2653], g: [0.1596, 0.8404], b: [0.0366, 0.0001], white: [0.3457, 0.3585] },
  adobe: { r: [0.64, 0.33], g: [0.21, 0.71], b: [0.15, 0.06], white: [0.3127, 0.329] },
};

function curve(trc) {
  if (trc === 'srgb') {
    // Parametric type 3: Y = (aX+b)^g for X >= d, cX otherwise.
    const params = [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045];
    return Buffer.concat([sig('para'), u32(0), Buffer.from([0, 3, 0, 0]), ...params.map(s15)]);
  }
  if (Array.isArray(trc)) {
    const b = Buffer.alloc(12 + trc.length * 2);
    b.write('curv', 0, 'latin1');
    b.writeUInt32BE(trc.length, 8);
    trc.forEach((v, i) => b.writeUInt16BE(Math.round(Math.min(1, Math.max(0, v)) * 65535), 12 + i * 2));
    return b;
  }
  const b = Buffer.alloc(14);
  b.write('curv', 0, 'latin1');
  b.writeUInt32BE(1, 8);
  b.writeUInt16BE(Math.round(trc * 256), 12);
  return b;
}

function assemble({ space, tags, cls = 'mntr', model = 'test' }) {
  const names = Object.keys(tags);
  const table = Buffer.alloc(4 + names.length * 12);
  table.writeUInt32BE(names.length, 0);
  let offset = 128 + table.length;
  const bodies = [];
  names.forEach((name, i) => {
    const body = pad4(tags[name]);
    table.write(name, 4 + i * 12, 'latin1');
    table.writeUInt32BE(offset, 8 + i * 12);
    table.writeUInt32BE(tags[name].length, 12 + i * 12);
    bodies.push(body);
    offset += body.length;
  });
  const header = Buffer.alloc(128);
  header.writeUInt32BE(offset, 0);
  header.writeUInt32BE(0x02400000, 8);
  header.write(cls, 12, 'latin1');
  header.write(space, 16, 'latin1');
  header.write('XYZ ', 20, 'latin1');
  header.writeUInt16BE(2024, 24);
  header.writeUInt16BE(1, 26);
  header.writeUInt16BE(1, 28);
  header.write('acsp', 36, 'latin1');
  header.write(model.slice(0, 4), 48, 'latin1');
  D50.forEach((v, i) => s15(v).copy(header, 68 + i * 4));
  return Buffer.concat([header, table, ...bodies]);
}

// RGB matrix/TRC display profile. `primaries` is a key of PRIMARIES or an object like them;
// `trc` is a gamma number, 'srgb', or an array of table entries in [0, 1].
export function makeRgbProfile(primaries = 'p3', trc = 'srgb', name = 'test rgb') {
  const p = typeof primaries === 'string' ? PRIMARIES[primaries] : primaries;
  const white = xy2xyz(p.white);
  const cols = [p.r, p.g, p.b].map(xy2xyz);
  const m = [0, 1, 2].map((row) => cols.map((c) => c[row]));
  const s = mul(inv3(m), white);
  const scaled = m.map((row) => row.map((v, j) => v * s[j]));
  const a = adapt(white);
  const adapted = matMul(a, scaled);
  const col = (j) => adapted.map((row) => row[j]);
  return assemble({
    space: 'RGB ',
    tags: {
      desc: desc(name),
      cprt: text('public domain'),
      wtpt: xyz(D50),
      rXYZ: xyz(col(0)),
      gXYZ: xyz(col(1)),
      bXYZ: xyz(col(2)),
      rTRC: curve(trc),
      gTRC: curve(trc),
      bTRC: curve(trc),
    },
  });
}

// Grey profile with a gamma (or table) tone curve.
export function makeGrayProfile(trc = 2.2, name = 'test grey') {
  return assemble({
    space: 'GRAY',
    tags: { desc: desc(name), cprt: text('public domain'), wtpt: xyz(D50), kTRC: curve(trc) },
  });
}

// ---- Attach a profile to an encoded image without touching its pixels ----

function embedJpeg(jpeg, icc) {
  const chunk = 65519;
  const count = Math.max(1, Math.ceil(icc.length / chunk));
  const segments = [];
  for (let i = 0; i < count; i++) {
    const data = icc.subarray(i * chunk, (i + 1) * chunk);
    const head = Buffer.alloc(4);
    head.writeUInt16BE(0xffe2, 0);
    head.writeUInt16BE(2 + 12 + 2 + data.length, 2);
    segments.push(head, Buffer.from('ICC_PROFILE\0', 'latin1'), Buffer.from([i + 1, count]), data);
  }
  return Buffer.concat([jpeg.subarray(0, 2), ...segments, jpeg.subarray(2)]);
}

function embedPng(png, icc) {
  const body = Buffer.concat([Buffer.from('icc\0\0', 'latin1'), zlib.deflateSync(icc)]);
  const head = Buffer.alloc(8);
  head.writeUInt32BE(body.length, 0);
  head.write('iCCP', 4, 'latin1');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(zlib.crc32(Buffer.concat([head.subarray(4), body])));
  // After IHDR (8 signature bytes + 25 for the chunk).
  return Buffer.concat([png.subarray(0, 33), head, body, crc, png.subarray(33)]);
}

function embedWebp(webp, icc, { width, height, alpha = false }) {
  const chunks = [];
  for (let o = 12; o + 8 <= webp.length; ) {
    const size = webp.readUInt32LE(o + 4);
    chunks.push({ type: webp.toString('latin1', o, o + 4), data: webp.subarray(o + 8, o + 8 + size) });
    o += 8 + size + (size & 1);
  }
  let vp8x = chunks.find((c) => c.type === 'VP8X')?.data;
  const rest = chunks.filter((c) => c.type !== 'VP8X' && c.type !== 'ICCP');
  if (vp8x) {
    vp8x = Buffer.from(vp8x);
  } else {
    vp8x = Buffer.alloc(10);
    vp8x.writeUIntLE(width - 1, 4, 3);
    vp8x.writeUIntLE(height - 1, 7, 3);
    if (alpha) vp8x[0] |= 0x10;
  }
  vp8x[0] |= 0x20;
  const out = [{ type: 'VP8X', data: vp8x }, { type: 'ICCP', data: icc }, ...rest];
  const parts = out.map(({ type, data }) => {
    const head = Buffer.alloc(8);
    head.write(type, 0, 'latin1');
    head.writeUInt32LE(data.length, 4);
    return Buffer.concat([head, data, Buffer.alloc(data.length & 1)]);
  });
  const riff = Buffer.alloc(12);
  riff.write('RIFF', 0, 'latin1');
  riff.write('WEBP', 8, 'latin1');
  riff.writeUInt32LE(4 + parts.reduce((n, p) => n + p.length, 0), 4);
  return Buffer.concat([riff, ...parts]);
}

// Attaches `icc` to a JPEG, PNG or WebP buffer (simple WebP files also need `width`/`height`).
export function embedIcc(buf, icc, opts = {}) {
  if (buf[0] === 0xff && buf[1] === 0xd8) return embedJpeg(buf, icc);
  if (buf.toString('latin1', 1, 4) === 'PNG') return embedPng(buf, icc);
  if (buf.toString('latin1', 0, 4) === 'RIFF') return embedWebp(buf, icc, opts);
  throw new Error('unsupported container');
}
