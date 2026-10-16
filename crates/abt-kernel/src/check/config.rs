//! The closed configuration of a program (language-v2, 4.3 and 4.5): the
//! execution block with its fifteen fields, expanded from a preset and its
//! overrides, its optional pre-trade limits (K-3.5), and the report block.
//! Every field that changes the model is required (WF-14, code P); the
//! kernel's `ExecConfig` is built from the block and nothing else.

use std::fmt;

use crate::ir::{Duration, Span};
use crate::syntax::ast::{ExecField, ExecValue, ExecutionBlock, ReportBlock};

/// What a pre-trade limit bounds (K-3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Gross,
    Net,
    Weight,
    Loss,
    Drawdown,
    Turnover,
    Participation,
    Names,
}

/// What happens to the bar's orders when a limit would be passed: the run
/// halts, the increasing orders are dropped, they are scaled to the limit,
/// or the run warns and goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitPolicy {
    Halt,
    Reject,
    Clamp,
    Warn,
}

impl LimitPolicy {
    pub fn name(self) -> &'static str {
        match self {
            LimitPolicy::Halt => "halt",
            LimitPolicy::Reject => "reject",
            LimitPolicy::Clamp => "clamp",
            LimitPolicy::Warn => "warn",
        }
    }
    fn parse(s: &str) -> Option<LimitPolicy> {
        Some(match s {
            "halt" => LimitPolicy::Halt,
            "reject" => LimitPolicy::Reject,
            "clamp" => LimitPolicy::Clamp,
            "warn" => LimitPolicy::Warn,
            _ => return None,
        })
    }
}

impl LimitKind {
    pub const ALL: [LimitKind; 8] = [
        LimitKind::Gross,
        LimitKind::Net,
        LimitKind::Weight,
        LimitKind::Loss,
        LimitKind::Drawdown,
        LimitKind::Turnover,
        LimitKind::Participation,
        LimitKind::Names,
    ];
    pub fn name(self) -> &'static str {
        match self {
            LimitKind::Gross => "gross",
            LimitKind::Net => "net",
            LimitKind::Weight => "weight",
            LimitKind::Loss => "loss",
            LimitKind::Drawdown => "drawdown",
            LimitKind::Turnover => "turnover",
            LimitKind::Participation => "participation",
            LimitKind::Names => "names",
        }
    }
    fn parse(s: &str) -> Option<LimitKind> {
        LimitKind::ALL.into_iter().find(|k| k.name() == s)
    }
    /// The policies K-3.5 admits for the kind: a count, a loss and a
    /// drawdown cannot be scaled; a turnover and a participation never halt.
    pub fn policies(self) -> &'static [LimitPolicy] {
        use LimitPolicy::*;
        match self {
            LimitKind::Gross | LimitKind::Net | LimitKind::Weight => &[Halt, Reject, Clamp, Warn],
            LimitKind::Names | LimitKind::Loss | LimitKind::Drawdown => &[Halt, Reject, Warn],
            LimitKind::Turnover | LimitKind::Participation => &[Reject, Clamp, Warn],
        }
    }
    /// The `per` the kind is written with: `weight <= X per name`, `loss <=
    /// X per day`, `turnover <= X per PERIOD`.
    fn per(self) -> Option<&'static [&'static str]> {
        match self {
            LimitKind::Weight => Some(&["name"]),
            LimitKind::Loss => Some(&["day"]),
            LimitKind::Turnover => Some(&["day", "week", "month", "quarter", "year"]),
            _ => None,
        }
    }
}

/// A pre-trade limit of `execution.limits` (K-3.5): `gross`, `net`,
/// `weight` and `turnover` bounds are fractions of the capital, `loss` of
/// the capital lost in a day, `drawdown` of the peak NAV, `participation`
/// of the bar's volume, `names` a count.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Limit {
    pub kind: LimitKind,
    pub bound: f64,
    /// The trailing period of a turnover limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    pub policy: LimitPolicy,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} <= {}", self.kind.name(), self.bound)?;
        match (self.kind, &self.period) {
            (LimitKind::Weight, _) => write!(f, " per name")?,
            (LimitKind::Loss, _) => write!(f, " per day")?,
            (_, Some(p)) => write!(f, " per {}", p)?,
            _ => {}
        }
        write!(f, " {}", self.policy.name())
    }
}

