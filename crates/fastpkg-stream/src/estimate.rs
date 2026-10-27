//! A cheap estimate of the memory one job needs at its peak, from the header and the pipeline
//! alone (no pixel is read). The binding layer admits jobs against a memory budget with it, so
//! that a burst of large images queues instead of holding every image at once.
//!
//! The model follows what a job does: decode (the decoded frame, after any decode-time shrink
//! the first resize allows, with the decoder's scratch), the operations (a resize holds its input,
//! its output and a half-resampled intermediate; most other operations work in place, a few copy
//! the frame), and encode (the frame plus the encoder's working planes). The job's peak is the
//! biggest of those phases. The per-pixel factors are calibrated against measured peak RSS
//! (`bench/admission-calibrate.mjs` prints estimate and measurement side by side).

use std::fs::File;

use fastpkg_codecs::{decode_footprint, probe_layout};
use fastpkg_core::options::Operation;
use fastpkg_core::pipeline::compute_target;
use fastpkg_core::{Fit, Format, HeaderInfo, PipelineSpec, Result};
use memmap2::Mmap;

use crate::{decode_plan, DecodePlan};

/// Decoders hold a little more than the pixels they return (row buffers, a second copy while
/// the result is handed on): percent of the frame.
const DECODE_PERCENT: u64 = 125;

/// A pipeline step that is not in place copies or widens the frame: how many frames it holds.
const COPYING_OP_FRAMES: u64 = 3;

/// A pipeline colourspace (linear light and the like) converts the frames it works on: percent.
const COLOURSPACE_PERCENT: u64 = 150;

/// Bytes of encoder working memory per pixel, on top of the frame that is being encoded.
fn encoder_bytes_per_pixel(format: Format) -> u64 {
    match format {
        // RGB copy and planar YCbCr.
        Format::Jpeg => 6,
        // Filtered rows, and the deflate state, for the whole image when adaptive.
        Format::Png => 8,
        Format::Webp => 6,
        // The AV1 encoder keeps YUV planes, the reconstruction and its analysis buffers.
        Format::Avif => 30,
        Format::Gif => 6,
        Format::Qoi | Format::Raw => 1,
        _ => 6,
    }
}

/// Output size (width, height) of the resize step, as the resize computes it, and the size of
/// the frame after the `cover` crop / `contain` padding that follows it.
fn resize_sizes(
    source: (u32, u32),
    operation: &Operation,
) -> Option<((u32, u32), (u32, u32))> {
    let Operation::Resize {
        width,
        height,
        fit,
        without_enlargement,
        without_reduction,
        defer_canvas,
        ..
    } = operation
    else {
        return None;
    };
    if width.is_none() && height.is_none() {
        return Some((source, source));
    }
    let resized = compute_target(
        source.0,
        source.1,
        *width,
        *height,
        *fit,
        *without_enlargement,
        *without_reduction,
    );
    if *defer_canvas {
        return Some((resized, resized));
    }
    let canvas = match fit {
        Fit::Cover => (
            width.map_or(resized.0, |w| resized.0.min(w)),
            height.map_or(resized.1, |h| resized.1.min(h)),
        ),
        Fit::Contain => (
            width.map_or(resized.0, |w| resized.0.max(w)),
            height.map_or(resized.1, |h| resized.1.max(h)),
        ),
        _ => resized,
    };
    Some((resized, canvas))
}

