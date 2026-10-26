// Deterministic corrupt-file corpus + differential runner for `failOn`.
// sharp is the oracle: for every (file, failOn level) the reject-vs-accept verdict must agree and,
// where both accept, the image dimensions must match. Used by tests/failon.test.mjs (small
// subset) and runnable directly for the full sweep:
//   FAILON_FULL=1 node tests/_failon-corpus.mjs [perBase=N] [format=png]
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { sharp, engine, make, prng, crc32, sha } from './_util.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const fixtures = path.join(here, '..', 'fixtures');
export const LEVELS = ['none', 'truncated', 'error', 'warning'];

// ---------------------------------------------------------------------------------------------
// base files
// ---------------------------------------------------------------------------------------------
export async function baseFiles() {
  const small = await make('png', 32, 24);
  const smallA = await make('pnga', 32, 24);
  const raw = (channels) => ({ raw: { width: 32, height: 24, channels } });
  const px = (channels) => Buffer.from(Array.from({ length: 32 * 24 * channels }, (_, i) => (i * 7 + (i >> 5) * 13) & 255));
  const files = [];
  const add = (format, name, buf) => files.push({ format, name, buf });
  const readFixture = (name) => fs.readFileSync(path.join(fixtures, name));
  // fixtures (large ones are cut down to keep the decode cheap)
  add('jpeg', 'fixture photo.jpg', readFixture('photo.jpg'));
  add('webp', 'fixture photo.webp', readFixture('photo.webp'));
  add('png', 'fixture png16-rgba', readFixture('png16-rgba.png'));
  add('png', 'fixture png16-gray', readFixture('png16-gray.png'));
  add('png', 'fixture photo.png (small)', await sharp(readFixture('photo.png')).resize(64, 48).png().toBuffer());
  add('avif', 'fixture avif-base', readFixture('avif-base.avif'));
  // generated
  add('jpeg', 'jpeg q80', await sharp(small).jpeg({ quality: 80 }).toBuffer());
  add('jpeg', 'jpeg progressive', await sharp(small).jpeg({ progressive: true }).toBuffer());
  add('jpeg', 'jpeg gray', await sharp(px(1), raw(1)).jpeg().toBuffer());
  add('jpeg', 'jpeg restart', await sharp(small).resize(96, 72).jpeg({ quality: 70 }).toBuffer());
  add('png', 'png rgb', await sharp(small).png().toBuffer());
  add('png', 'png rgba', await sharp(smallA).png().toBuffer());
  add('png', 'png palette', await sharp(small).png({ palette: true, colours: 8 }).toBuffer());
  add('png', 'png interlaced', await sharp(small).png({ progressive: true }).toBuffer());
  add('png', 'png gray-alpha', await sharp(px(2), raw(2)).png().toBuffer());
  add('png', 'png with text+gamma', pngInject(await sharp(small).png().toBuffer()));
  add('webp', 'webp lossy', await sharp(small).webp().toBuffer());
  add('webp', 'webp lossless', await sharp(small).webp({ lossless: true }).toBuffer());
  add('webp', 'webp alpha', await sharp(smallA).webp().toBuffer());
  add('webp', 'webp animated', await sharp(Buffer.concat([px(3), px(3)]), { raw: { width: 32, height: 48, channels: 3, pageHeight: 24 } }).webp().toBuffer());
  add('gif', 'gif', await sharp(small).gif().toBuffer());
  add('gif', 'gif animated', await sharp(Buffer.concat([px(3), px(3)]), { raw: { width: 32, height: 48, channels: 3, pageHeight: 24 } }).gif().toBuffer());
  add('tiff', 'tiff', await sharp(small).tiff().toBuffer());
  add('avif', 'avif', await sharp(small).avif({ effort: 0, quality: 30 }).toBuffer());
  add('svg', 'svg', Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="32" height="24"><rect width="32" height="24" fill="#369"/><circle cx="16" cy="12" r="8" fill="#fa0"/></svg>'));
  return files;
}

// Adds ancillary chunks (gAMA, tEXt, tIME) ahead of IDAT so ancillary mutations have targets.
function pngInject(png) {
  const chunks = pngChunks(png);
  const extra = [pngChunk('gAMA', Buffer.from([0, 1, 0xb1, 0x8f])), pngChunk('tEXt', Buffer.from('Comment\0hello')), pngChunk('tIME', Buffer.from([7, 0xe9, 1, 2, 3, 4, 5]))];
  const first = chunks.findIndex((c) => c.type === 'IDAT');
  const out = [png.subarray(0, 8)];
  chunks.forEach((c, i) => {
    if (i === first) out.push(...extra);
    out.push(png.subarray(c.start, c.end));
  });
  return Buffer.concat(out);
}

function pngChunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, 'latin1');
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)) >>> 0, 8 + data.length);
  return out;
}

