use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use fastpkg_codecs::{decode, decode_with_hint, encode, output_is_16, probe};
use fastpkg_core::options::Operation;
use fastpkg_core::{
    FastpkgError, Format, HeaderInfo, Image, OutputInfo, PipelineSpec, Result, Stats,
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
    let resize = spec.operations.iter().find_map(|op| match op {
        Operation::Resize {
            width,
            height,
            fit,
            without_enlargement,
            without_reduction,
            fast_shrink_on_load,
            ..
        } => Some((
            fastpkg_core::pipeline::compute_target(
                header.width,
                header.height,
                *width,
                *height,
                *fit,
                *without_enlargement,
                *without_reduction,
            ),
            *fast_shrink_on_load,
        )),
        _ => None,
    });
    let (hint, fast_shrink) = match resize {
        Some((hint, fast)) => (Some(hint), fast),
        None => (None, false),
    };
    let mut image = decode_with_hint(bytes, header.format, &spec.input, hint, fast_shrink)?;
    check_deadline(deadline)?;

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
        image = apply(image, &[Operation::AutoOrient])?;
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
    if linear_pipeline {
        leave_linear_colourspace(&mut image);
    }
    if let Some(space) = spec.output.colourspace.as_deref() {
        apply_output_colourspace(&mut image, space)?;
    }
    check_deadline(deadline)?;
    let (data, format) = encode(&image, &spec.output, header.format)?;
    check_deadline(deadline)?;

    let gray_output = image.gray && matches!(format, Format::Png | Format::Jpeg);
    let output_has_alpha = if gray_output {
        false
    } else if format == Format::Raw {
        image.has_alpha
    } else {
        image.has_alpha && format.supports_alpha()
    };
    let info = OutputInfo {
        format,
        width: image.width,
        height: image.height,
        channels: if gray_output {
            1
        } else if output_has_alpha {
            4
        } else {
            3
        },
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
    let temp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("img")
    ));
    std::fs::write(&temp, &result.data)?;
    std::fs::rename(&temp, path)?;
    Ok(())
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
