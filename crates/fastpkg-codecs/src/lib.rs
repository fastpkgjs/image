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

pub mod header;
pub mod png_enc;
pub mod png_huff;
pub mod png_quant;
pub mod svg;
mod webp;

/// Version of the bundled libwebp encoder, e.g. `1.5.0`.
pub fn libwebp_version() -> String {
    webp::version()
}

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
        Format::Svg => svg::probe(bytes, input.density)?,
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
        // libvips reports the default orientation for WebP files that carry an EXIF block.
        if header.orientation.is_none() && format == Format::Webp {
            header.orientation = Some(1);
        }
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
    // libjpeg skips junk between marker segments; the size reader gives up on it.
    let (width, height) = match imagesize::blob_size(bytes) {
        Ok(size) => (size.width as u32, size.height as u32),
        Err(error) => match jpeg_sof(bytes) {
            Some((w, h, ..)) if format == Format::Jpeg => (w, h),
            _ => return Err(FastpkgError::Decode(error.to_string())),
        },
    };
    let mut header = HeaderInfo {
        format,
        width,
        height,
        channels: 3,
        has_alpha: format.supports_alpha(),
        depth: "uchar".into(),
        space: "srgb".into(),
        pages: 1,
        page_height: height,
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
                let details = header::png_details(bytes);
                header.is_progressive = details.interlaced;
                header.density = details.density;
                if bytes[24] == 16 {
                    header.space = if matches!(color_type, 0 | 4) {
                        "grey16".into()
                    } else {
                        "rgb16".into()
                    };
                }
                // A tRNS chunk on an RGB/grey/palette image adds an alpha channel when decoded.
                if details.transparency && matches!(color_type, 0 | 2 | 3) {
                    header.has_alpha = true;
                    header.channels = if color_type == 0 { 2 } else { 4 };
                }
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
            let details = header::jpeg_details(bytes);
            header.chroma_subsampling = details.chroma_subsampling;
            // libvips assumes 72 dpi for JPEGs with neither a JFIF density nor any EXIF block.
            header.density = details
                .density
                .or_else(|| exif_density(bytes))
                .or_else(|| (!jpeg_flags(bytes).0).then_some(72));
        }
        Format::Webp => {
            let details = header::webp_details(bytes);
            header.has_alpha = webp_alpha(bytes) || details.alpha;
            if header.has_alpha {
                header.channels = 4;
            }
            if details.animated || details.frames > 1 {
                header.pages = details.frames.max(1);
                header.animated = details.frames > 1;
                header.loop_count = details.loop_count;
                if !details.delays.is_empty() {
                    header.delay = Some(details.delays);
                }
            }
        }
        Format::Gif => {
            let details = header::gif_details(bytes);
            if let Some((width, height)) = header::gif_canvas(bytes) {
                header.width = width;
                header.height = height;
                header.page_height = height;
            }
            header.has_alpha = details.transparent;
            header.channels = if details.transparent { 4 } else { 3 };
            header.is_palette = true;
            header.pages = details.frames.max(1);
            header.animated = details.frames > 1;
            // NETSCAPE counts the repetitions after the first play (0 = forever); libvips reports
            // total plays, and 1 when the extension is absent.
            header.loop_count = Some(match details.loop_count {
                Some(0) => 0,
                Some(n) => n + 1,
                None => 1,
            });
            header.delay = Some(if details.delays.is_empty() {
                vec![0]
            } else {
                details.delays
            });
        }
        Format::Qoi if bytes.len() >= 14 => {
            header.width = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            header.height = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            header.channels = bytes[12];
            header.has_alpha = bytes[12] == 4;
            header.page_height = header.height;
        }
        Format::Avif => {
            header.has_alpha = header::avif_has_alpha(bytes);
            header.channels = if header.has_alpha { 4 } else { 3 };
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
    let mut image = decode_with_hint_inner(bytes, format, input, hint, fast_shrink)?;
    // sharp expands every grey input to RGB(A); only extractChannel and
    // toColourspace('b-w') produce single-channel output.
    image.gray = false;
    Ok(image)
}

