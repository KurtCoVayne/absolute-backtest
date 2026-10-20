//! Freed heap memory handed back to the system (E-110). glibc gives a thread
//! a heap arena of its own (up to eight per core) and keeps the pages a
//! freed allocation leaves inside an arena for that arena's next one; the
//! services handle each request, and the kernel each run, on a thread of
//! its own, so every arena kept the peak of the largest request or run it
//! served, gigabytes past the resident budget. `trim` returns the free
//! pages of every arena when a request or a run ends.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The last trim, in milliseconds since the epoch: requests ending within
/// `TRIM_INTERVAL_MS` of it share it.
static LAST_TRIM_MS: AtomicU64 = AtomicU64::new(0);

const TRIM_INTERVAL_MS: u64 = 100;

/// Returns the free pages of every heap arena to the system (glibc's
/// `malloc_trim(0)`), at most once per `TRIM_INTERVAL_MS`; elsewhere
/// nothing.
pub fn trim() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let last = LAST_TRIM_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < TRIM_INTERVAL_MS || LAST_TRIM_MS.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    trim_now();
}

/// `trim` without the interval: a run that ended, whose memory is the
/// largest a process frees at once.
pub fn trim_now() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> std::os::raw::c_int;
        }
        // SAFETY: malloc_trim only returns pages the allocator holds free.
        unsafe {
            malloc_trim(0);
        }
    }
}

/// The process's resident anonymous memory in bytes (`RssAnon` of
/// `/proc/self/status`), where the system reports it.
pub fn anonymous_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("RssAnon:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kib| kib * 1024)
}
