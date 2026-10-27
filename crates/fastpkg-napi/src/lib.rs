use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

mod alloc;

use fastpkg_core::PipelineSpec;
use fastpkg_stream::{
    execute_bytes, execute_path, metadata_bytes, metadata_path, probe_bytes, probe_path,
    stats_bytes, stats_path, write_file, RunResult,
};
use napi::bindgen_prelude::{Buffer, Either};
use napi::{Error, Result, Status};
use napi_derive::napi;
use serde_json::{json, Value};

#[napi_derive::module_init]
fn init() {
    alloc::tune();
}

static CONCURRENCY: AtomicUsize = AtomicUsize::new(0);
/// Bumped by every `concurrency(n)`: a pool built under an older value is replaced by the next job.
static POOL_GENERATION: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The rayon pool of this blocking-pool thread: `concurrency` threads per image, as in sharp
    /// (rayon's global pool cannot be resized once it exists). It lives as long as the thread, so
    /// jobs on a warm thread pay nothing for it; idle blocking threads exit and take it along.
    static JOB_POOL: RefCell<Option<(u64, std::sync::Arc<rayon::ThreadPool>)>> = const { RefCell::new(None) };
}

fn configured_threads() -> usize {
    match CONCURRENCY.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(1, |value| value.get()),
        threads => threads,
    }
}

/// Runs `job` with every rayon operation inside it on this thread's pool of `concurrency`
/// threads, rebuilding the pool first if `concurrency()` changed since it was made.
fn in_job_pool<T: Send>(job: impl FnOnce() -> T + Send) -> T {
    let generation = POOL_GENERATION.load(Ordering::Acquire);
    let pool = JOB_POOL.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_ref() {
            Some((built, pool)) if *built == generation => Some(pool.clone()),
            _ => {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(configured_threads())
                    .thread_name(|index| format!("fastpkg-{index}"))
                    .build()
                    .ok()
                    .map(std::sync::Arc::new);
                *slot = pool.clone().map(|pool| (generation, pool));
                pool
            }
        }
    });
    match pool {
        Some(pool) => pool.install(job),
        None => job(),
    }
}
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
        in_job_pool(move || {
            let result = run_input(input, spec).map_err(|e| e.to_string())?;
            let info = serde_json::to_value(&result.info).map_err(|e| e.to_string())?;
            let metadata = serde_json::to_value(&result.header).map_err(|e| e.to_string())?;
            Ok::<_, String>((result.data, info, metadata))
        })
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
    tokio::task::spawn_blocking(move || {
        in_job_pool(move || match input {
            Either::A(buffer) => metadata_bytes(&buffer, &spec).map_err(map_error),
            Either::B(path) => metadata_path(&path, &spec).map_err(map_error),
        })
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
        in_job_pool(move || {
            let result = match input {
                Either::A(buffer) => stats_bytes(&buffer, &spec),
                Either::B(path) => stats_path(&path, &spec),
            }
            .map_err(map_error)?;
            serde_json::to_value(result)
                .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
        })
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub async fn probe(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    tokio::task::spawn_blocking(move || {
        in_job_pool(move || {
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
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub async fn to_file(input: Either<Buffer, String>, path: String, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    tokio::task::spawn_blocking(move || {
        in_job_pool(move || {
            let result = run_input(input, spec)?;
            write_file(&path, &result).map_err(map_error)?;
            serde_json::to_value(&result.info)
                .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
        })
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?
}

#[napi]
pub fn set_concurrency(value: Option<u32>) -> u32 {
    let requested = value.map_or(0, |threads| threads.max(1) as usize);
    CONCURRENCY.store(requested, Ordering::Relaxed);
    // Every job pool is rebuilt at its next job, so this takes effect even after the library
    // has been in use. Jobs already running finish on the pool they started with.
    POOL_GENERATION.fetch_add(1, Ordering::AcqRel);
    configured_threads() as u32
}

/// Threads of the pool a job started now would run on (diagnostics and tests).
#[napi]
pub async fn job_pool_threads() -> Result<u32> {
    tokio::task::spawn_blocking(|| in_job_pool(|| rayon::current_num_threads() as u32))
        .await
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
}

#[napi]
pub fn get_concurrency() -> u32 {
    configured_threads() as u32
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
        "webp": format!("libwebp {} via libwebp-sys (lossy, near-lossless, lossless for graphics or explicit effort) + image-webp (fast lossless)", fastpkg_codecs::libwebp_version()),
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