fn decode_with_hint_inner(
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
        if let Some(noise) = &create.noise {
            return create_noise_image(create.width, create.height, create.channels, noise);
        }
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
        Format::Svg => svg::decode(bytes, input.density)?,
        Format::Png => decode_png(bytes, &input.fail_on)?,
        Format::Jpeg => decode_jpeg_scaled(bytes, hint, fast_shrink, &input.fail_on)?,
        Format::Webp => {
            // libvips opens WebP through the demuxer, which rejects a damaged container
            // whatever `failOn` says.
            if !webp::container_valid(bytes) {
                return Err(FastpkgError::Decode(
                    "webp: unable to parse image (damaged RIFF container)".into(),
                ));
            }
            decode_webp(bytes, input, hint, fast_shrink)?
        }
        Format::Gif => decode_gif(bytes, &input.fail_on)?,
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

/// WebP through libwebp (exact fancy upsampling as in sharp, SIMD, optional scaled decode);
/// animated or damaged files fall back to the pure-Rust `image-webp` decoder, which also
/// decides the error message.
fn decode_webp(
    bytes: &[u8],
    input: &InputOptions,
    hint: Option<(u32, u32)>,
    fast_shrink: bool,
) -> Result<Image> {
    let max_pixels = if input.unlimited {
        u64::MAX
    } else {
        input.limit_input_pixels.min(Limits::default().max_pixels)
    };
    // Keep 2x headroom for the resampler unless `fastShrinkOnLoad` allows decoding straight to
    // the target size.
    let scale_to = hint.map(|(w, h)| {
        let headroom = if fast_shrink { 1 } else { 2 };
        (w.saturating_mul(headroom), h.saturating_mul(headroom))
    });
    if let Some(decoded) = webp::decode(bytes, scale_to, max_pixels)? {
        let mut image = Image::from_rgba(decoded.width, decoded.height, decoded.rgba)?;
        image.has_alpha = decoded.has_alpha || image.has_transparency();
        image.format = Format::Webp;
        if (decoded.width, decoded.height) != decoded.source {
            image.pre_shrink = Some(decoded.source);
        }
        return Ok(image);
    }
    let dynamic = image::load_from_memory_with_format(bytes, image::ImageFormat::WebP)
        .map_err(|e| FastpkgError::Decode(format!("{}: {e}", Format::Webp.mime())))?;
    dynamic_to_image(dynamic, Format::Webp)
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
        Image::from_raw_slice(raw.width, raw.height, raw.channels, &bytes[..expected])?
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
    let bytes = header::avif_trim_trailing(bytes);
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

/// GIF decoding as libvips judges it. A file cut inside a block is an error unless `failOn` is
/// `none`, which returns what survived; a frame whose LZW data is short decodes to a blank
/// canvas at any level, while an invalid code is fatal at any level.
fn decode_gif(bytes: &[u8], fail_on: &str) -> Result<Image> {
    let lenient = fail_on == "none";
    if !lenient && header::gif_truncated(bytes) {
        return Err(FastpkgError::Decode(
            "gifload: unexpected end of GIF source data".into(),
        ));
    }
    let bytes = header::gif_with_canvas(bytes);
    let decode = |data: &[u8]| -> Result<Image> {
        let dynamic = image::load_from_memory_with_format(data, image::ImageFormat::Gif)
            .map_err(|e| FastpkgError::Decode(format!("image/gif: {e}")))?;
        dynamic_to_image(dynamic, Format::Gif)
    };
    let invalid = |error: &FastpkgError| error.to_string().contains("invalid code");
    // libvips stops reading a frame once its last pixel is in, so a bad code after that point
    // does not matter; the frame is re-encoded from the pixels that were good.
    let recovered = |result: Result<Image>, data: &[u8]| match &result {
        Err(error) if invalid(error) => gif_reencode_first_frame(data)
            .map(|fixed| decode(&fixed))
            .unwrap_or(result),
        _ => result,
    };
    let mut result = recovered(decode(&bytes), &bytes);
    if let Err(error) = &result {
        if !lenient && (invalid(error) || !error.to_string().contains("truncated")) {
            return result;
        }
        if lenient && bytes.len() >= 13 {
            let repaired = header::gif_repair(&bytes);
            result = recovered(decode(&repaired), &repaired);
            if result.as_ref().is_err_and(invalid) {
                return result;
            }
        }
    }
    if result.is_ok() {
        return result;
    }
    let (width, height) = (
        u16::from_le_bytes([bytes[6], bytes[7]]) as u32,
        u16::from_le_bytes([bytes[8], bytes[9]]) as u32,
    );
    if bytes.len() >= 13 && width > 0 && height > 0 && header::gif_has_frame_data(&bytes) {
        validate_dimensions(width, height, &Limits::default())?;
        let mut image = Image::from_rgba(
            width,
            height,
            vec![0u8; width as usize * height as usize * 4],
        )?;
        image.has_alpha = true;
        image.format = Format::Gif;
        return Ok(image);
    }
    Err(FastpkgError::Decode("image/gif: unreadable data".into()))
}

/// The file reduced to its first frame with the LZW data re-encoded from the pixels that
/// decoded cleanly, provided those cover the whole frame.
fn gif_reencode_first_frame(bytes: &[u8]) -> Option<Vec<u8>> {
    let (start, width, height) = header::gif_first_frame(bytes)?;
    let code_size = *bytes.get(start)?;
    if !(2..=8).contains(&code_size) {
        return None;
    }
    let mut data = Vec::new();
    let mut at = start + 1;
    while let Some(&size) = bytes.get(at) {
        if size == 0 {
            break;
        }
        data.extend_from_slice(bytes.get(at + 1..(at + 1 + size as usize).min(bytes.len()))?);
        at += 1 + size as usize;
    }
    let need = width as usize * height as usize;
    if need == 0 || need > Limits::default().max_pixels as usize {
        return None;
    }
    let mut decoder = weezl::decode::Decoder::new(weezl::BitOrder::Lsb, code_size);
    let mut pixels = vec![0u8; need];
    let (mut input, mut filled) = (&data[..], 0usize);
    while filled < need {
        let step = decoder.decode_bytes(input, &mut pixels[filled..]);
        input = &input[step.consumed_in..];
        filled += step.consumed_out;
        if step.status.is_err() || (step.consumed_in == 0 && step.consumed_out == 0) {
            break;
        }
    }
    if filled < need {
        return None;
    }
    let packed = weezl::encode::Encoder::new(weezl::BitOrder::Lsb, code_size)
        .encode(&pixels)
        .ok()?;
    let mut out = bytes[..start + 1].to_vec();
    for block in packed.chunks(255) {
        out.push(block.len() as u8);
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&[0, 0x3b]);
    Some(out)
}

// Warnings libjpeg raised while decoding on this thread: (truncation, anything else), and the
// message of the fatal error that aborted the decode, if any.
thread_local! {
    static JPEG_WARNINGS: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
    static JPEG_FATAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn jpeg_message(cinfo: &mut mozjpeg_sys::jpeg_common_struct) -> Option<String> {
    let format = unsafe { cinfo.err.as_ref() }.and_then(|err| err.format_message)?;
    let mut buffer = [0u8; 80];
    // libjpeg writes the message into the buffer; the sys crate declares it as `&[u8; 80]`.
    let write: unsafe extern "C-unwind" fn(&mut mozjpeg_sys::jpeg_common_struct, &mut [u8; 80]) =
        unsafe { std::mem::transmute(format) };
    unsafe { write(cinfo, &mut buffer) };
    Some(String::from_utf8_lossy(buffer.split(|&b| b == 0).next().unwrap_or_default()).into_owned())
}

unsafe extern "C-unwind" fn jpeg_collect_warning(
    cinfo: &mut mozjpeg_sys::jpeg_common_struct,
    level: std::os::raw::c_int,
) {
    if level >= 0 {
        return; // trace messages
    }
    let text = jpeg_message(cinfo).unwrap_or_default();
    let truncated = text.contains("Premature end");
    // libvips fails a progressive file whose scan script is inconsistent, whatever `failOn` says.
    if text.contains("progression") {
        JPEG_FATAL.with(|fatal| *fatal.borrow_mut() = Some(text));
        return;
    }
    JPEG_WARNINGS.with(|counts| {
        let (a, b) = counts.get();
        counts.set(if truncated { (a + 1, b) } else { (a, b + 1) });
    });
}

unsafe extern "C-unwind" fn jpeg_unwind(cinfo: &mut mozjpeg_sys::jpeg_common_struct) {
    let text = jpeg_message(cinfo).unwrap_or_default();
    JPEG_FATAL.with(|fatal| *fatal.borrow_mut() = Some(text));
    std::panic::resume_unwind(Box::new("libjpeg fatal error"));
}

fn jpeg_error_manager() -> mozjpeg_sys::jpeg_error_mgr {
    unsafe {
        let mut err: mozjpeg_sys::jpeg_error_mgr = std::mem::zeroed();
        mozjpeg_sys::jpeg_std_error(&mut err);
        err.error_exit = Some(jpeg_unwind);
        err.emit_message = Some(jpeg_collect_warning);
        err
    }
}

fn decode_jpeg_scaled(
    bytes: &[u8],
    hint: Option<(u32, u32)>,
    fast_shrink: bool,
    fail_on: &str,
) -> Result<Image> {
    match decode_jpeg_mozjpeg(bytes, hint, fast_shrink, fail_on) {
        Ok(Some(image)) => return Ok(image),
        Ok(None) => {}
        Err(error) => return Err(error),
    }
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|e| FastpkgError::Decode(e.to_string()))?;
    dynamic_to_image(image, Format::Jpeg)
}

/// Smallest DCT scale numerator `n` (output is `n/8` of the source) whose
/// result still covers `target` on both axes. `headroom` keeps extra source
/// pixels for the resampler when moire-free output matters more than speed.
pub fn jpeg_scale_numerator(source: (u32, u32), target: (u32, u32), headroom: u32) -> u8 {
    let need = |s: u32, t: u32| -> u32 {
        let t = (t as u64 * headroom as u64).clamp(1, s as u64);
        // ceil(s * n / 8) >= t  <=>  s * n > 8 * (t - 1)  <=>  n >= 8 * (t - 1) / s + 1
        ((8 * (t - 1) / s as u64) + 1) as u32
    };
    need(source.0, target.0)
        .max(need(source.1, target.1))
        .clamp(1, 8) as u8
}

/// Decodes with libjpeg(-turbo). `Ok(None)` means libjpeg cannot handle the file (CMYK, odd
/// precisions, fatal errors) and the caller should fall back to another decoder; `Err` is a
/// `failOn` violation: truncated data always, other warnings only for `failOn: 'warning'`.
fn decode_jpeg_mozjpeg(
    bytes: &[u8],
    hint: Option<(u32, u32)>,
    fast_shrink: bool,
    fail_on: &str,
) -> Result<Option<Image>> {
    JPEG_WARNINGS.with(|counts| counts.set((0, 0)));
    JPEG_FATAL.with(|fatal| fatal.borrow_mut().take());
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut decompress = mozjpeg::decompress::DecompressBuilder::new()
            .with_err(jpeg_error_manager())
            .from_mem(bytes)
            .ok()?;
        let source_width = decompress.width() as u32;
        let source_height = decompress.height() as u32;
        if source_width == 0 || source_height == 0 {
            return None;
        }
        if let Some(hint) = hint {
            let scale = jpeg_scale_numerator(
                (source_width, source_height),
                hint,
                if fast_shrink { 1 } else { 2 },
            );
            decompress.scale(scale);
        }
        let mut started = decompress.rgba().ok()?;
        let width = started.width() as u32;
        let height = started.height() as u32;
        let pixels: Vec<[u8; 4]> = started.read_scanlines().ok()?;
        let data: Vec<u8> = bytemuck::cast_vec(pixels);
        let mut image = Image::from_rgba(width, height, data).ok()?;
        image.has_alpha = false;
        image.format = Format::Jpeg;
        if (width, height) != (source_width, source_height) {
            image.pre_shrink = Some((source_width, source_height));
        }
        Some(image)
    }))
    .ok()
    .flatten();
    let Some(image) = decoded else {
        // A fatal libjpeg error is a damaged file, whatever `failOn` says; an unsupported
        // colour conversion (CMYK and friends) is left to the fallback decoder.
        return match JPEG_FATAL.with(|fatal| fatal.borrow_mut().take()) {
            Some(text) if !text.contains("onversion") => {
                Err(FastpkgError::Decode(format!("VipsJpeg: {text}")))
            }
            _ => Ok(None),
        };
    };
    if let Some(text) = JPEG_FATAL.with(|fatal| fatal.borrow_mut().take()) {
        return Err(FastpkgError::Decode(format!("VipsJpeg: {text}")));
    }
    let (truncated, other) = JPEG_WARNINGS.with(|counts| counts.get());
    if fail_on != "none" && (truncated > 0 || (other > 0 && fail_on == "warning")) {
        return Err(FastpkgError::Decode(if truncated > 0 {
            "VipsJpeg: Premature end of JPEG file".into()
        } else {
            "VipsJpeg: Corrupt JPEG data".into()
        }));
    }
    Ok(Some(image))
}

