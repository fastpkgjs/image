use crate::color::Color;
use crate::Format;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_quality() -> u8 {
    80
}

fn default_alpha_quality() -> u8 {
    100
}

fn default_effort() -> u8 {
    4
}

fn default_compression() -> u8 {
    6
}

fn default_colors() -> u16 {
    256
}

fn default_dither() -> f64 {
    1.0
}

fn default_sharpen_sigma() -> f64 {
    1.0
}

fn default_m1() -> f64 {
    1.0
}

fn default_m2() -> f64 {
    2.0
}

fn default_x1() -> f64 {
    2.0
}

fn default_y2() -> f64 {
    10.0
}

fn default_y3() -> f64 {
    20.0
}

fn default_gamma() -> f64 {
    2.2
}

fn default_lower() -> f64 {
    1.0
}

fn default_upper() -> f64 {
    99.0
}

fn default_threshold() -> u8 {
    128
}

fn default_trim_threshold() -> f64 {
    10.0
}

fn default_median_size() -> u32 {
    3
}

fn default_blur_sigma() -> f64 {
    1.0
}

fn default_clahe_max_slope() -> f64 {
    3.0
}

fn default_density() -> u32 {
    72
}

fn default_limit() -> u64 {
    268_402_689
}

fn default_fail_on() -> String {
    "warning".into()
}

fn default_position() -> String {
    "centre".into()
}

