#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::too_many_arguments,
    clippy::needless_range_loop,
    clippy::unnecessary_unwrap
)]

use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use fast_image_resize::images::{Image as FirImage, TypedImage};
use fast_image_resize::pixels::U16x4;
use fast_image_resize::{FilterType as FirFilter, PixelType, ResizeAlg, ResizeOptions, Resizer};
use fastpkg_core::options::{
    Blend, ChannelSelector, ExtendWith, Fit, GradientKind, GradientSpec, GradientSpread, Kernel,
    NumberOrArray, Operation,
};
use fastpkg_core::{Color, FastpkgError, Format, Image, Result, Stats};
use image::imageops::FilterType as ImageFilter;
use rayon::prelude::*;

mod gauss;
mod sharpen;
mod vips;

pub fn apply(mut image: Image, operations: &[Operation]) -> Result<Image> {
    for op in operations {
        image = apply_one(image, op)?;
    }
    Ok(image)
}

/// For operations that commute with a box shrink done at decode time (the shrunk image can pass
/// through them with `pre_shrink` carried along): whether they swap width and height. These are
/// the operations that move pixels without resampling them (auto-orient, quarter-turn rotations,
/// flips) and the per-pixel linear ones (flatten, greyscale: the shrink averages premultiplied
/// samples, and a linear map of those equals the average of the mapped pixels). `None` for
/// everything else.
pub fn geometry_swaps_axes(op: &Operation, orientation: Option<u32>) -> Option<bool> {
    match op {
        Operation::Flatten { .. } | Operation::Greyscale | Operation::Grayscale => Some(false),
        Operation::AutoOrient => Some(matches!(orientation, Some(5..=8))),
        Operation::Flip { .. } | Operation::Flop { .. } => Some(false),
        Operation::Rotate { angle, .. } => {
            let turns = angle / 90.0;
            if !angle.is_finite() || turns.fract() != 0.0 {
                return None;
            }
            Some((turns as i64).rem_euclid(2) == 1)
        }
        _ => None,
    }
}

pub fn apply_one(mut image: Image, op: &Operation) -> Result<Image> {
    // Many operations rebuild the image buffer from scratch; carry per-image
    // metadata across so EXIF/ICC/XMP and the single-channel hint survive them.
    let gray = image.gray;
    let icc = image.icc.take();
    let exif = image.exif.take();
    let xmp = image.xmp.take();
    let swaps = geometry_swaps_axes(op, image.orientation);
    let shrunk = swaps.and_then(|_| image.pre_shrink.take());
    let mut out = apply_op(image, op)?;
    if let (Some((width, height)), Some(swaps)) = (shrunk, swaps) {
        out.pre_shrink = Some(if swaps {
            (height, width)
        } else {
            (width, height)
        });
    }
    out.gray |= gray;
    out.icc = out.icc.or(icc);
    out.exif = out.exif.or(exif);
    out.xmp = out.xmp.or(xmp);
    if matches!(op, Operation::AutoOrient) {
        // Pixels are upright now: retained EXIF must say so, or viewers rotate them twice.
        if let Some(exif) = out.exif.as_mut() {
            fastpkg_core::exif::set_orientation(exif, 1);
        }
    }
    Ok(out)
}

fn apply_op(mut image: Image, op: &Operation) -> Result<Image> {
    let preserves_wide = matches!(
        op,
        Operation::Resize { .. }
            | Operation::Canvas { .. }
            | Operation::Extend { .. }
            | Operation::Extract { .. }
            | Operation::Rotate { .. }
            | Operation::AutoOrient
            | Operation::Flip { .. }
            | Operation::Flop { .. }
            | Operation::Affine { .. }
    );
    if !preserves_wide {
        image.discard_wide();
    }
    match op {
        Operation::Resize {
            width,
            height,
            fit,
            position,
            background,
            kernel,
            without_enlargement,
            without_reduction,
            defer_canvas,
            ..
        } => resize(
            image,
            *width,
            *height,
            *fit,
            position,
            background,
            *kernel,
            *without_enlargement,
            *without_reduction,
            *defer_canvas,
        ),
        Operation::Canvas {
            width,
            height,
            fit,
            position,
            background,
        } => finish_canvas(image, *fit, *width, *height, position, background),
        Operation::Extend {
            width,
            height,
            background,
            position,
            extend_with,
            top,
            left,
            bottom,
            right,
        } => extend(
            image,
            *width,
            *height,
            [*top, *left, *bottom, *right],
            background,
            position,
            *extend_with,
        ),
        Operation::Extract {
            left,
            top,
            width,
            height,
        } => extract(image, *left, *top, *width, *height),
        Operation::Trim {
            background,
            threshold,
            line_art,
            margin,
        } => trim(image, background.as_deref(), *threshold, *line_art, *margin),
        Operation::Rotate { angle, background } => rotate(image, *angle, background),
        Operation::AutoOrient => auto_orient(image),
        Operation::Flip { flip: value } => flip(image, *value),
        Operation::Flop { flop: value } => flop(image, *value),
        Operation::Affine {
            matrix,
            idx,
            idy,
            odx,
            ody,
            background,
            interpolator,
        } => affine(
            image,
            matrix,
            *idx,
            *idy,
            *odx,
            *ody,
            background,
            interpolator.as_deref(),
        ),
        Operation::Sharpen {
            sigma,
            m1,
            m2,
            x1,
            y2,
            y3,
            simple,
        } => sharpen(image, *sigma, *m1, *m2, *x1, *y2, *y3, *simple),
        Operation::Median { size } => median(image, *size),
        Operation::Blur {
            sigma,
            min_amplitude,
            precision,
            mild,
        } => blur(image, *sigma, *min_amplitude, precision.as_deref(), *mild),
        Operation::Dilate { width } => morphology(image, *width, true),
        Operation::Erode { width } => morphology(image, *width, false),
        Operation::Flatten { background } => flatten(image, background),
        Operation::Unflatten => unflatten(image),
        Operation::Gamma {
            gamma: value,
            gamma_out,
        } => gamma(image, *value, *gamma_out),
        Operation::Negate { alpha } => negate(image, *alpha),
        Operation::Normalise { lower, upper } => normalise(image, *lower, *upper),
        Operation::Clahe {
            width,
            height,
            max_slope,
        } => clahe(image, *width, *height, *max_slope),
        Operation::Convolve {
            width,
            height,
            kernel,
            scale,
            offset,
        } => convolve(image, *width, *height, kernel, *scale, *offset),
        Operation::Threshold {
            threshold: value,
            grayscale,
        } => threshold(image, *value, *grayscale),
        Operation::Boolean {
            operand,
            operator,
            raw,
        } => boolean(image, operand, operator, raw.as_ref()),
        Operation::Linear { a, b } => linear(image, a.as_ref(), b.as_ref()),
        Operation::Recomb { matrix } => recomb(image, matrix),
        Operation::Modulate {
            brightness,
            saturation,
            hue,
            lightness,
        } => modulate(image, *brightness, *saturation, *hue, *lightness),
        Operation::Tint { tint: value } => tint(image, value),
        Operation::Greyscale | Operation::Grayscale => greyscale(image),
        Operation::RemoveAlpha => {
            let mut image = image;
            image.remove_alpha();
            Ok(image)
        }
        Operation::EnsureAlpha { alpha } => {
            let mut image = image;
            image.ensure_alpha((alpha.clamp(0.0, 1.0) * 255.0) as u8);
            Ok(image)
        }
        Operation::ExtractChannel { channel } => extract_channel(image, channel),
        Operation::JoinChannel { input, raw } => join_channel(image, input, raw.as_ref()),
        Operation::Bandbool { operator } => bandbool(image, operator),
        Operation::ReplaceColor {
            from,
            to,
            tolerance,
            softness,
            amount,
            preserve_opacity,
        } => replace_color(
            image,
            from,
            to,
            *tolerance,
            *softness,
            *amount,
            *preserve_opacity,
        ),
        Operation::Recolor {
            color,
            amount,
            preserve_opacity,
        } => recolor(image, color, *amount, *preserve_opacity),
        Operation::Duotone {
            shadows,
            highlights,
            amount,
            preserve_opacity,
        } => duotone(image, shadows, highlights, *amount, *preserve_opacity),
        Operation::GradientMap {
            gradient,
            amount,
            preserve_opacity,
        } => gradient_map(image, gradient, *amount, *preserve_opacity),
        Operation::GradientRecolor {
            gradient,
            amount,
            preserve_opacity,
        } => gradient_map(image, gradient, *amount, *preserve_opacity),
        Operation::GradientFill {
            gradient,
            amount,
            preserve_opacity,
        } => gradient_fill(image, gradient, *amount, *preserve_opacity),
        Operation::ApplyLut {
            data,
            strength,
            interpolation,
        } => apply_lut(image, data, *strength, interpolation.as_deref()),
        Operation::Curves {
            master,
            red,
            green,
            blue,
        } => curves(image, master, red, green, blue),
        Operation::Levels {
            input_black,
            input_white,
            gamma,
            output_black,
            output_white,
        } => levels(
            image,
            input_black.as_ref(),
            input_white.as_ref(),
            gamma.as_ref(),
            output_black.as_ref(),
            output_white.as_ref(),
        ),
        Operation::WhiteBalance { temperature, tint } => white_balance(image, *temperature, *tint),
        Operation::ColourBalance {
            shadows,
            midtones,
            highlights,
        } => colour_balance(image, *shadows, *midtones, *highlights),
        Operation::Posterize { levels } => posterize(image, *levels),
        Operation::Solarize { threshold } => solarize(image, *threshold),
        Operation::Fill {
            color,
            gradient,
            amount,
            preserve_opacity,
        } => match (color.as_deref(), gradient.as_ref()) {
            (Some(color), None) => fill_color(image, color, *amount, *preserve_opacity),
            (None, Some(gradient)) => gradient_fill(image, gradient, *amount, *preserve_opacity),
            _ => Err(FastpkgError::InvalidOptions(
                "fill requires exactly one of color or gradient".into(),
            )),
        },
        Operation::PaletteSwap {
            palette,
            dither,
            preserve_opacity,
        } => palette_swap(image, palette, *dither, *preserve_opacity),
        Operation::AlphaFromLuma {
            channel,
            invert,
            multiply,
            scale,
            offset,
        } => alpha_from_luma(image, channel, *invert, *multiply, *scale, *offset),
        Operation::Composite {
            input,
            blend,
            gravity,
            top,
            left,
            tile,
            density,
            premultiplied,
            source,
        } => composite(
            image,
            input,
            *blend,
            gravity,
            *top,
            *left,
            *tile,
            *density,
            *premultiplied,
            source.as_deref(),
        ),
    }
}

/// Pure data-movement transforms on the RGBA8 buffer, parallelised over output rows.
#[derive(Clone, Copy)]
enum Remap {
    FlipVertical,
    FlipHorizontal,
    Rotate90,
    Rotate180,
    Rotate270,
}

fn remap_pixels(mut image: Image, remap: Remap) -> Image {
    let w = image.width as usize;
    let h = image.height as usize;
    if w == 0 || h == 0 {
        return image;
    }
    let src = std::mem::take(&mut image.data);
    let mut out = vec![0u8; src.len()];
    match remap {
        Remap::FlipVertical => {
            out.par_chunks_exact_mut(w * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    let from = (h - 1 - y) * w * 4;
                    row.copy_from_slice(&src[from..from + w * 4]);
                });
        }
        Remap::FlipHorizontal | Remap::Rotate180 => {
            let vertical = matches!(remap, Remap::Rotate180);
            out.par_chunks_exact_mut(w * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    let sy = if vertical { h - 1 - y } else { y };
                    let from = &src[sy * w * 4..(sy + 1) * w * 4];
                    for (dst, px) in row.chunks_exact_mut(4).zip(from.chunks_exact(4).rev()) {
                        dst.copy_from_slice(px);
                    }
                });
        }
        Remap::Rotate90 | Remap::Rotate270 => {
            // Output is h wide and w tall. Work in bands of output rows so each source
            // row segment read is a few cache lines instead of one pixel.
            const BAND: usize = 16;
            let clockwise = matches!(remap, Remap::Rotate90);
            out.par_chunks_mut(BAND * h * 4)
                .enumerate()
                .for_each(|(band, chunk)| {
                    let first = band * BAND;
                    let rows = chunk.len() / (h * 4);
                    for c in 0..h {
                        // Source row feeding output column `c`.
                        let sy = if clockwise { h - 1 - c } else { c };
                        for r in 0..rows {
                            let out_row = first + r;
                            let sx = if clockwise { out_row } else { w - 1 - out_row };
                            let from = (sy * w + sx) * 4;
                            let to = (r * h + c) * 4;
                            chunk[to..to + 4].copy_from_slice(&src[from..from + 4]);
                        }
                    }
                });
            image.width = h as u32;
            image.height = w as u32;
        }
    }
    image.data = out;
    image
}

fn resize(
    mut image: Image,
    width: Option<u32>,
    height: Option<u32>,
    fit: Fit,
    position: &str,
    background: &str,
    kernel: Kernel,
    without_enlargement: bool,
    without_reduction: bool,
    defer_canvas: bool,
) -> Result<Image> {
    let (source_width, source_height) = image
        .pre_shrink
        .take()
        .unwrap_or((image.width, image.height));
    if width.is_none() && height.is_none() {
        return Ok(image);
    }
    let (rw, rh) = fastpkg_core::pipeline::compute_target(
        source_width,
        source_height,
        width,
        height,
        fit,
        without_enlargement,
        without_reduction,
    );
    let resized = resize_plain(image, rw, rh, kernel)?;
    if defer_canvas {
        return Ok(resized);
    }
    finish_canvas(resized, fit, width, height, position, background)
}

/// sharp's crop (`cover`) / embed (`contain`) step, applied to the already resampled image.
/// It runs after any flip/flop/rotate-by-90 that follows the resize in sharp's pipeline, which
/// is why it can be requested separately (`Operation::Canvas`).
fn finish_canvas(
    resized: Image,
    fit: Fit,
    width: Option<u32>,
    height: Option<u32>,
    position: &str,
    background: &str,
) -> Result<Image> {
    match fit {
        Fit::Cover => {
            let crop_w = width.map_or(resized.width, |w| resized.width.min(w));
            let crop_h = height.map_or(resized.height, |h| resized.height.min(h));
            if (crop_w, crop_h) == (resized.width, resized.height) {
                return Ok(resized);
            }
            let left = match horizontal(position) {
                Align::Start => 0,
                Align::Center => (resized.width - crop_w).div_ceil(2),
                Align::End => resized.width - crop_w,
            };
            let top = match vertical(position) {
                Align::Start => 0,
                Align::Center => (resized.height - crop_h).div_ceil(2),
                Align::End => resized.height - crop_h,
            };
            extract(resized, left, top, crop_w, crop_h)
        }
        Fit::Contain => {
            let canvas_w = width.map_or(resized.width, |w| resized.width.max(w));
            let canvas_h = height.map_or(resized.height, |h| resized.height.max(h));
            // sharp aligns the resized image against the *requested* size, so with
            // `withoutReduction` (resized image larger than the target) the offset is
            // negative and the image is cropped. Offsets use C integer division.
            let offset = |align: Align, target: Option<u32>, size: u32| -> i64 {
                let target = target.map_or(size as i64, i64::from);
                match align {
                    Align::Start => 0,
                    Align::Center => (target - size as i64) / 2,
                    Align::End => target - size as i64,
                }
            };
            let dx = offset(horizontal(position), width, resized.width);
            let dy = offset(vertical(position), height, resized.height);
            if (canvas_w, canvas_h) == (resized.width, resized.height) && dx == 0 && dy == 0 {
                return Ok(resized);
            }
            let (mut resized, mut dx, mut dy) = (resized, dx, dy);
            if dx < 0
                || dy < 0
                || dx + resized.width as i64 > canvas_w as i64
                || dy + resized.height as i64 > canvas_h as i64
            {
                let crop_left = (-dx).max(0) as u32;
                let crop_top = (-dy).max(0) as u32;
                let visible_w =
                    (resized.width - crop_left).min((canvas_w as i64 - dx.max(0)).max(1) as u32);
                let visible_h =
                    (resized.height - crop_top).min((canvas_h as i64 - dy.max(0)).max(1) as u32);
                resized = extract(resized, crop_left, crop_top, visible_w, visible_h)?;
                dx = dx.max(0);
                dy = dy.max(0);
            }
            let left = dx as u32;
            let top = dy as u32;
            let right = canvas_w.saturating_sub(left + resized.width);
            let bottom = canvas_h.saturating_sub(top + resized.height);
            extend(
                resized,
                None,
                None,
                [Some(top), Some(left), Some(bottom), Some(right)],
                background,
                position,
                ExtendWith::Background,
            )
        }
        _ => Ok(resized),
    }
}

fn resize_plain(image: Image, width: u32, height: u32, kernel: Kernel) -> Result<Image> {
    if width == image.width && height == image.height {
        return Ok(image);
    }
    let source_has_alpha = image.has_alpha;
    let mut out = resize_raw(image, width, height, kernel)?;
    out.has_alpha = source_has_alpha || out.has_transparency();
    Ok(out)
}

fn resize_raw(image: Image, width: u32, height: u32, kernel: Kernel) -> Result<Image> {
    resize_raw_opts(image, width, height, kernel, None)
}

fn resize_raw_opts(
    mut image: Image,
    width: u32,
    height: u32,
    kernel: Kernel,
    centering: Option<(f64, f64)>,
) -> Result<Image> {
    if image.has_wide() {
        return resize_raw_wide(image, width, height, kernel, centering);
    }
    let src = FirImage::from_vec_u8(
        image.width,
        image.height,
        std::mem::take(&mut image.data),
        PixelType::U8x4,
    )
    .map_err(|e| FastpkgError::Operation(e.to_string()))?;
    let mut dst = FirImage::new(width, height, PixelType::U8x4);
    let mut options = resize_options(kernel, image.has_alpha);
    if let Some(centering) = centering {
        options = options.fit_into_destination(Some(centering));
    }
    Resizer::new()
        .resize(&src, &mut dst, Some(&options))
        .map_err(|e| FastpkgError::Operation(e.to_string()))?;
    let mut out = Image::from_rgba(width, height, dst.into_vec())?;
    out.format = image.format;
    out.has_alpha = image.has_alpha;
    Ok(out)
}

fn resize_raw_wide(
    mut image: Image,
    width: u32,
    height: u32,
    kernel: Kernel,
    centering: Option<(f64, f64)>,
) -> Result<Image> {
    let wide = image.to_rgba16();
    let pixels = wide
        .chunks_exact(4)
        .map(|px| U16x4::new([px[0], px[1], px[2], px[3]]))
        .collect::<Vec<_>>();
    let src = TypedImage::from_pixels(image.width, image.height, pixels)
        .map_err(|e| FastpkgError::Operation(e.to_string()))?;
    let mut dst = TypedImage::<U16x4>::new(width, height);
    let mut options = resize_options(kernel, image.has_alpha);
    if let Some(centering) = centering {
        options = options.fit_into_destination(Some(centering));
    }
    Resizer::new()
        .resize_typed(&src, &mut dst, Some(&options))
        .map_err(|e| FastpkgError::Operation(e.to_string()))?;
    let out = dst.pixels().iter().flat_map(|px| px.0).collect::<Vec<_>>();
    image.set_wide_rgba(width, height, out)?;
    Ok(image)
}

