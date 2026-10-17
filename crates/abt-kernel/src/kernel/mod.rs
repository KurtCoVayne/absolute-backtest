//! The kernel: evaluation of a checked program over the data its demand
//! binds, in the closed executor loop of section 7. Evaluation is top-down with
//! memoisation; every derived tuple is requested at a bound temporal key, and
//! WF-4 and WF-8 guarantee that every such request terminates.

pub mod eval;
pub(crate) mod eval_set;
pub mod time;
pub mod value;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use hash::{FxHashMap, FxHashSet};

use crate::check::Program;
use crate::ir::*;
pub mod bind;
pub mod checkpoints;
pub mod events;

pub mod fold;
pub mod hash;
pub mod outputs;
pub mod pushdown;
pub mod service;
pub use crate::sim::{Executor, SimExecutor};
pub use bind::{bind, connect_dataset, install_provider, Bound, Connector, DataClient};
pub use events::{Book, EventKind, OrderId, RunEvent};
pub use fold::{Checkpoint, CheckpointEvery, Event, Fold};
pub use value::{Ctor, Decision, Order, OrderKind, Sym, Symbols, Tif, Value};

pub type Tuple = Vec<Value>;
/// Memo key: relation id, then the key time and the input values.
pub(crate) type MemoKey = (usize, Vec<Value>);

/// The memo of derived results, bucketed by the temporal key so that
/// dropping the past costs only what is dropped; the latest old result of
/// each relation read through `asof`, per inputs, is kept apart (a later bar
/// may read it however old).
#[derive(Default)]
pub(crate) struct Memo {
    by_time: BTreeMap<i64, FxHashMap<MemoKey, Derived>>,
    latest: FxHashMap<(usize, Vec<Value>), (i64, Derived)>,
    len: usize,
}

impl Memo {
    pub(crate) fn time_of(key: &MemoKey) -> i64 {
        key.1.first().and_then(|v| v.as_time()).unwrap_or(i64::MAX)
    }

    pub(crate) fn get(&self, key: &MemoKey) -> Option<&Derived> {
        let t = Memo::time_of(key);
        if let Some(hit) = self.by_time.get(&t).and_then(|b| b.get(key)) {
            return Some(hit);
        }
        // Before the oldest bucket: only a kept latest result can answer.
        if !self.latest.is_empty() && self.by_time.first_key_value().map(|(k, _)| t < *k).unwrap_or(true) {
            if let Some((kt, d)) = self.latest.get(&(key.0, key.1[1..].to_vec())) {
                if *kt == t {
                    return Some(d);
                }
            }
        }
        None
    }

    pub(crate) fn insert(&mut self, key: MemoKey, d: Derived) {
        if self.by_time.entry(Memo::time_of(&key)).or_default().insert(key, d).is_none() {
            self.len += 1;
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Memo::default();
    }

    pub(crate) fn len(&self) -> usize {
        self.len + self.latest.len()
    }

    /// Drop every result keyed before `cutoff`, keeping the latest of each
    /// `keep` relation per inputs.
    fn evict_before(&mut self, cutoff: i64, keep: &FxHashSet<usize>) {
        let recent = self.by_time.split_off(&cutoff);
        let old = std::mem::replace(&mut self.by_time, recent);
        for (t, bucket) in old {
            self.len -= bucket.len();
            for (k, d) in bucket {
                if !keep.contains(&k.0) {
                    continue;
                }
                let ins = k.1[1..].to_vec();
                match self.latest.get_mut(&(k.0, ins.clone())) {
                    Some(slot) if slot.0 < t => *slot = (t, d),
                    Some(_) => {}
                    None => {
                        self.latest.insert((k.0, ins), (t, d));
                    }
                }
            }
        }
    }
}
pub(crate) type Derived = Rc<Vec<(Tuple, usize)>>;
/// The instances a call left stuck: partial head tuples with their errors.
pub(crate) type StuckSet = Vec<(Vec<Option<Value>>, RunError)>;

/// One row of the bundle's security table (data-bundle doc, section 3,
/// "Identity"): the security `id` carried `ticker` from `from` (inclusive)
/// to `to` (exclusive; `None` is still).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Security {
    pub id: Sym,
    pub ticker: String,
    pub from: i64,
    pub to: Option<i64>,
}

/// The security carrying `ticker` at `as_of`, in a table.
pub fn resolve_ticker(securities: &[Security], ticker: &str, as_of: i64) -> Result<Sym, String> {
    securities
        .iter()
        .find(|s| s.ticker == ticker && s.from <= as_of && s.to.map(|to| as_of < to).unwrap_or(true))
        .map(|s| s.id)
        .ok_or_else(|| {
            format!(
                "`{}` is not a ticker of the bundle as of {} (a ticker literal names the security carrying it at the bundle date)",
                ticker,
                time::format_timestamp(as_of)
            )
        })
}

/// Primitive facts: an environment instance E (section 7).
#[derive(Clone, Debug, Default)]
pub struct Dataset {
    pub symbols: Symbols,
    /// Labels (`Ty::Label`), interned apart from the equities.
    pub labels: Symbols,
    pub facts: BTreeMap<String, Vec<Tuple>>,
    /// Availability time per tuple, parallel to `facts` (data-bundle doc,
    /// section 2): absent for a relation whose tuples are available at their
    /// own bar's close, the labelling convention.
    pub available_at: BTreeMap<String, Vec<i64>>,
    /// The security table: stable ids and their ticker history. Empty for
    /// a dataset without one, where the ticker is the id.
    pub securities: Vec<Security>,
    /// The bundle date a ticker literal resolves at; the last bar when unset.
    pub as_of: Option<i64>,
    /// Contract terms of the securities that are not plain shares (a
    /// future's multiplier and per-contract commission); a security absent
    /// here is a share with multiplier 1.
    pub contracts: BTreeMap<Sym, Contract>,
    /// No reviewer vouched for the data (D-2.1's `reviewed` attribute): a
    /// bundle whose tests have not passed, a loose Parquet directory. A
    /// dataset the generators or a test build is complete by construction.
    pub unreviewed: bool,
    /// The bundle version it was loaded from (the `version` a demand's
    /// `prefer version V` names).
    pub version: Option<String>,
}

/// The contract terms of a security (data-bundle doc, section 3, the
/// security table's contract columns).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Contract {
    /// Currency per point per contract: P&L is quantity x price change x multiplier.
    pub multiplier: f64,
    /// A future: its (back-adjusted) price may be zero or negative.
    pub future: bool,
    /// Commission per contract per side, replacing the per-share schedule.
    pub commission_per_contract: Option<f64>,
    /// An option's terms (D-2.4 `option_spec`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<OptionTerms>,
    /// A futures contract's terms (D-2.4 `future_spec`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub future_spec: Option<FutureSpec>,
}

/// An option's terms (D-2.4 `option_spec`, the kernel's `spec` of K-2.4),
/// adjusted by its underlying's corporate actions from their ex-dates
/// (K-5.5.5) once the book holds it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OptionTerms {
    pub underlying: Sym,
    pub right: crate::functions::Right,
    pub strike: f64,
    /// The expiry date, at midnight UTC of its day.
    pub expiry: i64,
    pub style: crate::functions::Style,
    /// Settled in cash (an index option) rather than by delivery.
    pub cash: bool,
    /// Settled on the expiry day's opening prints rather than its close.
    pub am: bool,
    /// Units of the underlying a contract delivers.
    pub multiplier: f64,
    pub occ: String,
    /// When the terms were first known.
    pub listed: i64,
}

impl OptionTerms {
    /// The intrinsic value of a unit at the underlying's price `s`.
    pub fn intrinsic(&self, s: f64) -> f64 {
        (self.right.sign() * (s - self.strike)).max(0.0)
    }

    /// The instant the option expires at, which its time to expiry runs to:
    /// the expiry day's open (AM) or its end (PM), ERRATA E-71.
    pub fn expires_at(&self) -> i64 {
        if self.am {
            self.expiry
        } else {
            self.expiry + time::DAY
        }
    }
}

/// A futures contract's terms (D-2.4 `future_spec`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FutureSpec {
    pub root: String,
    pub expiry: i64,
    pub last_trade: Option<i64>,
    pub point_value: f64,
    /// Settled in cash at expiry rather than by delivery.
    pub cash: bool,
}

impl Default for Contract {
    fn default() -> Contract {
        Contract {
            multiplier: 1.0,
            future: false,
            commission_per_contract: None,
            option: None,
            future_spec: None,
        }
    }
}

impl Dataset {
    pub fn new() -> Dataset {
        Dataset::default()
    }
    pub fn intern(&mut self, name: &str) -> Sym {
        self.symbols.intern(name)
    }
    /// The contract terms of a security (a share's when none were recorded).
    pub fn contract(&self, s: Sym) -> Contract {
        self.contracts.get(&s).cloned().unwrap_or_default()
    }
    pub fn is_future(&self, s: Sym) -> bool {
        self.contracts.get(&s).map(|c| c.future).unwrap_or(false)
    }
    /// Record that security `id` carried `ticker` over `[from, to)`.
    pub fn add_security(&mut self, id: &str, ticker: &str, from: i64, to: Option<i64>) -> Sym {
        let sym = self.symbols.intern(id);
        self.securities.push(Security {
            id: sym,
            ticker: ticker.to_string(),
            from,
            to,
        });
        sym
    }
    /// The last temporal key of any fact.
    pub fn last_bar(&self) -> Option<i64> {
        self.facts.values().flatten().filter_map(|tu| tu.iter().find_map(|v| v.as_time())).max()
    }
    /// The date a ticker literal resolves at: `as_of`, or the last bar.
    pub fn bundle_date(&self) -> Option<i64> {
        self.as_of.or_else(|| self.last_bar())
    }
    /// The security carrying `ticker` at the bundle date (or `as_of`).
    pub fn resolve_ticker(&self, ticker: &str, as_of: Option<i64>) -> Result<Sym, String> {
        let at = as_of.or_else(|| self.bundle_date()).unwrap_or(0);
        resolve_ticker(&self.securities, ticker, at)
    }
    /// Materialise `ticker(A, @T, S)` from the security table over the bars
    /// of `universe` (or of every fact), one row per security and bar in its
    /// interval; without a table every symbol is its own ticker. Replaces
    /// any `ticker` facts present.
    pub fn derive_tickers(&mut self) {
        let mut bars: BTreeSet<i64> = BTreeSet::new();
        let source = if self.facts.contains_key("universe") {
            vec!["universe".to_string()]
        } else {
            self.facts.keys().cloned().collect()
        };
        for rel in &source {
            for tu in &self.facts[rel] {
                bars.extend(tu.iter().find_map(|v| v.as_time()));
            }
        }
        let rows: Vec<(Sym, i64, String)> = if self.securities.is_empty() {
            let mut present: BTreeSet<(Sym, i64)> = BTreeSet::new();
            for rel in &source {
                for tu in &self.facts[rel] {
                    if let (Some(sym), Some(t)) = (tu.iter().find_map(|v| v.as_equity()), tu.iter().find_map(|v| v.as_time())) {
                        present.insert((sym, t));
                    }
                }
            }
            present.into_iter().map(|(sym, t)| (sym, t, self.symbols.name(sym).to_string())).collect()
        } else {
            let mut rows = Vec::new();
            for s in &self.securities {
                for &t in bars.range(s.from..) {
                    if s.to.map(|to| t >= to).unwrap_or(false) {
                        break;
                    }
                    rows.push((s.id, t, s.ticker.clone()));
                }
            }
            rows.sort_by_key(|r| (r.0, r.1));
            rows
        };
        let mut out = Vec::with_capacity(rows.len());
        for (sym, t, ticker) in rows {
            let label = self.labels.intern(&ticker);
            out.push(vec![Value::Equity(sym), Value::Time(t), Value::Label(label)]);
        }
        self.facts.insert("ticker".to_string(), out);
    }
    pub fn intern_label(&mut self, name: &str) -> Sym {
        self.labels.intern(name)
    }
    /// The spelling of a label value.
    pub fn label_name(&self, v: &Value) -> Option<&str> {
        match v {
            Value::Label(s) => Some(self.labels.name(*s)),
            _ => None,
        }
    }
    pub fn add(&mut self, relation: &str, tuple: Tuple) {
        let key = tuple.iter().find_map(|v| v.as_time());
        let n = self.facts.get(relation).map(|v| v.len()).unwrap_or(0);
        if let Some(avails) = self.available_at.get_mut(relation) {
            // The relation records availability: this tuple's is its bar.
            avails.push(key.unwrap_or(i64::MIN));
        } else if n == 0 {
            // Nothing to record yet.
        }
        self.facts.entry(relation.to_string()).or_default().push(tuple);
    }
    /// Add a tuple available at `avail` (at or after its bar's close); the
    /// relation then records availability for every tuple.
    pub fn add_available(&mut self, relation: &str, tuple: Tuple, avail: i64) {
        if !self.available_at.contains_key(relation) {
            let existing: Vec<i64> = self
                .facts
                .get(relation)
                .map(|tus| tus.iter().map(|tu| tu.iter().find_map(|v| v.as_time()).unwrap_or(i64::MIN)).collect())
                .unwrap_or_default();
            self.available_at.insert(relation.to_string(), existing);
        }
        self.available_at.get_mut(relation).unwrap().push(avail);
        self.facts.entry(relation.to_string()).or_default().push(tuple);
    }
    /// Whether any relation records availability apart from its bars.
    pub fn has_availability(&self) -> bool {
        !self.available_at.is_empty()
    }
    pub fn availability_of(&self, relation: &str) -> Option<&[i64]> {
        self.available_at.get(relation).map(|v| v.as_slice())
    }
    /// When tuple `i` of `relation` is available: its recorded time, or the
    /// close of its bucket at `res` (its own bar).
    pub fn available(&self, relation: &str, i: usize, key: i64, res: Resolution) -> i64 {
        match self.available_at.get(relation).and_then(|v| v.get(i)) {
            Some(&a) if a != i64::MIN => a,
            _ => time::bucket_range(res, key).1.max(key),
        }
    }
}

/// What the executor does when a fill would borrow: cash would go negative,
/// or gross exposure (Σ |position| × price) would exceed equity (section 6,
/// executor policy). Orders that reduce exposure are never leverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OnLeverage {
    /// Halt the run naming the bar, the decision and its rule.
    #[default]
    Halt,
    /// Drop the order with a reason and continue.
    Reject,
    /// Fill it; cash may go negative.
    Allow,
}

/// What the executor does with a `sell` larger than the long position or a
/// `cover` larger than the short (an order that would cross zero).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OnOversize {
    #[default]
    Halt,
    /// Fill up to the position and drop the remainder with a reason.
    Clamp,
    /// Fill the signed order; the position crosses zero.
    Allow,
}

/// What the executor does when equity at the execution bar is not positive
/// while orders are pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OnRuin {
    #[default]
    Halt,
    /// Continue: `target_weight` sizes from an equity of zero (a positive
    /// weight targets flat) and the other policies still apply.
    Continue,
}

/// Rounding of every order quantity (section 6, executor policy). The
/// type system keeps `Quantity<Shares>` real-valued; this is the one place
/// a contract size could later apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Lot {
    /// Truncate toward zero to whole shares; an order that rounds to zero
    /// makes neither a fill nor a drop.
    #[default]
    Whole,
    Fractional,
}

impl std::str::FromStr for OnLeverage {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "halt" => Ok(OnLeverage::Halt),
            "reject" => Ok(OnLeverage::Reject),
            "allow" => Ok(OnLeverage::Allow),
            _ => Err(format!("`{}` is not one of halt, reject, allow", s)),
        }
    }
}

impl std::str::FromStr for OnOversize {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "halt" => Ok(OnOversize::Halt),
            "clamp" => Ok(OnOversize::Clamp),
            "allow" => Ok(OnOversize::Allow),
            _ => Err(format!("`{}` is not one of halt, clamp, allow", s)),
        }
    }
}

impl std::str::FromStr for OnRuin {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "halt" => Ok(OnRuin::Halt),
            "continue" => Ok(OnRuin::Continue),
            _ => Err(format!("`{}` is not one of halt, continue", s)),
        }
    }
}

impl std::str::FromStr for Lot {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "whole" => Ok(Lot::Whole),
            "fractional" => Ok(Lot::Fractional),
            _ => Err(format!("`{}` is not one of whole, fractional", s)),
        }
    }
}

/// What the executor does when, at a bar's mark, equity is positive but
/// below the maintenance margin of gross exposure (a margin call).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OnMarginCall {
    #[default]
    Halt,
    /// Sell the fraction of every position that restores maintenance, at
    /// the bar's close plus commission and fee; the fills are flagged forced.
    Liquidate,
    /// Carry on; the run reports how many calls it ignored.
    Allow,
}

impl std::str::FromStr for OnMarginCall {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "halt" => Ok(OnMarginCall::Halt),
            "liquidate" => Ok(OnMarginCall::Liquidate),
            "allow" => Ok(OnMarginCall::Allow),
            _ => Err(format!("`{}` is not one of halt, liquidate, allow", s)),
        }
    }
}

/// A borrow bucket (data-bundle doc, section 5, shorting and borrow
/// availability): instruments whose average daily volume is below
/// `adv_below` (and not below the previous bucket's) pay `fee_bps` a year
/// on their short notional, or cannot be shorted at all.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BorrowBucket {
    /// Infinity for the last bucket (JSON has no infinity: it is stored as null).
    #[serde(with = "infinite_as_null")]
    pub adv_below: f64,
    pub fee_bps: f64,
    pub shortable: bool,
}

/// A corporate action or delisting the executor applied to the book
/// (data-bundle doc, section 4: splits adjust positions on the ex-date,
/// dividends are credited on the pay date, a delisted name is force-closed
/// at its last trade with a haircut by reason).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    Split {
        factor: f64,
    },
    /// `amount` a share on `shares` held at the ex-date, credited at the pay date.
    Dividend {
        amount: f64,
        shares: f64,
    },
    Delisting {
        reason: String,
        haircut: f64,
    },
    /// `amount` a share on `shares` held at the ex-date, reinvested at the
    /// ex-date close in `added` shares of the same name.
    Reinvest {
        amount: f64,
        shares: f64,
        added: f64,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActionRecord {
    pub t: i64,
    pub equity: Sym,
    pub action: Action,
    /// The cash the action moved (a cashed fraction, a dividend, the
    /// proceeds of a forced close).
    pub cash: f64,
}

/// What the evaluator did, for tests of its incremental state.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KernelStats {
    /// Windowed aggregations evaluated.
    pub window_calls: usize,
    /// Bars whose rows a windowed aggregation solved (once per group with the cache).
    pub window_bars_solved: usize,
    /// Windowed aggregation groups the cache holds.
    pub window_groups: usize,
    /// Bars' rows the cache holds now.
    pub window_rows_cached: usize,
    /// Tuples that arrived after their bar closed (the fold only).
    pub late_tuples: usize,
    /// Derived results held in the memo at the end of the run.
    #[serde(default)]
    pub memo_entries: usize,
    /// Instances that met a partial operation and were excluded by a later
    /// literal (Kleene conjunction: never a halt).
    #[serde(default)]
    pub excluded_after_undefined: usize,
    /// Running-sums windows built at the bar asked for because the buffer
    /// no longer held the checkpoint's window (a departure from the
    /// canonical walk; zero in a run whose groups are asked for in order).
    #[serde(default)]
    pub window_noncanonical: usize,
    /// Rows-window requests behind a group's last bar (a query after the
    /// run), each served from a buffer rebuilt by walking back from its bar.
    #[serde(default)]
    pub window_backward: usize,
    /// Aggregate literals answered by the data layer's pass (K-4.5).
    #[serde(default)]
    pub pushed_answers: usize,
}

/// What the evaluator derived for one relation over a run: how many times
/// it was asked for (one call per bar and input tuple that some rule or the
/// decision needed), how many of those calls derived at least one tuple, and
/// the tuples derived in all. A derived relation with calls and no tuples was
/// demanded and never held: the empty link the never-fired report names.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelationCoverage {
    pub name: String,
    /// `true` for a relation defined by rules; a primitive's counts are of lookups.
    pub derived: bool,
    pub calls: usize,
    pub nonempty: usize,
    pub tuples: usize,
}

/// Serde for an `f64` that may be infinite (JSON has no infinity): null
/// stands for +∞.
mod infinite_as_null {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(x: &f64, s: S) -> Result<S::Ok, S::Error> {
        if x.is_finite() {
            s.serialize_some(x)
        } else {
            s.serialize_none()
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::INFINITY))
    }
}

/// Interest and fees accrued over a run, between consecutive decision bars
/// at the configured annual rates over calendar time.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FundingSummary {
    pub cash_interest: f64,
    pub margin_interest: f64,
    pub borrow_fees: f64,
    pub short_rebate: f64,
}

