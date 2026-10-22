#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::too_many_arguments,
    clippy::needless_range_loop,
    clippy::unnecessary_unwrap
)]

use std::cmp::min;
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
use image::RgbaImage;
use rayon::prelude::*;

pub fn apply(mut image: Image, operations: &[Operation]) -> Result<Image> {
    for op in operations {
        image = apply_one(image, op)?;
    }
    Ok(image)
}

pub fn apply_one(mut image: Image, op: &Operation) -> Result<Image> {
    let preserves_wide = matches!(
        op,
        Operation::Resize { .. }
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
        ),
        Operation::Extend {
            width,
            height,
            background,
            position,
            extend_with,
        } => extend(image, *width, *height, background, position, *extend_with),
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
        } => trim(image, background, *threshold, *line_art),
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
            ..
        } => composite(image, input, *blend, gravity, *top, *left, *tile),
    }
}

fn to_rgba_image(image: &Image) -> Result<RgbaImage> {
    RgbaImage::from_raw(image.width, image.height, image.data.clone())
        .ok_or_else(|| FastpkgError::Operation("invalid RGBA buffer".into()))
}

fn from_rgba_image(mut image: Image, rgba: RgbaImage) -> Image {
    image.width = rgba.width();
    image.height = rgba.height();
    image.data = rgba.into_raw();
    image
}

fn resize(
    image: Image,
    width: Option<u32>,
    height: Option<u32>,
    fit: Fit,
    position: &str,
    background: &str,
    kernel: Kernel,
    without_enlargement: bool,
    without_reduction: bool,
) -> Result<Image> {
    if width.is_none() && height.is_none() {
        return Ok(image);
    }
    let (mut tw, mut th) = fastpkg_core::pipeline::compute_target(
        image.width,
        image.height,
        width,
        height,
        fit,
        without_enlargement || fit == Fit::Inside,
        without_reduction,
    );
    if fit == Fit::Cover && width.is_some() && height.is_some() {
        let source_has_alpha = image.has_alpha;
        let centering = (
            alignment_fraction(horizontal(position)),
            alignment_fraction(vertical(position)),
        );
        let mut resized = resize_raw_opts(
            image,
            width.unwrap(),
            height.unwrap(),
            kernel,
            Some(centering),
        )?;
        resized.has_alpha = source_has_alpha || resized.has_transparency();
        return Ok(resized);
    }
    if fit == Fit::Contain || fit == Fit::Inside {
        let scale = (tw as f64 / image.width as f64).min(th as f64 / image.height as f64);
        tw = (image.width as f64 * scale).round().max(1.0) as u32;
        th = (image.height as f64 * scale).round().max(1.0) as u32;
    }
    if fit == Fit::Outside {
        let scale = (tw as f64 / image.width as f64).max(th as f64 / image.height as f64);
        tw = (image.width as f64 * scale).round().max(1.0) as u32;
        th = (image.height as f64 * scale).round().max(1.0) as u32;
    }
    tw = tw.max(1);
    th = th.max(1);
    if tw == image.width && th == image.height {
        return Ok(image);
    }
    if fit == Fit::Fill && width.is_some() && height.is_some() {
        let source_has_alpha = image.has_alpha;
        let mut out = resize_raw(image, tw, th, kernel)?;
        let _ = background;
        out.has_alpha = source_has_alpha || out.has_transparency();
        return Ok(out);
    }
    let source_has_alpha = image.has_alpha;
    let mut out = resize_raw(image, tw, th, kernel)?;
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

fn alignment_fraction(align: Align) -> f64 {
    match align {
        Align::Start => 0.0,
        Align::Center => 0.5,
        Align::End => 1.0,
    }
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
    if left >= image.width || top >= image.height {
        return Err(FastpkgError::InvalidOptions(
            "extract region starts outside the image".into(),
        ));
    }
    let width = min(width, image.width - left);
    let height = min(height, image.height - top);
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
    background: &str,
    position: &str,
    extend_with: ExtendWith,
) -> Result<Image> {
    let width = width.unwrap_or(image.width).max(image.width);
    let height = height.unwrap_or(image.height).max(image.height);
    if width == image.width && height == image.height {
        return Ok(image);
    }
    let color = Color::parse(background)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {background}")))?;
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
        let x = match horizontal(position) {
            Align::Start => 0,
            Align::Center => (width - image.width) / 2,
            Align::End => width - image.width,
        };
        let y = match vertical(position) {
            Align::Start => 0,
            Align::Center => (height - image.height) / 2,
            Align::End => height - image.height,
        };
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
        return Ok(image);
    }
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for px in data.chunks_exact_mut(4) {
        px.copy_from_slice(&[color.r, color.g, color.b, color.alpha]);
    }
    let x = match horizontal(position) {
        Align::Start => 0,
        Align::Center => (width - image.width) / 2,
        Align::End => width - image.width,
    };
    let y = match vertical(position) {
        Align::Start => 0,
        Align::Center => (height - image.height) / 2,
        Align::End => height - image.height,
    };
    if matches!(extend_with, ExtendWith::Background) {
        for row in 0..image.height {
            let src = row as usize * image.width as usize * 4;
            let dst = ((y + row) as usize * width as usize + x as usize) * 4;
            data[dst..dst + image.width as usize * 4]
                .copy_from_slice(&image.data[src..src + image.width as usize * 4]);
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
                data[dst..dst + 4].copy_from_slice(&image.data[src..src + 4]);
            }
        }
    }
    let mut out = Image::from_rgba(width, height, data)?;
    out.format = image.format;
    out.has_alpha = image.has_alpha;
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

