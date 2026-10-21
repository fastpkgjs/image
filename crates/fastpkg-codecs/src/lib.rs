#![allow(clippy::chunks_exact_to_as_chunks)]

use std::io::Cursor;

use base64::Engine;
use fastpkg_core::options::{InputOptions, OutputOptions, RawInput};
use fastpkg_core::{validate_dimensions, FastpkgError, Format, HeaderInfo, Image, Limits, Result};
#[cfg(feature = "jxl-encode")]
use gamut_core::{Dimensions, EncodeImage, Gray8, ImageRef, Rgb8, Rgba8};
#[cfg(feature = "jxl-encode")]
use gamut_jxl::{ColorSpec, Container, Distance, Effort, JxlEncoder};
use image::codecs::avif::AvifEncoder;
use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::qoi::QoiEncoder;
use image::codecs::webp::WebPEncoder;
use image::{Delay, DynamicImage, ExtendedColorType, Frame, ImageDecoder, ImageEncoder};

pub mod svg;

pub fn probe(bytes: &[u8], input: &InputOptions) -> Result<HeaderInfo> {
    if let Some(raw) = &input.raw {
        return Ok(HeaderInfo {
            format: Format::Raw,
            width: raw.width,
            height: raw.height,
            channels: raw.channels,
            has_alpha: raw.channels == 2 || raw.channels == 4,
            depth: if raw_is_16(raw) {
                "ushort".into()
            } else {
                "uchar".into()
            },
            space: "srgb".into(),
            size: bytes.len(),
            pages: 1,
            page_height: raw.height,
            ..Default::default()
        });
    }
    if let Some(create) = &input.create {
        return Ok(HeaderInfo {
            format: Format::Raw,
            width: create.width,
            height: create.height,
            channels: create.channels,
            has_alpha: create.channels == 2 || create.channels == 4,
            depth: "uchar".into(),
            space: "srgb".into(),
            pages: 1,
            page_height: create.height,
            ..Default::default()
        });
    }
    if input.text.is_some() {
        return Ok(HeaderInfo {
            format: Format::Raw,
            width: 0,
            height: 0,
            channels: 4,
            has_alpha: true,
            depth: "uchar".into(),
            space: "srgb".into(),
            pages: 1,
            ..Default::default()
        });
    }

    let format = Format::from_magic(bytes);
    if matches!(
        format,
        Format::Tiff | Format::Jp2 | Format::Heif | Format::Pdf
    ) {
        return Err(FastpkgError::UnsupportedInput);
    }
    let mut header = match format {
        Format::Svg => svg::probe(bytes)?,
        Format::Jxl => probe_jxl(bytes)?,
        _ => probe_generic(bytes, format)?,
    };
    header.format = format;
    header.size = bytes.len();
    let (has_exif, has_icc, has_xmp) = match format {
        Format::Jpeg => jpeg_flags(bytes),
        Format::Png => png_flags(bytes),
        Format::Webp => webp_flags(bytes),
        _ => (false, false, false),
    };
    header.has_exif = has_exif;
    header.has_profile = has_icc;
    header.has_xmp = has_xmp;
    if has_exif {
        header.orientation = read_orientation(bytes);
    }
    validate_dimensions(
        header.width,
        header.height,
        &Limits {
            max_pixels: if input.unlimited {
                u64::MAX
            } else {
                input.limit_input_pixels
            },
            ..Default::default()
        },
    )?;
    Ok(header)
}

fn probe_generic(bytes: &[u8], format: Format) -> Result<HeaderInfo> {
    let size = imagesize::blob_size(bytes).map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let mut header = HeaderInfo {
        format,
        width: size.width as u32,
        height: size.height as u32,
        channels: 3,
        has_alpha: format.supports_alpha(),
        depth: "8-bit".into(),
        space: "srgb".into(),
        pages: 1,
        page_height: size.height as u32,
        ..Default::default()
    };
    match format {
        Format::Png => {
            if bytes.len() >= 26 && &bytes[12..16] == b"IHDR" {
                header.depth = if bytes[24] == 16 {
                    "ushort".into()
                } else {
                    "uchar".into()
                };
                let color_type = bytes[25];
                header.is_palette = color_type == 3;
                header.has_alpha = matches!(color_type, 4 | 6);
                header.channels = match color_type {
                    0 => 1,
                    2 => 3,
                    3 => 3,
                    4 => 2,
                    _ => 4,
                };
            }
        }
        Format::Jpeg => {
            if let Some((w, h, channels, progressive)) = jpeg_sof(bytes) {
                header.width = w;
                header.height = h;
                header.page_height = h;
                header.channels = channels;
                header.is_progressive = progressive;
            }
        }
        Format::Webp => header.has_alpha = webp_alpha(bytes),
        Format::Gif => {
            header.channels = 4;
            header.has_alpha = true;
            header.is_palette = true;
        }
        Format::Qoi if bytes.len() >= 14 => {
            header.width = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            header.height = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            header.channels = bytes[12];
            header.has_alpha = bytes[12] == 4;
            header.page_height = header.height;
        }
        Format::Avif => {
            header.channels = 4;
            header.has_alpha = true;
        }
        _ => {}
    }
    Ok(header)
}

fn probe_jxl(bytes: &[u8]) -> Result<HeaderInfo> {
    use jxl_oxide::integration::JxlDecoder;
    let decoder =
        JxlDecoder::new(Cursor::new(bytes)).map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    let color = decoder.color_type();
    Ok(HeaderInfo {
        format: Format::Jxl,
        width,
        height,
        channels: color.channel_count(),
        has_alpha: color.has_alpha(),
        depth: format!(
            "{}-bit",
            color.bits_per_pixel() / color.channel_count() as u16
        ),
        space: "srgb".into(),
        pages: 1,
        page_height: height,
        ..Default::default()
    })
}

pub fn decode(bytes: &[u8], format: Format, input: &InputOptions) -> Result<Image> {
    decode_with_hint(bytes, format, input, None, false)
}

