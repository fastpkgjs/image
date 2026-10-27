use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use fastpkg_codecs::{decode, decode_with_sink, encode, output_is_16, probe, ShrinkHint};
use fastpkg_core::options::Operation;
use fastpkg_core::{
    FastpkgError, Format, HeaderInfo, Image, OutputInfo, PipelineSpec, Result, RowSinkSpec, SinkFactory,
    Stats,
};
use fastpkg_meta::{enrich, MetadataExtras};
use fastpkg_ops::{
    apply, apply_output_colourspace, enter_pipeline_colourspace, leave_linear_colourspace,
    stats as compute_stats,
};
use memmap2::Mmap;

pub struct RunResult {
    pub data: Vec<u8>,
    pub format: Format,
    pub info: OutputInfo,
    pub header: HeaderInfo,
    pub metadata: MetadataExtras,
}

pub fn probe_bytes(bytes: &[u8], spec: &PipelineSpec) -> Result<(HeaderInfo, MetadataExtras)> {
    let mut header = probe(bytes, &spec.input)?;
    let extras = fastpkg_meta::probe(bytes);
    enrich(&mut header, &extras);
    Ok((header, extras))
}

pub fn probe_path(path: &str, spec: &PipelineSpec) -> Result<(HeaderInfo, MetadataExtras)> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    probe_bytes(&mmap, spec)
}

pub fn metadata_bytes(bytes: &[u8], spec: &PipelineSpec) -> Result<serde_json::Value> {
    let (mut header, _) = probe_bytes(bytes, spec)?;
    let extras = if header.has_exif || header.has_profile || header.has_xmp {
        fastpkg_meta::extract(bytes, header.format)
    } else {
        fastpkg_meta::probe(bytes)
    };
    enrich(&mut header, &extras);
    let mut value =
        serde_json::to_value(&header).map_err(|e| FastpkgError::Metadata(e.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        if let Some(raw) = extras.exif_raw.as_ref() {
            object.insert(
                "exif".into(),
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(raw)),
            );
        } else if let Some(exif) = extras.exif {
            object.insert("exif".into(), exif);
        }
        if let Some(raw) = extras.icc_raw.as_ref() {
            object.insert(
                "icc".into(),
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(raw)),
            );
        } else if let Some(icc) = extras.icc {
            object.insert("icc".into(), serde_json::Value::String(icc));
        }
        if let Some(raw) = extras.xmp_raw.as_ref() {
            object.insert(
                "xmp".into(),
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(raw)),
            );
        } else if let Some(xmp) = extras.xmp {
            object.insert("xmp".into(), serde_json::Value::String(xmp));
        }
        if let Some(comments) = extras.comments {
            object.insert("comments".into(), serde_json::Value::String(comments));
        }
    }
    Ok(value)
}

pub fn metadata_path(path: &str, spec: &PipelineSpec) -> Result<serde_json::Value> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    metadata_bytes(&mmap, spec)
}

pub fn stats_bytes(bytes: &[u8], spec: &PipelineSpec) -> Result<Stats> {
    let (header, _) = probe_bytes(bytes, spec)?;
    let mut image = decode(bytes, header.format, &spec.input)?;
    image.orientation = header.orientation;
    if spec.input.auto_orient {
        image = apply(image, &[Operation::AutoOrient])?;
    }
    Ok(compute_stats(&image))
}

pub fn stats_path(path: &str, spec: &PipelineSpec) -> Result<Stats> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    stats_bytes(&mmap, spec)
}

