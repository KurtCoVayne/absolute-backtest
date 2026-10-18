//! Staging (docs/spec/07-storage.md, ST-7): data moves from a source to
//! the hot store through stages whose outputs are immutable, so a failure
//! leaves the published version untouched.
//!
//! - S0 Land: the landing log of vendor objects (`land`), one line per
//!   object with its checksum, written by the adapters that fetch files.
//! - S1 Normalise: a relation's conformed rows per instrument, every value
//!   as the bits its block must reproduce (`normalise`).
//! - S2 Classify: appends, duplicates, revisions and late rows against the
//!   current version (`classify`).
//! - S3 Build, S4 Verify, S5 Publish: segments, presence and the manifest,
//!   every new block decoded against its source, the version renamed into
//!   place (`build`).
//! - S6 Export: the cold copy (`abt data export`; iteration 1 writes
//!   Parquet, the S3 tier is the owner's).
//!
//! The store's write path runs S1 to S5 (`Store::write_hot`); `Report`
//! is what `abt data ingest --measure` prints.

pub mod build;
pub mod classify;
pub mod normalise;

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::DataError;

/// What staging a version did and measured.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Report {
    pub relation: String,
    pub version: String,
    pub counts: classify::Counts,
    pub measure: build::Measure,
    pub verified: build::Verified,
    pub millis: [u64; 5],
}

/// S0: a landed vendor object, appended to the store's landing log
/// (`staging/landing.jsonl`): the source, the object's key, its checksum
/// (the vendor's where one exists, else BLAKE3 of the bytes), its size
/// and the instant it was fetched.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Landed {
    pub source: String,
    pub key: String,
    pub checksum: String,
    pub bytes: u64,
    pub fetched_at: i64,
    /// Whether the checksum was checked against the vendor's manifest.
    pub verified: bool,
}

pub fn land(root: &Path, entry: &Landed) -> Result<(), DataError> {
    let dir = root.join("staging");
    fs::create_dir_all(&dir)?;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(dir.join("landing.jsonl"))?;
    writeln!(f, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

/// The landing log's entries.
pub fn landed(root: &Path) -> Result<Vec<Landed>, DataError> {
    let path = root.join("staging").join("landing.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(DataError::from)).collect()
}

/// The BLAKE3 of a landed file, hex.
pub fn checksum(path: &Path) -> Result<String, DataError> {
    let mut hasher = blake3::Hasher::new();
    let mut f = fs::File::open(path)?;
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// The reports of this process's ingests, for `--measure`.
pub static REPORTS: std::sync::Mutex<Vec<Report>> = std::sync::Mutex::new(Vec::new());

pub fn record(report: Report) {
    if let Ok(mut r) = REPORTS.lock() {
        r.push(report);
    }
}

pub fn reports() -> Vec<Report> {
    REPORTS.lock().map(|r| r.clone()).unwrap_or_default()
}
