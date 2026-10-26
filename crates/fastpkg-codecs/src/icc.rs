//! Embedded ICC profiles to sRGB at decode time, as sharp does through libvips'
//! `icc_transform(embedded)`: perceptual intent, alpha untouched, and any profile that cannot
//! be used leaves the pixels as they are.
//!
//! The transforms come from `moxcms` (matrix/TRC profiles take its fixed-point SIMD path, LUT
//! profiles such as CMYK its CLUT path) and run on disjoint pixel runs in parallel.

use std::io::Read;
use std::sync::{Arc, OnceLock};

use fastpkg_core::image::PAR_MIN_PIXELS;
use fastpkg_core::Image;
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ProfileVersion, RenderingIntent, ToneReprCurve,
    TransformExecutor, TransformOptions, Xyzd,
};
use rayon::prelude::*;

/// Pixels per rayon task.
const CHUNK_PIXELS: usize = 1 << 14;

/// libvips' built-in CMYK profile (Argyll "Chemical proof", public domain), gzip-compressed.
/// sharp uses it for CMYK input without a usable embedded profile.
static BUILTIN_CMYK_GZ: &[u8] = include_bytes!("../assets/cmyk.icc.gz");

type Exec8 = Arc<dyn TransformExecutor<u8> + Send + Sync>;

fn options() -> TransformOptions {
    TransformOptions {
        rendering_intent: RenderingIntent::Perceptual,
        ..Default::default()
    }
}

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

fn srgb_eotf(e: f64) -> f64 {
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

fn inverse3(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let c = |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let det = m[0][0] * c(1, 1, 2, 2) - m[0][1] * c(1, 0, 2, 2) + m[0][2] * c(1, 0, 2, 1);
    let k = 1.0 / det;
    [
        [c(1, 1, 2, 2) * k, -c(0, 1, 2, 2) * k, c(0, 1, 1, 2) * k],
        [-c(1, 0, 2, 2) * k, c(0, 0, 2, 2) * k, -c(0, 0, 1, 2) * k],
        [c(1, 0, 2, 1) * k, -c(0, 0, 2, 1) * k, c(0, 0, 1, 1) * k],
    ]
}

/// An sRGB destination with black point compensation folded in, for LUT-based sources.
///
/// lcms (hence libvips) compensates the black point whenever the destination is a v4 profile
/// and the intent is perceptual, which is always the case for sharp's sRGB: the source's
/// darkest colour is mapped to black and white stays fixed, as an affine map in XYZ. moxcms has
/// no such option, but the map can live in the destination: its matrix takes the scale, and
/// the offset becomes a per-channel shift of the tone curve (with both rescaled so the curve
/// still spans 0..1). `black` is the source's darkest colorant as 16-bit samples in the layout
/// moxcms uses for it. Matrix/TRC sources have a black point of zero and need none of this.
fn target_with_bpc(source: &ColorProfile, black: [u16; 4]) -> Option<ColorProfile> {
    let mut target = ColorProfile::new_srgb();
    let cols = [target.red_colorant, target.green_colorant, target.blue_colorant];
    let m = [
        [cols[0].x, cols[1].x, cols[2].x],
        [cols[0].y, cols[1].y, cols[2].y],
        [cols[0].z, cols[1].z, cols[2].z],
    ];
    let bp = if matches!(
        source.version(),
        ProfileVersion::V4_0
            | ProfileVersion::V4_1
            | ProfileVersion::V4_2
            | ProfileVersion::V4_3
            | ProfileVersion::V4_4
    ) {
        // The fixed perceptual black of v4 profiles.
        [0.00336, 0.0034731, 0.00287]
    } else {
        // v2: the darkest colorant through the perceptual table, as XYZ (lcms keeps its tint).
        let plain = ColorProfile::new_srgb();
        let exec = source
            .create_transform_16bit(Layout::Rgba, &plain, Layout::Rgba, options())
            .ok()?;
        let mut out = [0u16; 4];
        exec.transform(&black, &mut out).ok()?;
        let lin: Vec<f64> = out[..3].iter().map(|&v| srgb_eotf(v as f64 / 65535.0)).collect();
        let xyz = |row: usize| m[row][0] * lin[0] + m[row][1] * lin[1] + m[row][2] * lin[2];
        [xyz(0), xyz(1), xyz(2)]
    };
    if !(1e-5..0.184).contains(&bp[1]) {
        return None;
    }
    let a: Vec<f64> = (0..3).map(|i| D50[i] / (D50[i] - bp[i])).collect();
    let inv = inverse3(&m);
    // Offset in linear RGB: M^-1 * (-A * bp), negated to a non-negative shift per channel.
    let shift: Vec<f64> = (0..3)
        .map(|j| (0..3).map(|i| inv[j][i] * a[i] * bp[i]).sum::<f64>())
        .collect();
    let colorant = |j: usize| {
        let scale = 1.0 + shift[j];
        Xyzd {
            x: m[0][j] / a[0] * scale,
            y: m[1][j] / a[1] * scale,
            z: m[2][j] / a[2] * scale,
        }
    };
    target.red_colorant = colorant(0);
    target.green_colorant = colorant(1);
    target.blue_colorant = colorant(2);
    let curve = |j: usize| {
        let table: Vec<u16> = (0..8192)
            .map(|i| {
                let lin = (srgb_eotf(i as f64 / 8191.0) + shift[j]) / (1.0 + shift[j]);
                (lin * 65535.0).round() as u16
            })
            .collect();
        Some(ToneReprCurve::Lut(table))
    };
    target.red_trc = curve(0);
    target.green_trc = curve(1);
    target.blue_trc = curve(2);
    target.cicp = None;
    Some(target)
}

/// Runs `exec` over `src` (`src_ch` samples per pixel) into `dst` (`dst_ch` per pixel), split
/// into pixel runs for large inputs. False if any run fails.
fn run<T: Copy + Default + Send + Sync>(
    exec: &(dyn TransformExecutor<T> + Send + Sync),
    src: &[T],
    src_ch: usize,
    dst: &mut [T],
    dst_ch: usize,
) -> bool {
    let go = |(s, d): (&[T], &mut [T])| exec.transform(s, d).is_ok();
    if src.len() / src_ch >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
        src.par_chunks(CHUNK_PIXELS * src_ch)
            .zip(dst.par_chunks_mut(CHUNK_PIXELS * dst_ch))
            .all(go)
    } else {
        go((src, dst))
    }
}

