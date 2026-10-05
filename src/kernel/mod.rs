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
pub use value::{Ctor, Decision, Sym, Symbols, Value};

pub type Tuple = Vec<Value>;
/// Memo key: relation id, then the key time and the input values.
pub(crate) type MemoKey = (usize, Vec<Value>);
pub(crate) type Derived = Rc<Vec<(Tuple, usize)>>;

/// Primitive facts: an environment instance E (section 7).
#[derive(Clone, Debug, Default)]
pub struct Dataset {
    pub symbols: Symbols,
    /// Labels (`Ty::Label`), interned apart from the equities.
    pub labels: Symbols,
    pub facts: BTreeMap<String, Vec<Tuple>>,
}

impl Dataset {
    pub fn new() -> Dataset {
        Dataset::default()
    }
    pub fn intern(&mut self, name: &str) -> Sym {
        self.symbols.intern(name)
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
        self.facts.entry(relation.to_string()).or_default().push(tuple);
    }
    /// The dataset restricted to tuples whose temporal key falls in a
    /// decision-resolution bucket at or before `t` (E|ₜ of section 7).
    pub fn truncated(&self, prog: &Program, t: i64) -> Dataset {
        let mut out = Dataset {
            symbols: self.symbols.clone(),
            labels: self.labels.clone(),
            facts: BTreeMap::new(),
        };
        for (name, tuples) in &self.facts {
            let Some(sig) = prog.relations.get(name) else { continue };
            let (Some(k), Some(res)) = (sig.key_pos(), sig.res) else { continue };
            let kept: Vec<Tuple> = tuples
                .iter()
                .filter(|tu| {
                    let key = tu[k].as_time().unwrap_or(i64::MAX);
                    let label = if res == prog.resolution { key } else { time::bucket(prog.resolution, key) };
                    label <= t
                })
                .cloned()
                .collect();
            out.facts.insert(name.clone(), kept);
        }
        out
    }
}

/// What the executor does when a fill would borrow: cash would go negative,
/// or gross exposure (Σ |position| × price) would exceed equity (section 6,
/// executor policy). Orders that reduce exposure are never leverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorrowBucket {
    pub adv_below: f64,
    pub fee_bps: f64,
    pub shortable: bool,
}

/// Interest and fees accrued over a run, between consecutive decision bars
/// at the configured annual rates over calendar time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FundingSummary {
    pub cash_interest: f64,
    pub margin_interest: f64,
    pub borrow_fees: f64,
    pub short_rebate: f64,
}

/// The book at a bar's mark, before that bar's decisions.
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug)]
pub struct ExecConfig {
    pub initial_cash: f64,
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
}