/// Decoder configured like libpng under libvips: chunk CRCs are only enforced from
/// `failOn: 'error'` up, zlib checksums from `'truncated'` up.
fn png_decoder<'a>(bytes: &'a [u8], fail_on: &str) -> png::Decoder<Cursor<&'a [u8]>> {
    let mut options = png::DecodeOptions::default();
    options.set_ignore_crc(matches!(fail_on, "none" | "truncated"));
    options.set_ignore_adler32(fail_on == "none");
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder
}

/// libpng refuses a chunk whose type is not four ASCII letters or whose length exceeds 2^31 - 1
/// while it reads the header chunks and the IDAT run, whatever `failOn` says.
fn png_chunk_headers_valid(bytes: &[u8]) -> bool {
    let mut pos = 8usize;
    let mut seen_idat = false;
    while pos + 12 <= bytes.len() {
        let kind = &bytes[pos + 4..pos + 8];
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        if !kind.iter().all(u8::is_ascii_alphabetic) || len > 0x7fff_ffff {
            return false;
        }
        if kind == b"IDAT" {
            seen_idat = true;
        } else if seen_idat {
            break;
        }
        match pos
            .checked_add(12)
            .and_then(|p| p.checked_add(len as usize))
        {
            Some(next) => pos = next,
            None => break,
        }
    }
    true
}

/// End offset of the last complete IDAT chunk when the data before it parses as a chunk list.
fn png_idat_end(bytes: &[u8]) -> Option<usize> {
    let mut pos = 8;
    let mut end = None;
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let next = pos.checked_add(12)?.checked_add(len)?;
        if next > bytes.len() {
            break;
        }
        match &bytes[pos + 4..pos + 8] {
            b"IDAT" => end = Some(next),
            b"IEND" => break,
            _ if end.is_some() => break,
            _ => {}
        }
        pos = next;
    }
    end
}

/// Size of the filtered raster (filter bytes included) a PNG header describes.
fn png_raster_size(info: &png::Info) -> u64 {
    let bits = u64::from(info.bit_depth as u8) * info.color_type.samples() as u64;
    let pass = |width: u64, height: u64| match (width, height) {
        (0, _) | (_, 0) => 0,
        _ => height * (1 + (width * bits).div_ceil(8)),
    };
    let (width, height) = (u64::from(info.width), u64::from(info.height));
    if !info.interlaced {
        return pass(width, height);
    }
    const PASSES: [(u64, u64, u64, u64); 7] = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];
    PASSES
        .iter()
        .map(|&(x0, y0, dx, dy)| {
            pass(
                width.saturating_sub(x0).div_ceil(dx),
                height.saturating_sub(y0).div_ceil(dy),
            )
        })
        .sum()
}

