//! The aggregate functions (D-6.1, P-1.2), their classes, the numerics
//! modes, and the partial points every implementation refuses alike.

use std::fmt;

/// A function of the closed set. `ols_beta` is `ols_slope` under its first
/// formulation's name (`docs/semantic-model.md`): the slope of the first
/// column on the second.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Function {
    Sum,
    Count,
    Mean,
    Var,
    Std,
    Min,
    Max,
    First,
    Last,
    Median,
    /// The level is in [0, 1].
    Quantile(f64),
    Cov,
    Corr,
    OlsSlope,
    OlsR,
    OlsBeta,
}

/// How a function's window state works (P-1.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Class {
    /// `sum`, `count`, `min`, `max`, `first`, `last`: O(1) amortised.
    Distributive,
    /// The moments: O(w) two-pass, O(1) running.
    Algebraic,
    /// `median`, `quantile`: an order-statistics structure.
    Holistic,
}

/// The numerics of the program's `execution` block (V-4.3; A-4.7).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// The window's rows reduced by the two-pass formulas in canonical order.
    TwoPass,
    /// Shifted compensated sliding sums for the moments of a `rows(N)`
    /// window, rebuilt exactly every N rows; every other window and
    /// function is reduced as in `TwoPass`.
    Running,
    /// Tick columns reduced as exact integers (`exact::reduce_ticks`, D-6.3,
    /// ST-3); a float column, which has no integer form, is reduced as in
    /// `TwoPass`.
    Exact,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::TwoPass => "two_pass",
            Mode::Running => "running",
            Mode::Exact => "exact",
        }
    }

    /// The mode a name denotes.
    pub fn parse(name: &str) -> Option<Mode> {
        Some(match name {
            "two_pass" => Mode::TwoPass,
            "running" => Mode::Running,
            "exact" => Mode::Exact,
            _ => return None,
        })
    }
}

impl Function {
    /// The function a name denotes; `quantile` takes its level separately.
    pub fn parse(name: &str, level: Option<f64>) -> Result<Function, AggError> {
        Ok(match name {
            "sum" => Function::Sum,
            "count" => Function::Count,
            "mean" => Function::Mean,
            "var" => Function::Var,
            "std" => Function::Std,
            "min" => Function::Min,
            "max" => Function::Max,
            "first" => Function::First,
            "last" => Function::Last,
            "median" => Function::Median,
            "quantile" => Function::Quantile(level.ok_or(AggError::NoLevel)?),
            "cov" => Function::Cov,
            "corr" => Function::Corr,
            "ols_slope" => Function::OlsSlope,
            "ols_r" => Function::OlsR,
            "ols_beta" => Function::OlsBeta,
            other => return Err(AggError::Unknown(other.to_string())),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Function::Sum => "sum",
            Function::Count => "count",
            Function::Mean => "mean",
            Function::Var => "var",
            Function::Std => "std",
            Function::Min => "min",
            Function::Max => "max",
            Function::First => "first",
            Function::Last => "last",
            Function::Median => "median",
            Function::Quantile(_) => "quantile",
            Function::Cov => "cov",
            Function::Corr => "corr",
            Function::OlsSlope => "ols_slope",
            Function::OlsR => "ols_r",
            Function::OlsBeta => "ols_beta",
        }
    }

    pub fn class(self) -> Class {
        match self {
            Function::Sum | Function::Count | Function::Min | Function::Max | Function::First | Function::Last => Class::Distributive,
            Function::Median | Function::Quantile(_) => Class::Holistic,
            _ => Class::Algebraic,
        }
    }

    /// The columns the function reads: two for the bivariate moments.
    pub fn arity(self) -> usize {
        match self {
            Function::Cov | Function::Corr | Function::OlsSlope | Function::OlsR | Function::OlsBeta => 2,
            _ => 1,
        }
    }

    /// The functions running sums answer (the moments and the sum).
    pub fn running(self) -> bool {
        matches!(
            self,
            Function::Mean | Function::Sum | Function::Std | Function::Cov | Function::Corr | Function::OlsSlope | Function::OlsR | Function::OlsBeta
        )
    }
}

/// A partial point (P-1.2): the window holds a value the function is
/// undefined at. The caller treats it as `⊥` or as a halt, per its rules.
#[derive(Clone, Debug, PartialEq)]
pub enum AggError {
    /// One observation for a dispersion or a regression.
    FewerThanTwo(Function),
    /// A constant series where the function divides by its spread.
    Constant(Function),
    /// `quantile` without a level.
    NoLevel,
    /// A quantile level outside [0, 1].
    LevelOutside(f64),
    Unknown(String),
}

impl fmt::Display for AggError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AggError::FewerThanTwo(Function::Std) => write!(f, "std of fewer than two observations"),
            AggError::FewerThanTwo(_) => write!(f, "covariance of fewer than two observations"),
            AggError::Constant(Function::Corr) => write!(f, "correlation of a constant series"),
            AggError::Constant(Function::OlsSlope | Function::OlsBeta) => write!(f, "ols_slope against a constant regressor"),
            AggError::Constant(Function::OlsR) => write!(f, "ols_r of a constant series"),
            AggError::Constant(g) => write!(f, "{} of a constant series", g.name()),
            AggError::NoLevel => write!(f, "quantile needs a level"),
            AggError::LevelOutside(q) => write!(f, "quantile level {} is outside [0, 1]", q),
            AggError::Unknown(name) => write!(f, "unknown aggregate `{}`", name),
        }
    }
}

impl std::error::Error for AggError {}