fn custom_filter(name: &'static str, func: fn(f64) -> f64, support: f64) -> FirFilter {
    // The supports are positive constants, so construction cannot fail.
    FirFilter::Custom(
        fast_image_resize::Filter::new(name, func, support)
            .unwrap_or_else(|_| unreachable!("filter support is a positive constant")),
    )
}

/// Lanczos window with two lobes (libvips `lanczos2`).
fn lanczos2_filter(x: f64) -> f64 {
    let x = x.abs();
    if x == 0.0 {
        1.0
    } else if x >= 2.0 {
        0.0
    } else {
        let px = std::f64::consts::PI * x;
        2.0 * px.sin() * (px / 2.0).sin() / (px * px)
    }
}

/// Quadratic "magic kernel" `m` (Costella): the base of both Magic Kernel Sharp variants.
fn magic_kernel(x: f64) -> f64 {
    let x = x.abs();
    if x < 0.5 {
        0.75 - x * x
    } else if x < 1.5 {
        0.5 * (x - 1.5) * (x - 1.5)
    } else {
        0.0
    }
}

/// Magic Kernel Sharp 2013: `m` sharpened with the `[-1, 6, -1] / 4` filter (radius 2.5).
fn mks2013_filter(x: f64) -> f64 {
    (6.0 * magic_kernel(x) - magic_kernel(x + 1.0) - magic_kernel(x - 1.0)) / 4.0
}

/// Magic Kernel Sharp 2021: `m` convolved with the truncated (seven taps) inverse of its own
/// sampled response, which makes the kernel interpolating (radius 4.5).
fn mks2021_filter(x: f64) -> f64 {
    // g[n] = sqrt(2) * (2 * sqrt(2) - 3)^|n|
    const G: [f64; 4] = [
        std::f64::consts::SQRT_2,
        -0.242_640_687_119_284_88,
        0.041_630_560_342_615_74,
        -0.007_142_674_936_409_807,
    ];
    let mut sum = G[0] * magic_kernel(x);
    for (n, g) in G.iter().enumerate().skip(1) {
        sum += g * (magic_kernel(x - n as f64) + magic_kernel(x + n as f64));
    }
    sum
}

fn resize_options(kernel: Kernel, has_alpha: bool) -> ResizeOptions {
    let algorithm = match kernel {
        Kernel::Nearest => ResizeAlg::Nearest,
        Kernel::Linear | Kernel::Bilinear => ResizeAlg::Convolution(FirFilter::Bilinear),
        Kernel::Cubic => ResizeAlg::Convolution(FirFilter::CatmullRom),
        Kernel::Mitchell => ResizeAlg::Convolution(FirFilter::Mitchell),
        Kernel::Lanczos2 => ResizeAlg::Convolution(custom_filter("lanczos2", lanczos2_filter, 2.0)),
        Kernel::Lanczos3 => ResizeAlg::Convolution(FirFilter::Lanczos3),
        Kernel::Mks2013 => ResizeAlg::Convolution(custom_filter("mks2013", mks2013_filter, 2.5)),
        Kernel::Mks2021 => ResizeAlg::Convolution(custom_filter("mks2021", mks2021_filter, 4.5)),
    };
    ResizeOptions::new()
        .resize_alg(algorithm)
        .use_alpha(has_alpha)
}

#[derive(Clone, Copy)]
enum Align {
    Start,
    Center,
    End,
}

fn horizontal(position: &str) -> Align {
    let p = position.to_ascii_lowercase();
    if p.contains("left") || p.contains("west") {
        Align::Start
    } else if p.contains("right") || p.contains("east") {
        Align::End
    } else {
        Align::Center
    }
}

fn vertical(position: &str) -> Align {
    let p = position.to_ascii_lowercase();
    if p.contains("top") || p.contains("north") {
        Align::Start
    } else if p.contains("bottom") || p.contains("south") {
        Align::End
    } else {
        Align::Center
    }
}

fn extract(mut image: Image, left: u32, top: u32, width: u32, height: u32) -> Result<Image> {
    if width == 0 || height == 0 {
        return Err(FastpkgError::InvalidOptions(
            "extract size must be non-zero".into(),
        ));
    }
    if left as u64 + width as u64 > image.width as u64
        || top as u64 + height as u64 > image.height as u64
    {
        return Err(FastpkgError::InvalidOptions(
            "extract_area: bad extract area".into(),
        ));
    }
    if image.has_wide() {
        let wide = image.to_rgba16();
        let mut out = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            let start = ((top + y) as usize * image.width as usize + left as usize) * 4;
            out.extend_from_slice(&wide[start..start + width as usize * 4]);
        }
        image.set_wide_rgba(width, height, out)?;
        return Ok(image);
    }
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let start = ((top + y) as usize * image.width as usize + left as usize) * 4;
        data.extend_from_slice(&image.data[start..start + width as usize * 4]);
    }
    let mut out = Image::from_rgba(width, height, data)?;
    out.format = image.format;
    out.has_alpha = image.has_alpha;
    out.orientation = image.orientation;
    Ok(out)
}

fn extend(
    mut image: Image,
    width: Option<u32>,
    height: Option<u32>,
    padding: [Option<u32>; 4],
    background: &str,
    position: &str,
    extend_with: ExtendWith,
) -> Result<Image> {
    let [top, left, bottom, right] = padding;
    let padded = padding.iter().any(Option::is_some);
    let (width, height) = if padded {
        let grow = |edge: Option<u32>| edge.unwrap_or(0) as u64;
        let w = image.width as u64 + grow(left) + grow(right);
        let h = image.height as u64 + grow(top) + grow(bottom);
        if w > u32::MAX as u64 || h > u32::MAX as u64 {
            return Err(FastpkgError::InvalidOptions(
                "extend dimensions are too large".into(),
            ));
        }
        (w as u32, h as u32)
    } else {
        (
            width.unwrap_or(image.width).max(image.width),
            height.unwrap_or(image.height).max(image.height),
        )
    };
    if width == image.width && height == image.height {
        return Ok(image);
    }
    // Explicit edges pin the image at (left, top); otherwise `position` aligns it.
    let x = if padded {
        left.unwrap_or(0)
    } else {
        match horizontal(position) {
            Align::Start => 0,
            Align::Center => (width - image.width) / 2,
            Align::End => width - image.width,
        }
    };
    let y = if padded {
        top.unwrap_or(0)
    } else {
        match vertical(position) {
            Align::Start => 0,
            Align::Center => (height - image.height) / 2,
            Align::End => height - image.height,
        }
    };
    let mut color = Color::parse(background)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {background}")))?;
    if image.gray {
        // A single-channel image can only be extended with a grey background.
        let lum = luminance(color.r, color.g, color.b);
        (color.r, color.g, color.b) = (lum, lum, lum);
    }
    if image.has_wide() {
        let wide = image.to_rgba16();
        let fill = [
            color.r as u16 * 257,
            color.g as u16 * 257,
            color.b as u16 * 257,
            color.alpha as u16 * 257,
        ];
        let mut out = vec![0u16; width as usize * height as usize * 4];
        for px in out.chunks_exact_mut(4) {
            px.copy_from_slice(&fill);
        }
        if matches!(extend_with, ExtendWith::Background) {
            for row in 0..image.height {
                let src = row as usize * image.width as usize * 4;
                let dst = ((y + row) as usize * width as usize + x as usize) * 4;
                out[dst..dst + image.width as usize * 4]
                    .copy_from_slice(&wide[src..src + image.width as usize * 4]);
            }
        } else {
            for row in 0..height {
                for col in 0..width {
                    let rx = col as i64 - x as i64;
                    let ry = row as i64 - y as i64;
                    let (sx, sy) = match extend_with {
                        ExtendWith::Copy => (
                            rx.clamp(0, image.width as i64 - 1) as u32,
                            ry.clamp(0, image.height as i64 - 1) as u32,
                        ),
                        ExtendWith::Mirror => (
                            mirror_index(rx, image.width),
                            mirror_index(ry, image.height),
                        ),
                        _ => (
                            rx.rem_euclid(image.width as i64) as u32,
                            ry.rem_euclid(image.height as i64) as u32,
                        ),
                    };
                    let src = (sy as usize * image.width as usize + sx as usize) * 4;
                    let dst = (row as usize * width as usize + col as usize) * 4;
                    out[dst..dst + 4].copy_from_slice(&wide[src..src + 4]);
                }
            }
        }
        image.set_wide_rgba(width, height, out)?;
        image.has_alpha = image.has_alpha || color.alpha < 255;
        return Ok(image);
    }
    let mut data = vec![0u8; width as usize * height as usize * 4];
    let fill = [color.r, color.g, color.b, color.alpha];
    let src_stride = image.width as usize * 4;
    let (x, y) = (x as usize, y as usize);
    let row_bytes = width as usize * 4;
    if matches!(extend_with, ExtendWith::Background) {
        let template: Vec<u8> = fill.iter().copied().cycle().take(row_bytes).collect();
        data.par_chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(row_index, row)| {
                if row_index >= y && row_index < y + image.height as usize {
                    // Only the borders need the background; the middle is the source row.
                    let src = (row_index - y) * src_stride;
                    row[..x * 4].copy_from_slice(&template[..x * 4]);
                    row[x * 4..x * 4 + src_stride]
                        .copy_from_slice(&image.data[src..src + src_stride]);
                    row[x * 4 + src_stride..].copy_from_slice(&template[x * 4 + src_stride..]);
                } else {
                    row.copy_from_slice(&template);
                }
            });
    } else {
        let map = |index: i64, len: u32| -> usize {
            match extend_with {
                ExtendWith::Copy => index.clamp(0, len as i64 - 1) as usize,
                ExtendWith::Mirror => mirror_index(index, len) as usize,
                _ => index.rem_euclid(len as i64) as usize,
            }
        };
        let columns = (0..width as i64)
            .map(|col| map(col - x as i64, image.width))
            .collect::<Vec<_>>();
        data.par_chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(|(row_index, row)| {
                let sy = map(row_index as i64 - y as i64, image.height);
                let src_row = &image.data[sy * src_stride..(sy + 1) * src_stride];
                // The pixels over the source itself are a straight copy of the row.
                row[x * 4..x * 4 + src_stride].copy_from_slice(src_row);
                let (left, rest) = row.split_at_mut(x * 4);
                let right = &mut rest[src_stride..];
                for (px, &sx) in left.chunks_exact_mut(4).zip(&columns[..x]) {
                    px.copy_from_slice(&src_row[sx * 4..sx * 4 + 4]);
                }
                for (px, &sx) in right
                    .chunks_exact_mut(4)
                    .zip(&columns[x + image.width as usize..])
                {
                    px.copy_from_slice(&src_row[sx * 4..sx * 4 + 4]);
                }
            });
    }
    let mut out = Image::from_rgba(width, height, data)?;
    out.format = image.format;
    // A translucent background adds an alpha channel to an opaque image (sharp: 4 channels).
    out.has_alpha =
        image.has_alpha || (matches!(extend_with, ExtendWith::Background) && color.alpha < 255);
    Ok(out)
}

fn mirror_index(index: i64, len: u32) -> u32 {
    let len = len as i64;
    if len <= 1 {
        return 0;
    }
    let period = len * 2;
    let v = index.rem_euclid(period);
    if v < len {
        v as u32
    } else {
        (period - v - 1) as u32
    }
}

/// sharp `trim` (libvips `find_trim`): the image is flattened onto black, smoothed with a 3x3
/// median (unless `line_art`), and a pixel is content when any band differs from the background
/// by more than `threshold`. Nothing to trim (uniform image, threshold too high) returns the
/// image unchanged.
fn trim(
    image: Image,
    background: Option<&str>,
    threshold: f64,
    line_art: bool,
    margin: u32,
) -> Result<Image> {
    let (w, h) = (image.width as usize, image.height as usize);
    // Alpha-premultiplied ("flattened") colour of a pixel, in float.
    let flat = |px: &[u8]| -> [f32; 3] {
        let a = ALPHA_FACTOR[px[3] as usize];
        [px[0] as f32 * a, px[1] as f32 * a, px[2] as f32 * a]
    };
    // libvips trim runs a 3x3 median over the flattened image first (not for line art).
    let median_filter = !line_art && w > 1 && h > 1;
    // Planar flattened rows, one extra replicated pixel on each side: [channel][x + 1].
    let plane = w + 2;
    let flat_row = |y: usize, out: &mut [f32]| {
        for (x, px) in image.data[y * w * 4..(y + 1) * w * 4]
            .chunks_exact(4)
            .enumerate()
        {
            let value = flat(px);
            for c in 0..3 {
                out[c * plane + x + 1] = value[c];
            }
        }
        for c in 0..3 {
            out[c * plane] = out[c * plane + 1];
            out[c * plane + w + 1] = out[c * plane + w];
        }
    };
    let bg = match background {
        Some(text) => {
            let color = Color::parse(text)
                .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {text}")))?;
            flat(&[color.r, color.g, color.b, color.alpha])
        }
        None if w == 0 || h == 0 => [0.0; 3],
        None if !median_filter => flat(&image.data[..4]),
        None => {
            // The first (top-left) filtered pixel: rows -1 and 0 are both row 0.
            let mut row0 = vec![0f32; 3 * plane];
            let mut row1 = vec![0f32; 3 * plane];
            flat_row(0, &mut row0);
            flat_row(1, &mut row1);
            [0, 1, 2].map(|c| {
                let at = |row: &[f32], dx: usize| row[c * plane + dx];
                median9([
                    at(&row0, 0),
                    at(&row0, 1),
                    at(&row0, 2),
                    at(&row0, 0),
                    at(&row0, 1),
                    at(&row0, 2),
                    at(&row1, 0),
                    at(&row1, 1),
                    at(&row1, 2),
                ])
            })
        }
    };
    let differs = |v: [f32; 3]| -> bool {
        let diff = (v[0] - bg[0])
            .abs()
            .max((v[1] - bg[1]).abs())
            .max((v[2] - bg[2]).abs());
        f64::from(diff) > threshold
    };
    // Per block of rows: (first row with a differing pixel, last such row, left, right).
    const BLOCK: usize = 32;
    let blocks: Vec<usize> = (0..h).step_by(BLOCK).collect();
    let bounds = blocks
        .par_iter()
        .map(|&y0| {
            let y1 = (y0 + BLOCK).min(h);
            let lo = if median_filter {
                y0.saturating_sub(1)
            } else {
                y0
            };
            let hi = if median_filter { y1.min(h - 1) } else { y1 - 1 };
            let mut rows = vec![0f32; (hi - lo + 1) * 3 * plane];
            for (i, row) in rows.chunks_exact_mut(3 * plane).enumerate() {
                flat_row(lo + i, row);
            }
            let row_at = |y: usize| &rows[(y - lo) * 3 * plane..(y - lo + 1) * 3 * plane];
            let mut found: Option<(usize, usize, usize, usize)> = None;
            let mut filtered = vec![0f32; 3 * w];
            for y in y0..y1 {
                if median_filter {
                    let triple = [
                        row_at(y.saturating_sub(1)),
                        row_at(y),
                        row_at((y + 1).min(h - 1)),
                    ];
                    for c in 0..3 {
                        median_row(
                            [
                                &triple[0][c * plane..(c + 1) * plane],
                                &triple[1][c * plane..(c + 1) * plane],
                                &triple[2][c * plane..(c + 1) * plane],
                            ],
                            &mut filtered[c * w..(c + 1) * w],
                        );
                    }
                } else {
                    let row = row_at(y);
                    for c in 0..3 {
                        filtered[c * w..(c + 1) * w]
                            .copy_from_slice(&row[c * plane + 1..c * plane + 1 + w]);
                    }
                }
                let at = |x: usize| [filtered[x], filtered[w + x], filtered[2 * w + x]];
                let first = (0..w).find(|&x| differs(at(x)));
                if let Some(first) = first {
                    let last = (first..w).rev().find(|&x| differs(at(x))).unwrap_or(first);
                    found = Some(match found {
                        None => (y, y, first, last),
                        Some((top, _, left, right)) => (top, y, left.min(first), right.max(last)),
                    });
                }
            }
            found
        })
        .reduce(
            || None,
            |a, b| match (a, b) {
                (None, other) | (other, None) => other,
                (Some((t1, b1, l1, r1)), Some((t2, b2, l2, r2))) => {
                    Some((t1.min(t2), b1.max(b2), l1.min(l2), r1.max(r2)))
                }
            },
        );
    let Some((top, bottom, left, right)) = bounds else {
        return Ok(image);
    };
    let margin = margin as usize;
    let left = left.saturating_sub(margin);
    let top = top.saturating_sub(margin);
    let right = (right + margin).min(w - 1);
    let bottom = (bottom + margin).min(h - 1);
    extract(
        image,
        left as u32,
        top as u32,
        (right - left + 1) as u32,
        (bottom - top + 1) as u32,
    )
}

/// Median of nine values through a sorting network (exact: no arithmetic is involved).
#[inline]
fn median9(mut p: [f32; 9]) -> f32 {
    #[inline(always)]
    fn sort(p: &mut [f32; 9], a: usize, b: usize) {
        if p[a] > p[b] {
            p.swap(a, b);
        }
    }
    for (a, b) in MEDIAN9_NETWORK {
        sort(&mut p, a, b);
    }
    p[4]
}

/// 3x3 median of one channel for a whole row. `rows` are the three source rows (each with one
/// replicated pixel on both sides, so `rows[r][x..x + 3]` is the window of output `x`).
fn median_row(rows: [&[f32]; 3], out: &mut [f32]) {
    const L: usize = 8;
    #[inline(always)]
    fn sort(p: &mut [[f32; L]; 9], a: usize, b: usize) {
        for i in 0..L {
            let (x, y) = (p[a][i], p[b][i]);
            p[a][i] = x.min(y);
            p[b][i] = x.max(y);
        }
    }
    let w = out.len();
    let mut x = 0;
    while x < w {
        let n = L.min(w - x);
        let mut p = [[0f32; L]; 9];
        for (r, row) in rows.iter().enumerate() {
            for d in 0..3 {
                for i in 0..L {
                    // Lanes past the end repeat the last pixel; their results are discarded.
                    p[r * 3 + d][i] = row[(x + i).min(w - 1) + d];
                }
            }
        }
        for (a, b) in MEDIAN9_NETWORK {
            sort(&mut p, a, b);
        }
        out[x..x + n].copy_from_slice(&p[4][..n]);
        x += n;
    }
}

const MEDIAN9_NETWORK: [(usize, usize); 19] = [
    (1, 2),
    (4, 5),
    (7, 8),
    (0, 1),
    (3, 4),
    (6, 7),
    (1, 2),
    (4, 5),
    (7, 8),
    (0, 3),
    (5, 8),
    (4, 7),
    (3, 6),
    (1, 4),
    (2, 5),
    (4, 7),
    (4, 2),
    (6, 4),
    (4, 2),
];

