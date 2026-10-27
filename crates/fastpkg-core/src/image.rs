use std::borrow::Cow;

use crate::color::Color;
use crate::{FastpkgError, Format, Result};
use rayon::prelude::*;

/// Images with fewer pixels than this are converted on the calling thread: waking the pool
/// costs more than the conversion saves.
pub const PAR_MIN_PIXELS: usize = 1 << 18;
/// Pixels handled per rayon task by the conversions below (about 128 KiB of RGBA input).
const PAR_CHUNK_PIXELS: usize = 1 << 15;

/// Builds a packed buffer of `out_ch` samples per pixel from RGBA input. `convert` receives a
/// run of whole pixels and the matching output run; large inputs are split across the pool.
fn pack<T: Copy + Default + Send + Sync, S: Sync>(
    src: &[S],
    out_ch: usize,
    convert: impl Fn(&[S], &mut [T]) + Sync,
) -> Vec<T> {
    let pixels = src.len() / 4;
    let mut out = vec![T::default(); pixels * out_ch];
    if pixels >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
        out.par_chunks_mut(PAR_CHUNK_PIXELS * out_ch)
            .zip(src.par_chunks(PAR_CHUNK_PIXELS * 4))
            .for_each(|(dst, src)| convert(src, dst));
    } else {
        convert(src, &mut out);
    }
    out
}

/// Expands `in_ch`-channel samples to opaque RGBA (alpha preset to 255 for `convert` to keep or
/// overwrite); `convert` gets a run of source samples and the matching RGBA output run.
fn expand(src: &[u8], in_ch: usize, convert: impl Fn(&[u8], &mut [u8]) + Sync) -> Vec<u8> {
    let pixels = src.len() / in_ch;
    let mut out = vec![255u8; pixels * 4];
    if pixels >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
        out.par_chunks_mut(PAR_CHUNK_PIXELS * 4)
            .zip(src.par_chunks(PAR_CHUNK_PIXELS * in_ch))
            .for_each(|(dst, src)| convert(src, dst));
    } else {
        convert(src, &mut out);
    }
    out
}

/// Runs `f` over disjoint runs of whole RGBA pixels, in parallel for large buffers.
fn for_each_pixel_run<T: Send>(data: &mut [T], f: impl Fn(&mut [T]) + Sync) {
    if data.len() / 4 >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
        data.par_chunks_mut(PAR_CHUNK_PIXELS * 4).for_each(&f);
    } else {
        f(data);
    }
}

#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub wide: Option<Vec<u16>>,
    pub has_alpha: bool,
    pub gray: bool,
    /// Full-resolution size of the source when the decoder already shrank it
    /// (JPEG DCT scaling). Resize targets are computed from this, not from `width`/`height`.
    pub pre_shrink: Option<(u32, u32)>,
    pub format: Format,
    pub space: String,
    pub depth: String,
    pub orientation: Option<u32>,
    pub icc: Option<Vec<u8>>,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub delay: Option<Vec<u32>>,
    pub loop_count: Option<u32>,
}

