//! Colour maths that mirrors what libvips (and therefore sharp) does for 8-bit sRGB images:
//! greyscale (`B_W`), CIELAB / LCh round trips (modulate, normalise, tint).

use std::sync::OnceLock;

const ENCODE_STEPS: usize = 8192;

fn decode_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0f32; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            *v = srgb_to_linear(i as f64 / 255.0) as f32;
        }
        lut
    })
}

fn encode_lut() -> &'static [f32] {
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=ENCODE_STEPS)
            .map(|i| linear_to_srgb(i as f64 / ENCODE_STEPS as f64) as f32)
            .collect()
    })
}

/// sRGB transfer function (0..1 -> linear 0..1).
pub fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer function (linear 0..1 -> 0..1).
pub fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Fast `linear_to_srgb` through a table with linear interpolation; input is clamped to 0..1.
#[inline]
fn encode(v: f32) -> f32 {
    let lut = encode_lut();
    let x = v.clamp(0.0, 1.0) * ENCODE_STEPS as f32;
    let i = (x as usize).min(ENCODE_STEPS - 1);
    let t = x - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * t
}

/// libvips `B_W`: relative luminance of the linear light, re-encoded with the sRGB curve.
#[inline]
pub fn bw(r: u8, g: u8, b: u8) -> u8 {
    let lut = decode_lut();
    let y = 0.2126 * lut[r as usize] + 0.7152 * lut[g as usize] + 0.0722 * lut[b as usize];
    (encode(y) * 255.0 + 0.5) as u8
}

/// Overwrites r, g and b of every RGBA pixel in `run` with their `B_W` luminance; the tables
/// are looked up once per run instead of once per pixel. Same arithmetic as [`bw`].
pub fn bw_rgba_run(run: &mut [u8]) {
    let decode = decode_lut();
    let encode_table = encode_lut();
    for px in run.chunks_exact_mut(4) {
        let y = 0.2126 * decode[px[0] as usize]
            + 0.7152 * decode[px[1] as usize]
            + 0.0722 * decode[px[2] as usize];
        let x = y.clamp(0.0, 1.0) * ENCODE_STEPS as f32;
        let i = (x as usize).min(ENCODE_STEPS - 1);
        let t = x - i as f32;
        let e = encode_table[i] + (encode_table[i + 1] - encode_table[i]) * t;
        let lum = (e * 255.0 + 0.5) as u8;
        px[..3].fill(lum);
    }
}

const RGB2XYZ: [[f64; 3]; 3] = [
    [0.4124, 0.3576, 0.1805],
    [0.2126, 0.7152, 0.0722],
    [0.0193, 0.1192, 0.9505],
];
const WHITE: [f64; 3] = [95.047, 100.0, 108.883];
const EPS: f64 = 0.008856;

fn xyz2rgb() -> &'static [[f64; 3]; 3] {
    static M: OnceLock<[[f64; 3]; 3]> = OnceLock::new();
    M.get_or_init(|| {
        let m = RGB2XYZ;
        let cof = |r: usize, c: usize| {
            let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
            let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
            m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
        };
        let det = m[0][0] * cof(0, 0) + m[0][1] * cof(0, 1) + m[0][2] * cof(0, 2);
        let mut inv = [[0.0; 3]; 3];
        for (r, row) in inv.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = cof(c, r) / det;
            }
        }
        inv
    })
}

#[inline]
fn lab_f(t: f64) -> f64 {
    if t > EPS {
        t.cbrt()
    } else {
        7.787 * t + 16.0 / 116.0
    }
}

#[inline]
fn lab_f_inv(f: f64, white: f64) -> f64 {
    let cube = f * f * f;
    if cube > EPS {
        white * cube
    } else {
        white * (f - 16.0 / 116.0) / 7.787
    }
}

/// 8-bit sRGB to CIELAB (D65), L in 0..100.
pub fn srgb_to_lab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let lut = decode_lut();
    let lin = [
        lut[r as usize] as f64,
        lut[g as usize] as f64,
        lut[b as usize] as f64,
    ];
    let xyz = [0, 1, 2].map(|i| {
        100.0 * (RGB2XYZ[i][0] * lin[0] + RGB2XYZ[i][1] * lin[1] + RGB2XYZ[i][2] * lin[2])
    });
    let fx = lab_f(xyz[0] / WHITE[0]);
    let fy = lab_f(xyz[1] / WHITE[1]);
    let fz = lab_f(xyz[2] / WHITE[2]);
    [
        (116.0 * fy - 16.0) as f32,
        (500.0 * (fx - fy)) as f32,
        (200.0 * (fy - fz)) as f32,
    ]
}

/// CIELAB (D65) to 8-bit sRGB; out-of-gamut values are clipped.
pub fn lab_to_srgb(l: f32, a: f32, b: f32) -> [u8; 3] {
    let (l, a, b) = (l as f64, a as f64, b as f64);
    let (fy, y) = if l < 8.0 {
        let y = WHITE[1] * l / 903.3;
        (7.787 * (y / WHITE[1]) + 16.0 / 116.0, y)
    } else {
        let fy = (l + 16.0) / 116.0;
        (fy, WHITE[1] * fy * fy * fy)
    };
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;
    let x = lab_f_inv(fx, WHITE[0]);
    let z = lab_f_inv(fz, WHITE[2]);
    let m = xyz2rgb();
    let xyz = [x / 100.0, y / 100.0, z / 100.0];
    [0, 1, 2].map(|i| {
        let lin = m[i][0] * xyz[0] + m[i][1] * xyz[1] + m[i][2] * xyz[2];
        (encode(lin as f32) * 255.0 + 0.5) as u8
    })
}