fn trim(image: Image, background: &str, threshold: f64, line_art: bool) -> Result<Image> {
    let bg = if background == "#000000" {
        image.get(0, 0)
    } else {
        Color::parse(background)
            .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {background}")))?
    };
    let threshold = if threshold > 0.0 {
        threshold
    } else if line_art {
        0.1
    } else {
        10.0
    };
    let mut left = image.width;
    let mut top = image.height;
    let mut right = 0;
    let mut bottom = 0;
    for y in 0..image.height {
        for x in 0..image.width {
            let px = image.get(x, y);
            let diff = (px.r as f64 - bg.r as f64).abs()
                + (px.g as f64 - bg.g as f64).abs()
                + (px.b as f64 - bg.b as f64).abs()
                + (px.alpha as f64 - bg.alpha as f64).abs();
            if diff > threshold {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    if right < left || bottom < top {
        return Err(FastpkgError::Operation("trim found no content".into()));
    }
    extract(image, left, top, right - left + 1, bottom - top + 1)
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
    let rgba = to_rgba_image(&image)?;
    let flipped = image::imageops::flip_vertical(&rgba);
    Ok(from_rgba_image(image, flipped))
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
    let rgba = to_rgba_image(&image)?;
    let flipped = image::imageops::flip_horizontal(&rgba);
    Ok(from_rgba_image(image, flipped))
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
        let rgba = to_rgba_image(&image)?;
        return Ok(from_rgba_image(image, image::imageops::rotate90(&rgba)));
    }
    if (angle - 180.0).abs() < f64::EPSILON {
        let rgba = to_rgba_image(&image)?;
        return Ok(from_rgba_image(image, image::imageops::rotate180(&rgba)));
    }
    if (angle - 270.0).abs() < f64::EPSILON {
        let rgba = to_rgba_image(&image)?;
        return Ok(from_rgba_image(image, image::imageops::rotate270(&rgba)));
    }
    let bg = Color::parse(background).unwrap_or(Color::BLACK);
    let theta = angle.to_radians();
    let cos = theta.cos();
    let sin = theta.sin();
    let new_w = (image.width as f64 * cos.abs() + image.height as f64 * sin.abs()).ceil() as u32;
    let new_h = (image.width as f64 * sin.abs() + image.height as f64 * cos.abs()).ceil() as u32;
    let cx = image.width as f64 / 2.0;
    let cy = image.height as f64 / 2.0;
    let ncx = new_w as f64 / 2.0;
    let ncy = new_h as f64 / 2.0;
    let mut out = Image::from_rgba(new_w, new_h, vec![0u8; new_w as usize * new_h as usize * 4])?;
    out.has_alpha = true;
    for y in 0..new_h {
        for x in 0..new_w {
            let dx = x as f64 + 0.5 - ncx;
            let dy = y as f64 + 0.5 - ncy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            let color = sample_bilinear(&image, sx, sy, bg);
            out.set(x, y, color);
        }
    }
    out.format = image.format;
    Ok(out)
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
    let theta = angle.to_radians();
    let cos = theta.cos();
    let sin = theta.sin();
    let new_w = (width as f64 * cos.abs() + height as f64 * sin.abs()).ceil() as u32;
    let new_h = (width as f64 * sin.abs() + height as f64 * cos.abs()).ceil() as u32;
    let cx = width as f64 / 2.0;
    let cy = height as f64 / 2.0;
    let ncx = new_w as f64 / 2.0;
    let ncy = new_h as f64 / 2.0;
    let mut out = vec![0u16; new_w as usize * new_h as usize * 4];
    for y in 0..new_h {
        for x in 0..new_w {
            let dx = x as f64 + 0.5 - ncx;
            let dy = y as f64 + 0.5 - ncy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            let pixel = sample_bilinear_wide(&wide, width, height, sx, sy, fallback);
            let dst = (y as usize * new_w as usize + x as usize) * 4;
            out[dst..dst + 4].copy_from_slice(&pixel);
        }
    }
    image.set_wide_rgba(new_w, new_h, out)?;
    image.has_alpha = true;
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
        5 => {
            let image = flop(image, true)?;
            rotate(image, 90.0, "#000000")?
        }
        6 => rotate(image, 90.0, "#000000")?,
        7 => {
            let image = flop(image, true)?;
            rotate(image, 270.0, "#000000")?
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
        let ox = a * (x + idx) + b * (y + idy) + odx;
        let oy = c * (x + idx) + d * (y + idy) + ody;
        min_x = min_x.min(ox);
        min_y = min_y.min(oy);
        max_x = max_x.max(ox);
        max_y = max_y.max(oy);
    }
    let width = (max_x - min_x).ceil().max(1.0) as u32;
    let height = (max_y - min_y).ceil().max(1.0) as u32;
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
                let ox = x as f64 + min_x;
                let oy = y as f64 + min_y;
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
        image.has_alpha = true;
        return Ok(image);
    }
    let mut out = Image::from_rgba(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    )?;
    out.has_alpha = true;
    for y in 0..height {
        for x in 0..width {
            let ox = x as f64 + min_x;
            let oy = y as f64 + min_y;
            let dx = ox - odx;
            let dy = oy - ody;
            let sx = inv_a * dx + inv_b * dy - idx;
            let sy = inv_c * dx + inv_d * dy - idy;
            out.set(x, y, sample_bilinear(&image, sx, sy, bg));
        }
    }
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
    out.data
        .par_chunks_exact_mut(4)
        .zip(blurred.par_chunks_exact(4))
        .for_each(|(dst, blur_px)| {
            let original_lum = luminance(dst[0], dst[1], dst[2]) as f64;
            let blurred_lum = luminance(blur_px[0], blur_px[1], blur_px[2]) as f64;
            let diff = original_lum - blurred_lum;
            let contrast = diff.abs();
            let amount = if contrast < x1 { m1 } else { m2 } * 0.5;
            let boost = (diff * amount).clamp(-dark_limit * 255.0, bright_limit * 255.0);
            for channel in 0..3 {
                dst[channel] = (dst[channel] as f64 + boost).clamp(0.0, 255.0) as u8;
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
fn blur_pixels_sse41(data: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
    use std::arch::x86_64::*;
    let sigma = sigma.max(0.1) as f32;
    let radius = (sigma * 3.0).ceil() as i32;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut sum = 0.0f32;
    for i in -radius..=radius {
        let value = (-(i * i) as f32 / (2.0 * sigma * sigma)).exp();
        kernel.push(value);
        sum += value;
    }
    for value in &mut kernel {
        *value /= sum;
    }

    let mut horizontal = vec![0.0f32; data.len()];
    horizontal
        .par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| unsafe {
            for x in 0..width {
                let mut acc = _mm_setzero_ps();
                let start = x as i32 - radius;
                let end = x as i32 + radius;
                if start >= 0 && end < width as i32 {
                    for (k, weight) in kernel.iter().enumerate() {
                        let source = (y * width + (x as i32 + k as i32 - radius) as usize) * 4;
                        let packed =
                            std::ptr::read_unaligned(data.as_ptr().add(source) as *const i32);
                        let values = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(_mm_cvtsi32_si128(packed)));
                        acc = _mm_add_ps(acc, _mm_mul_ps(values, _mm_set1_ps(*weight)));
                    }
                } else {
                    for (k, weight) in kernel.iter().enumerate() {
                        let sx = (x as i32 + k as i32 - radius).clamp(0, width as i32 - 1) as usize;
                        let source = (y * width + sx) * 4;
                        let packed =
                            std::ptr::read_unaligned(data.as_ptr().add(source) as *const i32);
                        let values = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(_mm_cvtsi32_si128(packed)));
                        acc = _mm_add_ps(acc, _mm_mul_ps(values, _mm_set1_ps(*weight)));
                    }
                }
                _mm_storeu_ps(row.as_mut_ptr().add(x * 4), acc);
            }
        });

    let mut out = vec![0u8; data.len()];
    out.par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| unsafe {
            for x in 0..width {
                let mut acc = _mm_setzero_ps();
                let start = y as i32 - radius;
                let end = y as i32 + radius;
                if start >= 0 && end < height as i32 {
                    for (k, weight) in kernel.iter().enumerate() {
                        let source = ((y as i32 + k as i32 - radius) as usize * width + x) * 4;
                        let values = _mm_loadu_ps(horizontal.as_ptr().add(source));
                        acc = _mm_add_ps(acc, _mm_mul_ps(values, _mm_set1_ps(*weight)));
                    }
                } else {
                    for (k, weight) in kernel.iter().enumerate() {
                        let sy =
                            (y as i32 + k as i32 - radius).clamp(0, height as i32 - 1) as usize;
                        let source = (sy * width + x) * 4;
                        let values = _mm_loadu_ps(horizontal.as_ptr().add(source));
                        acc = _mm_add_ps(acc, _mm_mul_ps(values, _mm_set1_ps(*weight)));
                    }
                }
                let mut values = [0.0f32; 4];
                _mm_storeu_ps(values.as_mut_ptr(), acc);
                for c in 0..4 {
                    row[x * 4 + c] = values[c].clamp(0.0, 255.0).round() as u8;
                }
            }
        });
    out
}

fn blur_pixels(data: &[u8], width: usize, height: usize, sigma: f64) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("sse4.1") {
        return unsafe { blur_pixels_sse41(data, width, height, sigma) };
    }
    let sigma = sigma.max(0.1);
    let radius = (sigma * 3.0).ceil() as i32;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut sum = 0.0f32;
    for i in -radius..=radius {
        let value = (-(i * i) as f32 / (2.0 * sigma as f32 * sigma as f32)).exp();
        kernel.push(value);
        sum += value;
    }
    for value in &mut kernel {
        *value /= sum;
    }

    let mut horizontal = vec![0.0f32; data.len()];
    horizontal
        .par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let base = y * width * 4;
            for x in 0..width {
                let mut acc = [0.0f32; 4];
                let start = x as i32 - radius;
                let end = x as i32 + radius;
                if start >= 0 && end < width as i32 {
                    for (k, weight) in kernel.iter().enumerate() {
                        let source = base + (x as i32 + k as i32 - radius) as usize * 4;
                        for c in 0..4 {
                            acc[c] += data[source + c] as f32 * weight;
                        }
                    }
                } else {
                    for (k, weight) in kernel.iter().enumerate() {
                        let sx = (x as i32 + k as i32 - radius).clamp(0, width as i32 - 1) as usize;
                        let source = base + sx * 4;
                        for c in 0..4 {
                            acc[c] += data[source + c] as f32 * weight;
                        }
                    }
                }
                row[x * 4..x * 4 + 4].copy_from_slice(&acc);
            }
        });

    let mut out = vec![0u8; data.len()];
    out.par_chunks_mut(width * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..width {
                let mut acc = [0.0f32; 4];
                let start = y as i32 - radius;
                let end = y as i32 + radius;
                if start >= 0 && end < height as i32 {
                    for (k, weight) in kernel.iter().enumerate() {
                        let source = ((y as i32 + k as i32 - radius) as usize * width + x) * 4;
                        for c in 0..4 {
                            acc[c] += horizontal[source + c] * weight;
                        }
                    }
                } else {
                    for (k, weight) in kernel.iter().enumerate() {
                        let sy =
                            (y as i32 + k as i32 - radius).clamp(0, height as i32 - 1) as usize;
                        let source = (sy * width + x) * 4;
                        for c in 0..4 {
                            acc[c] += horizontal[source + c] * weight;
                        }
                    }
                }
                for c in 0..4 {
                    row[x * 4 + c] = acc[c].clamp(0.0, 255.0).round() as u8;
                }
            }
        });
    out
}

