// Deterministic test pictures and a float reference resampler for judging downscale accuracy.
//
//   photo / detail / graphics: 8-bit RGB content generators (smooth photo-like, textured with
//   hard edges, and flat graphics with thin lines and text-like strokes).
//   referenceResize: separable Lanczos3 in f64 straight from the source pixels, with the support
//   scaled by the downscale ratio like libvips and fast_image_resize do: what a perfect
//   full-resolution resample gives, with no intermediate rounding.
//   psnr: against that reference.
export function rng(seed) {
  let s = seed >>> 0 || 1;
  return () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; };
}
// Smooth value noise, several octaves, plus pixel-level grain: "photo-like".
export function photo(w, h, seed = 1) {
  const r = rng(seed);
  const octs = [];
  for (let o = 0; o < 6; o++) {
    const n = 4 << o; // grid cells across
    const gx = n + 2, gy = Math.round((n * h) / w) + 2;
    const g = new Float64Array(gx * gy * 3).map(() => r());
    octs.push({ n, gx, gy, g, amp: 0.5 ** o });
  }
  const out = new Uint8Array(w * h * 3);
  const sm = (t) => t * t * (3 - 2 * t);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      for (let c = 0; c < 3; c++) {
        let v = 0, tot = 0;
        for (const o of octs) {
          const fx = (x / w) * o.n, fy = (y / w) * o.n;
          const ix = Math.floor(fx), iy = Math.floor(fy);
          const tx = sm(fx - ix), ty = sm(fy - iy);
          const at = (i, j) => o.g[((j * o.gx) + i) * 3 + c];
          const a = at(ix, iy) * (1 - tx) + at(ix + 1, iy) * tx;
          const b = at(ix, iy + 1) * (1 - tx) + at(ix + 1, iy + 1) * tx;
          v += o.amp * (a * (1 - ty) + b * ty);
          tot += o.amp;
        }
        v = v / tot;
        v = (v - 0.5) * 1.6 + 0.5 + (r() - 0.5) * 0.06; // contrast + sensor grain
        out[(y * w + x) * 3 + c] = Math.max(0, Math.min(255, Math.round(v * 255)));
      }
    }
  }
  return out;
}
// White page, black text-like strokes (1-2 px), diagonal hairlines, rings, flat colour blocks, checker.
export function graphics(w, h, seed = 2) {
  const r = rng(seed);
  const out = new Uint8Array(w * h * 3).fill(255);
  const set = (x, y, c) => { if (x >= 0 && y >= 0 && x < w && y < h) { const i = (y * w + x) * 3; out[i] = c[0]; out[i + 1] = c[1]; out[i + 2] = c[2]; } };
  const rect = (x0, y0, x1, y1, c) => { for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) set(x, y, c); };
  for (let i = 0; i < 14; i++) {
    const c = [Math.floor(r() * 256), Math.floor(r() * 256), Math.floor(r() * 256)];
    rect(Math.floor(r() * w * 0.8), Math.floor(r() * h * 0.8), 0, 0, c);
    const x0 = Math.floor(r() * w * 0.8), y0 = Math.floor(r() * h * 0.8);
    rect(x0, y0, x0 + Math.floor(r() * w * 0.15) + 8, y0 + Math.floor(r() * h * 0.15) + 8, c);
  }
  // "text": rows of tiny glyph-ish strokes
  for (let row = 0; row < h / 40; row++) {
    for (let col = 0; col < w / 14; col++) {
      if (r() < 0.25) continue;
      const x = col * 14 + 2, y = row * 40 + 8;
      const t = 1 + Math.floor(r() * 2);
      rect(x, y, x + t, y + 24, [0, 0, 0]);
      if (r() < 0.6) rect(x, y, x + 9, y + t, [0, 0, 0]);
      if (r() < 0.5) rect(x, y + 11, x + 9, y + 11 + t, [0, 0, 0]);
      if (r() < 0.4) rect(x + 8, y, x + 8 + t, y + 24, [0, 0, 0]);
    }
  }
  // hairlines at assorted angles
  for (let i = 0; i < 60; i++) {
    const x0 = r() * w, y0 = r() * h, a = r() * Math.PI, len = 100 + r() * 800;
    for (let s = 0; s < len; s += 0.5) set(Math.round(x0 + Math.cos(a) * s), Math.round(y0 + Math.sin(a) * s), [20, 20, 160]);
  }
  // rings
  for (let i = 0; i < 12; i++) {
    const cx = r() * w, cy = r() * h, rad = 20 + r() * 200;
    for (let t = 0; t < 6.2832; t += 0.5 / rad) set(Math.round(cx + Math.cos(t) * rad), Math.round(cy + Math.sin(t) * rad), [200, 30, 30]);
  }
  // checker patch
  for (let y = 0; y < Math.min(h, 400); y++) for (let x = 0; x < Math.min(w, 400); x++) if (((x >> 2) + (y >> 2)) & 1) set(x + (w >> 1), y + (h >> 2), [0, 0, 0]);
  return out;
}

