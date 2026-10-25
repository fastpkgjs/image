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

/// `dst[i] = clip(floor((sum_k taps[k] * srcs[k][i] + half) / scale))` for `i32` taps.
/// Columns are processed in blocks so the accumulator stays in registers across all taps.
#[inline(always)]
fn mac_int(dst: &mut [u8], taps: &[i32], srcs: &[&[u8]], half: i32, scale: i32) {
    let inv = 1.0 / scale as f32;
    let finish = |acc: i32| -> u8 {
        let n = acc + half;
        // Exact floor division: the float quotient is within one of the true value.
        let mut q = (n as f32 * inv) as i32;
        q -= i32::from(q * scale > n);
        q += i32::from((q + 1) * scale <= n);
        q.clamp(0, 255) as u8
    };
    let split = dst.len() / LANES * LANES;
    let (head, tail) = dst.split_at_mut(split);
    for (block, chunk) in head.chunks_exact_mut(LANES).enumerate() {
        let base = block * LANES;
        let mut acc = [0i32; LANES];
        for (&weight, src) in taps.iter().zip(srcs) {
            for (a, &value) in acc.iter_mut().zip(&src[base..base + LANES]) {
                *a += value as i32 * weight;
            }
        }
        for (out, &a) in chunk.iter_mut().zip(&acc) {
            *out = finish(a);
        }
    }
    for (i, out) in tail.iter_mut().enumerate() {
        let mut acc = 0i32;
        for (&weight, src) in taps.iter().zip(srcs) {
            acc += src[split + i] as i32 * weight;
        }
        *out = finish(acc);
    }
}

/// `dst[i] = min(255, (sum_k taps[k] * srcs[k][i] + 2^(shift-1)) >> shift)`.
#[inline(always)]
fn mac_shift(dst: &mut [u8], taps: &[i32], srcs: &[&[u8]], shift: u32) {
    let half = 1i32 << (shift - 1);
    let split = dst.len() / LANES * LANES;
    let (head, tail) = dst.split_at_mut(split);
    for (block, chunk) in head.chunks_exact_mut(LANES).enumerate() {
        let base = block * LANES;
        let mut acc = [half; LANES];
        for (&weight, src) in taps.iter().zip(srcs) {
            for (a, &value) in acc.iter_mut().zip(&src[base..base + LANES]) {
                *a += value as i32 * weight;
            }
        }
        for (out, &a) in chunk.iter_mut().zip(&acc) {
            *out = (a >> shift).clamp(0, 255) as u8;
        }
    }
    for (i, out) in tail.iter_mut().enumerate() {
        let mut acc = half;
        for (&weight, src) in taps.iter().zip(srcs) {
            acc += src[split + i] as i32 * weight;
        }
        *out = (acc >> shift).clamp(0, 255) as u8;
    }
}

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

pub(crate) enum Taps {
    Int { taps: Vec<i32>, shift: u32 },
}

impl Taps {
    fn len(&self) -> usize {
        match self {
            Taps::Int { taps, .. } => taps.len(),
        }
    }

    #[inline(always)]
    fn apply(&self, dst: &mut [u8], srcs: &[&[u8]]) {
        match self {
            Taps::Int { taps, shift } => mac_shift(dst, taps, srcs, *shift),
        }
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

pub(crate) fn build_taps(sigma: f64, min_ampl: f64) -> Taps {
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
    Taps::Int { taps, shift }
}

/// Separable Gaussian blur of RGBA8 data, equivalent to libvips `gaussblur`.
pub fn gaussblur(
    data: &[u8],
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
        return gaussblur_float(data, width, height, sigma, min_ampl);
    }
    let taps = build_taps(sigma, min_ampl);
    let radius = taps.len() / 2;
    if radius == 0 {
        return data.to_vec();
    }
    let row_len = width * 4;

    let mut horizontal = vec![0u8; data.len()];
    horizontal
        .par_chunks_exact_mut(row_len)
        .zip(data.par_chunks_exact(row_len))
        .for_each_init(
            || vec![0u8; (width + 2 * radius) * 4],
            |padded, (row, source)| {
                let lead = radius * 4;
                for chunk in padded[..lead].chunks_exact_mut(4) {
                    chunk.copy_from_slice(&source[..4]);
                }
                padded[lead..lead + row_len].copy_from_slice(source);
                for chunk in padded[lead + row_len..].chunks_exact_mut(4) {
                    chunk.copy_from_slice(&source[row_len - 4..]);
                }
                let srcs: Vec<&[u8]> = (0..taps.len())
                    .map(|k| &padded[k * 4..k * 4 + row_len])
                    .collect();
                taps.apply(row, &srcs);
            },
        );

    let mut out = vec![0u8; data.len()];
    out.par_chunks_exact_mut(row_len)
        .enumerate()
        .for_each(|(y, row)| {
            let srcs: Vec<&[u8]> = (0..taps.len())
                .map(|k| {
                    let sy = (y + k).saturating_sub(radius).min(height - 1);
                    &horizontal[sy * row_len..(sy + 1) * row_len]
                })
                .collect();
            taps.apply(row, &srcs);
        });
    out
}

/// 3x3 integer convolution (libvips `conv` with an int mask): `clip((sum + rounding) / scale)`,
/// edge pixels replicated. `mask` is row-major.
pub fn conv3x3(
    data: &[u8],
    width: usize,
    height: usize,
    mask: [i32; 9],
    scale: i32,
    rounding: i32,
) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let row_len = width * 4;
    let mut out = vec![0u8; data.len()];
    out.par_chunks_exact_mut(row_len).enumerate().for_each_init(
        || vec![0u8; (width + 2) * 4 * 3],
        |padded, (y, row)| {
            let stride = (width + 2) * 4;
            for (r, line) in padded.chunks_exact_mut(stride).enumerate() {
                let sy = (y + r).saturating_sub(1).min(height - 1);
                let source = &data[sy * row_len..(sy + 1) * row_len];
                line[..4].copy_from_slice(&source[..4]);
                line[4..4 + row_len].copy_from_slice(source);
                line[4 + row_len..].copy_from_slice(&source[row_len - 4..]);
            }
            let srcs: Vec<&[u8]> = (0..9)
                .map(|k| {
                    let (r, c) = (k / 3, k % 3);
                    &padded[r * stride + c * 4..r * stride + c * 4 + row_len]
                })
                .collect();
            mac_int(row, &mask, &srcs, rounding, scale);
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