fn median(image: Image, size: u32) -> Result<Image> {
    let size = size.max(1);
    if size == 1 {
        return Ok(image);
    }
    let radius = (size / 2) as i32;
    let w = image.width as i32;
    let h = image.height as i32;
    let window = (size * size) as usize;
    let mut out = image.data.clone();
    out.par_chunks_exact_mut(4)
        .enumerate()
        .map_init(
            || Vec::with_capacity(window),
            |values, (idx, px)| {
                let x = (idx as i32) % w;
                let y = (idx as i32) / w;
                for c in 0..4 {
                    values.clear();
                    for dy in -radius..=radius {
                        for dx in -radius..=radius {
                            let sx = (x + dx).clamp(0, w - 1) as usize;
                            let sy = (y + dy).clamp(0, h - 1) as usize;
                            values.push(image.data[(sy * w as usize + sx) * 4 + c]);
                        }
                    }
                    values.sort_unstable();
                    px[c] = values[values.len() / 2];
                }
            },
        )
        .for_each(|_| {});
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

fn gamma(mut image: Image, gamma: f64, gamma_out: Option<f64>) -> Result<Image> {
    let gamma = gamma.clamp(1.0, 3.0);
    let gamma_out = gamma_out.unwrap_or(gamma).clamp(1.0, 3.0);
    let exponent = gamma / gamma_out;
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = ((i as f64 / 255.0).powf(exponent) * 255.0).clamp(0.0, 255.0) as u8;
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

fn normalise(mut image: Image, lower: f64, upper: f64) -> Result<Image> {
    let mut histogram = [0u64; 256];
    for px in image.data.chunks_exact(4) {
        histogram[luminance(px[0], px[1], px[2]) as usize] += 1;
    }
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return Ok(image);
    }
    let low_target = (total as f64 * lower / 100.0) as u64;
    let high_target = (total as f64 * upper / 100.0) as u64;
    let mut cumulative = 0u64;
    let mut low = 0u8;
    let mut high = 255u8;
    for (i, count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative >= low_target {
            low = i as u8;
            break;
        }
    }
    cumulative = 0;
    for (i, count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative >= high_target {
            high = i as u8;
            break;
        }
    }
    if high <= low {
        return Ok(image);
    }
    let scale = 255.0 / (high - low) as f64;
    let mut lut = [0u8; 256];
    for (i, value) in lut.iter_mut().enumerate() {
        *value = (((i as f64 - low as f64) * scale).clamp(0.0, 255.0)) as u8;
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        px[0] = lut[px[0] as usize];
        px[1] = lut[px[1] as usize];
        px[2] = lut[px[2] as usize];
    });
    Ok(image)
}

