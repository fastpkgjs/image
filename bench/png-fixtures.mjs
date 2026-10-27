// Deterministic big-PNG writer shared by gen-huge-png.mjs and the memory benchmarks. The file is
// produced row by row through zlib (no raw frame is ever held) and every sample is a pure function
// of (x, y, channel), so the same picture can be written interlaced and non-interlaced.
//
//   await writePng(file, { width, height, depth: 8 | 16, color: 'gray' | 'graya' | 'rgb' | 'rgba',
//                          interlace: false, content: 'ramp' | 'noise', level: 9 })
//
// 'ramp' is a blocky colour ramp that deflates to a tiny file (the flat-colour/gradient uploads
// that trigger memory problems); 'noise' is incompressible per-sample noise (worst-case input).
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';

const TYPES = { gray: [0, 1], graya: [4, 2], rgb: [2, 3], rgba: [6, 4] };
const ADAM7 = [[0, 0, 8, 8], [4, 0, 8, 8], [0, 4, 4, 8], [2, 0, 4, 4], [0, 2, 2, 4], [1, 0, 2, 2], [0, 1, 1, 2]];

const hash = (x, y, c, seed) => {
  let h = Math.imul(x, 0x9e3779b1) ^ Math.imul(y, 0x85ebca6b) ^ Math.imul(c + 1, 0xc2b2ae35) ^ seed;
  h ^= h >>> 15;
  h = Math.imul(h, 0x2c1b3c6d);
  h ^= h >>> 12;
  h = Math.imul(h, 0x297a2d39);
  h ^= h >>> 15;
  return h >>> 0;
};

/** Sample value (0..2^depth-1) of the picture at (x, y), channel c. */
export function sampleOf({ depth, content, channels, seed = 1 }) {
  const max = depth === 16 ? 65535 : 255;
  const alpha = channels === 2 || channels === 4;
  return (x, y, c) => {
    if (alpha && c === channels - 1) return (((x + y) >> 5) & 255) * (depth === 16 ? 257 : 1);
    if (content === 'noise') return hash(x, y, c, seed) & max;
    if (depth === 16) return c === 0 ? (y << 2) & 65535 : c === 1 ? (x << 2) & 65535 : 32768;
    return c === 0 ? (y >> 6) & 255 : c === 1 ? (x >> 6) & 255 : 128;
  };
}

export async function writePng(file, opts) {
  const { width, height, depth = 8, color = 'rgb', interlace = false, content = 'ramp', level = 9, seed = 1 } = opts;
  const [type, channels] = TYPES[color];
  const sample = sampleOf({ depth, content, channels, seed });
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const fd = fs.openSync(file, 'w');
  const chunk = (name, data) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length, 0);
    head.write(name, 4, 'latin1');
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(zlib.crc32(data, zlib.crc32(head.subarray(4))), 0);
    fs.writeSync(fd, Buffer.concat([head, data, crc]));
  };
  fs.writeSync(fd, Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.set([depth, type, 0, 0, interlace ? 1 : 0], 8);
  chunk('IHDR', ihdr);

  const deflate = zlib.createDeflate({ level });
  let pending = [];
  let pendingSize = 0;
  deflate.on('data', (d) => {
    pending.push(d);
    pendingSize += d.length;
  });
  // IDAT chunks are cut from the compressed stream as it is produced.
  const drain = (force) => {
    if (pendingSize >= 1 << 20 || (force && pendingSize)) {
      chunk('IDAT', Buffer.concat(pending));
      pending = [];
      pendingSize = 0;
    }
  };
  const write = (buf) => new Promise((resolve) => (deflate.write(buf) ? resolve() : deflate.once('drain', resolve)));
  const bytesPer = depth / 8;
  for (const [x0, y0, dx, dy] of interlace ? ADAM7 : [[0, 0, 1, 1]]) {
    const pw = Math.ceil((width - x0) / dx);
    const ph = Math.ceil((height - y0) / dy);
    if (pw <= 0 || ph <= 0) continue;
    const rowBytes = pw * channels * bytesPer;
    let prev = Buffer.alloc(rowBytes);
    let row = Buffer.alloc(rowBytes);
    const out = Buffer.alloc(1 + rowBytes);
    out[0] = 2; // Up filter: repeated rows become runs of zeros
    for (let j = 0; j < ph; j++) {
      const y = y0 + j * dy;
      let o = 0;
      for (let i = 0; i < pw; i++) {
        const x = x0 + i * dx;
        for (let c = 0; c < channels; c++) {
          const v = sample(x, y, c);
          if (depth === 16) {
            row[o++] = v >> 8;
            row[o++] = v & 255;
          } else row[o++] = v;
        }
      }
      for (let i = 0; i < rowBytes; i++) out[1 + i] = (row[i] - prev[i]) & 255;
      [prev, row] = [row, prev];
      await write(Buffer.from(out));
      drain(false);
      if ((j & 1023) === 1023) deflate.flush();
    }
  }
  deflate.end();
  await new Promise((resolve) => deflate.on('end', resolve));
  drain(true);
  chunk('IEND', Buffer.alloc(0));
  fs.closeSync(fd);
  return fs.statSync(file).size;
}