fn flip(mut image: Image, flip: bool) -> Result<Image> {
    if !flip {
        return Ok(image);
    }
    if image.has_wide() {
        let wide = image.to_rgba16();
        let stride = image.width as usize * 4;
        let mut out = vec![0u16; wide.len()];
        for y in 0..image.height as usize {
            let src = (image.height as usize - 1 - y) * stride;
            let dst = y * stride;
            out[dst..dst + stride].copy_from_slice(&wide[src..src + stride]);
        }
        image.set_wide_rgba(image.width, image.height, out)?;
        return Ok(image);
    }
    Ok(remap_pixels(image, Remap::FlipVertical))
}

fn flop(mut image: Image, flop: bool) -> Result<Image> {
    if !flop {
        return Ok(image);
    }
    if image.has_wide() {
        let wide = image.to_rgba16();
        let stride = image.width as usize * 4;
        let mut out = vec![0u16; wide.len()];
        for (y, row) in out.chunks_exact_mut(stride).enumerate() {
            let src = &wide[y * stride..(y + 1) * stride];
            for (x, dst) in row.chunks_exact_mut(4).enumerate() {
                let sx = (image.width as usize - 1 - x) * 4;
                dst.copy_from_slice(&src[sx..sx + 4]);
            }
        }
        image.set_wide_rgba(image.width, image.height, out)?;
        return Ok(image);
    }
    Ok(remap_pixels(image, Remap::FlipHorizontal))
}

fn rotate(image: Image, angle: f64, background: &str) -> Result<Image> {
    let mut angle = angle % 360.0;
    if angle < 0.0 {
        angle += 360.0;
    }
    if angle == 0.0 {
        return Ok(image);
    }
    if image.has_wide() {
        return rotate_wide(image, angle, background);
    }
    if (angle - 90.0).abs() < f64::EPSILON {
        return Ok(remap_pixels(image, Remap::Rotate90));
    }
    if (angle - 180.0).abs() < f64::EPSILON {
        return Ok(remap_pixels(image, Remap::Rotate180));
    }
    if (angle - 270.0).abs() < f64::EPSILON {
        return Ok(remap_pixels(image, Remap::Rotate270));
    }
    let bg = Color::parse(background).unwrap_or(Color::BLACK);
    let (cos, sin, new_w, new_h, left, top) = rotation_frame(image.width, image.height, angle);
    let mut out = Image::from_rgba(new_w, new_h, vec![0u8; new_w as usize * new_h as usize * 4])?;
    out.has_alpha = image.has_alpha || bg.alpha < 255;
    let with_alpha = out.has_alpha;
    let fill = if with_alpha {
        premultiplied(bg)
    } else {
        [bg.r as f32, bg.g as f32, bg.b as f32, bg.alpha as f32]
    };
    out.data
        .par_chunks_exact_mut(new_w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, dst) in row.chunks_exact_mut(4).enumerate() {
                let (sx, sy) = rotation_source(cos, sin, left, top, x as u32, y as u32);
                dst.copy_from_slice(&sample_pixel(
                    &image,
                    sx,
                    sy,
                    Interpolator::Bilinear,
                    fill,
                    with_alpha,
                ));
            }
        });
    out.format = image.format;
    Ok(out)
}

/// Output frame of an arbitrary-angle rotation, following libvips: the size is the rounded
/// extent of the rotated source rectangle and its origin is the rounded top-left corner of
/// that rectangle. Returns `(cos, sin, width, height, left, top)`.
fn rotation_frame(width: u32, height: u32, angle: f64) -> (f64, f64, u32, u32, f64, f64) {
    let (sin, cos) = angle.to_radians().sin_cos();
    let (w, h) = (width as f64, height as f64);
    let xs = [0.0, w * cos, -h * sin, w * cos - h * sin];
    let ys = [0.0, w * sin, h * cos, w * sin + h * cos];
    let min = |values: &[f64; 4]| values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = |values: &[f64; 4]| values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let new_w = ((max(&xs) - min(&xs)).round() as u32).max(1);
    let new_h = ((max(&ys) - min(&ys)).round() as u32).max(1);
    (cos, sin, new_w, new_h, min(&xs).round(), min(&ys).round())
}

/// Source coordinate (pixel-index space) sampled by output pixel `(x, y)`.
fn rotation_source(cos: f64, sin: f64, left: f64, top: f64, x: u32, y: u32) -> (f64, f64) {
    let (ox, oy) = (x as f64 + left, y as f64 + top);
    (cos * ox + sin * oy, -sin * ox + cos * oy)
}

fn rotate_wide(mut image: Image, angle: f64, background: &str) -> Result<Image> {
    let wide = image.to_rgba16();
    let width = image.width as usize;
    let height = image.height as usize;
    if (angle - 90.0).abs() < f64::EPSILON {
        let mut out = vec![0u16; wide.len()];
        for y in 0..height {
            for x in 0..width {
                let src = (y * width + x) * 4;
                let dst = (x * height + (height - 1 - y)) * 4;
                out[dst..dst + 4].copy_from_slice(&wide[src..src + 4]);
            }
        }
        image.set_wide_rgba(height as u32, width as u32, out)?;
        return Ok(image);
    }
    if (angle - 180.0).abs() < f64::EPSILON {
        let mut out = vec![0u16; wide.len()];
        for y in 0..height {
            for x in 0..width {
                let src = (y * width + x) * 4;
                let dst = ((height - 1 - y) * width + (width - 1 - x)) * 4;
                out[dst..dst + 4].copy_from_slice(&wide[src..src + 4]);
            }
        }
        image.set_wide_rgba(width as u32, height as u32, out)?;
        return Ok(image);
    }
    if (angle - 270.0).abs() < f64::EPSILON {
        let mut out = vec![0u16; wide.len()];
        for y in 0..height {
            for x in 0..width {
                let src = (y * width + x) * 4;
                let dst = ((width - 1 - x) * height + y) * 4;
                out[dst..dst + 4].copy_from_slice(&wide[src..src + 4]);
            }
        }
        image.set_wide_rgba(height as u32, width as u32, out)?;
        return Ok(image);
    }
    let bg = Color::parse(background).unwrap_or(Color::BLACK);
    let fallback = [
        bg.r as u16 * 257,
        bg.g as u16 * 257,
        bg.b as u16 * 257,
        bg.alpha as u16 * 257,
    ];
    let (cos, sin, new_w, new_h, left, top) = rotation_frame(image.width, image.height, angle);
    let mut out = vec![0u16; new_w as usize * new_h as usize * 4];
    for y in 0..new_h {
        for x in 0..new_w {
            let (sx, sy) = rotation_source(cos, sin, left, top, x, y);
            let pixel = sample_bilinear_wide(&wide, width, height, sx, sy, fallback);
            let dst = (y as usize * new_w as usize + x as usize) * 4;
            out[dst..dst + 4].copy_from_slice(&pixel);
        }
    }
    image.set_wide_rgba(new_w, new_h, out)?;
    image.has_alpha = image.has_alpha || bg.alpha < 255;
    Ok(image)
}

/// Bilinear sample at source position `(x, y)` (pixel-index space). Like libvips' `affine`, taps
/// that fall outside the image read the background colour, so the edge of the image blends into
/// the background by coverage instead of ending in a hard line.
fn sample_bilinear_wide(
    wide: &[u16],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
    fallback: [u16; 4],
) -> [u16; 4] {
    if x <= -1.0 || y <= -1.0 || x >= width as f64 || y >= height as f64 {
        return fallback;
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let at = |px: i64, py: i64| -> [f64; 4] {
        if px < 0 || py < 0 || px >= width as i64 || py >= height as i64 {
            return fallback.map(|v| v as f64);
        }
        let idx = (py as usize * width + px as usize) * 4;
        [
            wide[idx] as f64,
            wide[idx + 1] as f64,
            wide[idx + 2] as f64,
            wide[idx + 3] as f64,
        ]
    };
    let c00 = at(x0, y0);
    let c10 = at(x0 + 1, y0);
    let c01 = at(x0, y0 + 1);
    let c11 = at(x0 + 1, y0 + 1);
    let mut out = [0u16; 4];
    for c in 0..4 {
        let top = c00[c] + (c10[c] - c00[c]) * fx;
        let bottom = c01[c] + (c11[c] - c01[c]) * fx;
        out[c] = (top + (bottom - top) * fy).clamp(0.0, 65_535.0).round() as u16;
    }
    out
}

/// `alpha / 255.0` in `f32` for every alpha value.
const ALPHA_FACTOR: [f32; 256] = {
    let mut table = [0.0; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = i as f32 / 255.0;
        i += 1;
    }
    table
};

/// Premultiplied float colour as libvips computes it: `colour * (alpha / 255)` in `f32`.
#[inline(always)]
fn premultiplied(color: Color) -> [f32; 4] {
    let factor = ALPHA_FACTOR[color.alpha as usize];
    [
        color.r as f32 * factor,
        color.g as f32 * factor,
        color.b as f32 * factor,
        color.alpha as f32,
    ]
}

/// `x.floor() as i64` without the libm call (valid for |x| < 2^62).
#[inline(always)]
fn floor_i64(x: f64) -> i64 {
    let truncated = x as i64;
    truncated - i64::from((truncated as f64) > x)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interpolator {
    Nearest,
    Bilinear,
    Bicubic,
}

impl Interpolator {
    /// `nohalo`, `lbb` and `vsqbs` are approximated by bilinear.
    fn parse(name: Option<&str>) -> Self {
        match name {
            Some("nearest") => Interpolator::Nearest,
            Some("bicubic") => Interpolator::Bicubic,
            _ => Interpolator::Bilinear,
        }
    }
}

/// Catmull-Rom weights for the four taps `floor(x) - 1 ..= floor(x) + 2` (libvips `bicubic`).
#[inline]
fn catmull_rom(t: f64) -> [f64; 4] {
    [
        ((-0.5 * t + 1.0) * t - 0.5) * t,
        (1.5 * t - 2.5) * t * t + 1.0,
        ((-1.5 * t + 2.0) * t + 0.5) * t,
        (0.5 * t - 0.5) * t * t,
    ]
}

/// Samples the image at source position `(x, y)` (pixel-index space) the way libvips' `affine`
/// does: taps outside the image read the background, so the edge blends into it by coverage.
/// Images with alpha are premultiplied, interpolated in float, unpremultiplied
/// (`value * (255 / alpha)`, zero for alpha 0) and truncated to 8 bits; opaque images are
/// interpolated directly and rounded. `background` is premultiplied for images with alpha.
fn sample_pixel(
    image: &Image,
    x: f64,
    y: f64,
    interpolator: Interpolator,
    background: [f32; 4],
    with_alpha: bool,
) -> [u8; 4] {
    let (w, h) = (image.width as i64, image.height as i64);
    let tap = |px: i64, py: i64| -> [f32; 4] {
        if px < 0 || py < 0 || px >= w || py >= h {
            return background;
        }
        let idx = (py as usize * w as usize + px as usize) * 4;
        let color = &image.data[idx..idx + 4];
        if with_alpha {
            premultiplied(Color {
                r: color[0],
                g: color[1],
                b: color[2],
                alpha: color[3],
            })
        } else {
            [
                color[0] as f32,
                color[1] as f32,
                color[2] as f32,
                color[3] as f32,
            ]
        }
    };
    let far = 3.0;
    let blended = if x < -far || y < -far || x > w as f64 + far || y > h as f64 + far {
        background
    } else {
        match interpolator {
            Interpolator::Nearest => tap(floor_i64(x), floor_i64(y)),
            Interpolator::Bilinear => {
                let (x0, y0) = (floor_i64(x), floor_i64(y));
                let (fx, fy) = (x - x0 as f64, y - y0 as f64);
                let c00 = tap(x0, y0);
                let c10 = tap(x0 + 1, y0);
                let c01 = tap(x0, y0 + 1);
                let c11 = tap(x0 + 1, y0 + 1);
                let mut out = [0f32; 4];
                for c in 0..4 {
                    let top = c00[c] as f64 + (c10[c] as f64 - c00[c] as f64) * fx;
                    let bottom = c01[c] as f64 + (c11[c] as f64 - c01[c] as f64) * fx;
                    out[c] = (top + (bottom - top) * fy) as f32;
                }
                out
            }
            Interpolator::Bicubic => {
                let (x0, y0) = (floor_i64(x), floor_i64(y));
                let wx = catmull_rom(x - x0 as f64);
                let wy = catmull_rom(y - y0 as f64);
                let mut acc = [0f64; 4];
                for (j, &weight_y) in wy.iter().enumerate() {
                    let mut row = [0f64; 4];
                    for (i, &weight_x) in wx.iter().enumerate() {
                        let value = tap(x0 - 1 + i as i64, y0 - 1 + j as i64);
                        for c in 0..4 {
                            row[c] += value[c] as f64 * weight_x;
                        }
                    }
                    for c in 0..4 {
                        acc[c] += row[c] * weight_y;
                    }
                }
                acc.map(|v| v as f32)
            }
        }
    };
    if with_alpha {
        let alpha = blended[3];
        let factor = if alpha == 0.0 { 0.0 } else { 255.0 / alpha };
        [
            (blended[0] * factor).clamp(0.0, 255.0) as u8,
            (blended[1] * factor).clamp(0.0, 255.0) as u8,
            (blended[2] * factor).clamp(0.0, 255.0) as u8,
            alpha.clamp(0.0, 255.0) as u8,
        ]
    } else {
        blended.map(round_f32_u8)
    }
}

/// `v.clamp(0.0, 255.0).round() as u8` without the libm `round` call.
#[inline(always)]
fn round_f32_u8(v: f32) -> u8 {
    let v = v.clamp(0.0, 255.0);
    let whole = v as u32;
    (whole + u32::from(v - whole as f32 >= 0.5)) as u8
}

fn auto_orient(image: Image) -> Result<Image> {
    let mut image = image;
    let orientation = image.orientation.unwrap_or(1);
    image = match orientation {
        2 => flop(image, true)?,
        3 => rotate(image, 180.0, "#000000")?,
        4 => flip(image, true)?,
        // 5 = transpose (mirror over the main diagonal): rotate 90 clockwise, then flop.
        5 => {
            let image = rotate(image, 90.0, "#000000")?;
            flop(image, true)?
        }
        6 => rotate(image, 90.0, "#000000")?,
        // 7 = transverse (mirror over the anti-diagonal): rotate 90 clockwise, then flip.
        7 => {
            let image = rotate(image, 90.0, "#000000")?;
            flip(image, true)?
        }
        8 => rotate(image, 270.0, "#000000")?,
        _ => image,
    };
    image.orientation = None;
    Ok(image)
}

fn affine(
    mut image: Image,
    matrix: &[f64],
    idx: f64,
    idy: f64,
    odx: f64,
    ody: f64,
    background: &str,
    interpolator: Option<&str>,
) -> Result<Image> {
    if matrix.len() != 4 && matrix.len() != 6 {
        return Err(FastpkgError::InvalidOptions(
            "affine matrix must have 4 or 6 values".into(),
        ));
    }
    let (a, b, c, d) = (matrix[0], matrix[1], matrix[2], matrix[3]);
    let det = a * d - b * c;
    if det.abs() < 1e-12 {
        return Err(FastpkgError::InvalidOptions(
            "affine matrix is not invertible".into(),
        ));
    }
    let inv_a = d / det;
    let inv_b = -b / det;
    let inv_c = -c / det;
    let inv_d = a / det;
    let corners = [
        (0.0, 0.0),
        (image.width as f64, 0.0),
        (0.0, image.height as f64),
        (image.width as f64, image.height as f64),
    ];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in corners {
        // The output frame ignores the offsets: they move the content within it.
        let ox = a * x + b * y;
        let oy = c * x + d * y;
        min_x = min_x.min(ox);
        min_y = min_y.min(oy);
        max_x = max_x.max(ox);
        max_y = max_y.max(oy);
    }
    // libvips sizes the output as the rounded extent of the transformed bounding box.
    let width = (max_x - min_x).round().max(1.0) as u32;
    let height = (max_y - min_y).round().max(1.0) as u32;
    // libvips places the output origin on the rounded top-left corner of the bounding box.
    let (origin_x, origin_y) = (min_x.round(), min_y.round());
    let bg = Color::parse(background).unwrap_or(Color::BLACK);
    if image.has_wide() {
        let wide = image.to_rgba16();
        let fallback = [
            bg.r as u16 * 257,
            bg.g as u16 * 257,
            bg.b as u16 * 257,
            bg.alpha as u16 * 257,
        ];
        let mut out = vec![0u16; width as usize * height as usize * 4];
        for y in 0..height {
            for x in 0..width {
                let ox = x as f64 + origin_x;
                let oy = y as f64 + origin_y;
                let dx = ox - odx;
                let dy = oy - ody;
                let sx = inv_a * dx + inv_b * dy - idx;
                let sy = inv_c * dx + inv_d * dy - idy;
                let pixel = sample_bilinear_wide(
                    &wide,
                    image.width as usize,
                    image.height as usize,
                    sx,
                    sy,
                    fallback,
                );
                let dst = (y as usize * width as usize + x as usize) * 4;
                out[dst..dst + 4].copy_from_slice(&pixel);
            }
        }
        image.set_wide_rgba(width, height, out)?;
        image.has_alpha = image.has_alpha || bg.alpha < 255;
        return Ok(image);
    }
    let mut out = Image::from_rgba(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    )?;
    out.has_alpha = image.has_alpha || bg.alpha < 255;
    let with_alpha = out.has_alpha;
    let fill = if with_alpha {
        premultiplied(bg)
    } else {
        [bg.r as f32, bg.g as f32, bg.b as f32, bg.alpha as f32]
    };
    let interpolator = Interpolator::parse(interpolator);
    out.data
        .par_chunks_exact_mut(width as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, dst) in row.chunks_exact_mut(4).enumerate() {
                let ox = x as f64 + origin_x;
                let oy = y as f64 + origin_y;
                let dx = ox - odx;
                let dy = oy - ody;
                let sx = inv_a * dx + inv_b * dy - idx;
                let sy = inv_c * dx + inv_d * dy - idy;
                dst.copy_from_slice(&sample_pixel(
                    &image,
                    sx,
                    sy,
                    interpolator,
                    fill,
                    with_alpha,
                ));
            }
        });
    out.format = image.format;
    Ok(out)
}