/// Whether the zlib stream spread over the IDAT run ends properly (final block, checksum): the
/// decoder stops after the last row and never notices a stream that was cut short. A stream
/// that breaks once the raster is complete is let through: libpng has stopped inflating by then
/// and treats the error as benign (a bad checksum right at the end is not).
fn png_zlib_complete(bytes: &[u8], raster: u64) -> bool {
    let mut inflate = flate2::Decompress::new(true);
    let mut sink = vec![0u8; 1 << 16];
    let mut pos = 8usize;
    let mut seen = false;
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let is_idat = &bytes[pos + 4..pos + 8] == b"IDAT";
        if seen && !is_idat {
            break;
        }
        if is_idat {
            seen = true;
            let end = pos.saturating_add(8).saturating_add(len).min(bytes.len());
            let mut input = &bytes[pos + 8..end];
            loop {
                let (before_in, before_out) = (inflate.total_in(), inflate.total_out());
                match inflate.decompress(input, &mut sink, flate2::FlushDecompress::None) {
                    Ok(flate2::Status::StreamEnd) => return true,
                    Ok(_) => {}
                    Err(error) => {
                        let total = inflate.total_out();
                        let checksum = error.message() == Some("incorrect data check");
                        return total > raster || (total == raster && !checksum);
                    }
                }
                input = &input[(inflate.total_in() - before_in) as usize..];
                if inflate.total_in() == before_in && inflate.total_out() == before_out {
                    break;
                }
            }
        }
        match pos.checked_add(12).and_then(|p| p.checked_add(len)) {
            Some(next) => pos = next,
            None => break,
        }
    }
    false
}

/// libpng stops once the last row is read, so a missing or damaged tail (IEND, trailing chunks)
/// is not an error: a failed decode is retried with the data cut after the IDAT run and a
/// synthetic IEND appended.
fn decode_png(bytes: &[u8], fail_on: &str) -> Result<Image> {
    match decode_png_once(bytes, fail_on) {
        Err(error) => {
            if let Some(end) = png_idat_end(bytes) {
                let mut cut = bytes[..end].to_vec();
                cut.extend_from_slice(&[
                    0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
                ]);
                if let Ok(image) = decode_png_once(&cut, fail_on) {
                    return Ok(image);
                }
            }
            Err(error)
        }
        ok => ok,
    }
}

/// With `failOn: 'none'` a PNG whose data stream breaks yields the rows decoded so far; the
/// rest stays zero, as in libvips. Interlaced files are all-or-nothing.
fn decode_png_once(bytes: &[u8], fail_on: &str) -> Result<Image> {
    let lenient = fail_on == "none";
    if !png_chunk_headers_valid(bytes) {
        return Err(FastpkgError::Decode("PNG: invalid chunk header".into()));
    }
    let decoder = png_decoder(bytes, fail_on);
    let mut reader = decoder
        .read_info()
        .map_err(|e| FastpkgError::Decode(e.to_string()))?;
    // libpng refuses a palette image without PLTE up front, even when it is told to be lenient.
    if reader.info().color_type == png::ColorType::Indexed && reader.info().palette.is_none() {
        return Err(FastpkgError::Decode("PNG: missing PLTE chunk".into()));
    }
    let buffer_size = reader
        .output_buffer_size()
        .ok_or_else(|| FastpkgError::Decode("PNG output buffer is too large".into()))?;
    let mut buffer = vec![0; buffer_size];
    let (out_color, out_depth) = reader.output_color_type();
    let (out_width, out_height) = reader.info().size();
    let out_line = reader.output_line_size(out_width).unwrap_or(0);
    let interlaced = reader.info().interlaced;
    // Interlaced files are read whole by libvips, so there a short stream is fatal at any level.
    if (!lenient || interlaced) && !png_zlib_complete(bytes, png_raster_size(reader.info())) {
        return Err(FastpkgError::Decode("PNG: truncated zlib stream".into()));
    }
    let info = match reader.next_frame(&mut buffer) {
        Ok(info) => info,
        Err(error) if lenient && !interlaced => {
            // Re-read row by row (a failed `next_frame` leaves the reader unusable).
            buffer.fill(0);
            if let Ok(mut rows) = png_decoder(bytes, fail_on).read_info() {
                for y in 0..out_height as usize {
                    match rows.next_row() {
                        Ok(Some(row)) => {
                            let data = row.data();
                            let len = data.len().min(out_line);
                            buffer[y * out_line..y * out_line + len].copy_from_slice(&data[..len]);
                        }
                        _ => break,
                    }
                }
            }
            let _ = error;
            png::OutputInfo {
                width: out_width,
                height: out_height,
                color_type: out_color,
                bit_depth: out_depth,
                line_size: out_line,
            }
        }
        Err(error) => return Err(FastpkgError::Decode(error.to_string())),
    };
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
        // sharp/libvips promote 16-bit grey to RGB16, so wide grey decodes as 3 channels.
        image.gray = false;
        image.format = Format::Png;
        return Ok(image);
    }
    let row_bytes = info.line_size;
    let (width_px, height_px) = (width as usize, height as usize);
    let rgba = match color_type {
        // Already tightly packed RGBA: reuse the decode buffer instead of copying it.
        png::ColorType::Rgba if row_bytes == width_px * 4 => {
            buffer.truncate(pixels * 4);
            buffer
        }
        _ => {
            let mut rgba = vec![255u8; pixels * 4];
            if width_px > 0 {
                for (dst_row, src_row) in rgba
                    .chunks_exact_mut(width_px * 4)
                    .zip(raw.chunks_exact(row_bytes))
                    .take(height_px)
                {
                    match color_type {
                        png::ColorType::Grayscale => {
                            for (dst, &v) in dst_row.chunks_exact_mut(4).zip(src_row) {
                                dst[..3].fill(v);
                            }
                        }
                        png::ColorType::GrayscaleAlpha => {
                            for (dst, src) in
                                dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(2))
                            {
                                dst[..3].fill(src[0]);
                                dst[3] = src[1];
                            }
                        }
                        png::ColorType::Rgb => {
                            for (dst, src) in
                                dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(3))
                            {
                                dst[..3].copy_from_slice(src);
                            }
                        }
                        png::ColorType::Rgba => dst_row.copy_from_slice(&src_row[..width_px * 4]),
                        _ => return Err(FastpkgError::Decode("unsupported PNG layout".into())),
                    }
                }
            }
            rgba
        }
    };
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