pub fn decode_with_hint(
    bytes: &[u8],
    format: Format,
    input: &InputOptions,
    hint: Option<(u32, u32)>,
    fast_shrink: bool,
) -> Result<Image> {
    if let Some(raw) = &input.raw {
        return decode_raw(bytes, raw);
    }
    if let Some(create) = &input.create {
        return create_image(
            create.width,
            create.height,
            create.channels,
            &create.background,
        );
    }
    if let Some(text) = &input.text {
        return svg::text_image(text);
    }
    let format = if format == Format::Unknown {
        Format::from_magic(bytes)
    } else {
        format
    };
    let mut image = match format {
        Format::Avif => decode_avif(bytes)?,
        Format::Jxl => decode_jxl(bytes)?,
        Format::Svg => svg::decode(bytes)?,
        Format::Png => decode_png(bytes)?,
        Format::Jpeg if hint.is_some() => decode_jpeg_scaled(bytes, hint.unwrap(), fast_shrink)?,
        Format::Raw => {
            return Err(FastpkgError::InvalidOptions(
                "raw input requires raw width/height/channels".into(),
            ))
        }
        Format::Tiff | Format::Jp2 | Format::Heif | Format::Pdf => {
            return Err(FastpkgError::UnsupportedInput)
        }
        other => {
            let image_format = to_image_format(other).ok_or(FastpkgError::UnsupportedInput)?;
            let dynamic = image::load_from_memory_with_format(bytes, image_format)
                .map_err(|e| FastpkgError::Decode(format!("{}: {e}", other.mime())))?;
            dynamic_to_image(dynamic, other)?
        }
    };
    image.format = format;
    if let Some(orientation) = read_orientation(bytes) {
        image.orientation = Some(orientation);
    }
    Ok(image)
}

fn decode_raw(bytes: &[u8], raw: &RawInput) -> Result<Image> {
    let wide = raw_is_16(raw);
    let bytes_per_sample = if wide { 2 } else { 1 };
    let expected =
        raw.width as usize * raw.height as usize * raw.channels as usize * bytes_per_sample;
    if bytes.len() < expected {
        return Err(FastpkgError::Decode(format!(
            "raw buffer is {} bytes, expected at least {}",
            bytes.len(),
            expected
        )));
    }
    validate_dimensions(raw.width, raw.height, &Limits::default())?;
    let mut image = if wide {
        let samples = bytes[..expected]
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect::<Vec<u16>>();
        Image::from_raw16(raw.width, raw.height, raw.channels, &samples)?
    } else {
        Image::from_raw(
            raw.width,
            raw.height,
            raw.channels,
            bytes[..expected].to_vec(),
        )?
    };
    image.format = Format::Raw;
    if raw.premultiplied {
        image.unpremultiply();
    }
    Ok(image)
}

fn raw_is_16(raw: &RawInput) -> bool {
    raw.depth
        .as_deref()
        .map(|depth| {
            matches!(
                depth.to_ascii_lowercase().as_str(),
                "ushort" | "uint16" | "16" | "rgb16" | "rgba16" | "grey16" | "gray16"
            )
        })
        .unwrap_or(false)
}

fn decode_jxl(bytes: &[u8]) -> Result<Image> {
    use jxl_oxide::integration::JxlDecoder;
    let decoder =
        JxlDecoder::new(Cursor::new(bytes)).map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let dynamic =
        DynamicImage::from_decoder(decoder).map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let mut image = dynamic_to_image(dynamic, Format::Jxl)?;
    image.format = Format::Jxl;
    Ok(image)
}

fn decode_avif(bytes: &[u8]) -> Result<Image> {
    let decoder =
        avif_decode::Decoder::from_avif(bytes).map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let decoded = decoder
        .to_image()
        .map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let (data, width, height, alpha) = match decoded {
        avif_decode::Image::Rgba8(img) => {
            let (buf, w, h) = img.into_contiguous_buf();
            let mut data = Vec::with_capacity(buf.len() * 4);
            for px in buf {
                data.extend_from_slice(&[px.r, px.g, px.b, px.a]);
            }
            (data, w as u32, h as u32, true)
        }
        avif_decode::Image::Rgb8(img) => {
            let (buf, w, h) = img.into_contiguous_buf();
            let mut data = Vec::with_capacity(buf.len() * 4);
            for px in buf {
                data.extend_from_slice(&[px.r, px.g, px.b, 255]);
            }
            (data, w as u32, h as u32, false)
        }
        avif_decode::Image::Gray8(img) => {
            let (buf, w, h) = img.into_contiguous_buf();
            let mut data = Vec::with_capacity(buf.len() * 4);
            for px in buf {
                data.extend_from_slice(&[px.value(), px.value(), px.value(), 255]);
            }
            (data, w as u32, h as u32, false)
        }
        avif_decode::Image::Rgba16(img) => {
            let (buf, w, h) = img.into_contiguous_buf();
            let mut data = Vec::with_capacity(buf.len() * 4);
            for px in buf {
                data.extend_from_slice(&[
                    (px.r >> 8) as u8,
                    (px.g >> 8) as u8,
                    (px.b >> 8) as u8,
                    (px.a >> 8) as u8,
                ]);
            }
            (data, w as u32, h as u32, true)
        }
        avif_decode::Image::Rgb16(img) => {
            let (buf, w, h) = img.into_contiguous_buf();
            let mut data = Vec::with_capacity(buf.len() * 4);
            for px in buf {
                data.extend_from_slice(&[
                    (px.r >> 8) as u8,
                    (px.g >> 8) as u8,
                    (px.b >> 8) as u8,
                    255,
                ]);
            }
            (data, w as u32, h as u32, false)
        }
        avif_decode::Image::Gray16(img) => {
            let (buf, w, h) = img.into_contiguous_buf();
            let mut data = Vec::with_capacity(buf.len() * 4);
            for px in buf {
                let v = (px.value() >> 8) as u8;
                data.extend_from_slice(&[v, v, v, 255]);
            }
            (data, w as u32, h as u32, false)
        }
    };
    let mut image = Image::from_rgba(width, height, data)?;
    image.has_alpha = alpha;
    image.format = Format::Avif;
    Ok(image)
}