/// Test colours: a grey ramp plus saturated and mid-level primaries and secondaries.
fn probe_pixels() -> Vec<u8> {
    let mut px: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, 255]).collect();
    for r in [0u8, 128, 255] {
        for g in [0u8, 128, 255] {
            for b in [0u8, 128, 255] {
                px.extend_from_slice(&[r, g, b, 255]);
            }
        }
    }
    px
}

/// True if the transform leaves every probe colour within one level (an sRGB-equivalent
/// profile), so the pixels can stay untouched.
fn is_identity8(exec: &Exec8) -> bool {
    let src = probe_pixels();
    let mut dst = vec![0u8; src.len()];
    exec.transform(&src, &mut dst).is_ok()
        && src
            .chunks_exact(4)
            .zip(dst.chunks_exact(4))
            .all(|(a, b)| (0..3).all(|c| a[c].abs_diff(b[c]) <= 1))
}

/// Converts `image` from its embedded profile `profile` to sRGB (to Display P3 for 16-bit
/// data, which is what sharp's `p3` processing profile does). `source_gray` says the file held
/// a single grey channel (plus optional alpha). Returns true if the pixels changed; false for
/// profiles that are corrupt, unsupported, of the wrong colour model, or equivalent to the
/// target.
pub fn convert_embedded(image: &mut Image, profile: &[u8], source_gray: bool) -> bool {
    let Ok(source) = ColorProfile::new_from_slice(profile) else {
        return false;
    };
    let wide = image.wide.is_some();
    let target = if wide {
        ColorProfile::new_display_p3()
    } else {
        ColorProfile::new_srgb()
    };
    match (source.color_space, source_gray) {
        (DataColorSpace::Rgb, false) => {}
        // GREY16 stays as it is in libvips.
        (DataColorSpace::Gray, true) if !wide => {
            return convert_gray(image, &source, &target);
        }
        _ => return false,
    }
    if wide {
        return convert_wide(image, &source, &target);
    }
    let Ok(exec) = source.create_transform_8bit(Layout::Rgba, &target, Layout::Rgba, options())
    else {
        return false;
    };
    if is_identity8(&exec) {
        return false;
    }
    let mut out = vec![0u8; image.data.len()];
    if !run(exec.as_ref(), &image.data, 4, &mut out, 4) {
        return false;
    }
    image.data = out;
    true
}

