// Minimal 16-bit PNG writer (big-endian samples, no filtering) for tests that need a real 16-bit source.
import zlib from 'node:zlib';
import { pngChunk, PNG_SIG } from './_util.mjs';

// ch: 1 (grey), 3 (rgb) or 4 (rgba); fn(x, y, channel) -> 0..65535
export function png16(w, h, ch, fn) {
  const colorType = { 1: 0, 3: 2, 4: 6 }[ch];
  const raw = Buffer.alloc((1 + w * ch * 2) * h);
  let o = 0;
  for (let y = 0; y < h; y++) {
    raw[o++] = 0;
    for (let x = 0; x < w; x++) for (let c = 0; c < ch; c++, o += 2) raw.writeUInt16BE(fn(x, y, c), o);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 16;
  ihdr[9] = colorType;
  return Buffer.concat([PNG_SIG, pngChunk('IHDR', ihdr), pngChunk('IDAT', zlib.deflateSync(raw)), pngChunk('IEND', Buffer.alloc(0))]);
}
