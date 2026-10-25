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
    let finish = |acc: i32| -> u16 { (((acc as u64 + half) * recip) >> 40) as u16 };
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
    (0..65536usize)
        .into_par_iter()
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

// ---------------------------------------------------------------------------------------------
// sRGB <-> CIELAB in blocks of pixels
//
// These are the arithmetic of `vips::srgb_to_lab` / `vips::lab_to_srgb`, operation for
// operation (same f64 expressions, same f32 quantisation), laid out as loops over small arrays
// so that the compiler vectorises them, and compiled for AVX2 when the CPU has it. The one
// difference is the cube root of the forward transform, which uses Halley iterations instead
// of libm's `cbrt`: they agree to a unit in the last place, which never moves a quantised
// value (checked over all 2^24 input colours in the tests).
// ---------------------------------------------------------------------------------------------

const BLOCK: usize = 8;
const UNIT: f32 = 32767.0 / 100.0;

/// Cube root of `t` in about [0.008, 1.3] to a few ulps: an integer-bit-trick seed for
/// `t^(-1/3)` refined by three division-free Newton steps in single precision (eight lanes per
/// vector), then one third-order Halley step in double precision.
#[inline(always)]
fn cbrt_fast(t: f64) -> f64 {
    let tf = t as f32;
    let mut z = f32::from_bits(0x54A2_3400 - tf.to_bits() / 3);
    for _ in 0..3 {
        z = z * (4.0 - tf * z * z * z) * (1.0 / 3.0);
    }
    let z = z as f64;
    let c = t * z * z;
    let c3 = c * c * c;
    c * (c3 + 2.0 * t) / (2.0 * c3 + t)
}

#[inline(always)]
fn lab_f(t: f64) -> f64 {
    // Both branches are evaluated for every lane; the cube root input is kept in range.
    let cube_root = cbrt_fast(t.max(vips::EPS));
    if t > vips::EPS {
        cube_root
    } else {
        7.787 * t + 16.0 / 116.0
    }
}

/// Forward transform of up to `BLOCK` RGBA pixels into quantised `LABS` L, a and b.
#[inline(always)]
fn forward_block(
    px: &[u8],
    decode: &[f32; 256],
    l_out: &mut [u16],
    a_out: &mut [i16],
    b_out: &mut [i16],
) {
    let m = vips::RGB2XYZ;
    let white = vips::WHITE;
    let mut lin = [[0f64; BLOCK]; 3];
    for i in 0..BLOCK {
        // A short block repeats its last pixel; the extra lanes are discarded.
        let at = (i * 4).min(px.len() - 4);
        lin[0][i] = decode[px[at] as usize] as f64;
        lin[1][i] = decode[px[at + 1] as usize] as f64;
        lin[2][i] = decode[px[at + 2] as usize] as f64;
    }
    let mut f = [[0f64; BLOCK]; 3];
    for j in 0..3 {
        for i in 0..BLOCK {
            let xyz = 100.0 * (m[j][0] * lin[0][i] + m[j][1] * lin[1][i] + m[j][2] * lin[2][i]);
            f[j][i] = lab_f(xyz / white[j]);
        }
    }
    let n = px.len() / 4;
    for i in 0..BLOCK.min(n) {
        let (fx, fy, fz) = (f[0][i], f[1][i], f[2][i]);
        let l = (116.0 * fy - 16.0) as f32;
        let a = (500.0 * (fx - fy)) as f32;
        let b = (200.0 * (fy - fz)) as f32;
        l_out[i] = (l * UNIT).round_ties_even().clamp(0.0, 32767.0) as u16;
        a_out[i] = (a * 256.0).round_ties_even().clamp(-32768.0, 32767.0) as i16;
        b_out[i] = (b * 256.0).round_ties_even().clamp(-32768.0, 32767.0) as i16;
    }
}

#[inline(always)]
fn forward_row_impl(px: &[u8], l: &mut [u16], a: &mut [i16], b: &mut [i16]) {
    let decode = vips::decode_lut();
    for (((px, l), a), b) in px
        .chunks(BLOCK * 4)
        .zip(l.chunks_mut(BLOCK))
        .zip(a.chunks_mut(BLOCK))
        .zip(b.chunks_mut(BLOCK))
    {
        forward_block(px, decode, l, a, b);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn forward_row_avx2(px: &[u8], l: &mut [u16], a: &mut [i16], b: &mut [i16]) {
    forward_row_impl(px, l, a, b);
}

fn forward_row(px: &[u8], l: &mut [u16], a: &mut [i16], b: &mut [i16]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked just above); the callee only runs safe code.
        return unsafe { forward_row_avx2(px, l, a, b) };
    }
    forward_row_impl(px, l, a, b);
}

