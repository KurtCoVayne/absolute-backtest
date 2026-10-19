//! `coactivation` (Q-4.4, Q-7): whether two rules fire independently, on
//! the 2×2 table of their firing over the attempted instances (an entity
//! at a bar where both rules were evaluated). The table is measured as
//! CORDS (Ilyas et al. 2004) measures the dependence of two columns: by
//! phi, the correlation of the two firings; by Jaccard's index, the share
//! of the instances where either fires on which both do; and by the mutual
//! information, in bits. The instances of one bar share its conditions, so
//! the p-value is clustered by time (E-21): the second rule's firing is
//! permuted among the instances of each bar, which keeps every bar's
//! firing counts and breaks only the association within bars; on one bar
//! it is the exact permutation test of Q-4.2. Phi carries a
//! stationary-bootstrap interval over bars (Q-7: no bare p-values).

use std::collections::BTreeMap;

use crate::tests::framework::{param, param_f64, param_usize, ClusterBy, Dependence, InputSpec, Param, ParamKind, ParamSpec, Params, Shape, Table, Test, TestError, TestResult};
use crate::tests::resample::{politis_white_block, quantile, stationary_resample};
use crate::tests::rng::Rng;

pub struct Coactivation;

/// The fewest bars over which phi's bootstrap interval is drawn: with
/// fewer, resampling bars says little about the variability of phi.
pub const MIN_BARS_FOR_INTERVAL: usize = 10;

const INPUTS: [InputSpec; 1] = [InputSpec {
    name: "instances",
    shape: Shape::Panel,
    columns: &["rule..."],
}];

const PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        name: "rules",
        kind: ParamKind::Text,
        default: None,
        range: None,
        doc: "the two rules' columns, as `a,b` or `(a, b)` (default the table's two columns)",
    },
    ParamSpec {
        name: "reps",
        kind: ParamKind::Int,
        default: Some(Param::Int(2000)),
        range: Some((1.0, 1e7)),
        doc: "the permutations of the p-value, and the resamples of the interval",
    },
    ParamSpec {
        name: "block",
        kind: ParamKind::Float,
        default: None,
        range: Some((1.0, 1e9)),
        doc: "the mean block, in bars, of the stationary bootstrap (default Politis and White's)",
    },
    ParamSpec {
        name: "level",
        kind: ParamKind::Float,
        default: Some(Param::Float(0.95)),
        range: Some((0.5, 0.9999)),
        doc: "the confidence level of the interval on phi",
    },
];

/// The association of two rules' firing on their 2×2 table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Association {
    /// The correlation of the two firings: (n11 n00 − n10 n01) /
    /// √(n1· n0· n·1 n·0).
    pub phi: f64,
    /// n11 / (n11 + n10 + n01).
    pub jaccard: f64,
    /// Σ p log₂(p / (p_a p_b)) over the four cells, in bits.
    pub mutual_information: f64,
    /// P(the second fires | the first fires), n11 / n1·.
    pub second_given_first: f64,
    /// P(the first fires | the second fires), n11 / n·1.
    pub first_given_second: f64,
}

/// The association of a 2×2 table `[n11, n10, n01, n00]`, where n10
/// counts the instances on which the first rule fires and the second does
/// not; a rule that fires on every instance or on none has none.
pub fn association(table: [u64; 4]) -> Result<Association, TestError> {
    let [n11, n10, n01, n00] = table;
    let (a1, a0, b1, b0) = (n11 + n10, n01 + n00, n11 + n01, n10 + n00);
    let n = a1 + a0;
    if a1 == 0 || a0 == 0 || b1 == 0 || b0 == 0 {
        return Err(TestError::Precondition(format!(
            "the first rule fires on {} and the second on {} of {} instances: a rule that fires always or never has no association to measure",
            a1, b1, n
        )));
    }
    let det = n11 as i128 * n00 as i128 - n10 as i128 * n01 as i128;
    let phi = det as f64 / (a1 as f64 * a0 as f64 * b1 as f64 * b0 as f64).sqrt();
    let nf = n as f64;
    let cell = |c: u64, row: u64, col: u64| if c == 0 { 0.0 } else { c as f64 / nf * (c as f64 * nf / (row as f64 * col as f64)).log2() };
    Ok(Association {
        phi,
        jaccard: n11 as f64 / (n11 + n10 + n01) as f64,
        mutual_information: cell(n11, a1, b1) + cell(n10, a1, b0) + cell(n01, a0, b1) + cell(n00, a0, b0),
        second_given_first: n11 as f64 / a1 as f64,
        first_given_second: n11 as f64 / b1 as f64,
    })
}