fn default_background() -> String {
    "#000000".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    #[default]
    Cover,
    Contain,
    Fill,
    Inside,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Kernel {
    Nearest,
    Linear,
    Bilinear,
    Cubic,
    Mitchell,
    Lanczos2,
    #[default]
    Lanczos3,
    Mks2013,
    Mks2021,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Blend {
    #[default]
    Over,
    Clear,
    Source,
    In,
    Out,
    Atop,
    Dest,
    DestOver,
    DestIn,
    DestOut,
    DestAtop,
    Xor,
    Add,
    Saturate,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColourDodge,
    ColorDodge,
    ColourBurn,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ExtendWith {
    #[default]
    Background,
    Copy,
    Repeat,
    Mirror,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumberOrArray {
    Number(f64),
    Array(Vec<f64>),
}

impl NumberOrArray {
    pub fn value(&self, channel: usize) -> f64 {
        match self {
            Self::Number(v) => *v,
            Self::Array(values) => values.get(channel).copied().unwrap_or(1.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChannelSelector {
    Index(u8),
    Name(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawInput {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    #[serde(default)]
    pub depth: Option<String>,
    #[serde(default)]
    pub premultiplied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInput {
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_create_channels")]
    pub channels: u8,
    #[serde(default = "default_background")]
    pub background: String,
    #[serde(default)]
    pub noise: Option<NoiseInput>,
}

/// Gaussian noise fill for `create: { noise }` inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoiseInput {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default = "default_noise_mean")]
    pub mean: f64,
    #[serde(default = "default_noise_sigma")]
    pub sigma: f64,
}

fn default_noise_mean() -> f64 {
    128.0
}

fn default_noise_sigma() -> f64 {
    30.0
}

fn default_create_channels() -> u8 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TextInput {
    pub text: String,
    pub font: Option<String>,
    pub fontfile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub align: Option<String>,
    #[serde(default = "default_density")]
    pub dpi: u32,
    #[serde(default)]
    pub rgba: bool,
    pub spacing: Option<f32>,
    pub wrap: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputOptions {
    pub raw: Option<RawInput>,
    pub create: Option<CreateInput>,
    pub text: Option<TextInput>,
    pub page: Option<u32>,
    pub pages: Option<u32>,
    #[serde(default)]
    pub animated: bool,
    #[serde(default = "default_density")]
    pub density: u32,
    #[serde(default = "default_limit")]
    pub limit_input_pixels: u64,
    #[serde(default)]
    pub unlimited: bool,
    #[serde(default)]
    pub auto_orient: bool,
    #[serde(default = "default_fail_on")]
    pub fail_on: String,
    #[serde(default = "default_true")]
    pub sequential_read: bool,
    #[serde(default)]
    pub ignore_icc: bool,
}

impl Default for InputOptions {
    fn default() -> Self {
        Self {
            raw: None,
            create: None,
            text: None,
            page: None,
            pages: None,
            animated: false,
            density: default_density(),
            limit_input_pixels: default_limit(),
            unlimited: false,
            auto_orient: false,
            fail_on: default_fail_on(),
            sequential_read: true,
            ignore_icc: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum GradientKind {
    #[default]
    Linear,
    Radial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum GradientSpread {
    #[default]
    Pad,
    Repeat,
    Reflect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradientStop {
    pub offset: f64,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradientSpec {
    #[serde(rename = "type", default)]
    pub kind: GradientKind,
    #[serde(default)]
    pub angle: f64,
    #[serde(default = "default_gradient_center")]
    pub center: [f64; 2],
    #[serde(default = "default_gradient_radius")]
    pub radius: f64,
    #[serde(default)]
    pub spread: GradientSpread,
    pub stops: Vec<GradientStop>,
}

fn default_gradient_center() -> [f64; 2] {
    [0.5, 0.5]
}

fn default_gradient_radius() -> f64 {
    0.5
}

fn default_amount() -> f64 {
    1.0
}

fn default_tolerance() -> f64 {
    0.12
}

fn default_softness() -> f64 {
    0.08
}

fn default_luma_channel() -> String {
    "luma".into()
}

fn default_posterize_levels() -> u16 {
    8
}

fn default_solarize_threshold() -> f64 {
    128.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Operation {
    Resize {
        width: Option<u32>,
        height: Option<u32>,
        #[serde(default)]
        fit: Fit,
        #[serde(default = "default_position")]
        position: String,
        #[serde(default = "default_background")]
        background: String,
        #[serde(default)]
        kernel: Kernel,
        #[serde(default)]
        without_enlargement: bool,
        #[serde(default)]
        without_reduction: bool,
        #[serde(default = "default_true")]
        fast_shrink_on_load: bool,
        /// Skip the `cover` crop / `contain` padding; a later `Canvas` op applies it.
        #[serde(default)]
        defer_canvas: bool,
    },
    /// The crop / padding half of a `Resize` whose canvas step was deferred.
    Canvas {
        width: Option<u32>,
        height: Option<u32>,
        #[serde(default)]
        fit: Fit,
        #[serde(default = "default_position")]
        position: String,
        #[serde(default = "default_background")]
        background: String,
    },
    Extend {
        width: Option<u32>,
        height: Option<u32>,
        #[serde(default = "default_background")]
        background: String,
        #[serde(default = "default_position")]
        position: String,
        #[serde(default)]
        extend_with: ExtendWith,
        #[serde(default)]
        top: Option<u32>,
        #[serde(default)]
        left: Option<u32>,
        #[serde(default)]
        bottom: Option<u32>,
        #[serde(default)]
        right: Option<u32>,
    },
    Extract {
        left: u32,
        top: u32,
        width: u32,
        height: u32,
    },
    Trim {
        /// Background to trim away; defaults to the top-left pixel.
        #[serde(default)]
        background: Option<String>,
        #[serde(default = "default_trim_threshold")]
        threshold: f64,
        #[serde(default)]
        line_art: bool,
        #[serde(default)]
        margin: u32,
    },
    Rotate {
        angle: f64,
        #[serde(default = "default_background")]
        background: String,
    },
    AutoOrient,
    Flip {
        #[serde(default = "default_true")]
        flip: bool,
    },
    Flop {
        #[serde(default = "default_true")]
        flop: bool,
    },
    Affine {
        matrix: Vec<f64>,
        #[serde(default)]
        idx: f64,
        #[serde(default)]
        idy: f64,
        #[serde(default)]
        odx: f64,
        #[serde(default)]
        ody: f64,
        #[serde(default = "default_background")]
        background: String,
        #[serde(default)]
        interpolator: Option<String>,
    },
    Sharpen {
        #[serde(default = "default_sharpen_sigma")]
        sigma: f64,
        #[serde(default = "default_m1")]
        m1: f64,
        #[serde(default = "default_m2")]
        m2: f64,
        #[serde(default = "default_x1")]
        x1: f64,
        #[serde(default = "default_y2")]
        y2: f64,
        #[serde(default = "default_y3")]
        y3: f64,
        #[serde(default)]
        simple: bool,
    },
    Median {
        #[serde(default = "default_median_size")]
        size: u32,
    },
    Blur {
        #[serde(default = "default_blur_sigma")]
        sigma: f64,
    },
    Dilate {
        #[serde(default = "default_morph_width")]
        width: u32,
    },
    Erode {
        #[serde(default = "default_morph_width")]
        width: u32,
    },
    Flatten {
        #[serde(default = "default_background")]
        background: String,
    },
    Unflatten,
    Gamma {
        #[serde(default = "default_gamma")]
        gamma: f64,
        gamma_out: Option<f64>,
    },
    Negate {
        #[serde(default = "default_true")]
        alpha: bool,
    },
    Normalise {
        #[serde(default = "default_lower")]
        lower: f64,
        #[serde(default = "default_upper")]
        upper: f64,
    },
    Clahe {
        width: u32,
        height: u32,
        #[serde(default = "default_clahe_max_slope")]
        max_slope: f64,
    },
    Convolve {
        width: u32,
        height: u32,
        kernel: Vec<f64>,
        scale: Option<f64>,
        #[serde(default)]
        offset: f64,
    },
    Threshold {
        #[serde(default = "default_threshold")]
        threshold: u8,
        #[serde(default = "default_true")]
        grayscale: bool,
    },
    Boolean {
        operand: String,
        operator: String,
        #[serde(default)]
        raw: Option<RawInput>,
    },
    Linear {
        #[serde(default)]
        a: Option<NumberOrArray>,
        #[serde(default)]
        b: Option<NumberOrArray>,
    },
    Recomb {
        matrix: Vec<Vec<f64>>,
    },
    Modulate {
        #[serde(default = "default_one")]
        brightness: f64,
        #[serde(default = "default_one")]
        saturation: f64,
        #[serde(default)]
        hue: f64,
        #[serde(default)]
        lightness: f64,
    },
    Tint {
        tint: String,
    },
    Greyscale,
    Grayscale,
    RemoveAlpha,
    EnsureAlpha {
        #[serde(default = "default_alpha")]
        alpha: f64,
    },
    ExtractChannel {
        channel: ChannelSelector,
    },
    JoinChannel {
        input: String,
        #[serde(default)]
        raw: Option<RawInput>,
    },
    Bandbool {
        operator: String,
    },
    ReplaceColor {
        from: String,
        to: String,
        #[serde(default = "default_tolerance")]
        tolerance: f64,
        #[serde(default = "default_softness")]
        softness: f64,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    Recolor {
        color: String,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    Duotone {
        shadows: String,
        highlights: String,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    GradientMap {
        gradient: GradientSpec,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    GradientRecolor {
        gradient: GradientSpec,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    GradientFill {
        gradient: GradientSpec,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    ApplyLut {
        data: String,
        #[serde(default = "default_amount")]
        strength: f64,
        #[serde(default)]
        interpolation: Option<String>,
    },
    Curves {
        #[serde(default)]
        master: Vec<[f64; 2]>,
        #[serde(default)]
        red: Vec<[f64; 2]>,
        #[serde(default)]
        green: Vec<[f64; 2]>,
        #[serde(default)]
        blue: Vec<[f64; 2]>,
    },
    Levels {
        #[serde(default)]
        input_black: Option<NumberOrArray>,
        #[serde(default)]
        input_white: Option<NumberOrArray>,
        #[serde(default)]
        gamma: Option<NumberOrArray>,
        #[serde(default)]
        output_black: Option<NumberOrArray>,
        #[serde(default)]
        output_white: Option<NumberOrArray>,
    },
    WhiteBalance {
        #[serde(default)]
        temperature: f64,
        #[serde(default)]
        tint: f64,
    },
    ColourBalance {
        #[serde(default)]
        shadows: [f64; 3],
        #[serde(default)]
        midtones: [f64; 3],
        #[serde(default)]
        highlights: [f64; 3],
    },
    Posterize {
        #[serde(default = "default_posterize_levels")]
        levels: u16,
    },
    Solarize {
        #[serde(default = "default_solarize_threshold")]
        threshold: f64,
    },
    Fill {
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        gradient: Option<GradientSpec>,
        #[serde(default = "default_amount")]
        amount: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    PaletteSwap {
        palette: Vec<String>,
        #[serde(default)]
        dither: f64,
        #[serde(default = "default_true")]
        preserve_opacity: bool,
    },
    AlphaFromLuma {
        #[serde(default = "default_luma_channel")]
        channel: String,
        #[serde(default)]
        invert: bool,
        #[serde(default)]
        multiply: bool,
        #[serde(default = "default_amount")]
        scale: f64,
        #[serde(default)]
        offset: f64,
    },
    Composite {
        input: String,
        #[serde(default)]
        blend: Blend,
        #[serde(default = "default_position")]
        gravity: String,
        #[serde(default)]
        top: Option<i32>,
        #[serde(default)]
        left: Option<i32>,
        #[serde(default)]
        tile: bool,
        #[serde(default)]
        density: Option<u32>,
        #[serde(default)]
        premultiplied: bool,
        /// Generated overlay (create / text / raw) instead of encoded bytes.
        #[serde(default)]
        source: Option<Box<InputOptions>>,
    },
}

fn default_morph_width() -> u32 {
    1
}

fn default_one() -> f64 {
    1.0
}

fn default_alpha() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetadataOptions {
    pub orientation: Option<u32>,
    pub density: Option<u32>,
    pub icc: Option<String>,
    pub exif: Option<serde_json::Value>,
    pub xmp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TileOptions {
    #[serde(default = "default_tile_size")]
    pub size: u32,
    #[serde(default)]
    pub overlap: u32,
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub depth: Option<String>,
    #[serde(default)]
    pub angle: Option<f64>,
    #[serde(default = "default_background")]
    pub background: String,
    #[serde(default)]
    pub skip_blanks: bool,
    #[serde(default)]
    pub id: Option<String>,
}

fn default_tile_size() -> u32 {
    256
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputOptions {
    #[serde(default)]
    pub format: Option<Format>,
    #[serde(default)]
    pub colourspace: Option<String>,
    #[serde(default = "default_quality")]
    pub quality: u8,
    #[serde(default)]
    pub lossless: bool,
    #[serde(default = "default_effort")]
    pub effort: u8,
    #[serde(default)]
    pub progressive: bool,
    #[serde(default)]
    pub chroma_subsampling: Option<String>,
    #[serde(default = "default_compression")]
    pub compression_level: u8,
    #[serde(default)]
    pub palette: bool,
    #[serde(default = "default_colors")]
    pub colors: u16,
    #[serde(default = "default_dither")]
    pub dither: f64,
    #[serde(default)]
    pub loop_count: Option<u32>,
    #[serde(default)]
    pub delay: Option<Vec<u32>>,
    #[serde(default)]
    pub page_height: Option<u32>,
    #[serde(default)]
    pub depth: Option<String>,
    #[serde(default)]
    pub tile: Option<TileOptions>,
    #[serde(default)]
    pub keep_metadata: bool,
    #[serde(default)]
    pub with_metadata: Option<MetadataOptions>,
    #[serde(default)]
    pub keep_exif: bool,
    #[serde(default)]
    pub keep_icc: bool,
    #[serde(default)]
    pub keep_xmp: bool,
    #[serde(default)]
    pub keep_gainmap: bool,
    #[serde(default)]
    pub with_exif: Option<serde_json::Value>,
    #[serde(default)]
    pub with_exif_raw: Option<String>,
    #[serde(default)]
    pub with_exif_merge: Option<serde_json::Value>,
    #[serde(default)]
    pub with_icc: Option<String>,
    #[serde(default)]
    pub with_icc_raw: Option<String>,
    #[serde(default)]
    pub with_xmp: Option<String>,
    #[serde(default)]
    pub with_density: Option<u32>,
    #[serde(default)]
    pub mozjpeg: bool,
    #[serde(default)]
    pub trellis_quantisation: bool,
    #[serde(default)]
    pub overshoot_deringing: bool,
    #[serde(default)]
    pub optimise_scans: bool,
    #[serde(default = "default_true")]
    pub optimise_coding: bool,
    /// Opt-in banded JPEG encode (see `fastpkg-codecs::jpeg_par`); ignored for small images.
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub smart_subsample: bool,
    #[serde(default)]
    pub near_lossless: bool,
    /// WebP `exact`: keep the RGB values under fully transparent pixels (libwebp `exact`).
    #[serde(default)]
    pub exact: bool,
    /// True when the caller passed `effort` explicitly (lossless WebP then uses libwebp).
    #[serde(default)]
    pub effort_set: bool,
    #[serde(default = "default_alpha_quality")]
    pub alpha_quality: u8,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub bitdepth: Option<u8>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub raw_depth: Option<String>,
}

impl Default for OutputOptions {
    fn default() -> Self {
        serde_json::from_str("{}").expect("default output options")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PipelineSpec {
    #[serde(default)]
    pub input: InputOptions,
    #[serde(default)]
    pub pipeline_colourspace: Option<String>,
    #[serde(default)]
    pub operations: Vec<Operation>,
    #[serde(default)]
    pub output: OutputOptions,
}

impl OutputOptions {
    pub fn resolved_format(&self, input: Format) -> Format {
        self.format.unwrap_or(match input {
            Format::Unknown | Format::Raw => Format::Png,
            other => other,
        })
    }

    pub fn background(&self) -> Color {
        Color::BLACK
    }
}