// fast_image_resize / libvips style: support scales with the downscale ratio.
const lanczos3 = (x) => { x = Math.abs(x); if (x === 0) return 1; if (x >= 3) return 0; const p = Math.PI * x; return (3 * Math.sin(p) * Math.sin(p / 3)) / (p * p); };
export function lanczosWeights(src, dst) {
  const scale = src / dst, fs = Math.max(scale, 1), support = 3 * fs;
  const out = [];
  for (let i = 0; i < dst; i++) {
    const center = (i + 0.5) * scale;
    const lo = Math.max(0, Math.floor(center - support + 0.5));
    const hi = Math.min(src, Math.ceil(center + support + 0.5));
    const w = [];
    let sum = 0;
    for (let j = lo; j < hi; j++) { const v = lanczos3((j + 0.5 - center) / fs); w.push(v); sum += v; }
    out.push({ lo, w: w.map((v) => v / sum) });
  }
  return out;
}
// Exact area average of src pixels into dst cells (fractional edge weights).
export function areaWeights(src, dst) {
  const s = src / dst, out = [];
  for (let i = 0; i < dst; i++) {
    const a = i * s, b = Math.min(src, (i + 1) * s);
    const lo = Math.floor(a), hi = Math.min(src, Math.ceil(b));
    const w = [];
    for (let j = lo; j < hi; j++) w.push((Math.min(b, j + 1) - Math.max(a, j)) / (b - a));
    out.push({ lo, w });
  }
  return out;
}
// Integer-factor box as the old shrink did it (last block partial).
export function intBoxWeights(src, f) {
  const dst = Math.ceil(src / f), out = [];
  for (let i = 0; i < dst; i++) {
    const lo = i * f, hi = Math.min(src, lo + f);
    out.push({ lo, w: new Array(hi - lo).fill(1 / (hi - lo)) });
  }
  return out;
}
// Separable apply on Float64Array interleaved with `ch` channels.
export function apply(data, w, h, ch, wx, wy) {
  const ow = wx.length, tmp = new Float64Array(ow * h * ch);
  for (let y = 0; y < h; y++) for (let i = 0; i < ow; i++) {
    const { lo, w: ws } = wx[i];
    for (let c = 0; c < ch; c++) { let s = 0; for (let k = 0; k < ws.length; k++) s += ws[k] * data[(y * w + lo + k) * ch + c]; tmp[(y * ow + i) * ch + c] = s; }
  }
  const oh = wy.length, out = new Float64Array(ow * oh * ch);
  for (let j = 0; j < oh; j++) {
    const { lo, w: ws } = wy[j];
    for (let k = 0; k < ws.length; k++) { const wk = ws[k], row = (lo + k) * ow * ch, o = j * ow * ch; for (let i = 0; i < ow * ch; i++) out[o + i] += wk * tmp[row + i]; }
  }
  return { data: out, w: ow, h: oh };
}
export const toU8 = (f) => Uint8Array.from(f, (v) => Math.max(0, Math.min(255, Math.round(v))));
export function psnr(a, b) {
  let se = 0;
  for (let i = 0; i < a.length; i++) { const d = a[i] - b[i]; se += d * d; }
  const mse = se / a.length;
  return mse === 0 ? Infinity : 10 * Math.log10((255 * 255) / mse);
}

// Posterised photo-like content with strong grain: textured regions separated by hard edges.
export function detail(w, h, seed = 3) {
  const base = photo(w, h, seed), r = rng(seed + 9), out = new Uint8Array(base.length);
  for (let i = 0; i < base.length; i++) out[i] = Math.max(0, Math.min(255, (Math.floor(base[i] / 64) * 64 + 20 + Math.round((r() - 0.5) * 70))));
  return out;
}

/** f64 Lanczos3 reference of an interleaved 8-bit picture resized to tw x th. */
export function referenceResize(src, w, h, ch, tw, th) {
  return apply(Float64Array.from(src), w, h, ch, lanczosWeights(w, tw), lanczosWeights(h, th)).data;
}
