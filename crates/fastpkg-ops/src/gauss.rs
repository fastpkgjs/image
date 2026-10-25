//! libvips-compatible Gaussian blur (`gaussblur`) and small integer convolutions on RGBA8.
//!
//! libvips builds a one-sided Gaussian mask (`vips_gaussmask`): taps `exp(-x^2 / (2 sigma^2))`
//! for every `x` until the value drops below `min_ampl`. With `integer` precision the taps are
//! `rint(20 * v)` and the pass divides by their sum with round-half-up; with `float` they are
//! normalised and the result is truncated. The mask is applied horizontally and then
//! vertically (each pass rounded to 8 bits), with the edge pixels replicated.

use rayon::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Precision {
    Integer,
    Float,
    /// libvips' `approximate` box-filter stack is not reproduced; it is treated as `integer`.
    Approximate,
}

impl Precision {
    pub fn parse(name: Option<&str>) -> Self {
        match name {
            Some("float") => Precision::Float,
            Some("approximate") => Precision::Approximate,
            _ => Precision::Integer,
        }
    }
}

/// One-sided mask values `v[0..=radius]` (centre first), as libvips computes them.
fn half_mask(sigma: f64, min_ampl: f64) -> Vec<f64> {
    let two_sigma2 = 2.0 * sigma * sigma;
    let mut half = Vec::new();
    for x in 0..5000 {
        let v = (-((x * x) as f64) / two_sigma2).exp();
        if v < min_ampl {
            break;
        }
        half.push(v);
    }
    if half.is_empty() {
        half.push(1.0);
    }
    half
}

const LANES: usize = 16;

/// Float-precision accumulation: `dst[i] = sum_k taps[k] * srcs[k][i]`.
#[inline(always)]
fn mac_f32(dst: &mut [f32], taps: &[f32], srcs: &[&[f32]]) {
    let split = dst.len() / LANES * LANES;
    let (head, tail) = dst.split_at_mut(split);
    for (block, chunk) in head.chunks_exact_mut(LANES).enumerate() {
        let base = block * LANES;
        let mut acc = [0f32; LANES];
        for (&weight, src) in taps.iter().zip(srcs) {
            for (a, &value) in acc.iter_mut().zip(&src[base..base + LANES]) {
                *a += value * weight;
            }
        }
        chunk.copy_from_slice(&acc);
    }
    for (i, out) in tail.iter_mut().enumerate() {
        let mut acc = 0f32;
        for (&weight, src) in taps.iter().zip(srcs) {
            acc += src[split + i] * weight;
        }
        *out = acc;
    }
}

fn full_mask(sigma: f64, min_ampl: f64) -> Vec<f64> {
    let half = half_mask(sigma, min_ampl);
    half.iter()
        .rev()
        .chain(half.iter().skip(1))
        .copied()
        .collect()
}

/// Fixed-point taps and their shift for libvips' `integer` precision.
fn build_taps(sigma: f64, min_ampl: f64) -> (Vec<i32>, u32) {
    let mask: Vec<i32> = full_mask(sigma, min_ampl)
        .iter()
        .map(|v| (v * 20.0).round_ties_even() as i32)
        .collect();
    let sum = mask.iter().sum::<i32>().max(1);
    // libvips' SIMD convolution turns the integer mask into fixed-point taps
    // `round(m * 2^k / sum)`: `k` is the smallest shift that makes the centre tap
    // (20 * 2^k / sum) larger than 32. (A sum of 30, i.e. sigma ~0.6, uses k = 12.)
    let shift = if sum == 30 {
        12
    } else {
        (1..=15u32)
            .find(|k| 20 * (1i64 << k) > 32 * sum as i64)
            .unwrap_or(15)
    };
    let one = 1i64 << shift;
    let taps = mask
        .iter()
        .map(|&m| ((m as i64 * one) as f64 / sum as f64).round() as i32)
        .collect();
    (taps, shift)
}

