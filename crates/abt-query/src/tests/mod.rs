//! The test framework (Q-4): the `Test` trait and its registry, the
//! dependence treatments, the seeded generator, and the engines the tests
//! share (least squares, HAC and clustered covariances, the stationary
//! bootstrap, the null distributions), with the starting set of Q-4.4.

pub mod dependence;
pub mod dist;
pub mod framework;
pub mod hac;
pub mod linalg;
pub mod registry;
pub mod regression;
pub mod resample;
pub mod rng;
pub mod set;

pub use framework::{ClusterBy, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
pub use registry::{get, names, register};
