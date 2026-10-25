//! Palette quantiser for indexed PNG output (MIT-licensed, no libimagequant).
//!
//! Pipeline: exact-colour shortcut (images with at most `max_colors` distinct colours are
//! mapped losslessly) -> premultiplied-RGBA histogram -> variance-based median cut -> a few
//! k-means refinement passes -> nearest-colour mapping with Floyd-Steinberg error diffusion.
//! Alpha is quantised together with colour (premultiplied space), fully transparent pixels all
//! share one entry. Everything is a pure function of its inputs: float maths is in fixed order
//! and parallel work is split on fixed row bands, so output does not depend on thread count.

#![allow(clippy::needless_range_loop)]

use rayon::prelude::*;

pub struct Quantized {
    /// RGB triples, entries with alpha < 255 first.
    pub palette: Vec<u8>,
    /// tRNS payload (trimmed to the last non-opaque entry); empty when fully opaque.
    pub trns: Vec<u8>,
    pub indices: Vec<u8>,
    pub colors: usize,
}

pub struct Settings {
    pub max_colors: usize,
    /// 0..=100; 100 uses as many colours as allowed.
    pub quality: u8,
    /// 1..=10, more refinement passes.
    pub effort: u8,
    /// 0.0..=1.0 error-diffusion strength.
    pub dither: f32,
}

type Px = [f32; 4];

#[inline]
fn dist(a: &Px, b: &Px) -> f32 {
    let d0 = a[0] - b[0];
    let d1 = a[1] - b[1];
    let d2 = a[2] - b[2];
    let d3 = a[3] - b[3];
    d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
}

#[inline]
fn premultiply(p: &[u8]) -> Px {
    let a = p[3] as f32;
    let k = a / 255.0;
    [p[0] as f32 * k, p[1] as f32 * k, p[2] as f32 * k, a]
}

// ---------------------------------------------------------------------------------------------
// Nearest colour search (small kd-tree)
// ---------------------------------------------------------------------------------------------

struct KdNode {
    /// Palette index of the point stored at this node.
    point: u16,
    axis: u8,
    left: u16,
    right: u16,
}

const NIL: u16 = u16::MAX;

struct KdTree {
    nodes: Vec<KdNode>,
    colors: Vec<Px>,
    root: u16,
}

impl KdTree {
    fn new(colors: &[Px]) -> Self {
        let mut tree = KdTree {
            nodes: Vec::with_capacity(colors.len()),
            colors: colors.to_vec(),
            root: NIL,
        };
        let mut ids: Vec<u16> = (0..colors.len() as u16).collect();
        tree.root = tree.build(&mut ids);
        tree
    }

    fn build(&mut self, ids: &mut [u16]) -> u16 {
        if ids.is_empty() {
            return NIL;
        }
        let mut lo = [f32::MAX; 4];
        let mut hi = [f32::MIN; 4];
        for &i in ids.iter() {
            for c in 0..4 {
                lo[c] = lo[c].min(self.colors[i as usize][c]);
                hi[c] = hi[c].max(self.colors[i as usize][c]);
            }
        }
        let mut axis = 0;
        for c in 1..4 {
            if hi[c] - lo[c] > hi[axis] - lo[axis] {
                axis = c;
            }
        }
        let colors = &self.colors;
        ids.sort_unstable_by(|&a, &b| {
            colors[a as usize][axis]
                .total_cmp(&colors[b as usize][axis])
                .then(a.cmp(&b))
        });
        let mid = ids.len() / 2;
        let point = ids[mid];
        let (left_ids, rest) = ids.split_at_mut(mid);
        let left = self.build(left_ids);
        let right = self.build(&mut rest[1..]);
        self.nodes.push(KdNode {
            point,
            axis: axis as u8,
            left,
            right,
        });
        (self.nodes.len() - 1) as u16
    }

    #[inline]
    fn nearest(&self, q: &Px) -> (usize, f32) {
        let mut best = (f32::MAX, u16::MAX);
        self.search(self.root, q, &mut best);
        (best.1 as usize, best.0)
    }