impl Test for Coactivation {
    fn name(&self) -> &str {
        "coactivation"
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
        "the two rules fire independently of each other within every bar"
    }
    fn dependence(&self) -> Dependence {
        Dependence::Clustered { by: ClusterBy::Time }
    }
    /// One row per attempted instance, with its bar in `time` and each
    /// rule's firing as 1 or 0; a row where either rule has no value was
    /// not attempted by both and is left out. The statistic and the effect
    /// are phi. The p-value is two-sided, the smaller tail of the
    /// within-bar permutation distribution doubled, each tail counted as
    /// (1 + exceedances) / (1 + reps); replication r draws from
    /// `Rng::stream(seed, r)`. The interval is the percentile interval of
    /// phi over `reps` stationary resamples of the bars, replication r
    /// drawing from `Rng::stream(seed, reps + r)`, and is given from
    /// `MIN_BARS_FOR_INTERVAL` bars; `n` is the number of instances, and
    /// the notes carry the table, Jaccard's index and the mutual
    /// information.
    fn run(&self, tables: &[Table], params: &Params, seed: u64) -> Result<TestResult, TestError> {
        let t = tables.first().ok_or_else(|| TestError::Input("coactivation takes one table of instances".into()))?;
        let (first, second) = rules(t, param(self, params, "rules"))?;
        let a = t.column(&first)?;
        let b = t.column(&second)?;
        let time = t.time.as_ref().ok_or_else(|| TestError::Input("coactivation needs the bar (`time`) of every instance".into()))?;
        if a.len() != time.len() || b.len() != time.len() {
            return Err(TestError::Input("the rules' columns and `time` differ in length".into()));
        }
        let reps = match param_usize(self, params, "reps") {
            Some(r) if r > 0 => r,
            _ => return Err(TestError::Input("`reps` is a positive integer".into())),
        };
        let block = match param_f64(self, params, "block") {
            Some(b) if b >= 1.0 => Some(b),
            None if !params.contains_key("block") => None,
            _ => return Err(TestError::Input("`block` is a mean block length of at least one bar".into())),
        };
        let level = param_f64(self, params, "level")
            .filter(|l| *l > 0.0 && *l < 1.0)
            .ok_or_else(|| TestError::Input("`level` is a number between 0 and 1".into()))?;
        let mut by_bar: BTreeMap<i64, [u64; 4]> = BTreeMap::new();
        let mut unattempted = 0usize;
        for i in 0..time.len() {
            if !a[i].is_finite() || !b[i].is_finite() {
                unattempted += 1;
                continue;
            }
            let cell = match (fired(a[i], &first, i)?, fired(b[i], &second, i)?) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            };
            by_bar.entry(time[i]).or_insert([0; 4])[cell] += 1;
        }
        let bars: Vec<[u64; 4]> = by_bar.into_values().collect();
        let mut table = [0u64; 4];
        for c in &bars {
            table.iter_mut().zip(c).for_each(|(s, v)| *s += v);
        }
        let observed = association(table)?;
        let n = table.iter().sum::<u64>();
        // The permutation distribution of n11, of which phi is an
        // increasing function when the margins are held, as every
        // within-bar permutation holds them.
        let (mut above, mut below) = (0usize, 0usize);
        for r in 0..reps {
            let mut rng = Rng::stream(seed, r as u64);
            let n11: u64 = bars.iter().map(|c| permuted_cofiring(c, &mut rng)).sum();
            above += usize::from(n11 >= table[0]);
            below += usize::from(n11 <= table[0]);
        }
        let tail = |k: usize| (1 + k) as f64 / (1 + reps) as f64;
        let p_value = (2.0 * tail(above).min(tail(below))).min(1.0);
        let mut notes = vec![
            format!("the 2×2 table: both {}, only `{}` {}, only `{}` {}, neither {}", table[0], first, table[1], second, table[2], table[3]),
            format!(
                "Jaccard's index {}, mutual information {} bits, P(`{}` | `{}`) {}, P(`{}` | `{}`) {}",
                observed.jaccard, observed.mutual_information, second, first, observed.second_given_first, first, second, observed.first_given_second
            ),
            format!("{} instances over {} bars, {} rows not attempted by both rules", n, bars.len(), unattempted),
            format!("p-value from {} permutations of `{}` within bars", reps, second),
        ];
        let interval = if bars.len() >= MIN_BARS_FOR_INTERVAL {
            let (abar, bbar) = ((table[0] + table[1]) as f64 / n as f64, (table[0] + table[2]) as f64 / n as f64);
            // Each bar's share of the covariance of the two firings, whose
            // persistence sets the bootstrap's block.
            let share: Vec<f64> = bars
                .iter()
                .map(|c| {
                    let (fa, fb, m) = ((c[0] + c[1]) as f64, (c[0] + c[2]) as f64, c.iter().sum::<u64>() as f64);
                    c[0] as f64 - fa * bbar - fb * abar + m * abar * bbar
                })
                .collect();
            let block = block.unwrap_or_else(|| politis_white_block(&share));
            let index: Vec<f64> = (0..bars.len()).map(|k| k as f64).collect();
            let mut drawn = Vec::with_capacity(bars.len());
            let mut phis = Vec::with_capacity(reps);
            for r in 0..reps {
                let mut rng = Rng::stream(seed, (reps + r) as u64);
                stationary_resample(&index, block, &mut rng, &mut drawn);
                let mut resampled = [0u64; 4];
                for &k in &drawn {
                    resampled.iter_mut().zip(&bars[k as usize]).for_each(|(s, v)| *s += v);
                }
                if let Ok(s) = association(resampled) {
                    phis.push(s.phi);
                }
            }
            phis.sort_by(f64::total_cmp);
            if 2 * phis.len() >= reps {
                notes.push(format!("interval from {} of {} stationary resamples of the bars, mean block {} bars", phis.len(), reps, block));
                Some((quantile(&phis, (1.0 - level) / 2.0), quantile(&phis, (1.0 + level) / 2.0), level))
            } else {
                notes.push(format!("no interval: in {} of {} resamples of the bars a rule fires always or never", reps - phis.len(), reps));
                None
            }
        } else {
            notes.push(format!("no interval: {} bars, fewer than the {} a resample of bars needs", bars.len(), MIN_BARS_FOR_INTERVAL));
            None
        };
        Ok(TestResult {
            statistic: observed.phi,
            p_value: Some(p_value),
            interval,
            effect: Some(observed.phi),
            df: None,
            n: n as usize,
            notes,
        })
    }
}