fn sharpen(
    mut image: Image,
    sigma: f64,
    m1: f64,
    m2: f64,
    x1: f64,
    y2: f64,
    y3: f64,
    simple: bool,
) -> Result<Image> {
    let (w, h) = (image.width as usize, image.height as usize);
    let has_alpha = image.has_alpha;
    // sharp premultiplies RGBA before sharpening and unpremultiplies afterwards.
    let mut data = std::mem::take(&mut image.data);
    if has_alpha {
        premultiply(&mut data);
    }
    if simple {
        // Fast, mild sharpen: a 3x3 integer convolution.
        data = gauss::conv3x3(&data, w, h, [-1, -1, -1, -1, 32, -1, -1, -1, -1], 24);
    } else {
        sharpen::sharpen(
            &mut data,
            w,
            h,
            sigma,
            (
                m1.max(0.0),
                m2.max(0.0),
                x1.max(0.0),
                y2.max(0.0),
                y3.max(0.0),
            ),
        );
    }
    if has_alpha {
        unpremultiply(&mut data);
    }
    let mut image = image;
    image.data = data;
    image.wide = None;
    Ok(image)
}

fn blur(
    image: Image,
    sigma: f64,
    min_ampl: Option<f64>,
    precision: Option<&str>,
    mild: bool,
) -> Result<Image> {
    let (w, h) = (image.width as usize, image.height as usize);
    let has_alpha = image.has_alpha;
    // sharp premultiplies RGBA before blurring and unpremultiplies afterwards.
    let mut data = image.data;
    if has_alpha {
        premultiply(&mut data);
    }
    let mut out = if mild {
        gauss::conv3x3(&data, w, h, [1; 9], 9)
    } else {
        gauss::gaussblur(
            data,
            w,
            h,
            sigma,
            min_ampl.unwrap_or(0.2),
            gauss::Precision::parse(precision),
        )
    };
    if has_alpha {
        unpremultiply(&mut out);
    }
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = has_alpha;
    Ok(out_image)
}

/// libvips `premultiply().cast(uchar)`: colour * alpha / 255, truncated.
fn premultiply(data: &mut [u8]) {
    data.par_chunks_mut(4096).for_each(|chunk| {
        for px in chunk.chunks_exact_mut(4) {
            let a = px[3] as u32;
            for c in &mut px[..3] {
                *c = (*c as u32 * a / 255) as u8;
            }
        }
    });
}

/// `ceil(2^32 / a)` for every alpha (0 for alpha 0): `n / a == (n * RECIPROCAL[a]) >> 32` for
/// every `n < 2^16`, which replaces a hardware division per channel.
const RECIPROCAL: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut a = 1;
    while a < 256 {
        table[a] = (1u64 << 32).div_ceil(a as u64);
        a += 1;
    }
    table
};

/// libvips `unpremultiply().cast(uchar)`: colour * 255 / alpha, truncated; alpha 0 gives 0.
fn unpremultiply(data: &mut [u8]) {
    data.par_chunks_mut(4096).for_each(|chunk| {
        for px in chunk.chunks_exact_mut(4) {
            let reciprocal = RECIPROCAL[px[3] as usize];
            for c in &mut px[..3] {
                *c = (((*c as u64 * 255 * reciprocal) >> 32).min(255)) as u8;
            }
        }
    });
}

fn median(image: Image, size: u32) -> Result<Image> {
    let size = size.max(1);
    if size > image.width || size > image.height {
        return Err(FastpkgError::Operation("rank: window too large".into()));
    }
    if size == 1 {
        return Ok(image);
    }
    let radius = (size / 2) as i32;
    let w = image.width as usize;
    let h = image.height as i32;
    let side = (2 * radius + 1) as usize;
    // Rank of the median in the sorted window (window includes edge-clamped duplicates).
    let rank = (side * side / 2) as u32;
    let mut out = vec![0u8; image.data.len()];
    // Sliding-window histogram per row and channel: O(window side) per pixel instead of
    // gathering and sorting the whole window. A 16-bin coarse histogram speeds up the rank scan.
    out.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let rows: Vec<&[u8]> = (-radius..=radius)
                .map(|dy| {
                    let sy = (y as i32 + dy).clamp(0, h - 1) as usize;
                    &image.data[sy * w * 4..(sy + 1) * w * 4]
                })
                .collect();
            for c in 0..4 {
                let mut fine = [0u32; 256];
                let mut coarse = [0u32; 16];
                let column = |x: i32| (x.clamp(0, w as i32 - 1) as usize) * 4 + c;
                for dx in -radius..=radius {
                    let at = column(dx);
                    for row in &rows {
                        let v = row[at] as usize;
                        fine[v] += 1;
                        coarse[v >> 4] += 1;
                    }
                }
                for x in 0..w {
                    let mut seen = 0u32;
                    let mut bucket = 0usize;
                    while seen + coarse[bucket] <= rank {
                        seen += coarse[bucket];
                        bucket += 1;
                    }
                    let mut v = bucket << 4;
                    while seen + fine[v] <= rank {
                        seen += fine[v];
                        v += 1;
                    }
                    out_row[x * 4 + c] = v as u8;
                    let leaving = column(x as i32 - radius);
                    let entering = column(x as i32 + radius + 1);
                    for row in &rows {
                        let old = row[leaving] as usize;
                        fine[old] -= 1;
                        coarse[old >> 4] -= 1;
                        let new = row[entering] as usize;
                        fine[new] += 1;
                        coarse[new >> 4] += 1;
                    }
                }
            }
        });
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = image.has_alpha;
    Ok(out_image)
}

/// `dst[i] = max(dst[i], src[i])` (or `min`), which compiles to `pmaxub`/`pminub`.
#[inline(always)]
fn fold_extreme(dst: &mut [u8], src: &[u8], dilate: bool) {
    if dilate {
        for (a, &b) in dst.iter_mut().zip(src) {
            *a = (*a).max(b);
        }
    } else {
        for (a, &b) in dst.iter_mut().zip(src) {
            *a = (*a).min(b);
        }
    }
}

/// Copies the alpha byte of every RGBA pixel from `source` into `target`.
#[inline(always)]
fn restore_alpha(target: &mut [u8], source: &[u8]) {
    for (t, s) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
        let word = (u32::from_le_bytes([t[0], t[1], t[2], t[3]]) & 0x00FF_FFFF)
            | (u32::from_le_bytes([s[0], s[1], s[2], s[3]]) & 0xFF00_0000);
        t.copy_from_slice(&word.to_le_bytes());
    }
}

/// `dst[i] = extreme of padded[i + k * stride]` for `k` in `0..count`. Wide windows use
/// power-of-two partial results so the cost grows with `log(count)`.
fn window_extreme(
    dst: &mut [u8],
    padded: &[u8],
    count: usize,
    stride: usize,
    dilate: bool,
    scratch: &mut [Vec<u8>; 2],
) {
    let n = dst.len();
    if count <= 16 {
        dst.copy_from_slice(&padded[..n]);
        for k in 1..count {
            fold_extreme(dst, &padded[k * stride..k * stride + n], dilate);
        }
        return;
    }
    let mut span = 1;
    let mut len = padded.len();
    scratch[0].clear();
    scratch[0].extend_from_slice(padded);
    let mut current = 0;
    while span * 2 <= count {
        let shift = span * stride;
        len -= shift;
        let (head, tail) = scratch.split_at_mut(1);
        let (source, target) = if current == 0 {
            (&head[0], &mut tail[0])
        } else {
            (&tail[0], &mut head[0])
        };
        target.clear();
        target.extend_from_slice(&source[..len]);
        fold_extreme(target, &source[shift..shift + len], dilate);
        current ^= 1;
        span *= 2;
    }
    let source = &scratch[current];
    dst.copy_from_slice(&source[..n]);
    let offset = (count - span) * stride;
    fold_extreme(dst, &source[offset..offset + n], dilate);
}

fn morphology(image: Image, width: u32, dilate: bool) -> Result<Image> {
    let radius = width.max(1) as usize;
    let w = image.width as usize;
    let h = image.height as usize;
    let row_len = w * 4;
    let mut image = image;
    if w == 0 || h == 0 {
        return Ok(image);
    }
    let mut data = std::mem::take(&mut image.data);
    let mut horizontal = vec![0u8; data.len()];
    // Horizontal pass: a window of 2 * radius + 1 pixels with the edge pixels replicated. All four
    // bytes are folded together; alpha is put back from the source afterwards.
    horizontal
        .par_chunks_exact_mut(row_len)
        .zip(data.par_chunks_exact(row_len))
        .for_each_init(
            || (Vec::<u8>::new(), [Vec::<u8>::new(), Vec::<u8>::new()]),
            |(padded, scratch), (dst, src)| {
                padded.clear();
                for _ in 0..radius {
                    padded.extend_from_slice(&src[..4]);
                }
                padded.extend_from_slice(src);
                for _ in 0..radius {
                    padded.extend_from_slice(&src[row_len - 4..]);
                }
                window_extreme(dst, padded, 2 * radius + 1, 4, dilate, scratch);
                restore_alpha(dst, src);
            },
        );
    // Vertical pass, written over the source buffer.
    data.par_chunks_exact_mut(row_len)
        .enumerate()
        .for_each(|(y, dst)| {
            let row = |d: usize| {
                let sy = (y + d).saturating_sub(radius).min(h - 1);
                &horizontal[sy * row_len..(sy + 1) * row_len]
            };
            dst.copy_from_slice(row(0));
            for d in 1..=2 * radius {
                fold_extreme(dst, row(d), dilate);
            }
            restore_alpha(dst, &horizontal[y * row_len..(y + 1) * row_len]);
        });
    image.data = data;
    image.wide = None;
    Ok(image)
}

fn flatten(mut image: Image, background: &str) -> Result<Image> {
    let bg = Color::parse(background)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {background}")))?;
    image.flatten(bg);
    Ok(image)
}

fn unflatten(mut image: Image) -> Result<Image> {
    image.ensure_alpha(255);
    for px in image.data.chunks_exact_mut(4) {
        if px[0] == 255 && px[1] == 255 && px[2] == 255 {
            px[3] = 0;
        }
    }
    image.has_alpha = true;
    Ok(image)
}

/// libvips `gamma` quantises to 8 bits twice: sharp applies `x^gamma` first and
/// `x^(1/gammaOut)` last (both truncating, with the float32 scale libvips uses), so even
/// `gamma(g)` with the default `gammaOut` is visibly lossy.
fn gamma(mut image: Image, gamma: f64, gamma_out: Option<f64>) -> Result<Image> {
    let gamma = gamma.clamp(1.0, 3.0);
    let gamma_out = gamma_out.unwrap_or(gamma).clamp(1.0, 3.0);
    let stage = |v: f64, p: f64| -> f64 {
        let scale = (255.0 / 255f64.powf(p)) as f32;
        (v.powf(p) * scale as f64) as f32 as f64
    };
    let mut lut = [0u8; 256];
    for (i, out) in lut.iter_mut().enumerate() {
        let first = stage(i as f64, gamma).trunc().clamp(0.0, 255.0);
        *out = stage(first, 1.0 / gamma_out).trunc().clamp(0.0, 255.0) as u8;
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = lut[px[0] as usize];
        px[1] = lut[px[1] as usize];
        px[2] = lut[px[2] as usize];
    });
    Ok(image)
}

fn negate(mut image: Image, alpha: bool) -> Result<Image> {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        unsafe { negate_avx2(&mut image.data, alpha) };
        return Ok(image);
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
        if alpha {
            px[3] = 255 - px[3];
        }
    });
    Ok(image)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn negate_avx2(data: &mut [u8], alpha: bool) {
    use std::arch::x86_64::*;
    let mut mask = [0u8; 32];
    for pixel in 0..8 {
        mask[pixel * 4] = 255;
        mask[pixel * 4 + 1] = 255;
        mask[pixel * 4 + 2] = 255;
        if alpha {
            mask[pixel * 4 + 3] = 255;
        }
    }
    let mask = _mm256_loadu_si256(mask.as_ptr() as *const __m256i);
    let mut offset = 0;
    while offset + 32 <= data.len() {
        let value = _mm256_loadu_si256(data.as_ptr().add(offset) as *const __m256i);
        _mm256_storeu_si256(
            data.as_mut_ptr().add(offset) as *mut __m256i,
            _mm256_xor_si256(value, mask),
        );
        offset += 32;
    }
    for (index, value) in data[offset..].iter_mut().enumerate() {
        if index % 4 != 3 || alpha {
            *value = 255 - *value;
        }
    }
}

/// sharp `normalise`: stretches the CIELAB lightness between two percentiles to 0..100 and
/// leaves chroma and alpha alone. The percentiles follow libvips `percent`: a histogram of the
/// integer part of L with `max(L) + 1` bins, cumulated and scaled to `bins - 1`, and the first
/// bin above `percent / 100 * bins` (or `bins` when none is).
fn normalise(mut image: Image, lower: f64, upper: f64) -> Result<Image> {
    let total = image.data.len() as u64 / 4;
    if total == 0 {
        return Ok(image);
    }
    let mut histogram = [0u64; 256];
    let mut max_bin = 0usize;
    for px in image.data.chunks_exact(4) {
        let l = vips::srgb_to_lab(px[0], px[1], px[2])[0];
        let bin = (l.max(0.0) as usize).min(255);
        histogram[bin] += 1;
        max_bin = max_bin.max(bin);
    }
    let bins = max_bin + 1;
    let min_bin = histogram.iter().position(|&count| count > 0).unwrap_or(0);
    let percent = |p: f64| -> f64 {
        // 0 and 100 select the extremes of the histogram.
        if p <= 0.0 {
            return min_bin as f64;
        }
        if p >= 100.0 {
            return max_bin as f64;
        }
        let limit = p / 100.0 * bins as f64;
        let mut cumulative = 0u64;
        for (i, count) in histogram.iter().take(bins).enumerate() {
            cumulative += count;
            if ((cumulative * (bins as u64 - 1)) / total) as f64 > limit {
                return i as f64;
            }
        }
        bins as f64
    };
    let low = percent(lower);
    let high = percent(upper);
    if high <= low {
        return Ok(image);
    }
    let factor = (100.0 / (high - low)) as f32;
    let offset = -(low as f32) * factor;
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let [l, a, b] = vips::srgb_to_lab(px[0], px[1], px[2]);
        let rgb = vips::lab_to_srgb(l * factor + offset, a, b);
        px[0] = rgb[0];
        px[1] = rgb[1];
        px[2] = rgb[2];
    });
    Ok(image)
}

/// sharp `clahe` (libvips `hist_local`): every band (alpha included) is mapped pixel by pixel
/// through the equalised histogram of the `width` x `height` window around it. Bin counts are
/// capped at `max_slope` (a count, whatever the window area), the clipped excess is spread
/// evenly over the 256 bins, and edges are mirrored. `max_slope == 0` disables the clipping.
fn clahe(image: Image, width: u32, height: u32, max_slope: f64) -> Result<Image> {
    let w = image.width as usize;
    let h = image.height as usize;
    if w == 0 || h == 0 {
        return Ok(image);
    }
    if width as usize > w || height as usize > h {
        return Err(FastpkgError::Operation(
            "hist_local: window too large".into(),
        ));
    }
    let (win_w, win_h) = (width.max(1) as usize, height.max(1) as usize);
    let area = (win_w * win_h) as f64;
    let limit = if max_slope > 0.0 { max_slope } else { f64::MAX };
    let channels = if image.has_alpha { 4 } else { 3 };
    let mirror = |v: isize, len: usize| -> usize {
        let len = len as isize;
        let v = if v < 0 {
            -v - 1
        } else if v >= len {
            2 * len - v - 1
        } else {
            v
        };
        v.clamp(0, len - 1) as usize
    };
    let (off_x, off_y) = ((win_w / 2) as isize, (win_h / 2) as isize);
    let src = &image.data;
    let mut out = image.data.clone();
    out.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let rows: Vec<usize> = (0..win_h as isize)
                .map(|wy| mirror(y as isize - off_y + wy, h) * w)
                .collect();
            let cols: Vec<usize> = (0..w as isize + win_w as isize)
                .map(|i| mirror(i - off_x, w))
                .collect();
            for c in 0..channels {
                let mut hist = [0u32; 256];
                for &col in &cols[0..win_w] {
                    for &r in &rows {
                        hist[src[(r + col) * 4 + c] as usize] += 1;
                    }
                }
                for x in 0..w {
                    if x > 0 {
                        let (remove, add) = (cols[x - 1], cols[x + win_w - 1]);
                        for &r in &rows {
                            hist[src[(r + remove) * 4 + c] as usize] -= 1;
                            hist[src[(r + add) * 4 + c] as usize] += 1;
                        }
                    }
                    let value = row[x * 4 + c] as usize;
                    let mut excess = 0.0f64;
                    let mut below = 0.0f64;
                    for (i, &count) in hist.iter().enumerate() {
                        let count = count as f64;
                        let kept = count.min(limit);
                        excess += count - kept;
                        if i <= value {
                            below += kept;
                        }
                    }
                    let cdf = below + (((value as f64 + 1.0) * excess) / 256.0).floor();
                    row[x * 4 + c] = ((cdf * 255.0 / area).floor()).clamp(0.0, 255.0) as u8;
                }
            }
        });
    let mut result = image;
    result.data = out;
    Ok(result)
}

/// `x.clamp(0.0, 255.0) as u8` (truncation; NaN becomes 0) in a form that vectorises: the
/// saturating float-to-int cast does not, so the integer part is read from the mantissa after
/// adding 2^23.
#[inline(always)]
#[allow(clippy::manual_clamp)] // `clamp` would keep a NaN; `max` maps it to 0 like the cast.
fn trunc_u8(x: f32) -> u8 {
    const MAGIC: f32 = 8_388_608.0;
    let x = x.max(0.0).min(255.0);
    let nearest = (x + MAGIC) - MAGIC;
    let floor = nearest - f32::from(u8::from(nearest > x));
    (floor + MAGIC).to_bits() as u8
}