/// Estimated peak working set, in bytes, of running `spec` over an image with this header;
/// `encodes` is false for jobs that only decode (`stats`).
pub fn estimate_header(header: &HeaderInfo, spec: &PipelineSpec, encodes: bool) -> u64 {
    let DecodePlan {
        hint,
        fast_shrink,
        swaps_axes,
        ..
    } = decode_plan(header, spec);
    let footprint = decode_footprint(header, hint, fast_shrink);

    // Animated inputs decode every frame (a stack of pages) when asked to.
    let frames = if spec.input.animated && matches!(header.format, Format::Gif | Format::Webp) {
        let pages = u64::from(header.pages.max(1));
        spec.input
            .pages
            .filter(|&n| n > 0)
            .map_or(pages, |n| pages.min(u64::from(n)))
    } else {
        1
    };
    // Bytes per pixel of the frame between the operations: 8-bit RGBA, or 16-bit samples.
    let frame_bpp: u64 = if header.depth == "ushort" { 8 } else { 4 };
    let pixels = |size: (u32, u32)| u64::from(size.0) * u64::from(size.1) * frames;
    let colourspace_percent = if spec.pipeline_colourspace.is_some() {
        COLOURSPACE_PERCENT
    } else {
        100
    };
    let scaled = |bytes: u64| bytes * colourspace_percent / 100;

    let mut peak = pixels(footprint.size) * footprint.bytes_per_pixel * DECODE_PERCENT / 100
        + footprint.extra;

    // `logical` is the size the operations compute their targets from (the full-resolution size,
    // as the resize does after a shrink on load), `held` the size of the pixels in memory.
    let mut logical = if swaps_axes {
        (header.height, header.width)
    } else {
        (header.width, header.height)
    };
    let mut held = footprint.size;
    // `held` is in decoder space: the quarter turn of the auto-orient comes after the decode.
    if swaps_axes {
        held = (held.1, held.0);
    }
    for operation in &spec.operations {
        if let Some((resized, canvas)) = resize_sizes(logical, operation) {
            let source = pixels(held) * frame_bpp;
            // The first pass makes a frame as wide as the output and as tall as the input.
            let middle = pixels((resized.0, held.1)).min(pixels((held.0, resized.1))) * frame_bpp;
            let output = pixels(resized) * frame_bpp;
            peak = peak.max(scaled(source + middle + output));
            logical = canvas;
            held = canvas;
            continue;
        }
        let frame = pixels(held) * frame_bpp;
        match operation {
            Operation::Extract { width, height, .. } => {
                held = (*width, *height);
                logical = held;
            }
            Operation::Extend {
                width,
                height,
                top,
                left,
                bottom,
                right,
                ..
            } => {
                let grown = |size: u32, given: Option<u32>, before: Option<u32>, after: Option<u32>| {
                    given.unwrap_or(size + before.unwrap_or(0) + after.unwrap_or(0))
                };
                held = (
                    grown(held.0, *width, *left, *right),
                    grown(held.1, *height, *top, *bottom),
                );
                logical = held;
                peak = peak.max(scaled(frame + pixels(held) * frame_bpp));
            }
            Operation::Rotate { angle, .. } => {
                let turns = angle / 90.0;
                if angle.is_finite() && turns.fract() == 0.0 {
                    if (turns as i64).rem_euclid(2) == 1 {
                        held = (held.1, held.0);
                        logical = (logical.1, logical.0);
                    }
                } else {
                    // The bounding box of the turned image.
                    let (sin, cos) = angle.to_radians().sin_cos();
                    let (w, h) = (f64::from(held.0), f64::from(held.1));
                    let rotated = (
                        (w * cos.abs() + h * sin.abs()).ceil() as u32,
                        (w * sin.abs() + h * cos.abs()).ceil() as u32,
                    );
                    held = rotated;
                    logical = rotated;
                    peak = peak.max(scaled(frame + pixels(held) * frame_bpp));
                }
            }
            Operation::Composite { input, source, .. } => {
                // The overlay is an encoded file (base64 here) that decodes to a multiple of its
                // size, or a generated image that is not bigger than the base in practice.
                let overlay = if source.is_some() {
                    frame
                } else {
                    (input.len() as u64 * 6).min(frame * 4)
                };
                peak = peak.max(scaled(frame * 2 + overlay));
            }
            Operation::AutoOrient
            | Operation::Flip { .. }
            | Operation::Flop { .. }
            | Operation::Canvas { .. }
            | Operation::Greyscale
            | Operation::Grayscale
            | Operation::Negate { .. }
            | Operation::Gamma { .. }
            | Operation::Linear { .. }
            | Operation::Modulate { .. }
            | Operation::Tint { .. }
            | Operation::Threshold { .. }
            | Operation::Posterize { .. }
            | Operation::Solarize { .. }
            | Operation::RemoveAlpha
            | Operation::EnsureAlpha { .. } => {}
            _ => peak = peak.max(scaled(frame * COPYING_OP_FRAMES)),
        }
    }

    if !encodes {
        return peak;
    }
    // Encode: the frame, and the encoder's own planes. A 16-bit output keeps the wide samples.
    let format = spec.output.resolved_format(header.format);
    let wide_output = match spec.output.depth.as_deref() {
        Some(depth) => depth == "ushort" || depth == "16",
        None => frame_bpp == 8 && format == Format::Png,
    };
    let encoded = if wide_output {
        pixels(held) * (8 + encoder_bytes_per_pixel(format) + 4)
    } else {
        pixels(held) * (4 + encoder_bytes_per_pixel(format))
    };
    peak = peak.max(encoded);
    peak
}

/// Estimate for an input held in memory. An error means the header cannot be read (the job then
/// fails on its own, almost for free).
pub fn estimate_bytes(bytes: &[u8], spec: &PipelineSpec, encodes: bool) -> Result<u64> {
    // Only a pipeline that turns the pixels by the EXIF orientation needs to read it.
    let orientation = spec.input.auto_orient
        || spec
            .operations
            .iter()
            .any(|operation| matches!(operation, Operation::AutoOrient));
    let header = probe_layout(bytes, &spec.input, orientation)?;
    Ok(estimate_header(&header, spec, encodes))
}

