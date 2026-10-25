// Deterministic images and encode options shared by the PNG stability test and its child process.
import crypto from 'node:crypto';
import engine from '../packages/image/index.js';

function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Flat shapes (a handful of exact colours, with and without alpha) on a transparent ground. */
export function flat(width, height, channels) {
  const px = Buffer.alloc(width * height * channels);
  const r = rng(1);
  const shapes = Array.from({ length: 9 }, () => ({
    x: Math.floor(r() * width), y: Math.floor(r() * height),
    w: 10 + Math.floor(r() * width / 2), h: 10 + Math.floor(r() * height / 2),
    c: [Math.floor(r() * 256), Math.floor(r() * 256), Math.floor(r() * 256), r() < 0.5 ? 255 : 128],
  }));
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      let c = channels === 4 ? [0, 0, 0, 0] : [250, 250, 250];
      for (const s of shapes) if (x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h) c = channels === 4 ? s.c : s.c.slice(0, 3);
      px.set(c, (y * width + x) * channels);
    }
  }
  return { px, width, height, channels };
}

/** Smooth gradients with a little deterministic noise (photo-like, > 256 colours). */
export function smooth(width, height, channels, noise = 3) {
  const px = Buffer.alloc(width * height * channels);
  const r = rng(2);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const o = (y * width + x) * channels;
      const v = [(x * 255) / width, (y * 255) / height, (((x + y) * 255) / (width + height)) | 0, 255];
      for (let c = 0; c < channels; c++) px[o + c] = Math.max(0, Math.min(255, Math.round(v[c] + (r() - 0.5) * noise * 2)));
    }
  }
  return { px, width, height, channels };
}

/** A random tile repeated across the image with a few exact-colour overlays: LZ77 territory
 * (long matches, many colours), where neither adaptive filtering nor plain entropy coding helps. */
export function tiled(width, height, channels) {
  const px = Buffer.alloc(width * height * channels);
  const r = rng(5);
  const tile = Buffer.from(Array.from({ length: 37 * 23 * channels }, () => Math.floor(r() * 256)));
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const t = ((y % 23) * 37 + (x % 37)) * channels;
      for (let c = 0; c < channels; c++) px[(y * width + x) * channels + c] = x > width * 0.7 && y > height * 0.6 ? 200 : tile[t + c];
    }
  }
  return { px, width, height, channels };
}

export const images = {
  flatRgba: flat(160, 120, 4),
  flatRgb: flat(150, 90, 3),
  gradientRgb: smooth(200, 120, 3, 0),
  noisyRgb: smooth(128, 96, 3, 40),
  noisyRgba: smooth(96, 64, 4, 60),
  gray: smooth(100, 80, 1, 6),
  // Large enough that the filtered stream is deflated as several independent blocks.
  largeRgba: smooth(720, 640, 4, 20),
  // Several independently deflated blocks each, through the run-length / LZ77 paths.
  bigFlat: flat(1100, 700, 4),
  tiled: tiled(900, 600, 4),
};

const make = (name) => {
  const { px, width, height, channels } = images[name];
  const input = engine(px, { raw: { width, height, channels } });
  // One-channel raw input is sRGB for sharp too; b-w makes a true grey PNG.
  return channels === 1 ? input.toColourspace('b-w') : input;
};

export const cases = [];
for (const name of ['flatRgba', 'flatRgb', 'gradientRgb', 'noisyRgb', 'gray']) {
  for (const level of [0, 1, 6, 9]) cases.push({ id: `${name}/level${level}`, name, options: { compressionLevel: level } });
}
cases.push(
  { id: 'noisyRgba/default', name: 'noisyRgba', options: {} },
  { id: 'largeRgba/default', name: 'largeRgba', options: {} },
  { id: 'largeRgba/level1', name: 'largeRgba', options: { compressionLevel: 1 } },
  { id: 'largeRgba/level9', name: 'largeRgba', options: { compressionLevel: 9 } },
  { id: 'bigFlat/default', name: 'bigFlat', options: {} },
  { id: 'bigFlat/level1', name: 'bigFlat', options: { compressionLevel: 1 } },
  { id: 'tiled/default', name: 'tiled', options: {} },
  { id: 'tiled/level1', name: 'tiled', options: { compressionLevel: 1 } },
  { id: 'tiled/level3', name: 'tiled', options: { compressionLevel: 3 } },
  { id: 'tiled/level9', name: 'tiled', options: { compressionLevel: 9 } },
  { id: 'gradientRgb/adaptiveFiltering=false', name: 'gradientRgb', options: { adaptiveFiltering: false } },
  { id: 'flatRgb/adaptiveFiltering=true', name: 'flatRgb', options: { adaptiveFiltering: true } },
  { id: 'noisyRgb/progressive', name: 'noisyRgb', options: { progressive: true } },
  { id: 'flatRgba/palette', name: 'flatRgba', options: { palette: true } },
  { id: 'flatRgb/palette', name: 'flatRgb', options: { palette: true } },
  { id: 'noisyRgb/palette', name: 'noisyRgb', options: { palette: true } },
  { id: 'noisyRgba/palette', name: 'noisyRgba', options: { palette: true } },
  { id: 'noisyRgb/palette-colours16', name: 'noisyRgb', options: { palette: true, colours: 16 } },
  { id: 'noisyRgb/palette-dither0-level0', name: 'noisyRgb', options: { palette: true, dither: 0, compressionLevel: 0 } },
  { id: 'noisyRgb/palette-quality40-effort1', name: 'noisyRgb', options: { quality: 40, effort: 1 } },
  { id: 'flatRgba/palette-progressive', name: 'flatRgba', options: { palette: true, progressive: true } },
);

export async function encode(entry) {
  return make(entry.name).png(entry.options).toBuffer();
}

export const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest('hex');

export async function allHashes() {
  const out = {};
  for (const entry of cases) out[entry.id] = sha256(await encode(entry));
  // 16-bit output (8-bit source forced to 16-bit samples).
  out['noisyRgb/rgb16'] = sha256(await make('noisyRgb').toColourspace('rgb16').png().toBuffer());
  out['flatRgba/rgb16-level0'] = sha256(await make('flatRgba').toColourspace('rgb16').png({ compressionLevel: 0 }).toBuffer());
  return out;
}