    fn search(&self, node: u16, q: &Px, best: &mut (f32, u16)) {
        if node == NIL {
            return;
        }
        let n = &self.nodes[node as usize];
        let p = &self.colors[n.point as usize];
        let d = dist(p, q);
        if d < best.0 || (d == best.0 && n.point < best.1) {
            *best = (d, n.point);
        }
        let diff = q[n.axis as usize] - p[n.axis as usize];
        let (near, far) = if diff < 0.0 {
            (n.left, n.right)
        } else {
            (n.right, n.left)
        };
        self.search(near, q, best);
        if diff * diff <= best.0 {
            self.search(far, q, best);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Exact colours
// ---------------------------------------------------------------------------------------------

#[inline]
fn norm_key(p: &[u8]) -> u32 {
    if p[3] == 0 {
        0
    } else {
        u32::from_le_bytes([p[0], p[1], p[2], p[3]])
    }
}

/// Distinct (normalised) colours with their counts if there are at most `limit` of them.
fn exact_colors(rgba: &[u8], limit: usize) -> Option<Vec<(u32, u64)>> {
    const SLOTS: usize = 1024;
    let mut keys = [0u32; SLOTS];
    let mut used = [false; SLOTS];
    let mut counts = [0u64; SLOTS];
    let mut found: Vec<usize> = Vec::new();
    let mut last_key = 0u32;
    let mut last_slot = usize::MAX;
    for p in rgba.chunks_exact(4) {
        let key = norm_key(p);
        if last_slot != usize::MAX && key == last_key {
            counts[last_slot] += 1;
            continue;
        }
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> 22) as usize;
        loop {
            if !used[slot] {
                if found.len() >= limit {
                    return None;
                }
                used[slot] = true;
                keys[slot] = key;
                found.push(slot);
                break;
            }
            if keys[slot] == key {
                break;
            }
            slot = (slot + 1) % SLOTS;
        }
        counts[slot] += 1;
        last_key = key;
        last_slot = slot;
    }
    Some(found.into_iter().map(|s| (keys[s], counts[s])).collect())
}

// ---------------------------------------------------------------------------------------------
// Histogram + median cut
// ---------------------------------------------------------------------------------------------

struct Entry {
    color: Px,
    weight: f32,
}

/// Exact (full precision) histogram when the image has few enough distinct colours; `None`
/// once the table would overflow.
fn histogram_exact(rgba: &[u8]) -> Option<Vec<Entry>> {
    const SLOTS: usize = 1 << 17;
    const LIMIT: usize = SLOTS / 2;
    let mut keys = vec![0u32; SLOTS];
    let mut counts = vec![0u32; SLOTS];
    let mut used = 0usize;
    let mut last_key = 0u32;
    let mut last_slot = usize::MAX;
    for p in rgba.chunks_exact(4) {
        if p[3] == 0 {
            continue;
        }
        let key = norm_key(p);
        if last_slot != usize::MAX && key == last_key {
            counts[last_slot] += 1;
            continue;
        }
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> 15) as usize;
        loop {
            if counts[slot] == 0 {
                used += 1;
                if used > LIMIT {
                    return None;
                }
                keys[slot] = key;
                break;
            }
            if keys[slot] == key {
                break;
            }
            slot = (slot + 1) & (SLOTS - 1);
        }
        counts[slot] += 1;
        last_key = key;
        last_slot = slot;
    }
    // Slot order depends only on the pixel data (fixed hash), so this is deterministic.
    Some(
        (0..SLOTS)
            .filter(|&i| counts[i] > 0)
            .map(|i| {
                let b = keys[i].to_le_bytes();
                Entry {
                    color: premultiply(&b),
                    weight: counts[i] as f32,
                }
            })
            .collect(),
    )
}

fn histogram(rgba: &[u8]) -> Vec<Entry> {
    if let Some(exact) = histogram_exact(rgba) {
        return exact;
    }
    const BINS: usize = 1 << 19;
    let mut count = vec![0u32; BINS];
    let mut sums = vec![[0u64; 4]; BINS];
    for p in rgba.chunks_exact(4) {
        let a = p[3] as u32;
        if a == 0 {
            continue;
        }
        let (r, g, b) = if a == 255 {
            (p[0] as u32, p[1] as u32, p[2] as u32)
        } else {
            (
                (p[0] as u32 * a + 127) / 255,
                (p[1] as u32 * a + 127) / 255,
                (p[2] as u32 * a + 127) / 255,
            )
        };
        let bin = ((r >> 3) << 14 | (g >> 3) << 9 | (b >> 3) << 4 | (a >> 4)) as usize;
        count[bin] += 1;
        let s = &mut sums[bin];
        s[0] += r as u64;
        s[1] += g as u64;
        s[2] += b as u64;
        s[3] += a as u64;
    }
    count
        .iter()
        .zip(&sums)
        .filter(|(c, _)| **c > 0)
        .map(|(&c, s)| {
            let n = c as f32;
            Entry {
                color: [
                    s[0] as f32 / n,
                    s[1] as f32 / n,
                    s[2] as f32 / n,
                    s[3] as f32 / n,
                ],
                weight: n,
            }
        })
        .collect()
}

#[derive(Clone)]
struct Cut {
    lo: usize,
    hi: usize,
    weight: f64,
    sum: [f64; 4],
    sumsq: [f64; 4],
}

impl Cut {
    fn measure(entries: &[Entry], lo: usize, hi: usize) -> Cut {
        let mut c = Cut {
            lo,
            hi,
            weight: 0.0,
            sum: [0.0; 4],
            sumsq: [0.0; 4],
        };
        for e in &entries[lo..hi] {
            let w = e.weight as f64;
            c.weight += w;
            for k in 0..4 {
                let v = e.color[k] as f64;
                c.sum[k] += w * v;
                c.sumsq[k] += w * v * v;
            }
        }
        c
    }

