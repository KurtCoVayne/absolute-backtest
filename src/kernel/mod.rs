//! The kernel: evaluation of a checked program over an environment instance
//! in the closed executor loop of section 7. Evaluation is top-down with
//! memoisation; every derived tuple is requested at a bound temporal key, and
//! WF-4 and WF-8 guarantee that every such request terminates.

pub mod eval;
pub mod time;
pub mod value;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use crate::check::Program;
use crate::ir::*;
pub mod executor;
pub mod fold;
pub use executor::{Executor, SimExecutor};
pub use fold::{fingerprint, run_fold, Checkpoint, CheckpointEvery, Event, EventLog, Fold};
pub use value::{Ctor, Decision, Order, OrderKind, Sym, Symbols, Tif, Value};

pub type Tuple = Vec<Value>;
/// Memo key: relation id, then the key time and the input values.
pub(crate) type MemoKey = (usize, Vec<Value>);
pub(crate) type Derived = Rc<Vec<(Tuple, usize)>>;

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
    /// own bar's close, the v1 convention.
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
}

impl Default for Contract {
    fn default() -> Contract {
        Contract {
            multiplier: 1.0,
            future: false,
            commission_per_contract: None,
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
    /// The dataset restricted to tuples whose temporal key falls in a
    /// decision-resolution bucket at or before `t` (E|ₜ of section 7).
    pub fn truncated(&self, prog: &Program, t: i64) -> Dataset {
        let mut out = Dataset {
            symbols: self.symbols.clone(),
            labels: self.labels.clone(),
            facts: BTreeMap::new(),
            available_at: BTreeMap::new(),
            securities: self.securities.clone(),
            as_of: Some(self.bundle_date().unwrap_or(t).min(t)),
            contracts: self.contracts.clone(),
        };
        // The close of the decision bar t: what a decision at t can have seen.
        let horizon = time::bucket_range(prog.resolution, t).1.max(t);
        for (name, tuples) in &self.facts {
            let Some(sig) = prog.relations.get(name) else { continue };
            let (Some(k), Some(res)) = (sig.key_pos(), sig.res) else { continue };
            let records = self.available_at.contains_key(name);
            for (i, tu) in tuples.iter().enumerate() {
                let key = tu[k].as_time().unwrap_or(i64::MAX);
                let keep = if records {
                    // Judged on availability (the v2 theorem).
                    self.available(name, i, key, res) <= horizon
                } else {
                    let label = if res == prog.resolution { key } else { time::bucket(prog.resolution, key) };
                    label <= t
                };
                if keep {
                    if records {
                        out.add_available(name, tu.clone(), self.available(name, i, key, res));
                    } else {
                        out.add(name, tu.clone());
                    }
                }
            }
            out.facts.entry(name.clone()).or_default();
        }
        out
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

/// The book at a bar's mark, before that bar's decisions.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExposureRecord {
    pub t: i64,
    pub cash: f64,
    /// Σ |position| · price.
    pub gross: f64,
    /// Σ position · price.
    pub net: f64,
    pub equity: f64,
    /// gross / equity (0 when equity is not positive).
    pub leverage: f64,
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
    /// `on_margin_call`.
    pub max_gross: f64,
    pub maintenance_margin: f64,
    pub on_margin_call: OnMarginCall,
    /// Funding, as annual rates accrued over the calendar time between
    /// consecutive decision bars: positive cash earns `cash_rate` (0 in v1,
    /// warned), a debit pays `margin_rate`, short notional pays its borrow
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
        }
    }
}

impl ExecConfig {
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
                message: "cash earns nothing (the v1 cash rate is zero; a rate series comes with the v2 catalog)".into(),
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
        res: Resolution,
        before: Option<i64>,
        after: Option<i64>,
    },
    /// A request the kernel cannot serve as asked: missing or ill-typed
    /// explain inputs, a binding of a variable the rule does not have, a
    /// parameter override outside its type or range.
    Request(String),
    /// An executor configuration the program cannot honour.
    Config(String),
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
            RunError::NoBars => write!(f, "the environment instance has no bars at the decision resolution"),
            RunError::NotABar { t, res, before, after } => {
                write!(f, "{} is not a bar at {}; ", time::format_timestamp(*t), res)?;
                match (before, after) {
                    (Some(b), Some(a)) => write!(f, "nearest bars are {} and {}", time::format_timestamp(*b), time::format_timestamp(*a)),
                    (Some(b), None) | (None, Some(b)) => write!(f, "the nearest bar is {}", time::format_timestamp(*b)),
                    (None, None) => write!(f, "the time domain is empty"),
                }
            }
            RunError::Request(m) => write!(f, "{}", m),
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
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RunResult {
    pub symbols: Vec<String>,
    pub bars: Vec<i64>,
    pub decisions: Vec<DecisionRecord>,
    pub fills: Vec<FillRecord>,
    /// Decisions the executor could not carry out, with the reason.
    pub dropped: Vec<(i64, Decision, String)>,
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
    /// Splits, dividends and delistings applied to the book.
    pub actions: Vec<ActionRecord>,
    pub stats: KernelStats,
    /// The models the configuration turned off, and what the run observed
    /// that the author should know (fills above the participation threshold).
    pub warnings: Vec<RunWarning>,
    /// The primitives the executor filled at and read volume from.
    pub price_relation: Option<String>,
    pub volume_relation: Option<String>,
}

impl RunResult {
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
        if d.order.is_market() {
            format!("{}({}, {})", d.ctor.name(), self.symbols[d.equity as usize], d.amount)
        } else {
            format!("{}({}, {}, {})", d.ctor.name(), self.symbols[d.equity as usize], d.amount, d.order)
        }
    }
}

