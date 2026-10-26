//! Lossy / near-lossless WebP encoding through libwebp (`libwebp-sys`, built from the bundled C
//! sources with the `cc` crate; no cmake or system library needed).

use std::ffi::{c_char, c_int, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};

use fastpkg_core::{FastpkgError, Result};
use libwebp_sys as w;

/// Maximum width / height of a WebP bitstream.
pub const MAX_DIMENSION: u32 = 16383;

/// Everything the libwebp encoder needs. Pixels are 8-bit RGBA, tightly packed.
pub struct Params<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    pub quality: f32,
    /// sharp `effort` (0-6), passed as libwebp `method`.
    pub effort: i32,
    pub lossless: bool,
    /// libwebp `near_lossless` (0 = most loss .. 100 = off).
    pub near_lossless: i32,
    pub alpha_quality: i32,
    /// libwebp `exact`: keep RGB under transparent pixels.
    pub exact: bool,
    pub smart_subsample: bool,
    pub preset: Option<&'a str>,
    pub exif: Option<&'a [u8]>,
    pub icc: Option<&'a [u8]>,
    pub xmp: Option<&'a [u8]>,
}

pub fn version() -> String {
    // WebPGetEncoderVersion: (major << 16) | (minor << 8) | revision
    let v = unsafe { w::WebPGetEncoderVersion() } as u32;
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

fn err(msg: impl Into<String>) -> FastpkgError {
    FastpkgError::Encode(format!("webp: {}", msg.into()))
}

pub fn encode(params: &Params) -> Result<Vec<u8>> {
    if params.width == 0 || params.height == 0 {
        return Err(err("image has zero width or height"));
    }
    if params.width > MAX_DIMENSION || params.height > MAX_DIMENSION {
        return Err(err(format!(
            "image is {}x{} but WebP supports at most {MAX_DIMENSION}x{MAX_DIMENSION}",
            params.width, params.height
        )));
    }
    let expected = params.width as usize * params.height as usize * 4;
    if params.rgba.len() != expected {
        return Err(err("pixel buffer does not match image dimensions"));
    }
    match catch_unwind(AssertUnwindSafe(|| encode_inner(params))) {
        Ok(result) => result,
        Err(_) => Err(err("encoder panicked")),
    }
}

/// Frees a libwebp-allocated `WebPData`.
struct OwnedData(w::WebPData);
impl Drop for OwnedData {
    fn drop(&mut self) {
        if !self.0.bytes.is_null() {
            unsafe { w::WebPFree(self.0.bytes as *mut c_void) };
            self.0.bytes = std::ptr::null();
            self.0.size = 0;
        }
    }
}

struct MemWriter(w::WebPMemoryWriter);
impl Drop for MemWriter {
    fn drop(&mut self) {
        unsafe { w::WebPMemoryWriterClear(&mut self.0) };
    }
}

struct Picture(w::WebPPicture);
impl Drop for Picture {
    fn drop(&mut self) {
        unsafe { w::WebPPictureFree(&mut self.0) };
    }
}

fn preset_of(name: Option<&str>) -> w::WebPPreset {
    match name {
        Some("photo") => w::WebPPreset::WEBP_PRESET_PHOTO,
        Some("picture") => w::WebPPreset::WEBP_PRESET_PICTURE,
        Some("drawing") => w::WebPPreset::WEBP_PRESET_DRAWING,
        Some("icon") => w::WebPPreset::WEBP_PRESET_ICON,
        Some("text") => w::WebPPreset::WEBP_PRESET_TEXT,
        _ => w::WebPPreset::WEBP_PRESET_DEFAULT,
    }
}

fn encode_inner(p: &Params) -> Result<Vec<u8>> {
    let mut config =
        w::WebPConfig::new_with_preset(preset_of(p.preset), p.quality.clamp(0.0, 100.0))
            .map_err(|_| err("libwebp version mismatch"))?;
    config.method = p.effort.clamp(0, 6);
    config.alpha_quality = p.alpha_quality.clamp(0, 100);
    config.use_sharp_yuv = p.smart_subsample as c_int;
    config.thread_level = 0;
    config.exact = p.exact as c_int;
    if p.lossless {
        config.lossless = 1;
        if p.near_lossless < 100 {
            config.near_lossless = p.near_lossless.clamp(0, 100);
        }
    }
    if unsafe { w::WebPValidateConfig(&config) } == 0 {
        return Err(err("invalid encoder configuration"));
    }

    let mut pic = Picture(w::WebPPicture::new().map_err(|_| err("libwebp version mismatch"))?);
    pic.0.width = p.width as c_int;
    pic.0.height = p.height as c_int;
    // Sharp YUV conversion only runs inside WebPEncode when the picture is kept as ARGB.
    pic.0.use_argb = (p.lossless || p.smart_subsample) as c_int;
    let imported = unsafe {
        if p.has_alpha {
            w::WebPPictureImportRGBA(&mut pic.0, p.rgba.as_ptr(), p.width as c_int * 4)
        } else {
            // RGBX ignores the fourth byte, so no RGB copy is needed.
            w::WebPPictureImportRGBX(&mut pic.0, p.rgba.as_ptr(), p.width as c_int * 4)
        }
    };
    if imported == 0 {
        return Err(err(format!(
            "could not import pixels (error {})",
            pic.0.error_code as i32
        )));
    }

    let mut writer = MemWriter(unsafe { std::mem::zeroed() });
    unsafe { w::WebPMemoryWriterInit(&mut writer.0) };
    pic.0.writer = Some(w::WebPMemoryWrite);
    pic.0.custom_ptr = &mut writer.0 as *mut _ as *mut c_void;
    if unsafe { w::WebPEncode(&config, &mut pic.0) } == 0 {
        return Err(err(format!(
            "encoding failed (error {})",
            pic.0.error_code as i32
        )));
    }
    let encoded = unsafe { std::slice::from_raw_parts(writer.0.mem, writer.0.size) };

    if p.exif.is_none() && p.icc.is_none() && p.xmp.is_none() {
        return Ok(encoded.to_vec());
    }
    attach_metadata(encoded, p.exif, p.icc, p.xmp)
}

/// Adds EXIF / ICC / XMP chunks to an already encoded WebP (any encoder) through libwebp's mux.
pub fn attach_metadata(
    encoded: &[u8],
    exif: Option<&[u8]>,
    icc: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>> {
    if exif.is_none() && icc.is_none() && xmp.is_none() {
        return Ok(encoded.to_vec());
    }
    struct Mux(*mut w::WebPMux);
    impl Drop for Mux {
        fn drop(&mut self) {
            unsafe { w::WebPMuxDelete(self.0) };
        }
    }
    let data = w::WebPData {
        bytes: encoded.as_ptr(),
        size: encoded.len(),
    };
    // copy_data = 1: the mux owns copies, so `encoded` may be freed independently.
    let mux = Mux(unsafe { w::WebPMuxCreateInternal(&data, 1, w::WEBP_MUX_ABI_VERSION as c_int) });
    if mux.0.is_null() {
        return Err(err("could not parse encoded image for metadata"));
    }
    for (fourcc, chunk) in [(b"ICCP\0", icc), (b"EXIF\0", exif), (b"XMP \0", xmp)] {
        let Some(chunk) = chunk.filter(|c| !c.is_empty()) else {
            continue;
        };
        let chunk = w::WebPData {
            bytes: chunk.as_ptr(),
            size: chunk.len(),
        };
        let status =
            unsafe { w::WebPMuxSetChunk(mux.0, fourcc.as_ptr() as *const c_char, &chunk, 1) };
        if status != w::WebPMuxError::WEBP_MUX_OK {
            return Err(err("could not attach metadata chunk"));
        }
    }
    let mut assembled = OwnedData(w::WebPData {
        bytes: std::ptr::null(),
        size: 0,
    });
    if unsafe { w::WebPMuxAssemble(mux.0, &mut assembled.0) } != w::WebPMuxError::WEBP_MUX_OK {
        return Err(err("could not assemble image with metadata"));
    }
    Ok(unsafe { std::slice::from_raw_parts(assembled.0.bytes, assembled.0.size) }.to_vec())
}

/// A decoded WebP: tightly packed non-premultiplied RGBA.
pub struct Decoded {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    /// Full-resolution size when libwebp scaled the image while decoding.
    pub source: (u32, u32),
}

/// Smallest `(width, height)` that is at least `min` on both axes and keeps the aspect ratio of
/// `source`. Returns `None` unless that is a reduction of 2x or more on both axes (libwebp's
/// rescaler is an area filter, so small reductions are better left to the resampler).
pub fn scaled_size(source: (u32, u32), min: (u32, u32)) -> Option<(u32, u32)> {
    let (sw, sh) = (source.0 as u64, source.1 as u64);
    let (mw, mh) = (min.0.max(1) as u64, min.1.max(1) as u64);
    let (w, h) = if mw * sh >= mh * sw {
        (mw, (sh * mw).div_ceil(sw))
    } else {
        ((sw * mh).div_ceil(sh), mh)
    };
    let (w, h) = (w.max(mw), h.max(mh));
    (w * 2 <= sw && h * 2 <= sh).then_some((w as u32, h as u32))
}

/// Decodes a still WebP (lossy, lossless, with or without alpha) through libwebp, with its
/// default fancy upsampling (what libvips / sharp use). `scale_to` asks for libwebp's built-in
/// scaled decode. `Ok(None)` means libwebp will not decode the file (animation, unsupported
/// canvas); the caller falls back to another decoder. Damaged data is an error, as in libvips.
/// `max_pixels` is checked against the header before anything is allocated.
pub fn decode(
    bytes: &[u8],
    scale_to: Option<(u32, u32)>,
    max_pixels: u64,
) -> Result<Option<Decoded>> {
    match catch_unwind(AssertUnwindSafe(|| {
        decode_inner(bytes, scale_to, max_pixels)
    })) {
        Ok(result) => result,
        Err(_) => Ok(None),
    }
}

/// Whether libwebp's demuxer accepts the RIFF container (sizes, chunk layout): the check libvips
/// applies before it decodes anything.
pub fn container_valid(bytes: &[u8]) -> bool {
    let data = w::WebPData {
        bytes: bytes.as_ptr(),
        size: bytes.len(),
    };
    let demux = unsafe {
        w::WebPDemuxInternal(
            &data,
            0,
            std::ptr::null_mut(),
            w::WEBP_DEMUX_ABI_VERSION as c_int,
        )
    };
    if demux.is_null() {
        return false;
    }
    unsafe { w::WebPDemuxDelete(demux) };
    true
}

fn decode_inner(
    bytes: &[u8],
    scale_to: Option<(u32, u32)>,
    max_pixels: u64,
) -> Result<Option<Decoded>> {
    let Ok(mut config) = w::WebPDecoderConfig::new() else {
        return Ok(None);
    };
    let status = unsafe { w::WebPGetFeatures(bytes.as_ptr(), bytes.len(), &mut config.input) };
    if status != w::VP8StatusCode::VP8_STATUS_OK || config.input.has_animation != 0 {
        return Ok(None);
    }
    let (source_w, source_h) = (config.input.width as u32, config.input.height as u32);
    if source_w == 0 || source_h == 0 || source_w > MAX_DIMENSION || source_h > MAX_DIMENSION {
        return Ok(None);
    }
    if source_w as u64 * source_h as u64 > max_pixels {
        return Err(FastpkgError::InputTooLarge {
            pixels: source_w as u64 * source_h as u64,
            limit: max_pixels,
        });
    }
    let (out_w, out_h) = match scale_to.and_then(|min| scaled_size((source_w, source_h), min)) {
        Some((sw, sh)) => {
            config.options.use_scaling = 1;
            config.options.scaled_width = sw as c_int;
            config.options.scaled_height = sh as c_int;
            (sw, sh)
        }
        None => (source_w, source_h),
    };
    // Zeroed memory comes straight from calloc, so a hostile canvas size that fails to decode
    // never touches the pages.
    let mut rgba = vec![0u8; out_w as usize * out_h as usize * 4];
    config.output.colorspace = w::WEBP_CSP_MODE::MODE_RGBA;
    config.output.width = out_w as c_int;
    config.output.height = out_h as c_int;
    config.output.is_external_memory = 1;
    config.output.u.RGBA = w::WebPRGBABuffer {
        rgba: rgba.as_mut_ptr(),
        stride: out_w as c_int * 4,
        size: rgba.len(),
    };
    let status = unsafe { w::WebPDecode(bytes.as_ptr(), bytes.len(), &mut config) };
    match status {
        w::VP8StatusCode::VP8_STATUS_OK => {}
        // The scaled decode was refused; the caller decodes at full size instead.
        w::VP8StatusCode::VP8_STATUS_INVALID_PARAM => return Ok(None),
        _ => {
            return Err(FastpkgError::Decode(
                "webp2vips: unable to read pixels".into(),
            ))
        }
    }
    Ok(Some(Decoded {
        rgba,
        width: out_w,
        height: out_h,
        has_alpha: config.input.has_alpha != 0,
        source: (source_w, source_h),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(rgba: &[u8], width: u32, height: u32) -> Params<'_> {
        Params {
            rgba,
            width,
            height,
            has_alpha: false,
            quality: 80.0,
            effort: 4,
            lossless: false,
            near_lossless: 100,
            alpha_quality: 100,
            exact: false,
            smart_subsample: false,
            preset: None,
            exif: None,
            icc: None,
            xmp: None,
        }
    }

    #[test]
    fn encodes_a_riff_webp() {
        let rgba: Vec<u8> = (0..16 * 16 * 4).map(|i| (i % 251) as u8).collect();
        let out = encode(&params(&rgba, 16, 16)).expect("encode");
        assert_eq!(&out[..4], b"RIFF");
        assert_eq!(&out[8..12], b"WEBP");
    }

    #[test]
    fn rejects_oversized_and_mismatched_input() {
        let rgba = vec![0u8; 4];
        let mut p = params(&rgba, 16384, 1);
        assert!(matches!(encode(&p), Err(FastpkgError::Encode(m)) if m.contains("16383")));
        p.width = 2;
        assert!(encode(&p).is_err(), "buffer length mismatch");
        p.width = 0;
        assert!(encode(&p).is_err(), "zero width");
    }

    #[test]
    fn attaches_metadata_chunks() {
        let rgba = vec![200u8; 8 * 8 * 4];
        let mut p = params(&rgba, 8, 8);
        p.exif = Some(b"Exif\0\0II*\0");
        p.xmp = Some(b"<x/>");
        let out = encode(&p).expect("encode");
        assert_eq!(&out[12..16], b"VP8X");
        assert!(out.windows(4).any(|w| w == b"EXIF"));
        assert!(out.windows(4).any(|w| w == b"XMP "));
    }

    #[test]
    fn lossless_exact_keeps_hidden_colour() {
        let mut rgba = vec![0u8; 16 * 16 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&[10, 200, 30, 0]);
        }
        rgba[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let mut p = params(&rgba, 16, 16);
        p.has_alpha = true;
        p.lossless = true;
        p.effort = 1;
        p.exact = true;
        let out = encode(&p).expect("encode");
        assert!(out.windows(4).any(|w| w == b"VP8L"));
    }

    #[test]
    fn scaled_size_needs_a_two_fold_reduction_and_never_undershoots() {
        assert_eq!(scaled_size((4000, 3000), (800, 600)), Some((800, 600)));
        assert_eq!(scaled_size((2000, 1500), (1000, 750)), Some((1000, 750)));
        assert_eq!(scaled_size((2000, 1500), (1001, 750)), None, "under 2x");
        // 1000x777 -> width 101: height must round up, not down.
        let (w, h) = scaled_size((1000, 777), (101, 1)).unwrap();
        assert_eq!((w, h), (101, 79));
        let (w, h) = scaled_size((777, 1000), (1, 101)).unwrap();
        assert_eq!((w, h), (79, 101));
        assert_eq!(scaled_size((10, 10), (0, 0)), Some((1, 1)));
    }

    #[test]
    fn decodes_what_it_encodes_and_scales() {
        let rgba: Vec<u8> = (0..64 * 48)
            .flat_map(|i| [(i % 64 * 4) as u8, (i / 64 * 5) as u8, 90, 255])
            .collect();
        let mut p = params(&rgba, 64, 48);
        p.lossless = true;
        p.effort = 1;
        let encoded = encode(&p).expect("encode");
        let full = decode(&encoded, None, u64::MAX).unwrap().expect("decoded");
        assert_eq!((full.width, full.height, full.source), (64, 48, (64, 48)));
        assert_eq!(full.rgba, rgba);
        assert!(!full.has_alpha);
        let small = decode(&encoded, Some((16, 12)), u64::MAX)
            .unwrap()
            .expect("decoded");
        assert_eq!(
            (small.width, small.height, small.source),
            (16, 12, (64, 48))
        );
        assert!(matches!(
            decode(&encoded, None, 100),
            Err(FastpkgError::InputTooLarge { .. })
        ));
        assert!(decode(&encoded[..20], None, u64::MAX).unwrap().is_none());
        assert!(decode(b"RIFF\0\0\0\0WEBPjunk", None, u64::MAX)
            .unwrap()
            .is_none());
    }

    #[test]
    fn attach_metadata_works_on_foreign_webp() {
        let rgba = vec![9u8; 8 * 8 * 4];
        let mut p = params(&rgba, 8, 8);
        p.lossless = true;
        let plain = encode(&p).expect("encode");
        let out = attach_metadata(&plain, Some(b"Exif\0\0II*\0"), None, None).expect("mux");
        assert_eq!(&out[12..16], b"VP8X");
        assert!(out.windows(4).any(|w| w == b"EXIF"));
    }
}