fn clahe(image: Image, width: u32, height: u32, max_slope: f64) -> Result<Image> {
    let w = image.width as usize;
    let h = image.height as usize;
    if w == 0 || h == 0 {
        return Ok(image);
    }
    let tile_w = (width as usize).clamp(1, w);
    let tile_h = (height as usize).clamp(1, h);
    let tiles_x = w.div_ceil(tile_w);
    let tiles_y = h.div_ceil(tile_h);
    let tile_count = tiles_x * tiles_y;
    let mut histograms = vec![[0u64; 256]; tile_count];
    for y in 0..h {
        let ty = y / tile_h;
        for x in 0..w {
            let tx = x / tile_w;
            let px = (y * w + x) * 4;
            let lum = luminance(image.data[px], image.data[px + 1], image.data[px + 2]);
            histograms[ty * tiles_x + tx][lum as usize] += 1;
        }
    }
    let clip_limit = if max_slope > 0.0 {
        (max_slope * (tile_w * tile_h) as f64 / 256.0).max(1.0)
    } else {
        f64::MAX
    };
    let mut luts = vec![[0u8; 256]; tile_count];
    for (tile_index, histogram) in histograms.iter_mut().enumerate() {
        let mut excess = 0u64;
        for count in histogram.iter_mut() {
            if *count as f64 > clip_limit {
                excess += *count - clip_limit as u64;
                *count = clip_limit as u64;
            }
        }
        if excess > 0 {
            let share = excess / 256;
            let remainder = excess % 256;
            for (i, count) in histogram.iter_mut().enumerate() {
                *count += share + u64::from((i as u64) < remainder);
            }
        }
        let total: u64 = histogram.iter().sum();
        let mut cumulative = 0u64;
        let mut lut = [0u8; 256];
        for (i, value) in lut.iter_mut().enumerate() {
            cumulative += histogram[i];
            *value = if total == 0 {
                i as u8
            } else {
                ((cumulative as f64 / total as f64) * 255.0).round() as u8
            };
        }
        luts[tile_index] = lut;
    }
    let mut out = image;
    out.data
        .par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(index, px)| {
            let x = index % w;
            let y = index / w;
            let lum = luminance(px[0], px[1], px[2]);
            let fx = (x as f64 + 0.5) / tile_w as f64 - 0.5;
            let fy = (y as f64 + 0.5) / tile_h as f64 - 0.5;
            let x0 = fx.floor().max(0.0) as usize;
            let y0 = fy.floor().max(0.0) as usize;
            let x1 = (x0 + 1).min(tiles_x - 1);
            let y1 = (y0 + 1).min(tiles_y - 1);
            let tx = (fx - x0 as f64).clamp(0.0, 1.0);
            let ty = (fy - y0 as f64).clamp(0.0, 1.0);
            let sample = |x: usize, y: usize| -> f64 { luts[y * tiles_x + x][lum as usize] as f64 };
            let top = sample(x0, y0) + (sample(x1, y0) - sample(x0, y0)) * tx;
            let bottom = sample(x0, y1) + (sample(x1, y1) - sample(x0, y1)) * tx;
            let new_lum = top + (bottom - top) * ty;
            if lum > 0 {
                let scale = new_lum / lum as f64;
                px[0] = (px[0] as f64 * scale).clamp(0.0, 255.0) as u8;
                px[1] = (px[1] as f64 * scale).clamp(0.0, 255.0) as u8;
                px[2] = (px[2] as f64 * scale).clamp(0.0, 255.0) as u8;
            } else {
                px[0] = new_lum.clamp(0.0, 255.0) as u8;
                px[1] = px[0];
                px[2] = px[0];
            }
        });
    Ok(out)
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
    let mut out = image.data.clone();
    out.par_chunks_exact_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..3 {
                    let mut acc = 0.0f32;
                    for ky in 0..kh {
                        let sy = (y as i32 + ky as i32 - cy).clamp(0, h as i32 - 1) as usize;
                        let source_row = sy * w;
                        for kx in 0..kw {
                            let sx = (x as i32 + kx as i32 - cx).clamp(0, w as i32 - 1) as usize;
                            acc += image.data[(source_row + sx) * 4 + c] as f32
                                * weights[ky * kw + kx];
                        }
                    }
                    row[x * 4 + c] = (acc / scale + offset as f32).clamp(0.0, 255.0) as u8;
                }
            }
        });
    let mut out_image = Image::from_rgba(image.width, image.height, out)?;
    out_image.format = image.format;
    out_image.has_alpha = image.has_alpha;
    Ok(out_image)
}