function pngChunks(buf) {
  const chunks = [];
  for (let p = 8; p + 12 <= buf.length; ) {
    const len = buf.readUInt32BE(p);
    const end = p + 12 + len;
    if (end > buf.length) break;
    chunks.push({ type: buf.toString('latin1', p + 4, p + 8), start: p, end, len });
    p = end;
  }
  return chunks;
}

// JPEG marker segments up to and including SOS; entropy data runs to the next real marker.
function jpegSegments(buf) {
  const segs = [];
  let p = 2;
  while (p + 4 <= buf.length && buf[p] === 0xff) {
    const marker = buf[p + 1];
    if (marker === 0xd9) break;
    const len = buf.readUInt16BE(p + 2);
    segs.push({ marker, start: p, end: p + 2 + len });
    p += 2 + len;
    if (marker === 0xda) {
      while (p + 1 < buf.length && !(buf[p] === 0xff && buf[p + 1] !== 0 && !(buf[p + 1] >= 0xd0 && buf[p + 1] <= 0xd7))) p++;
      segs.scanEnd = p;
    }
  }
  return segs;
}

// ---------------------------------------------------------------------------------------------
// mutations: each returns { id, buf }; `r` is a seeded PRNG so a (base, seed) pair is stable
// ---------------------------------------------------------------------------------------------
const pick = (r, n) => Math.floor(r() * n);
const noise = (r, n) => Buffer.from(Array.from({ length: n }, () => pick(r, 256)));
const cut = (buf, a, b) => Buffer.concat([buf.subarray(0, a), buf.subarray(b)]);
const splice = (buf, at, ins) => Buffer.concat([buf.subarray(0, at), ins, buf.subarray(at)]);

function genericMutations(base, r) {
  const b = base.buf;
  const n = b.length;
  const out = [];
  for (const f of [0.02, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.95, 0.98, 0.99]) out.push({ id: `trunc ${Math.floor(n * f)}`, buf: b.subarray(0, Math.max(1, Math.floor(n * f))) });
  for (const k of [1, 2, 3, 5, 8, 12, 20, 40]) if (n > k) out.push({ id: `trunc -${k}`, buf: b.subarray(0, n - k) });
  for (let i = 0; i < 6; i++) out.push({ id: `trunc rnd ${i}`, buf: b.subarray(0, 1 + pick(r, n - 1)) });
  for (let i = 0; i < 10; i++) {
    const m = Buffer.from(b);
    const count = 1 + pick(r, 3);
    const where = [];
    for (let j = 0; j < count; j++) {
      const at = pick(r, n);
      m[at] ^= 1 << pick(r, 8);
      where.push(at);
    }
    out.push({ id: `flip ${where.join(',')}`, buf: m });
  }
  for (let i = 0; i < 4; i++) {
    const m = Buffer.from(b);
    const at = 1 + pick(r, n - 1);
    m.fill(0, at, Math.min(n, at + 1 + pick(r, 24)));
    out.push({ id: `zero @${at}`, buf: m });
  }
  for (let i = 0; i < 3; i++) {
    const at = 1 + pick(r, n - 1);
    out.push({ id: `delete @${at}`, buf: cut(b, at, Math.min(n, at + 1 + pick(r, 16))) });
  }
  out.push({ id: 'trailing garbage 1', buf: Buffer.concat([b, Buffer.from([0x41])]) });
  out.push({ id: 'trailing garbage 16', buf: Buffer.concat([b, noise(r, 16)]) });
  out.push({ id: 'trailing garbage 1k', buf: Buffer.concat([b, noise(r, 1024)]) });
  out.push({ id: 'trailing zeros 64', buf: Buffer.concat([b, Buffer.alloc(64)]) });
  out.push({ id: 'insert garbage', buf: splice(b, 1 + pick(r, n - 1), noise(r, 1 + pick(r, 8))) });
  return out;
}

