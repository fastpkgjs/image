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

use fastpkg_codecs::{decode_footprint, probe};
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

/// Linear-light and other pipeline colourspaces work on float samples.
const COLOURSPACE_FRAMES: u64 = 4;

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
    let float_frames = if spec.pipeline_colourspace.is_some() {
        COLOURSPACE_FRAMES
    } else {
        1
    };

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
            peak = peak.max((source + middle + output) * float_frames);
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
                peak = peak.max((frame + pixels(held) * frame_bpp) * float_frames);
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
                    peak = peak.max((frame + pixels(held) * frame_bpp) * float_frames);
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
                peak = peak.max((frame * 2 + overlay) * float_frames);
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
            _ => peak = peak.max(frame * COPYING_OP_FRAMES * float_frames),
        }
    }

    if !encodes {
        return peak;
    }
    // Encode: the frame, and the encoder's own planes.
    let format = spec.output.resolved_format(header.format);
    let wide_output = spec.output.depth.as_deref() == Some("ushort")
        || spec.output.depth.as_deref() == Some("16");
    let encoded = pixels(held) * (frame_bpp.max(4) + encoder_bytes_per_pixel(format));
    peak = peak.max(encoded * if wide_output { 2 } else { 1 });
    peak
}

/// Estimate for an input held in memory. An error means the header cannot be read (the job then
/// fails on its own, almost for free).
pub fn estimate_bytes(bytes: &[u8], spec: &PipelineSpec, encodes: bool) -> Result<u64> {
    let header = probe(bytes, &spec.input)?;
    Ok(estimate_header(&header, spec, encodes))
}

pub fn estimate_path(path: &str, spec: &PipelineSpec, encodes: bool) -> Result<u64> {
    let file = File::open(path)?;
    // Mapping an empty file fails; a file this small is not an image either.
    let mmap = unsafe { Mmap::map(&file)? };
    estimate_bytes(&mmap, spec, encodes)
}