fn decode_jpeg_scaled(bytes: &[u8], hint: (u32, u32), fast_shrink: bool) -> Result<Image> {
    if let Some(image) = decode_jpeg_mozjpeg(bytes, hint, fast_shrink) {
        return Ok(image);
    }
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|e| FastpkgError::Decode(e.to_string()))?;
    dynamic_to_image(image, Format::Jpeg)
}

fn decode_jpeg_mozjpeg(bytes: &[u8], hint: (u32, u32), fast_shrink: bool) -> Option<Image> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut decompress = mozjpeg::Decompress::new_mem(bytes).ok()?;
        let source_width = decompress.width();
        let source_height = decompress.height();
        if source_width == 0 || source_height == 0 {
            return None;
        }
        let ratio = (hint.0 as f64 / source_width as f64).min(hint.1 as f64 / source_height as f64);
        let scaled = ratio * 8.0;
        let scale = if fast_shrink {
            scaled.floor()
        } else {
            scaled.ceil()
        }
        .clamp(1.0, 8.0) as u8;
        decompress.scale(scale);
        let mut started = decompress.rgb().ok()?;
        let width = started.width() as u32;
        let height = started.height() as u32;
        let pixels: Vec<[u8; 3]> = started.read_scanlines().ok()?;
        let mut data = Vec::with_capacity(pixels.len() * 4);
        for px in pixels {
            data.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        let mut image = Image::from_rgba(width, height, data).ok()?;
        image.has_alpha = false;
        image.format = Format::Jpeg;
        Some(image)
    }))
    .ok()
    .flatten()
}

fn decode_png(bytes: &[u8]) -> Result<Image> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let buffer_size = reader
        .output_buffer_size()
        .ok_or_else(|| FastpkgError::Decode("PNG output buffer is too large".into()))?;
    let mut buffer = vec![0; buffer_size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| FastpkgError::Decode(e.to_string()))?;
    let raw = &buffer[..info.buffer_size()];
    let width = info.width;
    let height = info.height;
    let pixels = width as usize * height as usize;
    let color_type = info.color_type;
    let gray = matches!(
        color_type,
        png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha
    );
    if info.bit_depth == png::BitDepth::Sixteen {
        let row_bytes = info.line_size;
        let mut wide = vec![0u16; pixels * 4];
        for y in 0..height as usize {
            let row = &raw[y * row_bytes..(y + 1) * row_bytes];
            let mut offset = 0;
            for x in 0..width as usize {
                let dst = (y * width as usize + x) * 4;
                let (r, g, b, a) = match color_type {
                    png::ColorType::Grayscale => {
                        let v = read_be_u16(row, offset);
                        offset += 2;
                        (v, v, v, 65_535)
                    }
                    png::ColorType::GrayscaleAlpha => {
                        let v = read_be_u16(row, offset);
                        let a = read_be_u16(row, offset + 2);
                        offset += 4;
                        (v, v, v, a)
                    }
                    png::ColorType::Rgb => {
                        let r = read_be_u16(row, offset);
                        let g = read_be_u16(row, offset + 2);
                        let b = read_be_u16(row, offset + 4);
                        offset += 6;
                        (r, g, b, 65_535)
                    }
                    png::ColorType::Rgba => {
                        let r = read_be_u16(row, offset);
                        let g = read_be_u16(row, offset + 2);
                        let b = read_be_u16(row, offset + 4);
                        let a = read_be_u16(row, offset + 6);
                        offset += 8;
                        (r, g, b, a)
                    }
                    _ => return Err(FastpkgError::Decode("unsupported 16-bit PNG layout".into())),
                };
                wide[dst..dst + 4].copy_from_slice(&[r, g, b, a]);
            }
        }
        let mut image = Image::from_rgba16(width, height, wide)?;
        image.has_alpha = matches!(
            color_type,
            png::ColorType::GrayscaleAlpha | png::ColorType::Rgba
        );
        image.gray = gray;
        image.format = Format::Png;
        return Ok(image);
    }
    let mut rgba = vec![255u8; pixels * 4];
    let row_bytes = info.line_size;
    for y in 0..height as usize {
        let row = &raw[y * row_bytes..(y + 1) * row_bytes];
        let mut offset = 0;
        for x in 0..width as usize {
            let dst = (y * width as usize + x) * 4;
            match color_type {
                png::ColorType::Grayscale => {
                    let v = row[offset];
                    offset += 1;
                    rgba[dst..dst + 4].copy_from_slice(&[v, v, v, 255]);
                }
                png::ColorType::GrayscaleAlpha => {
                    let v = row[offset];
                    let a = row[offset + 1];
                    offset += 2;
                    rgba[dst..dst + 4].copy_from_slice(&[v, v, v, a]);
                }
                png::ColorType::Rgb => {
                    rgba[dst..dst + 4].copy_from_slice(&[
                        row[offset],
                        row[offset + 1],
                        row[offset + 2],
                        255,
                    ]);
                    offset += 3;
                }
                png::ColorType::Rgba => {
                    rgba[dst..dst + 4].copy_from_slice(&row[offset..offset + 4]);
                    offset += 4;
                }
                _ => return Err(FastpkgError::Decode("unsupported PNG layout".into())),
            }
        }
    }
    let mut image = Image::from_rgba(width, height, rgba)?;
    image.has_alpha = matches!(
        color_type,
        png::ColorType::GrayscaleAlpha | png::ColorType::Rgba
    );
    image.gray = gray;
    image.format = Format::Png;
    Ok(image)
}