function pngMutations(base, r) {
  const b = base.buf;
  const chunks = pngChunks(b);
  const out = [];
  const crcAt = (c) => c.end - 4;
  chunks.forEach((c, i) => {
    const tag = `${c.type}#${i}`;
    const bad = Buffer.from(b);
    bad[crcAt(c)] ^= 0xff;
    out.push({ id: `crc ${tag}`, buf: bad });
    if (c.type !== 'IHDR' && c.type !== 'IEND') out.push({ id: `delete ${tag}`, buf: cut(b, c.start, c.end) });
    for (const [label, len] of [['len+1', c.len + 1], ['len-1', Math.max(0, c.len - 1)], ['len huge', 0x7fffffff], ['len 0', 0]]) {
      const m = Buffer.from(b);
      m.writeUInt32BE(len, c.start);
      out.push({ id: `${label} ${tag}`, buf: m });
    }
    out.push({ id: `duplicate ${tag}`, buf: splice(b, c.end, b.subarray(c.start, c.end)) });
    const flip = Buffer.from(b);
    if (c.len > 0) flip[c.start + 8 + pick(r, c.len)] ^= 1 << pick(r, 8);
    out.push({ id: `data flip ${tag}`, buf: flip });
    const t = Buffer.from(b);
    t[c.start + 4] ^= 0x20; // flips ancillary/critical case of the type
    out.push({ id: `type case ${tag}`, buf: t });
  });
  const iend = chunks.findIndex((c) => c.type === 'IEND');
  if (iend >= 0) {
    out.push({ id: 'drop IEND', buf: b.subarray(0, chunks[iend].start) });
    out.push({ id: 'garbage after IEND', buf: Buffer.concat([b, noise(r, 12)]) });
    out.push({ id: 'chunk after IEND', buf: Buffer.concat([b, pngChunk('tEXt', Buffer.from('a\0b'))]) });
    out.push({ id: 'IDAT after IEND', buf: Buffer.concat([b, pngChunk('IDAT', Buffer.from([0]))]) });
  }
  // IDAT payload damage re-wrapped in a valid CRC: isolates zlib/filter errors from CRC errors.
  const idats = chunks.filter((c) => c.type === 'IDAT');
  if (idats.length) {
    const data = Buffer.concat(idats.map((c) => b.subarray(c.start + 8, c.start + 8 + c.len)));
    const rebuild = (payload, label) => {
      const head = b.subarray(0, idats[0].start);
      const tail = b.subarray(idats[idats.length - 1].end);
      out.push({ id: label, buf: Buffer.concat([head, pngChunk('IDAT', payload), tail]) });
    };
    rebuild(data.subarray(0, Math.floor(data.length / 2)), 'IDAT half (valid crc)');
    rebuild(data.subarray(0, data.length - 4), 'IDAT -4 (valid crc)');
    rebuild(data.subarray(0, data.length - 1), 'IDAT -1 (valid crc)');
    rebuild(data.subarray(0, 2), 'IDAT 2 bytes (valid crc)');
    const f = Buffer.from(data);
    f[Math.min(f.length - 1, 2 + pick(r, Math.max(1, f.length - 2)))] ^= 0x10;
    rebuild(f, 'IDAT flip (valid crc)');
    const g = Buffer.from(data);
    g[g.length - 1] ^= 0xff; // adler
    rebuild(g, 'IDAT adler (valid crc)');
    rebuild(Buffer.concat([data, Buffer.from([1, 2, 3])]), 'IDAT trailing (valid crc)');
    const raw = zlib.inflateSync(data);
    rebuild(zlib.deflateSync(raw.subarray(0, raw.length - 3)), 'IDAT short raster (valid zlib)');
    rebuild(zlib.deflateSync(Buffer.concat([raw, Buffer.from([0, 0, 0])])), 'IDAT long raster (valid zlib)');
    const badFilter = Buffer.from(raw);
    badFilter[0] = 7;
    rebuild(zlib.deflateSync(badFilter), 'IDAT bad filter byte');
  }
  return out;
}

