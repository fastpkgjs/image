use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

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
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[napi_derive::module_init]
fn init() {
    alloc::tune();
}

static CONCURRENCY: AtomicUsize = AtomicUsize::new(0);
/// Bumped by every `concurrency(n)`: a pool built under an older value is dropped, never reused.
static POOL_GENERATION: AtomicU64 = AtomicU64::new(1);
/// Jobs that hold an admission permit right now (running, not queued).
static ACTIVE_JOBS: AtomicUsize = AtomicUsize::new(0);
static ADMISSION: OnceLock<Arc<Semaphore>> = OnceLock::new();
static POOLS: Mutex<PoolList> = Mutex::new(PoolList::new());

/// How long a pool may sit unused before its threads are released (tokio's blocking threads
/// keep alive for the same ten seconds). `FASTPKG_POOL_IDLE_MS` overrides it, for tests.
const POOL_IDLE_DEFAULT: Duration = Duration::from_secs(10);

fn configured_threads() -> usize {
    match CONCURRENCY.load(Ordering::Relaxed) {
        0 => cores(),
        threads => threads,
    }
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |value| value.get())
}

/// Most jobs that run at once: the cores, but at least four (sharp's libuv pool has four threads).
/// The excess queues in the async entry points without occupying a blocking-pool thread, so a
/// burst of 200 uploads holds the memory of `max_jobs()` decodes, not of 200.
fn max_jobs() -> usize {
    cores().max(4)
}

/// Rayon threads that all running jobs together may use: twice the cores (the second half hides
/// the serial phases of the jobs that are not on a core), and never less than `concurrency()`
/// so that a lone job always gets the full count.
fn thread_budget() -> usize {
    (2 * cores()).max(configured_threads())
}

/// Threads for a job that starts while `active` jobs (itself included) run: `concurrency()` when
/// it runs alone, the budget split between the running jobs when it does not.
fn pool_threads(active: usize) -> usize {
    (thread_budget() / active.max(1)).clamp(1, configured_threads())
}

/// A running job's claim on the limiter: the permit goes back when this is dropped, on error and
/// panic included.
struct Admission {
    _permit: OwnedSemaphorePermit,
}

impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE_JOBS.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn admit() -> Admission {
    let semaphore = ADMISSION.get_or_init(|| Arc::new(Semaphore::new(max_jobs())));
    // The semaphore is never closed, so acquiring only fails if that stops being true.
    let permit = Arc::clone(semaphore)
        .acquire_owned()
        .await
        .expect("job limiter closed");
    ACTIVE_JOBS.fetch_add(1, Ordering::AcqRel);
    Admission { _permit: permit }
}

struct IdlePool {
    generation: u64,
    threads: usize,
    since: Instant,
    pool: rayon::ThreadPool,
}

/// Rayon pools that no job is using. A job takes one of the size it wants or builds one, and
/// hands it back when done, so pools exist for the running jobs only (plus a few idle ones) and
/// not for every blocking-pool thread that ever ran a job. The oldest idle pool is the first one
/// to go: when the list holds more than a budget's worth of threads, and after the idle timeout.
struct PoolList {
    idle: Vec<IdlePool>,
    reaper_running: bool,
}

impl PoolList {
    const fn new() -> Self {
        PoolList {
            idle: Vec::new(),
            reaper_running: false,
        }
    }
}

fn lock_pools() -> MutexGuard<'static, PoolList> {
    // A poisoned list is still a valid list: nothing panics while it is being edited.
    POOLS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn pool_idle_timeout() -> Duration {
    static TIMEOUT: OnceLock<Duration> = OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        std::env::var("FASTPKG_POOL_IDLE_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .map_or(POOL_IDLE_DEFAULT, Duration::from_millis)
    })
}

/// A pool on its way back to the list when the job that took it ends, however it ends.
struct LeasedPool {
    generation: u64,
    threads: usize,
    pool: Option<rayon::ThreadPool>,
}

impl Drop for LeasedPool {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else { return };
        if self.generation != POOL_GENERATION.load(Ordering::Acquire) {
            return;
        }
        let mut dropped = Vec::new();
        let mut list = lock_pools();
        list.idle.push(IdlePool {
            generation: self.generation,
            threads: self.threads,
            since: Instant::now(),
            pool,
        });
        let budget = thread_budget();
        let mut idle_threads: usize = list.idle.iter().map(|idle| idle.threads).sum();
        while idle_threads > budget && !list.idle.is_empty() {
            let oldest = list.idle.remove(0);
            idle_threads -= oldest.threads;
            dropped.push(oldest);
        }
        if !list.reaper_running {
            list.reaper_running = std::thread::Builder::new()
                .name("fastpkg-pool-reaper".into())
                .spawn(reap_idle_pools)
                .is_ok();
        }
        drop(list);
        drop(dropped);
    }
}