fn read_be_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn dynamic_to_image(dynamic: DynamicImage, format: Format) -> Result<Image> {
    let has_alpha = dynamic.color().has_alpha();
    let is_gray = dynamic.color().channel_count() == 1;
    let (width, height) = (dynamic.width(), dynamic.height());
    validate_dimensions(width, height, &Limits::default())?;
    let rgba = dynamic.to_rgba8();
    let mut image = Image::from_rgba(width, height, rgba.into_raw())?;
    image.has_alpha = has_alpha || image.has_transparency();
    image.gray = is_gray && !image.has_alpha;
    image.format = format;
    Ok(image)
}

fn create_image(width: u32, height: u32, channels: u8, background: &str) -> Result<Image> {
    validate_dimensions(width, height, &Limits::default())?;
    let color = fastpkg_core::Color::parse(background)
        .ok_or_else(|| FastpkgError::InvalidOptions(format!("invalid color {background}")))?;
    let count = width as usize * height as usize;
    let mut image = match channels {
        1 => Image::from_luma(width, height, vec![color.r; count])?,
        3 => {
            let mut data = Vec::with_capacity(count * 3);
            for _ in 0..count {
                data.extend_from_slice(&[color.r, color.g, color.b]);
            }
            Image::from_rgb(width, height, data)?
        }
        _ => {
            let mut data = Vec::with_capacity(count * 4);
            for _ in 0..count {
                data.extend_from_slice(&[color.r, color.g, color.b, color.alpha]);
            }
            Image::from_rgba(width, height, data)?
        }
    };
    image.format = Format::Raw;
    Ok(image)
}

pub fn encode(
    image: &Image,
    options: &OutputOptions,
    input_format: Format,
) -> Result<(Vec<u8>, Format)> {
    let format = options.resolved_format(input_format);
    if !format.is_supported_output() {
        return Err(FastpkgError::UnsupportedOutput(format!("{format:?}")));
    }
    let mut out = Vec::new();
    match format {
        Format::Jpeg => encode_jpeg(image, options, &mut out)?,
        Format::Png => encode_png(image, options, &mut out)?,
        Format::Webp => encode_webp(image, options, &mut out)?,
        Format::Avif => encode_avif(image, options, &mut out)?,
        Format::Gif => encode_gif(image, options, &mut out)?,
        Format::Qoi => encode_qoi(image, options, &mut out)?,
        Format::Jxl => encode_jxl(image, options, &mut out)?,
        Format::Raw => {
            if output_is_16(options) {
                let wide = if image.has_alpha {
                    image.to_rgba16()
                } else {
                    image.to_rgb16()
                };
                out = Vec::with_capacity(wide.len() * 2);
                for value in wide {
                    out.extend_from_slice(&value.to_ne_bytes());
                }
            } else {
                out = if image.has_alpha {
                    image.data.clone()
                } else {
                    image.to_rgb()
                };
            }
        }
        _ => return Err(FastpkgError::UnsupportedOutput(format!("{format:?}"))),
    }
    Ok((out, format))
}

fn encode_jpeg(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let quality = options.quality.clamp(1, 100);
    let grayscale = image.gray;
    let pixels = if grayscale {
        image
            .data
            .chunks_exact(4)
            .map(|px| px[0])
            .collect::<Vec<u8>>()
    } else {
        image.to_rgb()
    };
    if image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
        return Err(FastpkgError::Encode(
            "JPEG dimensions exceed 65535 pixels".into(),
        ));
    }
    let encoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut compress = mozjpeg::Compress::new(if grayscale {
            mozjpeg::ColorSpace::JCS_GRAYSCALE
        } else {
            mozjpeg::ColorSpace::JCS_RGB
        });
        compress.set_size(image.width as usize, image.height as usize);
        compress.set_fastest_defaults();
        compress.set_quality(quality as f32);
        compress.set_optimize_coding(options.optimise_coding);
        if options.progressive {
            compress.set_progressive_mode();
            if options.optimise_scans {
                compress.set_optimize_scans(true);
            }
        }
        if let Some(chroma) = options.chroma_subsampling.as_deref() {
            match chroma {
                "4:4:4" | "444" => compress.set_chroma_sampling_pixel_sizes((1, 1), (1, 1)),
                "4:2:2" | "422" => compress.set_chroma_sampling_pixel_sizes((2, 1), (2, 1)),
                "4:2:0" | "420" => compress.set_chroma_sampling_pixel_sizes((2, 2), (2, 2)),
                _ => {}
            }
        }
        let mut compress = compress.start_compress(Vec::new()).ok()?;
        compress.write_scanlines(&pixels).ok()?;
        compress.finish().ok()
    }))
    .ok()
    .flatten();
    match encoded {
        Some(data) => *out = data,
        None => {
            let mut encoder = jpeg_encoder::Encoder::new(&mut *out, quality);
            encoder.set_progressive(options.progressive);
            encoder.set_optimized_huffman_tables(options.optimise_coding);
            encoder
                .encode(
                    &pixels,
                    image.width as u16,
                    image.height as u16,
                    if grayscale {
                        jpeg_encoder::ColorType::Luma
                    } else {
                        jpeg_encoder::ColorType::Rgb
                    },
                )
                .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        }
    }
    let orientation = options
        .with_metadata
        .as_ref()
        .and_then(|metadata| metadata.orientation)
        .or_else(|| exif_orientation_value(options.with_exif.as_ref()))
        .or_else(|| exif_orientation_value(options.with_exif_merge.as_ref()));
    if let Some(orientation) = orientation {
        write_jpeg_orientation(out, orientation);
    }
    if let Some(density) = options.with_density.or_else(|| {
        options
            .with_metadata
            .as_ref()
            .and_then(|metadata| metadata.density)
    }) {
        write_jpeg_density(out, density);
    }
    write_jpeg_metadata(out, image, options);
    Ok(())
}

fn exif_orientation_value(value: Option<&serde_json::Value>) -> Option<u32> {
    value.and_then(|exif| {
        exif.get("Orientation")
            .or_else(|| exif.get("orientation"))
            .and_then(|value| value.as_u64())
            .map(|value| value as u32)
    })
}

