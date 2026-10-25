//! libvips-compatible `sharpen`: the L channel of the image in CIELAB (as 16-bit `LABS`) is
//! blurred with an integer Gaussian mask, and `L + lut[L - blur(L)]` replaces it, where the lookup
//! table is a two-slope curve (`m1` below `x1`, `m2` above) clipped to `y2` (brighten) and `y3`
//! (darken). Colour (a, b) and alpha are untouched.

use rayon::prelude::*;

use crate::vips;

const LANES: usize = 16;

/// One-sided integer mask `rint(20 * exp(-x^2 / (2 sigma^2)))` down to an amplitude of 0.1.
fn mask(sigma: f64) -> Vec<i32> {
    let two_sigma2 = 2.0 * sigma * sigma;
    let mut half = Vec::new();
    for x in 0..5000 {
        let v = (-((x * x) as f64) / two_sigma2).exp();
        if v < 0.1 {
            break;
        }
        half.push((v * 20.0).round_ties_even() as i32);
    }
    if half.is_empty() {
        half.push(20);
    }
    half.iter()
        .rev()
        .chain(half.iter().skip(1))
        .copied()
        .collect()
}

/// `dst[i] = (sum_k taps[k] * srcs[k][i] + scale/2) / scale`, exact integer division through a
/// 40-bit reciprocal (inputs are below 2^15 * scale, which keeps the quotient exact).
#[inline(always)]
fn mac_u16(dst: &mut [u16], taps: &[i32], srcs: &[&[u16]], scale: i32) {
    let half = (scale / 2) as u64;
    let recip = ((1u64 << 40) / scale as u64) + 1;
    let finish = |acc: i32| -> u16 { ((acc as u64 + half) * recip >> 40) as u16 };
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

fn blur_plane(plane: &[u16], width: usize, height: usize, sigma: f64) -> Vec<u16> {
    let taps = mask(sigma);
    let scale: i32 = taps.iter().sum();
    let radius = taps.len() / 2;
    if radius == 0 {
        return plane.to_vec();
    }
    let mut horizontal = vec![0u16; plane.len()];
    horizontal
        .par_chunks_exact_mut(width)
        .zip(plane.par_chunks_exact(width))
        .for_each_init(
            || vec![0u16; width + 2 * radius],
            |padded, (row, source)| {
                padded[..radius].fill(source[0]);
                padded[radius..radius + width].copy_from_slice(source);
                padded[radius + width..].fill(source[width - 1]);
                let srcs: Vec<&[u16]> = (0..taps.len()).map(|k| &padded[k..k + width]).collect();
                mac_u16(row, &taps, &srcs, scale);
            },
        );
    let mut out = vec![0u16; plane.len()];
    out.par_chunks_exact_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            let srcs: Vec<&[u16]> = (0..taps.len())
                .map(|k| {
                    let sy = (y + k).saturating_sub(radius).min(height - 1);
                    &horizontal[sy * width..(sy + 1) * width]
                })
                .collect();
            mac_u16(row, &taps, &srcs, scale);
        });
    out
}

/// The sharpening curve over a `LABS` L difference `-32768..32768` (index = difference + 32768).
fn curve(m1: f64, m2: f64, x1: f64, y2: f64, y3: f64) -> Vec<i32> {
    const UNIT: f64 = 32767.0 / 100.0;
    (0..65536)
        .map(|i| {
            let d = i as f64 - 32768.0;
            let x = d.abs() / UNIT;
            let y = if x < x1 {
                m1 * x
            } else {
                m1 * x1 + m2 * (x - x1)
            };
            let y = if d >= 0.0 { y.min(y2) } else { -y.min(y3) };
            (y * UNIT).round_ties_even() as i32
        })
        .collect()
}

/// Sharpens RGBA8 `data` in place (RGB only).
pub fn sharpen(
    data: &mut [u8],
    width: usize,
    height: usize,
    sigma: f64,
    (m1, m2, x1, y2, y3): (f64, f64, f64, f64, f64),
) {
    const UNIT: f32 = 32767.0 / 100.0;
    let pixels = width * height;
    let mut lab_l = vec![0u16; pixels];
    let mut lab_a = vec![0i16; pixels];
    let mut lab_b = vec![0i16; pixels];
    lab_l
        .par_chunks_mut(width)
        .zip(lab_a.par_chunks_mut(width))
        .zip(lab_b.par_chunks_mut(width))
        .zip(data.par_chunks(width * 4))
        .for_each(|(((l_row, a_row), b_row), px_row)| {
            for (i, px) in px_row.chunks_exact(4).enumerate() {
                let [l, a, b] = vips::srgb_to_lab(px[0], px[1], px[2]);
                l_row[i] = (l * UNIT).round_ties_even().clamp(0.0, 32767.0) as u16;
                a_row[i] = (a * 256.0).round_ties_even().clamp(-32768.0, 32767.0) as i16;
                b_row[i] = (b * 256.0).round_ties_even().clamp(-32768.0, 32767.0) as i16;
            }
        });
    let blurred = blur_plane(&lab_l, width, height, sigma);
    let lut = curve(m1, m2, x1, y2, y3);
    data.par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, px_row)| {
            let base = y * width;
            for (i, px) in px_row.chunks_exact_mut(4).enumerate() {
                let l = lab_l[base + i] as i32;
                let diff = l - blurred[base + i] as i32;
                let sharpened = (l + lut[(diff + 32768) as usize]).clamp(0, 32767);
                let rgb = vips::lab_to_srgb(
                    sharpened as f32 / UNIT,
                    lab_a[base + i] as f32 / 256.0,
                    lab_b[base + i] as f32 / 256.0,
                );
                px[..3].copy_from_slice(&rgb);
            }
        });
}
