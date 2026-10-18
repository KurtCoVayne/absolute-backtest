//! The one implementation of every aggregate (docs/spec/04-protocol.md,
//! P-1.2; decision A-D8), used by the data layer's aggregate pass and by the
//! kernel alike, so that an aggregate pushed down to the data layer equals
//! the kernel's to the bit (Proposition P-1.1).
//!
//! Rows arrive in the canonical order (entity ascending, then time
//! ascending; A-4.7); nothing here sorts a window's rows. `reduce` takes a
//! group's columns whole; `WindowState` and `RowsWindow` keep a sliding
//! window as rows enter and leave; `ShiftedSums` are the running sums of
//! `numerics running`.

pub mod exact;
pub mod function;
pub mod moments;
pub mod order_stats;
pub mod reduce;
pub mod window;

pub use exact::{reduce_ticks, ExactSums, Ticks};
pub use function::{AggError, Class, Function, Mode};
pub use moments::{checkpoint_period, Neumaier, ShiftedSums};
pub use reduce::reduce;
pub use window::{RowsWindow, Value, WindowState};
