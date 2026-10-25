// Shared helpers for the differential and robustness suites.
// sharp (devDependency) is the oracle; `engine` is @fastpkg/image.
import { createRequire } from 'node:module';
import crypto from 'node:crypto';
import engine from '../packages/image/index.js';

const require = createRequire(import.meta.url);
export const sharp = require('sharp');
sharp.cache(false);
export { engine };

// Deterministic PRNG (mulberry32) so mutation tests are reproducible.
export function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const sha = (buf) => crypto.createHash('sha1').update(buf).digest('hex');

// Smooth, deterministic test pattern: gradients + low-frequency stripes + two hard-edged
// rectangles. Smooth content keeps resampling differences between libraries small.
export function pattern(width, height, channels = 3, { alpha = false } = {}) {
  const out = Buffer.alloc(width * height * channels);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const u = width > 1 ? x / (width - 1) : 0;
      const v = height > 1 ? y / (height - 1) : 0;
      let r = 40 + 170 * u;
      let g = 40 + 170 * v;
      let b = 128 + 90 * Math.sin((u + v) * 6.0);
      if (u > 0.15 && u < 0.4 && v > 0.2 && v < 0.5) [r, g, b] = [230, 40, 40];
      if (u > 0.6 && u < 0.85 && v > 0.55 && v < 0.9) [r, g, b] = [20, 30, 220];
      const o = (y * width + x) * channels;
      if (channels <= 2) {
        out[o] = Math.round(0.3 * r + 0.59 * g + 0.11 * b);
        if (channels === 2) out[o + 1] = alpha ? Math.round(255 * (0.2 + 0.8 * u)) : 255;
      } else {
        out[o] = Math.round(r);
        out[o + 1] = Math.round(g);
        out[o + 2] = Math.round(b);
        if (channels === 4) out[o + 3] = alpha ? Math.round(255 * (0.2 + 0.8 * u)) : 255;
      }
    }
  }
  return out;
}

const cache = new Map();
// Generated with sharp (the encoders are not under test here). Memoised.
export async function make(kind, width, height) {
  const key = `${kind}:${width}x${height}`;
  if (cache.has(key)) return cache.get(key);
  const ch = { jpeg: 3, png: 3, webp: 3, pnga: 4, gjpeg: 1, gpng: 1, gapng: 2, png16: 3 }[kind];
  const raw = pattern(width, height, ch, { alpha: kind === 'pnga' || kind === 'gapng' });
  let buf;
  if (kind === 'png16') {
    const wide = Buffer.alloc(raw.length * 2);
    for (let i = 0; i < raw.length; i++) wide.writeUInt16LE(raw[i] * 257, i * 2);
    buf = await sharp(wide, { raw: { width, height, channels: 3, depth: 'ushort' } }).toColourspace('rgb16').png().toBuffer();
  } else {
    const s = sharp(raw, { raw: { width, height, channels: ch } });
    if (kind === 'jpeg' || kind === 'gjpeg') buf = await s.jpeg({ quality: 92 }).toBuffer();
    else if (kind === 'webp') buf = await s.webp({ quality: 92 }).toBuffer();
    else buf = await s.png().toBuffer();
  }
  cache.set(key, buf);
  return buf;
}

// Run a pipeline builder against a library; never throws.
export async function exec(lib, build, { raw = true } = {}) {
  // raw: true -> 8-bit samples, 'ushort' -> 16-bit samples, false -> encoded output untouched
  let img;
  try {
    img = build(lib);
    if (raw) img = raw === 'ushort' ? img.raw({ depth: 'ushort' }) : img.raw();
    const { data, info } = await img.toBuffer({ resolveWithObject: true });
    return { data, info };
  } catch (error) {
    return { error };
  }
}

export function diffStats(a, b) {
  if (a.length !== b.length) return { mad: Infinity, max: Infinity };
  let sum = 0;
  let max = 0;
  for (let i = 0; i < a.length; i++) {
    const d = Math.abs(a[i] - b[i]);
    sum += d;
    if (d > max) max = d;
  }
  return { mad: a.length ? sum / a.length : 0, max };
}

// 16-bit aware variant (raw ushort buffers).
export function diffStats16(a, b) {
  if (a.length !== b.length) return { mad: Infinity, max: Infinity };
  let sum = 0;
  let max = 0;
  const n = a.length >> 1;
  for (let i = 0; i < n; i++) {
    const d = Math.abs(a.readUInt16LE(i * 2) - b.readUInt16LE(i * 2));
    sum += d;
    if (d > max) max = d;
  }
  return { mad: n ? sum / n : 0, max };
}

