#![allow(clippy::chunks_exact_to_as_chunks)]

use serde::{Deserialize, Serialize};

pub mod color;
pub mod exif;
pub mod image;
pub mod options;
pub mod pipeline;
pub mod sink;

pub use color::Color;
pub use image::Image;
pub use options::*;
pub use pipeline::*;
pub use sink::{RowSink, RowSinkSpec, SinkFactory};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Png,
    Jpeg,
    Webp,
    Avif,
    Jxl,
    Qoi,
    Gif,
    Svg,
    Raw,
    Tiff,
    Jp2,
    Heif,
    Pdf,
    #[default]
    Unknown,
}

impl Format {
    pub fn from_magic(bytes: &[u8]) -> Self {
        if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
            return Self::Png;
        }
        if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" {
            return Self::Jpeg;
        }
        // libvips sniffs "GIF8" and ignores the rest of the version.
        if bytes.len() >= 6 && &bytes[..4] == b"GIF8" {
            return Self::Gif;
        }
        if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Self::Webp;
        }
        if bytes.len() >= 4 && &bytes[..4] == b"qoif" {
            return Self::Qoi;
        }
        if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
            let brand = &bytes[8..12];
            if brand == b"avif" || brand == b"avis" {
                return Self::Avif;
            }
            if brand == b"heic" || brand == b"heix" || brand == b"hevc" || brand == b"mif1" {
                return Self::Heif;
            }
        }
        if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0x0a {
            return Self::Jxl;
        }
        if bytes.len() >= 12
            && &bytes[..4] == b"\x00\x00\x00\x0c"
            && &bytes[4..8] == b"JXL "
            && &bytes[8..12] == b"\x0d\x0a\x87\x0a"
        {
            return Self::Jxl;
        }
        if bytes.len() >= 4 && (&bytes[..4] == b"II*\x00" || &bytes[..4] == b"MM\x00*") {
            return Self::Tiff;
        }
        if bytes.len() >= 12 && &bytes[4..12] == b"jP  \r\n\x87\n" {
            return Self::Jp2;
        }
        if bytes.starts_with(b"%PDF") {
            return Self::Pdf;
        }
        let head = &bytes[..bytes.len().min(2048)];
        if let Ok(text) = std::str::from_utf8(head) {
            let trimmed = text.trim_start();
            if trimmed.starts_with("<svg")
                || (trimmed.starts_with("<?xml") && text.contains("<svg"))
            {
                return Self::Svg;
            }
        }
        Self::Unknown
    }

    pub fn from_extension(path: &str) -> Self {
        match path
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "png" | "apng" => Self::Png,
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            "jxl" => Self::Jxl,
            "qoi" => Self::Qoi,
            "gif" => Self::Gif,
            "svg" => Self::Svg,
            "tif" | "tiff" => Self::Tiff,
            "jp2" | "j2k" | "jpx" => Self::Jp2,
            "heic" | "heif" | "hif" => Self::Heif,
            "pdf" => Self::Pdf,
            _ => Self::Unknown,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Jxl => "image/jxl",
            Self::Qoi => "image/qoi",
            Self::Gif => "image/gif",
            Self::Svg => "image/svg+xml",
            Self::Raw => "application/octet-stream",
            Self::Tiff => "image/tiff",
            Self::Jp2 => "image/jp2",
            Self::Heif => "image/heif",
            Self::Pdf => "application/pdf",
            Self::Unknown => "application/octet-stream",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
            Self::Qoi => "qoi",
            Self::Gif => "gif",
            Self::Svg => "svg",
            Self::Raw => "raw",
            Self::Tiff => "tiff",
            Self::Jp2 => "jp2",
            Self::Heif => "heif",
            Self::Pdf => "pdf",
            Self::Unknown => "bin",
        }
    }

    pub fn is_supported_input(self) -> bool {
        matches!(
            self,
            Self::Png
                | Self::Jpeg
                | Self::Webp
                | Self::Avif
                | Self::Jxl
                | Self::Qoi
                | Self::Gif
                | Self::Svg
                | Self::Raw
        )
    }

    pub fn is_supported_output(self) -> bool {
        matches!(
            self,
            Self::Png
                | Self::Jpeg
                | Self::Webp
                | Self::Avif
                | Self::Jxl
                | Self::Qoi
                | Self::Gif
                | Self::Raw
        )
    }

    pub fn supports_alpha(self) -> bool {
        matches!(
            self,
            Self::Png | Self::Webp | Self::Avif | Self::Qoi | Self::Gif | Self::Jxl
        )
    }

    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp | Self::Avif)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HeaderInfo {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub has_alpha: bool,
    pub depth: String,
    pub space: String,
    pub size: usize,
    pub is_progressive: bool,
    pub is_palette: bool,
    pub pages: u32,
    pub page_height: u32,
    pub loop_count: Option<u32>,
    pub delay: Option<Vec<u32>>,
    pub orientation: Option<u32>,
    pub density: Option<u32>,
    pub chroma_subsampling: Option<String>,
    pub has_profile: bool,
    pub has_exif: bool,
    pub has_xmp: bool,
    pub animated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub channels: Vec<ChannelStats>,
    pub is_opaque: bool,
    pub entropy: f64,
    pub sharpness: f64,
    pub dominant: Option<Color>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStats {
    pub min: u32,
    pub max: u32,
    pub sum: u64,
    pub mean: f64,
    pub stdev: f64,
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OutputInfo {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub size: usize,
    pub space: String,
    pub depth: String,
    pub has_alpha: bool,
    pub premultiplied: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FastpkgError {
    #[error("unsupported input format")]
    UnsupportedInput,
    #[error("unsupported output format: {0}")]
    UnsupportedOutput(String),
    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),
    #[error("invalid options: {0}")]
    InvalidOptions(String),
    #[error("input image is too large ({pixels} pixels, limit {limit})")]
    InputTooLarge { pixels: u64, limit: u64 },
    #[error("image decode failed: {0}")]
    Decode(String),
    #[error("image encode failed: {0}")]
    Encode(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("metadata error: {0}")]
    Metadata(String),
    #[error("operation failed: {0}")]
    Operation(String),
    #[error("pipeline was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, FastpkgError>;

pub struct Limits {
    pub max_pixels: u64,
    pub max_memory: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pixels: 268_402_689,
            max_memory: 512 * 1024 * 1024,
        }
    }
}

pub fn validate_dimensions(width: u32, height: u32, limits: &Limits) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(FastpkgError::InvalidOptions(
            "image dimensions must be non-zero".into(),
        ));
    }
    let pixels = width as u64 * height as u64;
    if pixels > limits.max_pixels {
        return Err(FastpkgError::InputTooLarge {
            pixels,
            limit: limits.max_pixels,
        });
    }
    Ok(())
}
