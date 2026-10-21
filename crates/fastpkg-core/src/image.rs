use crate::color::Color;
use crate::{FastpkgError, Format, Result};

#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub wide: Option<Vec<u16>>,
    pub has_alpha: bool,
    pub gray: bool,
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
        let expected = width as usize * height as usize * 4;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "RGBA16 buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let rgba = wide_to_rgba8(&data);
        let mut image = Self::new(width, height, rgba)?;
        image.has_alpha = true;
        image.depth = "ushort".into();
        image.wide = Some(data);
        Ok(image)
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
        let expected = width as usize * height as usize * 3;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "RGB buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let mut rgba = vec![255u8; width as usize * height as usize * 4];
        for (src, dst) in data.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
            dst[0] = src[0];
            dst[1] = src[1];
            dst[2] = src[2];
            dst[3] = 255;
        }
        let mut img = Self::new(width, height, rgba)?;
        img.has_alpha = false;
        Ok(img)
    }

    pub fn from_luma(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        let expected = width as usize * height as usize;
        if data.len() != expected {
            return Err(FastpkgError::InvalidOptions(format!(
                "luma buffer length {} does not match {}x{}",
                data.len(),
                width,
                height
            )));
        }
        let mut rgba = vec![255u8; expected * 4];
        for (src, dst) in data.iter().zip(rgba.chunks_exact_mut(4)) {
            dst[0] = *src;
            dst[1] = *src;
            dst[2] = *src;
            dst[3] = 255;
        }
        let mut img = Self::new(width, height, rgba)?;
        img.has_alpha = false;
        img.gray = true;
        Ok(img)
    }

    pub fn from_raw(width: u32, height: u32, channels: u8, data: Vec<u8>) -> Result<Self> {
        match channels {
            1 => Self::from_luma(width, height, data),
            3 => Self::from_rgb(width, height, data),
            4 => Self::from_rgba(width, height, data),
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

    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixel_count() * 3);
        for px in self.data.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        out
    }

    pub fn to_rgba(&self) -> &[u8] {
        &self.data
    }

    pub fn to_rgba16(&self) -> Vec<u16> {
        self.wide
            .clone()
            .unwrap_or_else(|| self.data.iter().map(|value| *value as u16 * 257).collect())
    }

    pub fn to_rgb16(&self) -> Vec<u16> {
        let rgba = self.to_rgba16();
        let mut out = Vec::with_capacity(self.pixel_count() * 3);
        for px in rgba.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        out
    }

    pub fn to_luma16(&self) -> Vec<u16> {
        let rgba = self.to_rgba16();
        rgba.chunks_exact(4).map(|px| px[0]).collect()
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
            for px in self.data.chunks_exact_mut(4) {
                px[3] = alpha;
            }
            if let Some(wide) = self.wide.as_mut() {
                let alpha = alpha as u16 * 257;
                for px in wide.chunks_exact_mut(4) {
                    px[3] = alpha;
                }
            }
            self.has_alpha = true;
        }
    }

    pub fn remove_alpha(&mut self) {
        for px in self.data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        if let Some(wide) = self.wide.as_mut() {
            for px in wide.chunks_exact_mut(4) {
                px[3] = 65_535;
            }
        }
        self.has_alpha = false;
    }

    pub fn flatten(&mut self, background: Color) {
        if !self.has_alpha {
            return;
        }
        for px in self.data.chunks_exact_mut(4) {
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
        self.has_alpha = false;
    }

    pub fn premultiply(&mut self) {
        for px in self.data.chunks_exact_mut(4) {
            let a = px[3] as u16;
            px[0] = ((px[0] as u16 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u16 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u16 * a + 127) / 255) as u8;
        }
    }

    pub fn unpremultiply(&mut self) {
        if let Some(wide) = self.wide.as_mut() {
            for px in wide.chunks_exact_mut(4) {
                let a = px[3] as u32;
                if a == 0 || a == 65_535 {
                    continue;
                }
                px[0] = ((px[0] as u32 * 65_535 + a / 2) / a).min(65_535) as u16;
                px[1] = ((px[1] as u32 * 65_535 + a / 2) / a).min(65_535) as u16;
                px[2] = ((px[2] as u32 * 65_535 + a / 2) / a).min(65_535) as u16;
            }
            self.data = wide_to_rgba8(wide);
            return;
        }
        for px in self.data.chunks_exact_mut(4) {
            let a = px[3] as u16;
            if a == 0 || a == 255 {
                continue;
            }
            px[0] = ((px[0] as u16 * 255 + a / 2) / a).min(255) as u8;
            px[1] = ((px[1] as u16 * 255 + a / 2) / a).min(255) as u8;
            px[2] = ((px[2] as u16 * 255 + a / 2) / a).min(255) as u8;
        }
    }

    pub fn has_transparency(&self) -> bool {
        if let Some(wide) = self.wide.as_ref() {
            return wide.chunks_exact(4).any(|px| px[3] != 65_535);
        }
        self.data.chunks_exact(4).any(|px| px[3] != 255)
    }
}

fn wide_to_rgba8(wide: &[u16]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(wide.len());
    for value in wide {
        rgba.push(((*value as u32 * 255 + 32_767) / 65_535) as u8);
    }
    rgba
}
