//! Memory-aware admission of jobs.
//!
//! The count limit (`max_jobs`) bounds how many jobs run at once, but a job's memory depends on
//! its image: a 31 MP PNG holds 150-500 MB while it decodes, resizes and encodes, a thumbnail
//! holds almost nothing. Twelve admitted large jobs are gigabytes. Every job therefore also
//! claims its estimated peak working set (see `fastpkg_stream::estimate_header`) from a memory
//! budget before it starts, and gives it back when it ends, however it ends.
//!
//! * The budget is half of the container's memory limit (the cgroup limit, v2 or v1, of this
//!   process or the nearest ancestor that sets one), or a quarter of the machine's RAM when
//!   nothing limits the process, never less than 512 MB. `FASTPKG_MEMORY_BUDGET_MB` replaces it
//!   (`0`, `off` or `unlimited` switch the memory limit off and leave the count limit alone).
//! * Claims are counted in MiB on a tokio semaphore, which queues waiters first come, first
//!   served: a large job at the head of the queue is not overtaken by later ones, so small jobs
//!   cannot starve it, and it is not held up by smaller ones that run in the meantime either.
//! * A claim is never bigger than the whole budget, so a job that alone needs more than the
//!   budget runs when nothing else holds memory, and cannot wait forever.
//! * Jobs estimated below [`FREE_BELOW`] claim nothing: thumbnails pay no wait, no accounting
//!   and no atomics (the count limit bounds them: twelve of them are at most about 100 MB).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MIB: u64 = 1 << 20;

/// Estimates below this claim nothing.
const FREE_BELOW: u64 = 8 * MIB;

/// Least a derived budget may be. A smaller container still gets a budget that runs one ordinary
/// large job at a time next to the few small ones.
const MIN_BUDGET_MB: u64 = 512;

/// Used where the memory limit cannot be read (no `/proc`, no cgroup): macOS, Windows.
const FALLBACK_BUDGET_MB: u64 = 4096;

/// Where the budget comes from; the label is reported by `admission_info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetSource {
    Env,
    Cgroup,
    Ram,
    Fallback,
    Off,
}

impl BudgetSource {
    fn label(self) -> &'static str {
        match self {
            BudgetSource::Env => "FASTPKG_MEMORY_BUDGET_MB",
            BudgetSource::Cgroup => "cgroup limit",
            BudgetSource::Ram => "RAM",
            BudgetSource::Fallback => "default",
            BudgetSource::Off => "off",
        }
    }
}

/// `FASTPKG_MEMORY_BUDGET_MB`: `Some(Some(mb))` for a number of MB, `Some(None)` for "no memory
/// limit", `None` for anything else (unset, empty, not a number), which falls back to the derived
/// budget.
pub fn parse_budget_override(value: Option<&str>) -> Option<Option<u64>> {
    let value = value?.trim();
    match value.to_ascii_lowercase().as_str() {
        "0" | "off" | "unlimited" | "none" | "false" => Some(None),
        other => other.parse::<u64>().ok().map(Some),
    }
}

/// The derived budget in MB from the memory limit the process lives under (`limit`, bytes) and
/// the machine's RAM (`ram`, bytes): half the limit, or a quarter of the RAM when unlimited. The
/// tighter of the two counts when a limit is set above the RAM.
pub fn derive_budget_mb(limit: Option<u64>, ram: Option<u64>) -> (u64, BudgetSource) {
    let (bytes, source) = match (limit, ram) {
        (Some(limit), Some(ram)) if limit <= ram => (limit / 2, BudgetSource::Cgroup),
        (Some(limit), None) => (limit / 2, BudgetSource::Cgroup),
        (_, Some(ram)) => (ram / 4, BudgetSource::Ram),
        (None, None) => return (FALLBACK_BUDGET_MB, BudgetSource::Fallback),
    };
    ((bytes / MIB).max(MIN_BUDGET_MB), source)
}