/// Separable Gaussian blur of RGBA8 data, equivalent to libvips `gaussblur`.
pub fn gaussblur(
    data: Vec<u8>,
    width: usize,
    height: usize,
    sigma: f64,
    min_ampl: f64,
    precision: Precision,
) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    if precision == Precision::Float {
        return gaussblur_float(&data, width, height, sigma, min_ampl);
    }
    let (taps, shift) = build_taps(sigma, min_ampl);
    let radius = taps.len() / 2;
    if radius == 0 {
        return data;
    }
    blur_fixed(data, width, height, &taps[radius..], shift)
}

const STRIP: usize = 256;

/// Folded symmetric multiply-accumulate with fixed-point rounding:
/// `dst[i] = min(255, (w[0] * centre[i] + sum_k w[k] * (minus_k[i] + plus_k[i]) + half) >> shift)`.
/// All values are integers below 2^24, so the f32 arithmetic is exact and the result equals the
/// integer convolution libvips performs; f32 vectorises on every target without extra features.
#[inline(always)]
fn mac_fold_generic<const WIDE: usize>(
    dst: &mut [u8],
    weights: &[f32],
    shift: u32,
    centre: &[f32],
    pairs: &[(&[f32], &[f32])],
) {
    let inv = 1.0 / (1u32 << shift) as f32;
    // floor((acc + 2^(shift-1)) / 2^shift) == round_to_nearest((acc + 0.5) / 2^shift) (no ties
    // occur), and adding 2^23 leaves that integer in the low mantissa bits. This avoids the
    // saturating float-to-int cast, which does not vectorise.
    const MAGIC: f32 = 8_388_608.0;
    let finish =
        |acc: f32| -> u8 { ((acc + 0.5) * inv + MAGIC).min(MAGIC + 255.0).to_bits() as u8 };

    // Register blocks of decreasing width: one block keeps its accumulators in vector registers
    // across every tap, so wider blocks amortise the per-tap bookkeeping.
    #[inline(always)]
    fn block<const L: usize>(
        out: &mut [u8],
        base: usize,
        weights: &[f32],
        centre: &[f32],
        pairs: &[(&[f32], &[f32])],
        finish: &impl Fn(f32) -> u8,
    ) {
        let mut acc = [0f32; L];
        for (a, &c) in acc.iter_mut().zip(&centre[base..base + L]) {
            *a = c * weights[0];
        }
        for (&weight, (minus, plus)) in weights[1..].iter().zip(pairs) {
            let (m, p) = (&minus[base..base + L], &plus[base..base + L]);
            for i in 0..L {
                acc[i] += weight * (m[i] + p[i]);
            }
        }
        for (o, &a) in out.iter_mut().zip(&acc) {
            *o = finish(a);
        }
    }

    let mut at = 0;
    while at + WIDE <= dst.len() {
        block::<WIDE>(&mut dst[at..at + WIDE], at, weights, centre, pairs, &finish);
        at += WIDE;
    }
    while at + 16 <= dst.len() {
        block::<16>(&mut dst[at..at + 16], at, weights, centre, pairs, &finish);
        at += 16;
    }
    while at + 4 <= dst.len() {
        block::<4>(&mut dst[at..at + 4], at, weights, centre, pairs, &finish);
        at += 4;
    }
    for i in at..dst.len() {
        let mut acc = centre[i] * weights[0];
        for (&weight, (minus, plus)) in weights[1..].iter().zip(pairs) {
            acc += weight * (minus[i] + plus[i]);
        }
        dst[i] = finish(acc);
    }
}

/// [`mac_fold_generic`] compiled for the best vector width the CPU offers. The build targets
/// baseline x86-64 (SSE2); with AVX2 the same f32 loop vectorises twice as wide.
fn mac_fold(
    dst: &mut [u8],
    weights: &[f32],
    shift: u32,
    centre: &[f32],
    pairs: &[(&[f32], &[f32])],
) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked just above); the callee only runs safe code.
        return unsafe { mac_fold_avx2(dst, weights, shift, centre, pairs) };
    }
    mac_fold_generic::<32>(dst, weights, shift, centre, pairs);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn mac_fold_avx2(
    dst: &mut [u8],
    weights: &[f32],
    shift: u32,
    centre: &[f32],
    pairs: &[(&[f32], &[f32])],
) {
    mac_fold_generic::<64>(dst, weights, shift, centre, pairs);
}