/// The two rules a test of co-activation reads: the `rules` parameter's,
/// or the table's columns when it has exactly two.
fn rules(t: &Table, given: Option<&Param>) -> Result<(String, String), TestError> {
    let names: Vec<String> = match given {
        Some(Param::Text(s)) => s
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split(',')
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect(),
        Some(other) => return Err(TestError::Input(format!("`rules` names two columns, as `a,b`, not {}", other))),
        None => t.columns.keys().cloned().collect(),
    };
    match names.as_slice() {
        [a, b] if a != b => Ok((a.clone(), b.clone())),
        _ => Err(TestError::Input(format!(
            "coactivation compares two different rules; name them with `rules` (the table has {})",
            t.columns.keys().cloned().collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Whether a rule fired on an instance: its value is 1 or 0.
fn fired(v: f64, rule: &str, row: usize) -> Result<bool, TestError> {
    if v == 1.0 {
        Ok(true)
    } else if v == 0.0 {
        Ok(false)
    } else {
        Err(TestError::Input(format!("rule `{}` has the value {} on row {}: a firing is 1 or 0", rule, v, row)))
    }
}

/// One bar's co-firing count with the second rule's firing permuted among
/// the bar's instances: a hypergeometric draw, made as the draws without
/// replacement it is, the smaller of the two firing counts drawn from a
/// population holding the larger (the distribution is symmetric in them).
fn permuted_cofiring(c: &[u64; 4], rng: &mut Rng) -> u64 {
    let (fa, fb) = (c[0] + c[1], c[0] + c[2]);
    let (draws, mut successes) = (fa.min(fb), fa.max(fb));
    let mut remaining = c.iter().sum::<u64>();
    let mut hits = 0;
    for d in 0..draws {
        if successes == 0 {
            break;
        }
        if successes == remaining {
            return hits + (draws - d);
        }
        if (rng.below(remaining as usize) as u64) < successes {
            hits += 1;
            successes -= 1;
        }
        remaining -= 1;
    }
    hits
}