pub fn estimate_path(path: &str, spec: &PipelineSpec, encodes: bool) -> Result<u64> {
    let file = File::open(path)?;
    // Mapping an empty file fails; a file this small is not an image either.
    let mmap = unsafe { Mmap::map(&file)? };
    estimate_bytes(&mmap, spec, encodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpkg_core::options::Kernel;

    const MB: u64 = 1_000_000;

    fn header(format: Format, width: u32, height: u32) -> HeaderInfo {
        HeaderInfo {
            format,
            width,
            height,
            channels: 3,
            depth: "uchar".into(),
            pages: 1,
            page_height: height,
            ..Default::default()
        }
    }

    fn resize(width: u32) -> Operation {
        Operation::Resize {
            width: Some(width),
            height: None,
            fit: Fit::Cover,
            position: "centre".into(),
            background: "#000000".into(),
            kernel: Kernel::Lanczos3,
            without_enlargement: false,
            without_reduction: false,
            fast_shrink_on_load: true,
            defer_canvas: false,
        }
    }

    fn spec(operations: Vec<Operation>, format: Format) -> PipelineSpec {
        let mut spec = PipelineSpec::default();
        spec.operations = operations;
        spec.output.format = Some(format);
        spec
    }

    #[test]
    fn a_png_below_the_shrink_threshold_is_held_whole() {
        // 31 MP of RGBA is 124 MB, under the 128 MB where the decode-time shrink starts: about
        // what was measured (150 MB).
        let png = header(Format::Png, 6200, 5000);
        let bytes = estimate_header(&png, &spec(vec![resize(1200)], Format::Jpeg), true);
        assert!((130 * MB..180 * MB).contains(&bytes), "{bytes}");
    }

    #[test]
    fn a_large_downscale_of_a_big_png_shrinks_on_load() {
        let png = header(Format::Png, 6325, 6325);
        let shrunk = estimate_header(&png, &spec(vec![resize(1200)], Format::Jpeg), true);
        let kept = estimate_header(&png, &spec(vec![resize(3000)], Format::Jpeg), true);
        let plain = estimate_header(&png, &spec(vec![], Format::Jpeg), true);
        assert!(shrunk < 140 * MB, "{shrunk}");
        assert!(kept > 200 * MB, "{kept}");
        assert!(plain > kept, "{plain}");
    }

    #[test]
    fn jpegs_decode_scaled() {
        let jpeg = header(Format::Jpeg, 4000, 3000);
        let thumb = estimate_header(&jpeg, &spec(vec![resize(200)], Format::Jpeg), true);
        let medium = estimate_header(&jpeg, &spec(vec![resize(1200)], Format::Jpeg), true);
        let whole = estimate_header(&jpeg, &spec(vec![], Format::Jpeg), true);
        assert!(thumb < 8 * MB, "{thumb}");
        assert!(medium < 40 * MB, "{medium}");
        assert!((100 * MB..150 * MB).contains(&whole), "{whole}");
        // Progressive files keep their coefficients for the whole source.
        let mut progressive = jpeg.clone();
        progressive.is_progressive = true;
        let coefficients = estimate_header(&progressive, &spec(vec![resize(1200)], Format::Jpeg), true);
        assert!(coefficients > medium + 30 * MB, "{coefficients}");
    }

    #[test]
    fn stats_only_decodes() {
        let jpeg = header(Format::Jpeg, 4000, 3000);
        let plain = spec(vec![], Format::Jpeg);
        assert!(estimate_header(&jpeg, &plain, false) < estimate_header(&jpeg, &plain, true));
    }

    #[test]
    fn sixteen_bit_pngs_are_twice_as_wide() {
        let mut wide = header(Format::Png, 3000, 3000);
        wide.depth = "ushort".into();
        let narrow = header(Format::Png, 3000, 3000);
        let plain = spec(vec![], Format::Png);
        assert!(estimate_header(&wide, &plain, true) > estimate_header(&narrow, &plain, true));
    }

    #[test]
    fn heavy_formats_and_operations_cost_more() {
        let jpeg = header(Format::Jpeg, 4000, 3000);
        let jpeg_out = estimate_header(&jpeg, &spec(vec![], Format::Jpeg), true);
        let avif_out = estimate_header(&jpeg, &spec(vec![], Format::Avif), true);
        assert!(avif_out > jpeg_out);
        let blur: Operation = serde_json::from_str(r#"{"op":"blur","sigma":5.0}"#).unwrap();
        let blurred = estimate_header(&jpeg, &spec(vec![blur], Format::Jpeg), true);
        // A blur of the whole frame holds copies next to it; it also exceeds the plain encode.
        assert!(blurred > jpeg_out, "{blurred} {jpeg_out}");
    }
}