/// The limits as written, checked: a known kind once, a positive bound (a
/// whole one for `names`), the `per` the kind takes, a policy it admits
/// (`reject` when none is written).
fn expand_limits(items: &[crate::syntax::ast::LimitItem]) -> Result<Vec<Limit>, Vec<(Span, String)>> {
    let mut out: Vec<Limit> = Vec::new();
    let mut errors = Vec::new();
    for it in items {
        let Some(kind) = LimitKind::parse(&it.kind) else {
            let known: Vec<&str> = LimitKind::ALL.iter().map(|k| k.name()).collect();
            errors.push((it.span, format!("`{}` is not a limit ({})", it.kind, known.join(", "))));
            continue;
        };
        if out.iter().any(|l| l.kind == kind) {
            errors.push((it.span, format!("the `{}` limit is set twice", kind.name())));
            continue;
        }
        if !(it.bound > 0.0 && it.bound.is_finite()) || (kind == LimitKind::Names && it.bound.fract() != 0.0) {
            errors.push((
                it.span,
                format!(
                    "the `{}` limit's bound {} is not a positive {}",
                    kind.name(),
                    it.bound,
                    if kind == LimitKind::Names { "whole number" } else { "number" }
                ),
            ));
            continue;
        }
        let period = match (kind.per(), &it.per) {
            (None, None) => None,
            (None, Some(p)) => {
                errors.push((it.span, format!("the `{}` limit takes no `per` (found `per {}`)", kind.name(), p)));
                continue;
            }
            (Some(admitted), Some(p)) if admitted.contains(&p.as_str()) => (kind == LimitKind::Turnover).then(|| p.clone()),
            (Some(admitted), written) => {
                errors.push((
                    it.span,
                    format!(
                        "the `{}` limit is written `{} <= X per {}`{}",
                        kind.name(),
                        kind.name(),
                        admitted.join("|"),
                        written.as_ref().map(|p| format!(", not `per {}`", p)).unwrap_or_default()
                    ),
                ));
                continue;
            }
        };
        let policy = match it.policy.as_deref().map(LimitPolicy::parse) {
            None => LimitPolicy::Reject,
            Some(Some(p)) if kind.policies().contains(&p) => p,
            Some(_) => {
                let admitted: Vec<&str> = kind.policies().iter().map(|p| p.name()).collect();
                errors.push((
                    it.span,
                    format!("the `{}` limit admits {}, not `{}`", kind.name(), admitted.join(", "), it.policy.as_deref().unwrap_or_default()),
                ));
                continue;
            }
        };
        out.push(Limit {
            kind,
            bound: it.bound,
            period,
            policy,
        });
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// The commission schedule.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Commission {
    None,
    /// `per_share P min M`.
    PerShare {
        per_share: f64,
        min: f64,
    },
    /// `per_contract from catalog`.
    PerContract,
    /// `X bps per side`.
    Bps(f64),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Slippage {
    None,
    Fixed(f64),
    Vol { mult: f64, bars: u32 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Impact {
    None,
    Sqrt { coef: f64, days: u32 },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Leverage {
    Unconstrained,
    RegT,
    Gross { max: f64, halt: bool },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Delisting {
    LastPrice,
    Haircut(Vec<(String, f64)>),
}

/// The expanded execution block.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecBlock {
    /// The price relation the executor fills and marks at, with its
    /// calendar instance when written (`tr_index@weekly`).
    pub price: String,
    pub price_cal: Option<String>,
    /// The instance the price relation resolved to (the elaborator sets it).
    #[serde(default)]
    pub price_instance: String,
    pub orders: String,
    pub lot_fractional: bool,
    pub capital: f64,
    pub compounding: bool,
    pub commission: Commission,
    pub slippage: Slippage,
    pub impact: Impact,
    pub participation: Option<f64>,
    /// `cash X debit Y short Z`, or `None`.
    pub financing: Option<(f64, f64, f64)>,
    pub leverage: Leverage,
    pub actions_in_price: bool,
    pub delisting: Delisting,
    pub oversize: String,
    pub ruin: String,
    pub numerics: String,
    /// The pre-trade limits (K-3.5), optional.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<Limit>,
}

impl ExecBlock {
    /// The price relation's instance name as the program names it.
    pub fn price_relation(&self) -> String {
        match &self.price_cal {
            Some(c) if c != "base" => format!("{}@{}", self.price, c),
            _ => self.price.clone(),
        }
    }
    /// The fields whose value turns a model off, each named by the bias it
    /// leaves unmodeled (data-bundle doc, section 5): reported on every run.
    pub fn disabled_models(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.commission == Commission::None {
            out.push(("transaction-cost neglect", "commission none: fills cost nothing but their price".into()));
        }
        if self.slippage == Slippage::None {
            out.push(("slippage", "slippage none: every order fills at the bar's price".into()));
        }
        if self.impact == Impact::None {
            out.push(("market-impact", "impact none: a large order fills at the quoted price".into()));
        }
        if self.participation.is_none() {
            out.push(("liquidity", "participation none: an order fills whole whatever the bar's volume".into()));
        }
        if self.financing.is_none() {
            out.push(("cash-management", "financing none: cash earns nothing and a debit costs nothing".into()));
        } else if self.financing.map(|f| f.0 == 0.0).unwrap_or(false) {
            out.push(("cash-management", "financing cash 0: positive cash earns nothing".into()));
        }
        if self.leverage == Leverage::Unconstrained {
            out.push(("leverage", "leverage unconstrained: no gross limit and no margin call".into()));
        }
        out
    }
}

impl fmt::Display for ExecBlock {
    /// The block as the program would write it (what `abt show` prints).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "execution {{")?;
        writeln!(f, "  price         {}", self.price_relation())?;
        writeln!(f, "  orders        {}", self.orders)?;
        writeln!(f, "  lot           {}", if self.lot_fractional { "fractional" } else { "whole" })?;
        writeln!(f, "  capital       {} USD {}", self.capital, if self.compounding { "compounding" } else { "fixed" })?;
        let commission = match &self.commission {
            Commission::None => "none".to_string(),
            Commission::PerShare { per_share, min } => format!("per_share {} USD min {} USD", per_share, min),
            Commission::PerContract => "per_contract from catalog".to_string(),
            Commission::Bps(x) => format!("{} bps per side", x),
        };
        writeln!(f, "  commission    {}", commission)?;
        let slippage = match &self.slippage {
            Slippage::None => "none".to_string(),
            Slippage::Fixed(x) => format!("fixed {} bps", x),
            Slippage::Vol { mult, bars } => format!("vol {} over {} bars", mult, bars),
        };
        writeln!(f, "  slippage      {}", slippage)?;
        let impact = match &self.impact {
            Impact::None => "none".to_string(),
            Impact::Sqrt { coef, days } => format!("sqrt {} over {} days adv", coef, days),
        };
        writeln!(f, "  impact        {}", impact)?;
        writeln!(f, "  participation {}", self.participation.map(|x| format!("{} of bar volume", x)).unwrap_or_else(|| "none".into()))?;
        writeln!(
            f,
            "  financing     {}",
            self.financing.map(|(c, d, s)| format!("cash {} debit {} short {}", c, d, s)).unwrap_or_else(|| "none".into())
        )?;
        let leverage = match &self.leverage {
            Leverage::Unconstrained => "unconstrained".to_string(),
            Leverage::RegT => "reg_t".to_string(),
            Leverage::Gross { max, halt } => format!("gross <= {} {}", max, if *halt { "halt" } else { "reject" }),
        };
        writeln!(f, "  leverage      {}", leverage)?;
        writeln!(f, "  actions       {}", if self.actions_in_price { "in_price" } else { "apply" })?;
        let delisting = match &self.delisting {
            Delisting::LastPrice => "last_price".to_string(),
            Delisting::Haircut(hs) => {
                let parts: Vec<String> = hs.iter().map(|(r, x)| format!("{} {}", r, x)).collect();
                format!("haircut {{ {} }}", parts.join(", "))
            }
        };
        writeln!(f, "  delisting     {}", delisting)?;
        writeln!(f, "  oversize      {}", self.oversize)?;
        writeln!(f, "  ruin          {}", self.ruin)?;
        writeln!(f, "  numerics      {}", self.numerics)?;
        if !self.limits.is_empty() {
            let parts: Vec<String> = self.limits.iter().map(|l| l.to_string()).collect();
            writeln!(f, "  limits        {{ {} }}", parts.join("; "))?;
        }
        write!(f, "}}")
    }
}

/// The fifteen fields, in the order the block writes them.
pub const FIELDS: [&str; 15] = [
    "price",
    "orders",
    "lot",
    "capital",
    "commission",
    "slippage",
    "impact",
    "participation",
    "financing",
    "leverage",
    "actions",
    "delisting",
    "oversize",
    "ruin",
    "numerics",
];

/// Expand an execution block: the preset's fields (when it names one)
/// overridden by its `with` fields, every field required. Errors are
/// returned with the span to report them at.
pub fn expand_execution(block: &ExecutionBlock, presets: &[(String, Vec<ExecField>)]) -> Result<ExecBlock, Vec<(Span, String)>> {
    let mut fields: Vec<ExecField> = Vec::new();
    let span = match block {
        ExecutionBlock::Preset { name, span, with } => {
            match presets.iter().find(|(n, _)| n == name) {
                Some((_, f)) => fields.extend(f.iter().cloned()),
                None => {
                    let known: Vec<&str> = presets.iter().map(|(n, _)| n.as_str()).collect();
                    return Err(vec![(
                        *span,
                        format!(
                            "execution preset `{}` is not in the workspace; the presets are {}",
                            name,
                            if known.is_empty() { "none".to_string() } else { known.join(", ") }
                        ),
                    )]);
                }
            }
            for f in with {
                fields.retain(|g| g.name != f.name);
                fields.push(f.clone());
            }
            *span
        }
        ExecutionBlock::Fields { span, fields: f } => {
            fields.extend(f.iter().cloned());
            *span
        }
    };
    let mut errors = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for f in &fields {
        if seen.contains(&f.name.as_str()) {
            errors.push((f.span, format!("field `{}` is set twice in the execution block", f.name)));
        }
        seen.push(&f.name);
    }
    let missing: Vec<&str> = FIELDS.iter().copied().filter(|n| !seen.contains(n)).collect();
    if !missing.is_empty() {
        errors.push((
            span,
            format!(
                "the execution block sets no `{}`; every field is required, since the run is the program (the fifteen fields are {})",
                missing.join("`, `"),
                FIELDS.join(", ")
            ),
        ));
    }
    let limits = match fields.iter().find(|f| f.name == "limits").map(|f| &f.value) {
        Some(ExecValue::Limits(items)) => expand_limits(items).unwrap_or_else(|e| {
            errors.extend(e);
            Vec::new()
        }),
        _ => Vec::new(),
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    let get = |name: &str| -> &ExecValue { &fields.iter().find(|f| f.name == name).unwrap().value };
    let word = |name: &str| -> String {
        match get(name) {
            ExecValue::Word(w) => w.clone(),
            _ => String::new(),
        }
    };
    let (price, price_cal) = match get("price") {
        ExecValue::Price { rel, cal } => (rel.clone(), cal.clone()),
        _ => (String::new(), None),
    };
    let (capital, compounding) = match get("capital") {
        ExecValue::Capital { amount, compounding } => (*amount, *compounding),
        _ => (0.0, false),
    };
    let commission = match get("commission") {
        ExecValue::None => Commission::None,
        ExecValue::CommissionPerShare { per_share, min } => Commission::PerShare { per_share: *per_share, min: *min },
        ExecValue::CommissionPerContract => Commission::PerContract,
        ExecValue::CommissionBps(x) => Commission::Bps(*x),
        _ => Commission::None,
    };
    let slippage = match get("slippage") {
        ExecValue::SlippageFixed(x) => Slippage::Fixed(*x),
        ExecValue::SlippageVol { mult, bars } => Slippage::Vol { mult: *mult, bars: *bars },
        _ => Slippage::None,
    };
    let impact = match get("impact") {
        ExecValue::Impact { coef, days } => Impact::Sqrt { coef: *coef, days: *days },
        _ => Impact::None,
    };
    let participation = match get("participation") {
        ExecValue::Participation(x) => Some(*x),
        _ => None,
    };
    let financing = match get("financing") {
        ExecValue::Financing { cash, debit, short } => Some((*cash, *debit, *short)),
        _ => None,
    };
    let leverage = match get("leverage") {
        ExecValue::LeverageGross { max, policy } => Leverage::Gross { max: *max, halt: policy == "halt" },
        ExecValue::Word(w) if w == "reg_t" => Leverage::RegT,
        _ => Leverage::Unconstrained,
    };
    let delisting = match get("delisting") {
        ExecValue::Haircuts(hs) => Delisting::Haircut(hs.clone()),
        _ => Delisting::LastPrice,
    };
    Ok(ExecBlock {
        price_instance: price.clone(),
        price,
        price_cal,
        orders: word("orders"),
        lot_fractional: word("lot") == "fractional",
        capital,
        compounding,
        commission,
        slippage,
        impact,
        participation,
        financing,
        leverage,
        actions_in_price: word("actions") == "in_price",
        delisting,
        oversize: word("oversize"),
        ruin: word("ruin"),
        numerics: word("numerics"),
        limits,
    })
}

/// The report block: the reporting calendar (by its declared name), the
/// periods a year, and the run's window.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReportConfig {
    pub calendar: String,
    pub periods_per_year: f64,
    pub window: Option<(i64, i64)>,
}

impl fmt::Display for ReportConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "report {{ calendar {}   periods {} per year", self.calendar, self.periods_per_year)?;
        if let Some((lo, hi)) = self.window {
            write!(f, "   window {} .. {}", crate::kernel::time::format_timestamp(lo), crate::kernel::time::format_timestamp(hi))?;
        }
        write!(f, " }}")
    }
}

pub fn expand_report(block: &ReportBlock) -> Result<ReportConfig, Vec<(Span, String)>> {
    let mut errors = Vec::new();
    if block.calendar.is_none() {
        errors.push((block.span, "the report block names no `calendar`; every field is required".to_string()));
    }
    if block.periods.is_none() {
        errors.push((block.span, "the report block sets no `periods N per year`; every field is required".to_string()));
    }
    if let Some((n, sp)) = block.periods {
        if n <= 0.0 {
            errors.push((sp, format!("`periods {} per year` must be positive", n)));
        }
    }
    if let Some((lo, hi, sp)) = block.window {
        if lo > hi {
            errors.push((sp, "the report window's start is after its end".to_string()));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ReportConfig {
        calendar: block.calendar.as_ref().unwrap().0.clone(),
        periods_per_year: block.periods.unwrap().0,
        window: block.window.map(|(lo, hi, _)| (lo, hi)),
    })
}

/// The decision clock: the decision calendar's name and the session offset.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Clock {
    pub calendar: String,
    pub offset: Option<Duration>,
}