    fn sse(&self) -> f64 {
        if self.weight <= 0.0 {
            return 0.0;
        }
        (0..4)
            .map(|k| self.sumsq[k] - self.sum[k] * self.sum[k] / self.weight)
            .sum::<f64>()
            .max(0.0)
    }

    fn mean(&self) -> Px {
        let w = self.weight.max(1e-9);
        [
            (self.sum[0] / w) as f32,
            (self.sum[1] / w) as f32,
            (self.sum[2] / w) as f32,
            (self.sum[3] / w) as f32,
        ]
    }
}

fn median_cut(entries: &mut [Entry], max_colors: usize, target_sse: f64) -> Vec<Px> {
    let mut cuts = vec![Cut::measure(entries, 0, entries.len())];
    let mut total_sse = cuts[0].sse();
    while cuts.len() < max_colors && total_sse > target_sse {
        // Box with the largest error that can still be split.
        let mut pick = None;
        let mut pick_sse = 0.0;
        for (i, c) in cuts.iter().enumerate() {
            let s = c.sse();
            if c.hi - c.lo >= 2 && s > pick_sse {
                pick_sse = s;
                pick = Some(i);
            }
        }
        let Some(i) = pick else { break };
        let cut = cuts[i].clone();
        // Axis with the largest variance.
        let mut axis = 0;
        let mut best_var = -1.0;
        for k in 0..4 {
            let var = cut.sumsq[k] - cut.sum[k] * cut.sum[k] / cut.weight;
            if var > best_var {
                best_var = var;
                axis = k;
            }
        }
        // 256-bin histogram along the axis; pick the boundary that maximises the between-class
        // sum of squares (= minimises the summed error of both halves).
        let mut bin_w = [0f64; 256];
        let mut bin_s = [[0f64; 4]; 256];
        for e in &entries[cut.lo..cut.hi] {
            let b = (e.color[axis].round() as i32).clamp(0, 255) as usize;
            let w = e.weight as f64;
            bin_w[b] += w;
            for k in 0..4 {
                bin_s[b][k] += w * e.color[k] as f64;
            }
        }
        let mut left_w = 0.0;
        let mut left_s = [0.0f64; 4];
        let mut best_gain = -1.0;
        let mut split_bin = 0usize; // entries with bin <= split_bin go left
        for b in 0..255 {
            left_w += bin_w[b];
            for k in 0..4 {
                left_s[k] += bin_s[b][k];
            }
            let right_w = cut.weight - left_w;
            if left_w <= 0.0 || right_w <= 0.0 {
                continue;
            }
            let mut gain = 0.0;
            for k in 0..4 {
                let rs = cut.sum[k] - left_s[k];
                gain += left_s[k] * left_s[k] / left_w + rs * rs / right_w;
            }
            if gain > best_gain {
                best_gain = gain;
                split_bin = b;
            }
        }
        if best_gain < 0.0 {
            // All entries share one bin on the axis; split by position.
            let mid = (cut.lo + cut.hi) / 2;
            let left = Cut::measure(entries, cut.lo, mid);
            let right = Cut::measure(entries, mid, cut.hi);
            total_sse += left.sse() + right.sse() - pick_sse;
            cuts[i] = left;
            cuts.push(right);
            continue;
        }
        // Partition in place (stable order is not needed; determinism comes from fixed rules).
        let slice = &mut entries[cut.lo..cut.hi];
        let mut a = 0usize;
        for j in 0..slice.len() {
            let b = (slice[j].color[axis].round() as i32).clamp(0, 255) as usize;
            if b <= split_bin {
                slice.swap(a, j);
                a += 1;
            }
        }
        let mid = cut.lo + a;
        let left = Cut::measure(entries, cut.lo, mid);
        let right = Cut::measure(entries, mid, cut.hi);
        total_sse += left.sse() + right.sse() - pick_sse;
        cuts[i] = left;
        cuts.push(right);
    }
    cuts.iter().map(Cut::mean).collect()
}

/// Lloyd iterations over the histogram entries.
fn refine(entries: &[Entry], palette: &mut [Px], passes: usize) {
    for _ in 0..passes {
        let tree = KdTree::new(palette);
        let mut sum = vec![[0f64; 4]; palette.len()];
        let mut weight = vec![0f64; palette.len()];
        for e in entries {
            let (i, _) = tree.nearest(&e.color);
            let w = e.weight as f64;
            weight[i] += w;
            for k in 0..4 {
                sum[i][k] += w * e.color[k] as f64;
            }
        }
        for (i, p) in palette.iter_mut().enumerate() {
            if weight[i] > 0.0 {
                for k in 0..4 {
                    p[k] = (sum[i][k] / weight[i]) as f32;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------------------------

const BAND_ROWS: usize = 64;

/// Error-diffusion tuning: full-strength Floyd-Steinberg on photographs costs ~20% more bytes
/// than libimagequant for no visible gain, so the diffused error is damped and clamped. With
/// these values size and mean error are on par with or better than sharp on the test corpus.
const DITHER_GAIN: f32 = 0.6;
const DITHER_CLAMP: f32 = 8.0;

fn map_pixels(
    rgba: &[u8],
    width: usize,
    height: usize,
    tree: &KdTree,
    dither: f32,
    transparent: usize,
) -> Vec<u8> {
    let mut indices = vec![0u8; width * height];
    indices
        .par_chunks_mut(width * BAND_ROWS)
        .enumerate()
        .for_each(|(band, out)| {
            let y0 = band * BAND_ROWS;
            let rows = out.len() / width;
            map_band(
                &rgba[y0 * width * 4..(y0 + rows) * width * 4],
                width,
                rows,
                tree,
                dither,
                transparent,
                out,
            );
        });
    indices
}

fn map_band(
    rgba: &[u8],
    width: usize,
    rows: usize,
    tree: &KdTree,
    dither: f32,
    transparent: usize,
    out: &mut [u8],
) {
    const CACHE: usize = 1 << 12;
    let mut cache_key = vec![u64::MAX; CACHE];
    let mut cache_idx = vec![0u8; CACHE];
    let mut err_cur = vec![[0f32; 4]; width + 2];
    let mut err_next = vec![[0f32; 4]; width + 2];
    let diffuse = dither > 0.0;
    for y in 0..rows {
        for e in err_next.iter_mut() {
            *e = [0.0; 4];
        }
        let row = &rgba[y * width * 4..(y + 1) * width * 4];
        for x in 0..width {
            let p = &row[x * 4..x * 4 + 4];
            if p[3] == 0 {
                out[y * width + x] = transparent as u8;
                continue;
            }
            let src = premultiply(p);
            let e = err_cur[x + 1];
            let zero_err = !diffuse || (e[0] == 0.0 && e[1] == 0.0 && e[2] == 0.0 && e[3] == 0.0);
            let target: Px = if zero_err {
                src
            } else {
                let mut q = [
                    (src[0] + e[0]).clamp(0.0, 255.0),
                    (src[1] + e[1]).clamp(0.0, 255.0),
                    (src[2] + e[2]).clamp(0.0, 255.0),
                    if p[3] == 255 {
                        255.0
                    } else {
                        (src[3] + e[3]).clamp(0.0, 255.0)
                    },
                ];
                // Premultiplied colour cannot exceed alpha.
                for k in 0..3 {
                    q[k] = q[k].min(q[3]);
                }
                q
            };
            let idx = if zero_err {
                let key = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                let slot = (key.wrapping_mul(0x9E37_79B1) >> 20) as usize;
                let key = key as u64;
                if cache_key[slot] == key {
                    cache_idx[slot] as usize
                } else {
                    let found = tree.nearest(&target).0;
                    cache_key[slot] = key;
                    cache_idx[slot] = found as u8;
                    found
                }
            } else {
                tree.nearest(&target).0
            };
            let chosen = tree.colors[idx];
            out[y * width + x] = idx as u8;
            if diffuse {
                let mut d = [0f32; 4];
                let mut any = false;
                for k in 0..4 {
                    d[k] = ((target[k] - chosen[k]) * dither * DITHER_GAIN)
                        .clamp(-DITHER_CLAMP, DITHER_CLAMP);
                    any |= d[k] != 0.0;
                }
                if p[3] == 255 {
                    d[3] = 0.0;
                }
                if any {
                    for k in 0..4 {
                        let v = d[k];
                        err_cur[x + 2][k] += v * (7.0 / 16.0);
                        err_next[x][k] += v * (3.0 / 16.0);
                        err_next[x + 1][k] += v * (5.0 / 16.0);
                        err_next[x + 2][k] += v * (1.0 / 16.0);
                    }
                }
            }
        }
        std::mem::swap(&mut err_cur, &mut err_next);
    }
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

fn unpremultiply(c: &Px) -> [u8; 4] {
    let a = c[3].round().clamp(0.0, 255.0);
    if a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let k = 255.0 / a;
    [
        (c[0] * k).round().clamp(0.0, 255.0) as u8,
        (c[1] * k).round().clamp(0.0, 255.0) as u8,
        (c[2] * k).round().clamp(0.0, 255.0) as u8,
        a as u8,
    ]
}

/// Target total squared error (premultiplied RGBA, 8-bit units) for a quality in 0..=100.
fn target_sse(quality: u8, pixels: usize) -> f64 {
    if quality >= 100 {
        return 0.0;
    }
    let t = (100 - quality as i32) as f64 / 100.0;
    let rms = 16.0 * t * t; // per-channel RMS in 8-bit levels
    rms * rms * 4.0 * pixels as f64
}

pub fn quantize(rgba: &[u8], width: usize, height: usize, s: &Settings) -> Quantized {
    let pixels = width * height;
    let max_colors = s.max_colors.clamp(2, 256);

    // Lossless shortcut.
    if let Some(mut colors) = exact_colors(rgba, max_colors) {
        // Opaque-last, then by popularity, then by value: deterministic order.
        colors.sort_by(|a, b| {
            let (aa, ba) = ((a.0 >> 24) == 255, (b.0 >> 24) == 255);
            aa.cmp(&ba).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0))
        });
        return finish_exact(rgba, pixels, &colors);
    }

    // Fully transparent pixels all map to one reserved, exactly transparent entry.
    let has_clear = rgba.chunks_exact(4).any(|p| p[3] == 0);
    let budget = max_colors - has_clear as usize;
    let mut entries = histogram(rgba);
    let mut palette = if entries.is_empty() {
        Vec::new()
    } else {
        median_cut(&mut entries, budget, target_sse(s.quality, pixels))
    };
    refine(&entries, &mut palette, (s.effort as usize) / 2);
    let transparent = palette.len();
    if has_clear || palette.is_empty() {
        palette.push([0.0; 4]);
    }
    let tree = KdTree::new(&palette);
    let indices = map_pixels(
        rgba,
        width,
        height,
        &tree,
        s.dither.clamp(0.0, 1.0),
        transparent,
    );

    // Population per entry, then order: translucent first (for tRNS), by popularity.
    let mut pop = vec![0u64; palette.len()];
    for &i in &indices {
        pop[i as usize] += 1;
    }
    let mut order: Vec<usize> = (0..palette.len()).filter(|&i| pop[i] > 0).collect();
    let colors: Vec<[u8; 4]> = palette.iter().map(unpremultiply).collect();
    order.sort_by(|&a, &b| {
        let (ta, tb) = (colors[a][3] == 255, colors[b][3] == 255);
        ta.cmp(&tb)
            .then(pop[b].cmp(&pop[a]))
            .then(colors[a].cmp(&colors[b]))
    });
    let mut remap = vec![0u8; palette.len()];
    for (new, &old) in order.iter().enumerate() {
        remap[old] = new as u8;
    }
    let indices: Vec<u8> = indices.iter().map(|&i| remap[i as usize]).collect();
    assemble(order.iter().map(|&i| colors[i]).collect(), indices)
}

fn finish_exact(rgba: &[u8], pixels: usize, colors: &[(u32, u64)]) -> Quantized {
    let mut lookup = std::collections::HashMap::with_capacity(colors.len() * 2);
    for (i, (key, _)) in colors.iter().enumerate() {
        lookup.insert(*key, i as u8);
    }
    let mut indices = Vec::with_capacity(pixels);
    let mut last = (u32::MAX, 0u8);
    for p in rgba.chunks_exact(4) {
        let key = norm_key(p);
        if key != last.0 {
            last = (key, lookup[&key]);
        }
        indices.push(last.1);
    }
    assemble(
        colors
            .iter()
            .map(|(k, _)| {
                let b = k.to_le_bytes();
                [b[0], b[1], b[2], b[3]]
            })
            .collect(),
        indices,
    )
}

fn assemble(colors: Vec<[u8; 4]>, indices: Vec<u8>) -> Quantized {
    let mut palette = Vec::with_capacity(colors.len() * 3);
    let mut trns: Vec<u8> = Vec::new();
    for c in &colors {
        palette.extend_from_slice(&c[..3]);
        trns.push(c[3]);
    }
    while trns.last() == Some(&255) {
        trns.pop();
    }
    Quantized {
        palette,
        trns,
        indices,
        colors: colors.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(w: usize, h: usize, alpha: bool) -> Vec<u8> {
        let mut s = 99u32;
        let mut v = Vec::new();
        for i in 0..w * h {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let x = (i % w) as u32;
            v.extend_from_slice(&[
                (x * 255 / w as u32) as u8,
                (s >> 24) as u8,
                ((s >> 16) as u8) / 2,
                if alpha { (s >> 8) as u8 } else { 255 },
            ]);
        }
        v
    }

    #[test]
    fn exact_palette_is_lossless() {
        let mut data = Vec::new();
        for i in 0..4000u32 {
            let c = (i % 5) as u8 * 40;
            data.extend_from_slice(&[c, 255 - c, 7, if i % 5 == 0 { 0 } else { 255 }]);
        }
        let q = quantize(
            &data,
            100,
            40,
            &Settings {
                max_colors: 256,
                quality: 100,
                effort: 7,
                dither: 1.0,
            },
        );
        assert_eq!(q.colors, 5);
        for (i, p) in data.chunks_exact(4).enumerate() {
            let idx = q.indices[i] as usize;
            let a = *q.trns.get(idx).unwrap_or(&255);
            assert_eq!(a, p[3]);
            if p[3] != 0 {
                assert_eq!(&q.palette[idx * 3..idx * 3 + 3], &p[..3]);
            }
        }
    }

    #[test]
    fn lossy_respects_color_cap_and_is_deterministic() {
        let data = noise(128, 64, true);
        for cap in [2usize, 4, 16, 256] {
            let s = Settings {
                max_colors: cap,
                quality: 100,
                effort: 7,
                dither: 1.0,
            };
            let a = quantize(&data, 128, 64, &s);
            let b = quantize(&data, 128, 64, &s);
            assert!(a.colors <= cap);
            assert!(a.indices.iter().all(|&i| (i as usize) < a.colors));
            assert_eq!(a.indices, b.indices);
            assert_eq!(a.palette, b.palette);
        }
    }
}