/// The book at a bar's mark, before that bar's decisions (K-5.1, the
/// book-level row of `kernel.<run>.book`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExposureRecord {
    pub t: i64,
    pub cash: f64,
    /// Σ |position| · price.
    pub gross: f64,
    /// Σ position · price.
    pub net: f64,
    /// The NAV: cash, the marked positions and the receivables.
    pub equity: f64,
    /// gross / equity (0 when equity is not positive).
    pub leverage: f64,
    /// Dividends gone ex and not yet paid.
    #[serde(default)]
    pub receivables: f64,
    /// What a weight is a fraction of: the equity, or the fixed capital.
    #[serde(default)]
    pub capital: f64,
    /// The requirement of the leverage model at the mark (K-5.6 `approx`
    /// until the margin models land): gross over the gross multiple to
    /// open, the maintenance fraction of gross to hold.
    #[serde(default)]
    pub margin_initial: f64,
    #[serde(default)]
    pub margin_maintenance: f64,
    /// K-5.1's variation account: the variation of the futures margined
    /// daily since their last settlement, Σ Q · (mark − reference) · point
    /// value, which the NAV counts (ERRATA E-73).
    #[serde(default)]
    pub variation: f64,
    /// The margin model the requirement is from: the one every holding's
    /// came from, `mixed` when they differ, `approx` for the leverage
    /// model's (E-73).
    #[serde(default)]
    pub margin_source: String,
}

/// One holding of the book at a bar's mark (K-5.1, an instrument's row of
/// `kernel.<run>.book`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HoldingRecord {
    pub t: i64,
    pub instrument: Sym,
    pub position: f64,
    pub avg_cost: f64,
    /// The price the holding is marked at, and whether the bar has it
    /// (`price`) or it is the last one seen (`stale`); `None` when the
    /// instrument has had no price yet.
    pub mark: Option<f64>,
    pub mark_source: String,
    /// An option's underlying and Greeks at the mark (E-73); `None` for any
    /// other instrument.
    #[serde(default)]
    pub option: Option<OptionHolding>,
}

/// What a holding of an option adds to its row (Q-3.1, ERRATA E-73): its
/// underlying and the underlying's price at the bar, its expiry date and
/// multiplier, and its Greeks as the `greeks` relation holds them at the
/// bar, `[IV, Delta, Gamma, Theta, Vega, Rho]` with theta per year and vega
/// per unit of volatility (`None` where the model has none).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OptionHolding {
    pub underlying: Sym,
    pub spot: Option<f64>,
    pub expiry: i64,
    pub multiplier: f64,
    pub greeks: Option<[f64; 6]>,
}

fn pushdown_on() -> bool {
    true
}

/// Executor configuration: part of the kernel, not of the program (section 6).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExecConfig {
    /// The starting cash, and under fixed-base accounting the capital every
    /// weight is a fraction of.
    pub initial_cash: f64,
    /// Accounting (data-bundle doc, section 5): off (the default), the book
    /// is a fixed base of `initial_cash`: `target_weight` sizes against it,
    /// leverage is measured against it, and a bar's return is the change in
    /// NAV over it, so profits are not reinvested; on, weights size against
    /// the book's equity and returns compound.
    #[serde(default)]
    pub compounding: bool,
    /// Fixed slippage applied to the fill price, in basis points, against
    /// the order; added to the volatility-scaled part.
    pub slippage_bps: f64,
    /// Volatility-scaled slippage (data-bundle doc, section 5): the fill
    /// price moves against the order by `slippage_vol_mult` times the
    /// instrument's realized volatility, the sample standard deviation of
    /// its log returns over the last `vol_window` bars ending at the fill
    /// bar; with fewer than `vol_min_obs` returns only the fixed part applies.
    pub slippage_vol_mult: f64,
    pub vol_window: usize,
    pub vol_min_obs: usize,
    /// Commission per share, subject to a per-order minimum.
    pub commission_per_share: f64,
    /// Commission in basis points of the traded notional, on both sides
    /// (0 by default), added to the per-share schedule.
    #[serde(default)]
    pub commission_bps: f64,
    pub commission_min_per_order: f64,
    /// Regulatory fee on the notional of sells (and shorts), in basis points.
    pub fee_bps_on_sells: f64,
    /// Liquidity (data-bundle doc, section 5): a fill is at most
    /// `participation_cap` times the bar's volume (0 is no cap); a delta
    /// order's remainder expires, a target's re-issues itself at the next
    /// bars until reached or superseded. Impact moves the fill price against
    /// the order by `impact_coef` times the square root of the filled
    /// quantity over average daily volume (the mean bar volume over the
    /// `adv_window` bars ending at the fill bar); 0 is no impact. A fill
    /// above `participation_warn` of bar volume is reported on the run.
    pub participation_cap: f64,
    pub impact_coef: f64,
    pub adv_window: usize,
    pub participation_warn: f64,
    /// Primitive relation that supplies bar volumes; `None` picks a
    /// `Quantity<Shares>`-valued primitive, preferring one named `volume`.
    pub volume_relation: Option<String>,
    /// Margin (data-bundle doc, section 5, leverage): gross exposure may not
    /// exceed `max_gross` times equity after a fill, and cash may not go
    /// below −(max_gross − 1) times equity (1 is no borrowing, the default;
    /// 2 is Reg T's 50% initial margin, see `ExecConfig::reg_t`); what the
    /// `on_leverage` policy judges. At every bar's mark, positive equity
    /// below `maintenance_margin` times gross is a margin call, judged by
    /// `on_margin_call`. Infinity under `leverage unconstrained` (stored as
    /// null).
    #[serde(with = "infinite_as_null")]
    pub max_gross: f64,
    pub maintenance_margin: f64,
    pub on_margin_call: OnMarginCall,
    /// Funding, as annual rates accrued over the calendar time between
    /// consecutive decision bars: positive cash earns `cash_rate` (0 under
    /// `financing cash 0`, warned), a debit pays `margin_rate`, short notional pays its borrow
    /// bucket's fee and earns `short_rebate`.
    pub cash_rate: f64,
    pub margin_rate: f64,
    pub short_rebate: f64,
    /// Borrow buckets by average daily volume, ascending `adv_below`, the
    /// last unbounded; an instrument whose ADV is unknown is in the last.
    pub borrow: Vec<BorrowBucket>,
    /// The bundle date a ticker literal resolves at (data-bundle doc,
    /// section 3); the dataset's own, or its last bar, when `None`.
    pub as_of: Option<i64>,
    /// Keep, per windowed aggregation group, the rows of every bar solved,
    /// so that a rolling feature solves each bar once (data-bundle doc,
    /// section 2, "State"); off only to prove the cache exact.
    pub window_cache: bool,
    /// Push eligible aggregate literals down to the data layer's pass (K-4.5);
    /// off only to compare the two (`--no-pushdown`, G3). Not part of the
    /// configuration's identity: both give the same bits (D-6.1).
    #[serde(skip, default = "pushdown_on")]
    pub pushdown: bool,
    /// Solve rule bodies over blocks of bindings (the set evaluator, M-8)
    /// rather than one environment at a time; off only to compare the two
    /// (`--evaluator tuple`, `tests/eval_set.rs`). Not part of the
    /// configuration's identity: both give the same bits.
    #[serde(skip, default = "pushdown_on")]
    pub set_eval: bool,
    /// The pre-trade limits of the execution block (K-3.5).
    #[serde(default)]
    pub limits: Vec<crate::check::Limit>,
    /// The execution block's `late reject` (K-6.3): a live run drops the
    /// decisions of a bar evaluated after its deadline.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub late_reject: bool,
    /// The memory the derived state may take (K-4.6, `--memory-budget`);
    /// half the machine's memory when unset. Not part of the identity.
    #[serde(skip)]
    pub memory_budget: Option<u64>,
    /// A fault made deliberately at a bar, for the acceptance tests of the
    /// invariants (G7): the clause whose check it trips, and the bar.
    #[serde(skip)]
    pub inject: Option<(String, i64)>,
    /// Rolling `mean`, `sum`, `std`, `cov`, `corr` and `ols_slope` over a rows
    /// window from sliding sums (compensated, shifted, rebuilt exactly every
    /// N rows): O(1) a bar instead of O(N), equal to the exact two-pass
    /// formulas to rounding (about 1e-12 relative), not to the bit. A
    /// decision on an exact tie or threshold can differ; false gives the
    /// exact formulas. Records written before the option read as false.
    #[serde(default)]
    pub running_sums: bool,
    /// Delisting haircuts by reason label on the last trade price, and the
    /// haircut for a reason not listed (data-bundle doc, section 4 and
    /// section 10 item 2: conservative by default, 1 is a total loss).
    pub delisting_haircuts: Vec<(String, f64)>,
    pub delisting_haircut_default: f64,
    /// Delisting proceeds (data-bundle doc, section 4): false (the
    /// default), a delisted name is closed at its last trade less the
    /// haircut for its reason, with commission; true, at its last trade
    /// with no haircut and no cost (a vendor's convention that a name which
    /// stops printing leaves the book at its last price).
    #[serde(default)]
    pub delist_at_last_price: bool,
    /// Dividends: false (the default), a receivable credited in cash at the
    /// pay date; true, reinvested at the ex-date close in fractional shares
    /// of the same name, at no cost (a total-return book).
    #[serde(default)]
    pub reinvest_dividends: bool,
    /// Splits and dividends are already in the execution prices (a total
    /// return series as `price_relation`): the executor does not apply them
    /// to the book. Delistings still apply. False by default.
    #[serde(default)]
    pub actions_in_prices: bool,
    /// Primitive relation that supplies fill and valuation prices; `None`
    /// picks a Price-valued primitive, preferring one named `close`.
    pub price_relation: Option<String>,
    /// Parameter values replacing the program's defaults (section 3: the
    /// only values the kernel may vary between runs of one program). A name
    /// is `param` in the strategy or `unit::param`; each value must be of
    /// the parameter's type and within its declared range.
    pub param_overrides: Vec<(String, Lit)>,
    /// Policies for degenerate book states (section 6, executor policy);
    /// every default halts.
    pub on_leverage: OnLeverage,
    pub on_oversize: OnOversize,
    pub on_ruin: OnRuin,
    /// Rounding of order quantities; whole shares by default.
    pub lot: Lot,
    /// The run's window (data-bundle doc, section 7): the first and last
    /// decision bars at which the executor runs and the strategy decides.
    /// Bars before `start` remain data, so features and recursions warm up
    /// on them; `None` is the data's first or last bar.
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    /// `silence flat` (language-v2, 4.4): an instrument with no decision at
    /// a decision bar is closed; `silence hold` keeps it.
    #[serde(default)]
    pub silence_flat: bool,
    /// A block hold-out's mask (Q-2.3): `[from, to)` spans whose bars the
    /// run's ledgers leave out. Part of the run's id through π, not of the
    /// configuration's (the run crosses the blocks the same way).
    #[serde(skip)]
    pub embargo_mask: Vec<(i64, i64)>,
    /// The option model of an option program (K-5.5), from the execution
    /// block's option fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<OptionConfig>,
    /// `commission per_contract P USD`: a fixed commission per contract per
    /// side, an option's or a future's, where the catalog states none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_contract: Option<f64>,
    /// The margin models in order of preference (K-5.6); the leverage
    /// model's margin when none is listed (ERRATA E-71).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub margin_models: Vec<crate::check::MarginModel>,
    /// A future's daily settlement through cash (K-5.4, `settlement daily`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settlement_daily: bool,
}

/// How an option is marked (K-5.5.2): at the mid of its quote, at its last
/// trade, or at the model's price at its implied volatility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionMark {
    Mid,
    Last,
    Model,
}

/// The option model's configuration (K-2.2, K-5.5).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OptionConfig {
    pub mark: OptionMark,
    /// The fill (K-5.5.3): `touch`, `mid` or `last`, moved against the order
    /// by `spread` times the quoted spread (the half-spread from the mid).
    pub fill: String,
    pub spread: f64,
    /// `greeks data`: the binding's `greeks` rows; the model's otherwise.
    pub greeks_data: bool,
    pub exercise: crate::syntax::ast::ExerciseRule,
}

impl Default for ExecConfig {
    /// Conservative, non-zero costs (data-bundle doc, section 5: "defaults
    /// are conservative on purpose"): half a cent a share with a dollar
    /// minimum, the SEC-style fee on sells, and slippage of a tenth of a
    /// bar's realized volatility.
    fn default() -> ExecConfig {
        ExecConfig {
            initial_cash: 1_000_000.0,
            compounding: false,
            slippage_bps: 0.0,
            slippage_vol_mult: 0.1,
            vol_window: 20,
            vol_min_obs: 10,
            commission_per_share: 0.005,
            commission_bps: 0.0,
            commission_min_per_order: 1.0,
            fee_bps_on_sells: 0.278,
            participation_cap: 0.1,
            impact_coef: 0.1,
            adv_window: 20,
            participation_warn: 0.05,
            volume_relation: None,
            max_gross: 1.0,
            maintenance_margin: 0.25,
            on_margin_call: OnMarginCall::Halt,
            cash_rate: 0.0,
            margin_rate: 0.05,
            short_rebate: 0.0,
            borrow: vec![
                BorrowBucket {
                    adv_below: 100_000.0,
                    fee_bps: 0.0,
                    shortable: false,
                },
                BorrowBucket {
                    adv_below: 1_000_000.0,
                    fee_bps: 300.0,
                    shortable: true,
                },
                BorrowBucket {
                    adv_below: f64::INFINITY,
                    fee_bps: 25.0,
                    shortable: true,
                },
            ],
            price_relation: None,
            as_of: None,
            window_cache: true,
            pushdown: true,
            set_eval: true,
            limits: Vec::new(),
            late_reject: false,
            memory_budget: None,
            inject: None,
            running_sums: true,
            delisting_haircuts: vec![("bankruptcy".into(), 1.0), ("regulatory".into(), 1.0), ("acquisition".into(), 0.0), ("voluntary".into(), 0.0)],
            delisting_haircut_default: 1.0,
            delist_at_last_price: false,
            reinvest_dividends: false,
            actions_in_prices: false,
            param_overrides: Vec::new(),
            on_leverage: OnLeverage::Halt,
            on_oversize: OnOversize::Halt,
            on_ruin: OnRuin::Halt,
            lot: Lot::Whole,
            start: None,
            end: None,
            silence_flat: false,
            options: None,
            per_contract: None,
            margin_models: Vec::new(),
            settlement_daily: false,
            embargo_mask: Vec::new(),
        }
    }
}

impl ExecConfig {
    /// The configuration the program's `execution`, `decisions` and
    /// `report` blocks fix (language-v2, 4.3 to 4.5): every field of the
    /// executor is a field of the block or derived from one.
    pub fn from_program(prog: &Program) -> ExecConfig {
        use crate::check::{Commission, Delisting, Impact, Leverage, Slippage};
        let x = &prog.exec;
        let mut cfg = ExecConfig {
            initial_cash: x.capital,
            compounding: x.compounding,
            price_relation: Some(x.price_instance.clone()),
            volume_relation: prog.relations.contains_key("volume").then(|| "volume".to_string()),
            lot: if x.lot_fractional { Lot::Fractional } else { Lot::Whole },
            actions_in_prices: x.actions_in_price,
            running_sums: x.numerics == "running",
            silence_flat: prog.silence_flat,
            start: prog.report.window.map(|w| w.0),
            end: prog.report.window.map(|w| w.1),
            fee_bps_on_sells: 0.0,
            ..ExecConfig::default()
        };
        match &x.commission {
            Commission::None | Commission::PerContract => {
                cfg.commission_per_share = 0.0;
                cfg.commission_min_per_order = 0.0;
                cfg.commission_bps = 0.0;
            }
            Commission::PerContractFixed(c) => {
                cfg.commission_per_share = 0.0;
                cfg.commission_min_per_order = 0.0;
                cfg.commission_bps = 0.0;
                cfg.per_contract = Some(*c);
            }
            Commission::PerShare { per_share, min } => {
                cfg.commission_per_share = *per_share;
                cfg.commission_min_per_order = *min;
                cfg.commission_bps = 0.0;
            }
            Commission::Bps(b) => {
                cfg.commission_per_share = 0.0;
                cfg.commission_min_per_order = 0.0;
                cfg.commission_bps = *b;
            }
        }
        match &x.slippage {
            Slippage::None => {
                cfg.slippage_bps = 0.0;
                cfg.slippage_vol_mult = 0.0;
            }
            Slippage::Fixed(b) => {
                cfg.slippage_bps = *b;
                cfg.slippage_vol_mult = 0.0;
            }
            Slippage::Vol { mult, bars } => {
                cfg.slippage_bps = 0.0;
                cfg.slippage_vol_mult = *mult;
                cfg.vol_window = *bars as usize;
                cfg.vol_min_obs = (*bars as usize / 2).max(2);
            }
        }
        match &x.impact {
            Impact::None => cfg.impact_coef = 0.0,
            Impact::Sqrt { coef, days } => {
                cfg.impact_coef = *coef;
                cfg.adv_window = *days as usize;
            }
        }
        cfg.participation_cap = x.participation.unwrap_or(0.0);
        match x.financing {
            None => {
                cfg.cash_rate = 0.0;
                cfg.margin_rate = 0.0;
                cfg.short_rebate = 0.0;
                cfg.borrow = vec![BorrowBucket {
                    adv_below: f64::INFINITY,
                    fee_bps: 0.0,
                    shortable: true,
                }];
            }
            Some((cash, debit, short)) => {
                cfg.cash_rate = cash;
                cfg.margin_rate = debit;
                cfg.short_rebate = short;
            }
        }
        match &x.leverage {
            Leverage::Unconstrained => {
                cfg.max_gross = f64::INFINITY;
                cfg.on_leverage = OnLeverage::Allow;
                cfg.on_margin_call = OnMarginCall::Allow;
                cfg.maintenance_margin = 0.0;
            }
            Leverage::RegT => {
                cfg.max_gross = 2.0;
                cfg.maintenance_margin = 0.25;
                cfg.on_leverage = OnLeverage::Reject;
            }
            Leverage::Gross { max, halt } => {
                cfg.max_gross = *max;
                cfg.on_leverage = if *halt { OnLeverage::Halt } else { OnLeverage::Reject };
            }
        }
        match &x.delisting {
            Delisting::LastPrice => cfg.delist_at_last_price = true,
            Delisting::Haircut(hs) => {
                cfg.delist_at_last_price = false;
                cfg.delisting_haircuts = hs.iter().filter(|(r, _)| r != "other").cloned().collect();
                cfg.delisting_haircut_default = hs.iter().find(|(r, _)| r == "other").map(|(_, h)| *h).unwrap_or(1.0);
            }
        }
        cfg.on_oversize = match x.oversize.as_str() {
            "clamp" => OnOversize::Clamp,
            "allow" => OnOversize::Allow,
            _ => OnOversize::Halt,
        };
        cfg.on_ruin = if x.ruin == "continue" { OnRuin::Continue } else { OnRuin::Halt };
        cfg.limits = x.limits.clone();
        cfg.late_reject = x.late == "reject";
        if let (Some(mark), Some(fill), Some(greeks), Some(exercise)) = (&x.option_mark, &x.option_fill, &x.greeks, &x.exercise) {
            cfg.options = Some(OptionConfig {
                mark: match mark.as_str() {
                    "last" => OptionMark::Last,
                    "model" => OptionMark::Model,
                    _ => OptionMark::Mid,
                },
                fill: fill.base.clone(),
                spread: fill.spread,
                greeks_data: greeks == "data",
                exercise: *exercise,
            });
        }
        cfg.margin_models = x.margin.clone();
        cfg.settlement_daily = x.settlement.as_deref() == Some("daily");
        if let Some(m) = &x.margin_call {
            cfg.on_margin_call = match m.as_str() {
                "liquidate" => OnMarginCall::Liquidate,
                "allow" => OnMarginCall::Allow,
                _ => OnMarginCall::Halt,
            };
        }
        cfg
    }

    /// Every cost, liquidity and funding model off and every name shortable
    /// for free: the executor of the semantic model alone, for hand-computed
    /// tests and for an author who wants a frictionless run (which is then
    /// warned on, see `RunResult::warnings`). The margin limits keep their
    /// defaults.
    pub fn frictionless() -> ExecConfig {
        ExecConfig {
            slippage_bps: 0.0,
            slippage_vol_mult: 0.0,
            commission_per_share: 0.0,
            commission_bps: 0.0,
            commission_min_per_order: 0.0,
            fee_bps_on_sells: 0.0,
            participation_cap: 0.0,
            impact_coef: 0.0,
            margin_rate: 0.0,
            short_rebate: 0.0,
            borrow: vec![BorrowBucket {
                adv_below: f64::INFINITY,
                fee_bps: 0.0,
                shortable: true,
            }],
            ..ExecConfig::default()
        }
    }

    /// Reg T buying power (data-bundle doc, section 5): 50% initial margin,
    /// so gross exposure up to twice equity, 25% maintenance, and orders
    /// beyond it rejected and logged rather than halting. The cost and
    /// liquidity models keep their defaults.
    pub fn reg_t() -> ExecConfig {
        ExecConfig {
            max_gross: 2.0,
            maintenance_margin: 0.25,
            on_leverage: OnLeverage::Reject,
            ..ExecConfig::default()
        }
    }

    /// This configuration with the margin and funding settings of `other`
    /// (what `--frictionless --margin reg-t` means).
    pub fn with_margin_of(self, other: &ExecConfig) -> ExecConfig {
        ExecConfig {
            max_gross: other.max_gross,
            maintenance_margin: other.maintenance_margin,
            on_margin_call: other.on_margin_call,
            on_leverage: other.on_leverage,
            cash_rate: other.cash_rate,
            margin_rate: other.margin_rate,
            short_rebate: other.short_rebate,
            borrow: other.borrow.clone(),
            ..self
        }
    }