impl Default for ExecConfig {
    /// Conservative, non-zero costs (data-bundle doc, section 5: "defaults
    /// are conservative on purpose"): half a cent a share with a dollar
    /// minimum, the SEC-style fee on sells, and slippage of a tenth of a
    /// bar's realized volatility.
    fn default() -> ExecConfig {
        ExecConfig {
            initial_cash: 1_000_000.0,
            slippage_bps: 0.0,
            slippage_vol_mult: 0.1,
            vol_window: 20,
            vol_min_obs: 10,
            commission_per_share: 0.005,
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
            param_overrides: Vec::new(),
            on_leverage: OnLeverage::Halt,
            on_oversize: OnOversize::Halt,
            on_ruin: OnRuin::Halt,
            lot: Lot::Whole,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunWarning {
    /// The bias of the data-bundle doc's audit the warning relates to.
    pub bias: String,
    pub message: String,
}

/// Costs paid over a run, and the notional traded.
#[derive(Clone, Debug, Default, PartialEq)]
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
#[derive(Clone, Debug, Default, PartialEq)]
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

#[derive(Clone, Debug)]
pub struct DecisionRecord {
    pub t: i64,
    pub decision: Decision,
    pub rule: usize,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug, Default)]
pub struct RunResult {
    pub symbols: Vec<String>,
    pub bars: Vec<i64>,
    pub decisions: Vec<DecisionRecord>,
    pub fills: Vec<FillRecord>,
    /// Decisions the executor could not carry out, with the reason.
    pub dropped: Vec<(i64, Decision, String)>,
    /// Cash plus marked positions at each bar, before that bar's decisions.
    pub equity_curve: Vec<(i64, f64)>,
    pub final_cash: f64,
    pub final_positions: BTreeMap<Sym, f64>,
    pub costs: CostSummary,
    pub liquidity: LiquiditySummary,
    pub funding: FundingSummary,
    /// The book at every bar's mark, parallel to `equity_curve`.
    pub exposure: Vec<ExposureRecord>,
    /// The models the configuration turned off, and what the run observed
    /// that the author should know (fills above the participation threshold).
    pub warnings: Vec<RunWarning>,
}

impl RunResult {
    pub fn decisions_at(&self, t: i64) -> Vec<&Decision> {
        self.decisions.iter().filter(|d| d.t == t).map(|d| &d.decision).collect()
    }
    pub fn describe_decision(&self, d: &Decision) -> String {
        format!("{}({}, {})", d.ctor.name(), self.symbols[d.equity as usize], d.amount)
    }
}

/// A stored relation (primitive, executor or kernel state), indexed by key.
#[derive(Clone, Debug, Default)]
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
    pub(crate) in_progress: HashSet<MemoKey>,
    pub(crate) domains: HashMap<Resolution, BTreeSet<i64>>,
    pub(crate) cfg: ExecConfig,
    pub(crate) price_rel: Option<usize>,
    pub(crate) price_col: usize,
    pub(crate) volume_rel: Option<usize>,
    pub(crate) volume_col: usize,
    pub(crate) last_price: HashMap<Sym, f64>,
    pub(crate) params: HashMap<(String, String), Value>,
    pub(crate) labels: Symbols,
}

impl<'p> Kernel<'p> {
    pub fn new(prog: &'p Program, dataset: &Dataset, cfg: ExecConfig) -> Result<Kernel<'p>, RunError> {
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
        // Intern every equity and label literal of the program, then order
        // ids by identifier.
        let mut symbols = dataset.symbols.clone();
        let mut labels = dataset.labels.clone();
        let mut intern = |l: &Lit| match l {
            Lit::Equity(s) => {
                symbols.intern(s);
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
        let (symbols, remap) = symbols.sorted();
        let (labels, remap_labels) = labels.sorted();
        let remap_value = |v: &Value| -> Value {
            match v {
                Value::Equity(s) => Value::Equity(remap[*s as usize]),
                Value::Label(s) => Value::Label(remap_labels[*s as usize]),
                Value::Decision(d) => Value::Decision(Decision {
                    ctor: d.ctor,
                    equity: remap[d.equity as usize],
                    amount: d.amount,
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
        for (name, tuples) in &dataset.facts {
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
                params.insert((unit.clone(), name.clone()), lit_value(&p.value, &mut sym_tmp, &mut lab_tmp));
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
            params.insert((unit.to_string(), pname.to_string()), lit_value(value, &mut sym_tmp, &mut lab_tmp));
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
        Ok(Kernel {
            prog,
            symbols,
            stores,
            rels,
            rel_ids,
            compiled,
            memo: HashMap::new(),
            in_progress: HashSet::new(),
            domains,
            cfg,
            price_rel,
            price_col,
            volume_rel,
            volume_col,
            labels,
            last_price: HashMap::new(),
            params,
        })
    }

    pub fn program(&self) -> &'p Program {
        self.prog
    }

    pub fn decision_bars(&self) -> Vec<i64> {
        self.domains[&self.prog.resolution].iter().copied().collect()
    }

    fn rel(&self, name: &str) -> Result<usize, RunError> {
        self.rel_ids.get(name).copied().ok_or_else(|| RunError::Internal(format!("unknown relation `{}`", name)))
    }

    /// Run the closed loop of section 7 over every decision bar.
    pub fn run(&mut self) -> Result<RunResult, RunError> {
        let bars = self.decision_bars();
        if bars.is_empty() {
            return Err(RunError::NoBars);
        }
        let decide = self.rel("decide")?;
        let decided = self.rel("decided")?;
        let position = self.rel("position")?;
        let cash_rel = self.rel("cash")?;
        let fill_rel = self.rel("fill")?;
        let mut cash = self.cfg.initial_cash;
        let mut positions: BTreeMap<Sym, f64> = BTreeMap::new();
        self.stores[cash_rel].insert(bars[0], vec![Value::Time(bars[0]), Value::Num(cash)]);
        let mut result = RunResult {
            symbols: self.symbols.names().to_vec(),
            bars: bars.clone(),
            warnings: self.cfg.warnings(),
            ..Default::default()
        };
        let delta = self.prog.mode == DecisionMode::Delta;
        // Targets a participation cap left unreached, re-issued at each next
        // bar until reached or superseded by a decision on the instrument.
        let mut open_targets: BTreeMap<Sym, (Decision, usize)> = BTreeMap::new();
        let mut requested_total = 0.0;
        let mut filled_total = 0.0;
        let mut participations: Vec<f64> = Vec::new();
        let mut margin_calls = 0usize;
        let mut not_shortable = 0usize;
        let mut shorts_held = false;
        for (k, &t) in bars.iter().enumerate() {
            // Mark to market before the bar's decisions.
            let mut equity = cash;
            let mut gross = 0.0;
            let mut net = 0.0;
            for (&sym, &q) in &positions {
                if let Some(p) = self.price_at(sym, t) {
                    equity += q * p;
                    gross += q.abs() * p;
                    net += q * p;
                }
            }
            // Maintenance: positive equity below the margin of gross exposure
            // is a margin call (a non-positive equity is ruin, judged when an
            // order comes to be filled).
            if gross > 0.0 && equity > 0.0 && equity < self.cfg.maintenance_margin * gross - 1e-9 * gross {
                let message = format!(
                    "margin call: equity {:.2} is below {}% of gross exposure {:.2} at {}",
                    equity,
                    self.cfg.maintenance_margin * 100.0,
                    gross,
                    time::format_timestamp(t)
                );
                match self.cfg.on_margin_call {
                    OnMarginCall::Halt => {
                        return Err(RunError::Risk {
                            t,
                            rule: "(executor)".into(),
                            decision: "(mark to market)".into(),
                            message,
                        });
                    }
                    OnMarginCall::Allow => margin_calls += 1,
                    OnMarginCall::Liquidate => {
                        margin_calls += 1;
                        // Sell the fraction f of every position with
                        // equity >= maintenance * (1 - f) * gross.
                        let f = 1.0 - equity / (self.cfg.maintenance_margin * gross);
                        let syms: Vec<Sym> = positions.keys().copied().collect();
                        for sym in syms {
                            let pos = positions[&sym];
                            let Some(p) = self.price_at(sym, t) else { continue };
                            let mut qty = -(pos * f);
                            if self.cfg.lot == Lot::Whole {
                                qty = if qty < 0.0 { qty.floor() } else { qty.ceil() };
                            }
                            if qty == 0.0 || qty.abs() > pos.abs() {
                                qty = -pos;
                            }
                            let commission = if self.cfg.commission_per_share == 0.0 && self.cfg.commission_min_per_order == 0.0 {
                                0.0
                            } else {
                                (self.cfg.commission_per_share * qty.abs()).max(self.cfg.commission_min_per_order)
                            };
                            let fee = if qty < 0.0 { qty.abs() * p * self.cfg.fee_bps_on_sells / 10_000.0 } else { 0.0 };
                            cash -= qty * p + commission + fee;
                            equity -= commission + fee;
                            gross -= qty.abs() * p;
                            net -= -qty * p;
                            let new_pos = pos + qty;
                            if new_pos.abs() < 1e-9 {
                                positions.remove(&sym);
                            } else {
                                positions.insert(sym, new_pos);
                            }
                            result.costs.commissions += commission;
                            result.costs.fees += fee;
                            result.costs.turnover += qty.abs() * p;
                            self.stores[fill_rel].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(qty), Value::Num(p)]);
                            result.fills.push(FillRecord {
                                t,
                                equity: sym,
                                quantity: qty,
                                price: p,
                                at_last_price: false,
                                commission,
                                fee,
                                slippage: 0.0,
                                impact: 0.0,
                                participation: self.bar_volume(sym, t).filter(|v| *v > 0.0).map(|v| qty.abs() / v).unwrap_or(0.0),
                                partial: false,
                                forced: true,
                            });
                        }
                        // The executor relations at t describe the book after the call.
                        if let Some(tus) = self.stores[position].by_time.get_mut(&t) {
                            tus.clear();
                        }
                        for (&sym, &q) in &positions {
                            self.stores[position].insert(t, vec![Value::Equity(sym), Value::Time(t), Value::Num(q)]);
                        }
                        if let Some(tus) = self.stores[cash_rel].by_time.get_mut(&t) {
                            tus.clear();
                        }
                        self.stores[cash_rel].insert(t, vec![Value::Time(t), Value::Num(cash)]);
                    }
                }
            }
            result.exposure.push(ExposureRecord {
                t,
                cash,
                gross,
                net,
                equity,
                leverage: if equity > 0.0 { gross / equity } else { 0.0 },
            });
            result.equity_curve.push((t, equity));
            // Decisions at t.
            let tuples = self.call(decide, &[Some(Value::Time(t)), None])?;
            let mut by_equity: BTreeMap<Sym, Vec<(Decision, usize)>> = BTreeMap::new();
            for (tu, rule) in tuples.iter() {
                let Value::Decision(d) = &tu[1] else {
                    return Err(RunError::Internal("decide tuple without a decision".into()));
                };
                if delta && d.amount.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                    return Err(RunError::BadDecision {
                        t,
                        rule: self.prog.rule_label(*rule),
                        decision: result.describe_decision(d),
                        message: "delta decisions require a positive quantity".into(),
                    });
                }
                by_equity.entry(d.equity).or_default().push((d.clone(), *rule));
            }
            for (sym, ds) in &by_equity {
                if ds.len() > 1 {
                    let (a, ra) = &ds[0];
                    let (b, rb) = &ds[1];
                    return Err(RunError::Conflict {
                        t,
                        equity: self.symbols.name(*sym).to_string(),
                        first: (result.describe_decision(a), self.prog.rule_label(*ra)),
                        second: (result.describe_decision(b), self.prog.rule_label(*rb)),
                    });
                }
            }
            for ds in by_equity.values() {
                for (d, rule) in ds {
                    result.decisions.push(DecisionRecord { t, decision: d.clone(), rule: *rule });
                    self.stores[decided].insert(t, vec![Value::Time(t), Value::Decision(d.clone())]);
                }
            }
            // Executor: fills at the next bar (section 6, execution contract).
            if let Some(&tn) = bars.get(k + 1) {
                // Funding over the calendar time to the execution bar: interest
                // on cash or on the debit, borrow fee and rebate on short notional.
                let dt = (tn - t) as f64 / (365.0 * 86_400.0);
                if cash > 0.0 {
                    let i = cash * self.cfg.cash_rate * dt;
                    cash += i;
                    result.funding.cash_interest += i;
                } else if cash < 0.0 {
                    let i = -cash * self.cfg.margin_rate * dt;
                    cash -= i;
                    result.funding.margin_interest += i;
                }
                let shorts: Vec<(Sym, f64)> = positions.iter().filter(|(_, q)| **q < 0.0).map(|(s, q)| (*s, *q)).collect();
                for (sym, q) in shorts {
                    shorts_held = true;
                    let Some(p) = self.price_at(sym, tn) else { continue };
                    let notional = q.abs() * p;
                    let bucket = self.cfg.borrow_bucket(self.adv(sym, &bars, k + 1));
                    let fee = notional * bucket.fee_bps / 10_000.0 * dt;
                    let rebate = notional * self.cfg.short_rebate * dt;
                    cash -= fee;
                    cash += rebate;
                    result.funding.borrow_fees += fee;
                    result.funding.short_rebate += rebate;
                }
                // Mark the book at the execution bar.
                let mut equity_next = cash;
                for (&sym, &q) in &positions {
                    if let Some(p) = self.price_at(sym, tn) {
                        equity_next += q * p;
                    }
                }
                // Orders that reduce a position fund the ones that open or add,
                // so within a bar they fill first (symbol order within each group).
                // A decision on an instrument supersedes its open target; the
                // other open targets are re-issued (flagged, so they are not
                // counted as new requests).
                for sym in by_equity.keys() {
                    open_targets.remove(sym);
                }
                let mut pending: Vec<(Decision, usize, bool)> = by_equity.values().flatten().map(|(d, r)| (d.clone(), *r, false)).collect();
                pending.extend(open_targets.values().map(|(d, r)| (d.clone(), *r, true)));
                let mut marks: HashMap<Sym, f64> = HashMap::new();
                for (d, _, _) in &pending {
                    if let Some(p) = self.price_at(d.equity, tn) {
                        marks.insert(d.equity, p);
                    }
                }
                let reducing = |d: &Decision| -> bool {
                    let pos = positions.get(&d.equity).copied().unwrap_or(0.0);
                    match d.ctor {
                        Ctor::Sell | Ctor::Cover => pos != 0.0,
                        Ctor::Buy | Ctor::Short => false,
                        Ctor::TargetQuantity => pos != 0.0 && d.amount.abs() < pos.abs() && d.amount * pos >= 0.0,
                        Ctor::TargetWeight => {
                            pos != 0.0 && (d.amount == 0.0 || d.amount * pos < 0.0 || d.amount.abs() * equity_next.max(0.0) < pos.abs() * marks.get(&d.equity).copied().unwrap_or(0.0))
                        }
                    }
                };
                pending.sort_by_key(|(d, _, _)| !reducing(d));
                // Ruin: a book without positive equity cannot size or fund an order.
                if !pending.is_empty() && equity_next <= 0.0 && self.cfg.on_ruin == OnRuin::Halt {
                    let (d, rule, _) = &pending[0];
                    return Err(RunError::Risk {
                        t,
                        rule: self.prog.rule_label(*rule),
                        decision: result.describe_decision(d),
                        message: format!("ruin: equity at {} is {:.2}, not positive, with orders pending", time::format_timestamp(tn), equity_next),
                    });
                }
                let sizing_equity = equity_next.max(0.0);
                // A bar's transaction costs (commission, fee, slippage) are never
                // leverage: a fully invested book stays fully invested after
                // paying them, carrying a debit of at most the bar's costs, which
                // the next sizing sees (and margin interest prices).
                let mut bar_costs = 0.0;
                for (d, rule, reissued) in &pending {
                    let sym = d.equity;
                    let is_target = matches!(d.ctor, Ctor::TargetWeight | Ctor::TargetQuantity);
                    let name = self.symbols.name(sym).to_string();
                    let pos = positions.get(&sym).copied().unwrap_or(0.0);
                    let bar_price = self.bar_price(sym, tn);
                    // Slippage against the order: the fixed part plus a multiple
                    // of the instrument's realized volatility at the fill bar.
                    let slip = self.cfg.slippage_bps / 10_000.0 + self.cfg.slippage_vol_mult * self.realized_vol(sym, &bars, k + 1).unwrap_or(0.0);
                    let slipped = |p: f64, buying: bool| if buying { p * (1.0 + slip) } else { p * (1.0 - slip) };
                    // Lot rounding applies to what the decision names: a delta
                    // order's quantity, or a target's quantity (so a kept name
                    // never ends a fraction of a share over its target).
                    let round = |x: f64| -> f64 {
                        match self.cfg.lot {
                            Lot::Whole => x.trunc(),
                            Lot::Fractional => x,
                        }
                    };
                    let mut qty = match (d.ctor, bar_price) {
                        (Ctor::Buy | Ctor::Cover, _) => round(d.amount),
                        (Ctor::Sell | Ctor::Short, _) => -round(d.amount),
                        (Ctor::TargetQuantity, _) => round(d.amount) - pos,
                        // A long that is bought is sized at the price it will fill
                        // at, so the cash it spends is the weight of equity; any
                        // other target (a reduction, a short) is sized at the bar
                        // price, which is what the position is marked at.
                        (Ctor::TargetWeight, Some(p)) => {
                            let target = round(d.amount * sizing_equity / p);
                            let target = if target > pos && target > 0.0 {
                                round(d.amount * sizing_equity / slipped(p, true))
                            } else {
                                target
                            };
                            target - pos
                        }
                        // Without a price a weight cannot be sized, except the flat target.
                        (Ctor::TargetWeight, None) if d.amount == 0.0 => -pos,
                        (Ctor::TargetWeight, None) => {
                            open_targets.remove(&sym);
                            result.dropped.push((t, d.clone(), format!("no price for {} at {}", name, time::format_timestamp(tn))));
                            continue;
                        }
                    };
                    if qty == 0.0 {
                        // A target that is held: nothing to do, and an open one is reached.
                        open_targets.remove(&sym);
                        continue;
                    }
                    // Oversize: a delta order that would carry the position across zero.
                    let crosses = matches!(d.ctor, Ctor::Sell | Ctor::Cover) && pos != 0.0 && (pos + qty) * pos < 0.0;
                    if crosses {
                        let message = format!(
                            "{} {} of {} shares {}: the order would cross zero",
                            d.ctor.name(),
                            qty.abs(),
                            pos.abs(),
                            if pos > 0.0 { "held" } else { "short" }
                        );
                        match self.cfg.on_oversize {
                            OnOversize::Halt => {
                                return Err(RunError::Risk {
                                    t,
                                    rule: self.prog.rule_label(*rule),
                                    decision: result.describe_decision(d),
                                    message,
                                });
                            }
                            OnOversize::Clamp => {
                                let excess = qty.abs() - pos.abs();
                                result
                                    .dropped
                                    .push((t, d.clone(), format!("clamped at position: {} of {} shares filled, {} dropped", pos.abs(), qty.abs(), excess)));
                                qty = -pos;
                            }
                            OnOversize::Allow => {}
                        }
                    }
                    // Participation: a fill is at most the cap times the bar's volume.
                    let requested = qty.abs();
                    if !*reissued {
                        requested_total += requested;
                    }
                    let bar_volume = self.bar_volume(sym, tn);
                    let mut partial = false;
                    if let (true, Some(v)) = (self.cfg.participation_cap > 0.0, bar_volume) {
                        let cap = round(self.cfg.participation_cap * v);
                        if requested > cap {
                            partial = true;
                            qty = qty.signum() * cap;
                        }
                    }
                    let cap_pct = self.cfg.participation_cap * 100.0;
                    let partial_note = move |filled: f64| {
                        format!(
                            "partial fill: {} of {} shares (participation cap {}% of volume {})",
                            filled,
                            requested,
                            cap_pct,
                            bar_volume.unwrap_or(0.0)
                        )
                    };
                    if qty == 0.0 {
                        if is_target {
                            open_targets.insert(sym, (d.clone(), *rule));
                        } else {
                            result.dropped.push((t, d.clone(), format!("{}; remainder expired", partial_note(0.0))));
                        }
                        continue;
                    }
                    // The order shrinks the position without crossing zero: it is a
                    // liquidation (fillable at the last price) and never leverage.
                    let reduces = pos != 0.0 && (pos + qty) * pos >= 0.0 && (pos + qty).abs() < pos.abs();
                    // Price: the bar's, or the last known one for a liquidation.
                    let (p, at_last_price) = match bar_price {
                        Some(p) => (p, false),
                        None => match self.last_price.get(&sym).copied() {
                            Some(p) if reduces => (p, true),
                            _ => {
                                open_targets.remove(&sym);
                                result.dropped.push((t, d.clone(), format!("no price for {} at {}", name, time::format_timestamp(tn))));
                                continue;
                            }
                        },
                    };
                    // Impact: square root in participation of average daily volume.
                    let imp = match (self.cfg.impact_coef > 0.0, self.adv(sym, &bars, k + 1)) {
                        (true, Some(adv)) if adv > 0.0 => self.cfg.impact_coef * (qty.abs() / adv).sqrt(),
                        _ => 0.0,
                    };
                    let fill_price = if qty > 0.0 { p * (1.0 + slip + imp) } else { p * (1.0 - slip - imp) };
                    let commission = if self.cfg.commission_per_share == 0.0 && self.cfg.commission_min_per_order == 0.0 {
                        0.0
                    } else {
                        (self.cfg.commission_per_share * qty.abs()).max(self.cfg.commission_min_per_order)
                    };
                    let fee = if qty < 0.0 { qty.abs() * fill_price * self.cfg.fee_bps_on_sells / 10_000.0 } else { 0.0 };
                    let cost = qty * fill_price + commission + fee;
                    let slippage = qty.abs() * p * slip;
                    let impact = qty.abs() * p * imp;
                    let allowance = bar_costs + commission + fee + slippage + impact;
                    // Borrow availability: an order that opens or adds to a short
                    // needs its instrument's ADV bucket to be shortable.
                    if pos + qty < 0.0 && pos + qty < pos {
                        let adv = self.adv(sym, &bars, k + 1);
                        let bucket = self.cfg.borrow_bucket(adv);
                        if !bucket.shortable {
                            not_shortable += 1;
                            open_targets.remove(&sym);
                            result.dropped.push((
                                t,
                                d.clone(),
                                format!(
                                    "not shortable: ADV {} of {} is below {} (the smallest borrow bucket)",
                                    adv.map(|a| format!("{:.0}", a)).unwrap_or_else(|| "unknown".into()),
                                    name,
                                    bucket.adv_below
                                ),
                            ));
                            continue;
                        }
                    }
                    // Leverage: only an order that adds exposure can borrow, and
                    // only up to the configured gross multiple of equity.
                    if !reduces {
                        let new_cash = cash - cost;
                        let mut gross = 0.0;
                        let mut net = new_cash;
                        for (&s2, &q2) in &positions {
                            let q2 = if s2 == sym { q2 + qty } else { q2 };
                            let p2 = if s2 == sym { p } else { self.price_at(s2, tn).unwrap_or(0.0) };
                            gross += q2.abs() * p2;
                            net += q2 * p2;
                        }
                        if !positions.contains_key(&sym) {
                            gross += qty.abs() * p;
                            net += qty * p;
                        }
                        let tol = 1e-9 * (1.0 + net.abs()) + allowance;
                        let max_gross = self.cfg.max_gross.max(1.0);
                        if new_cash < -(max_gross - 1.0) * net.max(0.0) - tol || gross > max_gross * net + tol {
                            let message = format!(
                                "leverage: after the fill cash would be {:.2} and gross exposure {:.2} against equity {:.2} (limit {}x gross)",
                                new_cash, gross, net, max_gross
                            );
                            match self.cfg.on_leverage {
                                OnLeverage::Halt => {
                                    return Err(RunError::Risk {
                                        t,
                                        rule: self.prog.rule_label(*rule),
                                        decision: result.describe_decision(d),
                                        message,
                                    });
                                }
                                OnLeverage::Reject => {
                                    result.dropped.push((
                                        t,
                                        d.clone(),
                                        format!("rejected, would exceed {}x equity: cash {:.2}, gross {:.2} against equity {:.2}", max_gross, new_cash, gross, net),
                                    ));
                                    continue;
                                }
                                OnLeverage::Allow => {}
                            }
                        }
                    }
                    cash -= cost;
                    let new_pos = pos + qty;
                    if new_pos.abs() < 1e-9 {
                        positions.remove(&sym);
                    } else {
                        positions.insert(sym, new_pos);
                    }
                    self.last_price.insert(sym, fill_price);
                    self.stores[fill_rel].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(qty), Value::Num(fill_price)]);
                    bar_costs += commission + fee + slippage + impact;
                    result.costs.commissions += commission;
                    result.costs.fees += fee;
                    result.costs.slippage += slippage;
                    result.costs.impact += impact;
                    result.costs.turnover += qty.abs() * fill_price;
                    filled_total += qty.abs();
                    let participation = bar_volume.filter(|v| *v > 0.0).map(|v| qty.abs() / v).unwrap_or(0.0);
                    if bar_volume.is_some() {
                        participations.push(participation);
                    }
                    result.fills.push(FillRecord {
                        t: tn,
                        equity: sym,
                        quantity: qty,
                        price: fill_price,
                        at_last_price,
                        commission,
                        fee,
                        slippage,
                        impact,
                        participation,
                        partial,
                        forced: false,
                    });
                    // The remainder of a capped order: a delta order expires, a
                    // target re-issues itself at the next bar.
                    if partial {
                        if is_target {
                            open_targets.insert(sym, (d.clone(), *rule));
                        } else {
                            result.dropped.push((t, d.clone(), format!("{}; remainder expired", partial_note(qty.abs()))));
                        }
                    } else if is_target {
                        open_targets.remove(&sym);
                    }
                }
                for (&sym, &q) in &positions {
                    self.stores[position].insert(tn, vec![Value::Equity(sym), Value::Time(tn), Value::Num(q)]);
                }
                self.stores[cash_rel].insert(tn, vec![Value::Time(tn), Value::Num(cash)]);
            } else {
                // A decision on the last bar has no bar to fill at: it is
                // recorded in `decided` like any other, and dropped here so
                // that decisions = fills + dropped.
                for ds in by_equity.values() {
                    for (d, _) in ds {
                        result.dropped.push((t, d.clone(), "no next bar".to_string()));
                    }
                }
            }
        }
        result.liquidity = LiquiditySummary {
            fill_ratio: if requested_total > 0.0 { filled_total / requested_total } else { 1.0 },
            avg_participation: if participations.is_empty() {
                0.0
            } else {
                participations.iter().sum::<f64>() / participations.len() as f64
            },
            max_participation: participations.iter().cloned().fold(0.0, f64::max),
        };
        let above = participations.iter().filter(|p| **p > self.cfg.participation_warn).count();
        if above > 0 {
            result.warnings.push(RunWarning {
                bias: "market-impact".into(),
                message: format!(
                    "{} fills above {}% of bar volume (max {:.1}%); impact is modeled, capacity is limited",
                    above,
                    self.cfg.participation_warn * 100.0,
                    result.liquidity.max_participation * 100.0
                ),
            });
        }
        if margin_calls > 0 {
            result.warnings.push(RunWarning {
                bias: "leverage".into(),
                message: format!(
                    "{} margin calls (equity below {}% of gross) {}",
                    margin_calls,
                    self.cfg.maintenance_margin * 100.0,
                    if self.cfg.on_margin_call == OnMarginCall::Liquidate {
                        "liquidated pro rata at the bar's close"
                    } else {
                        "ignored"
                    }
                ),
            });
        }
        if result.funding.margin_interest > 0.0 {
            result.warnings.push(RunWarning {
                bias: "funding-cost neglect".into(),
                message: format!(
                    "margin interest of {:.2} charged at a constant {}% a year, not a rate series",
                    result.funding.margin_interest,
                    self.cfg.margin_rate * 100.0
                ),
            });
        }
        if shorts_held {
            result.warnings.push(RunWarning {
                bias: "shorting".into(),
                message: format!("borrow fees of {:.2} by ADV bucket are a proxy, modeled, not observed", result.funding.borrow_fees),
            });
        }
        if not_shortable > 0 {
            result.warnings.push(RunWarning {
                bias: "borrow-availability".into(),
                message: format!(
                    "{} short orders dropped: the instrument's ADV is in the smallest bucket, which is not shortable (a proxy for locate)",
                    not_shortable
                ),
            });
        }
        result.final_cash = cash;
        result.final_positions = positions;
        Ok(result)
    }

    /// Price of `sym` for marking at decision bar `t`: the bar's price, or
    /// the last price seen when the bar has none.
    pub fn price_at(&mut self, sym: Sym, t: i64) -> Option<f64> {
        match self.bar_price(sym, t) {
            Some(p) => Some(p),
            None => self.last_price.get(&sym).copied(),
        }
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
                rets.push((b / a).ln());
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
        if let Some(s) = self.symbols.get(raw) {
            return Ok(Value::Equity(s));
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

    fn equity(&self, name: &str) -> Result<Value, String> {
        self.symbols.get(name).map(Value::Equity).ok_or_else(|| format!("`{}` is not an equity of the dataset", name))
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

pub(crate) fn lit_value(l: &Lit, symbols: &mut Symbols, labels: &mut Symbols) -> Value {
    match l {
        Lit::Int(i) => Value::Count(*i),
        Lit::Float(x) | Lit::Shares(x) | Lit::Money(x, _) | Lit::Price(x, _) => Value::Num(*x),
        Lit::Duration(d) => Value::Dur(*d),
        // The checker resolves every string literal; an unresolved one can
        // only come from an unchecked program, and reads as an equity.
        Lit::Str(s) | Lit::Equity(s) => Value::Equity(symbols.intern(s)),
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
                expr(n, f);
                atom.terms.iter().for_each(|t| term(t, f));
            }
            Literal::Resample { inner, min, aggs, .. } => {
                inner.terms.iter().for_each(|t| term(t, f));
                expr(min, f);
                aggs.iter().for_each(|(_, _, e)| expr(e, f));
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
            Literal::Top { n, atom, by, .. } => {
                expr(n, f);
                atom.terms.iter().for_each(|t| term(t, f));
                if let Some(by) = by {
                    by.iter().for_each(|(k, _, _)| f(k));
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
    let full = run(prog, dataset, cfg.clone())?;
    let describe = |r: &RunResult, t: i64| -> Vec<String> {
        let mut v: Vec<String> = r.decisions_at(t).into_iter().map(|d| r.describe_decision(d)).collect();
        v.sort();
        v
    };
    let mut out = Vec::new();
    for &t in samples {
        let trunc = dataset.truncated(prog, t);
        let partial = run(prog, &trunc, cfg.clone())?;
        let a = describe(&full, t);
        let b = describe(&partial, t);
        if a != b {
            out.push(CausalityMismatch { t, full: a, truncated: b });
        }
    }
    Ok(out)
}
