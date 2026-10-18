//! The hot store's block format and the structures precomputed over it
//! (docs/spec/07-storage.md, ST-3 to ST-6, ST-8.3; 04-protocol.md 1.3): one
//! implementation shared by the writer (the data layer's staging) and every
//! reader (the data layer's queries, a kernel mapping segments), as pure
//! functions over slices, with no I/O, no registry and no semantics.
//!
//! - `tick`: a block's quantum (exponent × GCD), its verified reconstruction
//!   and the exact sums across blocks of different quanta (ST-3).
//! - `block`: the lane-interleaved layout, its width-specialised kernels,
//!   row ranges, single values, patched exceptions and the horizontal tail
//!   (ST-4.2 to ST-4.4, ST-8.3).
//! - `predict`: open, high and low from the close (ST-4.5).
//! - `summary`: block and cumulative summaries, cross-moments, sparse
//!   tables and the range rules (ST-5.1 to ST-5.3).
//! - `rank`: presence bitmaps with rank and select (ST-5.4).
//! - `wavelet`: wavelet matrices for order statistics over ranges (ST-5.7).
//! - `adjust`: adjusted views evaluated piecewise between events (ST-5.10).

pub mod block;
pub mod predict;
pub mod summary;
pub mod tick;

/// The values a block holds at most (ST-4.1).
pub const BLOCK: usize = 1024;

/// The version of the format every segment carries: readers refuse another.
pub const CODEC_VERSION: u16 = 1;