/// Gaussian noise image (`create: { noise: { type: 'gaussian', mean, sigma } }`).
fn create_noise_image(
    width: u32,
    height: u32,
    channels: u8,
    noise: &fastpkg_core::options::NoiseInput,
) -> Result<Image> {
    validate_dimensions(width, height, &Limits::default())?;
    if !(1..=4).contains(&channels) {
        return Err(FastpkgError::InvalidOptions(
            "create.channels must be between 1 and 4".into(),
        ));
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut state = seed ^ 0x2545_F491_4F6C_DD1D | 1;
    let mut next = move || {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    let count = width as usize * height as usize * channels as usize;
    let mut data = Vec::with_capacity(count);
    while data.len() < count {
        let (u1, u2) = (next(), next());
        let radius = (-2.0 * u1.ln()).sqrt();
        let angle = std::f64::consts::TAU * u2;
        for z in [radius * angle.cos(), radius * angle.sin()] {
            if data.len() < count {
                data.push((noise.mean + noise.sigma * z).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    let mut image = Image::from_raw(width, height, channels, data)?;
    image.format = Format::Raw;
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
                let wide = image.to_packed16();
                out = vec![0u8; wide.len() * 2];
                let fill = |(dst, src): (&mut [u8], &[u16])| {
                    for (bytes, value) in dst.chunks_exact_mut(2).zip(src) {
                        bytes.copy_from_slice(&value.to_ne_bytes());
                    }
                };
                const RUN: usize = 1 << 17;
                if wide.len() >= 4 * RUN {
                    use rayon::prelude::*;
                    out.par_chunks_mut(RUN * 2)
                        .zip(wide.par_chunks(RUN))
                        .for_each(fill);
                } else {
                    fill((&mut out, &wide));
                }
            } else {
                out = image.to_packed();
            }
        }
        _ => return Err(FastpkgError::UnsupportedOutput(format!("{format:?}"))),
    }
    Ok((out, format))
}

mod jpeg_par;

fn encode_jpeg(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    if image.has_alpha && image.has_translucent_alpha8() {
        // JPEG has no alpha: like libvips, composite onto black rather than keeping the
        // colour that sits under transparent pixels.
        let flat = image.with_opaque_pixels(image.flattened_onto_black());
        return encode_jpeg_opaque(&flat, options, out);
    }
    encode_jpeg_opaque(image, options, out)
}

/// libjpeg encoder with the settings shared by the single-pass and the banded paths.
fn jpeg_compressor(
    grayscale: bool,
    width: usize,
    height: usize,
    quality: u8,
    chroma: Option<&str>,
) -> mozjpeg::Compress {
    let mut compress = mozjpeg::Compress::new(if grayscale {
        mozjpeg::ColorSpace::JCS_GRAYSCALE
    } else {
        mozjpeg::ColorSpace::JCS_EXT_RGBA
    });
    compress.set_size(width, height);
    compress.set_fastest_defaults();
    compress.set_quality(quality as f32);
    match chroma {
        Some("4:4:4" | "444") => compress.set_chroma_sampling_pixel_sizes((1, 1), (1, 1)),
        Some("4:2:2" | "422") => compress.set_chroma_sampling_pixel_sizes((2, 1), (2, 1)),
        Some("4:2:0" | "420") => compress.set_chroma_sampling_pixel_sizes((2, 2), (2, 2)),
        _ => {}
    }
    compress
}

fn encode_jpeg_opaque(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let quality = options.quality.clamp(1, 100);
    let grayscale = image.gray;
    if options.parallel && (options.progressive || options.mozjpeg || options.optimise_scans) {
        return Err(FastpkgError::InvalidOptions(
            "parallel cannot be combined with progressive/mozjpeg encoding".into(),
        ));
    }
    let luma = grayscale.then(|| image.luma_plane());
    let pixels = luma
        .as_deref()
        .map_or_else(|| image.data.as_slice(), |pixels| pixels);
    if image.width > u16::MAX as u32 || image.height > u16::MAX as u32 {
        return Err(FastpkgError::Encode(
            "JPEG dimensions exceed 65535 pixels".into(),
        ));
    }
    let chroma = options.chroma_subsampling.as_deref();
    // Opt-in banded encode; `None` (small image, one thread, any band failure) falls through
    // to the normal single-pass encoder below.
    let banded = if options.parallel {
        jpeg_par::encode(
            pixels,
            image.width as usize,
            image.height as usize,
            if grayscale { 1 } else { 4 },
            rayon::current_num_threads(),
            |rows| {
                let mut compress =
                    jpeg_compressor(grayscale, image.width as usize, rows, quality, chroma);
                compress.set_optimize_coding(false);
                compress
            },
        )
    } else {
        None
    };
    let encoded = banded.or_else(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut compress = jpeg_compressor(
                grayscale,
                image.width as usize,
                image.height as usize,
                quality,
                chroma,
            );
            compress.set_optimize_coding(options.optimise_coding);
            if options.progressive {
                // `set_optimize_scans(true)` is deliberately not used: with the fast profile it
                // also coarsens quantisation (mean error +1.3 levels), and it only saves bytes.
                compress.set_progressive_mode();
            }
            let mut compress = compress.start_compress(Vec::new()).ok()?;
            compress.write_scanlines(pixels).ok()?;
            compress.finish().ok()
        }))
        .ok()
        .flatten()
    });
    match encoded {
        Some(data) => *out = data,
        None => {
            let rgb;
            let (samples, color_type) = if grayscale {
                (pixels, jpeg_encoder::ColorType::Luma)
            } else {
                rgb = image.to_rgb();
                (rgb.as_slice(), jpeg_encoder::ColorType::Rgb)
            };
            let mut encoder = jpeg_encoder::Encoder::new(&mut *out, quality);
            encoder.set_progressive(options.progressive);
            encoder.set_optimized_huffman_tables(options.optimise_coding);
            encoder
                .encode(samples, image.width as u16, image.height as u16, color_type)
                .map_err(|e| FastpkgError::Encode(e.to_string()))?;
        }
    }
    if let Some(density) = effective_density(options) {
        write_jpeg_density(out, density);
    }
    let exif = effective_exif(image, options);
    write_jpeg_metadata(out, image, options, exif.as_deref());
    Ok(())
}

fn effective_density(options: &OutputOptions) -> Option<u32> {
    options.with_density.or_else(|| {
        options
            .with_metadata
            .as_ref()
            .and_then(|metadata| metadata.density)
    })
}

fn effective_orientation(options: &OutputOptions) -> Option<u32> {
    options
        .with_metadata
        .as_ref()
        .and_then(|metadata| metadata.orientation)
        .or_else(|| exif_orientation_value(options.with_exif.as_ref()))
        .or_else(|| exif_orientation_value(options.with_exif_merge.as_ref()))
}