pub fn execute_bytes(bytes: &[u8], spec: &PipelineSpec) -> Result<RunResult> {
    fastpkg_core::pipeline::validate_spec(spec)?;
    let started = Instant::now();
    let deadline = spec
        .output
        .timeout
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| started + Duration::from_secs_f64(seconds));

    let (mut header, mut extras) = probe_bytes(bytes, spec)?;
    check_deadline(deadline)?;
    // Shrink-on-load is only safe when the resize is the first operation that resamples, so the
    // decoder-space target can be derived from the header alone. Operations that only move
    // pixels (auto-orient, quarter turns, flips) may come first: they commute with the shrink and
    // `apply_one` carries the pre-shrink size through them. The final size is still computed
    // from the full-resolution dimensions inside the resize op.
    let mut swaps_axes = spec.input.auto_orient && matches!(header.orientation, Some(5..=8));
    // The input-level auto-orient has consumed the tag by the time the operations run.
    let mut orientation = if spec.input.auto_orient {
        None
    } else {
        header.orientation
    };
    let mut leading = 0;
    // sharp does not shrink on load when the pixels are turned or flipped before the resize
    // (`rotateBefore`); the decode hint still looks through such operations here, and the
    // output size is then derived from the full-resolution dimensions like sharp does.
    let mut moved_before_resize = false;
    for op in &spec.operations {
        let Some(swap) = fastpkg_ops::geometry_swaps_axes(op, orientation) else {
            break;
        };
        moved_before_resize |= matches!(
            op,
            Operation::Rotate { .. } | Operation::Flip { .. } | Operation::Flop { .. }
        );
        swaps_axes ^= swap;
        if matches!(op, Operation::AutoOrient) {
            orientation = None;
        }
        leading += 1;
    }
    let (oriented_width, oriented_height) = if swaps_axes {
        (header.height, header.width)
    } else {
        (header.width, header.height)
    };
    // sharp does not shrink on load when the resize happens in another colour space either.
    let (hint, fast_shrink) = match spec
        .operations
        .get(leading)
        .filter(|_| spec.pipeline_colourspace.is_none())
    {
        Some(Operation::Resize {
            width,
            height,
            fit,
            without_enlargement,
            without_reduction,
            fast_shrink_on_load,
            ..
        }) => {
            let (tw, th) = fastpkg_core::pipeline::compute_target(
                oriented_width,
                oriented_height,
                *width,
                *height,
                *fit,
                *without_enlargement,
                *without_reduction,
            );
            let (xf, yf) = fastpkg_core::pipeline::resolve_shrink(
                oriented_width,
                oriented_height,
                *width,
                *height,
                *fit,
                *without_enlargement,
                *without_reduction,
            );
            // sharp never lets an axis shrink to nothing.
            let shrink = xf
                .min(oriented_width as f64)
                .min(yf.min(oriented_height as f64));
            let target = if swaps_axes { (th, tw) } else { (tw, th) };
            (Some(ShrinkHint { target, shrink }), *fast_shrink_on_load)
        }
        _ => (None, false),
    };
    // sharp keeps the pixels as stored when the input profile is to be kept (the profile
    // describes them); otherwise an embedded profile is converted to sRGB while decoding.
    let keeps_input_icc =
        (spec.output.keep_metadata || spec.output.keep_icc || spec.output.with_metadata.is_some())
            && spec.output.with_icc.is_none()
            && spec.output.with_icc_raw.is_none();
    let mut input = spec.input.clone();
    input.ignore_icc |= keeps_input_icc;
    // A big PNG whose first resampling step is this resize is resampled from its rows while it
    // is decoded, when nothing before the resize touches the pixels: no frame is ever held.
    // Samples are narrowed to 8 bits on the way when nothing afterwards needs the wide ones.
    let streamed_op = spec.operations.get(leading).filter(|_| {
        header.format == Format::Png
            && hint.is_some()
            && !swaps_axes
            && matches!(header.orientation, None | Some(1))
            && spec.operations[..leading]
                .iter()
                .all(|op| matches!(op, Operation::AutoOrient))
    });
    let narrow = spec.operations.len() == leading + 1
        && spec.pipeline_colourspace.is_none()
        && !output_is_16(&spec.output);
    let sink_factory = |sink: &RowSinkSpec| {
        streamed_op.and_then(|op| fastpkg_ops::resize_sink(op, sink, narrow))
    };
    let sinks: Option<SinkFactory> = streamed_op.is_some().then_some(&sink_factory);
    let mut image = decode_with_sink(bytes, header.format, &input, hint, fast_shrink, sinks)?;
    check_deadline(deadline)?;
    if image.format == Format::Jpeg && !moved_before_resize {
        if !fastpkg_codecs::jpeg_rule_eighths() {
            // sharp sizes the resize from the shrunk image it decoded (libjpeg rounds the
            // dimensions up), not from the original; forget the original size to do the same.
            image.pre_shrink = None;
        } else if let (Some(hint), Some(_)) = (&hint, image.pre_shrink) {
            // Decoded with a finer n/8 scale than libvips has, but the output size is still
            // sharp's: it comes from the image libvips would have decoded (a power-of-two scale,
            // the partial last block dropped), so size the resize from those dimensions.
            let denominator =
                fastpkg_codecs::jpeg_scale_denominator(hint.shrink, fast_shrink) as u32;
            if denominator > 1 {
                image.pre_shrink = Some((header.width / denominator, header.height / denominator));
            }
        }
    }

    let keep_metadata = spec.output.keep_metadata
        || spec.output.keep_exif
        || spec.output.keep_icc
        || spec.output.keep_xmp
        || spec.output.with_metadata.is_some();
    if keep_metadata {
        extras = fastpkg_meta::extract(bytes, header.format);
        enrich(&mut header, &extras);
        image.exif = extras.exif_raw.clone();
        image.icc = extras.icc_raw.clone();
        image.xmp = extras.xmp_raw.clone();
    }
    image.orientation = header.orientation;
    if spec.input.auto_orient {
        let shrunk = image.pre_shrink.take();
        image = apply(image, &[Operation::AutoOrient])?;
        image.pre_shrink = shrunk.map(|(w, h)| if swaps_axes { (h, w) } else { (w, h) });
    }

    // A 16-bit PNG decoded for a resize comes without its 8-bit twin (see
    // `Image::from_rgba16_deferred`); the operations that do not need it restore it themselves.
    if spec.pipeline_colourspace.is_some() {
        image.materialize_8bit();
    }
    let linear_pipeline = match spec.pipeline_colourspace.as_deref() {
        Some(space) => enter_pipeline_colourspace(&mut image, space).map_err(|error| {
            FastpkgError::InvalidOptions(format!("pipeline colourspace: {error}"))
        })?,
        None => false,
    };
    for operation in &spec.operations {
        image = apply(image, std::slice::from_ref(operation))?;
        check_deadline(deadline)?;
    }
    image.materialize_8bit();
    if linear_pipeline {
        leave_linear_colourspace(&mut image);
    }
    if let Some(space) = spec.output.colourspace.as_deref() {
        apply_output_colourspace(&mut image, space)?;
    }
    check_deadline(deadline)?;
    // The gray flag is an intent; only use a single-channel layout if no later
    // operation introduced colour.
    if image.gray && !image.is_neutral() {
        image.gray = false;
    }
    // libvips emits plain luma for `greyscale()`/`b-w` followed by raw output, dropping alpha.
    let greyed = spec
        .operations
        .iter()
        .any(|op| matches!(op, Operation::Greyscale | Operation::Grayscale))
        || matches!(
            spec.output
                .colourspace
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("b-w" | "bw" | "grey" | "gray")
        );
    if greyed && spec.output.resolved_format(header.format) == Format::Raw && image.is_neutral() {
        image.gray = true;
        image.has_alpha = false;
    }
    let (data, format) = encode(&image, &spec.output, header.format)?;
    check_deadline(deadline)?;

    // Which formats keep a single-channel (gray) layout, and which keep alpha.
    let gray_output = image.gray;
    let output_has_alpha = match format {
        Format::Raw | Format::Png => image.has_alpha,
        other => image.has_alpha && other.supports_alpha(),
    };
    let info = OutputInfo {
        format,
        width: image.width,
        height: image.height,
        channels: u8::from(output_has_alpha) + if gray_output { 1 } else { 3 },
        size: data.len(),
        space: image.space.clone(),
        depth: if matches!(format, Format::Png | Format::Raw) && output_is_16(&spec.output) {
            "ushort".into()
        } else {
            "uchar".into()
        },
        has_alpha: output_has_alpha,
        premultiplied: false,
    };
    header.width = image.width;
    header.height = image.height;
    header.channels = info.channels;
    header.has_alpha = info.has_alpha;
    header.size = data.len();
    header.format = format;
    Ok(RunResult {
        data,
        format,
        info,
        header,
        metadata: extras,
    })
}

pub fn execute_path(path: &str, spec: &PipelineSpec) -> Result<RunResult> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    let ext_format = Format::from_extension(path);
    if ext_format != Format::Unknown && Format::from_magic(&mmap) == Format::Unknown {
        let mut spec = spec.clone();
        if spec.output.format.is_none() {
            spec.output.format = Some(ext_format);
        }
        return execute_bytes(&mmap, &spec);
    }
    execute_bytes(&mmap, spec)
}

pub fn write_file(path: &str, result: &RunResult) -> Result<()> {
    let path = Path::new(path);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Unique per write so concurrent writes to one destination (or to paths that differ only in
    // case on case-insensitive filesystems) never share a temporary file.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!(
        "{}.{}-{unique}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("img"),
        std::process::id()
    ));
    let written = std::fs::write(&temp, &result.data).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    Ok(written?)
}

fn check_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() > deadline) {
        Err(FastpkgError::Operation("pipeline timed out".into()))
    } else {
        Ok(())
    }
}

pub fn exact_orientation(image: &Image) -> Option<u32> {
    image.orientation
}