fn convolve(
    image: Image,
    width: u32,
    height: u32,
    kernel: &[f64],
    scale: Option<f64>,
    offset: f64,
) -> Result<Image> {
    let w = image.width as usize;
    let h = image.height as usize;
    let kw = width as usize;
    let kh = height as usize;
    let cx = kw / 2;
    let cy = kh / 2;
    let scale = scale.unwrap_or_else(|| kernel.iter().sum::<f64>());
    let scale = if scale.abs() < 1e-12 { 1.0 } else { scale } as f32;
    let weights = kernel.iter().map(|value| *value as f32).collect::<Vec<_>>();
    let offset = offset as f32;
    // sharp premultiplies RGBA input before convolving (and unpremultiplies afterwards), and
    // convolves the alpha band as well.
    let premultiplied = image.has_alpha;
    let mut image = image;
    let original = std::mem::take(&mut image.data);
    let source: Vec<u8> = if premultiplied {
        let mut copy = original.clone();
        premultiply(&mut copy);
        copy
    } else {
        Vec::new()
    };
    let source: &[u8] = if premultiplied { &source } else { &original };
    let row_len = w * 4;
    let padded_len = (w + kw - 1) * 4;
    let mut out = original.clone();
    const BLOCK_ROWS: usize = 16;
    out.par_chunks_mut(BLOCK_ROWS * row_len)
        .enumerate()
        .for_each_init(
            || (Vec::<f32>::new(), vec![0f32; row_len]),
            |(padded, acc), (block, chunk)| {
                let y0 = block * BLOCK_ROWS;
                let rows = chunk.len() / row_len;
                // f32 copies of the source rows this block reads, edges replicated by the
                // kernel's reach so every tap is a plain slice.
                let lo = y0.saturating_sub(cy);
                let hi = (y0 + rows - 1 + (kh - 1 - cy)).min(h - 1);
                padded.clear();
                padded.resize((hi - lo + 1) * padded_len, 0.0);
                for (i, line) in padded.chunks_exact_mut(padded_len).enumerate() {
                    let row = &source[(lo + i) * row_len..(lo + i + 1) * row_len];
                    let lead = cx * 4;
                    for (d, &v) in line[lead..lead + row_len].iter_mut().zip(row) {
                        *d = v as f32;
                    }
                    let first = [row[0], row[1], row[2], row[3]].map(|v| v as f32);
                    let last = [
                        row[row_len - 4],
                        row[row_len - 3],
                        row[row_len - 2],
                        row[row_len - 1],
                    ]
                    .map(|v| v as f32);
                    for px in line[..lead].chunks_exact_mut(4) {
                        px.copy_from_slice(&first);
                    }
                    for px in line[lead + row_len..].chunks_exact_mut(4) {
                        px.copy_from_slice(&last);
                    }
                }
                for r in 0..rows {
                    let y = y0 + r;
                    // Taps in the order the kernel is applied: row by row, left to right.
                    let taps: Vec<(&[f32], f32)> = (0..kh)
                        .flat_map(|ky| {
                            let sy = (y + ky).saturating_sub(cy).min(h - 1) - lo;
                            let line = &padded[sy * padded_len..(sy + 1) * padded_len];
                            let weights = &weights;
                            (0..kw).map(move |kx| {
                                (&line[kx * 4..kx * 4 + row_len], weights[ky * kw + kx])
                            })
                        })
                        .collect();
                    convolve_row(acc, &taps, scale, offset);
                    let dst = &mut chunk[r * row_len..(r + 1) * row_len];
                    if premultiplied {
                        for (px, sum) in dst.chunks_exact_mut(4).zip(acc.chunks_exact(4)) {
                            let alpha = sum[3];
                            for c in 0..3 {
                                // Negative sums divide by a negative alpha (no clamping); alpha 0
                                // gives 0.
                                let value = if alpha != 0.0 {
                                    (sum[c] as f64 * 255.0 / alpha as f64) as f32
                                } else {
                                    0.0
                                };
                                px[c] = value.clamp(0.0, 255.0) as u8;
                            }
                            px[3] = alpha.clamp(0.0, 255.0) as u8;
                        }
                    } else {
                        for (px, sum) in dst.chunks_exact_mut(4).zip(acc.chunks_exact(4)) {
                            px[0] = trunc_u8(sum[0]);
                            px[1] = trunc_u8(sum[1]);
                            px[2] = trunc_u8(sum[2]);
                        }
                    }
                }
            },
        );
    // Keep the metadata-free result the previous implementation returned.
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = image.has_alpha;
    Ok(out_image)
}

/// `acc[i] = (sum over taps in order of taps[k].0[i] * taps[k].1) / scale + offset`, with the
/// taps of a register block accumulated in order so each lane sees the same f32 operations as a
/// per-pixel loop would.
#[inline(always)]
fn convolve_row(acc: &mut [f32], taps: &[(&[f32], f32)], scale: f32, offset: f32) {
    const LANES: usize = 32;
    let split = acc.len() / LANES * LANES;
    let (head, tail) = acc.split_at_mut(split);
    for (block, chunk) in head.chunks_exact_mut(LANES).enumerate() {
        let base = block * LANES;
        let mut sum = [0f32; LANES];
        for &(line, weight) in taps {
            let src = &line[base..base + LANES];
            for i in 0..LANES {
                sum[i] += src[i] * weight;
            }
        }
        for (out, &v) in chunk.iter_mut().zip(&sum) {
            *out = v / scale + offset;
        }
    }
    for (i, out) in tail.iter_mut().enumerate() {
        let mut sum = 0f32;
        for &(line, weight) in taps {
            sum += line[split + i] * weight;
        }
        *out = sum / scale + offset;
    }
}

/// sharp `threshold`: with `grayscale` the image is first converted to `B_W`; every band
/// (alpha included) is then compared with `>= threshold` and set to 0 or 255. A threshold of 0
/// leaves the image untouched.
fn threshold(mut image: Image, threshold: u8, grayscale: bool) -> Result<Image> {
    if threshold == 0 {
        return Ok(image);
    }
    let test = move |v: u8| if v >= threshold { 255 } else { 0 };
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        if grayscale {
            let value = test(vips::bw(px[0], px[1], px[2]));
            px[0] = value;
            px[1] = value;
            px[2] = value;
        } else {
            px[0] = test(px[0]);
            px[1] = test(px[1]);
            px[2] = test(px[2]);
        }
        px[3] = test(px[3]);
    });
    Ok(image)
}

fn boolean(
    image: Image,
    operand: &str,
    operator: &str,
    raw: Option<&fastpkg_core::options::RawInput>,
) -> Result<Image> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(operand)
        .map_err(|e| FastpkgError::InvalidOptions(format!("invalid base64 operand: {e}")))?;
    let overlay = if let Some(raw) = raw {
        fastpkg_codecs::decode(
            &bytes,
            Format::Raw,
            &fastpkg_core::options::InputOptions {
                raw: Some(raw.clone()),
                ..Default::default()
            },
        )?
    } else {
        fastpkg_codecs::decode(&bytes, Format::from_magic(&bytes), &Default::default())?
    };
    // libvips pads the smaller of the two images with zeros (top-left aligned).
    let (width, height) = (
        image.width.max(overlay.width),
        image.height.max(overlay.height),
    );
    let pad = |source: &Image| -> Vec<u8> {
        let mut data = vec![0u8; width as usize * height as usize * 4];
        for y in 0..source.height as usize {
            let src = y * source.width as usize * 4;
            let dst = y * width as usize * 4;
            let len = source.width as usize * 4;
            data[dst..dst + len].copy_from_slice(&source.data[src..src + len]);
        }
        data
    };
    let has_alpha = image.has_alpha;
    let mut data = pad(&image);
    let other = pad(&overlay);
    data.par_chunks_exact_mut(4)
        .zip(other.par_chunks_exact(4))
        .for_each(|(a, b)| {
            for i in 0..4 {
                a[i] = match operator {
                    "and" => a[i] & b[i],
                    "or" => a[i] | b[i],
                    "eor" | "xor" => a[i] ^ b[i],
                    _ => a[i],
                };
            }
            if !has_alpha {
                a[3] = 255;
            }
        });
    let mut out = Image::from_rgba(width, height, data)?;
    out.format = image.format;
    out.has_alpha = has_alpha;
    Ok(out)
}

fn linear(mut image: Image, a: Option<&NumberOrArray>, b: Option<&NumberOrArray>) -> Result<Image> {
    // sharp leaves the alpha band alone unless `a` has an entry for every band.
    let bands = if image.has_alpha { 4 } else { 3 };
    let len = match a {
        Some(NumberOrArray::Array(values)) => values.len(),
        _ => 1,
    };
    let touch_alpha = image.has_alpha && len == bands;
    let mut lut = [[0u8; 256]; 4];
    for c in 0..4 {
        let av = a.map(|v| v.value(c)).unwrap_or(1.0);
        let bv = b.map(|v| v.value(c)).unwrap_or(0.0);
        for (i, v) in lut[c].iter_mut().enumerate() {
            *v = (i as f64 * av + bv).clamp(0.0, 255.0) as u8;
        }
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        for c in 0..3 {
            px[c] = lut[c][px[c] as usize];
        }
        if touch_alpha {
            px[3] = lut[3][px[3] as usize];
        }
    });
    Ok(image)
}

fn recomb(mut image: Image, matrix: &[Vec<f64>]) -> Result<Image> {
    let n = matrix.len().min(4);
    if n == 4 && !image.has_alpha {
        return Err(FastpkgError::Operation(
            "recomb: bands in must equal matrix width".into(),
        ));
    }
    let mut coefficients = [[0.0f32; 4]; 4];
    for (row, values) in matrix.iter().take(n).enumerate() {
        for (col, value) in values.iter().take(4).enumerate() {
            coefficients[row][col] = *value as f32;
        }
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let input = [px[0] as f32, px[1] as f32, px[2] as f32, px[3] as f32];
        for row in 0..n {
            let value = coefficients[row][0] * input[0]
                + coefficients[row][1] * input[1]
                + coefficients[row][2] * input[2]
                + coefficients[row][3] * input[3];
            px[row] = value.clamp(0.0, 255.0) as u8;
        }
    });
    Ok(image)
}

/// sharp `modulate`: colours go through LCh; lightness is scaled by `brightness` and shifted by
/// `lightness`, chroma is scaled by `saturation` and the hue angle is rotated by `hue` degrees.
fn modulate(
    mut image: Image,
    brightness: f64,
    saturation: f64,
    hue: f64,
    lightness: f64,
) -> Result<Image> {
    if brightness == 1.0 && saturation == 1.0 && hue == 0.0 && lightness == 0.0 {
        return Ok(image);
    }
    let (brightness, saturation, lightness) =
        (brightness as f32, saturation as f32, lightness as f32);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let [l, a, b] = vips::srgb_to_lab(px[0], px[1], px[2]);
        let chroma = (a * a + b * b).sqrt();
        let mut angle = b.atan2(a).to_degrees();
        if angle < 0.0 {
            angle += 360.0;
        }
        let l = l * brightness + lightness;
        let chroma = chroma * saturation;
        let angle = (angle + hue as f32).to_radians();
        let rgb = vips::lab_to_srgb(l, chroma * angle.cos(), chroma * angle.sin());
        px[0] = rgb[0];
        px[1] = rgb[1];
        px[2] = rgb[2];
    });
    Ok(image)
}

/// sharp `tint`: a lookup table indexed by the `B_W` grey level; each entry keeps the lightness
/// of that grey in CIELAB and takes the tint colour's a/b scaled by a parabola that fades the
/// tint out towards black and white.
fn tint(mut image: Image, tint: &str) -> Result<Image> {
    let color = Color::parse(tint)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid tint color {tint}")))?;
    let [_, tint_a, tint_b] = vips::srgb_to_lab(color.r, color.g, color.b);
    let mut lut = [[0u8; 3]; 256];
    for (g, entry) in lut.iter_mut().enumerate() {
        let l = vips::srgb_to_lab(g as u8, g as u8, g as u8)[0];
        let unit = l / 100.0;
        let weight = 1.0 - 4.0 * (unit - 0.5) * (unit - 0.5);
        *entry = vips::lab_to_srgb(l, weight * tint_a, weight * tint_b);
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let rgb = lut[vips::bw(px[0], px[1], px[2]) as usize];
        px[0] = rgb[0];
        px[1] = rgb[1];
        px[2] = rgb[2];
    });
    Ok(image)
}

fn greyscale(mut image: Image) -> Result<Image> {
    // Whole runs per task (not one rayon item per pixel); tiny images stay on this thread.
    if image.data.len() >= 1 << 20 {
        image
            .data
            .par_chunks_mut(1 << 17)
            .for_each(vips::bw_rgba_run);
    } else {
        vips::bw_rgba_run(&mut image.data);
    }
    Ok(image)
}

fn extract_channel(mut image: Image, selector: &ChannelSelector) -> Result<Image> {
    let channel = match selector {
        ChannelSelector::Index(i) => *i as usize,
        ChannelSelector::Name(name) => match name.to_ascii_lowercase().as_str() {
            "red" | "r" => 0,
            "green" | "g" => 1,
            "blue" | "b" => 2,
            "alpha" | "a" => 3,
            _ => {
                return Err(FastpkgError::InvalidOptions(format!(
                    "invalid channel {name}"
                )))
            }
        },
    }
    .min(4);
    let channels = if image.has_alpha { 4 } else { 3 };
    if channel >= channels {
        return Err(FastpkgError::Operation(format!(
            "Cannot extract channel {channel} from image with channels 0-{}",
            channels - 1
        )));
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let value = px[channel];
        px[0] = value;
        px[1] = value;
        px[2] = value;
        px[3] = 255;
    });
    image.gray = true;
    image.has_alpha = false;
    Ok(image)
}

fn join_channel(
    image: Image,
    input: &str,
    raw: Option<&fastpkg_core::options::RawInput>,
) -> Result<Image> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| FastpkgError::InvalidOptions(format!("invalid joinChannel base64: {e}")))?;
    let overlay = if let Some(raw) = raw {
        fastpkg_codecs::decode(
            &bytes,
            Format::Raw,
            &fastpkg_core::options::InputOptions {
                raw: Some(raw.clone()),
                ..Default::default()
            },
        )?
    } else {
        fastpkg_codecs::decode(&bytes, Format::from_magic(&bytes), &Default::default())?
    };
    if overlay.width != image.width || overlay.height != image.height {
        return Err(FastpkgError::Operation(
            "joinChannel image dimensions must match".into(),
        ));
    }
    let mut out = image;
    out.ensure_alpha(255);
    for (dst, src) in out
        .data
        .chunks_exact_mut(4)
        .zip(overlay.data.chunks_exact(4))
    {
        dst[3] = src[0];
    }
    out.has_alpha = true;
    Ok(out)
}

fn bandbool(image: Image, operator: &str) -> Result<Image> {
    let mut out = image;
    let has_alpha = out.has_alpha;
    // Choose the combining function once instead of matching the operator for every pixel.
    #[inline(always)]
    fn run(data: &mut [u8], has_alpha: bool, combine: impl Fn(u8, u8) -> u8 + Sync) {
        data.par_chunks_mut(1 << 14).for_each(|chunk| {
            for px in chunk.chunks_exact_mut(4) {
                let mut value = combine(combine(px[0], px[1]), px[2]);
                if has_alpha {
                    value = combine(value, px[3]);
                }
                px[..3].fill(value);
                px[3] = 255;
            }
        });
    }
    match operator {
        "and" => run(&mut out.data, has_alpha, |a, b| a & b),
        "or" => run(&mut out.data, has_alpha, |a, b| a | b),
        "eor" | "xor" => run(&mut out.data, has_alpha, |a, b| a ^ b),
        // Any other operator keeps the first band and ignores alpha.
        _ => run(&mut out.data, false, |a, _| a),
    }
    out.has_alpha = false;
    Ok(out)
}

#[derive(Clone)]
struct BuiltGradient {
    kind: GradientKind,
    angle: f64,
    center: [f64; 2],
    radius: f64,
    spread: GradientSpread,
    stops: Vec<(f64, Color)>,
}

impl BuiltGradient {
    fn new(spec: &GradientSpec) -> Result<Self> {
        let mut stops = Vec::with_capacity(spec.stops.len());
        for stop in &spec.stops {
            let color = Color::parse(&stop.color).ok_or_else(|| {
                FastpkgError::InvalidOptions(format!("invalid gradient color {}", stop.color))
            })?;
            stops.push((stop.offset.clamp(0.0, 1.0), color));
        }
        if stops.is_empty() {
            return Err(FastpkgError::InvalidOptions(
                "gradient requires at least one stop".into(),
            ));
        }
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(Self {
            kind: spec.kind,
            angle: spec.angle.to_radians(),
            center: spec.center,
            radius: spec.radius.max(1e-6),
            spread: spec.spread,
            stops,
        })
    }

    fn spread(&self, t: f64) -> f64 {
        match self.spread {
            GradientSpread::Pad => t.clamp(0.0, 1.0),
            GradientSpread::Repeat => t.rem_euclid(1.0),
            GradientSpread::Reflect => {
                let v = t.rem_euclid(2.0);
                if v <= 1.0 {
                    v
                } else {
                    2.0 - v
                }
            }
        }
    }

    fn sample(&self, t: f64) -> Color {
        let t = self.spread(t);
        if t <= self.stops[0].0 {
            return self.stops[0].1;
        }
        let last = self.stops[self.stops.len() - 1];
        if t >= last.0 {
            return last.1;
        }
        for window in self.stops.windows(2) {
            let (a_pos, a_color) = window[0];
            let (b_pos, b_color) = window[1];
            if t >= a_pos && t <= b_pos {
                let span = (b_pos - a_pos).max(1e-9);
                return color_lerp(a_color, b_color, (t - a_pos) / span);
            }
        }
        last.1
    }

    fn lut(&self, size: usize) -> Vec<Color> {
        (0..size)
            .map(|i| self.sample(i as f64 / (size.saturating_sub(1).max(1)) as f64))
            .collect()
    }

    fn position(&self, x: usize, y: usize, width: usize, height: usize) -> f64 {
        let nx = if width > 1 {
            x as f64 / (width - 1) as f64
        } else {
            0.5
        };
        let ny = if height > 1 {
            y as f64 / (height - 1) as f64
        } else {
            0.5
        };
        match self.kind {
            GradientKind::Linear => {
                let cos = self.angle.cos();
                let sin = self.angle.sin();
                let denom = cos.abs() + sin.abs();
                (((nx - 0.5) * cos + (ny - 0.5) * sin) / denom.max(1e-9)) + 0.5
            }
            GradientKind::Radial => {
                let dx = nx - self.center[0];
                let dy = ny - self.center[1];
                (dx * dx + dy * dy).sqrt() / self.radius
            }
        }
    }
}

fn color_lerp(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        (x as f64 + (y as f64 - x as f64) * t)
            .clamp(0.0, 255.0)
            .round() as u8
    };
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        alpha: mix(a.alpha, b.alpha),
    }
}

fn srgb_to_linear(value: u8) -> f64 {
    let c = value as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> u8 {
    let c = value.clamp(0.0, 1.0);
    let v = if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).clamp(0.0, 255.0).round() as u8
}

fn color_to_oklab(color: Color) -> [f64; 3] {
    let r = srgb_to_linear(color.r);
    let g = srgb_to_linear(color.g);
    let b = srgb_to_linear(color.b);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_color(lab: [f64; 3], alpha: u8) -> Color {
    let l_ = lab[0] + 0.3963377774 * lab[1] + 0.2158037573 * lab[2];
    let m_ = lab[0] - 0.1055613458 * lab[1] - 0.0638541728 * lab[2];
    let s_ = lab[0] - 0.0894841775 * lab[1] - 1.2914855480 * lab[2];
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    Color {
        r: linear_to_srgb(r),
        g: linear_to_srgb(g),
        b: linear_to_srgb(b),
        alpha,
    }
}

fn replace_color(
    mut image: Image,
    from: &str,
    to: &str,
    tolerance: f64,
    softness: f64,
    amount: f64,
    preserve_opacity: bool,
) -> Result<Image> {
    let from = Color::parse(from).ok_or_else(|| {
        FastpkgError::InvalidOptions(format!("invalid replaceColor source {from}"))
    })?;
    let to = Color::parse(to)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid replaceColor target {to}")))?;
    let tolerance = if tolerance <= 1.0 {
        tolerance.clamp(0.0, 1.0) * 441.6729559
    } else {
        tolerance.clamp(0.0, 441.6729559)
    };
    let softness = if softness <= 1.0 {
        softness.clamp(0.0, 1.0) * 441.6729559
    } else {
        softness.max(0.0)
    };
    let amount = amount.clamp(0.0, 1.0);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let dr = px[0] as f64 - from.r as f64;
        let dg = px[1] as f64 - from.g as f64;
        let db = px[2] as f64 - from.b as f64;
        let distance = (dr * dr + dg * dg + db * db).sqrt();
        let weight = if distance <= tolerance {
            1.0
        } else if softness > 0.0 && distance < tolerance + softness {
            let t = 1.0 - (distance - tolerance) / softness;
            t * t * (3.0 - 2.0 * t)
        } else {
            0.0
        } * amount;
        if weight > 0.0 {
            px[0] = (px[0] as f64 + (to.r as f64 - px[0] as f64) * weight).round() as u8;
            px[1] = (px[1] as f64 + (to.g as f64 - px[1] as f64) * weight).round() as u8;
            px[2] = (px[2] as f64 + (to.b as f64 - px[2] as f64) * weight).round() as u8;
            if !preserve_opacity {
                px[3] = (px[3] as f64 + (to.alpha as f64 - px[3] as f64) * weight).round() as u8;
            }
        }
    });
    if !preserve_opacity && to.alpha < 255 {
        image.has_alpha = true;
    }
    Ok(image)
}