    /// The haircut on the last trade of a name delisted for `reason`.
    pub fn delisting_haircut(&self, reason: &str) -> f64 {
        self.delisting_haircuts.iter().find(|(r, _)| r == reason).map(|(_, h)| *h).unwrap_or(self.delisting_haircut_default)
    }

    /// The borrow bucket of an instrument with average daily volume `adv`
    /// (the last bucket when unknown).
    pub fn borrow_bucket(&self, adv: Option<f64>) -> BorrowBucket {
        let last = self.borrow.last().copied().unwrap_or(BorrowBucket {
            adv_below: f64::INFINITY,
            fee_bps: 0.0,
            shortable: true,
        });
        match adv {
            None => last,
            Some(a) => self.borrow.iter().find(|b| a < b.adv_below).copied().unwrap_or(last),
        }
    }

    /// The models this configuration turns off, each named by the bias of
    /// the data-bundle doc it leaves unmodeled.
    pub fn warnings(&self) -> Vec<RunWarning> {
        let mut w = Vec::new();
        if self.cash_rate == 0.0 {
            w.push(RunWarning {
                bias: "cash-management".into(),
                message: "cash earns nothing (the execution block's financing credits no cash rate)".into(),
            });
        }
        if self.commission_per_share == 0.0 && self.commission_min_per_order == 0.0 && self.fee_bps_on_sells == 0.0 {
            w.push(RunWarning {
                bias: "transaction-cost neglect".into(),
                message: "commissions and fees are zero; fills cost nothing but their price".into(),
            });
        }
        if self.slippage_bps == 0.0 && self.slippage_vol_mult == 0.0 {
            w.push(RunWarning {
                bias: "slippage".into(),
                message: "slippage is zero; every order fills at the bar's close".into(),
            });
        }
        if self.participation_cap == 0.0 {
            w.push(RunWarning {
                bias: "liquidity".into(),
                message: "no participation cap; an order fills whole whatever the bar's volume".into(),
            });
        }
        if self.impact_coef == 0.0 {
            w.push(RunWarning {
                bias: "market-impact".into(),
                message: "impact is zero; a large order fills at the quoted price".into(),
            });
        }
        w
    }
}

/// A model the run's configuration turned off (data-bundle doc, section 5:
/// a study run at zero cost is warned, never silent).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunWarning {
    /// The bias of the data-bundle doc's audit the warning relates to.
    pub bias: String,
    pub message: String,
}

/// Costs paid over a run, and the notional traded.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostSummary {
    pub commissions: f64,
    pub fees: f64,
    /// The notional lost to slippage: Σ |quantity| · |fill price − bar price|.
    pub slippage: f64,
    /// The notional lost to impact: Σ |quantity| · bar price · impact fraction.
    pub impact: f64,
    /// Σ |quantity| · fill price.
    pub turnover: f64,
}

/// How much of what was asked for was filled, and at what share of volume.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LiquiditySummary {
    /// Σ filled quantity over Σ quantity the decisions asked for (a target's
    /// re-issues count their fills, not a new request); 1 when nothing was asked.
    pub fill_ratio: f64,
    /// Mean and maximum of |filled| / bar volume over fills with a volume.
    pub avg_participation: f64,
    pub max_participation: f64,
}

#[derive(Clone, Debug)]
pub enum RunError {
    /// Partial arithmetic (section 7): the rule, the tuple and the expression.
    Arithmetic {
        rule: String,
        bindings: String,
        expr: String,
        message: String,
    },
    /// Two distinct decisions for one instrument at one T (section 6).
    Conflict {
        t: i64,
        equity: String,
        first: (String, String),
        second: (String, String),
    },
    /// A delta decision with a non-positive quantity (section 4).
    BadDecision {
        t: i64,
        rule: String,
        decision: String,
        message: String,
    },
    NoBars,
    /// An explain request at a timestamp outside the rule's time domain
    /// (section 6): the nearest bars before and after it, if any.
    NotABar {
        t: i64,
        cal: String,
        before: Option<i64>,
        after: Option<i64>,
    },
    /// A request the kernel cannot serve as asked: missing or ill-typed
    /// explain inputs, a binding of a variable the rule does not have, a
    /// parameter override outside its type or range.
    Request(String),
    /// An executor configuration the program cannot honour.
    Config(String),
    /// The demand has no binding (K-3.2): the data layer's refusal, naming
    /// the need no version satisfies and what each held version lacks.
    Unbound(String),
    /// A precondition refused the run before its first barrier (K-3.2).
    Precondition {
        clause: String,
        message: String,
    },
    /// An invariant failed at a decision barrier (K-3.3): the run halts.
    Invariant {
        clause: String,
        t: i64,
        message: String,
    },
    /// A degenerate book state that the configured policy halts on
    /// (section 6, executor policy): ruin, leverage, or an oversize order.
    Risk {
        t: i64,
        rule: String,
        decision: String,
        message: String,
    },
    Internal(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Arithmetic { rule, bindings, expr, message } => write!(f, "partial arithmetic in rule {} at tuple [{}]: `{}`: {}", rule, bindings, expr, message),
            RunError::Conflict { t, equity, first, second } => write!(
                f,
                "conflicting decisions for {} at {}: {} (rule {}) and {} (rule {})",
                equity,
                time::format_timestamp(*t),
                first.0,
                first.1,
                second.0,
                second.1
            ),
            RunError::BadDecision { t, rule, decision, message } => write!(f, "invalid decision {} from rule {} at {}: {}", decision, rule, time::format_timestamp(*t), message),
            RunError::NoBars => write!(f, "the data has no bars on the decision calendar"),
            RunError::NotABar { t, cal, before, after } => {
                write!(f, "{} is not a bar of calendar {}; ", time::format_timestamp(*t), cal)?;
                match (before, after) {
                    (Some(b), Some(a)) => write!(f, "nearest bars are {} and {}", time::format_timestamp(*b), time::format_timestamp(*a)),
                    (Some(b), None) | (None, Some(b)) => write!(f, "the nearest bar is {}", time::format_timestamp(*b)),
                    (None, None) => write!(f, "the time domain is empty"),
                }
            }
            RunError::Request(m) => write!(f, "{}", m),
            RunError::Unbound(m) => write!(f, "the demand has no binding: {}", m),
            RunError::Precondition { clause, message } => write!(f, "precondition `{}` refused the run: {}", clause, message),
            RunError::Invariant { clause, t, message } => write!(f, "invariant `{}` failed at {}: {}", clause, time::format_timestamp(*t), message),
            RunError::Config(m) => write!(f, "configuration error: {}", m),
            RunError::Risk { t, rule, decision, message } => write!(f, "risk policy halted the run at {}: {} (rule {}): {}", time::format_timestamp(*t), decision, rule, message),
            RunError::Internal(m) => write!(f, "internal kernel error: {}", m),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DecisionRecord {
    pub t: i64,
    pub decision: Decision,
    pub rule: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FillRecord {
    pub t: i64,
    pub equity: Sym,
    pub quantity: f64,
    pub price: f64,
    /// The bar had no price for the instrument and the order reduced the
    /// position, so it filled at the last known price (section 6).
    pub at_last_price: bool,
    /// Commission charged (per share, at least the per-order minimum).
    pub commission: f64,
    /// Regulatory fee charged (on sells).
    pub fee: f64,
    /// Slippage paid: |quantity| · bar price · slippage fraction.
    pub slippage: f64,
    /// Impact paid: |quantity| · bar price · impact fraction.
    pub impact: f64,
    /// |quantity| / bar volume (0 when the bar has no volume).
    pub participation: f64,
    /// The fill was capped by participation; the remainder expired (delta)
    /// or was re-issued (target).
    pub partial: bool,
    /// A forced liquidation by a margin call, not a decision of the strategy.
    pub forced: bool,
    /// An option's cost against its quote's mid (K-5.5.3): (price − mid) ×
    /// quantity × multiplier; 0 for every other fill.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spread: f64,
    /// Under daily settlement, the price the future's position is valued
    /// against after the fill (E-10): the opening fill's, else unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<f64>,
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RunResult {
    pub symbols: Vec<String>,
    pub bars: Vec<i64>,
    /// The event log (K-5.2), of which `decisions`, `fills`, `dropped` and
    /// `actions` are projections (`emit` keeps them).
    #[serde(default)]
    pub events: Vec<RunEvent>,
    pub decisions: Vec<DecisionRecord>,
    pub fills: Vec<FillRecord>,
    /// Decisions the executor could not carry out, with the reason.
    pub dropped: Vec<(i64, Decision, String)>,
    /// The decision bars a live run evaluated past their deadline (K-6.3).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub late_bars: BTreeSet<i64>,
    /// Cash plus marked positions at each bar, before that bar's decisions.
    pub equity_curve: Vec<(i64, f64)>,
    /// The fixed capital a bar's return is measured against when profits are
    /// not reinvested (`ExecConfig::compounding` off); `None` when returns
    /// compound on equity.
    #[serde(default)]
    pub base_capital: Option<f64>,
    /// The multiplier of every security that is not a share (what a fill's
    /// price is worth per unit), for reading trades back from the fills.
    #[serde(default)]
    pub multipliers: BTreeMap<Sym, f64>,
    pub final_cash: f64,
    pub final_positions: BTreeMap<Sym, f64>,
    pub costs: CostSummary,
    pub liquidity: LiquiditySummary,
    pub funding: FundingSummary,
    /// The book at every bar's mark, parallel to `equity_curve`.
    pub exposure: Vec<ExposureRecord>,
    /// The holdings at every bar's mark.
    #[serde(default)]
    pub holdings: Vec<HoldingRecord>,
    /// Splits, dividends and delistings applied to the book.
    pub actions: Vec<ActionRecord>,
    pub stats: KernelStats,
    /// The models the configuration turned off, and what the run observed
    /// that the author should know (fills above the participation threshold).
    pub warnings: Vec<RunWarning>,
    /// The primitives the executor filled at and read volume from.
    pub price_relation: Option<String>,
    pub volume_relation: Option<String>,
    /// What every relation derived over the run (the coverage report).
    #[serde(default)]
    pub coverage: Vec<RelationCoverage>,
    /// Per rule and body literal (written index): bindings entering, bindings
    /// leaving, and calls at which the literal left no binding (the observed
    /// cardinalities of the plan and the failure counts).
    #[serde(default)]
    pub literal_counts: Vec<Vec<(u64, u64, u64)>>,
    /// The plan the run evaluated under (`written` or `optimised`).
    #[serde(default)]
    pub plan: String,
    /// The program's contract with the run's statuses (K-3).
    #[serde(default)]
    pub contract: crate::contract::Contract,
    /// The tuples that arrived after their bucket's barrier (K-4.4).
    #[serde(default)]
    pub late: Vec<LateRecord>,
    /// The run's identity, under which its outputs are written
    /// (`kernel.<run id>.*`, K-7.1).
    #[serde(default)]
    pub run_id: String,
    /// The binding the run read (β, D-7.5).
    #[serde(default)]
    pub binding_id: String,
    /// How the report block's calendar partitions the run (what the query
    /// layer's period metrics read, Q-3.1).
    #[serde(default)]
    pub periods: ReportPeriods,
}

/// How the report block's calendar (V-4.5) partitions a run into periods:
/// per decision bar when it is the decision calendar, per calendar day on
/// `days`, per session day on a sessions calendar (the days the root, or
/// any root, had a session), and per decision bar, with a note, on another
/// calendar.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReportPeriods {
    pub periods_per_year: f64,
    pub by_day: bool,
    /// The days of a sessions calendar, each as its first instant.
    pub days: Option<Vec<i64>>,
    pub note: Option<String>,
}

/// The periods of a run of `prog` over its binding: the days of a sessions
/// calendar are read from the binding's `session` rows (labelled at each
/// session's close).
pub fn report_periods(prog: &Program, bound: &Bound, client: &DataClient) -> Result<ReportPeriods, RunError> {
    let mut p = ReportPeriods {
        periods_per_year: prog.report.periods_per_year,
        ..Default::default()
    };
    let cal = prog.report_cal();
    if cal == prog.decision_cal {
        return Ok(p);
    }
    match &cal {
        Cal::Days => p.by_day = true,
        Cal::Sessions | Cal::SessionsOf(_) => {
            let root = match &cal {
                Cal::SessionsOf(r) => Some(r.clone()),
                _ => None,
            };
            let days: BTreeSet<i64> = bound
                .scan(client, "session", None)?
                .iter()
                .filter(|r| root.as_ref().is_none_or(|root| r.sym.is_some_and(|s| bound.symbols.name(s) == root)))
                .map(|r| time::day_key(r.t) * time::DAY)
                .collect();
            p.by_day = true;
            p.days = Some(days.into_iter().collect());
        }
        other => {
            p.note = Some(format!(
                "report calendar `{}` is not the decision calendar, `days` or a sessions calendar; the periods are the decision bars",
                prog.cal_name(other)
            ))
        }
    }
    Ok(p)
}

/// A run's identity (Definition A-4.1, A-6): H(P), β, the run mode, and the
/// run id over them and the plan step.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub program_hash: String,
    pub binding_id: String,
    pub mode: String,
    pub run_id: String,
    /// The effective configuration's hash: what a checkpoint is verified
    /// against besides (H(P), β, mode), for a configuration the program's
    /// blocks do not state (a test's override).
    #[serde(default)]
    pub config: String,
}

/// A tuple that arrived after its bucket's barrier (K-4.4): its place in
/// the stream, its relation, instrument, time and receipt.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LateRecord {
    pub seq: u64,
    pub relation: String,
    pub entity: Option<Sym>,
    pub t: i64,
    pub r: i64,
}

impl RunResult {
    /// Appends an event to the log, numbering it, and to the projection its
    /// kind feeds; returns its seq.
    pub fn emit(&mut self, mut e: RunEvent) -> u64 {
        let seq = self.events.len() as u64;
        e.seq = seq;
        match &e.kind {
            EventKind::Decision { decision } => self.decisions.push(DecisionRecord {
                t: e.t,
                decision: decision.clone(),
                rule: e.rule.unwrap_or(usize::MAX),
            }),
            EventKind::Fill { fill, .. } => self.fills.push(fill.clone()),
            EventKind::Dropped { decided, decision, reason } | EventKind::ExpiredOrder { decided, decision, reason } | EventKind::Rejected { decided, decision, reason } => {
                self.dropped.push((*decided, decision.clone(), reason.clone()))
            }
            EventKind::Action { action, .. } => {
                if let Some(sym) = e.instrument {
                    self.actions.push(ActionRecord {
                        t: e.t,
                        equity: sym,
                        action: action.clone(),
                        cash: e.cash_flow.unwrap_or(0.0),
                    });
                }
            }
            _ => {}
        }
        self.events.push(e);
        seq
    }

    /// The bar returns under the run's accounting: the change in NAV over
    /// the previous bar's equity, or over the fixed capital when profits are
    /// not reinvested.
    pub fn bar_returns(&self) -> Vec<(i64, f64)> {
        self.equity_curve
            .windows(2)
            .map(|w| {
                let base = self.base_capital.unwrap_or(w[0].1);
                (w[1].0, if base > 0.0 { (w[1].1 - w[0].1) / base } else { 0.0 })
            })
            .collect()
    }

    /// The curve the metrics read: the equity curve when returns compound,
    /// otherwise the capital compounded by the fixed-base bar returns (what
    /// a CAGR or a drawdown of a non-reinvesting book is computed on).
    pub fn metric_curve(&self) -> Vec<(i64, f64)> {
        let Some(base) = self.base_capital else { return self.equity_curve.clone() };
        let Some(&(t0, _)) = self.equity_curve.first() else { return vec![] };
        let mut e = base;
        let mut out = vec![(t0, e)];
        for (t, r) in self.bar_returns() {
            e *= 1.0 + r;
            out.push((t, e));
        }
        out
    }

    pub fn decisions_at(&self, t: i64) -> Vec<&Decision> {
        self.decisions.iter().filter(|d| d.t == t).map(|d| &d.decision).collect()
    }
    pub fn describe_decision(&self, d: &Decision) -> String {
        if d.ctor == Ctor::Combo {
            let legs: Vec<String> = d.legs.iter().map(|(s, q)| format!("leg({}, {})", self.symbols[*s as usize], q)).collect();
            return if d.order.is_market() {
                format!("combo([{}])", legs.join(", "))
            } else {
                format!("combo([{}], {})", legs.join(", "), d.order)
            };
        }
        if d.order.is_market() {
            format!("{}({}, {})", d.ctor.name(), self.symbols[d.equity as usize], d.amount)
        } else {
            format!("{}({}, {}, {})", d.ctor.name(), self.symbols[d.equity as usize], d.amount, d.order)
        }
    }
}

/// A stored relation (primitive, executor or kernel state), indexed by key
/// and, within a key's block, by the relation's first entity: a lookup that
/// binds the entity is a binary search, not a scan of the bar's tuples.
/// Blocks are sorted lazily: an insert marks its block dirty and the next
/// indexed lookup sorts it once (bulk loads and event streams stay linear).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Store {
    pub by_time: BTreeMap<i64, Vec<Tuple>>,
    /// The argument position the blocks are sorted by; `None` for a
    /// relation without an entity (then every lookup scans).
    #[serde(default)]
    pub entity_pos: Option<usize>,
    /// Keys whose block may be out of order.
    #[serde(default)]
    pub dirty: BTreeSet<i64>,
    /// The receipt instant of the stream's latest tuple, and the run of
    /// tuples it has brought to each key since (`Kernel::insert_fact`).
    #[serde(skip)]
    runs: Option<(i64, FxHashMap<i64, Run>)>,
}

/// The tuples one receipt instant of the stream has brought to a key of a
/// store.
#[derive(Clone, Debug, Default)]
struct Run {
    /// Where they begin in the key's block: the rows before were received
    /// at earlier instants.
    start: usize,
    /// The block's length after the latest of them: anything else that
    /// changes the block ends the run.
    after: usize,
    /// Their identities, hashed.
    seen: FxHashSet<u64>,
}