fn threshold(mut image: Image, threshold: u8, grayscale: bool) -> Result<Image> {
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let lum = luminance(px[0], px[1], px[2]);
        let value = if lum >= threshold { 255 } else { 0 };
        if grayscale {
            px[0] = value;
            px[1] = value;
            px[2] = value;
            px[3] = 255;
        } else if value == 255 {
            px[0] = 255;
            px[1] = 255;
            px[2] = 255;
        } else {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        }
    });
    image.has_alpha = false;
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
    if overlay.width != image.width || overlay.height != image.height {
        return Err(FastpkgError::Operation(
            "boolean operand dimensions must match image".into(),
        ));
    }
    let mut out = image;
    out.data
        .par_chunks_exact_mut(4)
        .zip(overlay.data.par_chunks_exact(4))
        .for_each(|(a, b)| {
            for i in 0..4 {
                a[i] = match operator {
                    "and" => a[i] & b[i],
                    "or" => a[i] | b[i],
                    "eor" | "xor" => a[i] ^ b[i],
                    _ => a[i],
                };
            }
        });
    Ok(out)
}

fn linear(mut image: Image, a: Option<&NumberOrArray>, b: Option<&NumberOrArray>) -> Result<Image> {
    let mut lut = [[0u8; 256]; 4];
    for c in 0..4 {
        let av = a.map(|v| v.value(c)).unwrap_or(1.0);
        let bv = b.map(|v| v.value(c)).unwrap_or(0.0);
        for (i, v) in lut[c].iter_mut().enumerate() {
            *v = (i as f64 * av + bv).clamp(0.0, 255.0) as u8;
        }
    }
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        for c in 0..4 {
            px[c] = lut[c][px[c] as usize];
        }
    });
    Ok(image)
}

