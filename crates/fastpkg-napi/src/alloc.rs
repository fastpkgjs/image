//! Process-wide allocator tuning for the glibc targets (`*-linux-gnu`).
//!
//! glibc grows its mmap threshold dynamically: every time a mmapped chunk is freed, chunks of
//! that size and below are served from the brk heap / per-thread arenas from then on (the cap is
//! 32 MiB). An image service allocates and frees many multi-megabyte pixel buffers from several
//! threads (the Node main thread, the tokio blocking pool and the rayon workers), so after a few
//! iterations those buffers land in arenas where a single live allocation near the top pins the
//! rest and `free` cannot give the pages back. RSS then sits near the peak of the busiest
//! iteration: for sequential 1024x1024 RGBA decode/compose/encode loops that was about 2x what
//! the same loop needs when the buffers are returned to the kernel (190 MB vs 100 MB), and
//! 260 MB vs 100-180 MB for 8 concurrent 4000x3000 JPEG downscales.
//!
//! Pinning the threshold turns the dynamic adjustment off: buffers of `MMAP_THRESHOLD` and above
//! are mmapped and unmapped on every use, smaller ones keep using the fast arena paths. The
//! price is page-fault time for those buffers (system CPU time rises by a few ms per 1024x1024
//! RGBA pipeline, about +10% CPU there and about 0-2% for JPEG downscales). 4 MiB is the
//! smallest value that keeps 4 MiB RGBA frames and everything larger out of the arenas without
//! also mmapping the 2-3 MiB intermediates of downscaled JPEG decodes, which measurably cost
//! more (+10% CPU at 2 MiB, mostly `munmap`/fault time under concurrency).
//!
//! The other release targets do not have this behaviour (musl's allocator and the macOS and
//! Windows heaps hand large blocks back to the OS on free), so nothing is changed there.
//!
//! Escape hatches: `FASTPKG_MALLOC_TUNING=0` skips the tweak; glibc's own
//! `MALLOC_MMAP_THRESHOLD_` / `MALLOC_TOP_PAD_` / `GLIBC_TUNABLES` settings win if present.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn tune() {
    use std::env;

    /// Buffers of at least this many bytes are mmapped and unmapped on every allocation.
    const MMAP_THRESHOLD: libc::c_int = 4 << 20;

    if env::var_os("FASTPKG_MALLOC_TUNING").is_some_and(|v| v == "0") {
        return;
    }
    let user_tuned = [
        "MALLOC_MMAP_THRESHOLD_",
        "MALLOC_TOP_PAD_",
        "GLIBC_TUNABLES",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some());
    if user_tuned {
        return;
    }
    // SAFETY: mallopt only adjusts allocator parameters and may be called at any time.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn tune() {}