/// A stored relation (primitive, executor or kernel state), indexed by key.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Store {
    pub by_time: BTreeMap<i64, Vec<Tuple>>,
}

impl Store {
    pub fn insert(&mut self, key: i64, tuple: Tuple) {
        self.by_time.entry(key).or_default().push(tuple);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RelInfo {
    pub name: String,
    pub key_pos: usize,
    pub inputs: Vec<usize>,
    pub stored: bool,
    pub res: Resolution,
    pub rules: Vec<usize>,
    pub entity_positions: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct CompiledRule {
    pub idx: usize,
    pub slots: HashMap<String, usize>,
    pub nslots: usize,
    pub label: String,
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
            None => write!(f, "rule {} fired at {}{} with {} solution(s)", self.rule, time::format_timestamp(self.t), with, self.solutions),
        }
    }
}

pub struct Kernel<'p> {
    pub(crate) prog: &'p Program,
    pub symbols: Symbols,
    pub(crate) stores: Vec<Store>,
    pub(crate) rels: Vec<RelInfo>,
    pub(crate) rel_ids: HashMap<String, usize>,
    pub(crate) compiled: Vec<Rc<CompiledRule>>,
    pub(crate) memo: HashMap<MemoKey, Derived>,
    /// `R(...) asof T` on a derived relation: for (relation, inputs, bar)
    /// the latest bar at or before it with a tuple, a cache like `memo`.
    pub(crate) asof_memo: HashMap<(usize, Vec<Value>, i64), Option<i64>>,
    pub(crate) in_progress: HashSet<MemoKey>,
    pub(crate) domains: HashMap<Resolution, BTreeSet<i64>>,
    pub(crate) cfg: ExecConfig,
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
    pub(crate) last_price: HashMap<Sym, f64>,
    pub(crate) params: HashMap<(String, String), Value>,
    /// Dataset symbol and label ids to the kernel's (identifier order).
    pub(crate) remap_syms: Vec<Sym>,
    pub(crate) remap_labels: Vec<Sym>,
    /// The rows of every bar a windowed aggregation group has solved, by
    /// (rule, literal, the outer bindings the group reads), by bar.
    pub(crate) windows: HashMap<eval::WindowKey, eval::WindowCache>,
    /// For a `rows` window group, the earliest bar with rows when every bar
    /// before it is known to have none (where a walk back may stop).
    pub(crate) rows_floor: HashMap<eval::WindowKey, i64>,
    /// For a `rows` window group, the solved bars at which its anchor (the
    /// conjunction's first atom) holds: the group's rows.
    pub(crate) rows_anchor: HashMap<eval::WindowKey, BTreeSet<i64>>,
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
    /// What each equity literal of the program (a ticker) resolved to.
    pub(crate) literal_equities: HashMap<String, Sym>,
    /// Contract terms by kernel symbol (futures); a symbol absent is a share.
    pub(crate) contracts: HashMap<Sym, Contract>,
}

impl<'p> Kernel<'p> {
    pub fn new(prog: &'p Program, dataset: &Dataset, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
        Kernel::build(prog, dataset, cfg, true)
    }

    /// A kernel over the dataset's symbols and tables but none of its facts:
    /// the fold feeds them as events (`insert_fact`) and opens the time
    /// domains' buckets as the stream reaches them (`open_bucket`).
    pub fn new_streaming(prog: &'p Program, dataset: &Dataset, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
        Kernel::build(prog, dataset, cfg, false)
    }

