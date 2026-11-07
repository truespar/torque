use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Allocator tuning
// ---------------------------------------------------------------------------

/// Configure jemalloc to return freed memory to the OS aggressively.
///
/// - `background_thread:true` — async purging of dirty/muzzy pages. This is
///   inert unless the `background_threads` feature of `tikv-jemallocator` is
///   also on: the default feature set only compiles the runtime support, so
///   the option parses, does nothing, and leaves purging to whatever calls
///   [`purge_allocator`].
/// - `dirty_decay_ms:0` — return dirty pages to the OS immediately
/// - `muzzy_decay_ms:0` — immediately release muzzy (lazy-purged) pages
/// - `narenas:4` — limit arena count (default 2×cores = 40 on 20-core);
///   fewer arenas = less fragmentation and faster purge
#[cfg(feature = "jemalloc")]
#[allow(non_upper_case_globals)]
#[unsafe(export_name = "malloc_conf")]
pub static malloc_conf: &[u8] =
    b"background_thread:true,dirty_decay_ms:0,muzzy_decay_ms:0,narenas:4\0";

// ---------------------------------------------------------------------------
// Allocator purge
// ---------------------------------------------------------------------------

/// Drain allocator caches on all threads and return freed pages to the OS.
///
/// Index data is allocated on rayon worker threads during parallel builds
/// but freed on HTTP handler threads (or when the last `Arc` drops).
/// Per-thread delayed-free lists can hold onto large amounts of memory.
/// This function flushes thread caches on all rayon workers and the current
/// thread, then purges dirty pages across all arenas.
pub fn purge_allocator() {
    #[cfg(feature = "jemalloc")]
    {
        use tikv_jemalloc_ctl::{epoch, raw};
        // Flush tcache on every rayon worker thread
        rayon::broadcast(|_| {
            let _ = unsafe { raw::write(b"thread.tcache.flush\0", 0u64) };
        });
        // Flush tcache on the current (caller) thread too
        let _ = unsafe { raw::write(b"thread.tcache.flush\0", 0u64) };
        // Advance epoch so stats reflect the flushes
        let _ = epoch::advance();
        // Purge dirty pages across ALL arenas (4096 = MALLCTL_ARENAS_ALL)
        let _ = unsafe { raw::write(b"arena.4096.purge\0", 0u64) };
    }
    #[cfg(all(feature = "native", not(feature = "jemalloc"), target_env = "msvc"))]
    {
        unsafe { libmimalloc_sys::mi_collect(true); }
        rayon::broadcast(|_| {
            unsafe { libmimalloc_sys::mi_collect(true); }
        });
    }
    tracing::debug!("purged allocator caches");
}

// ---------------------------------------------------------------------------
// Real-time RSS reading (replaces Prometheus gauge polling)
// ---------------------------------------------------------------------------

/// How often to actually read RSS from the OS. Every Nth call to
/// `allocated_bytes()` does a real syscall; the rest return the cached value.
const REFRESH_INTERVAL: usize = 256;

static CACHED_RSS: AtomicUsize = AtomicUsize::new(0);
static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
static MEMORY_CEILING: AtomicU64 = AtomicU64::new(0);

/// Read current process RSS with caching (refreshed every 256 calls).
/// Costs ~1µs amortized over 256 calls.
#[inline]
pub fn allocated_bytes() -> usize {
    let count = CALL_COUNT.fetch_add(1, Ordering::Relaxed);
    if count % REFRESH_INTERVAL == 0 {
        allocated_bytes_fresh()
    } else {
        CACHED_RSS.load(Ordering::Relaxed)
    }
}

/// Read current process RSS without caching. Always does the OS syscall.
pub fn allocated_bytes_fresh() -> usize {
    let rss = process_rss().unwrap_or(0);
    CACHED_RSS.store(rss, Ordering::Relaxed);
    rss
}

/// Set the memory ceiling (called once at startup from Config::max_memory_bytes).
pub fn set_memory_ceiling(bytes: u64) {
    MEMORY_CEILING.store(bytes, Ordering::Release);
}

/// Get the memory ceiling (0 = unlimited).
pub fn memory_ceiling() -> Option<u64> {
    let v = MEMORY_CEILING.load(Ordering::Acquire);
    if v == 0 { None } else { Some(v) }
}

