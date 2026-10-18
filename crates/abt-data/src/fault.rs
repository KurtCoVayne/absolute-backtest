//! Crash injection for the recovery tests (ST-13's recovery row): with
//! `ABT_FAULT=<site>:<n>` in the environment the process aborts on the
//! `n`-th arrival at `site` (a point named in the write paths), as a
//! `kill -9` there would. Without the variable every point is free.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static SITE: OnceLock<Option<(String, u64)>> = OnceLock::new();
static ARRIVALS: AtomicU64 = AtomicU64::new(0);

fn configured() -> &'static Option<(String, u64)> {
    SITE.get_or_init(|| {
        let v = std::env::var("ABT_FAULT").ok()?;
        let (site, n) = v.split_once(':')?;
        Some((site.to_string(), n.parse().ok()?))
    })
}

/// A named point of a write path.
pub fn point(site: &str) {
    if let Some((s, n)) = configured() {
        if s == site {
            let k = ARRIVALS.fetch_add(1, Ordering::SeqCst) + 1;
            if k == *n {
                eprintln!("ABT_FAULT: aborting at {} (arrival {})", site, k);
                std::process::abort();
            }
        }
    }
}