fn recomb(mut image: Image, matrix: &[Vec<f64>]) -> Result<Image> {
    let n = matrix.len().min(4);
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

fn modulate(
    mut image: Image,
    brightness: f64,
    saturation: f64,
    hue: f64,
    lightness: f64,
) -> Result<Image> {
    let hue = hue.to_radians() as f32;
    let cos_h = hue.cos();
    let sin_h = hue.sin();
    let brightness = brightness as f32;
    let saturation = saturation as f32;
    let lightness = lightness as f32 / 255.0;
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let mut r = px[0] as f32 / 255.0;
        let mut g = px[1] as f32 / 255.0;
        let mut b = px[2] as f32 / 255.0;
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r = (lum + (r - lum) * saturation) * brightness;
        g = (lum + (g - lum) * saturation) * brightness;
        b = (lum + (b - lum) * saturation) * brightness;
        let nr = r * cos_h - g * sin_h;
        let ng = r * sin_h + g * cos_h;
        r = nr;
        g = ng;
        px[0] = ((r + lightness) * 255.0).clamp(0.0, 255.0) as u8;
        px[1] = ((g + lightness) * 255.0).clamp(0.0, 255.0) as u8;
        px[2] = ((b + lightness) * 255.0).clamp(0.0, 255.0) as u8;
    });
    Ok(image)
}