impl Store {
    /// Tuples held, over every key.
    pub fn len(&self) -> usize {
        self.by_time.values().map(|v| v.len()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.by_time.values().all(|v| v.is_empty())
    }
    pub fn insert(&mut self, key: i64, tuple: Tuple) {
        self.by_time.entry(key).or_default().push(tuple);
        if self.entity_pos.is_some() {
            self.dirty.insert(key);
        }
    }

    /// A tuple of the stream at `key`, received at instant `r`: a later
    /// receipt of a fact, one with the same `identity` at the key, replaces
    /// it (K-4.2). Only the rows of the key received at earlier instants
    /// are searched, and the instant's own when it has brought the identity
    /// already (two versions of one fact), so a bar received whole searches
    /// none.
    pub fn insert_received(&mut self, key: i64, tuple: Tuple, identity: &[usize], r: i64) {
        let Store { by_time, entity_pos, dirty, runs } = self;
        if runs.as_ref().is_none_or(|(at, _)| *at != r) {
            *runs = Some((r, FxHashMap::default()));
        }
        let runs = &mut runs.as_mut().expect("just set").1;
        let block = by_time.entry(key).or_default();
        let len = block.len();
        let run = runs.entry(key).or_default();
        if run.after != len || run.seen.is_empty() {
            *run = Run {
                start: len,
                after: len,
                seen: FxHashSet::default(),
            };
        }
        let same = |t: &Tuple| identity.iter().all(|&i| t[i] == tuple[i]);
        let ident = {
            let mut h = hash::FxHasher::default();
            for &i in identity {
                tuple[i].hash(&mut h);
            }
            h.finish()
        };
        let found = match block[..run.start].iter().position(same) {
            Some(i) => Some(i),
            None if run.seen.contains(&ident) => block[run.start..].iter().position(same).map(|i| run.start + i),
            None => None,
        };
        match found {
            Some(i) => block[i] = tuple,
            None => {
                block.push(tuple);
                run.seen.insert(ident);
                run.after = len + 1;
                if entity_pos.is_some() {
                    dirty.insert(key);
                }
            }
        }
    }

    /// Index the store by `entity_pos` (every block re-sorted on first use).
    pub fn index_by(&mut self, entity_pos: Option<usize>) {
        self.entity_pos = entity_pos;
        self.dirty = if entity_pos.is_some() { self.by_time.keys().copied().collect() } else { BTreeSet::new() };
    }

    /// The tuples at `key`, narrowed to those whose entity is `entity` when
    /// the store is indexed and the entity is bound (a superset of the
    /// matches otherwise: callers still filter on the full pattern).
    pub fn lookup(&mut self, key: i64, entity: Option<&Value>) -> &[Tuple] {
        let Some(e) = self.entity_pos else {
            return self.by_time.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
        };
        let Some(block) = self.by_time.get_mut(&key) else { return &[] };
        if self.dirty.remove(&key) {
            block.sort_by(|a, b| a[e].cmp(&b[e]));
            // The sort moves the run's tuples among the earlier ones.
            if let Some((_, runs)) = &mut self.runs {
                runs.remove(&key);
            }
        }
        match entity {
            Some(v) => {
                let lo = block.partition_point(|t| t[e] < *v);
                let hi = lo + block[lo..].partition_point(|t| t[e] <= *v);
                &block[lo..hi]
            }
            None => block,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RelInfo {
    pub name: String,
    pub key_pos: usize,
    pub inputs: Vec<usize>,
    pub stored: bool,
    pub cal: Cal,
    pub rules: Vec<usize>,
    pub entity_positions: Vec<usize>,
    /// A fact's key besides its time (ERRATA E-35): the entity arguments,
    /// then the labels. A stored relation holds one tuple per identity at
    /// a time (K-3.3); a later one is a revision and replaces it (K-4.2).
    pub identity: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct CompiledRule {
    pub idx: usize,
    pub slots: FxHashMap<String, usize>,
    pub nslots: usize,
    pub label: String,
    /// The slot of the head's first entity variable: the instrument whose
    /// sessions a per-instrument calendar ranges over.
    pub entity_slot: Option<usize>,
}

pub type Env = Vec<Option<Value>>;

/// The explanation of why a rule did or did not fire at a bar (section 7, diagnostics).
#[derive(Clone, Debug)]
pub struct Explanation {
    pub rule: String,
    pub t: i64,
    /// Variables pre-bound for this explanation, with the shown values.
    pub bindings: Vec<(String, String)>,
    pub solutions: usize,
    /// Index and text of the first body literal with no solution, if any.
    pub failed_at: Option<(usize, String)>,
    /// The variables of the first solution with their values, by name, when
    /// the rule fired: the instance a reader can check against the data.
    pub solution: Vec<(String, String)>,
    /// The bindings of the first instance entering the excluding literal,
    /// when the rule did not fire (the `failure` table's bindings).
    pub at_failure: Vec<(String, String)>,
}

impl std::fmt::Display for Explanation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let with = if self.bindings.is_empty() {
            String::new()
        } else {
            format!(" with {}", self.bindings.iter().map(|(v, x)| format!("{} = {}", v, x)).collect::<Vec<_>>().join(", "))
        };
        match &self.failed_at {
            Some((i, text)) => write!(
                f,
                "rule {} did not fire at {}{}: literal {} `{}` has no solution",
                self.rule,
                time::format_timestamp(self.t),
                with,
                i + 1,
                text
            ),
            None => {
                write!(f, "rule {} fired at {}{} with {} solution(s)", self.rule, time::format_timestamp(self.t), with, self.solutions)?;
                if !self.solution.is_empty() {
                    write!(f, "; first solution: {}", self.solution.iter().map(|(v, x)| format!("{} = {}", v, x)).collect::<Vec<_>>().join(", "))?;
                }
                Ok(())
            }
        }
    }
}

pub struct Kernel<'p> {
    /// The program the kernel evaluates: the checked one, or the lowered copy
    /// with its constants folded (compiler design, R6).
    pub(crate) prog: &'p Program,
    /// The checked program as written, for diagnostics in the author's words.
    pub(crate) source: &'p Program,
    /// The plan (compiler design, "Lower"): per rule the evaluation order of
    /// its body, the rules that contribute no work, the aggregation literals
    /// sharing one window state, and the plan's name.
    pub(crate) plan_order: Vec<Option<Rc<Vec<usize>>>>,
    /// Evaluate every rule in its written order rather than the plan's
    /// (`why not ... in written order`, V-8.3).
    pub(crate) written_order: bool,
    /// The instruments' ids in the data layer, set when the kernel is
    /// bound (what the outputs name an instrument by, K-7.1).
    pub(crate) data_ids: outputs::Ids,
    /// The run's identity (Definition A-4.1), set when it is bound: what a
    /// checkpoint is verified against (K-4.7).
    pub(crate) identity: Identity,
    pub(crate) skip_rule: Vec<bool>,
    pub(crate) shared_windows: FxHashMap<(usize, usize), usize>,
    pub(crate) plan_label: String,
    /// Per rule and written body literal: bindings in, bindings out, calls at
    /// which the literal emptied the survivors.
    pub(crate) literal_counts: Vec<Vec<(u64, u64, u64)>>,
    pub symbols: Symbols,
    pub(crate) stores: Vec<Store>,
    /// Shared so that a call holds its relation's facts without copying them.
    pub(crate) rels: Vec<Rc<RelInfo>>,
    pub(crate) rel_ids: HashMap<String, usize>,
    pub(crate) compiled: Vec<Rc<CompiledRule>>,
    pub(crate) memo: Memo,
    /// `R(...) asof T` on a derived relation: for (relation, inputs, bar)
    /// the latest bar at or before it with a tuple, a cache like `memo`.
    pub(crate) asof_memo: FxHashMap<(usize, Vec<Value>, i64), Option<i64>>,
    pub(crate) in_progress: FxHashSet<MemoKey>,
    /// Calls that halted on a stuck instance, by memo key (see `call`).
    pub(crate) memo_errors: FxHashMap<MemoKey, RunError>,
    /// The instances a call left stuck on a partial operation (section 7:
    /// a ⊥ instance halts the run where a decision demands it, and only
    /// there): by memo key, the partial head tuple of each with its error.
    /// A later call whose pattern meets one halts; one that does not reads
    /// the live tuples.
    pub(crate) memo_stuck: FxHashMap<MemoKey, Rc<StuckSet>>,
    pub(crate) domains: HashMap<Cal, BTreeSet<i64>>,
    /// Every calendar's labels over the whole dataset (the bars' keys, the
    /// periods' last bars, the sessions): what a fold opens buckets from.
    pub(crate) schedule: HashMap<Cal, BTreeSet<i64>>,
    /// Each instrument's sessions as (open, label) pairs by label (3.5), for
    /// the `sessions` calendars, `session_of` and the session constraints.
    pub(crate) sessions: FxHashMap<Sym, Rc<Vec<(i64, i64)>>>,
    /// Each instrument's session labels: its domain on the `sessions` calendar.
    pub(crate) session_domains: FxHashMap<Sym, BTreeSet<i64>>,
    /// The `clock(A, @T)` relation of a session-offset clock.
    pub(crate) clock_rel: Option<usize>,
    pub(crate) cfg: ExecConfig,
    /// The pushed-down aggregate literals' answers, by (rule, literal).
    pub(crate) pushed: FxHashMap<(usize, usize), Rc<pushdown::Pushed>>,
    /// The instant of the last barrier the fold passed: every tuple received
    /// by it is held, so a pushed answer that waits on no later receipt is
    /// what the kernel computes (ERRATA E-47). Before the first barrier (a
    /// kernel just built or restored) it is no instant, and no pushed answer
    /// is read; after the fold it stays the last barrier's.
    pub(crate) now: i64,
    /// The latest receipt among the tuples received and among the pushed
    /// answers read since the last decision barrier's checks (what
    /// `inv.no_read_past` and `inv.pushdown_receipt` compare with at(T)).
    pub(crate) received_r: i64,
    pub(crate) pushed_read_r: i64,
    pub(crate) price_rel: Option<usize>,
    pub(crate) price_col: usize,
    pub(crate) volume_rel: Option<usize>,
    pub(crate) volume_col: usize,
    /// The price relation's open, high and low companions (`open` next to
    /// `close`, `open_m` next to `close_m`), with their price columns: what
    /// market-on-open, limit and stop orders execute against.
    pub(crate) open_rel: Option<(usize, usize)>,
    pub(crate) high_rel: Option<(usize, usize)>,
    pub(crate) low_rel: Option<(usize, usize)>,
    pub(crate) last_price: FxHashMap<Sym, f64>,
    pub(crate) params: FxHashMap<(String, String), Value>,
    /// The bound instruments' symbols to the kernel's (identifier order).
    pub(crate) remap_syms: Vec<Sym>,
    /// The rows of every bar a windowed aggregation group has solved, by
    /// (rule, literal, the outer bindings the group reads), by bar.
    pub(crate) windows: FxHashMap<eval::WindowKey, eval::WindowCache>,
    /// For a `rows` window group, the earliest bar with rows when every bar
    /// before it is known to have none (where a walk back may stop).
    pub(crate) rows_floor: FxHashMap<eval::WindowKey, i64>,
    /// For a `rows` window group, the solved bars at which its anchor (the
    /// conjunction's first atom) holds: the group's rows.
    pub(crate) rows_anchor: FxHashMap<eval::WindowKey, BTreeSet<i64>>,
    /// Per (rule, literal): whether its rows window yields rows in
    /// environment order (see `eval::time_ordered`).
    pub(crate) time_ordered_memo: FxHashMap<(usize, usize), bool>,
    pub(crate) rows_plans: FxHashMap<(usize, usize), Rc<eval::RowsPlan>>,
    /// Per (rule, literal): what an aggregation's calls share.
    pub(crate) agg_plans: FxHashMap<(usize, usize), Rc<eval::AggPlan>>,
    pub(crate) rows_state: FxHashMap<eval::WindowKey, eval::RowsState>,
    /// Each calendar's time domain as a sorted vector, for the running
    /// sums' bar walk.
    pub(crate) domain_vecs: FxHashMap<(Cal, Option<Sym>), Rc<Vec<i64>>>,
    /// Memo retention (data-bundle doc, section 2, "State"): derived tuples
    /// older than `memo_horizon` seconds before the current bar (and than the
    /// last few bars) are dropped, except the latest of each relation read
    /// through `asof`, which a later bar may read however old. `None` keeps
    /// everything (a lookback the program does not bound).
    pub(crate) memo_horizon: Option<i64>,
    pub(crate) asof_read: FxHashSet<usize>,
    /// The longest `rows` window per resolution, in bars: a window cache
    /// that went cold re-solves up to that many bars back, so the memo keeps
    /// them (plus slack).
    pub(crate) rows_lookback: Vec<(Cal, usize)>,
    pub(crate) asof_memo_next: usize,
    /// Below this many results the memo is left alone.
    pub(crate) memo_min: usize,
    pub stats: KernelStats,
    pub(crate) labels: Symbols,
    /// The security table with ids in the kernel's symbol order, and the
    /// bundle date ticker literals and command-line names resolve at.
    pub(crate) securities: Vec<Security>,
    pub(crate) as_of: Option<i64>,
    /// The catalog's action relations, when the environment declares them.
    pub(crate) split_rel: Option<usize>,
    pub(crate) dividend_rel: Option<usize>,
    pub(crate) delisted_rel: Option<usize>,
    /// The equity class's `special_dividend`, what OCC adjusts strikes for.
    pub(crate) special_dividend_rel: Option<usize>,
    /// The option class's `quote` columns, when the program reads it.
    pub(crate) quote_cols: Option<crate::sim::option::QuoteCols>,
    /// Some instrument is an option: marks and volumes are its quote's.
    pub(crate) has_options: bool,
    /// What each equity literal of the program (a ticker) resolved to.
    pub(crate) literal_equities: HashMap<String, Sym>,
    /// Contract terms by kernel symbol (futures); a symbol absent is a share.
    pub(crate) contracts: FxHashMap<Sym, Contract>,
    /// Each underlying's chain snapshots: the contracts known at an
    /// instant, each with its receipt (D-2.4 `chain_index`).
    pub(crate) chains: FxHashMap<Sym, BTreeMap<i64, Vec<(Sym, i64)>>>,
    /// Each futures root's contracts by bar (D-2.4 `continuous`), which the
    /// futures model reads (M-7d).
    #[allow(dead_code)]
    pub(crate) rolls: FxHashMap<Sym, Vec<bind::Roll>>,
    /// Under `greeks data`, each contract's rows `(t, r, values)`.
    pub(crate) greeks_rows: FxHashMap<Sym, Vec<(i64, i64, [f64; 6])>>,
    /// The program's chains (D-7.2): what its `chain` relation holds.
    pub(crate) chain_needs: Vec<ChainNeed>,
    /// Per relation: calls computed (memo misses), calls that derived a
    /// tuple, tuples derived; the coverage report of a run.
    pub(crate) coverage: Vec<(usize, usize, usize)>,
    /// What the build noticed about the program against this data (a label
    /// the data never carries); reported with the run's warnings.
    pub(crate) run_warnings: Vec<RunWarning>,
    /// Record the summary state of every windowed aggregation group at
    /// every bar it serves (the `summary` state table of the compiler
    /// design, section 7.2); off by default.
    pub(crate) recording: bool,
    pub(crate) summary_rows: Vec<SummaryRow>,
}

/// A dynamic need as the kernel's `chain` evaluates it (K-2.4, D-4.4): the
/// underlying resolved, the filters of the data layer's chain query, and the
/// program's relation the spot is read from.
#[derive(Clone, Debug)]
pub(crate) struct ChainNeed {
    pub underlying: Sym,
    pub filters: abt_wire::generated::abt::wire::ChainFilters,
    pub spot: String,
}

/// One row of the `summary` state table: a windowed aggregation group
/// (rule, literal, the outer bindings it reads) at a bar, with the rows it
/// held and, when running sums served it, the shifted moment sums.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SummaryRow {
    pub rule: usize,
    pub literal: usize,
    pub outer: Vec<Option<Value>>,
    pub bar: i64,
    pub agg: String,
    pub rows: usize,
    /// `running` (sliding compensated sums), `two_pass` (the window's rows
    /// solved whole), `cached rows` (a calendar window's per-bar rows).
    pub path: &'static str,
    pub shift: Option<(f64, f64)>,
    /// `Σx, Σy, Σx², Σy², Σxy` of the shifted values.
    pub sums: Option<[f64; 5]>,
    pub value: Option<f64>,
}

impl<'p> Kernel<'p> {
    /// A kernel over what Bind read of the data layer (K-4.1): the binding's
    /// instruments, labels, ticker spans and contract terms, and every
    /// calendar's labels as the schedule. It holds no fact yet: the fold
    /// feeds the stream's tuples (`insert_fact`) and opens each calendar's
    /// buckets at their barriers (`open_bucket`).
    pub fn bound(prog: &'p Program, bound: &bind::Bound, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
        Kernel::build(prog, bound, cfg)
    }

    /// A bound kernel evaluating `prog` under a plan (compiler design,
    /// "Lower"): the plan's literal orders, infeasible rules, shared window
    /// states and folded constants are applied; everything else is `bound`.
    pub fn bound_planned(prog: &'p Program, lowered: &'p crate::lower::Lowered, bound: &bind::Bound, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
        let mut k = Kernel::build(lowered.program_for(prog), bound, cfg)?;
        k.apply_plan(prog, lowered);
        Ok(k)
    }

    fn apply_plan(&mut self, source: &'p Program, lowered: &'p crate::lower::Lowered) {
        self.source = source;
        self.plan_label = lowered.label.clone();
        self.plan_order = (0..self.prog.rules.len())
            .map(|ri| {
                let order = lowered.order.get(ri)?;
                let written: Vec<usize> = (0..self.prog.rules[ri].body.len()).collect();
                (*order != written).then(|| Rc::new(order.clone()))
            })
            .collect();
        self.skip_rule = (0..self.prog.rules.len()).map(|ri| lowered.skip.get(ri).copied().unwrap_or(false)).collect();
        self.shared_windows = lowered.shared_windows.iter().map(|(k, v)| (*k, *v)).collect();
        // The numerics setting of the plan is the executor configuration's.
        self.cfg.running_sums = lowered.numerics == crate::lower::Numerics::Running;
    }

    /// The plan this kernel evaluates under (`written` when none was applied).
    /// Record the summary state of every windowed aggregation group at
    /// every bar it serves (section 7.2's `summary` table).
    pub fn record_summaries(&mut self, on: bool) {
        self.recording = on;
        if !on {
            self.summary_rows.clear();
        }
    }
    pub fn summary_rows(&self) -> &[SummaryRow] {
        &self.summary_rows
    }
    /// The aggregate literals the data layer's pass answers (K-4.5), by
    /// (rule, literal): they hold no window state here.
    pub fn pushed_literals(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = self.pushed.keys().copied().collect();
        out.sort_unstable();
        out
    }
    pub fn plan_label(&self) -> &str {
        &self.plan_label
    }