fn recolor(mut image: Image, color: &str, amount: f64, preserve_opacity: bool) -> Result<Image> {
    let target = Color::parse(color)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid recolor {color}")))?;
    let target_lab = color_to_oklab(target);
    let amount = amount.clamp(0.0, 1.0);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let current = Color {
            r: px[0],
            g: px[1],
            b: px[2],
            alpha: px[3],
        };
        let lab = color_to_oklab(current);
        let result = oklab_to_color(
            [
                lab[0],
                lab[1] + (target_lab[1] - lab[1]) * amount,
                lab[2] + (target_lab[2] - lab[2]) * amount,
            ],
            if preserve_opacity {
                px[3]
            } else {
                target.alpha
            },
        );
        px[0] = result.r;
        px[1] = result.g;
        px[2] = result.b;
        px[3] = result.alpha;
    });
    if !preserve_opacity && target.alpha < 255 {
        image.has_alpha = true;
    }
    Ok(image)
}

fn duotone(
    image: Image,
    shadows: &str,
    highlights: &str,
    amount: f64,
    preserve_opacity: bool,
) -> Result<Image> {
    let spec = GradientSpec {
        kind: GradientKind::Linear,
        angle: 0.0,
        center: [0.5, 0.5],
        radius: 0.5,
        spread: GradientSpread::Pad,
        stops: vec![
            fastpkg_core::options::GradientStop {
                offset: 0.0,
                color: shadows.into(),
            },
            fastpkg_core::options::GradientStop {
                offset: 1.0,
                color: highlights.into(),
            },
        ],
    };
    gradient_map(image, &spec, amount, preserve_opacity)
}

fn gradient_map(
    mut image: Image,
    spec: &GradientSpec,
    amount: f64,
    preserve_opacity: bool,
) -> Result<Image> {
    let gradient = BuiltGradient::new(spec)?;
    let lut = gradient.lut(256);
    let amount = amount.clamp(0.0, 1.0);
    let has_alpha = AtomicBool::new(false);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let lum = luminance(px[0], px[1], px[2]);
        let target = lut[lum as usize];
        if target.alpha < 255 {
            has_alpha.store(true, Ordering::Relaxed);
        }
        px[0] = (px[0] as f64 + (target.r as f64 - px[0] as f64) * amount).round() as u8;
        px[1] = (px[1] as f64 + (target.g as f64 - px[1] as f64) * amount).round() as u8;
        px[2] = (px[2] as f64 + (target.b as f64 - px[2] as f64) * amount).round() as u8;
        if !preserve_opacity {
            px[3] = (px[3] as f64 + (target.alpha as f64 - px[3] as f64) * amount).round() as u8;
        }
    });
    if !preserve_opacity && has_alpha.load(Ordering::Relaxed) {
        image.has_alpha = true;
    }
    Ok(image)
}

fn gradient_fill(
    mut image: Image,
    spec: &GradientSpec,
    amount: f64,
    preserve_opacity: bool,
) -> Result<Image> {
    let gradient = BuiltGradient::new(spec)?;
    let lut = gradient.lut(1024);
    let amount = amount.clamp(0.0, 1.0);
    let width = image.width as usize;
    let height = image.height as usize;
    let has_alpha = AtomicBool::new(false);
    image
        .data
        .par_chunks_exact_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let t = gradient.spread(gradient.position(x, y, width, height));
                let target = lut[(t * 1023.0).round() as usize];
                if target.alpha < 255 {
                    has_alpha.store(true, Ordering::Relaxed);
                }
                px[0] = (px[0] as f64 + (target.r as f64 - px[0] as f64) * amount).round() as u8;
                px[1] = (px[1] as f64 + (target.g as f64 - px[1] as f64) * amount).round() as u8;
                px[2] = (px[2] as f64 + (target.b as f64 - px[2] as f64) * amount).round() as u8;
                if !preserve_opacity {
                    px[3] = (px[3] as f64 + (target.alpha as f64 - px[3] as f64) * amount).round()
                        as u8;
                }
            }
        });
    if !preserve_opacity && has_alpha.load(Ordering::Relaxed) {
        image.has_alpha = true;
    }
    Ok(image)
}

fn fill_color(mut image: Image, color: &str, amount: f64, preserve_opacity: bool) -> Result<Image> {
    let target = Color::parse(color)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid fill color {color}")))?;
    let amount = amount.clamp(0.0, 1.0);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = (px[0] as f64 + (target.r as f64 - px[0] as f64) * amount).round() as u8;
        px[1] = (px[1] as f64 + (target.g as f64 - px[1] as f64) * amount).round() as u8;
        px[2] = (px[2] as f64 + (target.b as f64 - px[2] as f64) * amount).round() as u8;
        if !preserve_opacity {
            px[3] = (px[3] as f64 + (target.alpha as f64 - px[3] as f64) * amount).round() as u8;
        }
    });
    if !preserve_opacity && target.alpha < 255 {
        image.has_alpha = true;
    }
    Ok(image)
}

struct CubeLut {
    size: usize,
    data: Vec<[f64; 3]>,
    domain_min: [f64; 3],
    domain_max: [f64; 3],
}

fn parse_cube(text: &str) -> Result<CubeLut> {
    let mut size = 0usize;
    let mut domain_min = [0.0; 3];
    let mut domain_max = [1.0; 3];
    let mut data = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("TITLE") => {}
            Some("LUT_3D_SIZE") => {
                size = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| FastpkgError::InvalidOptions("invalid LUT_3D_SIZE".into()))?;
            }
            Some("LUT_1D_SIZE") => {
                return Err(FastpkgError::InvalidOptions(
                    "1D .cube LUTs are not supported; use a 3D LUT".into(),
                ))
            }
            Some("DOMAIN_MIN") => {
                for c in &mut domain_min {
                    *c = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                }
            }
            Some("DOMAIN_MAX") => {
                for c in &mut domain_max {
                    *c = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1.0);
                }
            }
            Some(first) => {
                let r: f64 = first
                    .parse()
                    .map_err(|_| FastpkgError::InvalidOptions("invalid LUT row".into()))?;
                let g: f64 = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| FastpkgError::InvalidOptions("invalid LUT row".into()))?;
                let b: f64 = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| FastpkgError::InvalidOptions("invalid LUT row".into()))?;
                data.push([r, g, b]);
            }
            None => {}
        }
    }
    if size < 2 || data.len() != size * size * size {
        return Err(FastpkgError::InvalidOptions(format!(
            "LUT data has {} entries, expected {}^3",
            data.len(),
            size
        )));
    }
    Ok(CubeLut {
        size,
        data,
        domain_min,
        domain_max,
    })
}

impl CubeLut {
    fn sample(&self, rgb: [f64; 3], trilinear: bool) -> [f64; 3] {
        let n = self.size;
        let mut positions = [0.0; 3];
        for c in 0..3 {
            let span = (self.domain_max[c] - self.domain_min[c]).max(1e-9);
            positions[c] = ((rgb[c] - self.domain_min[c]) / span).clamp(0.0, 1.0) * (n - 1) as f64;
        }
        if !trilinear {
            let r = positions[0].round() as usize;
            let g = positions[1].round() as usize;
            let b = positions[2].round() as usize;
            return self.data[r + g * n + b * n * n];
        }
        let r0 = positions[0].floor() as usize;
        let g0 = positions[1].floor() as usize;
        let b0 = positions[2].floor() as usize;
        let r1 = (r0 + 1).min(n - 1);
        let g1 = (g0 + 1).min(n - 1);
        let b1 = (b0 + 1).min(n - 1);
        let fr = positions[0] - r0 as f64;
        let fg = positions[1] - g0 as f64;
        let fb = positions[2] - b0 as f64;
        let idx = |r: usize, g: usize, b: usize| self.data[r + g * n + b * n * n];
        let lerp = |a: [f64; 3], b: [f64; 3], t: f64| -> [f64; 3] {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        };
        let c00 = lerp(idx(r0, g0, b0), idx(r1, g0, b0), fr);
        let c10 = lerp(idx(r0, g1, b0), idx(r1, g1, b0), fr);
        let c01 = lerp(idx(r0, g0, b1), idx(r1, g0, b1), fr);
        let c11 = lerp(idx(r0, g1, b1), idx(r1, g1, b1), fr);
        lerp(lerp(c00, c10, fg), lerp(c01, c11, fg), fb)
    }
}

fn apply_lut(
    mut image: Image,
    text: &str,
    strength: f64,
    interpolation: Option<&str>,
) -> Result<Image> {
    let lut = parse_cube(text)?;
    let trilinear = interpolation != Some("nearest");
    let strength = strength.clamp(0.0, 1.0);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let input = [
            px[0] as f64 / 255.0,
            px[1] as f64 / 255.0,
            px[2] as f64 / 255.0,
        ];
        let output = lut.sample(input, trilinear);
        px[0] =
            (px[0] as f64 + (output[0] * 255.0 - px[0] as f64) * strength).clamp(0.0, 255.0) as u8;
        px[1] =
            (px[1] as f64 + (output[1] * 255.0 - px[1] as f64) * strength).clamp(0.0, 255.0) as u8;
        px[2] =
            (px[2] as f64 + (output[2] * 255.0 - px[2] as f64) * strength).clamp(0.0, 255.0) as u8;
    });
    Ok(image)
}

fn build_curve_lut(points: &[[f64; 2]]) -> [u8; 256] {
    let mut lut = [0u8; 256];
    if points.len() < 2 {
        for (i, v) in lut.iter_mut().enumerate() {
            *v = i as u8;
        }
        return lut;
    }
    let mut points: Vec<(f64, f64)> = points
        .iter()
        .map(|p| (p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)))
        .collect();
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let n = points.len();
    let mut slopes = vec![0.0; n.saturating_sub(1)];
    for i in 0..n - 1 {
        let dx = (points[i + 1].0 - points[i].0).max(1e-9);
        slopes[i] = (points[i + 1].1 - points[i].1) / dx;
    }
    let mut tangents = vec![0.0; n];
    tangents[0] = slopes[0];
    tangents[n - 1] = slopes[n - 2];
    for i in 1..n - 1 {
        tangents[i] = (slopes[i - 1] + slopes[i]) * 0.5;
    }
    for i in 0..n - 1 {
        if slopes[i].abs() < 1e-9 {
            tangents[i] = 0.0;
            tangents[i + 1] = 0.0;
        } else {
            let a = tangents[i] / slopes[i];
            let b = tangents[i + 1] / slopes[i];
            let h = a * a + b * b;
            if h > 9.0 {
                let t = 3.0 / h.sqrt();
                tangents[i] = t * a * slopes[i];
                tangents[i + 1] = t * b * slopes[i];
            }
        }
    }
    for (i, value) in lut.iter_mut().enumerate() {
        let x = i as f64 / 255.0;
        let y = if x <= points[0].0 {
            points[0].1
        } else if x >= points[n - 1].0 {
            points[n - 1].1
        } else {
            let mut segment = 0;
            while segment + 1 < n && x > points[segment + 1].0 {
                segment += 1;
            }
            let (x0, y0) = points[segment];
            let (x1, y1) = points[segment + 1];
            let h = (x1 - x0).max(1e-9);
            let t = (x - x0) / h;
            let t2 = t * t;
            let t3 = t2 * t;
            (2.0 * t3 - 3.0 * t2 + 1.0) * y0
                + (t3 - 2.0 * t2 + t) * h * tangents[segment]
                + (-2.0 * t3 + 3.0 * t2) * y1
                + (t3 - t2) * h * tangents[segment + 1]
        };
        *value = (y.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    lut
}

fn curves(
    mut image: Image,
    master: &[[f64; 2]],
    red: &[[f64; 2]],
    green: &[[f64; 2]],
    blue: &[[f64; 2]],
) -> Result<Image> {
    let master = build_curve_lut(master);
    let red = build_curve_lut(red);
    let green = build_curve_lut(green);
    let blue = build_curve_lut(blue);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = red[master[px[0] as usize] as usize];
        px[1] = green[master[px[1] as usize] as usize];
        px[2] = blue[master[px[2] as usize] as usize];
    });
    Ok(image)
}

fn option_value(value: Option<&NumberOrArray>, channel: usize, default: f64) -> f64 {
    value.map_or(default, |v| v.value(channel))
}

fn levels(
    mut image: Image,
    input_black: Option<&NumberOrArray>,
    input_white: Option<&NumberOrArray>,
    gamma: Option<&NumberOrArray>,
    output_black: Option<&NumberOrArray>,
    output_white: Option<&NumberOrArray>,
) -> Result<Image> {
    let mut luts = [[0u8; 256]; 4];
    for channel in 0..3 {
        let in_black = option_value(input_black, channel, 0.0);
        let in_white = option_value(input_white, channel, 255.0);
        let gamma = option_value(gamma, channel, 1.0).max(0.001);
        let out_black = option_value(output_black, channel, 0.0);
        let out_white = option_value(output_white, channel, 255.0);
        let span = (in_white - in_black).abs().max(1e-9);
        for i in 0..256 {
            let normalized = ((i as f64 - in_black) / span).clamp(0.0, 1.0);
            let corrected = normalized.powf(1.0 / gamma);
            luts[channel][i] =
                (out_black + corrected * (out_white - out_black)).clamp(0.0, 255.0) as u8;
        }
    }
    for (i, value) in luts[3].iter_mut().enumerate() {
        *value = i as u8;
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = luts[0][px[0] as usize];
        px[1] = luts[1][px[1] as usize];
        px[2] = luts[2][px[2] as usize];
        px[3] = luts[3][px[3] as usize];
    });
    Ok(image)
}

fn white_balance(mut image: Image, temperature: f64, tint: f64) -> Result<Image> {
    let t = (temperature / 100.0).clamp(-1.0, 1.0);
    let tint = (tint / 100.0).clamp(-1.0, 1.0);
    let r_gain = 1.0 + 0.3 * t + 0.1 * tint;
    let g_gain = 1.0 - 0.2 * tint;
    let b_gain = 1.0 - 0.3 * t + 0.1 * tint;
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = (px[0] as f64 * r_gain).clamp(0.0, 255.0) as u8;
        px[1] = (px[1] as f64 * g_gain).clamp(0.0, 255.0) as u8;
        px[2] = (px[2] as f64 * b_gain).clamp(0.0, 255.0) as u8;
    });
    Ok(image)
}

fn colour_balance(
    mut image: Image,
    shadows: [f64; 3],
    midtones: [f64; 3],
    highlights: [f64; 3],
) -> Result<Image> {
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let lum = luminance(px[0], px[1], px[2]) as f64 / 255.0;
        let shadow_weight = (1.0 - lum * 2.0).clamp(0.0, 1.0);
        let highlight_weight = (lum * 2.0 - 1.0).clamp(0.0, 1.0);
        let mid_weight = 1.0 - shadow_weight - highlight_weight;
        for channel in 0..3 {
            let shift = (shadows[channel] * shadow_weight
                + midtones[channel] * mid_weight
                + highlights[channel] * highlight_weight)
                * 2.55;
            px[channel] = (px[channel] as f64 + shift).clamp(0.0, 255.0) as u8;
        }
    });
    Ok(image)
}

fn posterize(mut image: Image, levels: u16) -> Result<Image> {
    let levels = levels.clamp(2, 256) as f64;
    let mut lut = [0u8; 256];
    for (i, value) in lut.iter_mut().enumerate() {
        *value = (((i as f64 / 255.0) * (levels - 1.0)).round() / (levels - 1.0) * 255.0)
            .clamp(0.0, 255.0) as u8;
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = lut[px[0] as usize];
        px[1] = lut[px[1] as usize];
        px[2] = lut[px[2] as usize];
    });
    Ok(image)
}

fn solarize(mut image: Image, threshold: f64) -> Result<Image> {
    let threshold = threshold.clamp(0.0, 255.0);
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        for channel in 0..3 {
            if px[channel] as f64 > threshold {
                px[channel] = 255 - px[channel];
            }
        }
    });
    Ok(image)
}

fn bayer8(x: usize, y: usize) -> f64 {
    const MATRIX: [[u8; 8]; 8] = [
        [0, 48, 12, 60, 3, 51, 15, 63],
        [32, 16, 44, 28, 35, 19, 47, 31],
        [8, 56, 4, 52, 11, 59, 7, 55],
        [40, 24, 36, 20, 43, 27, 39, 23],
        [2, 50, 14, 62, 1, 49, 13, 61],
        [34, 18, 46, 30, 33, 17, 45, 29],
        [10, 58, 6, 54, 9, 57, 5, 53],
        [42, 26, 38, 22, 41, 25, 37, 21],
    ];
    MATRIX[y % 8][x % 8] as f64 / 63.0 - 0.5
}

fn palette_swap(
    mut image: Image,
    palette: &[String],
    dither: f64,
    preserve_opacity: bool,
) -> Result<Image> {
    if palette.is_empty() {
        return Err(FastpkgError::InvalidOptions(
            "paletteSwap requires a palette".into(),
        ));
    }
    let palette: Vec<Color> = palette
        .iter()
        .map(|color| {
            Color::parse(color).ok_or_else(|| {
                FastpkgError::InvalidOptions(format!("invalid palette color {color}"))
            })
        })
        .collect::<Result<_>>()?;
    let dither = dither.clamp(0.0, 1.0) * 64.0;
    let width = image.width as usize;
    let has_alpha = AtomicBool::new(false);
    image
        .data
        .par_chunks_exact_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let offset = bayer8(x, y) * dither;
                let r = (px[0] as f64 + offset).clamp(0.0, 255.0);
                let g = (px[1] as f64 + offset).clamp(0.0, 255.0);
                let b = (px[2] as f64 + offset).clamp(0.0, 255.0);
                let mut best = palette[0];
                let mut best_distance = f64::MAX;
                for color in &palette {
                    let dr = r - color.r as f64;
                    let dg = g - color.g as f64;
                    let db = b - color.b as f64;
                    let distance = dr * dr + dg * dg + db * db;
                    if distance < best_distance {
                        best_distance = distance;
                        best = *color;
                    }
                }
                px[0] = best.r;
                px[1] = best.g;
                px[2] = best.b;
                if !preserve_opacity {
                    px[3] = best.alpha;
                    if best.alpha < 255 {
                        has_alpha.store(true, Ordering::Relaxed);
                    }
                }
            }
        });
    if !preserve_opacity && has_alpha.load(Ordering::Relaxed) {
        image.has_alpha = true;
    }
    Ok(image)
}