fn insert_jpeg_segment(data: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    if data.len() < 2 || data[0] != 0xff || data[1] != 0xd8 {
        return;
    }
    if payload.len() + 2 > u16::MAX as usize {
        return;
    }
    let mut segment = Vec::with_capacity(payload.len() + 4);
    segment.extend_from_slice(&[0xff, marker]);
    segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(payload);
    data.splice(2..2, segment);
}

fn write_jpeg_orientation(data: &mut Vec<u8>, orientation: u32) {
    if data.len() < 2 || data[0] != 0xff || data[1] != 0xd8 {
        return;
    }
    let mut tiff = Vec::with_capacity(32);
    tiff.extend_from_slice(b"II*\x00");
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&(orientation as u16).to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(&tiff);
    insert_jpeg_segment(data, 0xe1, &payload);
}

fn write_jpeg_density(data: &mut Vec<u8>, dpi: u32) {
    if data.len() < 20 || data[0] != 0xff || data[1] != 0xd8 {
        return;
    }
    let dpi = dpi.clamp(1, u16::MAX as u32) as u16;
    if data[2] == 0xff && data[3] == 0xe0 && &data[6..11] == b"JFIF\x00" {
        data[13] = 1;
        data[14..16].copy_from_slice(&dpi.to_be_bytes());
        data[16..18].copy_from_slice(&dpi.to_be_bytes());
    } else {
        let mut segment = vec![
            0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x01,
        ];
        segment.extend_from_slice(&dpi.to_be_bytes());
        segment.extend_from_slice(&dpi.to_be_bytes());
        segment.extend_from_slice(&[0x00, 0x00]);
        data.splice(2..2, segment);
    }
}

fn write_jpeg_metadata(data: &mut Vec<u8>, image: &Image, options: &OutputOptions) {
    if options.keep_exif || options.keep_metadata {
        if let Some(exif) = image.exif.as_ref() {
            let mut payload = b"Exif\x00\x00".to_vec();
            payload.extend_from_slice(exif);
            insert_jpeg_segment(data, 0xe1, &payload);
        }
    }
    if options.keep_xmp || options.keep_metadata {
        if let Some(xmp) = image.xmp.as_ref() {
            let mut payload = b"http://ns.adobe.com/xap/1.0/\x00".to_vec();
            payload.extend_from_slice(xmp);
            insert_jpeg_segment(data, 0xe1, &payload);
        }
    }
    if let Some(raw) = options.with_exif_raw.as_ref() {
        if let Ok(exif) = base64::engine::general_purpose::STANDARD.decode(raw) {
            let mut payload = b"Exif\x00\x00".to_vec();
            payload.extend_from_slice(&exif);
            insert_jpeg_segment(data, 0xe1, &payload);
        }
    }
    if let Some(raw) = options.with_icc_raw.as_ref() {
        if let Ok(icc) = base64::engine::general_purpose::STANDARD.decode(raw) {
            write_jpeg_icc(data, &icc);
        }
    }
    if let Some(xmp) = options.with_xmp.as_ref() {
        let mut payload = b"http://ns.adobe.com/xap/1.0/\x00".to_vec();
        payload.extend_from_slice(xmp.as_bytes());
        insert_jpeg_segment(data, 0xe1, &payload);
    }
    if options.keep_icc || options.keep_metadata {
        if let Some(icc) = image.icc.as_ref() {
            write_jpeg_icc(data, icc);
        }
    }
}

fn write_jpeg_icc(data: &mut Vec<u8>, profile: &[u8]) {
    const CHUNK: usize = 65519;
    let count = profile.len().div_ceil(CHUNK).max(1) as u8;
    for (index, chunk) in profile.chunks(CHUNK).enumerate() {
        let mut payload = b"ICC_PROFILE\x00".to_vec();
        payload.push(index as u8 + 1);
        payload.push(count);
        payload.extend_from_slice(chunk);
        insert_jpeg_segment(data, 0xe2, &payload);
    }
}

pub fn output_is_16(options: &OutputOptions) -> bool {
    options.bitdepth == Some(16)
        || options.depth.as_deref().is_some_and(is_16bit_name)
        || options.raw_depth.as_deref().is_some_and(is_16bit_name)
        || options.colourspace.as_deref().is_some_and(is_16bit_name)
}

fn is_16bit_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "ushort" | "uint16" | "16" | "rgb16" | "rgba16" | "grey16" | "gray16"
    )
}

fn encode_png(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    if options.palette {
        encode_png_palette(image, options, out)?;
        write_png_metadata(out, image, options);
        if let Some(density) = options.with_density.or_else(|| {
            options
                .with_metadata
                .as_ref()
                .and_then(|metadata| metadata.density)
        }) {
            write_png_density(out, density);
        }
        return Ok(());
    }
    if output_is_16(options) {
        encode_png_16(image, options, out)?;
        write_png_metadata(out, image, options);
        if let Some(density) = options.with_density.or_else(|| {
            options
                .with_metadata
                .as_ref()
                .and_then(|metadata| metadata.density)
        }) {
            write_png_density(out, density);
        }
        return Ok(());
    }
    let compression = match options.compression_level {
        0..=6 => png::Compression::Fast,
        7..=8 => png::Compression::Balanced,
        _ => png::Compression::High,
    };
    let mut encoder = png::Encoder::new(&mut *out, image.width, image.height);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(compression);
    encoder.set_filter(png::Filter::Adaptive);
    let result = if image.gray {
        encoder.set_color(png::ColorType::Grayscale);
        let luma = image
            .data
            .chunks_exact(4)
            .map(|px| px[0])
            .collect::<Vec<u8>>();
        let mut writer = encoder
            .write_header()
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        writer
            .write_image_data(&luma)
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    } else if image.has_alpha {
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder
            .write_header()
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        writer
            .write_image_data(&image.data)
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    } else {
        encoder.set_color(png::ColorType::Rgb);
        let rgb = image.to_rgb();
        let mut writer = encoder
            .write_header()
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        writer
            .write_image_data(&rgb)
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    };
    result?;
    write_png_metadata(out, image, options);
    if let Some(density) = options.with_density.or_else(|| {
        options
            .with_metadata
            .as_ref()
            .and_then(|metadata| metadata.density)
    }) {
        write_png_density(out, density);
    }
    Ok(())
}