    fn build(prog: &'p Program, dataset: &bind::Bound, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
        // Resolve string overrides by their parameter's type, as the checker
        // did for the program's own literals.
        let mut cfg = cfg;
        for (name, value) in cfg.param_overrides.iter_mut() {
            if let Lit::Str(s) = value {
                let (unit, pname) = name.split_once("::").unwrap_or((prog.strategy.as_str(), name.as_str()));
                if let Some(p) = prog.param(unit, pname) {
                    *value = if p.ty == Ty::Label { Lit::Label(s.clone()) } else { Lit::Entity(s.clone()) };
                }
            }
        }
        // Resolve every equity literal of the program (a ticker, as of the
        // bundle date, through the security table; the ticker is the id
        // without one), intern every label literal, then order ids by
        // identifier.
        let as_of = cfg.as_of.or_else(|| dataset.bundle_date());
        let mut symbols = dataset.symbols.clone();
        let mut labels = dataset.labels.clone();
        let mut literal_equities: HashMap<String, Sym> = HashMap::new();
        let mut unresolved: Vec<String> = Vec::new();
        // A label the data never carries makes every literal naming it false
        // (the Oct 9 study: `member(A, T, "SP500")` on data whose index is
        // `SPX` decided nothing, silently); the run warns, naming the labels
        // the data holds. A warning and not a refusal: a vocabulary member
        // the data happens not to carry (`"acquisition"` on data whose
        // delistings are all bankruptcies) is a legitimate literal that is
        // simply false here.
        let mut unknown_labels: Vec<String> = Vec::new();
        let mut intern = |l: &Lit| match l {
            Lit::Entity(s) => {
                if literal_equities.contains_key(s) {
                    return;
                }
                if dataset.securities.is_empty() {
                    let sym = symbols.intern(s);
                    literal_equities.insert(s.clone(), sym);
                } else {
                    match dataset.resolve_ticker(s, as_of) {
                        Ok(sym) => {
                            literal_equities.insert(s.clone(), sym);
                        }
                        Err(m) => unresolved.push(m),
                    }
                }
            }
            Lit::Label(s) => {
                if dataset.labels.get(s).is_none() {
                    unknown_labels.push(s.clone());
                }
                labels.intern(s);
            }
            _ => {}
        };
        for unit in prog.params.values() {
            for p in unit.values() {
                intern(&p.value);
            }
        }
        for (_, l) in &cfg.param_overrides {
            intern(l);
        }
        for rule in &prog.rules {
            for_each_lit(rule, &mut intern);
        }
        // A chain's underlying is named by its ticker, as a literal is.
        for c in &prog.demand.chains {
            intern(&Lit::Entity(c.underlying.clone()));
        }
        if let Some(m) = unresolved.first() {
            return Err(RunError::Config(m.clone()));
        }
        let mut run_warnings: Vec<RunWarning> = dataset.warnings.clone();
        if !unknown_labels.is_empty() {
            unknown_labels.sort();
            unknown_labels.dedup();
            let mut present: Vec<&str> = dataset.labels.names().iter().map(|x| x.as_str()).collect();
            present.sort_unstable();
            let shown = if present.is_empty() {
                "none".to_string()
            } else if present.len() > 24 {
                format!("{}, ... ({} labels in all)", present[..24].join(", "), present.len())
            } else {
                present.join(", ")
            };
            let named: Vec<String> = unknown_labels.iter().map(|l| format!("`{}`", l)).collect();
            run_warnings.push(RunWarning {
                bias: "vocabulary".into(),
                message: format!(
                    "the label{} {} occur{} in no relation of the data, so every literal naming {} is false; the labels the data holds are: {}",
                    if named.len() > 1 { "s" } else { "" },
                    named.join(", "),
                    if named.len() > 1 { "" } else { "s" },
                    if named.len() > 1 { "them" } else { "it" },
                    shown
                ),
            });
        }
        // Corporate actions the data carries and the program does not load
        // (no `need actions`): the executor cannot apply what it never sees,
        // so the run says so rather than booking a split as a price move.
        let carried: Vec<&str> = if cfg.actions_in_prices { vec!["delisted"] } else { vec!["split", "dividend", "delisted"] };
        // Judged on the relations the data has, not on their tuples, so that
        // a truncated sample and a fold stopped early warn as the whole run.
        let ignored: Vec<&str> = carried.into_iter().filter(|r| dataset.has_relation(r) && !prog.relations.contains_key(*r)).collect();
        if !ignored.is_empty() {
            run_warnings.push(RunWarning {
                bias: "corporate actions".into(),
                message: format!(
                    "the data has {} and the program does not load {} (its data block has no `need actions`), so the executor applies none: {}",
                    ignored.join(", "),
                    if ignored.len() > 1 { "them" } else { "it" },
                    if ignored.contains(&"split") {
                        "a split moves the price and not the position, and a delisted name is never closed"
                    } else {
                        "a delisted name is never closed"
                    }
                ),
            });
        }
        // The vocabularies the executor writes `spec` and `margin_by` in
        // (K-2.4, E-4).
        if prog.class == "option" {
            for w in ["call", "put", "american", "european", "physical", "cash"] {
                labels.intern(w);
            }
        }
        if !cfg.margin_models.is_empty() {
            for w in ["published", "reg_t", "portfolio", "span", "approx"] {
                labels.intern(w);
            }
        }
        let (symbols, remap) = symbols.sorted();
        let (labels, _) = labels.sorted();
        let literal_equities: HashMap<String, Sym> = literal_equities.into_iter().map(|(k, s)| (k, remap[s as usize])).collect();
        let securities: Vec<Security> = dataset
            .securities
            .iter()
            .map(|s| Security {
                id: remap[s.id as usize],
                ..s.clone()
            })
            .collect();
        let contracts: FxHashMap<Sym, Contract> = dataset
            .contracts
            .iter()
            .map(|(s, c)| {
                let mut c = c.clone();
                if let Some(o) = c.option.as_mut() {
                    o.underlying = remap[o.underlying as usize];
                }
                (remap[*s as usize], c)
            })
            .collect();
        let chains: FxHashMap<Sym, BTreeMap<i64, Vec<(Sym, i64)>>> = dataset
            .chains
            .iter()
            .map(|(u, snaps)| (remap[*u as usize], snaps.iter().map(|(t, v)| (*t, v.iter().map(|(s, r)| (remap[*s as usize], *r)).collect())).collect()))
            .collect();
        let rolls: FxHashMap<Sym, Vec<bind::Roll>> = dataset
            .rolls
            .iter()
            .map(|(root, v)| {
                (
                    remap[*root as usize],
                    v.iter()
                        .map(|r| bind::Roll {
                            contract: remap[r.contract as usize],
                            ..r.clone()
                        })
                        .collect(),
                )
            })
            .collect();
        let greeks_rows: FxHashMap<Sym, Vec<(i64, i64, [f64; 6])>> = dataset.greeks.iter().map(|(s, v)| (remap[*s as usize], v.clone())).collect();
        let chain_needs: Vec<ChainNeed> = prog
            .demand
            .chains
            .iter()
            .filter_map(|c| {
                let underlying = *literal_equities.get(&c.underlying)?;
                Some(ChainNeed {
                    underlying,
                    filters: bind::chain_filters(c),
                    spot: c.spot.clone(),
                })
            })
            .collect();
        // Relation table.
        let mut rels = Vec::new();
        let mut rel_ids = HashMap::new();
        for (name, sig) in &prog.relations {
            let key_pos = sig.key_pos().ok_or_else(|| RunError::Internal(format!("relation `{}` has no temporal key", name)))?;
            let stored = matches!(sig.kind, Kind::Primitive { .. } | Kind::Executor | Kind::KernelState);
            rel_ids.insert(name.clone(), rels.len());
            rels.push(Rc::new(RelInfo {
                name: name.clone(),
                key_pos,
                inputs: vec![],
                stored,
                cal: sig.cal.clone(),
                rules: prog.rules_for(name),
                entity_positions: sig.args.iter().enumerate().filter(|(_, a)| a.ty.is_entity()).map(|(i, _)| i).collect(),
                identity: sig
                    .args
                    .iter()
                    .enumerate()
                    .filter(|(i, a)| *i != key_pos && a.ty.is_entity())
                    .chain(sig.args.iter().enumerate().filter(|(i, a)| *i != key_pos && a.ty == Ty::Label))
                    .map(|(i, _)| i)
                    .collect(),
            }));
        }
        // Stores and time domains.
        let mut stores: Vec<Store> = rels
            .iter()
            .map(|r| {
                let mut st = Store::default();
                st.index_by(r.entity_positions.first().copied());
                st
            })
            .collect();
        // The base domain: the keys of the stored relations at the base
        // calendar that are read at their own bars (bars and session
        // labels); a catalog relation or a series read as of the bar
        // contributes no bar.
        let own_bar: Vec<bool> = rels
            .iter()
            .map(|r| prog.relations.get(&r.name).map(|s| s.cal == Cal::Base && s.native.is_none()).unwrap_or(false))
            .collect();
        // The calendars' sources, as Bind read them: the keys of the
        // relations read at their own bars, the sessions, and the prints a
        // session-offset clock is placed on (in the kernel's symbol order).
        let _ = &own_bar;
        let base_bars: BTreeSet<i64> = dataset.base_bars.clone();
        let session_rows: Vec<(Sym, i64, i64)> = dataset.sessions.iter().map(|(s, open, label)| (remap[*s as usize], *open, *label)).collect();
        let prints: FxHashMap<Sym, BTreeSet<i64>> = dataset.prints.iter().map(|(s, ts)| (remap[*s as usize], ts.clone())).collect();
        // The sessions by instrument, and the calendars' domains (3.1, 3.5):
        // a civil calendar's labels are the last base bar of each period;
        // the sessions calendar's labels are the session labels.
        let mut sessions: FxHashMap<Sym, Rc<Vec<(i64, i64)>>> = FxHashMap::default();
        {
            let mut by_sym: BTreeMap<Sym, Vec<(i64, i64)>> = BTreeMap::new();
            for (sym, open, label) in session_rows {
                by_sym.entry(sym).or_default().push((open, label));
            }
            for (sym, mut v) in by_sym {
                v.sort_by_key(|(_, l)| *l);
                v.dedup_by_key(|(_, l)| *l);
                sessions.insert(sym, Rc::new(v));
            }
        }
        let mut domains: HashMap<Cal, BTreeSet<i64>> = HashMap::new();
        let mut wanted: BTreeSet<Cal> = prog.calendars.values().cloned().collect();
        wanted.extend(prog.relations.values().map(|s| s.cal.clone()));
        wanted.insert(prog.decision_cal.clone());
        wanted.insert(Cal::Base);
        for cal in wanted {
            let dom: BTreeSet<i64> = match &cal {
                Cal::Base => base_bars.clone(),
                Cal::Sessions => sessions.values().flat_map(|v| v.iter().map(|(_, l)| *l)).collect(),
                Cal::SessionsOf(root) => {
                    let sym = symbols.get(root);
                    sym.and_then(|s| sessions.get(&s)).map(|v| v.iter().map(|(_, l)| *l).collect()).unwrap_or_default()
                }
                civil => {
                    let mut last: BTreeMap<i64, i64> = BTreeMap::new();
                    for &t in &base_bars {
                        let k = time::period_key(civil, t);
                        let e = last.entry(k).or_insert(t);
                        if t > *e {
                            *e = t;
                        }
                    }
                    last.into_values().collect()
                }
            };
            domains.insert(cal, dom);
        }
        // The schedule is every calendar's labels over the binding; the
        // domains start empty and grow as the stream's barriers open their
        // buckets.
        let schedule = domains.clone();
        for d in domains.values_mut() {
            d.clear();
        }
        // Compiled rules: variable slots.
        let mut compiled = Vec::new();
        for (i, rule) in prog.rules.iter().enumerate() {
            let mut slots: FxHashMap<String, usize> = FxHashMap::default();
            collect_vars(rule, &mut |v| {
                let n = slots.len();
                slots.entry(v.to_string()).or_insert(n);
            });
            let nslots = slots.len();
            let entity_slot = prog.relations.get(&rule.head.name).and_then(|sig| {
                sig.args.iter().zip(&rule.head.terms).find_map(|(a, t)| match t {
                    Term::Var(v, _) if a.ty.is_entity() => slots.get(v).copied(),
                    _ => None,
                })
            });
            compiled.push(Rc::new(CompiledRule {
                idx: i,
                slots,
                nslots,
                label: prog.rule_label(i),
                entity_slot,
            }));
        }
        // Parameters.
        let mut params = FxHashMap::default();
        let mut sym_tmp = symbols.clone();
        let mut lab_tmp = labels.clone();
        for (unit, ps) in &prog.params {
            for (name, p) in ps {
                params.insert((unit.clone(), name.clone()), lit_value(&p.value, &mut sym_tmp, &mut lab_tmp, &literal_equities));
            }
        }
        for (name, value) in &cfg.param_overrides {
            let (unit, pname) = name.split_once("::").unwrap_or((prog.strategy.as_str(), name.as_str()));
            let Some(p) = prog.param(unit, pname) else {
                let known: Vec<String> = prog
                    .params
                    .iter()
                    .flat_map(|(u, ps)| ps.keys().map(move |n| if *u == prog.strategy { n.clone() } else { format!("{}::{}", u, n) }))
                    .collect();
                return Err(RunError::Request(format!(
                    "no parameter `{}` in `{}`; the parameters are {}",
                    name,
                    unit,
                    if known.is_empty() { "none".to_string() } else { known.join(", ") }
                )));
            };
            if !crate::check::types::compat(&p.ty, &value.ty()) {
                return Err(RunError::Request(format!("parameter `{}` of `{}` is {}, and `{}` is {}", pname, unit, p.ty, value, value.ty())));
            }
            if let Some((lo, hi)) = &p.range {
                let within = match (lit_magnitude(value), lit_magnitude(lo), lit_magnitude(hi)) {
                    (Some(v), Some(l), Some(h)) => l <= v && v <= h,
                    _ => true,
                };
                if !within {
                    return Err(RunError::Request(format!("parameter `{}` of `{}` = {} is outside its range {}..{}", pname, unit, value, lo, hi)));
                }
            }
            // WF-4 was judged on the default: `lag` by a zero duration is
            // causal, not strict (section 5), so an override may not change
            // whether a lag duration is zero.
            if let (Lit::Duration(d), Lit::Duration(v)) = (&p.value, value) {
                if d.is_zero() != v.is_zero() && lag_uses_param(prog, unit, pname) {
                    return Err(RunError::Request(format!(
                        "parameter `{}` of `{}` is a lag duration and the checker judged WF-4 with its default {}; an override may not change whether it is zero (got {})",
                        pname, unit, d, v
                    )));
                }
            }
            // The strategy's parameters are in scope of every unit it links
            // (a library instance reads them by its own unit): the override
            // applies wherever the parameter is.
            let v = lit_value(value, &mut sym_tmp, &mut lab_tmp, &literal_equities);
            let qualified = name.contains("::");
            for (u, ps) in &prog.params {
                if (u == unit || !qualified) && ps.contains_key(pname) {
                    params.insert((u.clone(), pname.to_string()), v.clone());
                }
            }
        }
        // W7 on the values the run uses: the checker judged a calendar
        // window's `min K` against its duration on the parameters' defaults,
        // and an override may make the window unable to hold K bars, which
        // leaves its rule deriving nothing, silently.
        if !cfg.param_overrides.is_empty() {
            let effective = |unit: &str, name: &str| -> Option<Lit> {
                cfg.param_overrides
                    .iter()
                    .rev()
                    .find(|(n, _)| match n.split_once("::") {
                        Some((u, pn)) => u == unit && pn == name,
                        None => n == name,
                    })
                    .map(|(_, v)| v.clone())
                    .or_else(|| prog.param(unit, name).map(|p| p.value.clone()))
            };
            let value = |e: &Expr, unit: &str, overridden: bool| -> Option<Lit> {
                match e {
                    Expr::Lit(l, _) => Some(l.clone()),
                    Expr::Param(p, _) if overridden => effective(unit, p),
                    Expr::Param(p, _) => prog.param(unit, p).map(|p| p.value.clone()),
                    _ => None,
                }
            };
            fn windows<'a>(lits: &'a [Literal], out: &mut Vec<(&'a Expr, &'a Expr)>) {
                for l in lits {
                    match l {
                        Literal::Window {
                            kind: WindowKind::Window | WindowKind::Prior,
                            dur,
                            min,
                            ..
                        } => out.push((dur, min)),
                        Literal::Agg { conj, .. } => windows(conj, out),
                        Literal::Case { branches, .. } => branches.iter().for_each(|b| windows(&b.guard, out)),
                        _ => {}
                    }
                }
            }
            for (ri, rule) in prog.rules.iter().enumerate() {
                let mut ws = Vec::new();
                windows(&rule.body, &mut ws);
                for (dur, min) in ws {
                    let (Some(Lit::Duration(d)), Some(Lit::Int(k))) = (value(dur, &rule.unit, true), value(min, &rule.unit, true)) else {
                        continue;
                    };
                    let defaults = (value(dur, &rule.unit, false), value(min, &rule.unit, false));
                    if defaults == (Some(Lit::Duration(d)), Some(Lit::Int(k))) {
                        continue;
                    }
                    let cal = &prog.infos[ri].cal;
                    let bars = crate::check::rule::window_capacity(cal, prog.base, &d);
                    if k > bars {
                        run_warnings.push(RunWarning {
                            bias: "window sanity".into(),
                            message: format!(
                                "with the parameter overrides, rule {} reads a window of {} with `min {}`, and such a window holds at most {} bars on calendar `{}`: the aggregate never has enough rows, so the rule derives nothing (W7)",
                                prog.rule_label(ri),
                                d,
                                k,
                                bars,
                                prog.cal_name(cal)
                            ),
                        });
                    }
                }
            }
        }
        // The price relation the execution block names: a primitive or a
        // derived Price relation the executor fills and marks at (4.3).
        let price_rel = match &cfg.price_relation {
            Some(name) => {
                let id = *rel_ids.get(name).ok_or_else(|| RunError::Config(format!("price relation `{}` is not in the program", name)))?;
                let sig = prog.relations.get(name).unwrap();
                if price_column(sig).is_none() || !sig.args.iter().any(|a| a.ty.is_entity()) {
                    return Err(RunError::Config(format!("price relation `{}` must have an entity argument and a Price<...> output", name)));
                }
                Some(id)
            }
            None => rel_ids.get("close").copied(),
        };
        let price_col = price_rel.and_then(|id| price_column(prog.relations.get(&rels[id].name).unwrap())).unwrap_or(0);
        // The base volume for the participation cap and impact.
        let volume_rel = match &cfg.volume_relation {
            Some(name) => {
                let id = *rel_ids.get(name).ok_or_else(|| RunError::Config(format!("volume relation `{}` is not in the program", name)))?;
                let sig = prog.relations.get(name).unwrap();
                if shares_column(sig).is_none() || !sig.args.iter().any(|a| a.ty.is_entity()) {
                    return Err(RunError::Config(format!("volume relation `{}` must have an entity argument and a quantity output", name)));
                }
                Some(id)
            }
            None => rel_ids.get("volume").copied(),
        };
        let volume_col = volume_rel.and_then(|id| shares_column(prog.relations.get(&rels[id].name).unwrap())).unwrap_or(0);
        // The base bar primitives market-on-open, limit and stop orders execute against.
        let companion = |which: &str| -> Option<(usize, usize)> {
            let sig = prog.relations.get(which)?;
            if !matches!(sig.kind, Kind::Primitive { .. }) {
                return None;
            }
            Some((*rel_ids.get(which)?, price_column(sig)?))
        };
        let (open_rel, high_rel, low_rel) = (companion("open"), companion("high"), companion("low"));
        // An order that executes away from the close needs the price
        // relation's companions (section 6, order types): without them every
        // such order waits for a print that never comes and is dropped, which
        // the Oct 9 study found silent. Refused before the run instead.
        for (ri, rule) in prog.rules.iter().enumerate().filter(|(_, r)| r.head.name == "decide") {
            let Some(Term::Ctor(_, subs, _)) = rule.head.terms.get(1) else { continue };
            let Some(order) = subs.get(2) else { continue };
            let (kind, needs): (&str, Vec<(&str, bool)>) = match order {
                Term::Param(p, _) if p == "moo" || p == "moo_moc" => (p.as_str(), vec![("open", open_rel.is_some())]),
                Term::Ctor(c, _, _) if c == "limit" || c == "stop" => (c.as_str(), vec![("open", open_rel.is_some()), ("high", high_rel.is_some()), ("low", low_rel.is_some())]),
                _ => continue,
            };
            let missing: Vec<&str> = needs.iter().filter(|(_, have)| !have).map(|(n, _)| *n).collect();
            if missing.is_empty() {
                continue;
            }
            let price_name = price_rel.map(|id| rels[id].name.clone()).unwrap_or_else(|| "(none)".to_string());
            let expected: Vec<String> = missing
                .iter()
                .map(|m| {
                    if price_name.contains("close") {
                        format!("`{}`", price_name.replace("close", m))
                    } else {
                        format!("a `{}` relation named like the price relation", m)
                    }
                })
                .collect();
            let _ = price_name;
            return Err(RunError::Config(format!(
                "rule {} places a `{}` order, but the data has no {} bars ({}): every such order would wait for a print that never comes and be dropped; add `need bars` for a class with {}, or use `market` or `moc`",
                crate::check::rule_label(&prog.rules, ri),
                kind,
                missing.join(", "),
                expected.join(", "),
                missing.join(", ")
            )));
        }
        // The catalog's actions, by name (data-bundle doc, section 3).
        let primitive = |name: &str| -> Option<usize> {
            let id = *rel_ids.get(name)?;
            matches!(prog.relations.get(name)?.kind, Kind::Primitive { .. }).then_some(id)
        };
        let split_rel = primitive("split");
        let dividend_rel = primitive("dividend");
        let delisted_rel = primitive("delisted");
        let special_dividend_rel = primitive("special_dividend");
        let quote_cols = crate::sim::option::QuoteCols::of(prog, &rel_ids);
        let has_options = contracts.values().any(|c| c.option.is_some());
        // What the memo must keep: the relations read through `asof`, and the
        // longest calendar lookback (windows and lags) of any rule.
        let mut asof_read: FxHashSet<usize> = FxHashSet::default();
        let mut horizon: Option<i64> = Some(0);
        let mut rows_lookback: Vec<(Cal, usize)> = Vec::new();
        for rule in &prog.rules {
            let rule_res = prog.relations.get(&rule.head.name).map(|s| s.cal.clone()).unwrap_or(prog.decision_cal.clone());
            let count = |e: &Expr| -> Option<usize> {
                match e {
                    Expr::Lit(Lit::Int(n), _) => Some(*n as usize),
                    Expr::Param(p, _) => match params.get(&(rule.unit.clone(), p.clone())) {
                        Some(Value::Count(n)) => Some(*n as usize),
                        _ => None,
                    },
                    _ => None,
                }
            };
            let dur_secs = |e: &Expr| -> Option<i64> {
                let d = match e {
                    Expr::Lit(Lit::Duration(d), _) => *d,
                    Expr::Param(p, _) => match params.get(&(rule.unit.clone(), p.clone())) {
                        Some(Value::Dur(d)) => **d,
                        _ => return None,
                    },
                    _ => return None,
                };
                Some((d.months * 31 + d.days) * time::DAY + d.nanos)
            };
            let mut stack: Vec<&Literal> = rule.body.iter().collect();
            while let Some(lit) = stack.pop() {
                match lit {
                    Literal::AsOf { atom, .. } => {
                        if let Some(&id) = rel_ids.get(&atom.name) {
                            asof_read.insert(id);
                        }
                    }
                    Literal::Agg { conj, .. } => stack.extend(conj.iter()),
                    Literal::Case { branches, .. } => stack.extend(branches.iter().flat_map(|b| b.guard.iter())),
                    Literal::Window {
                        kind: WindowKind::Rows | WindowKind::PriorRows,
                        dur,
                        ..
                    } => match count(dur) {
                        Some(n) => match rows_lookback.iter_mut().find(|(r, _)| *r == rule_res) {
                            Some(slot) => slot.1 = slot.1.max(n),
                            None => rows_lookback.push((rule_res.clone(), n)),
                        },
                        None => horizon = None,
                    },
                    Literal::Window {
                        kind: WindowKind::Bucket(_) | WindowKind::Session,
                        ..
                    } => {
                        // A bucket or a session spans at most a year of base bars.
                        horizon = horizon.map(|h| h.max(366 * time::DAY));
                    }
                    Literal::Window {
                        kind: WindowKind::Window | WindowKind::Prior,
                        dur,
                        ..
                    }
                    | Literal::Builtin(Builtin::Lag { n: dur, .. }, _) => {
                        horizon = match (horizon, dur_secs(dur)) {
                            (Some(h), Some(d)) => Some(h.max(d)),
                            _ => None,
                        };
                    }
                    _ => {}
                }
            }
        }
        // The decision clock of a session offset (3.5): per instrument and
        // session, the base bar labelled open + offset, or the first bar it
        // traded after it within the session.
        if let (Some(offset), Some(&cid)) = (prog.clock.offset, rel_ids.get("clock")) {
            let print_rel = rel_ids.get("close").copied();
            let mut facts: Vec<(Sym, i64)> = Vec::new();
            for (sym, v) in &sessions {
                for &(open, label) in v.iter() {
                    let target = time::add_duration(open, offset);
                    if target > label {
                        continue;
                    }
                    let bar = match print_rel {
                        Some(_) => prints.get(sym).and_then(|ts| ts.range(target..=label).next().copied()),
                        None => base_bars.range(target..=label).next().copied(),
                    };
                    if let Some(b) = bar {
                        facts.push((*sym, b));
                    }
                }
            }
            for (sym, b) in facts {
                stores[cid].insert(b, vec![Value::Equity(sym), Value::Time(b)]);
            }
        }
        // A week of slack covers `prev` across weekends and holidays at any resolution.
        let memo_horizon = horizon.map(|h| h + 7 * time::DAY);
        let n_rels = rels.len();
        let literal_counts: Vec<Vec<(u64, u64, u64)>> = prog.rules.iter().map(|r| vec![(0, 0, 0); r.body.len()]).collect();
        Ok(Kernel {
            prog,
            source: prog,
            plan_order: vec![None; prog.rules.len()],
            written_order: false,
            data_ids: Default::default(),
            identity: Default::default(),
            skip_rule: vec![false; prog.rules.len()],
            shared_windows: Default::default(),
            plan_label: "written".into(),
            literal_counts,
            symbols,
            stores,
            rels,
            clock_rel: rel_ids.get("clock").copied().filter(|_| prog.clock.offset.is_some()),
            rel_ids,
            compiled,
            memo: Default::default(),
            asof_memo: Default::default(),
            in_progress: Default::default(),
            memo_errors: Default::default(),
            memo_stuck: Default::default(),
            domains,
            schedule,
            session_domains: sessions.iter().map(|(s, v)| (*s, v.iter().map(|(_, l)| *l).collect())).collect(),
            sessions,
            cfg,
            pushed: Default::default(),
            now: i64::MIN,
            received_r: i64::MIN,
            pushed_read_r: i64::MIN,
            price_rel,
            price_col,
            volume_rel,
            volume_col,
            open_rel,
            high_rel,
            low_rel,
            labels,
            securities,
            as_of,
            split_rel,
            dividend_rel,
            delisted_rel,
            special_dividend_rel,
            quote_cols,
            has_options,
            literal_equities,
            contracts,
            chains,
            rolls,
            greeks_rows,
            chain_needs,
            last_price: Default::default(),
            params,
            remap_syms: remap,
            windows: Default::default(),
            rows_floor: Default::default(),
            rows_anchor: Default::default(),
            time_ordered_memo: Default::default(),
            rows_plans: Default::default(),
            agg_plans: Default::default(),
            rows_state: Default::default(),
            domain_vecs: Default::default(),
            recording: false,
            summary_rows: Vec::new(),
            memo_horizon,
            asof_read,
            rows_lookback,
            asof_memo_next: 200_000,
            memo_min: 200_000,
            stats: KernelStats::default(),
            coverage: vec![(0, 0, 0); n_rels],
            run_warnings,
        })
    }

