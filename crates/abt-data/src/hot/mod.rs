//! The hot store (docs/spec/07-storage.md): a relation's rows held as
//! integer ticks in lane-interleaved blocks per instrument, with the
//! structures of ST-5 beside them, memory-mapped from content-addressed
//! segments and read through `abt-codec`. A version of a hot relation is
//! the directory `hot/<relation>/<version>/`: its manifest, its presence
//! file (labels, instruments, sequence runs and bitmaps) and the segments
//! it names under `hot/segments/`. Staging (ST-7) builds versions in
//! `crate::staging`.

pub mod manifest;
pub mod presence;
pub mod segment;
pub mod version;

pub use manifest::HotManifest;
pub use version::HotVersion;
