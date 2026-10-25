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

mod vips;

pub fn apply(mut image: Image, operations: &[Operation]) -> Result<Image> {
    for op in operations {
        image = apply_one(image, op)?;
    }
    Ok(image)
}

pub fn apply_one(mut image: Image, op: &Operation) -> Result<Image> {
    // Many operations rebuild the image buffer from scratch; carry per-image
    // metadata across so EXIF/ICC/XMP and the single-channel hint survive them.
    let gray = image.gray;
    let icc = image.icc.take();
    let exif = image.exif.take();
    let xmp = image.xmp.take();
    let mut out = apply_op(image, op)?;
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
            ..
        } => affine(image, matrix, *idx, *idy, *odx, *ody, background),
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
        Operation::Blur { sigma } => blur(image, *sigma),
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

fn resize_options(kernel: Kernel, has_alpha: bool) -> ResizeOptions {
    let algorithm = match kernel {
        Kernel::Nearest => ResizeAlg::Nearest,
        Kernel::Linear | Kernel::Bilinear => ResizeAlg::Convolution(FirFilter::Bilinear),
        Kernel::Cubic => ResizeAlg::Convolution(FirFilter::CatmullRom),
        Kernel::Mitchell => ResizeAlg::Convolution(FirFilter::Mitchell),
        Kernel::Lanczos2 => ResizeAlg::Convolution(FirFilter::Lanczos3),
        Kernel::Lanczos3 => ResizeAlg::Convolution(FirFilter::Lanczos3),
        Kernel::Mks2013 | Kernel::Mks2021 => ResizeAlg::Convolution(FirFilter::Lanczos3),
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
    if matches!(extend_with, ExtendWith::Background) {
        data.par_chunks_exact_mut(width as usize * 4)
            .enumerate()
            .for_each(|(row_index, row)| {
                let inside = row_index >= y && row_index < y + image.height as usize;
                for px in row.chunks_exact_mut(4) {
                    px.copy_from_slice(&fill);
                }
                if inside {
                    let src = (row_index - y) * src_stride;
                    row[x * 4..x * 4 + src_stride]
                        .copy_from_slice(&image.data[src..src + src_stride]);
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
        data.par_chunks_exact_mut(width as usize * 4)
            .enumerate()
            .for_each(|(row_index, row)| {
                let sy = map(row_index as i64 - y as i64, image.height);
                let src_row = &image.data[sy * src_stride..(sy + 1) * src_stride];
                for (px, &sx) in row.chunks_exact_mut(4).zip(&columns) {
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
    let flat = |px: &[u8]| -> [f32; 3] {
        let a = px[3] as f32 / 255.0;
        [px[0] as f32 * a, px[1] as f32 * a, px[2] as f32 * a]
    };
    let mut values: Vec<[f32; 3]> = image.data.chunks_exact(4).map(flat).collect();
    if !line_art && w > 1 && h > 1 {
        let source = values.clone();
        values.par_iter_mut().enumerate().for_each(|(index, out)| {
            let (x, y) = ((index % w) as isize, (index / w) as isize);
            for (c, slot) in out.iter_mut().enumerate() {
                let mut window = [0f32; 9];
                let mut n = 0;
                for dy in -1..=1isize {
                    for dx in -1..=1isize {
                        let sx = (x + dx).clamp(0, w as isize - 1) as usize;
                        let sy = (y + dy).clamp(0, h as isize - 1) as usize;
                        window[n] = source[sy * w + sx][c];
                        n += 1;
                    }
                }
                window.sort_by(|a, b| a.total_cmp(b));
                *slot = window[4];
            }
        });
    }
    let bg = match background {
        Some(text) => {
            let color = Color::parse(text)
                .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {text}")))?;
            flat(&[color.r, color.g, color.b, color.alpha])
        }
        None => values.first().copied().unwrap_or([0.0; 3]),
    };
    let mut left = w;
    let mut top = h;
    let mut right = 0usize;
    let mut bottom = 0usize;
    let mut found = false;
    for (index, v) in values.iter().enumerate() {
        let diff = (v[0] - bg[0])
            .abs()
            .max((v[1] - bg[1]).abs())
            .max((v[2] - bg[2]).abs());
        if f64::from(diff) > threshold {
            let (x, y) = (index % w, index / w);
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
            found = true;
        }
    }
    if !found {
        return Ok(image);
    }
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
    out.data
        .par_chunks_exact_mut(new_w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, dst) in row.chunks_exact_mut(4).enumerate() {
                let (sx, sy) = rotation_source(cos, sin, left, top, x as u32, y as u32);
                let color = sample_bilinear(&image, sx, sy, bg);
                dst.copy_from_slice(&[color.r, color.g, color.b, color.alpha]);
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

fn sample_bilinear_wide(
    wide: &[u16],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
    fallback: [u16; 4],
) -> [u16; 4] {
    if x < -0.5 || y < -0.5 || x > width as f64 - 0.5 || y > height as f64 - 0.5 {
        return fallback;
    }
    let x = x.clamp(0.0, (width - 1) as f64);
    let y = y.clamp(0.0, (height - 1) as f64);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let at = |px: usize, py: usize| -> [f64; 4] {
        let idx = (py * width + px) * 4;
        [
            wide[idx] as f64,
            wide[idx + 1] as f64,
            wide[idx + 2] as f64,
            wide[idx + 3] as f64,
        ]
    };
    let c00 = at(x0, y0);
    let c10 = at(x1, y0);
    let c01 = at(x0, y1);
    let c11 = at(x1, y1);
    let mut out = [0u16; 4];
    for c in 0..4 {
        let top = c00[c] + (c10[c] - c00[c]) * fx;
        let bottom = c01[c] + (c11[c] - c01[c]) * fx;
        out[c] = (top + (bottom - top) * fy).clamp(0.0, 65_535.0).round() as u16;
    }
    out
}

fn sample_bilinear(image: &Image, x: f64, y: f64, fallback: Color) -> Color {
    if x < -0.5 || y < -0.5 || x > image.width as f64 - 0.5 || y > image.height as f64 - 0.5 {
        return fallback;
    }
    let x = x.clamp(0.0, (image.width - 1) as f64);
    let y = y.clamp(0.0, (image.height - 1) as f64);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(image.width - 1);
    let y1 = (y0 + 1).min(image.height - 1);
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let c00 = image.get(x0, y0);
    let c10 = image.get(x1, y0);
    let c01 = image.get(x0, y1);
    let c11 = image.get(x1, y1);
    let lerp = |a: u8, b: u8, c: u8, d: u8| -> u8 {
        let top = a as f64 + (b as f64 - a as f64) * fx;
        let bottom = c as f64 + (d as f64 - c as f64) * fx;
        (top + (bottom - top) * fy).clamp(0.0, 255.0).round() as u8
    };
    Color {
        r: lerp(c00.r, c10.r, c01.r, c11.r),
        g: lerp(c00.g, c10.g, c01.g, c11.g),
        b: lerp(c00.b, c10.b, c01.b, c11.b),
        alpha: lerp(c00.alpha, c10.alpha, c01.alpha, c11.alpha),
    }
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
                let color = sample_bilinear(&image, sx, sy, bg);
                dst.copy_from_slice(&[color.r, color.g, color.b, color.alpha]);
            }
        });
    out.format = image.format;
    Ok(out)
}

fn sharpen(
    image: Image,
    sigma: f64,
    m1: f64,
    m2: f64,
    x1: f64,
    y2: f64,
    y3: f64,
    simple: bool,
) -> Result<Image> {
    let sigma = sigma.clamp(0.1, 10.0);
    let blurred = blur_pixels(
        &image.data,
        image.width as usize,
        image.height as usize,
        if simple { 1.0 } else { sigma },
    );
    let x1 = if simple { f64::MAX } else { x1.max(0.0) };
    let m1 = if simple { 0.5 } else { m1.max(0.0) };
    let m2 = if simple { 0.5 } else { m2.max(0.0) };
    let bright_limit = if simple { 1.0 } else { (y2 / 255.0).max(0.0) };
    let dark_limit = if simple { 1.0 } else { (y3 / 255.0).max(0.0) };
    let mut out = image;
    let row_bytes = (out.width as usize * 4).max(4);
    out.data
        .par_chunks_mut(row_bytes)
        .zip(blurred.par_chunks(row_bytes))
        .for_each(|(dst_row, blur_row)| {
            for (dst, blur_px) in dst_row.chunks_exact_mut(4).zip(blur_row.chunks_exact(4)) {
                let original_lum = luminance(dst[0], dst[1], dst[2]) as f64;
                let blurred_lum = luminance(blur_px[0], blur_px[1], blur_px[2]) as f64;
                let diff = original_lum - blurred_lum;
                let contrast = diff.abs();
                let amount = if contrast < x1 { m1 } else { m2 } * 0.5;
                let boost = (diff * amount).clamp(-dark_limit * 255.0, bright_limit * 255.0);
                for channel in 0..3 {
                    dst[channel] = (dst[channel] as f64 + boost).clamp(0.0, 255.0) as u8;
                }
            }
        });
    Ok(out)
}

fn blur(image: Image, sigma: f64) -> Result<Image> {
    let out = blur_pixels(
        &image.data,
        image.width as usize,
        image.height as usize,
        sigma,
    );
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = image.has_alpha;
    Ok(out_image)
}

/// `dst[i] = sum_k kernel[k] * srcs[k][i]`, accumulated in tap order. Columns are processed in
/// blocks so the accumulator stays in registers across all taps.
#[inline(always)]
fn mac_rows(dst: &mut [f32], kernel: &[f32], srcs: &[&[f32]]) {
    const LANES: usize = 16;
    let split = dst.len() / LANES * LANES;
    let (head, tail) = dst.split_at_mut(split);
    for (block, chunk) in head.chunks_exact_mut(LANES).enumerate() {
        let base = block * LANES;
        let mut acc = [0.0f32; LANES];
        for (&weight, src) in kernel.iter().zip(srcs) {
            for (a, &value) in acc.iter_mut().zip(&src[base..base + LANES]) {
                *a += value * weight;
            }
        }
        chunk.copy_from_slice(&acc);
    }
    for (i, out) in tail.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for (&weight, src) in kernel.iter().zip(srcs) {
            acc += src[split + i] * weight;
        }
        *out = acc;
    }
}

/// Exactly `value.clamp(0.0, 255.0).round() as u8` (ties away from zero), written with an
/// integer truncation so it vectorises instead of calling `roundf` per sample.
#[inline(always)]
fn round_u8(value: f32) -> u8 {
    let value = value.clamp(0.0, 255.0);
    let whole = value as i32;
    (whole + i32::from(value - whole as f32 >= 0.5)) as u8
}

/// Separable Gaussian blur on RGBA8. Both passes accumulate whole rows (`acc[i] += w * src[i]`)
/// in tap order, so the compiler vectorises them without any architecture-specific code and
/// the f32 result is identical to a per-pixel multiply-accumulate over the same taps.
fn blur_pixels(data: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let sigma = sigma.max(0.1) as f32;
    let radius = (sigma * 3.0).ceil() as usize;
    let mut kernel = Vec::with_capacity(radius * 2 + 1);
    let mut sum = 0.0f32;
    for i in -(radius as i32)..=(radius as i32) {
        let value = (-(i * i) as f32 / (2.0 * sigma * sigma)).exp();
        kernel.push(value);
        sum += value;
    }
    for value in &mut kernel {
        *value /= sum;
    }

    let row_len = width * 4;
    let mut horizontal = vec![0.0f32; data.len()];
    horizontal
        .par_chunks_exact_mut(row_len)
        .zip(data.par_chunks_exact(row_len))
        .for_each_init(
            || vec![0.0f32; (width + 2 * radius) * 4],
            |padded, (row, source)| {
                // Edge-clamped copy of the source row as f32: padded[j] = source[clamp(j - radius)].
                for (j, dst) in padded.chunks_exact_mut(4).enumerate() {
                    let sx = j.saturating_sub(radius).min(width - 1);
                    for (d, &v) in dst.iter_mut().zip(&source[sx * 4..sx * 4 + 4]) {
                        *d = v as f32;
                    }
                }
                let taps: Vec<&[f32]> = (0..kernel.len())
                    .map(|k| &padded[k * 4..k * 4 + row_len])
                    .collect();
                mac_rows(row, &kernel, &taps);
            },
        );

    let mut out = vec![0u8; data.len()];
    out.par_chunks_exact_mut(row_len).enumerate().for_each_init(
        || vec![0.0f32; row_len],
        |acc, (y, row)| {
            let taps: Vec<&[f32]> = (0..kernel.len())
                .map(|k| {
                    let sy = (y + k).saturating_sub(radius).min(height - 1);
                    &horizontal[sy * row_len..(sy + 1) * row_len]
                })
                .collect();
            mac_rows(acc, &kernel, &taps);
            for (dst, &value) in row.iter_mut().zip(acc.iter()) {
                *dst = round_u8(value);
            }
        },
    );
    out
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

fn morphology(image: Image, width: u32, dilate: bool) -> Result<Image> {
    let radius = width.max(1) as i32;
    let w = image.width as usize;
    let h = image.height as usize;
    let mut horizontal = vec![0u8; image.data.len()];
    horizontal
        .par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..3 {
                    let mut value = if dilate { 0u8 } else { 255u8 };
                    for dx in -radius..=radius {
                        let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        let v = image.data[(y * w + sx) * 4 + c];
                        value = if dilate { value.max(v) } else { value.min(v) };
                    }
                    row[x * 4 + c] = value;
                }
                row[x * 4 + 3] = image.data[(y * w + x) * 4 + 3];
            }
        });
    let mut out = horizontal.clone();
    out.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..3 {
                    let mut value = if dilate { 0u8 } else { 255u8 };
                    for dy in -radius..=radius {
                        let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        let v = horizontal[(sy * w + x) * 4 + c];
                        value = if dilate { value.max(v) } else { value.min(v) };
                    }
                    row[x * 4 + c] = value;
                }
            }
        });
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = image.has_alpha;
    Ok(out_image)
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
    let cx = kw as i32 / 2;
    let cy = kh as i32 / 2;
    let scale = scale.unwrap_or_else(|| kernel.iter().sum::<f64>());
    let scale = if scale.abs() < 1e-12 { 1.0 } else { scale } as f32;
    let weights = kernel.iter().map(|value| *value as f32).collect::<Vec<_>>();
    // sharp premultiplies RGBA input before convolving (and unpremultiplies afterwards), and
    // convolves the alpha band as well.
    let premultiply = image.has_alpha;
    let source: Vec<u8> = if premultiply {
        image
            .data
            .chunks_exact(4)
            .flat_map(|px| {
                let a = px[3] as u32;
                [
                    (px[0] as u32 * a / 255) as u8,
                    (px[1] as u32 * a / 255) as u8,
                    (px[2] as u32 * a / 255) as u8,
                    px[3],
                ]
            })
            .collect()
    } else {
        image.data.clone()
    };
    // Accumulate in float without clamping: sharp unpremultiplies the raw convolution result,
    // so negative sums (e.g. a laplacian) still divide by a negative alpha.
    let mut sums = vec![0f32; w * h * 4];
    sums.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..4 {
                    if c == 3 && !premultiply {
                        continue;
                    }
                    let mut acc = 0.0f32;
                    for ky in 0..kh {
                        let sy = (y as i32 + ky as i32 - cy).clamp(0, h as i32 - 1) as usize;
                        let source_row = sy * w;
                        for kx in 0..kw {
                            let sx = (x as i32 + kx as i32 - cx).clamp(0, w as i32 - 1) as usize;
                            acc += source[(source_row + sx) * 4 + c] as f32 * weights[ky * kw + kx];
                        }
                    }
                    // No clamping or rounding here: sharp unpremultiplies the raw float result.
                    row[x * 4 + c] = acc / scale + offset as f32;
                }
            }
        });
    let mut out = image.data.clone();
    out.par_chunks_exact_mut(4)
        .zip(sums.par_chunks_exact(4))
        .for_each(|(px, sum)| {
            if premultiply {
                let alpha = sum[3];
                for c in 0..3 {
                    // Negative sums divide by a negative alpha (no clamping); alpha 0 gives 0.
                    let value = if alpha != 0.0 {
                        (sum[c] as f64 * 255.0 / alpha as f64) as f32
                    } else {
                        0.0
                    };
                    px[c] = value.clamp(0.0, 255.0) as u8;
                }
                px[3] = alpha.clamp(0.0, 255.0) as u8;
            } else {
                for c in 0..3 {
                    px[c] = sum[c].clamp(0.0, 255.0) as u8;
                }
            }
        });
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = image.has_alpha;
    Ok(out_image)
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
    out.data.par_chunks_exact_mut(4).for_each(|px| {
        let mut value = match operator {
            "and" => px[0] & px[1] & px[2],
            "or" => px[0] | px[1] | px[2],
            "eor" | "xor" => px[0] ^ px[1] ^ px[2],
            _ => px[0],
        };
        if has_alpha {
            match operator {
                "and" => value &= px[3],
                "or" => value |= px[3],
                "eor" | "xor" => value ^= px[3],
                _ => {}
            }
        }
        px[0] = value;
        px[1] = value;
        px[2] = value;
        px[3] = 255;
    });
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
        let (ow, oh) = (overlay.width as i32, overlay.height as i32);
        for dy in 0..out.height as i32 {
            for dx in 0..out.width as i32 {
                let (sx, sy) = (dx - x, dy - y);
                let s = if sx >= 0 && sy >= 0 && sx < ow && sy < oh {
                    overlay.get(sx as u32, sy as u32)
                } else {
                    Color::TRANSPARENT
                };
                let d = out.get(dx as u32, dy as u32);
                out.set(
                    dx as u32,
                    dy as u32,
                    blend_pixel(d, s, blend, premultiplied),
                );
            }
        }
        return Ok(out);
    }
    blend_at(&mut out, &overlay, x, y, blend, premultiplied);
    Ok(out)
}

fn blend_at(dst: &mut Image, src: &Image, ox: i32, oy: i32, blend: Blend, premult: bool) {
    for sy in 0..src.height {
        for sx in 0..src.width {
            let dx = ox + sx as i32;
            let dy = oy + sy as i32;
            if dx < 0 || dy < 0 || dx >= dst.width as i32 || dy >= dst.height as i32 {
                continue;
            }
            let s = src.get(sx, sy);
            let d = dst.get(dx as u32, dy as u32);
            let color = blend_pixel(d, s, blend, premult);
            dst.set(dx as u32, dy as u32, color);
        }
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
