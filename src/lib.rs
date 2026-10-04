//! Strategy DSL v1: typed IR, well-formedness checker and backtesting kernel.
//!
//! The semantics are fixed by `docs/semantic-model.md`; every checker rule
//! and kernel behaviour cites the section or judgment it implements.

// RunError is deliberately rich (rule, tuple and expression of a halt); its
// size on the error path does not matter.
#![allow(clippy::result_large_err)]

pub mod check;
pub mod data;
pub mod ir;
pub mod kernel;
pub mod lexer;
pub mod parser;

pub use check::{check_program, check_workspace, Code, Diagnostic, Program, Severity, Workspace};
pub use ir::*;
pub use kernel::{run, verify_causality, Dataset, ExecConfig, RunError, RunResult};
pub use parser::{parse_units, ParseError};