fn alpha_from_luma(
    mut image: Image,
    channel: &str,
    invert: bool,
    multiply: bool,
    scale: f64,
    offset: f64,
) -> Result<Image> {
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let mut value = match channel {
            "red" | "r" => px[0] as f64,
            "green" | "g" => px[1] as f64,
            "blue" | "b" => px[2] as f64,
            "alpha" | "a" => px[3] as f64,
            _ => luminance(px[0], px[1], px[2]) as f64,
        };
        value = value * scale + offset * 255.0;
        if invert {
            value = 255.0 - value;
        }
        let value = value.clamp(0.0, 255.0);
        px[3] = if multiply {
            (px[3] as f64 * value / 255.0).round() as u8
        } else {
            value.round() as u8
        };
    });
    image.has_alpha = true;
    Ok(image)
}

fn composite(
    image: Image,
    input: &str,
    blend: Blend,
    gravity: &str,
    top: Option<i32>,
    left: Option<i32>,
    tile: bool,
    density: Option<u32>,
    premultiplied: bool,
    source: Option<&fastpkg_core::options::InputOptions>,
) -> Result<Image> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| FastpkgError::InvalidOptions(format!("invalid composite base64: {e}")))?;
    let mut input_options = source.cloned().unwrap_or_default();
    if let Some(density) = density {
        input_options.density = density;
    }
    let format = if input_options.raw.is_some()
        || input_options.create.is_some()
        || input_options.text.is_some()
    {
        Format::Raw
    } else {
        Format::from_magic(&bytes)
    };
    let overlay = fastpkg_codecs::decode(&bytes, format, &input_options)?;
    if overlay.width == 0 || overlay.height == 0 {
        return Err(FastpkgError::InvalidOptions(
            "composite overlay has no pixels".into(),
        ));
    }
    let mut out = image;
    out.ensure_alpha(255);
    // Operators whose result is not `dest` where the overlay is transparent (clear, source, in,
    // out, dest-in, dest-atop) act on the whole canvas: sharp pads the overlay to canvas size.
    let global = matches!(
        blend,
        Blend::Clear | Blend::Source | Blend::In | Blend::Out | Blend::DestIn | Blend::DestAtop
    );
    if tile {
        // With gravity one tile is placed like a plain overlay and the grid continues in both
        // directions; an explicit top/left instead crops the replicated grid at that offset
        // (clamped to the grid).
        let (ow, oh) = (overlay.width as i32, overlay.height as i32);
        let (cw, ch) = (out.width as i32, out.height as i32);
        // sharp replicates an odd number of tiles per axis (at least enough to cover the canvas).
        let odd_tiles = |canvas: i32, tile: i32| -> i32 {
            let count = (canvas + tile - 1) / tile;
            if count % 2 == 0 {
                count + 1
            } else {
                count
            }
        };
        let (tw, th) = (odd_tiles(cw, ow) * ow, odd_tiles(ch, oh) * oh);
        let (x0, y0) = match (left, top) {
            // Explicit offsets pick the crop of the replicated grid (clamped to its size).
            (Some(l), Some(t)) => {
                if l < 0 || t < 0 {
                    return Err(FastpkgError::Operation(
                        "extract_area: bad extract area".into(),
                    ));
                }
                (-l.min(tw - cw), -t.min(th - ch))
            }
            // Gravity places one tile like a plain overlay; the grid continues from there.
            _ => {
                let place = |align: Align, canvas: i32, tile: i32| -> i32 {
                    let origin = match align {
                        Align::Start => 0,
                        Align::Center => (canvas - tile).div_euclid(2),
                        Align::End => canvas - tile,
                    };
                    origin.rem_euclid(tile) - tile
                };
                (
                    place(horizontal(gravity), cw, ow),
                    place(vertical(gravity), ch, oh),
                )
            }
        };
        let mut y = y0;
        while y < ch {
            let mut x = x0;
            while x < cw {
                blend_at(&mut out, &overlay, x, y, blend, premultiplied);
                x += ow;
            }
            y += oh;
        }
        return Ok(out);
    }
    let x = left.unwrap_or_else(|| match horizontal(gravity) {
        Align::Start => 0,
        Align::Center => (out.width as i32 - overlay.width as i32) / 2,
        Align::End => out.width as i32 - overlay.width as i32,
    });
    let y = top.unwrap_or_else(|| match vertical(gravity) {
        Align::Start => 0,
        Align::Center => (out.height as i32 - overlay.height as i32) / 2,
        Align::End => out.height as i32 - overlay.height as i32,
    });
    if global {
        let (ow, oh) = (overlay.width as i64, overlay.height as i64);
        let (x, y) = (x as i64, y as i64);
        let width = out.width as usize;
        out.data
            .par_chunks_exact_mut(width * 4)
            .enumerate()
            .for_each(|(dy, row)| {
                let sy = dy as i64 - y;
                for (dx, d) in row.chunks_exact_mut(4).enumerate() {
                    let sx = dx as i64 - x;
                    let s = if sx >= 0 && sy >= 0 && sx < ow && sy < oh {
                        overlay.get(sx as u32, sy as u32)
                    } else {
                        Color::TRANSPARENT
                    };
                    let d_color = Color {
                        r: d[0],
                        g: d[1],
                        b: d[2],
                        alpha: d[3],
                    };
                    let c = blend_pixel(d_color, s, blend, premultiplied);
                    d.copy_from_slice(&[c.r, c.g, c.b, c.alpha]);
                }
            });
        return Ok(out);
    }
    blend_at(&mut out, &overlay, x, y, blend, premultiplied);
    Ok(out)
}

/// `c / 255.0` for every 8-bit value (the table holds exactly what the division yields).
const UNIT: [f64; 256] = {
    let mut table = [0.0; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = i as f64 / 255.0;
        i += 1;
    }
    table
};

/// Exactly `value.clamp(0.0, 255.0).round() as u8` (ties away from zero) without the libm
/// `round` call, which does not inline on baseline x86-64.
#[inline(always)]
fn round_u8_f64(value: f64) -> u8 {
    let value = value.clamp(0.0, 255.0);
    let whole = value as u32;
    (whole + u32::from(value - whole as f64 >= 0.5)) as u8
}

fn blend_at(dst: &mut Image, src: &Image, ox: i32, oy: i32, blend: Blend, premult: bool) {
    let (dw, dh) = (dst.width as i64, dst.height as i64);
    let (sw, sh) = (src.width as i64, src.height as i64);
    let (ox, oy) = (ox as i64, oy as i64);
    let (x0, x1) = (ox.max(0), (ox + sw).min(dw));
    let (y0, y1) = (oy.max(0), (oy + sh).min(dh));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let row_bytes = dw as usize * 4;
    let rows = &mut dst.data[y0 as usize * row_bytes..y1 as usize * row_bytes];
    let work = |(row_index, dst_row): (usize, &mut [u8])| {
        let sy = (y0 + row_index as i64 - oy) as usize;
        let src_row = &src.data[sy * sw as usize * 4..(sy + 1) * sw as usize * 4];
        let from = (x0 - ox) as usize * 4;
        let to = (x1 - ox) as usize * 4;
        let target = &mut dst_row[x0 as usize * 4..x1 as usize * 4];
        for (d, s) in target
            .chunks_exact_mut(4)
            .zip(src_row[from..to].chunks_exact(4))
        {
            let color = blend_pixel(
                Color {
                    r: d[0],
                    g: d[1],
                    b: d[2],
                    alpha: d[3],
                },
                Color {
                    r: s[0],
                    g: s[1],
                    b: s[2],
                    alpha: s[3],
                },
                blend,
                premult,
            );
            d.copy_from_slice(&[color.r, color.g, color.b, color.alpha]);
        }
    };
    if (x1 - x0) * (y1 - y0) >= 16_384 {
        rows.par_chunks_exact_mut(row_bytes)
            .enumerate()
            .for_each(work);
    } else {
        rows.chunks_exact_mut(row_bytes).enumerate().for_each(work);
    }
}

/// Blends one overlay pixel `s` onto the backdrop pixel `d`.
///
/// The arithmetic follows libvips' `composite` (what sharp uses), which differs from the
/// textbook W3C formulas in a few places: separable blend functions are evaluated on
/// *premultiplied* colours, `atop`/`dest-atop` take their colour weights from `over`/`dest-over`
/// while keeping the Porter-Duff alpha, and `saturate`/`add` are additive operators.
fn blend_pixel(d: Color, s: Color, blend: Blend, src_premultiplied: bool) -> Color {
    if blend == Blend::Clear {
        return Color::TRANSPARENT;
    }
    if blend == Blend::Source {
        return s;
    }
    if blend == Blend::Dest {
        return d;
    }

    if blend == Blend::Over {
        // The general arithmetic reduces exactly to these results (see the test
        // `over_fast_paths_match_general_blend`).
        if s.alpha == 255 {
            return s;
        }
        if s.alpha == 0 && !src_premultiplied {
            return if d.alpha == 0 { Color::TRANSPARENT } else { d };
        }
    }
    if blend == Blend::Over {
        return blend_over(d, s, src_premultiplied);
    }
    blend_general(d, s, blend, src_premultiplied)
}

/// `Blend::Over` with the same floating-point operations, in the same order, as the
/// `fa = 1, fb = 1 - sa` case of [`blend_general`].
#[inline]
fn blend_over(d: Color, s: Color, src_premultiplied: bool) -> Color {
    let sa = UNIT[s.alpha as usize];
    let da = UNIT[d.alpha as usize];
    let dfb = da * (1.0 - sa);
    let out_a = sa + dfb;
    if out_a <= 0.0 {
        return Color::TRANSPARENT;
    }
    let channel = |s: u8, d: u8| -> u8 {
        let sp = if src_premultiplied {
            UNIT[s as usize]
        } else {
            UNIT[s as usize] * sa
        };
        round_u8_f64((sp + dfb * UNIT[d as usize]) / out_a * 255.0)
    };
    Color {
        r: channel(s.r, d.r),
        g: channel(s.g, d.g),
        b: channel(s.b, d.b),
        alpha: round_u8_f64(out_a * 255.0),
    }
}