function jpegMutations(base, r) {
  const b = base.buf;
  const segs = jpegSegments(b);
  const out = [];
  segs.forEach((s, i) => {
    const tag = `${s.marker.toString(16)}#${i}`;
    if (s.marker !== 0xda) out.push({ id: `delete seg ${tag}`, buf: cut(b, s.start, s.end) });
    for (const [label, delta] of [['len+3', 3], ['len-1', -1], ['len huge', 0xfff0]]) {
      const m = Buffer.from(b);
      const len = label === 'len huge' ? delta : Math.max(0, m.readUInt16BE(s.start + 2) + delta);
      m.writeUInt16BE(len & 0xffff, s.start + 2);
      out.push({ id: `${label} ${tag}`, buf: m });
    }
    out.push({ id: `truncate in seg ${tag}`, buf: b.subarray(0, s.start + 4 + pick(r, Math.max(1, s.end - s.start - 4))) });
  });
  const eoi = b.lastIndexOf(Buffer.from([0xff, 0xd9]));
  if (eoi > 0) {
    out.push({ id: 'drop EOI', buf: b.subarray(0, eoi) });
    out.push({ id: 'garbage after EOI', buf: Buffer.concat([b, noise(r, 8)]) });
    out.push({ id: 'extra EOI', buf: Buffer.concat([b, Buffer.from([0xff, 0xd9])]) });
    out.push({ id: 'fill FF before EOI', buf: splice(b, eoi, Buffer.from([0xff, 0xff])) });
    out.push({ id: 'junk before EOI', buf: splice(b, eoi, noise(r, 3)) });
  }
  const scanStart = segs.length ? segs[segs.length - 1].end : 0;
  const scanEnd = segs.scanEnd ?? b.length;
  if (scanEnd - scanStart > 8) {
    for (let i = 0; i < 12; i++) {
      const m = Buffer.from(b);
      const at = scanStart + pick(r, scanEnd - scanStart);
      m[at] = pick(r, 256);
      out.push({ id: `entropy byte @${at}`, buf: m });
    }
    for (let i = 0; i < 4; i++) {
      const at = scanStart + pick(r, scanEnd - scanStart);
      out.push({ id: `entropy delete @${at}`, buf: cut(b, at, at + 1 + pick(r, 6)) });
    }
    out.push({ id: 'entropy stuffed marker', buf: splice(b, scanStart + pick(r, scanEnd - scanStart), Buffer.from([0xff, 0xc4])) });
    out.push({ id: 'entropy zero run', buf: (() => { const m = Buffer.from(b); m.fill(0, scanStart + 4, Math.min(scanEnd, scanStart + 40)); return m; })() });
    out.push({ id: 'entropy ones run', buf: (() => { const m = Buffer.from(b); m.fill(0xff, scanStart + 4, Math.min(scanEnd, scanStart + 12)); return m; })() });
  }
  out.push({ id: 'garbage before SOI', buf: Buffer.concat([noise(r, 4), b]) });
  return out;
}