fn tint(mut image: Image, tint: &str) -> Result<Image> {
    let color = Color::parse(tint)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid tint color {tint}")))?;
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let lum = luminance(px[0], px[1], px[2]);
        px[0] = (lum as u16 * color.r as u16 / 255) as u8;
        px[1] = (lum as u16 * color.g as u16 / 255) as u8;
        px[2] = (lum as u16 * color.b as u16 / 255) as u8;
    });
    Ok(image)
}

fn greyscale(mut image: Image) -> Result<Image> {
    image.data.par_chunks_exact_mut(4).for_each(|px| {
        let lum = luminance(px[0], px[1], px[2]);
        px[0] = lum;
        px[1] = lum;
        px[2] = lum;
    });
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
    .min(3);
    let mut out = image.data.clone();
    out.par_chunks_exact_mut(4)
        .zip(image.data.par_chunks_exact(4))
        .for_each(|(dst, src)| {
            dst[0] = src[channel];
            dst[1] = src[channel];
            dst[2] = src[channel];
            dst[3] = 255;
        });
    image.data = out;
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
) -> Result<Image> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| FastpkgError::InvalidOptions(format!("invalid composite base64: {e}")))?;
    let overlay = fastpkg_codecs::decode(&bytes, Format::from_magic(&bytes), &Default::default())?;
    let mut out = image;
    out.ensure_alpha(255);
    if tile {
        let mut y = 0;
        while y < out.height {
            let mut x = 0;
            while x < out.width {
                blend_at(&mut out, &overlay, x as i32, y as i32, blend);
                x += overlay.width;
            }
            y += overlay.height;
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
    blend_at(&mut out, &overlay, x, y, blend);
    Ok(out)
}

fn blend_at(dst: &mut Image, src: &Image, ox: i32, oy: i32, blend: Blend) {
    for sy in 0..src.height {
        for sx in 0..src.width {
            let dx = ox + sx as i32;
            let dy = oy + sy as i32;
            if dx < 0 || dy < 0 || dx >= dst.width as i32 || dy >= dst.height as i32 {
                continue;
            }
            let s = src.get(sx, sy);
            let d = dst.get(dx as u32, dy as u32);
            let color = blend_pixel(d, s, blend);
            dst.set(dx as u32, dy as u32, color);
        }
    }
}

fn blend_pixel(d: Color, s: Color, blend: Blend) -> Color {
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
    let source = [s.r as f64 / 255.0, s.g as f64 / 255.0, s.b as f64 / 255.0];
    let backdrop = [d.r as f64 / 255.0, d.g as f64 / 255.0, d.b as f64 / 255.0];

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
            Blend::Add => (cb + cs).min(1.0),
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

    let blended = if blend == Blend::Saturate {
        set_saturation(backdrop, source)
    } else {
        [
            separable(blend, backdrop[0], source[0]),
            separable(blend, backdrop[1], source[1]),
            separable(blend, backdrop[2], source[2]),
        ]
    };

    let (fa, fb, use_blend) = match blend {
        Blend::Over => (1.0, 1.0 - sa, true),
        Blend::In => (da, 0.0, false),
        Blend::Out => (1.0 - da, 0.0, false),
        Blend::Atop => (da, 1.0 - sa, false),
        Blend::DestOver => (1.0 - da, 1.0, false),
        Blend::DestIn => (0.0, sa, false),
        Blend::DestOut => (0.0, 1.0 - sa, false),
        Blend::DestAtop => (1.0 - da, sa, false),
        Blend::Xor => (1.0 - da, 1.0 - sa, false),
        _ => (1.0, 1.0 - sa, true),
    };

    let out_a = if use_blend {
        sa + da * (1.0 - sa)
    } else {
        sa * fa + da * fb
    };
    let channel = |index: usize| -> u8 {
        if out_a <= 0.0 {
            return 0;
        }
        let value = if use_blend {
            sa * da * blended[index]
                + sa * (1.0 - da) * source[index]
                + (1.0 - sa) * da * backdrop[index]
        } else {
            sa * fa * source[index] + da * fb * backdrop[index]
        };
        (value / out_a * 255.0).clamp(0.0, 255.0).round() as u8
    };

    Color {
        r: channel(0),
        g: channel(1),
        b: channel(2),
        alpha: (out_a * 255.0).round() as u8,
    }
}

fn rgb_to_hsl(rgb: [f64; 3]) -> [f64; 3] {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-9 {
        return [0.0, 0.0, l];
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == rgb[0] {
        (rgb[1] - rgb[2]) / d + if rgb[1] < rgb[2] { 6.0 } else { 0.0 }
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / d + 2.0
    } else {
        (rgb[0] - rgb[1]) / d + 4.0
    };
    [h / 6.0, s, l]
}

fn hsl_to_rgb(hsl: [f64; 3]) -> [f64; 3] {
    if hsl[1] <= 0.0 {
        return [hsl[2], hsl[2], hsl[2]];
    }
    let q = if hsl[2] < 0.5 {
        hsl[2] * (1.0 + hsl[1])
    } else {
        hsl[2] + hsl[1] - hsl[2] * hsl[1]
    };
    let p = 2.0 * hsl[2] - q;
    let hue = |mut t: f64| -> f64 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [
        hue(hsl[0] + 1.0 / 3.0),
        hue(hsl[0]),
        hue(hsl[0] - 1.0 / 3.0),
    ]
}

fn set_saturation(backdrop: [f64; 3], source: [f64; 3]) -> [f64; 3] {
    let mut hsl = rgb_to_hsl(backdrop);
    let source_hsl = rgb_to_hsl(source);
    hsl[1] = source_hsl[1];
    hsl_to_rgb(hsl)
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
                let lum = luminance(px[0], px[1], px[2]);
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
            image.discard_wide();
            image.data.par_chunks_exact_mut(4).for_each(|px| {
                let lum = luminance(px[0], px[1], px[2]);
                px[0] = lum;
                px[1] = lum;
                px[2] = lum;
                px[3] = 255;
            });
            image.gray = true;
            image.has_alpha = false;
            Ok(())
        }
        other => Err(FastpkgError::UnsupportedOutput(format!(
            "colourspace {other}"
        ))),
    }
}