fn encode_png_16(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let wide = image.to_rgba16();
    let mut samples = Vec::new();
    let color_type = if image.gray {
        if image.has_alpha {
            for px in wide.chunks_exact(4) {
                samples.extend_from_slice(&px[0].to_be_bytes());
                samples.extend_from_slice(&px[3].to_be_bytes());
            }
            png::ColorType::GrayscaleAlpha
        } else {
            for px in wide.chunks_exact(4) {
                samples.extend_from_slice(&px[0].to_be_bytes());
            }
            png::ColorType::Grayscale
        }
    } else if image.has_alpha {
        for px in wide.chunks_exact(4) {
            for value in px {
                samples.extend_from_slice(&value.to_be_bytes());
            }
        }
        png::ColorType::Rgba
    } else {
        for px in wide.chunks_exact(4) {
            for value in &px[..3] {
                samples.extend_from_slice(&value.to_be_bytes());
            }
        }
        png::ColorType::Rgb
    };
    let mut encoder = png::Encoder::new(&mut *out, image.width, image.height);
    encoder.set_color(color_type);
    encoder.set_depth(png::BitDepth::Sixteen);
    encoder.set_compression(match options.compression_level {
        0..=6 => png::Compression::Fast,
        7..=8 => png::Compression::Balanced,
        _ => png::Compression::High,
    });
    encoder.set_filter(png::Filter::Adaptive);
    let mut writer = encoder
        .write_header()
        .map_err(|e| FastpkgError::Encode(e.to_string()))?;
    writer
        .write_image_data(&samples)
        .map_err(|e| FastpkgError::Encode(e.to_string()))
}

