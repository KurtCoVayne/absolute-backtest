//! Residency (ST-9.1, ST-9.2; A-D45): hot means resident. At start and
//! after each publication the data layer prefaults every segment of the
//! versions it serves and locks them in memory, best effort: a refused lock
//! (a limit on locked memory) leaves the store unlocked and is reported as
//! a shortfall in `stats`, with the process's major page faults. The memory
//! profile is the data layer's configuration: it sets how many kernels the
//! query layer's plan executor runs at once.

use std::sync::atomic::{AtomicU64, Ordering};

use super::version::HotVersion;

/// The memory profile (ST-9.2): the machine abt runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// A machine that runs only abt: about 14 GB at peak, four kernels.
    Dedicated,
    /// A desktop session beside abt (the default until a dedicated
    /// machine exists): at most two kernels at a time, staging only while
    /// no study runs, run-scoped structures at most 1.5 GB.
    Workstation,
}

impl Profile {
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "dedicated" => Some(Profile::Dedicated),
            "workstation" => Some(Profile::Workstation),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Dedicated => "dedicated",
            Profile::Workstation => "workstation",
        }
    }

    /// Kernels the plan executor runs at once (ST-9.2).
    pub fn kernel_limit(self) -> u32 {
        match self {
            Profile::Dedicated => 4,
            Profile::Workstation => 2,
        }
    }

    /// Run-scoped structures' budget in bytes (ST-9.2).
    pub fn run_scope_bytes(self) -> u64 {
        match self {
            Profile::Dedicated => 2 << 30,
            Profile::Workstation => 3 << 29,
        }
    }
}

/// What the data layer does with a version's segments when it opens them.
#[derive(Debug)]
pub struct Residency {
    pub profile: Profile,
    /// Prefault and lock (a served store); a test's or a one-off command's
    /// store leaves its pages to the operating system.
    pub resident: bool,
    locked: AtomicU64,
    shortfall: AtomicU64,
    prefaulted: AtomicU64,
}

impl Residency {
    /// No prefault, no lock (the default of an opened store).
    pub fn off() -> Residency {
        Residency {
            profile: Profile::Workstation,
            resident: false,
            locked: AtomicU64::new(0),
            shortfall: AtomicU64::new(0),
            prefaulted: AtomicU64::new(0),
        }
    }

    /// Prefault and lock under the profile.
    pub fn with_profile(profile: Profile) -> Residency {
        Residency {
            profile,
            resident: true,
            ..Residency::off()
        }
    }

    /// Prefaults and locks every segment of the version, best effort: a
    /// segment the lock is refused for counts as a shortfall.
    pub fn apply(&self, v: &HotVersion) {
        if !self.resident {
            return;
        }
        for seg in &v.segments {
            let m = seg.map();
            let _ = m.advise(memmap2::Advice::WillNeed);
            // A touch per page: the prefault (MAP_POPULATE's effect on a
            // mapping made without it).
            let mut sum = 0u64;
            let mut at = 0usize;
            while at < m.len() {
                sum = sum.wrapping_add(m[at] as u64);
                at += 4096;
            }
            std::hint::black_box(sum);
            self.prefaulted.fetch_add(m.len() as u64, Ordering::Relaxed);
            match m.lock() {
                Ok(()) => self.locked.fetch_add(m.len() as u64, Ordering::Relaxed),
                Err(_) => self.shortfall.fetch_add(m.len() as u64, Ordering::Relaxed),
            };
        }
    }

    /// Bytes locked, bytes the lock was refused for, bytes prefaulted.
    pub fn figures(&self) -> (u64, u64, u64) {
        (self.locked.load(Ordering::Relaxed), self.shortfall.load(Ordering::Relaxed), self.prefaulted.load(Ordering::Relaxed))
    }
}

/// The process's major page faults so far (`/proc/self/stat`, Linux),
/// else 0.
pub fn major_faults() -> u64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else { return 0 };
    // The command name sits in parentheses and may hold spaces: the fields
    // after the closing one, of which majflt is the tenth.
    let Some(after) = stat.rfind(')') else { return 0 };
    stat[after + 1..].split_whitespace().nth(9).and_then(|f| f.parse().ok()).unwrap_or(0)
}

/// The locked-memory limit of this process in bytes (`RLIMIT_MEMLOCK`),
/// from `/proc/self/limits`; `None` when unlimited or unknown.
pub fn memlock_limit() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = text.lines().find(|l| l.starts_with("Max locked memory"))?;
    let soft = line.split_whitespace().nth(3)?;
    soft.parse().ok()
}
