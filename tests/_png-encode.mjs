// Minimal in-memory PNG writer for the shrink tests: every colour type and bit depth, optional
// PLTE/tRNS, filters 0 and 2, Adam7 interlacing, and truncation. `sample(x, y, c)` returns an
// integer in 0..2^depth-1 (a palette index for type 3). Pure functions of (x, y, c), so the same
// picture can be written interlaced and not.
import zlib from 'node:zlib';

export const W = 192;
export const H = 128;

function crc(type, data) {
  return zlib.crc32(data, zlib.crc32(Buffer.from(type, 'latin1')));
}
export function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, 'latin1');
  data.copy(out, 8);
  out.writeUInt32BE(crc(type, data), 8 + data.length);
  return out;
}

const ADAM7 = [[0, 0, 8, 8], [4, 0, 8, 8], [0, 4, 4, 8], [2, 0, 4, 4], [0, 2, 2, 4], [1, 0, 2, 2], [0, 1, 1, 2]];

export function encode({ width, height, type, depth, sample, palette, trns, filter = 0, truncate, interlace = false }) {
  const channels = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 }[type];
  const rows = [];
  for (const [x0, y0, dx, dy] of interlace ? ADAM7 : [[0, 0, 1, 1]]) {
    const pw = Math.ceil((width - x0) / dx);
    const ph = Math.ceil((height - y0) / dy);
    if (pw <= 0 || ph <= 0) continue;
    const rowBytes = Math.ceil((pw * channels * depth) / 8);
    let prev = Buffer.alloc(rowBytes);
    for (let j = 0; j < ph; j++) {
      const y = y0 + j * dy;
      const row = Buffer.alloc(rowBytes);
      let bit = 0;
      for (let i = 0; i < pw; i++) {
        for (let c = 0; c < channels; c++) {
          const v = sample(x0 + i * dx, y, c);
          if (depth === 16) row.writeUInt16BE(v, bit >> 3);
          else if (depth === 8) row[bit >> 3] = v;
          else row[bit >> 3] |= v << (8 - depth - (bit & 7));
          bit += depth;
        }
      }
      const out = Buffer.alloc(1 + rowBytes);
      out[0] = filter;
      for (let i = 0; i < rowBytes; i++) out[1 + i] = filter === 2 ? (row[i] - prev[i]) & 255 : row[i];
      rows.push(out);
      prev = row;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.set([depth, type, 0, 0, interlace ? 1 : 0], 8);
  const parts = [Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr)];
  if (palette) parts.push(chunk('PLTE', Buffer.from(palette.flat())));
  if (trns) parts.push(chunk('tRNS', Buffer.from(trns)));
  parts.push(chunk('IDAT', zlib.deflateSync(Buffer.concat(rows))), chunk('IEND', Buffer.alloc(0)));
  const png = Buffer.concat(parts);
  return truncate ? png.subarray(0, Math.floor(png.length * truncate)) : png;
}

// Smooth value in 0..1 per channel, so box-shrink and direct resampling agree closely.
export const wave = (x, y, c, w = W, h = H) => 0.5 + 0.45 * Math.sin((x / w) * (2.2 + c) + (y / h) * (1.7 - c * 0.4) + c);
export const scaled = (depth) => (x, y, c) => Math.round(wave(x, y, c) * ((1 << depth) - 1));

export function palette(depth) {
  const n = 1 << depth;
  return Array.from({ length: n }, (_, i) => [Math.round((255 * i) / (n - 1)), Math.round(255 - (255 * i) / (n - 1)), (i * 53) & 255]);
}
// Index image: a smooth ramp, so neighbouring pixels have similar palette entries.
export const ramp = (depth) => (x, y) => Math.min((1 << depth) - 1, Math.floor(((x + y) / (W + H)) * (1 << depth)));

/** Every colour type / bit depth the decoder expands, as `encode` options (without size). */
export const cases = {
  'gray 1': { type: 0, depth: 1, sample: (x, y) => ((x >> 3) + (y >> 3)) & 1 },
  'gray 2': { type: 0, depth: 2, sample: scaled(2) },
  'gray 4': { type: 0, depth: 4, sample: scaled(4) },
  'gray 8': { type: 0, depth: 8, sample: scaled(8) },
  'gray 16': { type: 0, depth: 16, sample: scaled(16) },
  'gray 8 + tRNS': { type: 0, depth: 8, sample: scaled(8), trns: [0, 100] },
  'gray alpha 8': { type: 4, depth: 8, sample: (x, y, c) => (c ? Math.round(255 * (0.15 + 0.85 * (x / W))) : scaled(8)(x, y, 0)) },
  'gray alpha 16': { type: 4, depth: 16, sample: (x, y, c) => (c ? Math.round(65535 * (0.15 + 0.85 * (x / W))) : scaled(16)(x, y, 0)) },
  'rgb 8': { type: 2, depth: 8, sample: scaled(8) },
  'rgb 16': { type: 2, depth: 16, sample: scaled(16) },
  'rgb 8 + tRNS': { type: 2, depth: 8, sample: scaled(8), trns: [0, 10, 0, 20, 0, 30] },
  'rgba 8': { type: 6, depth: 8, sample: (x, y, c) => (c === 3 ? Math.round(255 * (0.1 + 0.9 * (y / H))) : scaled(8)(x, y, c)) },
  'rgba 16': { type: 6, depth: 16, sample: (x, y, c) => (c === 3 ? Math.round(65535 * (0.1 + 0.9 * (y / H))) : scaled(16)(x, y, c)) },
  // Fully transparent block with garbage colour under it: premultiplied averaging must ignore it.
  'rgba 8 transparent block': { type: 6, depth: 8, sample: (x, y, c) => (x < W / 2 && y < H / 2 ? (c === 3 ? 0 : 255 * (c === 0)) : c === 3 ? 255 : scaled(8)(x, y, c)) },
  'palette 1': { type: 3, depth: 1, sample: (x, y) => ((x >> 4) + (y >> 4)) & 1, palette: palette(1) },
  'palette 2': { type: 3, depth: 2, sample: ramp(2), palette: palette(2) },
  'palette 4': { type: 3, depth: 4, sample: ramp(4), palette: palette(4) },
  'palette 8': { type: 3, depth: 8, sample: ramp(8), palette: palette(8) },
  'palette 8 + tRNS': { type: 3, depth: 8, sample: ramp(8), palette: palette(8), trns: Array.from({ length: 256 }, (_, i) => 255 - (i >> 1)) },
};