    /// What every relation derived so far (section 7, diagnostics): the
    /// counts behind the coverage report, in relation id order.
    pub fn coverage(&self) -> Vec<RelationCoverage> {
        self.rels
            .iter()
            .enumerate()
            .zip(&self.coverage)
            .map(|((rel, info), &(calls, nonempty, tuples))| {
                // A stored relation's tuples are what the data (or the
                // executor) holds, whichever way the rules read them.
                let tuples = if info.stored { self.stores[rel].len() } else { tuples };
                RelationCoverage {
                    name: info.name.clone(),
                    derived: !info.stored,
                    calls,
                    nonempty,
                    tuples,
                }
            })
            .collect()
    }

    /// The evaluator's counters, with the cache's current size.
    pub fn stats(&self) -> KernelStats {
        KernelStats {
            window_groups: self.windows.len(),
            window_rows_cached: self.windows.values().map(|m| m.len()).sum(),
            memo_entries: self.memo.len(),
            ..self.stats.clone()
        }
    }

    /// Insert one primitive fact (already in the kernel's symbol order) and
    /// return its temporal key.
    /// A tuple of the stream, received at instant `r`; a revision (a later
    /// receipt of the same fact) replaces the row it revises (K-4.2,
    /// `Store::insert_received`).
    pub fn insert_fact(&mut self, rel: usize, tuple: Tuple, r: i64) -> Result<i64, RunError> {
        let info = &self.rels[rel];
        let key = tuple[info.key_pos].as_time().ok_or_else(|| RunError::Internal(format!("non-timestamp key in `{}`", info.name)))?;
        self.stores[rel].insert_received(key, tuple, &info.identity, r);
        Ok(key)
    }

    /// A bucket of a calendar exists from now on.
    pub fn open_bucket(&mut self, cal: &Cal, label: i64) {
        if self.domains.entry(cal.clone()).or_default().insert(label) {
            // The running sums' walk reads the domain as a vector.
            self.domain_vecs.retain(|(c, _), _| c != cal);
        }
    }

    /// The label of the bucket of a calendar holding `t`: the first label at
    /// or after it in the schedule.
    pub fn label_on(&self, cal: &Cal, t: i64) -> Option<i64> {
        self.schedule.get(cal)?.range(t..).next().copied()
    }

    /// A tuple keyed at `key` arrived after its bar closed: everything
    /// derived at or after `key` may have read its absence. Derived values
    /// are recomputed on demand; the windowed groups forget the bars from
    /// `key` on. Decisions already emitted stand (`decided` is a log).
    pub fn invalidate_from(&mut self, key: i64) {
        self.memo.clear();
        self.asof_memo.clear();
        self.memo_errors.clear();
        self.memo_stuck.clear();
        for cache in self.windows.values_mut() {
            let keep = std::mem::take(cache);
            *cache = keep.into_iter().filter(|(t, _)| *t < key).collect();
        }
        self.rows_floor.clear();
        self.rows_anchor.clear();
        self.rows_state.clear();
        self.stats.late_tuples += 1;
    }

    /// Drop memo entries no later bar needs (see `memo_horizon`): those keyed
    /// before both `t - horizon` and `keep_from`, except the latest entry of
    /// each relation read through `asof` for its inputs. A dropped entry is
    /// recomputed if asked for, so this bounds memory, never changes results.
    pub fn evict_memo(&mut self, t: i64, keep_from: i64) {
        let Some(h) = self.memo_horizon else { return };
        // A small memo is not worth a pass.
        if self.memo.len() < self.memo_min && self.asof_memo.len() < self.memo_min {
            return;
        }
        let mut cutoff = (t - h).min(keep_from);
        for (res, n) in &self.rows_lookback {
            if let Some(&back) = self.domains.get(res).and_then(|d| d.range(..=t).rev().nth(*n + 3)) {
                cutoff = cutoff.min(back);
            } else {
                return;
            }
        }
        self.memo.evict_before(cutoff, &self.asof_read);
        self.memo_stuck.retain(|k, _| Memo::time_of(k) >= cutoff);
        // The as-of shortcuts are not bucketed: scanned only once they double.
        if self.asof_memo.len() >= self.asof_memo_next {
            self.asof_memo.retain(|(_, _, k), _| *k >= cutoff);
            self.asof_memo_next = (2 * self.asof_memo.len()).max(self.memo_min);
        }
    }

    /// Evict at every bar however small the memo (what tests use to prove
    /// that eviction never changes a result).
    pub fn force_memo_eviction(&mut self) {
        self.memo_min = 0;
        self.asof_memo_next = 0;
    }

    /// Entries in the memo now (reported by `--timing`).
    pub fn memo_len(&self) -> usize {
        self.memo.len()
    }

    /// The kernel's symbol names, by symbol id (identifier order).
    pub fn symbol_names(&self) -> &[String] {
        self.symbols.names()
    }

    pub fn relation_id(&self, name: &str) -> Option<usize> {
        self.rel_ids.get(name).copied()
    }

    pub fn relation_cal(&self, rel: usize) -> Cal {
        self.rels[rel].cal.clone()
    }

    /// The native resolution a stored relation's keys are read at: a catalog
    /// relation's or a series', else the base.
    pub fn relation_native(&self, rel: usize) -> Resolution {
        self.prog.relations.get(&self.rels[rel].name).and_then(|s| s.native).unwrap_or(self.prog.base)
    }

    pub fn is_primitive(&self, rel: usize) -> bool {
        matches!(self.prog.relations[&self.rels[rel].name].kind, Kind::Primitive { .. })
    }

    pub fn program(&self) -> &'p Program {
        self.prog
    }

    /// The decision bars of the run: the decision calendar's domain within
    /// the configured window.
    pub fn decision_bars(&self) -> Vec<i64> {
        let lo = self.cfg.start.unwrap_or(i64::MIN);
        let hi = self.cfg.end.unwrap_or(i64::MAX);
        self.domains.get(&self.prog.decision_cal).map(|d| d.range(lo..=hi).copied().collect()).unwrap_or_default()
    }

    /// The base bars of decision bar `t`'s bucket: `(t, t)` on the base
    /// calendar, else from the bar after the previous label to `t`.
    pub(crate) fn decision_bucket(&self, t: i64) -> (i64, i64) {
        if self.prog.decision_cal == Cal::Base {
            return (t, t);
        }
        let prev = self.domains.get(&self.prog.decision_cal).and_then(|d| d.range(..t).next_back().copied());
        (prev.map(|p| p + 1).unwrap_or(i64::MIN), t)
    }

    /// The session of `sym` holding base bar `t`: its open and label.
    pub fn session_at(&self, sym: Sym, t: i64) -> Option<(i64, i64)> {
        let v = self.sessions.get(&sym)?;
        let i = v.partition_point(|(_, l)| *l < t);
        let (open, label) = *v.get(i)?;
        (open < t && t <= label).then_some((open, label))
    }

    fn rel(&self, name: &str) -> Result<usize, RunError> {
        self.rel_ids.get(name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", name)))
    }

    /// The per-literal counters so far (see `RunResult::literal_counts`).
    pub fn literal_counts(&self) -> &[Vec<(u64, u64, u64)>] {
        &self.literal_counts
    }

    /// The bars of the time domain of a calendar: what a relation on it can
    /// be queried at.
    pub fn bars_at(&self, cal: &Cal) -> Vec<i64> {
        self.domains.get(cal).map(|d| d.iter().copied().collect()).unwrap_or_default()
    }

    /// `t` is a bar of the calendar's domain, or the error naming the
    /// nearest bars (what `explain` and `query` refuse: a label between two
    /// bars would read a phantom bucket).
    pub fn check_bar(&self, cal: &Cal, t: i64) -> Result<(), RunError> {
        let domain = self.domains.get(cal).ok_or_else(|| RunError::Internal(format!("no time domain at {}", cal)))?;
        if domain.contains(&t) {
            Ok(())
        } else {
            Err(RunError::NotABar {
                t,
                cal: self.prog.cal_name(cal),
                before: domain.range(..t).next_back().copied(),
                after: domain.range(t..).next().copied(),
            })
        }
    }

    /// The id of a kernel relation that every program has.
    pub(crate) fn rel_id(&self, name: &str) -> usize {
        self.rel_ids[name]
    }

    /// The tuples of `relation` at bar `t` for the given `+` inputs (in
    /// signature order), after a run: what a study reads back from a run
    /// (data-bundle doc, section 7, "per-run relations on request").
    pub fn query(&mut self, relation: &str, t: i64, inputs: &[Value]) -> Result<Vec<Tuple>, RunError> {
        let rel = self
            .rel_ids
            .get(relation)
            .copied()
            .ok_or_else(|| RunError::Config(format!("`{}` is not a relation of this program", relation)))?;
        let info = self.rels[rel].clone();
        if inputs.len() != info.inputs.len() {
            return Err(RunError::Config(format!("`{}` takes {} input(s), {} given", relation, info.inputs.len(), inputs.len())));
        }
        let width = self.prog.relations[relation].args.len();
        let mut pattern: Vec<Option<Value>> = vec![None; width];
        pattern[info.key_pos] = Some(Value::Time(t));
        for (&i, v) in info.inputs.iter().zip(inputs) {
            pattern[i] = Some(v.clone());
        }
        let found = self.call(rel, &pattern)?;
        Ok(found.iter().map(|(tu, _)| tu.clone()).collect())
    }

    /// Price of `sym` for marking at decision bar `t`: the bar's price, or
    /// the last price seen when the bar has none.
    pub fn price_at(&mut self, sym: Sym, t: i64) -> Option<f64> {
        match self.bar_price(sym, t) {
            Some(p) => Some(p),
            None => self.last_price.get(&sym).copied(),
        }
    }

    /// A field of `sym`'s bar at decision bar `t` from a companion of the
    /// price relation: the tuple at `t`, or over the fine tuples of its
    /// bucket the first (`open`), the largest (`high`) or the smallest (`low`).
    fn bar_field(&self, rel: Option<(usize, usize)>, sym: Sym, t: i64, pick: fn(Option<f64>, f64) -> f64) -> Option<f64> {
        let (id, col) = rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        if info.cal == self.prog.decision_cal {
            return self.stores[id]
                .by_time
                .get(&t)
                .and_then(|tus| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)))
                .and_then(|tu| tu[col].as_f64());
        }
        let (lo, hi) = self.decision_bucket(t);
        let mut acc = None;
        for (_, tus) in self.stores[id].by_time.range(lo..=hi) {
            for tu in tus.iter().filter(|tu| tu[entity_pos] == Value::Equity(sym)) {
                if let Some(v) = tu[col].as_f64() {
                    acc = Some(pick(acc, v));
                }
            }
        }
        acc
    }

    /// The open of `sym`'s bar at `t` (the first fine open of its bucket).
    pub fn bar_open(&self, sym: Sym, t: i64) -> Option<f64> {
        self.bar_field(self.open_rel, sym, t, |acc, v| acc.unwrap_or(v))
    }

    pub fn bar_high(&self, sym: Sym, t: i64) -> Option<f64> {
        self.bar_field(self.high_rel, sym, t, |acc, v| acc.map(|a| a.max(v)).unwrap_or(v))
    }

    pub fn bar_low(&self, sym: Sym, t: i64) -> Option<f64> {
        self.bar_field(self.low_rel, sym, t, |acc, v| acc.map(|a| a.min(v)).unwrap_or(v))
    }

    /// The last decision bar at or after `t` within `t`'s session (the
    /// instrument's own, or its calendar day) at which `sym` has a price:
    /// the close a market-on-close order decided at `t` executes at (the
    /// session's last print; on a daily or coarser calendar, `t` itself).
    pub fn session_last_bar(&self, sym: Sym, t: i64) -> Option<i64> {
        let Some(end) = self.session_close(sym, t) else { return Some(t) };
        let domain = self.domains.get(&Cal::Base)?;
        let bars: Vec<i64> = domain.range(t..=end).rev().copied().collect();
        bars.into_iter().find(|&b| self.has_price(sym, b))
    }

    /// The instant a market-on-close order decided at `t` waits for: the
    /// end of `sym`'s session holding `t` (its label), or, on a base without
    /// sessions, the last base bar of `t`'s day in the schedule (the
    /// calendar is known before the bars are, the prints are not). `None`
    /// where `moc` executes at `t` itself: a daily or coarser base, or a
    /// decision calendar coarser than the base. The session's last print is
    /// known only once the stream has passed this instant (K-4.2: the
    /// session close drives `moc`).
    pub fn session_close(&self, sym: Sym, t: i64) -> Option<i64> {
        if !self.prog.base.is_sub_daily() || self.prog.decision_cal != Cal::Base {
            return None;
        }
        match self.session_at(sym, t) {
            Some((_, label)) => Some(label),
            None => {
                let day_end = time::floor_div(t, time::DAY) * time::DAY + time::DAY - 1;
                Some(self.schedule.get(&Cal::Base).and_then(|d| d.range(t..=day_end).next_back().copied()).unwrap_or(day_end))
            }
        }
    }

    /// Whether `sym` has a print at base bar `t`: a tuple of the price
    /// relation when it is stored, else of the base `close`.
    fn has_price(&self, sym: Sym, t: i64) -> bool {
        let stored = self.price_rel.filter(|&id| self.rels[id].stored).or_else(|| self.rel_ids.get("close").copied());
        let Some(id) = stored else { return false };
        let Some(&entity_pos) = self.rels[id].entity_positions.first() else { return false };
        self.stores[id].by_time.get(&t).map(|tus| tus.iter().any(|tu| tu[entity_pos] == Value::Equity(sym))).unwrap_or(false)
    }

    /// Currency per point per unit held: a future's multiplier, 1 for a share.
    pub fn multiplier(&self, sym: Sym) -> f64 {
        self.contracts.get(&sym).map(|c| c.multiplier).unwrap_or(1.0)
    }

    /// A future: sold short without a borrow, priced back-adjusted.
    pub fn is_future(&self, sym: Sym) -> bool {
        self.contracts.get(&sym).map(|c| c.future).unwrap_or(false)
    }

    /// An option: an instrument with terms (K-5.5.1).
    pub fn is_option(&self, sym: Sym) -> bool {
        self.contracts.get(&sym).is_some_and(|c| c.option.is_some())
    }

    /// A contract (a future or an option): sold short without a borrow,
    /// charged `commission per_contract`.
    pub fn is_contract(&self, sym: Sym) -> bool {
        self.contracts.get(&sym).is_some_and(|c| c.future || c.option.is_some())
    }

    /// A future's commission per contract per side, when it has one.
    pub fn commission_per_contract(&self, sym: Sym) -> Option<f64> {
        self.contracts.get(&sym).and_then(|c| c.commission_per_contract)
    }

    /// Volume of `sym` over decision bar `t` from the configured volume
    /// relation: the tuple at `t`, or the sum of the fine tuples in its bucket.
    pub fn bar_volume(&self, sym: Sym, t: i64) -> Option<f64> {
        // An option's volume is its quote's (K-5.5.3).
        if self.has_options && self.is_option(sym) {
            return self.quote_scan(sym, t).and_then(|q| q.volume);
        }
        let id = self.volume_rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let col = self.volume_col;
        if info.cal == self.prog.decision_cal {
            self.stores[id]
                .by_time
                .get(&t)
                .and_then(|tus| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)))
                .and_then(|tu| tu[col].as_f64())
        } else {
            let (lo, hi) = self.decision_bucket(t);
            let mut sum = None;
            for (_, tus) in self.stores[id].by_time.range(lo..=hi) {
                for tu in tus.iter().filter(|tu| tu[entity_pos] == Value::Equity(sym)) {
                    if let Some(v) = tu[col].as_f64() {
                        sum = Some(sum.unwrap_or(0.0) + v);
                    }
                }
            }
            sum
        }
    }

    /// Average daily volume of `sym` at `bars[end]`: at a daily resolution the
    /// mean bar volume over the `adv_window` bars ending there (inclusive);
    /// at a finer resolution the mean over the `adv_window` days before the
    /// bar's day of each day's summed volume (the current, partial day is
    /// left out; on the first day its volume so far stands in). `None`
    /// without any volume.
    pub fn adv(&self, sym: Sym, bars: &[i64], end: usize) -> Option<f64> {
        let end = end.min(bars.len().saturating_sub(1));
        let n = self.cfg.adv_window.max(1);
        if self.prog.base.is_sub_daily() && self.prog.decision_cal == Cal::Base {
            let today = time::day_key(bars[end]);
            let mut days: Vec<(i64, Option<f64>)> = Vec::new();
            for &t in bars[..=end].iter().rev() {
                let d = time::day_key(t);
                if d == today {
                    continue;
                }
                if days.last().map(|(k, _)| *k != d).unwrap_or(true) {
                    if days.iter().filter(|(_, v)| v.is_some()).count() == n {
                        break;
                    }
                    days.push((d, None));
                }
                if let Some(v) = self.bar_volume(sym, t) {
                    let last = days.last_mut().unwrap();
                    last.1 = Some(last.1.unwrap_or(0.0) + v);
                }
            }
            let sums: Vec<f64> = days.iter().filter_map(|(_, v)| *v).collect();
            if sums.is_empty() {
                let so_far: f64 = bars[..=end].iter().rev().take_while(|&&t| time::day_key(t) == today).filter_map(|&t| self.bar_volume(sym, t)).sum();
                return (so_far > 0.0).then_some(so_far);
            }
            return Some(sums.iter().sum::<f64>() / sums.len() as f64);
        }
        let lo = end.saturating_sub(n - 1);
        let vols: Vec<f64> = bars[lo..=end].iter().filter_map(|&t| self.bar_volume(sym, t)).collect();
        if vols.is_empty() {
            None
        } else {
            Some(vols.iter().sum::<f64>() / vols.len() as f64)
        }
    }

    /// Realized volatility of `sym` at `bars[end]`: the sample standard
    /// deviation of its log returns over the `vol_window` bars ending there
    /// (inclusive), from the price relation; `None` with fewer than
    /// `vol_min_obs` returns (a bar without a price yields no return).
    pub fn realized_vol(&mut self, sym: Sym, bars: &[i64], end: usize) -> Option<f64> {
        if self.cfg.slippage_vol_mult == 0.0 {
            return None;
        }
        let lo = end.saturating_sub(self.cfg.vol_window);
        let mut rets = Vec::new();
        let mut prev: Option<f64> = None;
        for &t in &bars[lo..=end.min(bars.len() - 1)] {
            let p = self.bar_price(sym, t);
            if let (Some(a), Some(b)) = (prev, p) {
                // A back-adjusted future may cross zero: no log return there.
                if a > 0.0 && b > 0.0 {
                    rets.push((b / a).ln());
                }
            }
            if p.is_some() {
                prev = p;
            }
        }
        if rets.len() < self.cfg.vol_min_obs.max(2) {
            return None;
        }
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        Some((rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt())
    }

    /// Price of `sym` at decision bar `t` from the configured price relation:
    /// the tuple at `t`, or the last fine tuple in its bucket; `None` when the
    /// bar has no price, so that the executor drops rather than fills.
    pub fn bar_price(&mut self, sym: Sym, t: i64) -> Option<f64> {
        // An option is marked at its quote (K-5.5.2).
        let found = if self.has_options && self.is_option(sym) {
            self.option_mark(sym, t).map(|(p, _)| p)
        } else {
            self.bar_price_quiet(sym, t)
        };
        if let Some(p) = found {
            self.last_price.insert(sym, p);
        }
        found
    }

    /// `bar_price` without remembering the price as the last one seen.
    pub(crate) fn bar_price_quiet(&mut self, sym: Sym, t: i64) -> Option<f64> {
        let id = self.price_rel?;
        let info = self.rels[id].clone();
        let entity_pos = *info.entity_positions.first()?;
        let col = self.price_col;
        let found = if !info.stored {
            // A derived price relation (a total-return index): its tuple at
            // the decision bar, evaluated on demand.
            let width = self.prog.relations[&info.name].args.len();
            let mut pattern: Vec<Option<Value>> = vec![None; width];
            pattern[info.key_pos] = Some(Value::Time(t));
            pattern[entity_pos] = Some(Value::Equity(sym));
            match self.call(id, &pattern) {
                Ok(tuples) => tuples.first().and_then(|(tu, _)| tu[col].as_f64()),
                Err(_) => None,
            }
        } else if info.cal == self.prog.decision_cal {
            let key = Value::Equity(sym);
            self.stores[id].lookup(t, Some(&key)).iter().find(|tu| tu[entity_pos] == key).and_then(|tu| tu[col].as_f64())
        } else {
            let (lo, hi) = self.decision_bucket(t);
            self.stores[id]
                .by_time
                .range(lo..=hi)
                .rev()
                .find_map(|(_, tus)| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)).and_then(|tu| tu[col].as_f64()))
        };
        found
    }

    /// The bracketed inputs of a rule: none in the linked program, where
    /// every template is instantiated.
    /// Parse a command-line value as an input of type `ty`: an equity by
    /// identifier, a timestamp, or a literal in the DSL's own grammar.
    pub fn parse_input(&self, ty: &Ty, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        match ty {
            Ty::Entity => self.equity(raw.trim_matches('"')),
            Ty::Label => {
                let name = raw.trim_matches('"');
                self.labels.get(name).map(Value::Label).ok_or_else(|| format!("`{}` is not a label of the dataset", name))
            }
            Ty::Timestamp => time::parse_timestamp(raw)
                .map(Value::Time)
                .ok_or_else(|| format!("`{}` is not a Timestamp (YYYY-MM-DD[THH:MM[:SS]])", raw)),
            Ty::Count => raw.parse().map(Value::Count).map_err(|_| format!("`{}` is not a Count", raw)),
            Ty::Duration => match crate::syntax::parse_lit(raw) {
                Ok(Lit::Duration(d)) => Ok(Value::Dur(Box::new(d))),
                _ => Err(format!("`{}` is not a Duration (such as 20d, 3mo or 1y)", raw)),
            },
            Ty::Quantity(_) => match crate::syntax::parse_lit(raw) {
                Ok(l @ (Lit::Int(_) | Lit::Float(_))) => Ok(Value::Num(l_num(&l))),
                Ok(l @ (Lit::Quantity(..) | Lit::Money(..) | Lit::Price(..))) if crate::check::types::compat(ty, &l.ty()) => Ok(Value::Num(l_num(&l))),
                Ok(l) => Err(format!("`{}` is {}, not {}", raw, l.ty(), ty)),
                Err(_) => Err(format!("`{}` is not a {}", raw, ty)),
            },
            Ty::Decision | Ty::IntLit | Ty::StrLit | Ty::DimVar(_) => Err(format!("a {} cannot be given on the command line", ty)),
        }
    }

    /// Parse a command-line value for a body variable, whose type is not
    /// declared: an equity of the dataset, a timestamp, or a literal (a bare
    /// integer is a Count; write a quantity with a decimal point or a unit).
    pub fn parse_binding(&self, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        if let Ok(v) = self.equity(raw) {
            return Ok(v);
        }
        if let Some(t) = time::parse_timestamp(raw) {
            return Ok(Value::Time(t));
        }
        match crate::syntax::parse_lit(raw) {
            Ok(Lit::Str(name) | Lit::Entity(name)) => self.equity(&name),
            Ok(Lit::Time(t)) => Ok(Value::Time(t)),
            Ok(Lit::Label(name)) => self.labels.get(&name).map(Value::Label).ok_or_else(|| format!("`{}` is not a label of the dataset", name)),
            Ok(Lit::Int(i)) => Ok(Value::Count(i)),
            Ok(Lit::Duration(d)) => Ok(Value::Dur(Box::new(d))),
            Ok(l) => Ok(Value::Num(l_num(&l))),
            Err(_) => Err(format!("`{}` is not an equity of the dataset, a timestamp or a literal", raw)),
        }
    }

    /// A security by id, or by the ticker it carries at the bundle date.
    fn equity(&self, name: &str) -> Result<Value, String> {
        if let Some(s) = self.symbols.get(name) {
            return Ok(Value::Equity(s));
        }
        if self.securities.is_empty() {
            return Err(format!("`{}` is not an equity of the dataset", name));
        }
        resolve_ticker(&self.securities, name, self.as_of.unwrap_or(0)).map(Value::Equity)
    }

    /// Why rule `rule_idx` did or did not fire at `t` (section 7): the first
    /// body literal with no solution. `inputs` supplies the rule's `+` head
    /// arguments, in signature order (empty for a decide rule).
    pub fn explain(&mut self, rule_idx: usize, t: i64, inputs: &[Value]) -> Result<Explanation, RunError> {
        self.explain_with(rule_idx, t, inputs, &[])
    }

    /// `explain` with body variables pre-bound: the explanation is then about
    /// the bindings that agree with them (one instrument, say) rather than
    /// about the union of every binding.
    /// A rule's instance at a bar as evaluation enters its body: the head's
    /// key and inputs bound, and the variables pre-bound (with their shown
    /// values). Only bars of the rule's time domain are evaluated.
    #[allow(clippy::type_complexity)]
    fn entry(&mut self, rule_idx: usize, t: i64, inputs: &[Value], bindings: &[(String, Value)]) -> Result<(Rc<CompiledRule>, Env, Vec<(String, String)>), RunError> {
        let rule = &self.prog.rules[rule_idx];
        let cr = self.compiled[rule_idx].clone();
        let rel = self.rel(&rule.head.name)?;
        let info = self.rels[rel].clone();
        // Only bars of the rule's time domain are ever evaluated (section 6);
        // a label between two bars would read a phantom bucket.
        let cal = self.prog.infos[rule_idx].cal.clone();
        let domain = self.domains.get(&cal).ok_or_else(|| RunError::Internal(format!("no time domain at {}", cal)))?;
        if !domain.contains(&t) && !cal.per_entity() {
            return Err(RunError::NotABar {
                t,
                cal: self.prog.cal_name(&cal),
                before: domain.range(..t).next_back().copied(),
                after: domain.range(t..).next().copied(),
            });
        }
        let mut env: Env = vec![None; cr.nslots];
        if let Term::Var(v, _) = &rule.head.terms[info.key_pos] {
            env[cr.slots[v]] = Some(Value::Time(t));
        }
        if inputs.len() != info.inputs.len() {
            return Err(RunError::Request(format!("rule {} takes {} inputs, {} given", cr.label, info.inputs.len(), inputs.len())));
        }
        for (pos, val) in info.inputs.iter().zip(inputs) {
            if let Term::Var(v, _) = &rule.head.terms[*pos] {
                env[cr.slots[v]] = Some(val.clone());
            }
        }
        let mut shown = Vec::with_capacity(bindings.len());
        for (var, val) in bindings {
            let Some(&slot) = cr.slots.get(var) else {
                let mut vars: Vec<&String> = cr.slots.keys().collect();
                vars.sort();
                return Err(RunError::Request(format!(
                    "rule {} has no variable `{}`; its variables are {}",
                    cr.label,
                    var,
                    vars.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                )));
            };
            if let Some(x) = &env[slot] {
                if x != val {
                    return Err(RunError::Request(format!(
                        "`{}` is already {} in rule {} and cannot also be {}",
                        var,
                        self.show(x),
                        cr.label,
                        self.show(val)
                    )));
                }
            }
            env[slot] = Some(val.clone());
            shown.push((var.clone(), self.show(val)));
        }
        Ok((cr, env, shown))
    }

    /// Every solution of a rule's body at a bar, its variables by name, with
    /// the instances left stuck on a partial operation (a query's rows,
    /// V-8.3).
    #[allow(clippy::type_complexity)]
    pub fn solutions(&mut self, rule_idx: usize, t: i64, bindings: &[(String, Value)]) -> Result<(Vec<Vec<(String, Value)>>, Vec<RunError>), RunError> {
        let (cr, env, _) = self.entry(rule_idx, t, &[], bindings)?;
        let order = self.plan_order(rule_idx);
        let body = &self.prog.rules[rule_idx].body;
        let (sols, stuck) = self.solve_kleene(&cr, body, &order, vec![env], Some(rule_idx))?;
        let mut names: Vec<(&String, usize)> = cr.slots.iter().map(|(n, &s)| (n, s)).collect();
        names.sort();
        let rows = sols
            .iter()
            .map(|e| names.iter().filter_map(|(n, s)| e[*s].as_ref().map(|v| ((*n).clone(), v.clone()))).collect())
            .collect();
        Ok((rows, stuck.into_iter().map(|s| s.error).collect()))
    }

    /// Evaluate in every rule's written order rather than the plan's.
    pub fn set_written_order(&mut self, on: bool) {
        self.written_order = on;
    }

    /// The kernel's calendars, finer first and the decision calendar last.
    pub fn calendars(&self) -> Vec<Cal> {
        bind::calendars(self).into_iter().map(|(c, _)| c).collect()
    }

    pub fn explain_with(&mut self, rule_idx: usize, t: i64, inputs: &[Value], bindings: &[(String, Value)]) -> Result<Explanation, RunError> {
        let (cr, env, shown) = self.entry(rule_idx, t, inputs, bindings)?;
        let rule = &self.prog.rules[rule_idx];
        // Literal by literal in the written order, under Kleene conjunction
        // (`solve_kleene`): the literal reported is the one at which the last
        // instance was excluded, stuck instances included; an instance stuck
        // to the end is the halt the run would have met.
        let mut envs = vec![env];
        let mut stuck: Vec<eval::Stuck> = Vec::new();
        let order = self.plan_order(rule_idx);
        for &i in order.iter() {
            let lit = &rule.body[i];
            let before = envs.len() + stuck.len();
            let entering: Option<Env> = envs.first().cloned().or_else(|| stuck.first().map(|s| s.env.clone()));
            let (next, still) = self.solve_kleene_from(&cr, &rule.body, &[i], envs, stuck, None)?;
            if next.is_empty() && still.is_empty() && before > 0 {
                let shown_lit = self.source.rules.get(rule_idx).and_then(|r| r.body.get(i)).unwrap_or(lit);
                let mut at_failure: Vec<(String, String)> = entering
                    .map(|e| {
                        cr.slots
                            .iter()
                            .filter_map(|(name, &slot)| e.get(slot).and_then(|v| v.as_ref()).map(|v| (name.clone(), self.show(v))))
                            .collect()
                    })
                    .unwrap_or_default();
                at_failure.sort();
                return Ok(Explanation {
                    rule: cr.label.clone(),
                    t,
                    bindings: shown,
                    solutions: 0,
                    failed_at: Some((i, shown_lit.describe())),
                    solution: Vec::new(),
                    at_failure,
                });
            }
            envs = next;
            stuck = still;
        }
        if envs.is_empty() {
            if let Some(s) = stuck.into_iter().next() {
                return Err(s.error);
            }
        }
        let mut solution: Vec<(String, String)> = cr.slots.iter().filter_map(|(name, &slot)| envs[0][slot].as_ref().map(|v| (name.clone(), self.show(v)))).collect();
        solution.sort();
        Ok(Explanation {
            rule: cr.label.clone(),
            t,
            bindings: shown,
            solutions: envs.len(),
            failed_at: None,
            solution,
            at_failure: Vec::new(),
        })
    }
}