impl Image {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let expected = width as usize * height as usize * 4;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "RGBA buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        Ok(Self {
            width,
            height,
            data,
            wide: None,
            has_alpha: true,
            gray: false,
            pre_shrink: None,
            format: Format::Raw,
            space: "srgb".into(),
            depth: "uchar".into(),
            orientation: None,
            icc: None,
            exif: None,
            xmp: None,
            delay: None,
            loop_count: None,
        })
    }

    pub fn from_rgba(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::new(width, height, data)
    }

    pub fn from_rgba16(width: u32, height: u32, data: Vec<u16>) -> Result<Self> {
        let mut image = Self::from_rgba16_deferred(width, height, data)?;
        image.materialize_8bit();
        Ok(image)
    }

    /// Like [`Image::from_rgba16`] but without the 8-bit twin of the samples (`data` is empty
    /// until [`Image::materialize_8bit`]). For frames big enough that a second copy matters, when
    /// the next thing that happens to them is an operation that works on the wide samples alone
    /// (a resize, flips and quarter turns); `fastpkg_ops::apply` restores the twin before any
    /// other operation.
    pub fn from_rgba16_deferred(width: u32, height: u32, data: Vec<u16>) -> Result<Self> {
        let expected = width as usize * height as usize * 4;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "RGBA16 buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let mut image = Self::new(0, 0, Vec::new())?;
        image.width = width;
        image.height = height;
        image.has_alpha = true;
        image.depth = "ushort".into();
        image.wide = Some(data);
        Ok(image)
    }

    /// Builds the 8-bit twin of a wide image whose twin was deferred (nothing otherwise).
    pub fn materialize_8bit(&mut self) {
        if let Some(wide) = self.wide.as_ref() {
            if self.data.is_empty() && !wide.is_empty() {
                self.data = wide_to_rgba8(wide);
            }
        }
    }

    pub fn from_raw16(width: u32, height: u32, channels: u8, data: &[u16]) -> Result<Self> {
        let pixels = width as usize * height as usize;
        let expected = pixels * channels as usize;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "raw16 buffer length {} does not match {}x{}x{}",
                data.len(),
                width,
                height,
                channels
            )));
        }
        let mut rgba = vec![65_535u16; pixels * 4];
        match channels {
            1 => {
                for (src, dst) in data.iter().zip(rgba.chunks_exact_mut(4)) {
                    dst[0] = *src;
                    dst[1] = *src;
                    dst[2] = *src;
                }
            }
            2 => {
                for (src, dst) in data.chunks_exact(2).zip(rgba.chunks_exact_mut(4)) {
                    dst[0] = src[0];
                    dst[1] = src[0];
                    dst[2] = src[0];
                    dst[3] = src[1];
                }
            }
            3 => {
                for (src, dst) in data.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
                    dst[0] = src[0];
                    dst[1] = src[1];
                    dst[2] = src[2];
                }
            }
            4 => rgba.copy_from_slice(data),
            other => {
                return Err(FastpkgError::InvalidOptions(format!(
                    "unsupported raw16 channel count {other}"
                )))
            }
        }
        let mut image = Self::from_rgba16(width, height, rgba)?;
        image.has_alpha = matches!(channels, 2 | 4);
        image.gray = matches!(channels, 1 | 2);
        image.format = Format::Raw;
        Ok(image)
    }

    pub fn from_rgb(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::from_rgb_slice(width, height, &data)
    }

    pub fn from_rgb_slice(width: u32, height: u32, data: &[u8]) -> Result<Self> {
        let expected = width as usize * height as usize * 3;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "RGB buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let rgba = expand(data, 3, |src, dst| {
            for (src, dst) in src.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
                dst[..3].copy_from_slice(src);
            }
        });
        let mut img = Self::new(width, height, rgba)?;
        img.has_alpha = false;
        Ok(img)
    }

    pub fn from_luma(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::from_luma_slice(width, height, &data)
    }

    pub fn from_luma_slice(width: u32, height: u32, data: &[u8]) -> Result<Self> {
        let expected = width as usize * height as usize;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "luma buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let rgba = expand(data, 1, |src, dst| {
            for (src, dst) in src.iter().zip(dst.chunks_exact_mut(4)) {
                dst[..3].fill(*src);
            }
        });
        let mut img = Self::new(width, height, rgba)?;
        img.has_alpha = false;
        img.gray = true;
        Ok(img)
    }

    /// Grey + alpha samples, expanded to RGBA like every other grey input.
    pub fn from_luma_alpha(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        Self::from_luma_alpha_slice(width, height, &data)
    }

    pub fn from_luma_alpha_slice(width: u32, height: u32, data: &[u8]) -> Result<Self> {
        let expected = width as usize * height as usize * 2;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "grey+alpha buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let rgba = expand(data, 2, |src, dst| {
            for (src, dst) in src.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
                dst[..3].fill(src[0]);
                dst[3] = src[1];
            }
        });
        let mut img = Self::new(width, height, rgba)?;
        img.has_alpha = true;
        img.gray = true;
        Ok(img)
    }

    pub fn from_raw(width: u32, height: u32, channels: u8, data: Vec<u8>) -> Result<Self> {
        if channels == 4 {
            return Self::from_rgba(width, height, data);
        }
        Self::from_raw_slice(width, height, channels, &data)
    }

    /// Like [`Image::from_raw`], reading from a borrowed buffer (no intermediate copy for 1-3
    /// channels).
    pub fn from_raw_slice(width: u32, height: u32, channels: u8, data: &[u8]) -> Result<Self> {
        match channels {
            1 => Self::from_luma_slice(width, height, data),
            2 => Self::from_luma_alpha_slice(width, height, data),
            3 => Self::from_rgb_slice(width, height, data),
            4 => Self::from_rgba(
                width,
                height,
                pack(data, 4, |src, dst| dst.copy_from_slice(src)),
            ),
            other => Err(FastpkgError::InvalidOptions(format!(
                "unsupported raw channel count {other}"
            ))),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    pub fn get(&self, x: u32, y: u32) -> Color {
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        Color {
            r: self.data[idx],
            g: self.data[idx + 1],
            b: self.data[idx + 2],
            alpha: self.data[idx + 3],
        }
    }

    pub fn set(&mut self, x: u32, y: u32, color: Color) {
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        self.data[idx] = color.r;
        self.data[idx + 1] = color.g;
        self.data[idx + 2] = color.b;
        self.data[idx + 3] = color.alpha;
    }

    /// True when every pixel has r == g == b, so a single-channel layout loses nothing.
    pub fn is_neutral(&self) -> bool {
        let neutral = |run: &[u8]| {
            run.chunks_exact(4)
                .all(|px| px[0] == px[1] && px[1] == px[2])
        };
        if self.pixel_count() >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
            self.data.par_chunks(PAR_CHUNK_PIXELS * 4).all(neutral)
        } else {
            neutral(&self.data)
        }
    }

    /// True when any 8-bit alpha sample is below 255 (ignores `has_alpha` and `wide`).
    pub fn has_translucent_alpha8(&self) -> bool {
        let translucent = |run: &[u8]| run.chunks_exact(4).any(|px| px[3] != 255);
        if self.pixel_count() >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
            self.data.par_chunks(PAR_CHUNK_PIXELS * 4).any(translucent)
        } else {
            translucent(&self.data)
        }
    }

    /// A copy of this image's metadata around new 8-bit RGBA pixels (`wide` is dropped and the
    /// result is opaque). `data` must hold `width * height * 4` bytes.
    pub fn with_opaque_pixels(&self, data: Vec<u8>) -> Image {
        Image {
            width: self.width,
            height: self.height,
            data,
            wide: None,
            has_alpha: false,
            gray: self.gray,
            pre_shrink: self.pre_shrink,
            format: self.format,
            space: self.space.clone(),
            depth: self.depth.clone(),
            orientation: self.orientation,
            icc: self.icc.clone(),
            exif: self.exif.clone(),
            xmp: self.xmp.clone(),
            delay: self.delay.clone(),
            loop_count: self.loop_count,
        }
    }

    /// RGBA pixels composited onto black (what JPEG output needs); same arithmetic as
    /// [`Image::flatten`] with a black background, without cloning the source first.
    pub fn flattened_onto_black(&self) -> Vec<u8> {
        pack(&self.data, 4, |src, dst| {
            for (d, px) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                let a = px[3] as u16;
                d[0] = ((px[0] as u16 * a + 127) / 255) as u8;
                d[1] = ((px[1] as u16 * a + 127) / 255) as u8;
                d[2] = ((px[2] as u16 * a + 127) / 255) as u8;
                d[3] = 255;
            }
        })
    }

    /// First channel of every pixel (the luma plane of a gray image).
    pub fn luma_plane(&self) -> Vec<u8> {
        pack(&self.data, 1, |src, dst| {
            for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                *d = px[0];
            }
        })
    }

    /// Number of interleaved channels a gray-aware encoder emits for this image.
    pub fn output_channels(&self) -> u8 {
        let colour = if self.gray { 1 } else { 3 };
        colour + u8::from(self.has_alpha)
    }

    /// Packed 8-bit samples: `L`, `LA`, `RGB` or `RGBA` depending on `gray`/`has_alpha`.
    pub fn to_packed(&self) -> Vec<u8> {
        match (self.gray, self.has_alpha) {
            (false, true) => pack(&self.data, 4, |src, dst| dst.copy_from_slice(src)),
            (false, false) => self.to_rgb(),
            (true, false) => self.luma_plane(),
            (true, true) => pack(&self.data, 2, |src, dst| {
                for (d, px) in dst.chunks_exact_mut(2).zip(src.chunks_exact(4)) {
                    d[0] = px[0];
                    d[1] = px[3];
                }
            }),
        }
    }

    /// Packed 16-bit samples with the same layout as [`Image::to_packed`].
    pub fn to_packed16(&self) -> Vec<u16> {
        let rgba = self.rgba16();
        match (self.gray, self.has_alpha) {
            (false, true) => rgba.into_owned(),
            (false, false) => pack(&rgba, 3, |src, dst| {
                for (d, px) in dst.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                    d.copy_from_slice(&px[..3]);
                }
            }),
            (true, false) => pack(&rgba, 1, |src, dst| {
                for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = px[0];
                }
            }),
            (true, true) => pack(&rgba, 2, |src, dst| {
                for (d, px) in dst.chunks_exact_mut(2).zip(src.chunks_exact(4)) {
                    d[0] = px[0];
                    d[1] = px[3];
                }
            }),
        }
    }

    pub fn to_rgb(&self) -> Vec<u8> {
        pack(&self.data, 3, |src, dst| {
            for (d, px) in dst.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&px[..3]);
            }
        })
    }

    pub fn to_rgba(&self) -> &[u8] {
        &self.data
    }

    /// 16-bit RGBA samples: borrowed when the image is wide, widened from the 8-bit samples
    /// otherwise.
    pub fn rgba16(&self) -> Cow<'_, [u16]> {
        match self.wide.as_deref() {
            Some(wide) => Cow::Borrowed(wide),
            None => Cow::Owned(pack(&self.data, 4, |src, dst| {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = *s as u16 * 257;
                }
            })),
        }
    }

    /// An owned copy of [`Image::rgba16`].
    pub fn to_rgba16(&self) -> Vec<u16> {
        self.rgba16().into_owned()
    }

    /// Moves the wide samples out (widening 8-bit ones) so an operation can rebuild the image
    /// from them without a second full-size copy. The caller must store a result with
    /// [`Image::set_wide_rgba`] afterwards.
    pub fn take_rgba16(&mut self) -> Vec<u16> {
        match self.wide.take() {
            Some(wide) => wide,
            None => self.to_rgba16(),
        }
    }

    pub fn to_rgb16(&self) -> Vec<u16> {
        let rgba = self.rgba16();
        pack(&rgba, 3, |src, dst| {
            for (d, px) in dst.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&px[..3]);
            }
        })
    }

    pub fn to_luma16(&self) -> Vec<u16> {
        let rgba = self.rgba16();
        pack(&rgba, 1, |src, dst| {
            for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                *d = px[0];
            }
        })
    }

    pub fn has_wide(&self) -> bool {
        self.wide.is_some()
    }

    pub fn set_wide_rgba(&mut self, width: u32, height: u32, data: Vec<u16>) -> Result<()> {
        let expected = width as usize * height as usize * 4;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "RGBA16 buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        self.width = width;
        self.height = height;
        self.data = wide_to_rgba8(&data);
        self.wide = Some(data);
        self.depth = "ushort".into();
        Ok(())
    }

    pub fn discard_wide(&mut self) {
        self.wide = None;
        if self.depth == "ushort" {
            self.depth = "uchar".into();
        }
    }

    pub fn ensure_alpha(&mut self, alpha: u8) {
        if !self.has_alpha {
            for_each_pixel_run(&mut self.data, |run| {
                for px in run.chunks_exact_mut(4) {
                    px[3] = alpha;
                }
            });
            if let Some(wide) = self.wide.as_mut() {
                let alpha = alpha as u16 * 257;
                for_each_pixel_run(wide, |run| {
                    for px in run.chunks_exact_mut(4) {
                        px[3] = alpha;
                    }
                });
            }
            self.has_alpha = true;
        }
    }

    pub fn remove_alpha(&mut self) {
        for_each_pixel_run(&mut self.data, |run| {
            for px in run.chunks_exact_mut(4) {
                px[3] = 255;
            }
        });
        if let Some(wide) = self.wide.as_mut() {
            for_each_pixel_run(wide, |run| {
                for px in run.chunks_exact_mut(4) {
                    px[3] = 65_535;
                }
            });
        }
        self.has_alpha = false;
    }

    pub fn flatten(&mut self, background: Color) {
        if !self.has_alpha {
            return;
        }
        for_each_pixel_run(&mut self.data, |run| {
            for px in run.chunks_exact_mut(4) {
                let a = px[3] as u16;
                if a == 255 {
                    continue;
                }
                let inv = 255 - a;
                px[0] = ((px[0] as u16 * a + background.r as u16 * inv + 127) / 255) as u8;
                px[1] = ((px[1] as u16 * a + background.g as u16 * inv + 127) / 255) as u8;
                px[2] = ((px[2] as u16 * a + background.b as u16 * inv + 127) / 255) as u8;
                px[3] = 255;
            }
        });
        self.has_alpha = false;
    }

    pub fn premultiply(&mut self) {
        for_each_pixel_run(&mut self.data, |run| {
            for px in run.chunks_exact_mut(4) {
                let a = px[3] as u16;
                px[0] = ((px[0] as u16 * a + 127) / 255) as u8;
                px[1] = ((px[1] as u16 * a + 127) / 255) as u8;
                px[2] = ((px[2] as u16 * a + 127) / 255) as u8;
            }
        });
    }

    pub fn unpremultiply(&mut self) {
        if let Some(wide) = self.wide.as_mut() {
            for_each_pixel_run(wide, |run| {
                for px in run.chunks_exact_mut(4) {
                    let a = px[3] as u32;
                    if a == 0 || a == 65_535 {
                        continue;
                    }
                    px[0] = ((px[0] as u32 * 65_535 + a / 2) / a).min(65_535) as u16;
                    px[1] = ((px[1] as u32 * 65_535 + a / 2) / a).min(65_535) as u16;
                    px[2] = ((px[2] as u32 * 65_535 + a / 2) / a).min(65_535) as u16;
                }
            });
            self.data = wide_to_rgba8(wide);
            return;
        }
        for_each_pixel_run(&mut self.data, |run| {
            for px in run.chunks_exact_mut(4) {
                let a = px[3] as u16;
                if a == 0 || a == 255 {
                    continue;
                }
                px[0] = ((px[0] as u16 * 255 + a / 2) / a).min(255) as u8;
                px[1] = ((px[1] as u16 * 255 + a / 2) / a).min(255) as u8;
                px[2] = ((px[2] as u16 * 255 + a / 2) / a).min(255) as u8;
            }
        });
    }

    pub fn has_transparency(&self) -> bool {
        if let Some(wide) = self.wide.as_ref() {
            return if wide.len() / 4 >= PAR_MIN_PIXELS && rayon::current_num_threads() > 1 {
                wide.par_chunks(PAR_CHUNK_PIXELS * 4)
                    .any(|run| run.chunks_exact(4).any(|px| px[3] != 65_535))
            } else {
                wide.chunks_exact(4).any(|px| px[3] != 65_535)
            };
        }
        self.has_translucent_alpha8()
    }
}