/// Separable convolution with symmetric fixed-point taps (`half_taps[0]` is the centre tap).
fn blur_fixed(
    mut data: Vec<u8>,
    width: usize,
    height: usize,
    half_taps: &[i32],
    shift: u32,
) -> Vec<u8> {
    let radius = half_taps.len() - 1;
    let weights: Vec<f32> = half_taps.iter().map(|&t| t as f32).collect();
    let row_len = width * 4;

    // Horizontal pass, in place: one edge-replicated f32 copy of the row, then folded taps over it.
    data.par_chunks_exact_mut(row_len).for_each_init(
        || vec![0f32; (width + 2 * radius) * 4],
        |padded, row| {
            let lead = radius * 4;
            for (d, &v) in padded[lead..lead + row_len].iter_mut().zip(row.iter()) {
                *d = v as f32;
            }
            let (first, last) = (
                [row[0], row[1], row[2], row[3]].map(|v| v as f32),
                [
                    row[row_len - 4],
                    row[row_len - 3],
                    row[row_len - 2],
                    row[row_len - 1],
                ]
                .map(|v| v as f32),
            );
            for chunk in padded[..lead].chunks_exact_mut(4) {
                chunk.copy_from_slice(&first);
            }
            for chunk in padded[lead + row_len..].chunks_exact_mut(4) {
                chunk.copy_from_slice(&last);
            }
            let centre = &padded[lead..lead + row_len];
            let pairs: Vec<(&[f32], &[f32])> = (1..=radius)
                .map(|k| {
                    (
                        &padded[lead - k * 4..lead - k * 4 + row_len],
                        &padded[lead + k * 4..lead + k * 4 + row_len],
                    )
                })
                .collect();
            mac_fold(row, &weights, shift, centre, &pairs);
        },
    );

    // Vertical pass in blocks of rows and strips of columns: the rows a strip needs are widened
    // to f32 once into a small scratch (L2-resident) and every output row of the block reads them.
    let block_rows = (height / (rayon::current_num_threads() * 4))
        .clamp(32, 256)
        .max(4 * radius);
    let mut out = vec![0u8; data.len()];
    out.par_chunks_mut(block_rows * row_len)
        .enumerate()
        .for_each_init(Vec::new, |scratch: &mut Vec<f32>, (block, chunk)| {
            let y0 = block * block_rows;
            let rows = chunk.len() / row_len;
            let lo = y0.saturating_sub(radius);
            let hi = (y0 + rows - 1 + radius).min(height - 1);
            for strip in (0..row_len).step_by(STRIP) {
                let sw = STRIP.min(row_len - strip);
                scratch.clear();
                scratch.resize((hi - lo + 1) * sw, 0.0);
                for (s, line) in scratch.chunks_exact_mut(sw).enumerate() {
                    let source = &data[(lo + s) * row_len + strip..][..sw];
                    for (d, &v) in line.iter_mut().zip(source) {
                        *d = v as f32;
                    }
                }
                let line_at = |y: isize| -> &[f32] {
                    let s = y.clamp(0, height as isize - 1) as usize - lo;
                    &scratch[s * sw..(s + 1) * sw]
                };
                for r in 0..rows {
                    let y = (y0 + r) as isize;
                    let pairs: Vec<(&[f32], &[f32])> = (1..=radius as isize)
                        .map(|k| (line_at(y - k), line_at(y + k)))
                        .collect();
                    let dst = &mut chunk[r * row_len + strip..][..sw];
                    mac_fold(dst, &weights, shift, line_at(y), &pairs);
                }
            }
        });
    out
}

