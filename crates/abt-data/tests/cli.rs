//! The data layer's own program, `abt-data`, driven as a subprocess. Its
//! commands are the front's `abt data ...` and are tested through the front
//! (`crates/abt/tests/data_cli.rs`, `deployment.rs`); what is the program's
//! alone is tested here.

use std::process::Command;

/// `abt-data --version` and `-V` print one line and nothing else, `abt-data
/// VERSION (BUILD)`: the crate's version and the identity the build script
/// gave the build (`scripts/build_id.rs`), which is never empty.
#[test]
fn version_prints_the_crates_version_and_the_builds_identity() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_abt-data")).arg(flag).output().expect("abt-data runs");
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string());
        assert_eq!(out.status.code(), Some(0), "{}: {}", flag, stderr);
        assert_eq!(stderr, "", "{}", flag);
        let id = stdout
            .strip_prefix(concat!("abt-data ", env!("CARGO_PKG_VERSION"), " ("))
            .and_then(|rest| rest.strip_suffix(")\n"))
            .unwrap_or_else(|| panic!("{}: {:?}", flag, stdout));
        assert!(!id.is_empty() && id.trim() == id && !id.contains('\n'), "{}: {:?}", flag, stdout);
        assert_eq!(id, env!("ABT_BUILD"), "{}: the identity printed is the build's", flag);
    }
}
