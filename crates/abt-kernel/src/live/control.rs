//! The kill switch (K-6.5): `halt_all` stops evaluation after the current
//! barrier, `flatten_all` makes the next decision bar's decisions a closing
//! decision on every position, through the normal path. A run's control is
//! read at every decision barrier; it is set by a `control` request to the
//! kernel service, by the deployment's file appearing, or by a signal the
//! deployment names.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use super::deployment::Deployment;

/// A run's kill switch. Clones share their flags: the kernel service keeps
/// one, the run's fold reads another.
#[derive(Clone, Debug, Default)]
pub struct LiveControl {
    halt: Arc<AtomicBool>,
    flatten: Arc<AtomicBool>,
    file: Option<PathBuf>,
    /// The signals it halts on: each one's name, number and how many times
    /// it had been raised when the control was made.
    signals: Vec<(String, i32, u64)>,
}

impl LiveControl {
    /// A control set only by requests.
    pub fn new() -> LiveControl {
        LiveControl::default()
    }

    /// A deployment's kill switch: its file, and its signals, whose handlers
    /// are installed now.
    pub fn of(d: &Deployment) -> LiveControl {
        let mut c = LiveControl {
            file: d.halt_file().map(PathBuf::from),
            ..LiveControl::default()
        };
        for name in d.signals() {
            if let Some(n) = signals::install(name) {
                c.signals.push((name.to_string(), n, signals::raised(n)));
            }
        }
        c
    }

    /// `control halt`: halt after the current barrier.
    pub fn halt(&self) {
        self.halt.store(true, Ordering::SeqCst);
    }

    /// `control flatten`: close every position at the next decision bar.
    pub fn flatten(&self) {
        self.flatten.store(true, Ordering::SeqCst);
    }

    /// What asks the run to halt, if anything does.
    pub fn halt_requested(&self) -> Option<String> {
        if self.halt.load(Ordering::SeqCst) {
            return Some("control".into());
        }
        if let Some(f) = self.file.as_ref().filter(|f| f.exists()) {
            return Some(format!("file {}", f.display()));
        }
        self.signals.iter().find(|(_, n, at)| signals::raised(*n) > *at).map(|(name, ..)| format!("signal {}", name))
    }

    /// Whether a flatten was asked for since the last call (once per
    /// request), and what asked.
    pub fn take_flatten(&self) -> Option<String> {
        self.flatten.swap(false, Ordering::SeqCst).then(|| "control".to_string())
    }
}

/// The signals' handlers: each counts the times its signal was raised,
/// which is all a handler may safely do.
mod signals {
    use super::{AtomicU64, Ordering};

    static RAISED: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];

    /// The signal's number on this platform.
    pub fn number(name: &str) -> Option<i32> {
        Some(match name.trim_start_matches("SIG") {
            "HUP" => 1,
            "INT" => 2,
            "QUIT" => 3,
            "TERM" => 15,
            #[cfg(target_os = "macos")]
            "USR1" => 30,
            #[cfg(target_os = "macos")]
            "USR2" => 31,
            #[cfg(not(target_os = "macos"))]
            "USR1" => 10,
            #[cfg(not(target_os = "macos"))]
            "USR2" => 12,
            _ => return None,
        })
    }

    pub fn raised(n: i32) -> u64 {
        RAISED.get(n as usize).map(|c| c.load(Ordering::SeqCst)).unwrap_or(0)
    }

    #[cfg(unix)]
    extern "C" fn on_signal(n: i32) {
        if let Some(c) = RAISED.get(n as usize) {
            c.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }

    /// Installs the handler for the named signal; its number, or none
    /// where it cannot be.
    #[cfg(unix)]
    pub fn install(name: &str) -> Option<i32> {
        let n = number(name)?;
        // SAFETY: `signal` installs a handler that only increments an
        // atomic counter, which is async-signal-safe; SIG_ERR is -1.
        let previous = unsafe { signal(n, on_signal as extern "C" fn(i32) as usize) };
        (previous != usize::MAX).then_some(n)
    }

    #[cfg(not(unix))]
    pub fn install(_name: &str) -> Option<i32> {
        None
    }

    #[cfg(all(test, unix))]
    mod tests {
        extern "C" {
            fn raise(sig: i32) -> i32;
        }

        #[test]
        fn a_signal_the_deployment_names_halts_and_counts_from_the_control() {
            let d = crate::live::deployment::parse("deployment { account A broker mock kill_switch { halt_all on SIGUSR2 } }").unwrap();
            let c = crate::live::LiveControl::of(&d);
            assert_eq!(c.halt_requested(), None);
            // SAFETY: raising a signal whose handler was just installed.
            assert_eq!(unsafe { raise(super::number("SIGUSR2").unwrap()) }, 0);
            assert_eq!(c.halt_requested().as_deref(), Some("signal SIGUSR2"));
            // A control made after the signal does not see it.
            assert_eq!(crate::live::LiveControl::of(&d).halt_requested(), None);
        }
    }
}