function webpMutations(base, r) {
  const b = base.buf;
  const out = [];
  const size = b.readUInt32LE(4);
  for (const delta of [-1, -8, 1, 2, 100, 0x10000]) {
    const m = Buffer.from(b);
    m.writeUInt32LE((size + delta) >>> 0, 4);
    out.push({ id: `riff size ${delta >= 0 ? '+' : ''}${delta}`, buf: m });
  }
  // RIFF chunk walk
  for (let p = 12, i = 0; p + 8 <= b.length && i < 8; i++) {
    const len = b.readUInt32LE(p + 4);
    const type = b.toString('latin1', p, p + 4);
    for (const delta of [-1, 1, 1000]) {
      const m = Buffer.from(b);
      m.writeUInt32LE(Math.max(0, len + delta) >>> 0, p + 4);
      out.push({ id: `chunk ${type}#${i} len${delta > 0 ? '+' : ''}${delta}`, buf: m });
    }
    if (type !== 'VP8 ' && type !== 'VP8L' && type !== 'VP8X') out.push({ id: `delete chunk ${type}#${i}`, buf: cut(b, p, p + 8 + len + (len & 1)) });
    p += 8 + len + (len & 1);
  }
  out.push({ id: 'garbage after riff', buf: Buffer.concat([b, noise(r, 9)]) });
  out.push({ id: 'odd pad missing', buf: b.subarray(0, b.length - (b.length & 1 ? 1 : 0) - 1) });
  return out;
}

function gifMutations(base, r) {
  const b = base.buf;
  const out = [];
  const trailer = b.lastIndexOf(0x3b);
  if (trailer > 0) {
    out.push({ id: 'drop trailer', buf: b.subarray(0, trailer) });
    out.push({ id: 'garbage after trailer', buf: Buffer.concat([b, noise(r, 6)]) });
    out.push({ id: 'two trailers', buf: Buffer.concat([b, Buffer.from([0x3b])]) });
  }
  for (const at of [6, 7, 8, 9, 10, 11, 12]) {
    const m = Buffer.from(b);
    m[at] = pick(r, 256);
    out.push({ id: `header byte ${at}`, buf: m });
  }
  const img = b.indexOf(0x2c, 13);
  if (img > 0) {
    out.push({ id: 'drop image descriptor', buf: cut(b, img, img + 10) });
    out.push({ id: 'bad LZW min code', buf: (() => { const m = Buffer.from(b); const at = img + 10; if (at < m.length) m[at] = 13; return m; })() });
    out.push({ id: 'bad sub-block size', buf: (() => { const m = Buffer.from(b); const at = img + 11; if (at < m.length) m[at] = 255; return m; })() });
    out.push({ id: 'cut mid image data', buf: b.subarray(0, img + 14) });
  }
  return out;
}

function tiffMutations(base, r) {
  const b = base.buf;
  const out = [];
  for (let i = 0; i < 8; i++) {
    const m = Buffer.from(b);
    m[8 + pick(r, 40)] ^= 0xff;
    out.push({ id: `ifd byte ${i}`, buf: m });
  }
  return out;
}

export function mutationsFor(base, seed = 1) {
  let h = seed >>> 0;
  for (const ch of base.name) h = (Math.imul(h, 31) + ch.charCodeAt(0)) >>> 0;
  const r = prng(h);
  const specific = { png: pngMutations, jpeg: jpegMutations, webp: webpMutations, gif: gifMutations, tiff: tiffMutations }[base.format];
  const all = [{ id: 'pristine', buf: base.buf }, ...(specific ? specific(base, r) : []), ...genericMutations(base, r)];
  return all.map((m) => ({ ...m, base: base.name, format: base.format }));
}

// ---------------------------------------------------------------------------------------------
// differential runner
// ---------------------------------------------------------------------------------------------
export async function outcome(lib, buf, failOn) {
  try {
    const { data, info } = await lib(buf, { failOn }).raw().toBuffer({ resolveWithObject: true });
    return { ok: true, width: info.width, height: info.height, channels: info.channels, hash: sha(data) };
  } catch (error) {
    return { ok: false, message: String(error?.message ?? error).slice(0, 120) };
  }
}

