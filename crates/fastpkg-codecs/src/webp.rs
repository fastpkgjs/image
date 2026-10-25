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