fn convert_wide(image: &mut Image, source: &ColorProfile, target: &ColorProfile) -> bool {
    let Ok(exec) = source.create_transform_16bit(Layout::Rgba, target, Layout::Rgba, options())
    else {
        return false;
    };
    let Some(wide) = image.wide.as_ref() else {
        return false;
    };
    // Same sampling as the 8-bit check, scaled to 16 bits.
    let src: Vec<u16> = probe_pixels().iter().map(|&v| v as u16 * 257).collect();
    let mut probe = vec![0u16; src.len()];
    if exec.transform(&src, &mut probe).is_err()
        || src
            .chunks_exact(4)
            .zip(probe.chunks_exact(4))
            .all(|(a, b)| (0..3).all(|c| a[c].abs_diff(b[c]) <= 257))
    {
        return false;
    }
    let mut out = vec![0u16; wide.len()];
    if !run(exec.as_ref(), wide, 4, &mut out, 4) {
        return false;
    }
    let (width, height) = (image.width, image.height);
    image.set_wide_rgba(width, height, out).is_ok()
}

fn convert_gray(image: &mut Image, source: &ColorProfile, target: &ColorProfile) -> bool {
    let Ok(exec) = source.create_transform_8bit(Layout::Gray, target, Layout::Rgb, options())
    else {
        return false;
    };
    let pixels = image.data.len() / 4;
    let plane: Vec<u8> = image.data.chunks_exact(4).map(|px| px[0]).collect();
    let mut rgb = vec![0u8; pixels * 3];
    if !run(exec.as_ref(), &plane, 1, &mut rgb, 3) {
        return false;
    }
    for (px, out) in image.data.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
        px[..3].copy_from_slice(out);
    }
    true
}

/// The 8-bit CMYK to sRGB transform of the built-in profile, built once.
fn builtin_cmyk() -> Option<&'static Exec8> {
    static BUILTIN: OnceLock<Option<Exec8>> = OnceLock::new();
    BUILTIN
        .get_or_init(|| {
            let mut raw = Vec::new();
            flate2::read::GzDecoder::new(BUILTIN_CMYK_GZ)
                .read_to_end(&mut raw)
                .ok()?;
            let profile = ColorProfile::new_from_slice(&raw).ok()?;
            cmyk_transform(&profile)
        })
        .as_ref()
}

fn cmyk_transform(profile: &ColorProfile) -> Option<Exec8> {
    if profile.color_space != DataColorSpace::Cmyk {
        return None;
    }
    let target = target_with_bpc(profile, [65535; 4]).unwrap_or_else(ColorProfile::new_srgb);
    profile
        .create_transform_8bit(Layout::Rgba, &target, Layout::Rgb, options())
        .ok()
}

/// Converts interleaved 8-bit CMYK (0 = no ink, 255 = full ink) to opaque RGBA in sRGB, with
/// the embedded profile when it is a usable CMYK profile and libvips' built-in one otherwise.
pub fn cmyk_to_rgba(cmyk: &[u8], embedded: Option<&[u8]>) -> Option<Vec<u8>> {
    let own = embedded
        .and_then(|bytes| ColorProfile::new_from_slice(bytes).ok())
        .and_then(|profile| cmyk_transform(&profile));
    let exec = match own.as_ref() {
        Some(exec) => exec,
        None => builtin_cmyk()?,
    };
    let pixels = cmyk.len() / 4;
    let mut rgba = vec![255u8; pixels * 4];
    let ok = if pixels >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
        cmyk.par_chunks(CHUNK_PIXELS * 4)
            .zip(rgba.par_chunks_mut(CHUNK_PIXELS * 4))
            .all(|(src, dst)| cmyk_run(exec, src, dst))
    } else {
        cmyk_run(exec, cmyk, &mut rgba)
    };
    ok.then_some(rgba)
}

/// CMYK run to the RGB channels of an RGBA run (alpha is left as it is).
fn cmyk_run(exec: &Exec8, src: &[u8], dst: &mut [u8]) -> bool {
    let mut rgb = vec![0u8; src.len() / 4 * 3];
    if exec.transform(src, &mut rgb).is_err() {
        return false;
    }
    for (px, out) in dst.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
        px[..3].copy_from_slice(out);
    }
    true
}