/// The EXIF block to embed: the retained one (when kept) with the requested
/// `withMetadata({ orientation, density })` overrides applied in place, or a minimal
/// orientation-only block when an orientation is requested but nothing was retained.
fn effective_exif(image: &Image, options: &OutputOptions) -> Option<Vec<u8>> {
    let mut exif = image
        .exif
        .clone()
        .filter(|_| options.keep_exif || options.keep_metadata);
    let orientation = effective_orientation(options).and_then(|value| u16::try_from(value).ok());
    if let Some(exif) = exif.as_mut() {
        if let Some(orientation) = orientation {
            fastpkg_core::exif::set_orientation(exif, orientation);
        }
        if let Some(density) = effective_density(options) {
            fastpkg_core::exif::set_density(exif, density);
        }
    } else if let Some(orientation) = orientation {
        exif = Some(fastpkg_core::exif::orientation_only(orientation));
    }
    exif
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

fn write_jpeg_metadata(
    data: &mut Vec<u8>,
    image: &Image,
    options: &OutputOptions,
    exif: Option<&[u8]>,
) {
    if let Some(exif) = exif {
        let mut payload = b"Exif\x00\x00".to_vec();
        payload.extend_from_slice(exif);
        insert_jpeg_segment(data, 0xe1, &payload);
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
    use png_enc::{encode, Params};
    let level = options.compression_level.min(9);
    let adaptive = options.adaptive_filtering;
    let (width, height) = (image.width, image.height);
    if options.palette {
        encode_png_palette(image, options, out);
    } else if output_is_16(options) && !eight_bit_source(image, options) {
        // `bitdepth: 16` only matters for images that carry 16-bit samples (sharp keeps 8-bit
        // sources at 8 bits); toColourspace('rgb16') etc. still force it.
        let wide = image.to_rgba16();
        let mut samples = Vec::with_capacity(wide.len() * 2);
        let color_type = if image.gray {
            for px in wide.chunks_exact(4) {
                samples.extend_from_slice(&px[0].to_be_bytes());
                if image.has_alpha {
                    samples.extend_from_slice(&px[3].to_be_bytes());
                }
            }
            if image.has_alpha {
                4
            } else {
                0
            }
        } else {
            let n = if image.has_alpha { 4 } else { 3 };
            for px in wide.chunks_exact(4) {
                for value in &px[..n] {
                    samples.extend_from_slice(&value.to_be_bytes());
                }
            }
            if image.has_alpha {
                6
            } else {
                2
            }
        };
        encode(
            &Params {
                width,
                height,
                bit_depth: 16,
                color_type,
                palette: None,
                trns: None,
                interlace: options.progressive,
                level,
                adaptive,
            },
            &samples,
            out,
        );
    } else {
        let (samples, color_type): (std::borrow::Cow<[u8]>, u8) = if image.gray {
            (
                image.to_packed().into(),
                if image.has_alpha { 4 } else { 0 },
            )
        } else if image.has_alpha {
            ((&image.data[..]).into(), 6)
        } else {
            (image.to_rgb().into(), 2)
        };
        encode(
            &Params {
                width,
                height,
                bit_depth: 8,
                color_type,
                palette: None,
                trns: None,
                interlace: options.progressive,
                level,
                adaptive,
            },
            &samples,
            out,
        );
    }
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

fn eight_bit_source(image: &Image, options: &OutputOptions) -> bool {
    options.bitdepth == Some(16)
        && options.depth.is_none()
        && options.raw_depth.is_none()
        && options.colourspace.is_none()
        && !image.has_wide()
        && image.depth != "ushort"
}

/// Indexed PNG through the built-in quantiser. Mirrors sharp/libvips: `colours` selects a
/// 1/2/4/8-bit depth and therefore caps the palette at 2/4/16/256 entries.
fn encode_png_palette(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) {
    let cap = match options.colors {
        0..=2 => 2,
        3..=4 => 4,
        5..=16 => 16,
        _ => 256,
    };
    let quantized = png_quant::quantize(
        &image.data,
        image.width as usize,
        image.height as usize,
        &png_quant::Settings {
            max_colors: cap,
            quality: if options.quality_set {
                options.quality
            } else {
                100
            },
            effort: if options.effort_set {
                options.effort
            } else {
                7
            },
            dither: options.dither as f32,
        },
    );
    let depth = match quantized.colors {
        0..=2 => 1,
        3..=4 => 2,
        5..=16 => 4,
        _ => 8,
    };
    // Adam7 here is written at 8 bits per index (no sub-byte pass packing).
    let depth = if options.progressive { 8 } else { depth };
    let packed = png_enc::pack_indices(&quantized.indices, image.width as usize, depth);
    png_enc::encode(
        &png_enc::Params {
            width: image.width,
            height: image.height,
            bit_depth: depth,
            color_type: 3,
            palette: Some(&quantized.palette),
            trns: (!quantized.trns.is_empty()).then_some(&quantized.trns[..]),
            interlace: options.progressive,
            level: options.compression_level.min(9),
            adaptive: options.adaptive_filtering,
        },
        &packed,
        out,
    );
}

/// (EXIF, ICC, XMP) chunk payloads.
type WebpMetadata = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

/// EXIF / ICC / XMP bytes to embed in a WebP, honouring `keep*` and the `with*` overrides.
fn webp_metadata(image: &Image, options: &OutputOptions) -> WebpMetadata {
    let mut exif = effective_exif(image, options);
    let mut icc = image
        .icc
        .clone()
        .filter(|_| options.keep_icc || options.keep_metadata);
    let mut xmp = image
        .xmp
        .clone()
        .filter(|_| options.keep_xmp || options.keep_metadata);
    if let Some(raw) = options.with_exif_raw.as_ref() {
        exif = base64::engine::general_purpose::STANDARD.decode(raw).ok();
    }
    if let Some(raw) = options.with_icc_raw.as_ref() {
        icc = base64::engine::general_purpose::STANDARD.decode(raw).ok();
    }
    if let Some(text) = options.with_xmp.as_ref() {
        xmp = Some(text.as_bytes().to_vec());
    }
    (exif, icc, xmp)
}

/// WebP through libwebp: lossy and near-lossless (and lossless when asked).
fn encode_webp_libwebp(
    image: &Image,
    options: &OutputOptions,
    force_lossless: Option<(i32, f32)>,
    out: &mut Vec<u8>,
) -> Result<()> {
    let (exif, icc, xmp) = webp_metadata(image, options);
    // sharp/libvips: `nearLossless` switches to the lossless coder and passes the quality as
    // libwebp's `near_lossless` level.
    let near = options.near_lossless && !options.lossless && force_lossless.is_none();
    let (effort, quality) = force_lossless.unwrap_or((
        options.effort as i32,
        // In near-lossless mode `quality` is the preprocessing level (below), not the lossless
        // compression effort; libvips / sharp pin that effort at 75.
        if near { 75.0 } else { options.quality as f32 },
    ));
    *out = webp::encode(&webp::Params {
        rgba: &image.data,
        width: image.width,
        height: image.height,
        has_alpha: image.has_alpha,
        quality,
        effort,
        lossless: options.lossless || near || force_lossless.is_some(),
        near_lossless: if near { options.quality as i32 } else { 100 },
        alpha_quality: options.alpha_quality as i32,
        exact: options.exact,
        smart_subsample: options.smart_subsample,
        preset: options.preset.as_deref(),
        exif: exif.as_deref(),
        icc: icc.as_deref(),
        xmp: xmp.as_deref(),
    })?;
    Ok(())
}

fn encode_webp(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    if image.width > webp::MAX_DIMENSION || image.height > webp::MAX_DIMENSION {
        return Err(FastpkgError::Encode(format!(
            "webp: image is {}x{} but WebP supports at most {m}x{m}",
            image.width,
            image.height,
            m = webp::MAX_DIMENSION
        )));
    }
    // Lossy / near-lossless always use libwebp; so does lossless with an explicit `effort`
    // (sharp semantics: effort -> method, quality -> lossless size/speed trade-off).
    if !options.lossless || options.effort_set {
        return encode_webp_libwebp(image, options, None, out);
    }
    let pixels = image.width as usize * image.height as usize;
    // Palette-like content (flat art, screenshots, indexed or dithered images) compresses 3-20x
    // better with libwebp than with the pure-Rust coder, so the fast pass is skipped for it.
    if pixels > LIBWEBP_ALWAYS_PIXELS && few_colours(image) {
        // Method 2 is about 20% slower than method 1 and rarely more than 0.5% smaller, except
        // on small images, where it saves around 10% (and costs little in absolute terms).
        let method = match pixels {
            0..=0x7_FFFF => 2,
            0x8_0000..=0xFF_FFFF => 1,
            _ => 0,
        };
        if encode_webp_libwebp(image, options, Some((method, 100.0)), out).is_ok() {
            return Ok(());
        }
    }
    encode_webp_lossless_fast(image, out)?;
    let (exif, icc, xmp) = webp_metadata(image, options);
    let mut best: Option<Vec<u8>> = None;
    for effort in lossless_candidates(image, options, out.len()) {
        let mut better = Vec::new();
        let limit = best
            .as_ref()
            .map_or(out.len() + metadata_len(&exif, &icc, &xmp), |b| b.len());
        if encode_webp_libwebp(image, options, Some(effort), &mut better).is_ok()
            && better.len() < limit
        {
            best = Some(better);
        }
    }
    if let Some(better) = best {
        *out = better;
        return Ok(());
    }
    if exif.is_some() || icc.is_some() || xmp.is_some() {
        *out = webp::attach_metadata(out, exif.as_deref(), icc.as_deref(), xmp.as_deref())?;
    }
    Ok(())
}

fn metadata_len(exif: &Option<Vec<u8>>, icc: &Option<Vec<u8>>, xmp: &Option<Vec<u8>>) -> usize {
    [exif, icc, xmp]
        .iter()
        .map(|m| m.as_ref().map_or(0, |m| m.len() + 8))
        .sum()
}

/// Escalate to libwebp when the fast lossless file is below this share of the raw pixel bytes
/// (3/10). Photographs mostly stay above 40%; gradients and smooth photographs land below.
const LIBWEBP_ESCALATE_RATIO: (usize, usize) = (3, 10);
/// Deterministic pixel sample used by the colour-diversity test.
const COLOUR_SAMPLE: usize = 4096;
/// At most this many distinct colours among `COLOUR_SAMPLE` samples mark graphic content
/// (flat art, indexed and dithered images have well under 25%, photographs 50-100%).
const COLOUR_SAMPLE_GRAPHIC_MAX: usize = 1024;
/// Images up to this many pixels are always challenged: it costs a few milliseconds.
const LIBWEBP_ALWAYS_PIXELS: usize = 16 * 1024;
/// Up to this many pixels both libwebp candidates are simply run in full; above it a
/// deterministic sample of rows predicts which one (if any) is worth running.
const LIBWEBP_TRY_BOTH_PIXELS: usize = 160 * 1024;
/// No libwebp challenge to the fast coder above this many pixels for non-graphic content
/// (about 0.25 s per megapixel).
const LIBWEBP_MAX_PIXELS: usize = 8 << 20;
/// Required gain on the sample (15%) before the full image is escalated.
const LIBWEBP_MIN_GAIN: (usize, usize) = (85, 100);

/// Decides whether the pure-Rust lossless output should be challenged by libwebp, and with which
/// `(method, quality)` candidates (the smallest result wins). The pure-Rust coder has a single
/// predictor and no palette, so it loses by 25-60% on gradients and smooth content even after
/// the palette-like content has been routed away. Two libwebp settings matter here, both at
/// quality 100 (the cheap `method 0` is clearly best on noisy photographic gradients, `method 1`
/// on exact gradients, with `method 2` never more than 0.5% smaller than `method 1`): they are
/// either both run (small images) or one is picked by encoding a small deterministic sample of
/// rows with the fast coder and with each setting, and only escalating when the sample shrinks
/// by 15%. Photographs fail the ratio test and keep the fast coder's speed. Escalation costs
/// about 0.25 s per megapixel (method 0) to 0.5 s (method 1) on this kind of content.
fn lossless_candidates(image: &Image, options: &OutputOptions, fast_len: usize) -> Vec<(i32, f32)> {
    let channels = if image.gray {
        1
    } else if image.has_alpha {
        4
    } else {
        3
    };
    let pixels = image.width as usize * image.height as usize;
    if pixels <= LIBWEBP_ALWAYS_PIXELS {
        return vec![(1, 100.0)];
    }
    let (num, den) = LIBWEBP_ESCALATE_RATIO;
    if pixels > LIBWEBP_MAX_PIXELS || fast_len * den >= pixels * channels * num {
        return Vec::new();
    }
    if pixels <= LIBWEBP_TRY_BOTH_PIXELS {
        return vec![(0, 100.0), (1, 100.0)];
    }
    let Some(sample) = sample_rows(image) else {
        return vec![(0, 100.0), (1, 100.0)];
    };
    let mut fast = Vec::new();
    let size_with = |effort: (i32, f32)| {
        let mut buf = Vec::new();
        encode_webp_libwebp(&sample, options, Some(effort), &mut buf)
            .ok()
            .map(|_| buf.len())
    };
    if encode_webp_lossless_fast(&sample, &mut fast).is_err() {
        return Vec::new();
    }
    let (Some(m0), Some(m1)) = (size_with((0, 100.0)), size_with((1, 100.0))) else {
        return Vec::new();
    };
    let (gain_num, gain_den) = LIBWEBP_MIN_GAIN;
    if m0.min(m1) * gain_den > fast.len() * gain_num {
        return Vec::new();
    }
    // Method 0 (about 2.5x cheaper, and on whole noisy gradients often smaller than method 1,
    // which a sample cannot show) unless method 1 is clearly better on the sample.
    if m1 * 100 < m0 * 85 {
        vec![(1, 100.0)]
    } else {
        vec![(0, 100.0)]
    }
}

/// Eight evenly spaced bands of full rows (about 64 Ki pixels in all) as one image, or `None`
/// when the image is too short to be sampled.
fn sample_rows(image: &Image) -> Option<Image> {
    const BANDS: usize = 8;
    const TARGET: usize = 64 * 1024;
    let (w, h) = (image.width as usize, image.height as usize);
    let band = (TARGET / (w * BANDS)).max(2);
    if h < band * BANDS * 2 {
        return None;
    }
    let mut data = Vec::with_capacity(band * BANDS * w * 4);
    for i in 0..BANDS {
        let start = (h - band) * i / (BANDS - 1);
        data.extend_from_slice(&image.data[start * w * 4..(start + band) * w * 4]);
    }
    let mut sample = Image::new(image.width, (band * BANDS) as u32, data).ok()?;
    sample.has_alpha = image.has_alpha;
    sample.gray = image.gray;
    Some(sample)
}

/// True when a deterministic sample of `COLOUR_SAMPLE` pixels contains few distinct colours.
fn few_colours(image: &Image) -> bool {
    const SLOTS: usize = 8192;
    let pixels = image.width as usize * image.height as usize;
    let samples = pixels.min(COLOUR_SAMPLE);
    let step = (pixels / samples).max(1);
    let mut table = vec![u64::MAX; SLOTS];
    let mut distinct = 0usize;
    for i in 0..samples {
        // Strided with a fixed pseudo-random offset inside each stride so periodic content
        // cannot alias with the sampling grid.
        let index = (i * step + (i.wrapping_mul(7919) % step)).min(pixels - 1);
        let px = &image.data[index * 4..index * 4 + 4];
        // Fully transparent pixels count as one colour (libwebp discards their RGB).
        let key = if px[3] == 0 {
            0
        } else {
            u64::from(u32::from_le_bytes([px[0], px[1], px[2], px[3]])) | 1 << 32
        };
        let mut slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 51) as usize;
        loop {
            if table[slot] == key {
                break;
            }
            if table[slot] == u64::MAX {
                table[slot] = key;
                distinct += 1;
                break;
            }
            slot = (slot + 1) & (SLOTS - 1);
        }
    }
    distinct * COLOUR_SAMPLE <= COLOUR_SAMPLE_GRAPHIC_MAX * samples
}

/// Pure-Rust (`image-webp`) lossless encoder: very fast, larger files than libwebp.
fn encode_webp_lossless_fast(image: &Image, out: &mut Vec<u8>) -> Result<()> {
    {
        let encoder = WebPEncoder::new_lossless(&mut *out);
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
    }
}

fn encode_avif(image: &Image, options: &OutputOptions, out: &mut Vec<u8>) -> Result<()> {
    let speed = (11u8.saturating_sub(options.effort.clamp(1, 10))).clamp(1, 10);
    let mut encoder =
        AvifEncoder::new_with_speed_quality(&mut *out, speed, options.quality.clamp(1, 100));
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
    // GIF transparency is 1-bit: libvips (cgif) treats alpha below 128 as transparent.
    let mut pixels = image.data.clone();
    if image.has_alpha {
        for px in pixels.chunks_exact_mut(4) {
            px[3] = if px[3] < 128 { 0 } else { 255 };
        }
    } else {
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    let rgba = image::RgbaImage::from_raw(image.width, image.height, pixels)
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
    let exif = effective_exif(image, options);
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
        // Colour-space chunks (iCCP) must precede PLTE as well as IDAT.
        if tag == b"IDAT" || tag == b"IEND" || (tag == b"PLTE" && kind == b"iCCP") {
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
    if let Some(exif) = effective_exif(image, options) {
        insert_png_chunk(data, b"eXIf", &exif);
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

/// Pixels per inch from the EXIF `XResolution` / `ResolutionUnit` tags.
fn exif_density(bytes: &[u8]) -> Option<u32> {
    let mut cursor = Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    let x = exif.get_field(exif::Tag::XResolution, exif::In::PRIMARY)?;
    let value = match &x.value {
        exif::Value::Rational(values) => values.first()?.to_f64(),
        _ => return None,
    };
    let unit = exif
        .get_field(exif::Tag::ResolutionUnit, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .unwrap_or(2);
    match unit {
        2 => Some(value.round() as u32),
        3 => Some((value * 2.54).round() as u32),
        _ => None,
    }
    .filter(|dpi| *dpi > 25)
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

#[cfg(test)]
mod tests {
    use super::jpeg_scale_numerator;

    fn scaled(size: u32, numerator: u8) -> u32 {
        (size as u64 * numerator as u64).div_ceil(8) as u32
    }

    #[test]
    fn jpeg_scale_never_undershoots_and_is_minimal() {
        let sizes = [
            1, 2, 3, 7, 8, 9, 15, 100, 799, 800, 1000, 3000, 3001, 4000, 5000, 65_535,
        ];
        let targets = [1, 2, 3, 8, 50, 300, 1200, 2999, 3000, 6000];
        for &sw in &sizes {
            for &sh in &sizes {
                for &tw in &targets {
                    for &th in &targets {
                        for headroom in [1, 2] {
                            let n = jpeg_scale_numerator((sw, sh), (tw, th), headroom);
                            assert!((1..=8).contains(&n));
                            let need_w = (tw * headroom).min(sw);
                            let need_h = (th * headroom).min(sh);
                            assert!(
                                scaled(sw, n) >= need_w && scaled(sh, n) >= need_h,
                                "{sw}x{sh} -> {tw}x{th} x{headroom}: n={n} gives {}x{}",
                                scaled(sw, n),
                                scaled(sh, n)
                            );
                            if n > 1 {
                                let smaller = n - 1;
                                assert!(
                                    scaled(sw, smaller) < need_w || scaled(sh, smaller) < need_h,
                                    "{sw}x{sh} -> {tw}x{th} x{headroom}: n={n} is not minimal"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