fn encode_png_palette(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let colors = (options.colors as usize).clamp(2, 256);
    let quant = color_quant::NeuQuant::new(10, colors, &image.data);
    let palette_rgba = quant.color_map_rgba();
    let palette_len = palette_rgba.len() / 4;
    let mut palette = Vec::with_capacity(palette_len * 3);
    let mut alpha = Vec::with_capacity(palette_len);
    let mut has_alpha = false;
    for px in palette_rgba.chunks_exact(4) {
        palette.extend_from_slice(&px[..3]);
        alpha.push(px[3]);
        has_alpha |= px[3] < 255;
    }
    const BAYER: [[i32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let dither = options.dither.clamp(0.0, 1.0) * 32.0;
    let width = image.width as usize;
    let mut indices = Vec::with_capacity(image.pixel_count());
    for (index, px) in image.data.chunks_exact(4).enumerate() {
        let x = index % width;
        let y = index / width;
        let offset = (BAYER[y % 4][x % 4] as f64 / 16.0 - 0.5) * dither;
        let pixel = [
            (px[0] as f64 + offset).clamp(0.0, 255.0) as u8,
            (px[1] as f64 + offset).clamp(0.0, 255.0) as u8,
            (px[2] as f64 + offset).clamp(0.0, 255.0) as u8,
            px[3],
        ];
        indices.push(quant.index_of(&pixel) as u8);
    }
    let mut encoder = png::Encoder::new(&mut *out, image.width, image.height);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(palette);
    if has_alpha {
        encoder.set_trns(alpha);
    }
    encoder.set_compression(match options.compression_level {
        0..=6 => png::Compression::Fast,
        7..=8 => png::Compression::Balanced,
        _ => png::Compression::High,
    });
    let mut writer = encoder
        .write_header()
        .map_err(|e| FastpkgError::Encode(e.to_string()))?;
    writer
        .write_image_data(&indices)
        .map_err(|e| FastpkgError::Encode(e.to_string()))
}

fn encode_webp(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    if options.lossless {
        let mut encoder = WebPEncoder::new_lossless(&mut *out);
        if let Some(exif) = image
            .exif
            .as_ref()
            .filter(|_| options.keep_exif || options.keep_metadata)
        {
            let _ = encoder.set_exif_metadata(exif.clone());
        }
        if let Some(icc) = image
            .icc
            .as_ref()
            .filter(|_| options.keep_icc || options.keep_metadata)
        {
            let _ = encoder.set_icc_profile(icc.clone());
        }
        if image.gray {
            let luma = image
                .data
                .chunks_exact(4)
                .map(|px| px[0])
                .collect::<Vec<u8>>();
            encoder
                .write_image(&luma, image.width, image.height, ExtendedColorType::L8)
                .map_err(|e| FastpkgError::Encode(e.to_string()))
        } else if image.has_alpha {
            encoder
                .write_image(
                    &image.data,
                    image.width,
                    image.height,
                    ExtendedColorType::Rgba8,
                )
                .map_err(|e| FastpkgError::Encode(e.to_string()))
        } else {
            let rgb = image.to_rgb();
            encoder
                .write_image(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
                .map_err(|e| FastpkgError::Encode(e.to_string()))
        }
    } else {
        let mut webp = if image.has_alpha {
            oxideav_webp::WebpImage::from_rgba8(image.width, image.height, image.data.clone())
        } else {
            oxideav_webp::WebpImage::from_rgb8(image.width, image.height, image.to_rgb())
        }
        .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        let mut metadata = oxideav_webp::Metadata::default();
        if options.keep_exif || options.keep_metadata {
            metadata.exif = image.exif.clone();
        }
        if options.keep_icc || options.keep_metadata {
            metadata.icc = image.icc.clone();
        }
        if options.keep_xmp || options.keep_metadata {
            metadata.xmp = image.xmp.clone();
        }
        if let Some(raw) = options.with_exif_raw.as_ref() {
            metadata.exif = base64::engine::general_purpose::STANDARD.decode(raw).ok();
        }
        if let Some(raw) = options.with_icc_raw.as_ref() {
            metadata.icc = base64::engine::general_purpose::STANDARD.decode(raw).ok();
        }
        if let Some(xmp) = options.with_xmp.as_ref() {
            metadata.xmp = Some(xmp.as_bytes().to_vec());
        }
        webp.metadata = metadata;
        let encode_options =
            oxideav_webp::EncodeOptions::default().with_quality(options.quality as f32);
        *out = oxideav_webp::encode(&webp, &encode_options)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        Ok(())
    }
}

fn encode_avif(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let speed = (11u8.saturating_sub(options.effort.clamp(1, 10))).clamp(1, 10);
    let mut encoder = AvifEncoder::new_with_speed_quality(&mut *out, speed, options.quality);
    encoder = encoder.with_num_threads(None);
    if image.has_alpha {
        encoder
            .write_image(
                &image.data,
                image.width,
                image.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    } else {
        let rgb = image.to_rgb();
        encoder
            .write_image(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    }
}

fn encode_gif(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let mut encoder = GifEncoder::new_with_speed(
        &mut *out,
        (10u8.saturating_sub(options.effort).clamp(1, 30)) as i32,
    );
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| FastpkgError::Encode(e.to_string()))?;
    let rgba = image::RgbaImage::from_raw(image.width, image.height, image.data.clone())
        .ok_or_else(|| FastpkgError::Encode("invalid RGBA buffer".into()))?;
    let delay = options
        .delay
        .as_ref()
        .and_then(|d| d.first())
        .copied()
        .unwrap_or(100);
    let frame = Frame::from_parts(rgba, 0, 0, Delay::from_numer_denom_ms(delay, 1));
    encoder
        .encode_frame(frame)
        .map_err(|e| FastpkgError::Encode(e.to_string()))
}

fn encode_qoi(image: &Image, _options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let encoder = QoiEncoder::new(&mut *out);
    if image.has_alpha {
        encoder
            .write_image(
                &image.data,
                image.width,
                image.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    } else {
        let rgb = image.to_rgb();
        encoder
            .write_image(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
            .map_err(|e| FastpkgError::Encode(e.to_string()))
    }
}

#[cfg(feature = "jxl-encode")]
fn encode_jxl(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let dimensions = Dimensions {
        width: image.width,
        height: image.height,
    };
    let distance = ((100u32.saturating_sub(options.quality as u32)) as f32 / 20.0).clamp(0.1, 25.0);
    let effort = Effort::from_level(options.effort.clamp(1, 10)).unwrap_or(Effort::Squirrel);
    let mut encoder = if options.lossless {
        JxlEncoder::lossless()
    } else {
        JxlEncoder::lossy(Distance::new(distance).map_err(|e| FastpkgError::Encode(e.to_string()))?)
    }
    .with_effort(effort);
    let exif = if options.keep_exif || options.keep_metadata {
        image.exif.clone()
    } else {
        None
    };
    let exif = options
        .with_exif_raw
        .as_ref()
        .and_then(|raw| base64::engine::general_purpose::STANDARD.decode(raw).ok())
        .or(exif);
    let xmp = if options.keep_xmp || options.keep_metadata {
        image.xmp.clone()
    } else {
        None
    }
    .or_else(|| options.with_xmp.as_ref().map(|xmp| xmp.as_bytes().to_vec()));
    let icc = if options.keep_icc || options.keep_metadata {
        image.icc.clone()
    } else {
        None
    }
    .or_else(|| {
        options
            .with_icc_raw
            .as_ref()
            .and_then(|raw| base64::engine::general_purpose::STANDARD.decode(raw).ok())
    });
    if let Some(exif) = exif.as_ref() {
        encoder = encoder.with_exif(exif);
    }
    if let Some(xmp) = xmp.as_ref().and_then(|xmp| std::str::from_utf8(xmp).ok()) {
        encoder = encoder.with_xmp(xmp);
    }
    if let Some(icc) = icc {
        encoder = encoder.with_color(ColorSpec::Icc(icc));
    }
    encoder = encoder.with_container(if exif.is_some() || xmp.is_some() {
        Container::IsoBmff
    } else {
        Container::Codestream
    });
    let encoded = if image.gray {
        let luma = image
            .data
            .chunks_exact(4)
            .map(|px| px[0])
            .collect::<Vec<u8>>();
        let source = ImageRef::<Gray8>::new(&luma, dimensions)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        encoder
            .encode_to_vec(source)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?
    } else if image.has_alpha {
        let source = ImageRef::<Rgba8>::new(&image.data, dimensions)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        encoder
            .encode_to_vec(source)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?
    } else {
        let rgb = image.to_rgb();
        let source = ImageRef::<Rgb8>::new(&rgb, dimensions)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        encoder
            .encode_to_vec(source)
            .map_err(|e| FastpkgError::Encode(e.to_string()))?
    };
    *out = encoded;
    Ok(())
}

#[cfg(not(feature = "jxl-encode"))]
fn encode_jxl(_image: &Image, _options: &OutputOptions, _out: &mut Vec<u8>) -> Result<()> {
    Err(FastpkgError::UnsupportedOutput(
        "JXL encoding requires the jxl-encode feature (CMake + ClangCL + libjxl)".into(),
    ))
}

fn insert_png_chunk(data: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return;
    }
    let mut offset = 8usize;
    let mut insert_at = None;
    while offset + 12 <= data.len() {
        let len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        let tag = &data[offset + 4..offset + 8];
        if tag == b"IDAT" || tag == b"IEND" {
            insert_at = Some(offset);
            break;
        }
        offset = offset.saturating_add(12 + len);
    }
    let Some(at) = insert_at else { return };
    let mut chunk = Vec::with_capacity(payload.len() + 12);
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(payload);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(payload);
    chunk.extend_from_slice(&hasher.finalize().to_be_bytes());
    data.splice(at..at, chunk);
}

fn write_png_metadata(data: &mut Vec<u8>, image: &Image, options: &OutputOptions) {
    if options.keep_exif || options.keep_metadata {
        if let Some(exif) = image.exif.as_ref() {
            insert_png_chunk(data, b"eXIf", exif);
        }
    }
    if let Some(raw) = options.with_exif_raw.as_ref() {
        if let Ok(exif) = base64::engine::general_purpose::STANDARD.decode(raw) {
            insert_png_chunk(data, b"eXIf", &exif);
        }
    }
    if options.keep_icc || options.keep_metadata {
        if let Some(icc) = image.icc.as_ref() {
            write_png_iccp(data, icc);
        }
    }
    if let Some(raw) = options.with_icc_raw.as_ref() {
        if let Ok(icc) = base64::engine::general_purpose::STANDARD.decode(raw) {
            write_png_iccp(data, &icc);
        }
    }
    if options.keep_xmp || options.keep_metadata {
        if let Some(xmp) = image.xmp.as_ref() {
            write_png_xmp(data, xmp);
        }
    }
    if let Some(xmp) = options.with_xmp.as_ref() {
        write_png_xmp(data, xmp.as_bytes());
    }
}

fn write_png_iccp(data: &mut Vec<u8>, profile: &[u8]) {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    if encoder.write_all(profile).is_err() {
        return;
    }
    let Ok(compressed) = encoder.finish() else {
        return;
    };
    let mut payload = b"ICC\x00\x00".to_vec();
    payload.extend_from_slice(&compressed);
    insert_png_chunk(data, b"iCCP", &payload);
}

fn write_png_xmp(data: &mut Vec<u8>, xmp: &[u8]) {
    let mut payload = b"XML:com.adobe.xmp\x00\x00\x00\x00\x00".to_vec();
    payload.extend_from_slice(xmp);
    insert_png_chunk(data, b"iTXt", &payload);
}

fn write_png_density(data: &mut Vec<u8>, dpi: u32) {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return;
    }
    let ppm = ((dpi as u64 * 10000) / 254).min(u32::MAX as u64) as u32;
    let mut payload = Vec::with_capacity(9);
    payload.extend_from_slice(&ppm.to_be_bytes());
    payload.extend_from_slice(&ppm.to_be_bytes());
    payload.push(1);
    insert_png_chunk(data, b"pHYs", &payload);
}

fn to_image_format(format: Format) -> Option<image::ImageFormat> {
    match format {
        Format::Png => Some(image::ImageFormat::Png),
        Format::Jpeg => Some(image::ImageFormat::Jpeg),
        Format::Webp => Some(image::ImageFormat::WebP),
        Format::Avif => Some(image::ImageFormat::Avif),
        Format::Gif => Some(image::ImageFormat::Gif),
        Format::Qoi => Some(image::ImageFormat::Qoi),
        _ => None,
    }
}

fn jpeg_sof(bytes: &[u8]) -> Option<(u32, u32, u8, bool)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            offset += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            break;
        }
        if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let data = &bytes[offset + 4..offset + 2 + length];
            if data.len() >= 6 {
                let h = u16::from_be_bytes([data[1], data[2]]) as u32;
                let w = u16::from_be_bytes([data[3], data[4]]) as u32;
                let channels = match data[5] {
                    1 => 1,
                    4 => 4,
                    _ => 3,
                };
                return Some((w, h, channels, matches!(marker, 0xc2 | 0xc6 | 0xca | 0xce)));
            }
        }
        offset += 2 + length;
    }
    None
}