#[inline(always)]
fn lab_f_inv(f: f64, white: f64) -> f64 {
    let cube = f * f * f;
    if cube > vips::EPS {
        white * cube
    } else {
        white * (f - 16.0 / 116.0) / 7.787
    }
}

/// Reverse transform of one row: L comes from `l` (already sharpened, 0..32767), a and b from the
/// quantised planes; writes RGB into the first three bytes of each RGBA pixel.
///
/// Blocks in which every lane takes the same branch (nearly all of them: L >= 8 and cubes
/// above the epsilon) skip the arithmetic of the other branch.
#[inline(always)]
fn reverse_row_impl(l: &[i32], a: &[i16], b: &[i16], px: &mut [u8]) {
    let m = vips::xyz2rgb();
    let white = vips::WHITE;
    let encode = vips::encode_lut();
    let steps = vips::ENCODE_STEPS as f32;
    for (((l, a), b), px) in l
        .chunks(BLOCK)
        .zip(a.chunks(BLOCK))
        .zip(b.chunks(BLOCK))
        .zip(px.chunks_mut(BLOCK * 4))
    {
        let n = l.len();
        // A short block repeats its last pixel; the extra lanes are discarded.
        let (mut li, mut ai, mut bi) = ([0i32; BLOCK], [0i16; BLOCK], [0i16; BLOCK]);
        if n == BLOCK {
            li.copy_from_slice(l);
            ai.copy_from_slice(a);
            bi.copy_from_slice(b);
        } else {
            for i in 0..BLOCK {
                let k = i.min(n - 1);
                (li[i], ai[i], bi[i]) = (l[k], a[k], b[k]);
            }
        }
        let (mut lv, mut av, mut bv) = ([0f64; BLOCK], [0f64; BLOCK], [0f64; BLOCK]);
        for i in 0..BLOCK {
            lv[i] = (li[i] as f32 / UNIT) as f64;
            av[i] = (ai[i] as f32 / 256.0) as f64;
            bv[i] = (bi[i] as f32 / 256.0) as f64;
        }
        let mut fy = [0f64; BLOCK];
        let mut y = [0f64; BLOCK];
        for i in 0..BLOCK {
            let f = (lv[i] + 16.0) / 116.0;
            fy[i] = f;
            y[i] = white[1] * f * f * f;
        }
        if lv.iter().any(|&v| v < 8.0) {
            for i in 0..BLOCK {
                if lv[i] < 8.0 {
                    let y_low = white[1] * lv[i] / 903.3;
                    fy[i] = 7.787 * (y_low / white[1]) + 16.0 / 116.0;
                    y[i] = y_low;
                }
            }
        }
        let (mut x, mut z) = ([0f64; BLOCK], [0f64; BLOCK]);
        let mut fx = [0f64; BLOCK];
        let mut fz = [0f64; BLOCK];
        for i in 0..BLOCK {
            fx[i] = av[i] / 500.0 + fy[i];
            fz[i] = fy[i] - bv[i] / 200.0;
            x[i] = white[0] * (fx[i] * fx[i] * fx[i]);
            z[i] = white[2] * (fz[i] * fz[i] * fz[i]);
        }
        for i in 0..BLOCK {
            // Lanes at or below the epsilon (very dark, strongly coloured) use the linear
            // branch of the CIELAB inverse instead of the cube.
            if fx[i] * fx[i] * fx[i] <= vips::EPS {
                x[i] = lab_f_inv(fx[i], white[0]);
            }
            if fz[i] * fz[i] * fz[i] <= vips::EPS {
                z[i] = lab_f_inv(fz[i], white[2]);
            }
        }
        let mut lin = [[0f32; BLOCK]; 3];
        for i in 0..BLOCK {
            let xyz = [x[i] / 100.0, y[i] / 100.0, z[i] / 100.0];
            for c in 0..3 {
                lin[c][i] = (m[c][0] * xyz[0] + m[c][1] * xyz[1] + m[c][2] * xyz[2]) as f32;
            }
        }
        for i in 0..n {
            for c in 0..3 {
                let v = lin[c][i];
                let xs = v.clamp(0.0, 1.0) * steps;
                let idx = (xs as usize).min(vips::ENCODE_STEPS - 1);
                let t = xs - idx as f32;
                let e = encode[idx] + (encode[idx + 1] - encode[idx]) * t;
                px[i * 4 + c] = (e * 255.0 + 0.5) as u8;
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn reverse_row_avx2(l: &[i32], a: &[i16], b: &[i16], px: &mut [u8]) {
    reverse_row_impl(l, a, b, px);
}

fn reverse_row(l: &[i32], a: &[i16], b: &[i16], px: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked just above); the callee only runs safe code.
        return unsafe { reverse_row_avx2(l, a, b, px) };
    }
    reverse_row_impl(l, a, b, px);
}

/// Sharpens RGBA8 `data` in place (RGB only).
pub fn sharpen(
    data: &mut [u8],
    width: usize,
    height: usize,
    sigma: f64,
    (m1, m2, x1, y2, y3): (f64, f64, f64, f64, f64),
) {
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
            forward_row(px_row, l_row, a_row, b_row);
        });
    let blurred = blur_plane(&lab_l, width, height, sigma);
    let lut = curve(m1, m2, x1, y2, y3);
    data.par_chunks_mut(width * 4).enumerate().for_each_init(
        || vec![0i32; width],
        |sharp_l, (y, px_row)| {
            let base = y * width;
            for (i, s) in sharp_l.iter_mut().enumerate() {
                let l = lab_l[base + i] as i32;
                let diff = l - blurred[base + i] as i32;
                *s = (l + lut[(diff + 32768) as usize]).clamp(0, 32767);
            }
            reverse_row(
                sharp_l,
                &lab_a[base..base + width],
                &lab_b[base..base + width],
                px_row,
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-vectorisation forward transform, from `vips::srgb_to_lab`.
    fn reference_forward(r: u8, g: u8, b: u8) -> (u16, i16, i16) {
        let [l, a, b] = vips::srgb_to_lab(r, g, b);
        (
            (l * UNIT).round_ties_even().clamp(0.0, 32767.0) as u16,
            (a * 256.0).round_ties_even().clamp(-32768.0, 32767.0) as i16,
            (b * 256.0).round_ties_even().clamp(-32768.0, 32767.0) as i16,
        )
    }

    fn check_forward(colors: impl Iterator<Item = u32>) {
        let mut px = Vec::new();
        for c in colors {
            px.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]);
        }
        let n = px.len() / 4;
        let (mut l, mut a, mut b) = (vec![0u16; n], vec![0i16; n], vec![0i16; n]);
        forward_row(&px, &mut l, &mut a, &mut b);
        for i in 0..n {
            let want = reference_forward(px[i * 4], px[i * 4 + 1], px[i * 4 + 2]);
            assert_eq!((l[i], a[i], b[i]), want, "rgb {:?}", &px[i * 4..i * 4 + 3]);
        }
    }

    #[test]
    fn forward_matches_reference_on_sampled_colours() {
        check_forward((0..1 << 24).step_by(97));
        check_forward(
            (0..1 << 24)
                .step_by(4099)
                .map(|v: u32| v.reverse_bits() >> 8),
        );
        // Block tail handling: lengths that are not a multiple of the block size.
        for n in 1..20 {
            check_forward((0..n).map(|i| i * 0x0001_0307 + 11));
        }
    }

    /// Every one of the 2^24 colours (about two seconds in release mode).
    #[test]
    #[ignore]
    fn forward_matches_reference_on_all_colours() {
        for high in 0..256u32 {
            check_forward((0..1 << 16).map(|low| (high << 16) | low));
        }
    }

    #[test]
    fn reverse_matches_reference() {
        let mut s = 0x1234_5678u32;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let n = 300_000;
        let l: Vec<i32> = (0..n).map(|_| (next() % 32768) as i32).collect();
        let a: Vec<i16> = (0..n)
            .map(|_| ((next() % 40000) as i32 - 20000) as i16)
            .collect();
        let b: Vec<i16> = (0..n)
            .map(|_| ((next() % 40000) as i32 - 20000) as i16)
            .collect();
        let mut px = vec![9u8; n * 4];
        reverse_row(&l, &a, &b, &mut px);
        for i in 0..n {
            let want =
                vips::lab_to_srgb(l[i] as f32 / UNIT, a[i] as f32 / 256.0, b[i] as f32 / 256.0);
            assert_eq!(
                &px[i * 4..i * 4 + 3],
                &want,
                "l={} a={} b={}",
                l[i],
                a[i],
                b[i]
            );
            assert_eq!(px[i * 4 + 3], 9);
        }
    }
}