/// 3x3 integer convolution as libvips applies it to 8-bit images in sharp's mild `blur()` and
/// `sharpen()`: `clip(floor(sum / divisor))` (no rounding), edge pixels replicated. `mask` is
/// row-major. All sums are small integers, so f32 arithmetic is exact; `floor(s / d)` equals
/// `round_to_nearest((s - (d - 1) / 2) / d)` (the fraction never reaches 0.5), which avoids the
/// saturating float-to-int cast that does not vectorise.
pub fn conv3x3(data: &[u8], width: usize, height: usize, mask: [i32; 9], divisor: i32) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    const MAGIC: f32 = 8_388_608.0;
    const LANES: usize = 32;
    let row_len = width * 4;
    let stride = (width + 2) * 4;
    let weights = mask.map(|m| m as f32);
    let centre = (divisor as f32 - 1.0) / 2.0;
    let inv = 1.0 / divisor as f32;
    let finish = |acc: f32| -> u8 {
        ((acc - centre) * inv + MAGIC)
            .max(MAGIC)
            .min(MAGIC + 255.0)
            .to_bits() as u8
    };
    let mut out = vec![0u8; data.len()];
    out.par_chunks_exact_mut(row_len).enumerate().for_each_init(
        || vec![0f32; stride * 3],
        |padded, (y, row)| {
            for (r, line) in padded.chunks_exact_mut(stride).enumerate() {
                let sy = (y + r).saturating_sub(1).min(height - 1);
                let source = &data[sy * row_len..(sy + 1) * row_len];
                for (d, &v) in line[4..4 + row_len].iter_mut().zip(source) {
                    *d = v as f32;
                }
                for k in 0..4 {
                    line[k] = line[4 + k];
                    line[4 + row_len + k] = line[row_len + k];
                }
            }
            let taps: Vec<&[f32]> = (0..9)
                .map(|k| {
                    let (r, c) = (k / 3, k % 3);
                    &padded[r * stride + c * 4..r * stride + c * 4 + row_len]
                })
                .collect();
            let mut at = 0;
            while at + LANES <= row_len {
                let mut acc = [0f32; LANES];
                for (tap, &weight) in taps.iter().zip(&weights) {
                    let src = &tap[at..at + LANES];
                    for i in 0..LANES {
                        acc[i] += src[i] * weight;
                    }
                }
                for (o, &a) in row[at..at + LANES].iter_mut().zip(&acc) {
                    *o = finish(a);
                }
                at += LANES;
            }
            for i in at..row_len {
                let mut acc = 0f32;
                for (tap, &weight) in taps.iter().zip(&weights) {
                    acc += tap[i] * weight;
                }
                row[i] = finish(acc);
            }
        },
    );
    out
}