/// Drops idle pools that outlived the timeout, and exits once none is left (the next pool that
/// goes idle starts a new one), so a quiet process keeps neither pools nor this thread.
fn reap_idle_pools() {
    let timeout = pool_idle_timeout();
    loop {
        std::thread::sleep((timeout / 2).max(Duration::from_millis(5)));
        let expired: Vec<IdlePool>;
        {
            let mut list = lock_pools();
            let now = Instant::now();
            let (stale, fresh): (Vec<_>, Vec<_>) = std::mem::take(&mut list.idle)
                .into_iter()
                .partition(|idle| now.duration_since(idle.since) >= timeout);
            list.idle = fresh;
            expired = stale;
            if list.idle.is_empty() {
                list.reaper_running = false;
                break;
            }
        }
        drop(expired);
    }
}

fn lease_pool(threads: usize) -> LeasedPool {
    let generation = POOL_GENERATION.load(Ordering::Acquire);
    let mut stale = Vec::new();
    let recycled = {
        let mut list = lock_pools();
        let (current, old): (Vec<_>, Vec<_>) = std::mem::take(&mut list.idle)
            .into_iter()
            .partition(|idle| idle.generation == generation);
        list.idle = current;
        stale.extend(old);
        // Newest first: the pools that keep being used stay warm, the others age out.
        list.idle
            .iter()
            .rposition(|idle| idle.threads == threads)
            .map(|index| list.idle.remove(index).pool)
    };
    drop(stale);
    let pool = recycled.or_else(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("fastpkg-{index}"))
            .build()
            .ok()
    });
    LeasedPool {
        generation,
        threads,
        pool,
    }
}

/// Runs `job` with every rayon operation inside it on a pool of its own (`concurrency` threads
/// per image, as in sharp; rayon's global pool cannot be resized once it exists), shrunk to its
/// share of the thread budget while other jobs run.
fn in_job_pool<T: Send>(job: impl FnOnce() -> T + Send) -> T {
    let lease = lease_pool(pool_threads(ACTIVE_JOBS.load(Ordering::Acquire)));
    match lease.pool.as_ref() {
        Some(pool) => pool.install(job),
        None => job(),
    }
}

/// Admits the job (at most `max_jobs()` run at once, the rest wait here, not on a thread), runs
/// it on the blocking pool inside its rayon pool, and gives the permit back when it ends.
async fn run_job<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let admission = admit().await;
    tokio::task::spawn_blocking(move || {
        let _admission = admission;
        in_job_pool(job)
    })
    .await
    .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
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
    let result = run_job(move || {
        let result = run_input(input, spec).map_err(|e| e.to_string())?;
        let info = serde_json::to_value(&result.info).map_err(|e| e.to_string())?;
        let metadata = serde_json::to_value(&result.header).map_err(|e| e.to_string())?;
        Ok::<_, String>((result.data, info, metadata))
    })
    .await?
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
    run_job(move || match input {
        Either::A(buffer) => metadata_bytes(&buffer, &spec).map_err(map_error),
        Either::B(path) => metadata_path(&path, &spec).map_err(map_error),
    })
    .await?
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
    run_job(move || {
        let result = match input {
            Either::A(buffer) => stats_bytes(&buffer, &spec),
            Either::B(path) => stats_path(&path, &spec),
        }
        .map_err(map_error)?;
        serde_json::to_value(result).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
    })
    .await?
}

#[napi]
pub async fn probe(input: Either<Buffer, String>, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    run_job(move || {
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
    .await?
}

#[napi]
pub async fn to_file(input: Either<Buffer, String>, path: String, spec: Value) -> Result<Value> {
    let spec = parse_spec(spec)?;
    run_job(move || {
        let result = run_input(input, spec)?;
        write_file(&path, &result).map_err(map_error)?;
        serde_json::to_value(&result.info)
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))
    })
    .await?
}

#[napi]
pub fn set_concurrency(value: Option<u32>) -> u32 {
    let requested = value.map_or(0, |threads| threads.max(1) as usize);
    CONCURRENCY.store(requested, Ordering::Relaxed);
    // Pools built under the old value are never reused, so this takes effect even after the
    // library has been in use. Jobs already running finish on the pool they started with.
    POOL_GENERATION.fetch_add(1, Ordering::AcqRel);
    drop(std::mem::take(&mut lock_pools().idle));
    configured_threads() as u32
}

/// Threads of the pool a job started now would run on (diagnostics and tests).
#[napi]
pub async fn job_pool_threads() -> Result<u32> {
    run_job(|| rayon::current_num_threads() as u32).await
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