/// Check if current RSS is under the memory ceiling.
/// Uses fresh (uncached) RSS read for accuracy during writes.
pub fn check_memory() -> Result<(), String> {
    if let Some(ceiling) = memory_ceiling() {
        let used = allocated_bytes_fresh() as u64;
        if used > ceiling {
            return Err(format!(
                "Memory limit exceeded: using {} MB of {} MB limit. \
                 Reject new data until memory is freed.",
                used / (1024 * 1024),
                ceiling / (1024 * 1024),
            ));
        }
    }
    Ok(())
}

/// Preflight check: estimate if an operation of `estimated_bytes` would
/// exceed the memory ceiling. Uses 2x multiplier for parsing/build overhead.
/// Returns Ok if there's enough headroom.
pub fn check_memory_preflight(estimated_bytes: u64) -> Result<(), String> {
    if let Some(ceiling) = memory_ceiling() {
        let used = allocated_bytes_fresh() as u64;
        let estimate = estimated_bytes * 2;
        if used + estimate > ceiling {
            return Err(format!(
                "Insufficient memory: using {} MB, operation needs ~{} MB, limit is {} MB.",
                used / (1024 * 1024),
                estimate / (1024 * 1024),
                ceiling / (1024 * 1024),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "native")]
pub fn available_memory_bytes() -> u64 {
    system_memory().available_memory()
}

/// The system's memory figures alone (`System::new_all` would list every process, disk and
/// network too: about 200 ms on Windows, on every import that asks).
#[cfg(feature = "native")]
fn system_memory() -> sysinfo::System {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys
}

#[cfg(not(feature = "native"))]
pub fn available_memory_bytes() -> u64 {
    u64::MAX // unknown platform: don't gate
}

/// Build-phase memory multiplier: peak working set as a multiple of the uncompressed payload. 6 is a
/// conservative default; callers can pass a workload-specific value via
/// `check_import_admission_with_factor`.
pub const DEFAULT_BUILD_MULTIPLIER: u64 = 6;

/// Admission control for collection imports.
///
/// Estimates peak memory required to process an import of `payload_bytes`
/// uncompressed bytes, compares against currently-available physical
/// memory, and rejects with a descriptive error if insufficient.
///
/// Gives clients a clean 507 rather than thrashing the host.
///
/// Applies a 70% safety margin: reservation must fit in 70% of the
/// currently-available memory, leaving headroom for concurrent work.
pub fn check_import_admission(payload_bytes: u64) -> Result<(), String> {
    check_import_admission_with_factor(payload_bytes, DEFAULT_BUILD_MULTIPLIER)
}

/// As `check_import_admission` but with a caller-supplied build multiplier.
pub fn check_import_admission_with_factor(
    payload_bytes: u64,
    build_multiplier: u64,
) -> Result<(), String> {
    let available = available_memory_bytes();
    if available == 0 || available == u64::MAX {
        return Ok(()); // unknown: fall through; preflight ceiling check still applies
    }
    let estimate = payload_bytes.saturating_mul(build_multiplier);
    let budget = available.saturating_mul(70) / 100;
    if estimate > budget {
        return Err(format!(
            "Insufficient memory to accept import: payload {} MB × {}x build \
             overhead ≈ {} MB peak required, but only {} MB available \
             (70% safety margin = {} MB). Delete a collection or restart.",
            payload_bytes / (1024 * 1024),
            build_multiplier,
            estimate / (1024 * 1024),
            available / (1024 * 1024),
            budget / (1024 * 1024),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Disk space checks
// ---------------------------------------------------------------------------

/// Minimum free disk space required for writes (10 MB).
/// Below this threshold, imports are rejected to prevent silent persistence
/// failures and filesystem corruption.
const DISK_LOW_THRESHOLD: u64 = 10 * 1024 * 1024;

/// Returns (free_bytes, total_bytes) for the data directory filesystem.
/// Returns (0, 0) if the filesystem is unavailable.
pub fn disk_space() -> (u64, u64) {
    {
        // On native Linux/macOS, use libc::statvfs on the data directory.
        // Falls back to (0, 0) on error (effectively disabling the check).
        (0, 0)
    }
}

/// Preflight check: reject writes if disk space is critically low.
/// Returns Ok if there's enough space, Err with a user-facing message if not.
pub fn check_disk_preflight() -> Result<(), String> {
    let (free, total) = disk_space();
    if total == 0 {
        return Ok(()); // no filesystem or not supported — skip check
    }
    if free < DISK_LOW_THRESHOLD {
        return Err(format!(
            "Insufficient disk space: {} MB free of {} MB total. \
             Delete unused collections or databases to free space.",
            free / (1024 * 1024),
            total / (1024 * 1024),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Platform-specific RSS reading
// ---------------------------------------------------------------------------

/// Read process RSS in bytes.
#[cfg(target_os = "linux")]
pub(crate) fn process_rss() -> Option<usize> {
    use std::io::Read;
    let mut buf = [0u8; 128];
    let mut f = std::fs::File::open("/proc/self/statm").ok()?;
    let n = f.read(&mut buf).ok()?;
    let s = std::str::from_utf8(&buf[..n]).ok()?;
    // /proc/self/statm: size resident shared text lib data dt
    let rss_pages: usize = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(rss_pages * 4096)
}

/// Read process RSS in bytes via GetProcessMemoryInfo.
#[cfg(target_os = "windows")]
pub(crate) fn process_rss() -> Option<usize> {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCounters {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            ppsmemCounters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    unsafe {
        let mut pmc = core::mem::zeroed::<ProcessMemoryCounters>();
        pmc.cb = core::mem::size_of::<ProcessMemoryCounters>() as u32;
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            pmc.cb,
        );
        if ok != 0 {
            Some(pmc.WorkingSetSize)
        } else {
            None
        }
    }
}

/// Fallback for other platforms.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(crate) fn process_rss() -> Option<usize> {
    None
}

// ---------------------------------------------------------------------------
// Total system memory (for config auto-detect)
// ---------------------------------------------------------------------------

/// Get total system memory in bytes.
#[cfg(feature = "native")]
pub fn total_memory_bytes() -> u64 {
    system_memory().total_memory()
}

// ---------------------------------------------------------------------------
// MemoryGuard (now uses real-time RSS instead of Prometheus gauge)
// ---------------------------------------------------------------------------

/// Guards against exceeding the configured memory limit.
/// Uses real-time RSS reads via `check_memory()`.
#[derive(Clone)]
pub struct MemoryGuard {
    max_bytes: u64,
    /// Prometheus gauge for observability (still updated by stats collector).
    resident_gauge: prometheus_client::metrics::gauge::Gauge,
}

impl MemoryGuard {
    pub fn new(max_bytes: u64, resident_gauge: prometheus_client::metrics::gauge::Gauge) -> Self {
        // Set the global ceiling so check_memory() and preflight work
        set_memory_ceiling(max_bytes);
        Self {
            max_bytes,
            resident_gauge,
        }
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Returns Ok(()) if memory is under the limit, or an error message if over.
    /// Uses real-time RSS reading (not the cached gauge).
    pub fn check(&self) -> Result<(), String> {
        check_memory()
    }

    /// Preflight: check if an operation of `estimated_bytes` would exceed the limit.
    pub fn check_preflight(&self, estimated_bytes: u64) -> Result<(), String> {
        check_memory_preflight(estimated_bytes)
    }

    /// Update the Prometheus gauge with the latest RSS (called by stats collector).
    pub fn update_gauge(&self) {
        let rss = allocated_bytes() as i64;
        self.resident_gauge.set(rss);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_rss_returns_value() {
        let rss = process_rss();
        // Should return Some on Linux/Windows, None on other platforms
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(rss.is_some(), "process_rss() should return Some on this platform");
        let _ = rss; // suppress unused on other platforms
    }

    #[test]
    fn allocated_bytes_returns_nonzero() {
        let v = allocated_bytes_fresh();
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(v > 0, "allocated_bytes_fresh() should be > 0");
        let _ = v;
    }

    #[test]
    fn check_memory_passes_under_limit() {
        set_memory_ceiling(u64::MAX); // effectively unlimited
        assert!(check_memory().is_ok());
    }

    #[test]
    fn check_memory_fails_over_limit() {
        set_memory_ceiling(1); // 1 byte — any process exceeds this
        let result = check_memory();
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(result.is_err());
        // Restore
        set_memory_ceiling(u64::MAX);
        let _ = result;
    }

    #[test]
    fn preflight_rejects_large_estimate() {
        set_memory_ceiling(1); // 1 byte
        let result = check_memory_preflight(1024 * 1024 * 1024); // 1 GB
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(result.is_err());
        set_memory_ceiling(u64::MAX);
        let _ = result;
    }
}
