// Deterministic 16384x16384 (256 MP) RGB PNG for the streaming-decode memory case in mem.mjs:
//
//   node bench/gen-huge-png.mjs [out.png]     # default bench/.cache/mem/huge-256mp.png
//
// Written row by row through zlib (no raw buffer is ever held), a blocky colour ramp that
// deflates to well under 1 MB, like the flat-colour/gradient files that trigger the problem.
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const out = process.argv[2] ?? path.join(here, '.cache', 'mem', 'huge-256mp.png');
const size = 16384;

fs.mkdirSync(path.dirname(out), { recursive: true });
const fd = fs.openSync(out, 'w');
const chunk = (type, data) => {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(data.length, 0);
  head.write(type, 4, 'latin1');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(zlib.crc32(data, zlib.crc32(head.subarray(4))), 0);
  fs.writeSync(fd, Buffer.concat([head, data, crc]));
};

fs.writeSync(fd, Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(size, 0);
ihdr.writeUInt32BE(size, 4);
ihdr.set([8, 2, 0, 0, 0], 8); // 8-bit RGB, deflate, adaptive filter, no interlace
chunk('IHDR', ihdr);

const deflate = zlib.createDeflate({ level: 9 });
const parts = [];
deflate.on('data', (d) => parts.push(d));
// Up filter (type 2): identical rows become runs of zero bytes, which deflate ~1000:1.
const row = Buffer.alloc(size * 3);
const prev = Buffer.alloc(size * 3);
const filtered = Buffer.alloc(1 + size * 3);
filtered[0] = 2;
for (let y = 0; y < size; y++) {
  for (let x = 0; x < size; x++) {
    row[x * 3] = (y >> 6) & 255;
    row[x * 3 + 1] = (x >> 6) & 255;
    row[x * 3 + 2] = 128;
  }
  for (let i = 0; i < row.length; i++) filtered[1 + i] = (row[i] - prev[i]) & 255;
  row.copy(prev);
  deflate.write(filtered);
  if ((y & 1023) === 1023) deflate.flush();
}
deflate.end();
await new Promise((resolve) => deflate.on('end', resolve));
chunk('IDAT', Buffer.concat(parts));
chunk('IEND', Buffer.alloc(0));
fs.closeSync(fd);
console.log(out, fs.statSync(out).size, 'bytes');