/// Separable blend function `B(cb, cs)` on non-premultiplied backdrop/source values.
#[inline(always)]
fn separable(mode: Blend, cb: f64, cs: f64) -> f64 {
    match mode {
        Blend::Multiply => cb * cs,
        Blend::Screen => cb + cs - cb * cs,
        Blend::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        Blend::Darken => cb.min(cs),
        Blend::Lighten => cb.max(cs),
        Blend::Difference => (cb - cs).abs(),
        Blend::Exclusion => cb + cs - 2.0 * cb * cs,
        Blend::HardLight => {
            if cs <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        Blend::SoftLight => {
            let d = if cb <= 0.25 {
                ((16.0 * cb - 12.0) * cb + 4.0) * cb
            } else {
                cb.sqrt()
            };
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        Blend::ColourDodge | Blend::ColorDodge => {
            if cs >= 1.0 {
                1.0
            } else if cb <= 0.0 {
                0.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        Blend::ColourBurn | Blend::ColorBurn => {
            if cs <= 0.0 {
                0.0
            } else if cb >= 1.0 {
                1.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        _ => cs,
    }
}

/// The separable family of `composite` blend modes, monomorphised per mode by the caller.
#[inline(always)]
fn blend_separable(
    d: Color,
    s: Color,
    src_premultiplied: bool,
    sep: impl Fn(f64, f64) -> f64,
) -> Color {
    let sa = UNIT[s.alpha as usize];
    let da = UNIT[d.alpha as usize];
    let out_a = sa + da - sa * da;
    if out_a <= 0.0 {
        return Color::TRANSPARENT;
    }
    let channel = |s: u8, d: u8| -> u8 {
        let a = if src_premultiplied {
            UNIT[s as usize]
        } else {
            UNIT[s as usize] * sa
        };
        let b = da * UNIT[d as usize];
        let premult = (1.0 - da) * a + (1.0 - sa) * b + sa * da * sep(b, a);
        round_u8_f64(premult / out_a * 255.0)
    };
    Color {
        r: channel(s.r, d.r),
        g: channel(s.g, d.g),
        b: channel(s.b, d.b),
        alpha: round_u8_f64(out_a * 255.0),
    }
}

fn blend_general(d: Color, s: Color, blend: Blend, src_premultiplied: bool) -> Color {
    macro_rules! separable_arm {
        ($mode:expr) => {
            return blend_separable(d, s, src_premultiplied, |cb, cs| separable($mode, cb, cs))
        };
    }
    match blend {
        Blend::Multiply => separable_arm!(Blend::Multiply),
        Blend::Screen => separable_arm!(Blend::Screen),
        Blend::Overlay => separable_arm!(Blend::Overlay),
        Blend::Darken => separable_arm!(Blend::Darken),
        Blend::Lighten => separable_arm!(Blend::Lighten),
        Blend::Difference => separable_arm!(Blend::Difference),
        Blend::Exclusion => separable_arm!(Blend::Exclusion),
        Blend::HardLight => separable_arm!(Blend::HardLight),
        Blend::SoftLight => separable_arm!(Blend::SoftLight),
        Blend::ColourDodge | Blend::ColorDodge => separable_arm!(Blend::ColourDodge),
        Blend::ColourBurn | Blend::ColorBurn => separable_arm!(Blend::ColourBurn),
        _ => {}
    }
    let sa = UNIT[s.alpha as usize];
    let da = UNIT[d.alpha as usize];
    // Premultiplied overlay colour (`A'`): with `premultiplied: true` the stored values are
    // used as-is, even where they exceed the alpha (libvips does not clamp them).
    let sp = [s.r, s.g, s.b].map(|c| {
        let c = UNIT[c as usize];
        if src_premultiplied {
            c
        } else {
            c * sa
        }
    });
    let dst = [UNIT[d.r as usize], UNIT[d.g as usize], UNIT[d.b as usize]];

    // (output alpha, premultiplied output colour)
    let (out_a, premult): (f64, [f64; 3]) = match blend {
        Blend::Atop => (da, [0, 1, 2].map(|i| sp[i] + (1.0 - sa) * da * dst[i])),
        Blend::DestAtop => (sa, [0, 1, 2].map(|i| (1.0 - da) * sp[i] + da * dst[i])),
        Blend::Add => ((sa + da).min(1.0), [0, 1, 2].map(|i| sp[i] + da * dst[i])),
        Blend::Saturate => (
            (sa + da).min(1.0),
            [0, 1, 2].map(|i| sa.min(1.0 - da) * sp[i] + da * dst[i]),
        ),
        Blend::Over
        | Blend::In
        | Blend::Out
        | Blend::DestOver
        | Blend::DestIn
        | Blend::DestOut
        | Blend::Xor => {
            let (fa, fb) = match blend {
                Blend::In => (da, 0.0),
                Blend::Out => (1.0 - da, 0.0),
                Blend::DestOver => (1.0 - da, 1.0),
                Blend::DestIn => (0.0, sa),
                Blend::DestOut => (0.0, 1.0 - sa),
                Blend::Xor => (1.0 - da, 1.0 - sa),
                _ => (1.0, 1.0 - sa),
            };
            (
                sa * fa + da * fb,
                [0, 1, 2].map(|i| fa * sp[i] + da * fb * dst[i]),
            )
        }
        _ => (
            sa + da - sa * da,
            [0, 1, 2].map(|i| {
                let a = sp[i];
                let b = da * dst[i];
                (1.0 - da) * a + (1.0 - sa) * b + sa * da * separable(blend, b, a)
            }),
        ),
    };

    let channel = |index: usize| -> u8 {
        if out_a <= 0.0 {
            return 0;
        }
        round_u8_f64(premult[index] / out_a * 255.0)
    };

    Color {
        r: channel(0),
        g: channel(1),
        b: channel(2),
        alpha: round_u8_f64(out_a * 255.0),
    }
}

struct StatsAccum {
    min: [u32; 4],
    max: [u32; 4],
    sum: [u64; 4],
    sum_sq: [f64; 4],
    min_x: [u32; 4],
    min_y: [u32; 4],
    max_x: [u32; 4],
    max_y: [u32; 4],
    hist: [u64; 256],
    bins: Vec<[u64; 4]>,
    opaque: bool,
    sharpness: f64,
    pixels: u64,
}

impl StatsAccum {
    fn new() -> Self {
        Self {
            min: [255; 4],
            max: [0; 4],
            sum: [0; 4],
            sum_sq: [0.0; 4],
            min_x: [0; 4],
            min_y: [0; 4],
            max_x: [0; 4],
            max_y: [0; 4],
            hist: [0; 256],
            bins: vec![[0; 4]; 4096],
            opaque: true,
            sharpness: 0.0,
            pixels: 0,
        }
    }

    fn merge(&mut self, other: &Self) {
        for c in 0..4 {
            if other.min[c] < self.min[c] {
                self.min[c] = other.min[c];
                self.min_x[c] = other.min_x[c];
                self.min_y[c] = other.min_y[c];
            }
            if other.max[c] > self.max[c] {
                self.max[c] = other.max[c];
                self.max_x[c] = other.max_x[c];
                self.max_y[c] = other.max_y[c];
            }
            self.sum[c] += other.sum[c];
            self.sum_sq[c] += other.sum_sq[c];
        }
        for i in 0..256 {
            self.hist[i] += other.hist[i];
        }
        for i in 0..4096 {
            for c in 0..4 {
                self.bins[i][c] += other.bins[i][c];
            }
        }
        self.opaque &= other.opaque;
        self.sharpness += other.sharpness;
        self.pixels += other.pixels;
    }
}

pub fn stats(image: &Image) -> Stats {
    let channels = if image.has_alpha { 4 } else { 3 };
    let width = image.width as usize;
    let height = image.height as usize;
    if width == 0 || height == 0 {
        return Stats::default();
    }
    let row_bytes = width * 4;
    let rows_per_chunk = 32usize;
    let accum = image
        .data
        .par_chunks(row_bytes * rows_per_chunk)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let start_y = chunk_index * rows_per_chunk;
            let mut acc = StatsAccum::new();
            for (local_row, row) in chunk.chunks_exact(row_bytes).enumerate() {
                let y = start_y + local_row;
                for (x, px) in row.chunks_exact(4).enumerate() {
                    let values = [px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32];
                    for c in 0..channels {
                        let value = values[c];
                        if value < acc.min[c] {
                            acc.min[c] = value;
                            acc.min_x[c] = x as u32;
                            acc.min_y[c] = y as u32;
                        }
                        if value > acc.max[c] {
                            acc.max[c] = value;
                            acc.max_x[c] = x as u32;
                            acc.max_y[c] = y as u32;
                        }
                        acc.sum[c] += value as u64;
                        acc.sum_sq[c] += (value * value) as f64;
                    }
                    if px[3] != 255 {
                        acc.opaque = false;
                    }
                    let lum = luminance(px[0], px[1], px[2]);
                    acc.hist[lum as usize] += 1;
                    let key = (((px[0] as usize) >> 4) << 8)
                        | (((px[1] as usize) >> 4) << 4)
                        | ((px[2] as usize) >> 4);
                    if px[3] >= 128 {
                        acc.bins[key][0] += px[0] as u64;
                        acc.bins[key][1] += px[1] as u64;
                        acc.bins[key][2] += px[2] as u64;
                        acc.bins[key][3] += 1;
                    }
                    let index = (y * width + x) * 4;
                    let p = image.data[index] as f64;
                    if x + 1 < width {
                        acc.sharpness += (p - image.data[index + 4] as f64).abs();
                    }
                    if y + 1 < height {
                        acc.sharpness += (p - image.data[index + row_bytes] as f64).abs();
                    }
                    acc.pixels += 1;
                }
            }
            acc
        })
        .reduce(StatsAccum::new, |mut a, b| {
            a.merge(&b);
            a
        });

    let count = accum.pixels as f64;
    let mut channel_stats = Vec::with_capacity(channels);
    for c in 0..channels {
        let mean = accum.sum[c] as f64 / count;
        let variance = (accum.sum_sq[c] / count - mean * mean).max(0.0);
        channel_stats.push(fastpkg_core::ChannelStats {
            min: accum.min[c],
            max: accum.max[c],
            sum: accum.sum[c],
            mean,
            stdev: variance.sqrt(),
            min_x: accum.min_x[c],
            min_y: accum.min_y[c],
            max_x: accum.max_x[c],
            max_y: accum.max_y[c],
        });
    }

    // 16-bit images report their statistics on the full 0..65535 range (sharp/libvips).
    if let Some(wide) = image
        .wide
        .as_ref()
        .filter(|w| w.len() == width * height * 4)
    {
        let mut min = [u32::MAX; 4];
        let mut max = [0u32; 4];
        let mut positions = [[(0u32, 0u32); 2]; 4];
        let mut sum = [0u64; 4];
        let mut sum_sq = [0f64; 4];
        for (index, px) in wide.chunks_exact(4).enumerate() {
            let (x, y) = ((index % width) as u32, (index / width) as u32);
            for c in 0..channels {
                let value = px[c] as u32;
                if value < min[c] {
                    min[c] = value;
                    positions[c][0] = (x, y);
                }
                if value > max[c] {
                    max[c] = value;
                    positions[c][1] = (x, y);
                }
                sum[c] += value as u64;
                sum_sq[c] += value as f64 * value as f64;
            }
        }
        for (c, stat) in channel_stats.iter_mut().enumerate() {
            let mean = sum[c] as f64 / count;
            stat.min = min[c];
            stat.max = max[c];
            stat.sum = sum[c];
            stat.mean = mean;
            stat.stdev = (sum_sq[c] / count - mean * mean).max(0.0).sqrt();
            (stat.min_x, stat.min_y) = positions[c][0];
            (stat.max_x, stat.max_y) = positions[c][1];
        }
    }

    let entropy = accum
        .hist
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / count;
            -p * p.log2()
        })
        .sum();
    let sharpness = if width > 1 && height > 1 {
        accum.sharpness / ((width as f64 - 1.0) * (height as f64 - 1.0) * 2.0)
    } else {
        0.0
    };
    let dominant = accum
        .bins
        .iter()
        .enumerate()
        .max_by_key(|(_, bin)| bin[3])
        .filter(|(_, bin)| bin[3] > 0)
        .map(|(_, bin)| Color {
            r: (bin[0] / bin[3]) as u8,
            g: (bin[1] / bin[3]) as u8,
            b: (bin[2] / bin[3]) as u8,
            alpha: 255,
        });

    Stats {
        channels: channel_stats,
        is_opaque: accum.opaque,
        entropy,
        sharpness,
        dominant,
    }
}

fn luminance(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

pub fn image_filter(kernel: Kernel) -> ImageFilter {
    match kernel {
        Kernel::Nearest => ImageFilter::Nearest,
        Kernel::Linear | Kernel::Bilinear => ImageFilter::Triangle,
        Kernel::Cubic => ImageFilter::CatmullRom,
        Kernel::Mitchell => ImageFilter::CatmullRom,
        Kernel::Lanczos2 | Kernel::Lanczos3 | Kernel::Mks2013 | Kernel::Mks2021 => {
            ImageFilter::Lanczos3
        }
    }
}

pub fn enter_pipeline_colourspace(image: &mut Image, space: &str) -> Result<bool> {
    match space.to_ascii_lowercase().as_str() {
        "srgb" | "rgb" => Ok(false),
        "linear" | "scrgb" => {
            image.discard_wide();
            image.data.par_chunks_exact_mut(4).for_each(|px| {
                px[0] = (srgb_to_linear(px[0]) * 255.0).round() as u8;
                px[1] = (srgb_to_linear(px[1]) * 255.0).round() as u8;
                px[2] = (srgb_to_linear(px[2]) * 255.0).round() as u8;
            });
            Ok(true)
        }
        "b-w" | "bw" | "grey" | "gray" => {
            image.discard_wide();
            image.data.par_chunks_exact_mut(4).for_each(|px| {
                let lum = luminance(px[0], px[1], px[2]);
                px[0] = lum;
                px[1] = lum;
                px[2] = lum;
            });
            image.gray = true;
            image.has_alpha = false;
            Ok(false)
        }
        other => Err(FastpkgError::UnsupportedOperation(format!(
            "pipeline colourspace {other}"
        ))),
    }
}

pub fn leave_linear_colourspace(image: &mut Image) {
    image.discard_wide();
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = linear_to_srgb(px[0] as f64 / 255.0);
        px[1] = linear_to_srgb(px[1] as f64 / 255.0);
        px[2] = linear_to_srgb(px[2] as f64 / 255.0);
    });
}

pub fn apply_output_colourspace(image: &mut Image, space: &str) -> Result<()> {
    match space.to_ascii_lowercase().as_str() {
        "srgb" | "rgb" | "scrgb" => {
            image.gray = false;
            Ok(())
        }
        "rgb16" => {
            image.gray = false;
            image.remove_alpha();
            image.depth = "ushort".into();
            Ok(())
        }
        "rgba16" => {
            image.gray = false;
            image.ensure_alpha(255);
            image.depth = "ushort".into();
            Ok(())
        }
        "grey16" | "gray16" => {
            image.discard_wide();
            image.data.par_chunks_exact_mut(4).for_each(|px| {
                let lum = vips::bw(px[0], px[1], px[2]);
                px[0] = lum;
                px[1] = lum;
                px[2] = lum;
                px[3] = 255;
            });
            image.gray = true;
            image.has_alpha = false;
            image.depth = "ushort".into();
            Ok(())
        }
        "b-w" | "bw" | "grey" | "gray" => {
            // Alpha survives the conversion: sharp reports 2 channels for LA output.
            image.discard_wide();
            image.data.par_chunks_exact_mut(4).for_each(|px| {
                let lum = vips::bw(px[0], px[1], px[2]);
                px[0] = lum;
                px[1] = lum;
                px[2] = lum;
            });
            image.gray = true;
            Ok(())
        }
        other => Err(FastpkgError::UnsupportedOutput(format!(
            "colourspace {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> u8 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 33) as u8
    }

    #[test]
    fn over_fast_paths_match_general_blend() {
        let mut state = 7u64;
        for premultiplied in [false, true] {
            for sa in [0u8, 255] {
                for da in 0..=255u8 {
                    for _ in 0..4000 {
                        let d = Color {
                            r: lcg(&mut state),
                            g: lcg(&mut state),
                            b: lcg(&mut state),
                            alpha: da,
                        };
                        let s = Color {
                            r: lcg(&mut state),
                            g: lcg(&mut state),
                            b: lcg(&mut state),
                            alpha: sa,
                        };
                        assert_eq!(
                            blend_pixel(d, s, Blend::Over, premultiplied),
                            blend_general(d, s, Blend::Over, premultiplied),
                            "{d:?} {s:?} premultiplied {premultiplied}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn blend_over_matches_general() {
        let mut state = 31u64;
        for premultiplied in [false, true] {
            for _ in 0..3_000_000 {
                let d = Color {
                    r: lcg(&mut state),
                    g: lcg(&mut state),
                    b: lcg(&mut state),
                    alpha: lcg(&mut state),
                };
                let s = Color {
                    r: lcg(&mut state),
                    g: lcg(&mut state),
                    b: lcg(&mut state),
                    alpha: lcg(&mut state),
                };
                assert_eq!(
                    blend_over(d, s, premultiplied),
                    blend_general(d, s, Blend::Over, premultiplied),
                    "{d:?} {s:?} premultiplied {premultiplied}"
                );
            }
        }
    }

    #[test]
    fn morphology_matches_naive_window() {
        let mut state = 5u64;
        for (w, h, radius, dilate) in [
            (13usize, 9usize, 1u32, true),
            (13, 9, 3, false),
            (40, 11, 9, true),
            (7, 30, 25, false),
            (3, 3, 80, true),
            (64, 64, 8, false),
        ] {
            let data: Vec<u8> = (0..w * h * 4).map(|_| lcg(&mut state)).collect();
            let image = Image::from_rgba(w as u32, h as u32, data.clone()).unwrap();
            let out = morphology(image, radius, dilate).unwrap().data;
            let r = radius as i64;
            for y in 0..h {
                for x in 0..w {
                    for c in 0..4 {
                        let mut value = if dilate { 0u8 } else { 255u8 };
                        if c == 3 {
                            value = data[(y * w + x) * 4 + 3];
                        } else {
                            for dy in -r..=r {
                                for dx in -r..=r {
                                    let sx = (x as i64 + dx).clamp(0, w as i64 - 1) as usize;
                                    let sy = (y as i64 + dy).clamp(0, h as i64 - 1) as usize;
                                    let v = data[(sy * w + sx) * 4 + c];
                                    value = if dilate { value.max(v) } else { value.min(v) };
                                }
                            }
                        }
                        assert_eq!(
                            out[(y * w + x) * 4 + c],
                            value,
                            "{w}x{h} r{radius} ({x},{y},{c})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn reciprocal_division_is_exact() {
        for alpha in 1..=255u64 {
            for colour in 0..=255u64 {
                let n = colour * 255;
                assert_eq!((n * RECIPROCAL[alpha as usize]) >> 32, n / alpha);
            }
        }
    }

    #[test]
    fn median9_matches_sorting() {
        let mut state = 17u64;
        for _ in 0..200_000 {
            let mut values = [0f32; 9];
            for v in &mut values {
                *v = (lcg(&mut state) % 7) as f32 * 0.25 + (lcg(&mut state) % 2) as f32;
            }
            let mut sorted = values;
            sorted.sort_by(|a, b| a.total_cmp(b));
            assert_eq!(median9(values), sorted[4], "{values:?}");
        }
    }

    #[test]
    fn rounding_helpers_match_std() {
        let mut state = 99u64;
        for _ in 0..2_000_000 {
            let value = (lcg(&mut state) as f64) * 1.0307 + (lcg(&mut state) as f64) / 256.0 - 7.0;
            assert_eq!(round_u8_f64(value), value.clamp(0.0, 255.0).round() as u8);
        }
        for half in 0..=510 {
            let value = half as f64 / 2.0;
            assert_eq!(round_u8_f64(value), value.clamp(0.0, 255.0).round() as u8);
        }
    }

    #[test]
    #[ignore]
    fn bench_blend() {
        let mut state = 5u64;
        let px: Vec<(Color, Color)> = (0..2_000_000)
            .map(|_| {
                (
                    Color {
                        r: lcg(&mut state),
                        g: lcg(&mut state),
                        b: lcg(&mut state),
                        alpha: 100 + lcg(&mut state) % 100,
                    },
                    Color {
                        r: lcg(&mut state),
                        g: lcg(&mut state),
                        b: lcg(&mut state),
                        alpha: 100 + lcg(&mut state) % 100,
                    },
                )
            })
            .collect();
        for blend in [
            Blend::Over,
            Blend::Multiply,
            Blend::SoftLight,
            Blend::Screen,
        ] {
            let t = std::time::Instant::now();
            let mut acc = 0u32;
            for &(d, s) in &px {
                let c = blend_pixel(d, s, blend, false);
                acc = acc.wrapping_add(c.r as u32 + c.g as u32 + c.b as u32 + c.alpha as u32);
            }
            println!(
                "{blend:?}: {:.1} ns/px ({acc})",
                t.elapsed().as_nanos() as f64 / px.len() as f64
            );
        }
    }

    /// The straightforward implementation (before the table lookups and monomorphised
    /// separable modes), kept as the reference the optimised code must reproduce exactly.
    fn reference_blend(d: Color, s: Color, blend: Blend, src_premultiplied: bool) -> Color {
        if blend == Blend::Clear {
            return Color::TRANSPARENT;
        }
        if blend == Blend::Source {
            return s;
        }
        if blend == Blend::Dest {
            return d;
        }

        let sa = s.alpha as f64 / 255.0;
        let da = d.alpha as f64 / 255.0;
        // Premultiplied overlay colour (`A'`): with `premultiplied: true` the stored values are
        // used as-is, even where they exceed the alpha (libvips does not clamp them).
        let sp = [s.r, s.g, s.b].map(|c| {
            let c = c as f64 / 255.0;
            if src_premultiplied {
                c
            } else {
                c * sa
            }
        });
        let dst = [d.r as f64 / 255.0, d.g as f64 / 255.0, d.b as f64 / 255.0];

        let separable = |mode: Blend, cb: f64, cs: f64| -> f64 {
            match mode {
                Blend::Multiply => cb * cs,
                Blend::Screen => cb + cs - cb * cs,
                Blend::Overlay => {
                    if cb <= 0.5 {
                        2.0 * cb * cs
                    } else {
                        1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
                    }
                }
                Blend::Darken => cb.min(cs),
                Blend::Lighten => cb.max(cs),
                Blend::Difference => (cb - cs).abs(),
                Blend::Exclusion => cb + cs - 2.0 * cb * cs,
                Blend::HardLight => {
                    if cs <= 0.5 {
                        2.0 * cb * cs
                    } else {
                        1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
                    }
                }
                Blend::SoftLight => {
                    let d = if cb <= 0.25 {
                        ((16.0 * cb - 12.0) * cb + 4.0) * cb
                    } else {
                        cb.sqrt()
                    };
                    if cs <= 0.5 {
                        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
                    } else {
                        cb + (2.0 * cs - 1.0) * (d - cb)
                    }
                }
                Blend::ColourDodge | Blend::ColorDodge => {
                    if cs >= 1.0 {
                        1.0
                    } else if cb <= 0.0 {
                        0.0
                    } else {
                        (cb / (1.0 - cs)).min(1.0)
                    }
                }
                Blend::ColourBurn | Blend::ColorBurn => {
                    if cs <= 0.0 {
                        0.0
                    } else if cb >= 1.0 {
                        1.0
                    } else {
                        1.0 - ((1.0 - cb) / cs).min(1.0)
                    }
                }
                _ => cs,
            }
        };

        // (output alpha, premultiplied output colour)
        let (out_a, premult): (f64, [f64; 3]) = match blend {
            Blend::Atop => (da, [0, 1, 2].map(|i| sp[i] + (1.0 - sa) * da * dst[i])),
            Blend::DestAtop => (sa, [0, 1, 2].map(|i| (1.0 - da) * sp[i] + da * dst[i])),
            Blend::Add => ((sa + da).min(1.0), [0, 1, 2].map(|i| sp[i] + da * dst[i])),
            Blend::Saturate => (
                (sa + da).min(1.0),
                [0, 1, 2].map(|i| sa.min(1.0 - da) * sp[i] + da * dst[i]),
            ),
            Blend::Over
            | Blend::In
            | Blend::Out
            | Blend::DestOver
            | Blend::DestIn
            | Blend::DestOut
            | Blend::Xor => {
                let (fa, fb) = match blend {
                    Blend::In => (da, 0.0),
                    Blend::Out => (1.0 - da, 0.0),
                    Blend::DestOver => (1.0 - da, 1.0),
                    Blend::DestIn => (0.0, sa),
                    Blend::DestOut => (0.0, 1.0 - sa),
                    Blend::Xor => (1.0 - da, 1.0 - sa),
                    _ => (1.0, 1.0 - sa),
                };
                (
                    sa * fa + da * fb,
                    [0, 1, 2].map(|i| fa * sp[i] + da * fb * dst[i]),
                )
            }
            _ => (
                sa + da - sa * da,
                [0, 1, 2].map(|i| {
                    let a = sp[i];
                    let b = da * dst[i];
                    (1.0 - da) * a + (1.0 - sa) * b + sa * da * separable(blend, b, a)
                }),
            ),
        };

        let channel = |index: usize| -> u8 {
            if out_a <= 0.0 {
                return 0;
            }
            (premult[index] / out_a * 255.0).clamp(0.0, 255.0).round() as u8
        };

        Color {
            r: channel(0),
            g: channel(1),
            b: channel(2),
            alpha: (out_a * 255.0).round() as u8,
        }
    }

    #[test]
    fn optimised_blends_match_reference() {
        let blends = [
            Blend::Clear,
            Blend::Source,
            Blend::Over,
            Blend::In,
            Blend::Out,
            Blend::Atop,
            Blend::Dest,
            Blend::DestOver,
            Blend::DestIn,
            Blend::DestOut,
            Blend::DestAtop,
            Blend::Xor,
            Blend::Add,
            Blend::Saturate,
            Blend::Multiply,
            Blend::Screen,
            Blend::Overlay,
            Blend::Darken,
            Blend::Lighten,
            Blend::ColourDodge,
            Blend::ColourBurn,
            Blend::HardLight,
            Blend::SoftLight,
            Blend::Difference,
            Blend::Exclusion,
        ];
        let mut state = 1234u64;
        for blend in blends {
            for premultiplied in [false, true] {
                for round in 0..400_000 {
                    // Mix fully random pixels with extreme alphas and colours.
                    let pick = |state: &mut u64| match round % 7 {
                        0 => [0u8, 255][(lcg(state) & 1) as usize],
                        _ => lcg(state),
                    };
                    let d = Color {
                        r: pick(&mut state),
                        g: pick(&mut state),
                        b: pick(&mut state),
                        alpha: pick(&mut state),
                    };
                    let s = Color {
                        r: pick(&mut state),
                        g: pick(&mut state),
                        b: pick(&mut state),
                        alpha: pick(&mut state),
                    };
                    assert_eq!(
                        blend_pixel(d, s, blend, premultiplied),
                        reference_blend(d, s, blend, premultiplied),
                        "{blend:?} {d:?} {s:?} premultiplied {premultiplied}"
                    );
                }
            }
        }
    }
}