fn wide_to_rgba8(wide: &[u16]) -> Vec<u8> {
    pack(wide, 4, |src, dst| {
        for (d, value) in dst.iter_mut().zip(src) {
            *d = ((*value as u32 * 255 + 32_767) / 65_535) as u8;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Large enough to take the parallel branches (>= PAR_MIN_PIXELS) with a ragged tail.
    fn big(gray: bool, has_alpha: bool) -> Image {
        let (w, h) = (700u32, 401u32);
        let data: Vec<u8> = (0..w as usize * h as usize * 4)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mut image = Image::new(w, h, data).unwrap();
        if gray {
            for px in image.data.chunks_exact_mut(4) {
                px[1] = px[0];
                px[2] = px[0];
            }
        }
        image.gray = gray;
        image.has_alpha = has_alpha;
        image
    }

    #[test]
    fn parallel_conversions_match_the_per_pixel_definition() {
        assert!(big(false, false).pixel_count() >= PAR_MIN_PIXELS);
        for (gray, alpha) in [(false, false), (false, true), (true, false), (true, true)] {
            let image = big(gray, alpha);
            let expected: Vec<u8> = image
                .data
                .chunks_exact(4)
                .flat_map(|px| match (gray, alpha) {
                    (false, false) => vec![px[0], px[1], px[2]],
                    (false, true) => px.to_vec(),
                    (true, false) => vec![px[0]],
                    (true, true) => vec![px[0], px[3]],
                })
                .collect();
            assert_eq!(image.to_packed(), expected, "gray={gray} alpha={alpha}");
            let wide: Vec<u16> = expected.iter().map(|v| *v as u16 * 257).collect();
            assert_eq!(
                image.to_packed16(),
                wide,
                "16-bit gray={gray} alpha={alpha}"
            );
        }
        let image = big(false, true);
        let rgb16: Vec<u16> = image
            .data
            .chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .map(|v| v as u16 * 257)
            .collect();
        assert_eq!(image.to_rgb16(), rgb16);
    }

    #[test]
    fn parallel_alpha_helpers_match_serial_arithmetic() {
        let image = big(false, true);
        assert!(image.has_translucent_alpha8());
        assert!(!image.is_neutral());
        assert!(big(true, false).is_neutral());
        let flat = image.flattened_onto_black();
        let mut reference = image.clone();
        reference.flatten(Color::BLACK);
        assert_eq!(flat, reference.data);
        let mut opaque = image.clone();
        opaque.remove_alpha();
        assert!(!opaque.has_translucent_alpha8());
        let mut roundtrip = image.clone();
        roundtrip.premultiply();
        roundtrip.unpremultiply();
        let serial: Vec<u8> = image
            .data
            .chunks_exact(4)
            .flat_map(|px| {
                let a = px[3] as u16;
                let pre = |c: u8| (c as u16 * a + 127) / 255;
                let un = |c: u16| {
                    if a == 0 || a == 255 {
                        c as u8
                    } else {
                        ((c * 255 + a / 2) / a).min(255) as u8
                    }
                };
                [un(pre(px[0])), un(pre(px[1])), un(pre(px[2])), px[3]]
            })
            .collect();
        assert_eq!(roundtrip.data, serial);
    }

    #[test]
    fn raw_expansion_matches_for_every_channel_count() {
        let (w, h) = (700u32, 401u32);
        let pixels = w as usize * h as usize;
        for channels in 1..=4usize {
            let data: Vec<u8> = (0..pixels * channels)
                .map(|i| (i * 7 + i / 5) as u8)
                .collect();
            let image = Image::from_raw_slice(w, h, channels as u8, &data).unwrap();
            for (i, px) in image.data.chunks_exact(4).enumerate() {
                let s = &data[i * channels..(i + 1) * channels];
                let expected = match channels {
                    1 => [s[0], s[0], s[0], 255],
                    2 => [s[0], s[0], s[0], s[1]],
                    3 => [s[0], s[1], s[2], 255],
                    _ => [s[0], s[1], s[2], s[3]],
                };
                assert_eq!(px, expected, "channels={channels} pixel={i}");
            }
        }
    }
}