/// Whether any `lag` in `unit`'s rules takes parameter `name` as its length.
fn lag_uses_param(prog: &Program, unit: &str, name: &str) -> bool {
    fn lit(l: &Literal, name: &str) -> bool {
        match l {
            Literal::Builtin(Builtin::Lag { n: Expr::Param(p, _), .. }, _) => p == name,
            Literal::Agg { conj, .. } => conj.iter().any(|c| lit(c, name)),
            Literal::Case { branches, .. } => branches.iter().any(|b| b.guard.iter().any(|g| lit(g, name))),
            _ => false,
        }
    }
    prog.rules.iter().filter(|r| r.unit == unit).any(|r| r.body.iter().any(|l| lit(l, name)))
}

fn l_num(l: &Lit) -> f64 {
    match l {
        Lit::Int(i) => *i as f64,
        Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => *x,
        Lit::Time(t) => *t as f64,
        Lit::Duration(_) | Lit::Str(_) | Lit::Entity(_) | Lit::Label(_) => f64::NAN,
    }
}

/// The magnitude a literal is ranged by: its number, or a duration's
/// approximate length in days; an entity has none.
fn lit_magnitude(l: &Lit) -> Option<f64> {
    match l {
        Lit::Duration(d) => Some(d.approx_days()),
        Lit::Str(_) | Lit::Entity(_) | Lit::Label(_) => None,
        _ => Some(l_num(l)),
    }
}

fn shares_column(sig: &Signature) -> Option<usize> {
    sig.args
        .iter()
        .position(|a| a.mode == Mode::Out && matches!(&a.ty, Ty::Quantity(d) if d.c2 == 0 && d.s2 == 2 && d.t2 == 0))
}

fn price_column(sig: &Signature) -> Option<usize> {
    sig.args
        .iter()
        .position(|a| a.mode == Mode::Out && matches!(&a.ty, Ty::Quantity(d) if d.c2 == 2 && d.s2 == -2 && d.t2 == 0))
}

pub(crate) fn lit_value(l: &Lit, symbols: &mut Symbols, labels: &mut Symbols, equities: &HashMap<String, Sym>) -> Value {
    match l {
        Lit::Int(i) => Value::Count(*i),
        Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Value::Num(*x),
        Lit::Duration(d) => Value::Dur(Box::new(*d)),
        Lit::Time(t) => Value::Time(*t),
        // A ticker literal is what `Kernel::new` resolved it to; one it did
        // not see (an unchecked program) reads as its own id.
        Lit::Str(s) | Lit::Entity(s) => Value::Equity(equities.get(s).copied().unwrap_or_else(|| symbols.intern(s))),
        Lit::Label(s) => Value::Label(labels.intern(s)),
    }
}

fn for_each_lit(rule: &Rule, f: &mut dyn FnMut(&Lit)) {
    fn term(t: &Term, f: &mut dyn FnMut(&Lit)) {
        match t {
            Term::Lit(l, _) => f(l),
            Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term(s, f)),
            _ => {}
        }
    }
    fn expr(e: &Expr, f: &mut dyn FnMut(&Lit)) {
        match e {
            Expr::Lit(l, _) => f(l),
            Expr::Neg(x, _) => expr(x, f),
            Expr::Bin(_, a, b, _) => {
                expr(a, f);
                expr(b, f);
            }
            Expr::Call(_, args, _) => args.iter().for_each(|a| expr(a, f)),
            _ => {}
        }
    }
    fn lit(l: &Literal, f: &mut dyn FnMut(&Lit)) {
        match l {
            Literal::Atom(a) | Literal::Neg(a) => a.terms.iter().for_each(|t| term(t, f)),
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    term(t, f);
                    term(t1, f);
                }
                Builtin::Lag { t, n, t1 } => {
                    term(t, f);
                    expr(n, f);
                    term(t1, f);
                }
                Builtin::MonthStart { t } | Builtin::DayStart { t } => term(t, f),
                Builtin::SessionOf { a, t, s } => {
                    term(a, f);
                    term(t, f);
                    term(s, f);
                }
            },
            Literal::Window { dur, min, lo, .. } => {
                expr(dur, f);
                expr(min, f);
                if let Some(lo) = lo {
                    expr(lo, f);
                }
            }
            Literal::Cmp { lhs, rhs, .. } => {
                expr(lhs, f);
                expr(rhs, f);
            }
            Literal::Assign { expr: e, .. } => expr(e, f),
            Literal::Agg { args, conj, .. } => {
                args.iter().for_each(|a| expr(a, f));
                conj.iter().for_each(|c| lit(c, f));
            }
            Literal::Top { n, atom, .. } => {
                if let Some(n) = n {
                    expr(n, f);
                }
                atom.terms.iter().for_each(|t| term(t, f));
            }
            Literal::AsOf { atom, at, .. } => {
                atom.terms.iter().for_each(|t| term(t, f));
                term(at, f);
            }
            Literal::Case { branches, otherwise, .. } => {
                for b in branches {
                    b.guard.iter().for_each(|c| lit(c, f));
                    expr(&b.value, f);
                }
                if let Some(e) = otherwise {
                    expr(e, f);
                }
            }
        }
    }
    rule.head.terms.iter().for_each(|t| term(t, f));
    rule.body.iter().for_each(|l| lit(l, f));
}

fn collect_vars(rule: &Rule, f: &mut dyn FnMut(&str)) {
    fn term(t: &Term, f: &mut dyn FnMut(&str)) {
        match t {
            Term::Var(v, _) => f(v),
            Term::Ctor(_, subs, _) => subs.iter().for_each(|s| term(s, f)),
            _ => {}
        }
    }
    fn expr(e: &Expr, f: &mut dyn FnMut(&str)) {
        let mut vs = Vec::new();
        e.vars(&mut vs);
        for (v, _) in vs {
            f(&v);
        }
    }
    fn lit(l: &Literal, f: &mut dyn FnMut(&str)) {
        match l {
            Literal::Atom(a) | Literal::Neg(a) => a.terms.iter().for_each(|t| term(t, f)),
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    term(t, f);
                    term(t1, f);
                }
                Builtin::Lag { t, n, t1 } => {
                    term(t, f);
                    expr(n, f);
                    term(t1, f);
                }
                Builtin::MonthStart { t } | Builtin::DayStart { t } => term(t, f),
                Builtin::SessionOf { a, t, s } => {
                    term(a, f);
                    term(t, f);
                    term(s, f);
                }
            },
            Literal::Window { var, base, dur, min, lo, .. } => {
                term(var, f);
                term(base, f);
                expr(dur, f);
                expr(min, f);
                if let Some(lo) = lo {
                    expr(lo, f);
                }
            }
            Literal::Cmp { lhs, rhs, .. } => {
                expr(lhs, f);
                expr(rhs, f);
            }
            Literal::Assign { var, expr: e, .. } => {
                f(var);
                expr(e, f);
            }
            Literal::Agg { var, args, conj, .. } => {
                f(var);
                args.iter().for_each(|a| expr(a, f));
                conj.iter().for_each(|c| lit(c, f));
            }
            Literal::Top { n, atom, by, rank, .. } => {
                if let Some(n) = n {
                    expr(n, f);
                }
                atom.terms.iter().for_each(|t| term(t, f));
                if let Some(by) = by {
                    by.iter().for_each(|(k, _, _)| f(k));
                }
                if let Some((k, _, _)) = rank {
                    f(k);
                }
            }
            Literal::AsOf { atom, at, .. } => {
                atom.terms.iter().for_each(|t| term(t, f));
                term(at, f);
            }
            Literal::Case { var, branches, otherwise, .. } => {
                f(var);
                for b in branches {
                    b.guard.iter().for_each(|c| lit(c, f));
                    expr(&b.value, f);
                }
                if let Some(e) = otherwise {
                    expr(e, f);
                }
            }
        }
    }
    rule.head.terms.iter().for_each(|t| term(t, f));
    rule.body.iter().for_each(|l| lit(l, f));
}

/// A live run's deadline when none is given (K-6.3): one bar of its
/// decision calendar (a day for a civil calendar or a session).
pub fn deadline_of(prog: &Program) -> i64 {
    match (&prog.decision_cal, prog.base.nanos()) {
        (Cal::Base, Some(n)) => n,
        (Cal::Weeks(w), _) => *w as i64 * 7 * time::DAY,
        (Cal::Months(m), _) => *m as i64 * 30 * time::DAY,
        (Cal::Years(y), _) => *y as i64 * 365 * time::DAY,
        _ => time::DAY,
    }
}

/// The clock a live run measures its lag against (K-6.3): the data layer's,
/// the wall clock unless a test shares another.
#[derive(Clone)]
pub struct LiveClock(pub Arc<dyn Fn() -> i64 + Send + Sync>);

impl LiveClock {
    pub fn wall() -> LiveClock {
        LiveClock(Arc::new(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
        }))
    }

    pub fn now(&self) -> i64 {
        (self.0)()
    }
}

impl std::fmt::Debug for LiveClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LiveClock")
    }
}

/// The executor mode of a run (K-6.2): the stream it reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    /// The data layer's replay of what it holds (D-5.5).
    #[default]
    ReplaySim,
    /// A live session's stream (D-5.2 live), recorded as it is read.
    LiveSim { session: String },
}

/// How a run reads its stream (K-4.2, K-4.7): where it stops, whether it
/// takes checkpoints, and the checkpoint it resumes from.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    /// The executor mode (K-6.2).
    pub mode: RunMode,
    /// A live-sim run whose recorded stream this run replays (D-5.5): its
    /// session's versions with the barriers that run's stream emitted.
    pub replay_of: Option<String>,
    /// Replay at the data layer's clock's pace (`--paced`, D-5.5).
    pub paced: bool,
    /// The clock a live run measures its lag against (the wall clock when
    /// none).
    pub clock: Option<LiveClock>,
    /// How long after its barrier a decision bar may be evaluated (K-6.3);
    /// one bar of the decision calendar when none.
    pub deadline: Option<i64>,
    /// Stop after the event with this seq (a truncated stream, G5).
    pub to_seq: Option<u64>,
    /// Stop after the decision at this bar: what the data received by its
    /// barrier decides (the stream truncated at that barrier's seq).
    pub horizon: Option<i64>,
    pub checkpoint_every: Option<CheckpointEvery>,
    pub from: Option<Checkpoint>,
    /// Record the windowed groups' summaries (the `summary` state table).
    pub record_summaries: bool,
    /// Evict the memo at every bar however small (tests that prove eviction
    /// changes nothing).
    pub force_memo_eviction: bool,
    /// Write the run's outputs through the data layer at every checkpoint
    /// (the rows the run gained since the last) and when it ends or halts
    /// (K-7.1).
    pub write_outputs: bool,
    /// π, the plan step that asked for the run (Q-2.5): part of its id.
    pub plan_step: Option<serde_json::Value>,
}