/// The tightest `memory.max` / `memory.limit_in_bytes` on the path of this process's cgroup up
/// to the root. `proc_cgroup` is the content of `/proc/self/cgroup`, `mount` where the cgroup
/// file systems are mounted (`/sys/fs/cgroup`). A container sees its own cgroup as the root, so
/// the root's file is read too.
pub fn cgroup_limit(mount: &Path, proc_cgroup: &str) -> Option<u64> {
    let mut limit: Option<u64> = None;
    let mut consider = |dir: &Path, file: &str| {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else {
            return;
        };
        // "max" is v2's "no limit"; v1 reports a huge page-rounded number instead.
        if let Ok(bytes) = text.trim().parse::<u64>() {
            if bytes < (1 << 60) {
                limit = Some(limit.map_or(bytes, |current| current.min(bytes)));
            }
        }
    };
    for line in proc_cgroup.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(_), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (base, file) = if controllers.is_empty() {
            (mount.to_path_buf(), "memory.max")
        } else if controllers.split(',').any(|name| name == "memory") {
            (mount.join("memory"), "memory.limit_in_bytes")
        } else {
            continue;
        };
        let mut dir = base.clone();
        consider(&dir, file);
        for part in path.split('/').filter(|part| !part.is_empty()) {
            dir.push(part);
            consider(&dir, file);
        }
    }
    limit
}

/// `MemTotal` of `/proc/meminfo`, in bytes.
pub fn meminfo_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

fn detect() -> (Option<u64>, BudgetSource) {
    let from_env = std::env::var("FASTPKG_MEMORY_BUDGET_MB").ok();
    match parse_budget_override(from_env.as_deref()) {
        Some(Some(mb)) => return (Some(mb.max(1)), BudgetSource::Env),
        Some(None) => return (None, BudgetSource::Off),
        None => {}
    }
    #[cfg(target_os = "linux")]
    let (limit, ram) = (
        std::fs::read_to_string("/proc/self/cgroup")
            .ok()
            .and_then(|text| cgroup_limit(Path::new("/sys/fs/cgroup"), &text)),
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| meminfo_total(&text)),
    );
    #[cfg(not(target_os = "linux"))]
    let (limit, ram): (Option<u64>, Option<u64>) = (None, None);
    let (mb, source) = derive_budget_mb(limit, ram);
    (Some(mb), source)
}

struct Gate {
    budget_mb: Option<u64>,
    source: BudgetSource,
    semaphore: Arc<Semaphore>,
    /// Estimates below this many bytes claim nothing: the smaller of [`FREE_BELOW`] and an eighth
    /// of a small budget.
    free_below: u64,
}

/// MiB being claimed right now, the most that was claimed at once, and how many claims had to wait.
static IN_FLIGHT_MB: AtomicU64 = AtomicU64::new(0);
static PEAK_IN_FLIGHT_MB: AtomicU64 = AtomicU64::new(0);
static CLAIMS: AtomicU64 = AtomicU64::new(0);
static WAITS: AtomicU64 = AtomicU64::new(0);

/// A running job's share of the memory budget; it goes back when this is dropped.
#[derive(Debug)]
pub struct Claim {
    _permit: Option<OwnedSemaphorePermit>,
    mb: u64,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.mb > 0 {
            IN_FLIGHT_MB.fetch_sub(self.mb, Ordering::AcqRel);
        }
    }
}

impl Gate {
    fn new(budget_mb: Option<u64>, source: BudgetSource) -> Self {
        let permits = budget_mb.map_or(0, |mb| mb.min(u32::MAX as u64 >> 1) as usize);
        Gate {
            budget_mb,
            source,
            semaphore: Arc::new(Semaphore::new(permits)),
            free_below: budget_mb.map_or(u64::MAX, |mb| FREE_BELOW.min(mb * MIB / 8)),
        }
    }

    /// The MiB a job estimated at `bytes` claims: nothing for a small one, otherwise the estimate
    /// rounded up and capped at the whole budget.
    fn weight_mb(&self, bytes: u64) -> u64 {
        match self.budget_mb {
            Some(budget) if bytes >= self.free_below => bytes.div_ceil(MIB).clamp(1, budget),
            _ => 0,
        }
    }

    async fn claim(&self, bytes: u64) -> Claim {
        let mb = self.weight_mb(bytes);
        if mb == 0 {
            return Claim {
                _permit: None,
                mb: 0,
            };
        }
        let permits = mb as u32;
        // `try_acquire` only succeeds when nobody is queued ahead (queued claims are handed the
        // permits as they come back), so the fast path keeps the order too.
        let permit = match Arc::clone(&self.semaphore).try_acquire_many_owned(permits) {
            Ok(permit) => permit,
            Err(_) => {
                WAITS.fetch_add(1, Ordering::Relaxed);
                // The semaphore is never closed, so acquiring only fails if that stops being true.
                Arc::clone(&self.semaphore)
                    .acquire_many_owned(permits)
                    .await
                    .expect("memory budget closed")
            }
        };
        CLAIMS.fetch_add(1, Ordering::Relaxed);
        let now = IN_FLIGHT_MB.fetch_add(mb, Ordering::AcqRel) + mb;
        PEAK_IN_FLIGHT_MB.fetch_max(now, Ordering::AcqRel);
        Claim {
            _permit: Some(permit),
            mb,
        }
    }
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| {
        let (budget_mb, source) = detect();
        Gate::new(budget_mb, source)
    })
}