// Compare a pipeline between sharp and the engine. Returns null when equivalent, otherwise a
// short failure string. `tol` = { mad, max } (pixels), `dims:false` to skip dimension check.
export async function compare(build, tol = { mad: 4 }, opts = {}) {
  const [s, e] = await Promise.all([exec(sharp, build, opts), exec(engine, build, opts)]);
  if (s.error) {
    // Errors that come from libvips internals (not sharp's own argument validation) are
    // implementation details; the engine is free to succeed or fail on those.
    if (!isValidationError(s.error)) return null;
    if (!e.error) return `sharp rejects (${String(s.error.message).slice(0, 60)}) but engine succeeded`;
    return null;
  }
  if (e.error) return `engine rejects: ${String(e.error.message).slice(0, 90)}`;
  const si = s.info;
  const ei = e.info;
  if (si.width !== ei.width || si.height !== ei.height) {
    // opts.sizeSlack: sharp's JPEG shrink-on-load derives the target from the already
    // DCT-scaled size, so its output can be 1px off the mathematically exact size that the engine
    // (and PNG/WebP sources in sharp) produce. Accept +-slack there and skip the pixel check.
    if (opts.sizeSlack && Math.abs(si.width - ei.width) <= opts.sizeSlack && Math.abs(si.height - ei.height) <= opts.sizeSlack) return null;
    return `size sharp=${si.width}x${si.height} engine=${ei.width}x${ei.height}`;
  }
  if (si.channels !== ei.channels) return `channels sharp=${si.channels} engine=${ei.channels}`;
  const d = opts.raw === 'ushort' ? diffStats16(s.data, e.data) : diffStats(s.data, e.data);
  if (d.mad > (tol.mad ?? 0) || (tol.max !== undefined && d.max > tol.max)) {
    return `pixels mad=${d.mad.toFixed(2)} max=${d.max} (tol mad<=${tol.mad ?? 0}${tol.max !== undefined ? ` max<=${tol.max}` : ''})`;
  }
  return null;
}

// JPEG sources are decoded by different IDCT / chroma-upsampling code in each library, so even
// bit-exact geometric ops differ by a few levels there. Loosen "exact" tolerances accordingly.
export function jt(kind, tol) {
  if (!/jpeg$/.test(kind)) return tol;
  // Bitwise ops (boolean / bandbool) turn a 1-level decoder difference into a large per-pixel one
  // wherever a bit boundary is crossed, so only the mean can be bounded.
  if (tol.bitwise) return { mad: 4 };
  return { mad: Math.max(tol.mad ?? 0, 1.2), max: tol.max === undefined ? undefined : Math.max(tol.max, 10) };
}

// sharp validates arguments in JS and throws messages like "Expected integer between 1 and 100 ...".
export function isValidationError(error) {
  return /^(Expected|Invalid|Cannot extract|Cannot read properties|Unable|Missing|Provided|Unsupported|Input file is missing)/i.test(String(error && error.message));
}

// ---- corpus mutation (shared by the robustness suite and its child process) ----------------

// Deterministic mutations of `base`: 10 truncations (10%..95%) and `flips` random edits spread
// over header (first 64 bytes) and body.
export function fuzzInputs(base, seed, flips = 200) {
  const out = [];
  for (const pct of [10, 20, 30, 40, 50, 60, 70, 80, 90, 95]) {
    out.push({ id: `trunc${pct}`, buf: base.subarray(0, Math.max(1, Math.floor((base.length * pct) / 100))) });
  }
  const rnd = prng(seed);
  for (let i = 0; i < flips; i++) {
    const kind = ['flip', 'set', 'zero-run', 'insert', 'delete', 'flip'][Math.floor(rnd() * 6)];
    const header = rnd() < 0.5;
    const span = header ? Math.min(64, base.length) : base.length;
    const at = Math.floor(rnd() * span);
    const b = Buffer.from(base);
    let buf = b;
    let desc = `${kind}@${at}`;
    if (kind === 'flip' || kind === 'set') {
      const n = 1 + Math.floor(rnd() * 8);
      for (let k = 0; k < n && at + k < b.length; k++) b[at + k] = kind === 'flip' ? b[at + k] ^ (1 + Math.floor(rnd() * 255)) : rnd() < 0.5 ? 0 : 255;
      desc += `x${n}`;
    } else if (kind === 'zero-run') {
      const n = 4 + Math.floor(rnd() * 29);
      b.fill(0, at, Math.min(b.length, at + n));
      desc += `x${n}`;
    } else if (kind === 'insert') {
      const n = 1 + Math.floor(rnd() * 16);
      const ins = Buffer.alloc(n);
      for (let k = 0; k < n; k++) ins[k] = Math.floor(rnd() * 256);
      buf = Buffer.concat([b.subarray(0, at), ins, b.subarray(at)]);
      desc += `x${n}`;
    } else {
      const n = 1 + Math.floor(rnd() * 16);
      buf = Buffer.concat([b.subarray(0, at), b.subarray(Math.min(b.length, at + n))]);
      desc += `x${n}`;
    }
    out.push({ id: `mut${i}`, desc, buf });
  }
  return out;
}

// ---- hand-built malformed files -----------------------------------------------------------

let crcT;
export function crc32(buf) {
  if (!crcT) {
    crcT = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcT[n] = c >>> 0;
    }
  }
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = crcT[(c ^ buf[i]) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

export function pngChunk(type, data, { badCrc = false } = {}) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'latin1'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE((crc32(td) ^ (badCrc ? 0xdeadbeef : 0)) >>> 0);
  return Buffer.concat([len, td, crc]);
}

export const PNG_SIG = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

export function pngWith(width, height, { depth = 8, colorType = 2, idat, badIhdrCrc = false, badIdatCrc = false } = {}) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = depth;
  ihdr[9] = colorType;
  return Buffer.concat([PNG_SIG, pngChunk('IHDR', ihdr, { badCrc: badIhdrCrc }), pngChunk('IDAT', idat ?? Buffer.from([0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]), { badCrc: badIdatCrc }), pngChunk('IEND', Buffer.alloc(0))]);
}
