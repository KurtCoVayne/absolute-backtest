//! The strategy language of `docs/language-v2.md`: its front end, the
//! elaborator and checker, the compiler stages, the backtesting kernel and
//! the query facility. Every judgment and kernel behaviour cites the section
//! it implements; `corpus-v2/` is the acceptance corpus.

// RunError is deliberately rich (rule, tuple and expression of a halt); its
// size on the error path does not matter.
#![allow(clippy::result_large_err)]

pub mod analysis;
pub mod bundle;
pub mod check;
pub mod compiler;
pub mod data;
pub mod dump;
pub mod ingest;
pub mod ir;
pub mod kernel;
pub mod lower;
pub mod observe;
pub mod plan;
pub mod rewrite;
pub mod state;
pub mod study;
pub mod syntax;
pub mod table;
pub mod verify;

pub use check::{check_program, check_workspace, Code, Diagnostic, Program, Severity, Workspace};
pub use ir::*;
pub use kernel::{run, verify_causality, Dataset, ExecConfig, RelationCoverage, RunError, RunResult};
pub use syntax::{parse_units, ParseError};
