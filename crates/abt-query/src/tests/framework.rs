//! The test framework (Q-4.1): a statistical test is a pure function of its
//! input tables, its parameters and a seed, registered by name, declaring its
//! null hypothesis and the dependence treatment its p-value assumes (Q-4.2).
//! A result is reproducible to the bit: resampling draws from the
//! framework's generator (`rng`), keyed by (seed, replication).

use std::collections::BTreeMap;
use std::fmt;

pub use super::dependence::{ClusterBy, Dependence};

/// What shape an input table has (Q-4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// One value per time: a return series, a spread.
    Series,
    /// Entity × time: a feature and the forward returns of a universe.
    Panel,
    /// One value per entity at one time.
    CrossSection,
    /// Rows without a time order: a grid's outcomes, a contingency table.
    Rows,
}

/// An input a test takes: a name, a shape, the columns it reads.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct InputSpec {
    pub name: &'static str,
    pub shape: Shape,
    /// The value columns, by name.
    pub columns: &'static [&'static str],
}

/// The type of a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Int,
    Float,
    Bool,
    Text,
}

/// A parameter a test takes, with its default and range.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ParamSpec {
    pub name: &'static str,
    pub kind: ParamKind,
    pub default: Option<Param>,
    /// Inclusive bounds of a numeric parameter.
    pub range: Option<(f64, f64)>,
    pub doc: &'static str,
}

/// A parameter's value.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Param {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl fmt::Display for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Param::Int(x) => write!(f, "{}", x),
            Param::Float(x) => write!(f, "{}", x),
            Param::Bool(x) => write!(f, "{}", x),
            Param::Text(x) => write!(f, "{}", x),
        }
    }
}

/// The parameters of one application, by name.
pub type Params = BTreeMap<String, Param>;

/// A test's result (Q-4.1).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TestResult {
    pub statistic: f64,
    pub p_value: Option<f64>,
    /// (lo, hi, level).
    pub interval: Option<(f64, f64, f64)>,
    pub effect: Option<f64>,
    pub df: Option<f64>,
    pub n: usize,
    pub notes: Vec<String>,
}

/// Why a test gave no result: its input is unfit, a precondition fails, or
/// the computation broke down (a singular design).
#[derive(Clone, Debug, PartialEq)]
pub enum TestError {
    Input(String),
    Precondition(String),
    Numeric(String),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Input(m) => write!(f, "input: {}", m),
            TestError::Precondition(m) => write!(f, "precondition: {}", m),
            TestError::Numeric(m) => write!(f, "numeric: {}", m),
        }
    }
}

impl std::error::Error for TestError {}

/// An input table: named numeric columns of one length, with the time and
/// the entity of each row when the shape has them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub time: Option<Vec<i64>>,
    pub entity: Option<Vec<u64>>,
    pub columns: BTreeMap<String, Vec<f64>>,
}

impl Table {
    /// A series of one column.
    pub fn series(name: &str, values: Vec<f64>) -> Table {
        let mut columns = BTreeMap::new();
        columns.insert(name.to_string(), values);
        Table { time: None, entity: None, columns }
    }

    pub fn len(&self) -> usize {
        self.columns.values().next().map(|c| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A column by name.
    pub fn column(&self, name: &str) -> Result<&[f64], TestError> {
        self.columns.get(name).map(|c| c.as_slice()).ok_or_else(|| TestError::Input(format!("no column `{}`", name)))
    }

    /// The first column (a series' only one).
    pub fn first(&self) -> Result<&[f64], TestError> {
        self.columns.values().next().map(|c| c.as_slice()).ok_or_else(|| TestError::Input("an empty table".into()))
    }

    /// The rows of a panel by bar: each bar's time with its rows, the bars
    /// in time order and each bar's rows in entity order, so that a
    /// statistic summed over a bar does not depend on the order the rows
    /// came in. A table without `time` and `entity`, whose columns or
    /// vectors differ in length, or with an entity twice in one bar is
    /// refused.
    pub fn bars(&self) -> Result<Vec<(i64, Vec<usize>)>, TestError> {
        let (Some(time), Some(entity)) = (&self.time, &self.entity) else {
            return Err(TestError::Input("a panel needs the `time` and the `entity` of every row".into()));
        };
        let n = self.len();
        if time.len() != n || entity.len() != n || self.columns.values().any(|c| c.len() != n) {
            return Err(TestError::Input("the columns, `time` and `entity` of a panel differ in length".into()));
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (time[i], entity[i]));
        let mut bars: Vec<(i64, Vec<usize>)> = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            if k > 0 && time[order[k - 1]] == time[i] && entity[order[k - 1]] == entity[i] {
                return Err(TestError::Input(format!("entity {} has two rows at time {}", entity[i], time[i])));
            }
            match bars.last_mut() {
                Some((t, rows)) if *t == time[i] => rows.push(i),
                _ => bars.push((time[i], vec![i])),
            }
        }
        Ok(bars)
    }
}

/// A statistical test (Q-4.1).
pub trait Test: Send + Sync {
    /// The registry key: lowercase and stable.
    fn name(&self) -> &str;
    /// Changes when the statistic or its algorithm changes; recorded with
    /// every result.
    fn version(&self) -> &str;
    fn inputs(&self) -> &[InputSpec];
    fn params(&self) -> &[ParamSpec];
    /// The null hypothesis, in one sentence.
    fn null(&self) -> &str;
    fn dependence(&self) -> Dependence;
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError>;
}

/// A parameter's value, or its declared default.
pub fn param<'a>(test: &'a dyn Test, params: &'a Params, name: &str) -> Option<&'a Param> {
    params.get(name).or_else(|| test.params().iter().find(|p| p.name == name).and_then(|p| p.default.as_ref()))
}

pub fn param_f64(test: &dyn Test, params: &Params, name: &str) -> Option<f64> {
    match param(test, params, name)? {
        Param::Float(x) => Some(*x),
        Param::Int(x) => Some(*x as f64),
        _ => None,
    }
}

pub fn param_usize(test: &dyn Test, params: &Params, name: &str) -> Option<usize> {
    match param(test, params, name)? {
        Param::Int(x) if *x >= 0 => Some(*x as usize),
        Param::Float(x) if *x >= 0.0 && x.fract() == 0.0 => Some(*x as usize),
        _ => None,
    }
}