/// The fold over a data service (K-4.1, K-4.2): Bind, the calendars put
/// under the run's namespace, the subscription, and every event folded with
/// `exec`; returns the run and the kernel at the end of the stream, and
/// hands each checkpoint taken to `on_checkpoint`.
pub fn replay<'p>(
    prog: &'p Program,
    lowered: Option<&'p crate::lower::Lowered>,
    client: &DataClient,
    cfg: ExecConfig,
    exec: &mut dyn Executor,
    opts: &Replay,
    on_checkpoint: &mut dyn FnMut(Checkpoint),
) -> Result<(RunResult, Kernel<'p>), RunError> {
    replay_observed(prog, lowered, client, cfg, exec, opts, on_checkpoint, &mut |_, _, _| Ok(Next::Continue))
}

/// What a run reports as it goes (the kernel service's progress frames and
/// its queries between barriers, K-7.3): its identity once it is bound,
/// then every decision barrier it closes.
#[derive(Clone, Debug)]
pub enum Progress {
    Started { run_id: String },
    Barrier { t: i64 },
}

/// What the run does after a progress report: go on, or take a checkpoint
/// at the end of the current decision bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Continue,
    Checkpoint,
}

/// What a run of the program would read now, without a run (P-3.4's `show
/// binding`, E-59): the binding, its relations' versions, and the decision
/// calendar's bars, the sample a plan places its hold-out and its
/// walk-forward folds in (Q-2.3).
pub fn sample(prog: &Program, client: &DataClient, cfg: &ExecConfig) -> Result<serde_json::Value, RunError> {
    let bound = bind::bind_at(prog, client, &bind::point_of(prog, &cfg.param_overrides))?;
    let probe = Kernel::build(prog, &bound, cfg.clone())?;
    let labels: Vec<i64> = bind::calendars(&probe).pop().map(|(_, l)| l).unwrap_or_default();
    let relations: Vec<serde_json::Value> = bound
        .binding
        .pairs
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|p| serde_json::json!({"relation": p.relation, "version": p.version}))
        .collect();
    Ok(serde_json::json!({
        "program_hash": crate::compiler::normalise(prog).hash,
        "binding": bound.binding.id,
        "relations": relations,
        "calendar": prog.cal_name(&prog.decision_cal),
        "bars": labels.len(),
        "first": labels.first(),
        "last": labels.last(),
    }))
}

/// `replay`, handing `on_progress` the kernel and the run so far when the
/// run is bound and after every decision barrier; an error it returns halts
/// the run there.
#[allow(clippy::too_many_arguments)]
pub fn replay_observed<'p>(
    prog: &'p Program,
    lowered: Option<&'p crate::lower::Lowered>,
    client: &DataClient,
    mut cfg: ExecConfig,
    exec: &mut dyn Executor,
    opts: &Replay,
    on_checkpoint: &mut dyn FnMut(Checkpoint),
    on_progress: &mut dyn FnMut(Progress, &mut Kernel<'p>, &RunResult) -> Result<Next, RunError>,
) -> Result<(RunResult, Kernel<'p>), RunError> {
    let mut bound = bind::bind_at(prog, client, &bind::point_of(prog, &cfg.param_overrides))?;
    // A live-sim run's bars are its session's plan, which its data does not
    // hold yet; a replay of one reads the bars its stream recorded (K-6.2,
    // D-5.5). The data layer would answer a pushed aggregate at the bucket's
    // end, not at the instant a live barrier was emitted: both evaluate
    // every aggregate (E-69).
    let mut recorded = None;
    if opts.replay_of.is_some() && opts.mode != RunMode::ReplaySim {
        return Err(RunError::Config("a replay of a recorded run is a replay-sim run".into()));
    }
    if let RunMode::LiveSim { session } = &opts.mode {
        if opts.horizon.is_some() || opts.to_seq.is_some() || opts.from.is_some() {
            return Err(RunError::Config("a live-sim run reads its session from now on: no horizon, truncation or resumption".into()));
        }
        bound.base_bars = bind::session_bars(client, session)?;
        cfg.pushdown = false;
    }
    if let Some(run) = &opts.replay_of {
        let (clock, bars) = bind::recorded_clock(client, run)?;
        bound.base_bars = bars;
        recorded = Some(clock);
        cfg.pushdown = false;
    }
    // The preconditions, before the first barrier (K-3.2).
    let mut contract = crate::contract::contract(prog);
    let pre_warnings = crate::contract::check::preconditions(&mut contract, prog, &bound, client, &cfg)?;
    let periods = report_periods(prog, &bound, client)?;
    // A truncated stream resolves ticker literals where it ends.
    let probe = Kernel::build(lowered.map(|l| l.program_for(prog)).unwrap_or(prog), &bound, cfg.clone())?;
    let identity = Identity {
        program_hash: crate::compiler::normalise(prog).hash,
        binding_id: bound.binding.id.clone().unwrap_or_default(),
        mode: match &opts.mode {
            RunMode::ReplaySim => "replay-sim".into(),
            RunMode::LiveSim { .. } => "live-sim".into(),
        },
        run_id: String::new(),
        // Through a JSON value: a field that is infinite (no leverage
        // limit) is null there, and canonical JSON has no infinity.
        config: abt_wire::Hash::of(
            abt_wire::to_canonical_json(&serde_json::to_value(&cfg).map_err(|e| RunError::Internal(e.to_string()))?)
                .map_err(|e| RunError::Internal(e.to_string()))?
                .as_bytes(),
        )
        .hex(),
    };
    let run = bind::run_id(
        &identity.program_hash,
        &identity.binding_id,
        &bind::plan_step(opts.plan_step.as_ref(), &cfg, opts.to_seq, opts.horizon),
        &identity.mode,
    );
    let identity = Identity { run_id: run.clone(), ..identity };
    let (mut spec, cal_of) = bind::stream_spec(&probe, &bound, client, &run)?;
    if let RunMode::LiveSim { session } = &opts.mode {
        spec.clock = abt_wire::generated::abt::wire::ClockSource::Live;
        spec.session = Some(session.clone());
    }
    if let Some(clock) = recorded {
        bind::with_recorded_clock(&mut spec, clock);
    }
    let mut to_seq = opts.to_seq;
    let mut end_label: Option<i64> = None;
    if opts.horizon.is_some() || opts.to_seq.is_some() {
        let dcal = prog.decision_cal.to_string();
        let index = client.stream_index(spec.clone())?;
        if let Some(h) = opts.horizon {
            let at = index.iter().rfind(|r| r.calendar == dcal && r.label <= h);
            to_seq = Some(to_seq.map_or(at.map(|r| r.seq).unwrap_or(0), |s| s.min(at.map(|r| r.seq).unwrap_or(0))));
        }
        end_label = index.iter().filter(|r| to_seq.is_none_or(|s| r.seq <= s)).map(|r| r.label).max();
    }
    drop(probe);
    if let Some(end) = end_label.or(opts.horizon) {
        cfg.as_of = Some(cfg.as_of.or(bound.bundle_date()).map_or(end, |a| a.min(end)));
    }
    let mut kernel = match lowered {
        Some(l) => Kernel::bound_planned(prog, l, &bound, cfg.clone())?,
        None => Kernel::bound(prog, &bound, cfg.clone())?,
    };
    kernel.data_ids = outputs::instrument_ids(&kernel, &bound);
    kernel.identity = identity;
    // A literal the data layer does not answer is evaluated here: the run
    // says so (E-47).
    for message in bind::push_down(&mut kernel, &bound, client, &spec)? {
        kernel.run_warnings.push(RunWarning { bias: "pushdown".into(), message });
    }
    kernel.run_warnings.extend(pre_warnings);
    let mut decoder = bind::Decoder::new(&kernel, &bound, cal_of);
    let mut fold = match &opts.from {
        Some(cp) => Fold::restore(kernel, exec, cp.clone())?,
        None => Fold::new(kernel, exec),
    }
    .with_contract(contract);
    if let Some(e) = opts.checkpoint_every {
        fold = fold.with_checkpoints(e);
    }
    if matches!(opts.mode, RunMode::LiveSim { .. }) {
        fold = fold.with_live(fold::Live {
            clock: opts.clock.clone().unwrap_or_else(LiveClock::wall),
            deadline: opts.deadline.unwrap_or_else(|| deadline_of(prog)),
            fraction: 0.01,
        });
    }
    if opts.record_summaries {
        fold.kernel.record_summaries(true);
    }
    if opts.force_memo_eviction {
        fold.kernel.force_memo_eviction();
    }
    let from = opts.from.as_ref().map(|c| c.cursor).unwrap_or(1);
    let mut halt: Option<RunError> = None;
    // How far the outputs are written: a resumed run writes its whole
    // history at its first write (rows already there are the same rows).
    let mut written = outputs::Written::default();
    {
        let (k, r) = fold.parts();
        on_progress(Progress::Started { run_id: run.clone() }, k, r)?;
    }
    if to_seq.is_none_or(|t| t >= from) {
        let mut sub = client.subscribe_paced(spec, from, to_seq, opts.paced)?;
        'stream: while let Some(b) = sub.next_batch()? {
            for ev in decoder.events(&mut fold.kernel, &bound, &b)? {
                let before = fold.last_bar();
                if let Err(e) = fold.step(ev.into()) {
                    halt = Some(e);
                    break 'stream;
                }
                if let Some(cp) = fold.take_checkpoint() {
                    // The outputs incrementally at every checkpoint (K-7.1),
                    // before the checkpoint: a run resumed from it finds them.
                    if opts.write_outputs {
                        let rows = outputs::growing(&run, prog, &fold.kernel.data_ids, &cp.result, written)
                            .and_then(|(t, upto)| outputs::write(client, t.into_iter().filter(|(_, b)| b.num_rows() > 0).collect()).map(|_| upto));
                        match rows {
                            Ok(upto) => written = upto,
                            Err(e) => {
                                halt = Some(e);
                                break 'stream;
                            }
                        }
                    }
                    on_checkpoint(cp);
                }
                if let Some(t) = fold.last_bar().filter(|t| before != Some(*t)) {
                    let (k, r) = fold.parts();
                    match on_progress(Progress::Barrier { t }, k, r) {
                        Ok(Next::Continue) => {}
                        Ok(Next::Checkpoint) => fold.request_checkpoint(),
                        Err(e) => {
                            halt = Some(e);
                            break 'stream;
                        }
                    }
                }
            }
        }
    }
    // The run's outputs, under its namespace (K-7.1): a halted run writes
    // what it has, and its halt is the error whether or not they could be.
    if let Some(e) = halt {
        if opts.write_outputs {
            let mut partial = fold.result().clone();
            partial.run_id = run.clone();
            partial.binding_id = bound.binding.id.clone().unwrap_or_default();
            partial.periods = periods;
            let ids = fold.kernel.data_ids.clone();
            let put = outputs::tables(&run, prog, &bound, &ids, &fold.kernel.cfg, &partial, &format!("halted({})", e), written).and_then(|t| outputs::write(client, t));
            if let Err(w) = put {
                return Err(RunError::Internal(format!("{} (and the halted run's outputs could not be written: {})", e, w)));
            }
        }
        return Err(e);
    }
    let (mut result, mut kernel) = fold.finish()?;
    result.run_id = run.clone();
    result.binding_id = bound.binding.id.clone().unwrap_or_default();
    result.periods = periods;
    if !opts.write_outputs {
        if !prog.observes.is_empty() {
            result.contract.set(
                "post.ledgers",
                crate::contract::Status::NotApplicable {
                    why: "the run writes no outputs (a run over a data service writes them)".into(),
                },
            );
        }
        return Ok((result, kernel));
    }
    let ids = kernel.data_ids.clone();
    let (ledgers, notes) = outputs::ledgers(&run, prog, &mut kernel, &ids, &result)?;
    result.warnings.extend(notes.into_iter().map(|message| RunWarning { bias: "observation".into(), message }));
    if !prog.observes.is_empty() {
        result.contract.set("post.ledgers", crate::contract::Status::Checked { times: ledgers.len() as u64 });
    }
    let mut tables = outputs::tables(&run, prog, &bound, &ids, &kernel.cfg, &result, "completed", written)?;
    tables.extend(ledgers);
    outputs::write(client, tables)?;
    Ok((result, kernel))
}

/// A kernel holding every tuple of the stream and every bucket opened, with
/// no decision taken (what the query commands read the model from).
pub fn load<'p>(prog: &'p Program, lowered: Option<&'p crate::lower::Lowered>, client: &DataClient, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
    let bound = bind::bind(prog, client)?;
    let mut kernel = match lowered {
        Some(l) => Kernel::bound_planned(prog, l, &bound, cfg.clone())?,
        None => Kernel::bound(prog, &bound, cfg.clone())?,
    };
    kernel.data_ids = outputs::instrument_ids(&kernel, &bound);
    let program_hash = crate::compiler::normalise(prog).hash;
    let run = bind::run_id(&program_hash, bound.binding.id.as_deref().unwrap_or(""), &serde_json::Value::Null, "replay-sim");
    let (spec, cal_of) = bind::stream_spec(&kernel, &bound, client, &run)?;
    let mut decoder = bind::Decoder::new(&kernel, &bound, cal_of);
    let mut sub = client.subscribe(spec, 1, None)?;
    while let Some(b) = sub.next_batch()? {
        for ev in decoder.events(&mut kernel, &bound, &b)? {
            match ev {
                bind::StreamEvent::Tuple { rel, r, tuple, .. } => {
                    kernel.insert_fact(rel, tuple, r)?;
                }
                bind::StreamEvent::Barrier { cal, label, .. } => kernel.open_bucket(&cal, label),
            }
        }
    }
    Ok(kernel)
}

/// Runs `f` on a thread with the stack deep recursion needs (evaluation
/// recurses one level per bar of strictly-earlier recursion).
pub fn on_kernel_thread<T: Send>(f: impl FnOnce() -> Result<T, RunError> + Send) -> Result<T, RunError> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn_scoped(s, f)
            .map_err(|e| RunError::Internal(format!("cannot spawn kernel thread: {}", e)))?
            .join()
            .map_err(|_| RunError::Internal("kernel thread panicked".into()))?
    })
}

/// A run over a data service with the simulated executor: the closed loop
/// of section 7 as the fold over the stream.
pub fn run_on(prog: &Program, lowered: Option<&crate::lower::Lowered>, client: &DataClient, cfg: ExecConfig, opts: &Replay) -> Result<RunResult, RunError> {
    on_kernel_thread(|| {
        let mut exec = SimExecutor::new(cfg.clone());
        replay(prog, lowered, client, cfg, &mut exec, opts, &mut |_| {}).map(|(r, k)| {
            std::mem::forget(k);
            r
        })
    })
}

/// A run over a data service that hands the kernel at the end of the stream
/// to `f` (the explanations, the ledger and the state tables read it).
pub fn run_keeping<T: Send>(
    prog: &Program,
    lowered: Option<&crate::lower::Lowered>,
    client: &DataClient,
    cfg: ExecConfig,
    opts: &Replay,
    f: impl for<'k> FnOnce(&RunResult, &mut Kernel<'k>) -> Result<T, RunError> + Send,
) -> Result<(RunResult, T), RunError> {
    on_kernel_thread(|| {
        let mut exec = SimExecutor::new(cfg.clone());
        let (r, mut k) = replay(prog, lowered, client, cfg, &mut exec, opts, &mut |_| {})?;
        let out = f(&r, &mut k)?;
        std::mem::forget(k);
        Ok((r, out))
    })
}

/// Run a checked program over a dataset held in memory: the data service
/// the front registers holds it (E-36), and the run is the fold over its
/// stream.
pub fn run(prog: &Program, dataset: &Dataset, cfg: ExecConfig) -> Result<RunResult, RunError> {
    let client = bind::connect_dataset(dataset, prog)?;
    run_on(prog, None, &client, cfg, &Replay::default())
}

/// `run` under a plan (compiler design, "Lower"): the kernel applies the
/// plan's literal orders, infeasible rules, shared window states and folded
/// constants.
pub fn run_planned(prog: &Program, lowered: &crate::lower::Lowered, dataset: &Dataset, cfg: ExecConfig) -> Result<RunResult, RunError> {
    let client = bind::connect_dataset(dataset, prog)?;
    run_on(prog, Some(lowered), &client, cfg, &Replay::default())
}

/// A sampled timestamp at which the full run and the truncated run disagree.
#[derive(Clone, Debug)]
pub struct CausalityMismatch {
    pub t: i64,
    pub full: Vec<String>,
    pub truncated: Vec<String>,
}

/// The empirical check of the causality theorem (section 7): for each sampled
/// t, decide(t) over the whole stream must equal decide(t) over the stream
/// truncated at t's barrier (Proposition K-4.1, G5).
pub fn verify_causality(prog: &Program, dataset: &Dataset, cfg: ExecConfig, samples: &[i64]) -> Result<Vec<CausalityMismatch>, RunError> {
    let client = bind::connect_dataset(dataset, prog)?;
    verify_causality_on(prog, &client, cfg, samples)
}

/// `verify_causality` over a data service.
pub fn verify_causality_on(prog: &Program, client: &DataClient, cfg: ExecConfig, samples: &[i64]) -> Result<Vec<CausalityMismatch>, RunError> {
    let full = run_on(prog, None, client, cfg.clone(), &Replay::default())?;
    let describe = |r: &RunResult, t: i64| -> Vec<String> {
        let mut v: Vec<String> = r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect();
        v.sort();
        v
    };
    let mut out = Vec::new();
    for &t in samples {
        let partial = run_on(
            prog,
            None,
            client,
            cfg.clone(),
            &Replay {
                horizon: Some(t),
                ..Replay::default()
            },
        )?;
        let a = describe(&full, t);
        let b = describe(&partial, t);
        if a != b {
            out.push(CausalityMismatch { t, full: a, truncated: b });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod store_tests {
    use super::*;

    const KEY: i64 = 7;
    const IDENTITY: [usize; 1] = [0];

    fn tu(e: Sym, v: f64) -> Tuple {
        vec![Value::Equity(e), Value::Time(KEY), Value::Num(v)]
    }

    fn rows(s: &Store) -> Vec<(Sym, f64)> {
        s.by_time[&KEY].iter().map(|t| (t[0].as_equity().unwrap(), t[2].as_f64().unwrap())).collect()
    }

    fn indexed() -> Store {
        let mut s = Store::default();
        s.index_by(Some(0));
        s
    }

    #[test]
    fn a_bar_received_whole_holds_every_tuple_in_the_order_received() {
        let mut s = indexed();
        for (e, v) in [(2, 30.0), (0, 10.0), (1, 20.0)] {
            s.insert_received(KEY, tu(e, v), &IDENTITY, 100);
        }
        assert_eq!(rows(&s), vec![(2, 30.0), (0, 10.0), (1, 20.0)]);
    }

    #[test]
    fn a_later_receipt_revises_the_row_in_place() {
        let mut s = indexed();
        for (e, v) in [(0, 10.0), (1, 20.0), (2, 30.0)] {
            s.insert_received(KEY, tu(e, v), &IDENTITY, 100);
        }
        s.insert_received(KEY, tu(1, 21.0), &IDENTITY, 200);
        s.insert_received(KEY, tu(3, 40.0), &IDENTITY, 200);
        assert_eq!(rows(&s), vec![(0, 10.0), (1, 21.0), (2, 30.0), (3, 40.0)]);
    }

    #[test]
    fn two_receipts_of_a_fact_at_one_instant_keep_the_later() {
        // Two versions of one relation bound together can bring a fact twice
        // at one instant.
        let mut s = indexed();
        for (e, v) in [(0, 10.0), (1, 20.0), (0, 12.0)] {
            s.insert_received(KEY, tu(e, v), &IDENTITY, 100);
        }
        assert_eq!(rows(&s), vec![(0, 12.0), (1, 20.0)]);
    }

    #[test]
    fn a_block_changed_by_another_writer_is_searched_whole() {
        let mut s = indexed();
        s.insert_received(KEY, tu(0, 10.0), &IDENTITY, 100);
        s.insert(KEY, tu(1, 20.0));
        s.insert_received(KEY, tu(1, 25.0), &IDENTITY, 100);
        assert_eq!(rows(&s), vec![(0, 10.0), (1, 25.0)]);
    }

    #[test]
    fn a_lookup_that_sorts_the_block_ends_the_instants_run() {
        // The sort moves a row received earlier among the instant's own.
        let mut s = indexed();
        s.insert_received(KEY, tu(1, 20.0), &IDENTITY, 100);
        s.insert_received(KEY, tu(0, 10.0), &IDENTITY, 200);
        assert_eq!(s.lookup(KEY, None).len(), 2);
        s.insert_received(KEY, tu(1, 21.0), &IDENTITY, 200);
        assert_eq!(rows(&s), vec![(0, 10.0), (1, 21.0)]);
    }
}