// Verdict for one case: null when sharp and the engine agree, else a mismatch descriptor.
//   accept-vs-reject: we accept, sharp rejects (the dangerous one)
//   reject-vs-accept: we reject, sharp accepts
//   dims: both accept, different dimensions / channels
//   pixels: both accept, same shape, different bytes (informational)
export async function judge(buf, failOn) {
  const [s, e] = [await outcome(sharp, buf, failOn), await outcome(engine, buf, failOn)];
  if (s.ok && !e.ok) return { kind: 'reject-vs-accept', sharp: 'ok', engine: e.message };
  if (!s.ok && e.ok) return { kind: 'accept-vs-reject', sharp: s.message, engine: 'ok' };
  if (s.ok && e.ok) {
    if (s.width !== e.width || s.height !== e.height) return { kind: 'dims', sharp: `${s.width}x${s.height}`, engine: `${e.width}x${e.height}` };
    // Channel counts are left alone: opaque GIFs decode to RGBA here and RGB in sharp, which is
    // an output-shape quirk unrelated to failOn.
    if (s.channels === e.channels && s.hash !== e.hash) return { kind: 'pixels', sharp: s.hash.slice(0, 8), engine: e.hash.slice(0, 8) };
  }
  return null;
}

// Runs the corpus. `perBase` caps the mutations tried per base file (deterministic stride
// sampling so every mutation family stays represented); `budgetMs` stops early.
export async function runCorpus({ formats, levels = LEVELS, perBase = Infinity, seed = 1, budgetMs = Infinity, bases } = {}) {
  const started = Date.now();
  bases ??= await baseFiles();
  const mismatches = [];
  const totals = { cases: 0, byFormatLevel: {} };
  for (const base of bases) {
    if (formats && !formats.includes(base.format)) continue;
    // A format the engine cannot decode at all is not a failOn matter.
    const pristine = await outcome(engine, base.buf, 'warning');
    if (!pristine.ok) continue;
    let muts = mutationsFor(base, seed);
    if (muts.length > perBase) {
      const stride = muts.length / perBase;
      muts = Array.from({ length: perBase }, (_, i) => muts[Math.floor(i * stride)]);
    }
    for (const m of muts) {
      for (const failOn of levels) {
        if (Date.now() - started > budgetMs) return { mismatches, totals, truncated: true };
        totals.cases++;
        const key = `${base.format}/${failOn}`;
        totals.byFormatLevel[key] ??= { cases: 0 };
        totals.byFormatLevel[key].cases++;
        const verdict = await judge(m.buf, failOn);
        if (verdict) mismatches.push({ ...verdict, failOn, base: base.name, format: base.format, mutation: m.id, buf: m.buf });
      }
    }
  }
  return { mismatches, totals, truncated: false };
}

export function summarise({ mismatches, totals }) {
  const rows = {};
  for (const m of mismatches) {
    const key = `${m.format}/${m.failOn}`;
    rows[key] ??= {};
    rows[key][m.kind] = (rows[key][m.kind] ?? 0) + 1;
  }
  return { rows, total: totals.cases };
}

// CLI: node tests/_failon-corpus.mjs [perBase=N] [format=png,jpeg] [seed=N] [show=N]
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = Object.fromEntries(process.argv.slice(2).map((a) => a.split('=')));
  const result = await runCorpus({ formats: args.format?.split(','), perBase: args.perBase ? Number(args.perBase) : Infinity, seed: Number(args.seed ?? 1) });
  const { rows, total } = summarise(result);
  console.log(`cases: ${total}`);
  for (const [key, kinds] of Object.entries(rows).sort()) console.log(key.padEnd(16), JSON.stringify(kinds));
  const groups = new Map();
  for (const m of result.mismatches) {
    if (m.kind === 'pixels') continue;
    const g = `${m.kind} | ${m.format} | ${m.mutation.replace(/[#@]?\d+/g, '#')} | ${m.failOn}`;
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(m);
  }
  for (const [g, list] of [...groups].sort((a, b) => b[1].length - a[1].length).slice(0, Number(args.show ?? 60))) {
    console.log(`${String(list.length).padStart(4)}  ${g}   e.g. ${list[0].base}: ${list[0].mutation} sharp=${list[0].sharp} engine=${list[0].engine}`);
  }
  if (args.dump) {
    fs.mkdirSync(args.dump, { recursive: true });
    result.mismatches.filter((m) => m.kind !== 'pixels').slice(0, 200).forEach((m, i) => fs.writeFileSync(path.join(args.dump, `${i}-${m.format}-${m.failOn}-${m.kind}.bin`), m.buf));
  }
}