fn webp_alpha(bytes: &[u8]) -> bool {
    bytes.len() >= 21 && &bytes[12..16] == b"VP8X" && (bytes[20] & 0x10) != 0
}

fn read_orientation(bytes: &[u8]) -> Option<u32> {
    let mut cursor = Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
}

fn jpeg_flags(bytes: &[u8]) -> (bool, bool, bool) {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return (false, false, false);
    }
    let mut exif = false;
    let mut icc = false;
    let mut xmp = false;
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            offset += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            break;
        }
        let data = &bytes[offset + 4..offset + 2 + length];
        if marker == 0xe1 {
            exif |= data.starts_with(b"Exif\x00\x00");
            xmp |= data.starts_with(b"http://ns.adobe.com/xap/1.0/\x00");
        } else if marker == 0xe2 && data.starts_with(b"ICC_PROFILE\x00") {
            icc = true;
        }
        offset += 2 + length;
    }
    (exif, icc, xmp)
}

fn png_flags(bytes: &[u8]) -> (bool, bool, bool) {
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return (false, false, false);
    }
    let (mut exif, mut icc, mut xmp) = (false, false, false);
    let mut offset = 8;
    while offset + 12 <= bytes.len() {
        let len = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let start = offset + 8;
        let end = start.saturating_add(len).min(bytes.len());
        match kind {
            b"eXIf" => exif = true,
            b"iCCP" => icc = true,
            b"iTXt" | b"tEXt" | b"zTXt" => {
                xmp |= bytes[start..end].windows(9).any(|w| w == b"<x:xmpmeta");
            }
            b"IEND" => break,
            _ => {}
        }
        offset = end.saturating_add(4);
    }
    (exif, icc, xmp)
}

fn webp_flags(bytes: &[u8]) -> (bool, bool, bool) {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return (false, false, false);
    }
    let (mut exif, mut icc, mut xmp) = (false, false, false);
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let kind = &bytes[offset..offset + 4];
        let len = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        match kind {
            b"EXIF" => exif = true,
            b"ICCP" => icc = true,
            b"XMP " => xmp = true,
            _ => {}
        }
        offset = offset.saturating_add(8 + len + (len & 1));
    }
    (exif, icc, xmp)
}