/// Waits for `bytes` of the memory budget (see the module documentation).
pub async fn claim(bytes: u64) -> Claim {
    gate().claim(bytes).await
}

/// The state of the limiter, for diagnostics and tests.
pub fn info() -> Value {
    let gate = gate();
    json!({
        "budgetMb": gate.budget_mb,
        "source": gate.source.label(),
        "freeBelowMb": gate.free_below as f64 / MIB as f64,
        "inFlightMb": IN_FLIGHT_MB.load(Ordering::Acquire),
        "peakInFlightMb": PEAK_IN_FLIGHT_MB.load(Ordering::Acquire),
        "claims": CLAIMS.load(Ordering::Relaxed),
        "waits": WAITS.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_values() {
        assert_eq!(parse_budget_override(None), None);
        assert_eq!(parse_budget_override(Some("")), None);
        assert_eq!(parse_budget_override(Some("lots")), None);
        assert_eq!(parse_budget_override(Some("-5")), None);
        assert_eq!(parse_budget_override(Some("1.5")), None);
        assert_eq!(parse_budget_override(Some("768")), Some(Some(768)));
        assert_eq!(parse_budget_override(Some(" 64 ")), Some(Some(64)));
        for off in ["0", "off", "OFF", "unlimited", "none"] {
            assert_eq!(parse_budget_override(Some(off)), Some(None), "{off}");
        }
    }

    #[test]
    fn derived_budget() {
        let gib = 1u64 << 30;
        // Half a container's limit, a quarter of unlimited RAM.
        assert_eq!(derive_budget_mb(Some(4 * gib), Some(64 * gib)), (2048, BudgetSource::Cgroup));
        assert_eq!(derive_budget_mb(None, Some(32 * gib)), (8192, BudgetSource::Ram));
        // A limit above the RAM is no limit.
        assert_eq!(derive_budget_mb(Some(128 * gib), Some(32 * gib)), (8192, BudgetSource::Ram));
        // Never below 512 MB.
        assert_eq!(derive_budget_mb(Some(gib), Some(32 * gib)), (512, BudgetSource::Cgroup));
        assert_eq!(derive_budget_mb(Some(256 << 20), None), (512, BudgetSource::Cgroup));
        assert_eq!(derive_budget_mb(None, Some(gib)), (512, BudgetSource::Ram));
        assert_eq!(derive_budget_mb(None, None), (FALLBACK_BUDGET_MB, BudgetSource::Fallback));
    }

    #[test]
    fn meminfo() {
        let text = "MemTotal:       32871480 kB\nMemFree:         1048576 kB\n";
        assert_eq!(meminfo_total(text), Some(32871480 * 1024));
        assert_eq!(meminfo_total("MemFree: 1 kB\n"), None);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fastpkg-cgroup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cgroup_v2_takes_the_tightest_ancestor() {
        let mount = scratch("v2");
        std::fs::create_dir_all(mount.join("user.slice/app.scope")).unwrap();
        std::fs::write(mount.join("user.slice/memory.max"), "8589934592\n").unwrap();
        std::fs::write(mount.join("user.slice/app.scope/memory.max"), "max\n").unwrap();
        let limit = cgroup_limit(&mount, "0::/user.slice/app.scope\n");
        assert_eq!(limit, Some(8589934592));
        std::fs::write(mount.join("user.slice/app.scope/memory.max"), "1073741824\n").unwrap();
        assert_eq!(cgroup_limit(&mount, "0::/user.slice/app.scope\n"), Some(1073741824));
        // Inside a container the process sees its own cgroup as the root.
        std::fs::write(mount.join("memory.max"), "536870912\n").unwrap();
        assert_eq!(cgroup_limit(&mount, "0::/\n"), Some(536870912));
        assert_eq!(cgroup_limit(&mount, "0::/not/there\n"), Some(536870912));
        let _ = std::fs::remove_dir_all(&mount);
    }

    #[test]
    fn cgroup_v1_and_unlimited() {
        let mount = scratch("v1");
        std::fs::create_dir_all(mount.join("memory/docker/abc")).unwrap();
        std::fs::write(
            mount.join("memory/memory.limit_in_bytes"),
            "9223372036854771712\n",
        )
        .unwrap();
        std::fs::write(mount.join("memory/docker/abc/memory.limit_in_bytes"), "2147483648\n").unwrap();
        let text = "12:cpu,cpuacct:/docker/abc\n4:memory:/docker/abc\n1:name=systemd:/docker/abc\n";
        assert_eq!(cgroup_limit(&mount, text), Some(2147483648));
        std::fs::write(mount.join("memory/docker/abc/memory.limit_in_bytes"), "9223372036854771712\n").unwrap();
        assert_eq!(cgroup_limit(&mount, text), None);
        assert_eq!(cgroup_limit(&mount, "0::/\n"), None);
        let _ = std::fs::remove_dir_all(&mount);
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[test]
    fn small_jobs_claim_nothing() {
        let gate = Gate::new(Some(1024), BudgetSource::Env);
        assert_eq!(gate.weight_mb(0), 0);
        assert_eq!(gate.weight_mb(FREE_BELOW - 1), 0);
        assert_eq!(gate.weight_mb(FREE_BELOW), 8);
        assert_eq!(gate.weight_mb(100 * MIB + 1), 101);
        // A small budget lowers the line to an eighth of itself.
        let small = Gate::new(Some(64), BudgetSource::Env);
        assert_eq!(small.weight_mb(7 * MIB), 0);
        assert_eq!(small.weight_mb(8 * MIB), 8);
        // Without a budget nothing is claimed at all.
        let off = Gate::new(None, BudgetSource::Off);
        assert_eq!(off.weight_mb(u64::MAX), 0);
    }

    #[test]
    fn a_claim_above_the_budget_takes_all_of_it_and_runs_alone() {
        block_on(async {
            let gate = Arc::new(Gate::new(Some(100), BudgetSource::Env));
            assert_eq!(gate.weight_mb(300 * MIB), 100);
            let huge = gate.claim(300 * MIB).await;
            assert_eq!(gate.semaphore.available_permits(), 0);
            let started = Arc::new(AtomicU64::new(0));
            let waiting = tokio::spawn({
                let (gate, started) = (Arc::clone(&gate), Arc::clone(&started));
                async move {
                    let _claim = gate.claim(20 * MIB).await;
                    started.store(1, Ordering::SeqCst);
                }
            });
            settle().await;
            assert_eq!(started.load(Ordering::SeqCst), 0, "ran next to the huge job");
            drop(huge);
            waiting.await.unwrap();
            assert_eq!(started.load(Ordering::SeqCst), 1);
            assert_eq!(gate.semaphore.available_permits(), 100);
        });
    }

    #[test]
    fn claims_are_served_in_order() {
        block_on(async {
            let gate = Arc::new(Gate::new(Some(100), BudgetSource::Env));
            let order = Arc::new(std::sync::Mutex::new(Vec::new()));
            let first = gate.claim(60 * MIB).await;
            let spawn = |name: &'static str, mb: u64| {
                let (gate, order) = (Arc::clone(&gate), Arc::clone(&order));
                tokio::spawn(async move {
                    let claim = gate.claim(mb * MIB).await;
                    order.lock().unwrap().push(name);
                    tokio::task::yield_now().await;
                    drop(claim);
                })
            };
            // `big` cannot start next to `first`; `later` would fit next to it, but not ahead of
            // `big`.
            let big = spawn("big", 90);
            settle().await;
            let later = spawn("later", 30);
            settle().await;
            assert!(order.lock().unwrap().is_empty(), "a claim jumped the queue");
            drop(first);
            big.await.unwrap();
            later.await.unwrap();
            assert_eq!(*order.lock().unwrap(), ["big", "later"]);
        });
    }

    #[test]
    fn claims_come_back_when_the_job_panics() {
        block_on(async {
            let gate = Gate::new(Some(100), BudgetSource::Env);
            let claim = gate.claim(80 * MIB).await;
            assert_eq!(gate.semaphore.available_permits(), 20);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _claim = claim;
                panic!("job failed");
            }));
            assert!(outcome.is_err());
            assert_eq!(gate.semaphore.available_permits(), 100);
        });
    }
}