/// `precision: 'float'`: normalised taps, both passes in `f32` (no 8-bit rounding between them),
/// the result truncated. The small bias absorbs f32 accumulation error on flat areas.
fn gaussblur_float(data: &[u8], width: usize, height: usize, sigma: f64, min_ampl: f64) -> Vec<u8> {
    let mask = full_mask(sigma, min_ampl);
    let total: f64 = mask.iter().sum();
    let taps: Vec<f32> = mask.iter().map(|v| (v / total) as f32).collect();
    let radius = taps.len() / 2;
    if radius == 0 {
        return data.to_vec();
    }
    let row_len = width * 4;
    let mut horizontal = vec![0f32; data.len()];
    horizontal
        .par_chunks_exact_mut(row_len)
        .zip(data.par_chunks_exact(row_len))
        .for_each_init(
            || vec![0f32; (width + 2 * radius) * 4],
            |padded, (row, source)| {
                for (j, dst) in padded.chunks_exact_mut(4).enumerate() {
                    let sx = j.saturating_sub(radius).min(width - 1);
                    for (d, &v) in dst.iter_mut().zip(&source[sx * 4..sx * 4 + 4]) {
                        *d = v as f32;
                    }
                }
                let srcs: Vec<&[f32]> = (0..taps.len())
                    .map(|k| &padded[k * 4..k * 4 + row_len])
                    .collect();
                mac_f32(row, &taps, &srcs);
            },
        );
    let mut out = vec![0u8; data.len()];
    out.par_chunks_exact_mut(row_len).enumerate().for_each_init(
        || vec![0f32; row_len],
        |acc, (y, row)| {
            let srcs: Vec<&[f32]> = (0..taps.len())
                .map(|k| {
                    let sy = (y + k).saturating_sub(radius).min(height - 1);
                    &horizontal[sy * row_len..(sy + 1) * row_len]
                })
                .collect();
            mac_f32(acc, &taps, &srcs);
            for (dst, &value) in row.iter_mut().zip(acc.iter()) {
                *dst = (value + 5e-4).clamp(0.0, 255.0) as u8;
            }
        },
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect()
    }

    /// Reference implementation of libvips' integer separable convolution.
    fn reference(data: &[u8], w: usize, h: usize, sigma: f64) -> Vec<u8> {
        let (taps, shift) = build_taps(sigma, 0.2);
        let r = taps.len() / 2;
        let pass = |src: &[u8], horizontal: bool| -> Vec<u8> {
            let mut out = vec![0u8; src.len()];
            for y in 0..h {
                for x in 0..w {
                    for c in 0..4 {
                        let mut acc = 1i64 << (shift - 1);
                        for (k, &t) in taps.iter().enumerate() {
                            let (sx, sy) = if horizontal {
                                ((x + k).saturating_sub(r).min(w - 1), y)
                            } else {
                                (x, (y + k).saturating_sub(r).min(h - 1))
                            };
                            acc += t as i64 * src[(sy * w + sx) * 4 + c] as i64;
                        }
                        out[(y * w + x) * 4 + c] = (acc >> shift).clamp(0, 255) as u8;
                    }
                }
            }
            out
        };
        pass(&pass(data, true), false)
    }

    #[test]
    fn fixed_point_blur_matches_integer_reference() {
        for (w, h, sigma) in [
            (37, 29, 0.6),
            (64, 9, 1.0),
            (5, 70, 2.0),
            (50, 40, 7.5),
            (3, 3, 20.0),
        ] {
            let data = noise(w * h * 4);
            assert_eq!(
                gaussblur(data.clone(), w, h, sigma, 0.2, Precision::Integer),
                reference(&data, w, h, sigma),
                "{w}x{h} sigma {sigma}"
            );
        }
    }

    #[test]
    fn conv3x3_matches_integer_division() {
        for (w, h, mask, divisor) in [
            (9usize, 7usize, [1; 9], 9),
            (33, 5, [-1, -1, -1, -1, 32, -1, -1, -1, -1], 24),
            (1, 4, [1; 9], 9),
            (6, 1, [-1, -1, -1, -1, 32, -1, -1, -1, -1], 24),
        ] {
            let data = noise(w * h * 4);
            let out = conv3x3(&data, w, h, mask, divisor);
            for y in 0..h {
                for x in 0..w {
                    for c in 0..4 {
                        let mut sum = 0i32;
                        for k in 0..9 {
                            let sx = (x + k % 3).saturating_sub(1).min(w - 1);
                            let sy = (y + k / 3).saturating_sub(1).min(h - 1);
                            sum += mask[k] * data[(sy * w + sx) * 4 + c] as i32;
                        }
                        let expected = sum.div_euclid(divisor).clamp(0, 255) as u8;
                        assert_eq!(out[(y * w + x) * 4 + c], expected, "{w}x{h} ({x},{y},{c})");
                    }
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn bench_blur() {
        let (w, h) = (4000, 3000);
        let data = noise(w * h * 4);
        for sigma in [2.0, 20.0] {
            let mut best = f64::MAX;
            for _ in 0..4 {
                let input = data.clone();
                let t = std::time::Instant::now();
                std::hint::black_box(gaussblur(input, w, h, sigma, 0.2, Precision::Integer));
                best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "sigma {sigma}: {best:.0} ms ({} threads)",
                rayon::current_num_threads()
            );
        }
    }
}
