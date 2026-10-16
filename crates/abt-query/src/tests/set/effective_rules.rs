//! `effective_rules` (Q-4.4, Q-7): the effective number of independent
//! rules, from the eigenvalues λ of the correlation matrix of the rules'
//! activations, defined as their participation ratio (Σλ)² / Σλ². It is n
//! for n uncorrelated rules and 1 for rules that always fire together;
//! between, it is the number of equal eigenvalues that would spread the
//! activations' variance as evenly. For a correlation matrix Σλ is the
//! trace n and Σλ² the sum of the squared correlations, so the ratio is n²
//! over that sum. Descriptive: there is no null and no p-value.

use crate::tests::framework::{param, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::linalg::{symmetric_eigen, Matrix};

pub struct EffectiveRules;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "activations",
    shape: Shape::Rows,
    columns: &["rule..."],
}];

const PARAMS: [ParamSpec; 1] = [ParamSpec {
    name: "rules",
    kind: ParamKind::Text,
    default: None,
    range: None,
    doc: "the rules' columns, as `a,b,...` (default every column)",
}];

/// The participation ratio (Σλ)² / Σλ² of the eigenvalues of a
/// correlation matrix, with the eigenvalues in descending order.
pub fn effective_number(correlation: &Matrix) -> Result<(f64, Vec<f64>), TestError> {
    if correlation.rows != correlation.cols || correlation.rows == 0 {
        return Err(TestError::Input(format!("a correlation matrix is square, not {}×{}", correlation.rows, correlation.cols)));
    }
    if correlation.data.iter().any(|v| !v.is_finite()) {
        return Err(TestError::Input("the correlation matrix has a value that is not finite".into()));
    }
    let n = correlation.rows;
    if (0..n).any(|i| (0..i).any(|j| (correlation[(i, j)] - correlation[(j, i)]).abs() > 1e-12)) {
        return Err(TestError::Input("a correlation matrix is symmetric".into()));
    }
    let (values, _) = symmetric_eigen(correlation);
    let sum: f64 = values.iter().sum();
    let squares: f64 = values.iter().map(|l| l * l).sum();
    if squares <= 0.0 {
        return Err(TestError::Numeric("the eigenvalues of the correlation matrix are all zero".into()));
    }
    Ok((sum * sum / squares, values))
}

impl Test for EffectiveRules {
    fn name(&self) -> &str {
        "effective_rules"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn inputs(&self) -> &[InputSpec] {
        &INPUTS
    }
    fn params(&self) -> &[ParamSpec] {
        &PARAMS
    }
    fn null(&self) -> &str {
        "none: the effective number of independent rules is a description"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Descriptive
    }
    /// One row per instance (or per bar), one column per rule, its
    /// activation (a firing as 1 or 0, or a rate); a row with a missing
    /// value is left out, and a rule whose activation never varies has no
    /// correlation and is left out with a note. The statistic is the
    /// effective number of the rules left; `n` is the number of rows.
    fn run(&self, tables: &[Table], params: &Params, _seed: u64) -> Result<TestResult, TestError> {
        let t = tables.first().ok_or_else(|| TestError::Input("effective_rules takes one table of activations".into()))?;
        let names: Vec<String> = match param(self, params, "rules") {
            Some(Param::Text(s)) => s
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .split(',')
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .collect(),
            Some(other) => return Err(TestError::Input(format!("`rules` names columns, as `a,b,...`, not {}", other))),
            None => t.columns.keys().cloned().collect(),
        };
        if names.is_empty() {
            return Err(TestError::Input("effective_rules needs at least one rule".into()));
        }
        let columns: Vec<&[f64]> = names.iter().map(|n| t.column(n)).collect::<Result<_, _>>()?;
        let len = columns[0].len();
        if columns.iter().any(|c| c.len() != len) {
            return Err(TestError::Input("the rules' columns differ in length".into()));
        }
        let rows: Vec<usize> = (0..len).filter(|&i| columns.iter().all(|c| c[i].is_finite())).collect();
        if rows.len() < 2 {
            return Err(TestError::Precondition(format!("{} rows with every rule's activation; a correlation needs two", rows.len())));
        }
        // Each rule's deviations from its mean, and their norm.
        let mut kept: Vec<(&str, Vec<f64>, f64)> = Vec::new();
        let mut constant: Vec<&str> = Vec::new();
        for (name, c) in names.iter().zip(&columns) {
            let mean = rows.iter().map(|&i| c[i]).sum::<f64>() / rows.len() as f64;
            let d: Vec<f64> = rows.iter().map(|&i| c[i] - mean).collect();
            let norm = d.iter().map(|v| v * v).sum::<f64>().sqrt();
            if norm > 0.0 {
                kept.push((name, d, norm));
            } else {
                constant.push(name);
            }
        }
        if kept.is_empty() {
            return Err(TestError::Precondition(format!("no rule's activation varies over the {} rows", rows.len())));
        }
        let k = kept.len();
        let mut correlation = Matrix::identity(k);
        for i in 0..k {
            for j in (i + 1)..k {
                let r = kept[i].1.iter().zip(&kept[j].1).map(|(x, y)| x * y).sum::<f64>() / (kept[i].2 * kept[j].2);
                correlation[(i, j)] = r;
                correlation[(j, i)] = r;
            }
        }
        let (effective, values) = effective_number(&correlation)?;
        let mut notes = vec![
            format!("{} rules: {}", k, kept.iter().map(|(n, _, _)| *n).collect::<Vec<_>>().join(", ")),
            format!("eigenvalues of their correlation matrix: {}", values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")),
            "the effective number is the participation ratio (Σλ)² / Σλ²".to_string(),
        ];
        if !constant.is_empty() {
            notes.push(format!("left out, their activation constant: {}", constant.join(", ")));
        }
        if rows.len() < len {
            notes.push(format!("{} rows with a missing activation left out", len - rows.len()));
        }
        Ok(TestResult {
            statistic: effective,
            p_value: None,
            interval: None,
            effect: None,
            df: None,
            n: rows.len(),
            notes,
        })
    }
}
