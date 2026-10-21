use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use fastpkg_core::PipelineSpec;
use fastpkg_stream::{
    execute_bytes, execute_path, metadata_bytes, metadata_path, probe_bytes, probe_path,
    stats_bytes, stats_path, write_file, RunResult,
};
use napi::bindgen_prelude::{Buffer, Either};
use napi::{Error, Result, Status};
use napi_derive::napi;
use serde_json::{json, Value};

static CONCURRENCY: AtomicUsize = AtomicUsize::new(0);
static SIMD_ENABLED: AtomicBool = AtomicBool::new(true);

#[napi(object)]
pub struct JsOutput {
    pub data: Buffer,
    pub info: Value,
    pub metadata: Value,
}

#[napi(object)]
pub struct JsProbe {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub channels: u32,
    pub has_alpha: bool,
    pub pages: u32,
    pub page_height: u32,
    pub orientation: Option<u32>,
    pub size: u32,
}

fn parse_spec(spec: Value) -> Result<PipelineSpec> {
    serde_json::from_value(spec)
        .map_err(|e| Error::new(Status::InvalidArg, format!("invalid pipeline options: {e}")))
}

fn map_error(err: fastpkg_core::FastpkgError) -> Error {
    let status = match err {
        fastpkg_core::FastpkgError::UnsupportedInput
        | fastpkg_core::FastpkgError::UnsupportedOutput(_)
        | fastpkg_core::FastpkgError::UnsupportedOperation(_) => Status::InvalidArg,
        fastpkg_core::FastpkgError::InvalidOptions(_) => Status::InvalidArg,
        fastpkg_core::FastpkgError::InputTooLarge { .. } => Status::InvalidArg,
        _ => Status::GenericFailure,
    };
    Error::new(status, err.to_string())
}

fn run_input(input: Either<Buffer, String>, spec: PipelineSpec) -> Result<RunResult> {
    match input {
        Either::A(buffer) => execute_bytes(&buffer, &spec).map_err(map_error),
        Either::B(path) => execute_path(&path, &spec).map_err(map_error),
    }
}

#[napi]
pub async fn process(input: Either<Buffer, String>, spec: Value) -> Result<JsOutput> {
    let spec = parse_spec(spec)?;
    let result = tokio::task::spawn_blocking(move || {
        let result = run_input(input, spec).map_err(|e| e.to_string())?;
        let info = serde_json::to_value(&result.info).map_err(|e| e.to_string())?;
        let metadata = serde_json::to_value(&result.header).map_err(|e| e.to_string())?;
        Ok::<_, String>((result.data, info, metadata))
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
    .map_err(|e| Error::new(Status::GenericFailure, e))?;
    Ok(JsOutput {
        data: Buffer::from(result.0),
        info: result.1,
        metadata: result.2,
    })
}

#[napi]
pub async fn metadata(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    tokio::task::spawn_blocking(move || match input {
        Either::A(buffer) => metadata_bytes(&buffer, &spec).map_err(map_error),
        Either::B(path) => metadata_path(&path, &spec).map_err(map_error),
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub fn metadata_sync(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    match input {
        Either::A(buffer) => metadata_bytes(&buffer, &spec).map_err(map_error),
        Either::B(path) => metadata_path(&path, &spec).map_err(map_error),
    }
}

#[napi]
pub fn probe_sync(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    let (header, extras) = match input {
        Either::A(buffer) => probe_bytes(&buffer, &spec),
        Either::B(path) => probe_path(&path, &spec),
    }
    .map_err(map_error)?;
    let mut value = serde_json::to_value(&header)
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        if let Some(orientation) = extras.orientation {
            object.insert("orientation".into(), json!(orientation));
        }
        if let Some(exif) = extras.exif {
            object.insert("exif".into(), exif);
        }
    }
    Ok(value)
}

#[napi]
pub async fn stats(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    tokio::task::spawn_blocking(move || {
        let result = match input {
            Either::A(buffer) => stats_bytes(&buffer, &spec),
            Either::B(path) => stats_path(&path, &spec),
        }
        .map_err(map_error)?;
        serde_json::to_value(result).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub async fn probe(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    tokio::task::spawn_blocking(move || {
        let (header, extras) = match input {
            Either::A(buffer) => probe_bytes(&buffer, &spec),
            Either::B(path) => probe_path(&path, &spec),
        }
        .map_err(map_error)?;
        let mut value = serde_json::to_value(&header)
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        if let Some(object) = value.as_object_mut() {
            if let Some(orientation) = extras.orientation {
                object.insert("orientation".into(), json!(orientation));
            }
            if let Some(exif) = extras.exif {
                object.insert("exif".into(), exif);
            }
        }
        Ok(value)
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub async fn to_file(input: Either<Buffer, String>, path: String, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    tokio::task::spawn_blocking(move || {
        let result = run_input(input, spec)?;
        write_file(&path, &result).map_err(map_error)?;
        serde_json::to_value(&result.info)
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub fn set_concurrency(value: Option<u32>) -> u32 {
    let requested = value.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|v| v.get() as u32)
            .unwrap_or(1)
    });
    let requested = requested.max(1);
    let previous = CONCURRENCY.swap(requested as usize, Ordering::Relaxed);
    if previous == 0 {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(requested as usize)
            .build_global();
    }
    rayon::current_num_threads() as u32
}

#[napi]
pub fn get_concurrency() -> u32 {
    rayon::current_num_threads() as u32
}

#[napi]
pub fn set_simd(enabled: Option<bool>) -> bool {
    SIMD_ENABLED.store(enabled.unwrap_or(true), Ordering::Relaxed);
    SIMD_ENABLED.load(Ordering::Relaxed)
}

#[napi]
pub fn simd_enabled() -> bool {
    SIMD_ENABLED.load(Ordering::Relaxed)
}

#[napi]
pub fn versions() -> Value {
    json!({
        "fastpkg": env!("CARGO_PKG_VERSION"),
        "napi": "3",
        "rust": "1.99",
        "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "webp": "lossy + lossless (pure Rust)",
        "avif": "ravif (pure Rust encode)",
        "jxl": "jxl-oxide (pure Rust decode)",
        "png": "png + fdeflate",
        "jpeg": "zune-jpeg + image encoder",
        "gif": "gif + color_quant",
    })
}

#[napi]
pub fn formats() -> Value {
    json!({
        "jpeg": { "input": true, "output": true, "stream": true },
        "png": { "input": true, "output": true, "stream": true },
        "webp": { "input": true, "output": true, "stream": true, "lossy": true },
        "avif": { "input": true, "output": true, "stream": false },
        "jxl": { "input": true, "output": false, "stream": false },
        "gif": { "input": true, "output": true, "stream": true },
        "qoi": { "input": true, "output": true, "stream": true },
        "svg": { "input": true, "output": false, "stream": false },
        "raw": { "input": true, "output": true, "stream": true },
        "tiff": { "input": false, "output": false },
        "jp2": { "input": false, "output": false },
        "heif": { "input": false, "output": false },
        "pdf": { "input": false, "output": false }
    })
}
