//! The one implementation of every aggregate (docs/spec/04-protocol.md,
//! P-1.2; A-D8): window states and reductions in the canonical reduction
//! order, used by the data layer's aggregate pass and by the kernel alike, so
//! a pushed-down aggregate equals the kernel's to the bit. Filled at M-1.