    fn build(prog: &'p Program, dataset: &Dataset, cfg: ExecConfig, load_facts: bool) -> Result<Kernel<'p>, RunError> {
        // Resolve string overrides by their parameter's type, as the checker
        // did for the program's own literals.
        let mut cfg = cfg;
        for (name, value) in cfg.param_overrides.iter_mut() {
            if let Lit::Str(s) = value {
                let (unit, pname) = name.split_once("::").unwrap_or((prog.strategy.as_str(), name.as_str()));
                if let Some(p) = prog.param(unit, pname) {
                    *value = if p.ty == Ty::Label { Lit::Label(s.clone()) } else { Lit::Equity(s.clone()) };
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
        let mut intern = |l: &Lit| match l {
            Lit::Equity(s) => {
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
        if let Some(m) = unresolved.first() {
            return Err(RunError::Config(m.clone()));
        }
        let (symbols, remap) = symbols.sorted();
        let (labels, remap_labels) = labels.sorted();
        let literal_equities: HashMap<String, Sym> = literal_equities.into_iter().map(|(k, s)| (k, remap[s as usize])).collect();
        let securities: Vec<Security> = dataset
            .securities
            .iter()
            .map(|s| Security {
                id: remap[s.id as usize],
                ..s.clone()
            })
            .collect();
        let contracts: HashMap<Sym, Contract> = dataset.contracts.iter().map(|(s, c)| (remap[*s as usize], c.clone())).collect();
        let remap_value = |v: &Value| -> Value {
            match v {
                Value::Equity(s) => Value::Equity(remap[*s as usize]),
                Value::Label(s) => Value::Label(remap_labels[*s as usize]),
                Value::Decision(d) => Value::Decision(Decision {
                    ctor: d.ctor,
                    equity: remap[d.equity as usize],
                    amount: d.amount,
                    order: d.order,
                }),
                v => v.clone(),
            }
        };
        // Relation table.
        let mut rels = Vec::new();
        let mut rel_ids = HashMap::new();
        for (name, sig) in &prog.relations {
            let key_pos = sig.key_pos().ok_or_else(|| RunError::Internal(format!("relation `{}` has no temporal key", name)))?;
            let stored = matches!(sig.kind, Kind::Primitive { .. } | Kind::Executor | Kind::KernelState);
            let res = sig.res.ok_or_else(|| RunError::Internal(format!("relation `{}` has no resolution", name)))?;
            rel_ids.insert(name.clone(), rels.len());
            rels.push(RelInfo {
                name: name.clone(),
                key_pos,
                inputs: sig.args.iter().enumerate().filter(|(_, a)| a.mode == Mode::In).map(|(i, _)| i).collect(),
                stored,
                res,
                rules: prog.rules_for(name),
                entity_positions: sig.args.iter().enumerate().filter(|(_, a)| a.ty.is_entity()).map(|(i, _)| i).collect(),
            });
        }
        // Stores and time domains.
        let mut stores: Vec<Store> = rels.iter().map(|_| Store::default()).collect();
        let mut native: HashMap<Resolution, BTreeSet<i64>> = HashMap::new();
        for (name, tuples) in dataset.facts.iter().filter(|_| load_facts) {
            let Some(&id) = rel_ids.get(name) else { continue };
            let info = &rels[id];
            for tu in tuples {
                let tu: Tuple = tu.iter().map(remap_value).collect();
                let key = tu[info.key_pos].as_time().ok_or_else(|| RunError::Internal(format!("non-timestamp key in `{}`", name)))?;
                native.entry(info.res).or_default().insert(key);
                stores[id].insert(key, tu);
            }
        }
        let all_res = [Resolution::M1, Resolution::M5, Resolution::M15, Resolution::M30, Resolution::H1, Resolution::D1];
        let mut domains: HashMap<Resolution, BTreeSet<i64>> = HashMap::new();
        for r in all_res {
            let mut dom: BTreeSet<i64> = native.get(&r).cloned().unwrap_or_default();
            for (fr, ts) in &native {
                if fr.finer_than(r) {
                    for &t in ts {
                        dom.insert(time::bucket(r, t));
                    }
                }
            }
            domains.insert(r, dom);
        }
        // Compiled rules: variable slots.
        let mut compiled = Vec::new();
        for (i, rule) in prog.rules.iter().enumerate() {
            let mut slots: HashMap<String, usize> = HashMap::new();
            collect_vars(rule, &mut |v| {
                let n = slots.len();
                slots.entry(v.to_string()).or_insert(n);
            });
            let nslots = slots.len();
            compiled.push(Rc::new(CompiledRule {
                idx: i,
                slots,
                nslots,
                label: prog.rule_label(i),
            }));
        }
        // Parameters.
        let mut params = HashMap::new();
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
            params.insert((unit.to_string(), pname.to_string()), lit_value(value, &mut sym_tmp, &mut lab_tmp, &literal_equities));
        }
        // Price relation for the executor.
        let price_rel = match &cfg.price_relation {
            Some(name) => {
                let id = *rel_ids.get(name).ok_or_else(|| RunError::Config(format!("price relation `{}` is not in the program", name)))?;
                let sig = prog.relations.get(name).unwrap();
                if !matches!(sig.kind, Kind::Primitive { .. }) || price_column(sig).is_none() || !sig.args.iter().any(|a| a.ty.is_entity()) {
                    return Err(RunError::Config(format!(
                        "price relation `{}` must be a primitive with an equity argument and a Price<...> output; `{}` has none",
                        name, name
                    )));
                }
                if !sig.res.map(|r| r <= prog.resolution).unwrap_or(false) {
                    return Err(RunError::Config(format!("price relation `{}` is coarser than the decision resolution {}", name, prog.resolution)));
                }
                Some(id)
            }
            None => {
                let mut cands: Vec<(&String, &Signature)> = prog
                    .relations
                    .iter()
                    .filter(|(_, s)| matches!(s.kind, Kind::Primitive { .. }) && s.res.map(|r| r <= prog.resolution).unwrap_or(false))
                    .filter(|(_, s)| s.args.iter().any(|a| a.ty.is_entity()) && price_column(s).is_some())
                    .collect();
                cands.sort_by_key(|(n, s)| (s.res != Some(prog.resolution), !n.starts_with("close"), (*n).clone()));
                cands.first().map(|(n, _)| rel_ids[*n])
            }
        };
        let price_col = price_rel.and_then(|id| price_column(prog.relations.get(&rels[id].name).unwrap())).unwrap_or(0);
        // Volume relation for the participation cap and impact.
        let volume_rel = match &cfg.volume_relation {
            Some(name) => {
                let id = *rel_ids.get(name).ok_or_else(|| RunError::Config(format!("volume relation `{}` is not in the program", name)))?;
                let sig = prog.relations.get(name).unwrap();
                if !matches!(sig.kind, Kind::Primitive { .. }) || shares_column(sig).is_none() || !sig.args.iter().any(|a| a.ty.is_entity()) {
                    return Err(RunError::Config(format!(
                        "volume relation `{}` must be a primitive with an equity argument and a Quantity<Shares> output; `{}` has none",
                        name, name
                    )));
                }
                if !sig.res.map(|r| r <= prog.resolution).unwrap_or(false) {
                    return Err(RunError::Config(format!("volume relation `{}` is coarser than the decision resolution {}", name, prog.resolution)));
                }
                Some(id)
            }
            None => {
                let mut cands: Vec<(&String, &Signature)> = prog
                    .relations
                    .iter()
                    .filter(|(_, s)| matches!(s.kind, Kind::Primitive { .. }) && s.res.map(|r| r <= prog.resolution).unwrap_or(false))
                    .filter(|(_, s)| s.args.iter().any(|a| a.ty.is_entity()) && shares_column(s).is_some())
                    .collect();
                cands.sort_by_key(|(n, s)| (s.res != Some(prog.resolution), !n.starts_with("volume"), (*n).clone()));
                cands.first().map(|(n, _)| rel_ids[*n])
            }
        };
        let volume_col = volume_rel.and_then(|id| shares_column(prog.relations.get(&rels[id].name).unwrap())).unwrap_or(0);
        let companion = |which: &str| -> Option<(usize, usize)> {
            let id = price_rel?;
            let name = rels[id].name.replacen("close", which, 1);
            if name == rels[id].name {
                return None;
            }
            let sig = prog.relations.get(&name)?;
            if !matches!(sig.kind, Kind::Primitive { .. }) {
                return None;
            }
            Some((*rel_ids.get(&name)?, price_column(sig)?))
        };
        let (open_rel, high_rel, low_rel) = (companion("open"), companion("high"), companion("low"));
        // The catalog's actions, by name (data-bundle doc, section 3).
        let primitive = |name: &str| -> Option<usize> {
            let id = *rel_ids.get(name)?;
            matches!(prog.relations.get(name)?.kind, Kind::Primitive { .. }).then_some(id)
        };
        let split_rel = primitive("split");
        let dividend_rel = primitive("dividend");
        let delisted_rel = primitive("delisted");
        Ok(Kernel {
            prog,
            symbols,
            stores,
            rels,
            rel_ids,
            compiled,
            memo: HashMap::new(),
            asof_memo: HashMap::new(),
            in_progress: HashSet::new(),
            domains,
            cfg,
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
            literal_equities,
            contracts,
            last_price: HashMap::new(),
            params,
            remap_syms: remap,
            remap_labels,
            windows: HashMap::new(),
            rows_floor: HashMap::new(),
            rows_anchor: HashMap::new(),
            stats: KernelStats::default(),
        })
    }

    /// The evaluator's counters, with the cache's current size.
    pub fn stats(&self) -> KernelStats {
        KernelStats {
            window_groups: self.windows.len(),
            window_rows_cached: self.windows.values().map(|m| m.len()).sum(),
            ..self.stats.clone()
        }
    }

    /// A dataset tuple with its symbols and labels in the kernel's order.
    pub fn remap_tuple(&self, tu: &[Value]) -> Tuple {
        tu.iter()
            .map(|v| match v {
                Value::Equity(s) => Value::Equity(self.remap_syms[*s as usize]),
                Value::Label(s) => Value::Label(self.remap_labels[*s as usize]),
                Value::Decision(d) => Value::Decision(Decision {
                    ctor: d.ctor,
                    equity: self.remap_syms[d.equity as usize],
                    amount: d.amount,
                    order: d.order,
                }),
                v => v.clone(),
            })
            .collect()
    }

    /// Insert one primitive fact (already in the kernel's symbol order) and
    /// return its temporal key.
    pub fn insert_fact(&mut self, rel: usize, tuple: Tuple) -> Result<i64, RunError> {
        let info = &self.rels[rel];
        let key = tuple[info.key_pos].as_time().ok_or_else(|| RunError::Internal(format!("non-timestamp key in `{}`", info.name)))?;
        self.stores[rel].insert(key, tuple);
        Ok(key)
    }

    /// A bucket of resolution `res` exists from now on.
    pub fn open_bucket(&mut self, res: Resolution, label: i64) {
        self.domains.entry(res).or_default().insert(label);
    }

    /// A tuple keyed at `key` arrived after its bar closed: everything
    /// derived at or after `key` may have read its absence. Derived values
    /// are recomputed on demand; the windowed groups forget the bars from
    /// `key` on. Decisions already emitted stand (`decided` is a log).
    pub fn invalidate_from(&mut self, key: i64) {
        self.memo.clear();
        self.asof_memo.clear();
        for cache in self.windows.values_mut() {
            let keep = std::mem::take(cache);
            *cache = keep.into_iter().filter(|(t, _)| *t < key).collect();
        }
        self.rows_floor.clear();
        self.rows_anchor.clear();
        self.stats.late_tuples += 1;
    }

    /// The kernel's symbol names, by symbol id (identifier order).
    pub fn symbol_names(&self) -> &[String] {
        self.symbols.names()
    }

    pub fn relation_id(&self, name: &str) -> Option<usize> {
        self.rel_ids.get(name).copied()
    }

    pub fn relation_resolution(&self, rel: usize) -> Resolution {
        self.rels[rel].res
    }

    pub fn is_primitive(&self, rel: usize) -> bool {
        matches!(self.prog.relations[&self.rels[rel].name].kind, Kind::Primitive { .. })
    }

    pub fn program(&self) -> &'p Program {
        self.prog
    }

    /// The decision bars of the run: the domain at the decision resolution
    /// within the configured window.
    pub fn decision_bars(&self) -> Vec<i64> {
        let lo = self.cfg.start.unwrap_or(i64::MIN);
        let hi = self.cfg.end.unwrap_or(i64::MAX);
        self.domains[&self.prog.resolution].range(lo..=hi).copied().collect()
    }

    fn rel(&self, name: &str) -> Result<usize, RunError> {
        self.rel_ids.get(name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", name)))
    }

    /// Run the closed loop of section 7 over every decision bar.
    /// Run the strategy bar by bar with the simulated executor (section 7,
    /// the closed loop).
    pub fn run(&mut self) -> Result<RunResult, RunError> {
        let mut exec = SimExecutor::new(self.cfg.clone());
        self.run_with(&mut exec)
    }

    /// Run the strategy bar by bar with any executor: at each decision bar,
    /// the previous bar's orders are filled, the bar is opened (actions,
    /// mark, margin), the strategy decides, and the executor takes the
    /// decisions; the executor finishes when the data ends.
    pub fn run_with(&mut self, exec: &mut dyn Executor) -> Result<RunResult, RunError> {
        let bars = self.decision_bars();
        if bars.is_empty() {
            return Err(RunError::NoBars);
        }
        let mut result = RunResult {
            symbols: self.symbols.names().to_vec(),
            warnings: self.cfg.warnings(),
            price_relation: self.price_rel.map(|id| self.rels[id].name.clone()),
            volume_relation: self.volume_rel.map(|id| self.rels[id].name.clone()),
            base_capital: (!self.cfg.compounding).then_some(self.cfg.initial_cash),
            multipliers: self.contracts.iter().map(|(s, c)| (*s, c.multiplier)).collect(),
            ..Default::default()
        };
        for (k, &t) in bars.iter().enumerate() {
            if k > 0 {
                exec.fill(self, t, &mut result)?;
            }
            exec.open_bar(self, t, &mut result)?;
            let by_equity = self.decide_at(t, &mut result)?;
            exec.on_decisions(self, t, &by_equity, &mut result)?;
        }
        exec.finish(self, &mut result);
        result.stats = self.stats();
        Ok(result)
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
        if info.res == self.prog.resolution {
            return self.stores[id]
                .by_time
                .get(&t)
                .and_then(|tus| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)))
                .and_then(|tu| tu[col].as_f64());
        }
        let (lo, hi) = time::bucket_range(self.prog.resolution, t);
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

    /// The last decision bar at or after `t` within `t`'s calendar day at
    /// which `sym` has a price: the close a market-on-close order decided at
    /// `t` executes at (the session's last print; at @1d, `t` itself).
    pub fn session_last_bar(&self, sym: Sym, t: i64) -> Option<i64> {
        let id = self.price_rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let lo = time::bucket_range(self.prog.resolution, t).0.min(t);
        let day_end = time::floor_div(t, time::DAY) * time::DAY + time::DAY - 1;
        self.stores[id]
            .by_time
            .range(lo..=day_end)
            .rev()
            .find(|(_, tus)| tus.iter().any(|tu| tu[entity_pos] == Value::Equity(sym)))
            .map(|(&ts, _)| if info.res == self.prog.resolution { ts } else { time::bucket(self.prog.resolution, ts) })
    }

    /// Currency per point per unit held: a future's multiplier, 1 for a share.
    pub fn multiplier(&self, sym: Sym) -> f64 {
        self.contracts.get(&sym).map(|c| c.multiplier).unwrap_or(1.0)
    }

    /// A future: sold short without a borrow, priced back-adjusted.
    pub fn is_future(&self, sym: Sym) -> bool {
        self.contracts.get(&sym).map(|c| c.future).unwrap_or(false)
    }

    /// A future's commission per contract per side, when it has one.
    pub fn commission_per_contract(&self, sym: Sym) -> Option<f64> {
        self.contracts.get(&sym).and_then(|c| c.commission_per_contract)
    }

    /// Volume of `sym` over decision bar `t` from the configured volume
    /// relation: the tuple at `t`, or the sum of the fine tuples in its bucket.
    pub fn bar_volume(&self, sym: Sym, t: i64) -> Option<f64> {
        let id = self.volume_rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let col = self.volume_col;
        if info.res == self.prog.resolution {
            self.stores[id]
                .by_time
                .get(&t)
                .and_then(|tus| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)))
                .and_then(|tu| tu[col].as_f64())
        } else {
            let (lo, hi) = time::bucket_range(self.prog.resolution, t);
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

    /// Average daily volume of `sym` at `bars[end]`: the mean bar volume over
    /// the `adv_window` bars ending there (inclusive); `None` without any.
    pub fn adv(&self, sym: Sym, bars: &[i64], end: usize) -> Option<f64> {
        let lo = end.saturating_sub(self.cfg.adv_window.max(1) - 1);
        let vols: Vec<f64> = bars[lo..=end.min(bars.len() - 1)].iter().filter_map(|&t| self.bar_volume(sym, t)).collect();
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
        let id = self.price_rel?;
        let info = &self.rels[id];
        let entity_pos = *info.entity_positions.first()?;
        let col = self.price_col;
        let found = if info.res == self.prog.resolution {
            self.stores[id]
                .by_time
                .get(&t)
                .and_then(|tus| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)))
                .and_then(|tu| tu[col].as_f64())
        } else {
            let (lo, hi) = time::bucket_range(self.prog.resolution, t);
            self.stores[id]
                .by_time
                .range(lo..=hi)
                .rev()
                .find_map(|(_, tus)| tus.iter().find(|tu| tu[entity_pos] == Value::Equity(sym)).and_then(|tu| tu[col].as_f64()))
        };
        if let Some(p) = found {
            self.last_price.insert(sym, p);
        }
        found
    }

    /// The `+` head arguments of rule `rule_idx`, in signature order: what
    /// `explain` needs as `inputs`.
    pub fn rule_inputs(&self, rule_idx: usize) -> Vec<(String, Ty)> {
        let sig = &self.prog.relations[&self.prog.rules[rule_idx].head.name];
        sig.args.iter().filter(|a| a.mode == Mode::In).map(|a| (a.name.clone(), a.ty.clone())).collect()
    }

    /// Parse a command-line value as an input of type `ty`: an equity by
    /// identifier, a timestamp, or a literal in the DSL's own grammar.
    pub fn parse_input(&self, ty: &Ty, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        match ty {
            Ty::Equity => self.equity(raw.trim_matches('"')),
            Ty::Label => {
                let name = raw.trim_matches('"');
                self.labels.get(name).map(Value::Label).ok_or_else(|| format!("`{}` is not a label of the dataset", name))
            }
            Ty::Timestamp => time::parse_timestamp(raw)
                .map(Value::Time)
                .ok_or_else(|| format!("`{}` is not a Timestamp (YYYY-MM-DD[THH:MM[:SS]])", raw)),
            Ty::Count => raw.parse().map(Value::Count).map_err(|_| format!("`{}` is not a Count", raw)),
            Ty::Duration => match crate::parser::parse_lit(raw) {
                Ok(Lit::Duration(d)) => Ok(Value::Dur(d)),
                _ => Err(format!("`{}` is not a Duration (such as 20d, 3mo or 1y)", raw)),
            },
            Ty::Quantity(_) => match crate::parser::parse_lit(raw) {
                Ok(l @ (Lit::Int(_) | Lit::Float(_))) => Ok(Value::Num(l_num(&l))),
                Ok(l @ (Lit::Shares(_) | Lit::Money(..))) if crate::check::types::compat(ty, &l.ty()) => Ok(Value::Num(l_num(&l))),
                Ok(l) => Err(format!("`{}` is {}, not {}", raw, l.ty(), ty)),
                Err(_) => Err(format!("`{}` is not a {}", raw, ty)),
            },
            Ty::Decision | Ty::IntLit | Ty::StrLit => Err(format!("a {} cannot be given on the command line", ty)),
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
        match crate::parser::parse_lit(raw) {
            Ok(Lit::Str(name) | Lit::Equity(name)) => self.equity(&name),
            Ok(Lit::Label(name)) => self.labels.get(&name).map(Value::Label).ok_or_else(|| format!("`{}` is not a label of the dataset", name)),
            Ok(Lit::Int(i)) => Ok(Value::Count(i)),
            Ok(Lit::Duration(d)) => Ok(Value::Dur(d)),
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
    pub fn explain_with(&mut self, rule_idx: usize, t: i64, inputs: &[Value], bindings: &[(String, Value)]) -> Result<Explanation, RunError> {
        let rule = &self.prog.rules[rule_idx];
        let cr = self.compiled[rule_idx].clone();
        let rel = self.rel(&rule.head.name)?;
        let info = self.rels[rel].clone();
        // Only bars of the rule's time domain are ever evaluated (section 6);
        // a label between two bars would read a phantom bucket.
        let res = self.prog.infos[rule_idx].resolution;
        let domain = self.domains.get(&res).ok_or_else(|| RunError::Internal(format!("no time domain at {}", res)))?;
        if !domain.contains(&t) {
            return Err(RunError::NotABar {
                t,
                res,
                before: domain.range(..t).next_back().copied(),
                after: domain.range(t..).next().copied(),
            });
        }
        let mut env: Env = vec![None; cr.nslots];
        if let Term::Var(v, _) = &rule.head.terms[info.key_pos] {
            env[cr.slots[v]] = Some(Value::Time(t));
        }
        if inputs.len() != info.inputs.len() {
            let need: Vec<String> = self.rule_inputs(rule_idx).iter().map(|(n, ty)| format!("{}: {}", n, ty)).collect();
            return Err(RunError::Request(format!(
                "rule {} takes {} inputs ({}), {} given",
                cr.label,
                info.inputs.len(),
                need.join(", "),
                inputs.len()
            )));
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
        let mut envs = vec![env];
        for (i, lit) in rule.body.iter().enumerate() {
            let mut next = Vec::new();
            for e in &envs {
                self.step(&cr, lit, e, &mut next)?;
            }
            if next.is_empty() {
                return Ok(Explanation {
                    rule: cr.label.clone(),
                    t,
                    bindings: shown,
                    solutions: 0,
                    failed_at: Some((i, lit.describe())),
                });
            }
            envs = next;
        }
        Ok(Explanation {
            rule: cr.label.clone(),
            t,
            bindings: shown,
            solutions: envs.len(),
            failed_at: None,
        })
    }
}

/// Whether any `lag` in `unit`'s rules takes parameter `name` as its length.
fn lag_uses_param(prog: &Program, unit: &str, name: &str) -> bool {
    fn lit(l: &Literal, name: &str) -> bool {
        match l {
            Literal::Builtin(Builtin::Lag { n: Expr::Param(p, _), .. }, _) => p == name,
            Literal::Agg { conj, .. } => conj.iter().any(|c| lit(c, name)),
            _ => false,
        }
    }
    prog.rules.iter().filter(|r| r.unit == unit).any(|r| r.body.iter().any(|l| lit(l, name)))
}

fn l_num(l: &Lit) -> f64 {
    match l {
        Lit::Int(i) => *i as f64,
        Lit::Float(x) | Lit::Shares(x) | Lit::Money(x, _) | Lit::Price(x, _) => *x,
        Lit::Duration(_) | Lit::Str(_) | Lit::Equity(_) | Lit::Label(_) => f64::NAN,
    }
}

/// The magnitude a literal is ranged by: its number, or a duration's
/// approximate length in days; an equity has none.
fn lit_magnitude(l: &Lit) -> Option<f64> {
    match l {
        Lit::Duration(d) => Some(d.approx_days()),
        Lit::Str(_) | Lit::Equity(_) | Lit::Label(_) => None,
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
        Lit::Float(x) | Lit::Shares(x) | Lit::Money(x, _) | Lit::Price(x, _) => Value::Num(*x),
        Lit::Duration(d) => Value::Dur(*d),
        // A ticker literal is what `Kernel::new` resolved it to; one it did
        // not see (an unchecked program) reads as its own id.
        Lit::Str(s) | Lit::Equity(s) => Value::Equity(equities.get(s).copied().unwrap_or_else(|| symbols.intern(s))),
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
            },
            Literal::Window { dur, min, .. } => {
                expr(dur, f);
                expr(min, f);
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
            Literal::Resample { inner, min, aggs, .. } => {
                inner.terms.iter().for_each(|t| term(t, f));
                expr(min, f);
                aggs.iter().for_each(|(_, _, e)| expr(e, f));
            }
            Literal::AsOf { atom, at, .. } => {
                atom.terms.iter().for_each(|t| term(t, f));
                term(at, f);
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
            },
            Literal::Window { var, base, dur, min, .. } => {
                term(var, f);
                term(base, f);
                expr(dur, f);
                expr(min, f);
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
            Literal::Resample { inner, as_var, min, aggs, .. } => {
                inner.terms.iter().for_each(|t| term(t, f));
                f(as_var);
                expr(min, f);
                aggs.iter().for_each(|(x, _, e)| {
                    f(x);
                    expr(e, f);
                });
            }
            Literal::AsOf { atom, at, .. } => {
                atom.terms.iter().for_each(|t| term(t, f));
                term(at, f);
            }
        }
    }
    rule.head.terms.iter().for_each(|t| term(t, f));
    rule.body.iter().for_each(|l| lit(l, f));
}

/// Run a checked program over a dataset; the whole closed loop of section 7.
/// Evaluation recurses one level per bar of strictly-earlier recursion, so it
/// runs on a thread with a generous stack.
pub fn run(prog: &Program, dataset: &Dataset, cfg: ExecConfig) -> Result<RunResult, RunError> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn_scoped(s, || {
                let mut k = Kernel::new(prog, dataset, cfg)?;
                k.run()
            })
            .map_err(|e| RunError::Internal(format!("cannot spawn kernel thread: {}", e)))?
            .join()
            .map_err(|_| RunError::Internal("kernel thread panicked".into()))?
    })
}

/// A sampled timestamp at which the full run and the truncated run disagree.
#[derive(Clone, Debug)]
pub struct CausalityMismatch {
    pub t: i64,
    pub full: Vec<String>,
    pub truncated: Vec<String>,
}

/// The empirical check of the causality theorem (section 7): for each sampled
/// t, decide(t) over E must equal decide(t) over E|ₜ.
pub fn verify_causality(prog: &Program, dataset: &Dataset, cfg: ExecConfig, samples: &[i64]) -> Result<Vec<CausalityMismatch>, RunError> {
    // Both runs are the fold, which honours availability (its executor's
    // fills included); the truncated runs hold what was available at each
    // sampled bar.
    let full = run_fold(prog, dataset, cfg.clone())?;
    let describe = |r: &RunResult, t: i64| -> Vec<String> {
        let mut v: Vec<String> = r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect();
        v.sort();
        v
    };
    let mut out = Vec::new();
    for &t in samples {
        let trunc = dataset.truncated(prog, t);
        let partial = run_fold(prog, &trunc, cfg.clone())?;
        let a = describe(&full, t);
        let b = describe(&partial, t);
        if a != b {
            out.push(CausalityMismatch { t, full: a, truncated: b });
        }
    }
    Ok(out)
}
