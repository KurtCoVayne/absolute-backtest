//! The abstract syntax of the second formulation (corpus-v2/README.md, "The
//! grammar the corpus assumes"): units are strategies, libraries, classes
//! and execution presets; a strategy is nine blocks. Bodies are the literal
//! forms over surface atoms, which carry bracketed arguments and calendar
//! instances until the elaborator links them.

use crate::ir::{Cal, CmpOp, Dir, Duration, Expr, Lit, Resolution, Span, Term, Ties};

/// A vocabulary line of a class: its kind, its items (a series name with
/// its calendar), and its span.
pub type VocabEntry = (String, Vec<(String, Option<Resolution>)>, Span);
/// The instruments table of a universe block: the rows (name, first bar,
/// span) and the block's span.
pub type InstrumentsTable = (Vec<(String, Option<i64>, Span)>, Span);

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Unit {
    Strategy(Strategy),
    Library(Library),
    Class(Class),
    Execution(ExecutionPreset),
}

impl Unit {
    pub fn name(&self) -> &str {
        match self {
            Unit::Strategy(s) => &s.name,
            Unit::Library(l) => &l.name,
            Unit::Class(c) => &c.name,
            Unit::Execution(e) => &e.name,
        }
    }
    pub fn span(&self) -> Span {
        match self {
            Unit::Strategy(s) => s.span,
            Unit::Library(l) => l.span,
            Unit::Class(c) => c.span,
            Unit::Execution(e) => e.span,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Unit::Strategy(_) => "strategy",
            Unit::Library(_) => "library",
            Unit::Class(_) => "class",
            Unit::Execution(_) => "execution",
        }
    }
}

/// A strategy: the nine blocks, each optional in the syntax so that the
/// checker can name the missing one (WF-14).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Strategy {
    pub name: String,
    pub span: Span,
    pub data: Option<DataBlock>,
    pub uses: Vec<(String, Span)>,
    pub calendars: Option<CalendarsBlock>,
    pub clock: Option<ClockBlock>,
    pub universe: Option<UniverseBlock>,
    pub params: Option<ParamsBlock>,
    pub rules: Option<RulesBlock>,
    pub decisions: Option<DecisionsBlock>,
    pub execution: Option<ExecutionBlock>,
    pub report: Option<ReportBlock>,
    /// Every block header with its span, in order, for duplicate detection.
    pub blocks: Vec<(String, Span)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Library {
    pub name: String,
    pub span: Span,
    pub data: Option<DataBlock>,
    pub uses: Vec<(String, Span)>,
    pub rules: RulesBlock,
}

/// A class catalog, as `abt bundle env` generates it: the base resolution,
/// the unit, the primitive signatures and the vocabularies.
#[derive(Clone, Debug, PartialEq)]
pub struct Class {
    pub name: String,
    pub span: Span,
    pub base: Option<Resolution>,
    pub unit: Option<String>,
    pub rels: Vec<RelDecl>,
    /// `lists "SPX", ...`, `reasons ...`, `schemes ...`, `names "SPXTR" @1d, ...`.
    pub vocab: Vec<VocabEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionPreset {
    pub name: String,
    pub span: Span,
    pub fields: Vec<ExecField>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DataBlock {
    pub span: Span,
    pub class: Option<(String, Span)>,
    pub needs: Vec<Need>,
    pub require: Option<(Require, Span)>,
    pub prefer: Option<(Prefer, Span)>,
    pub budget: Option<(Budget, Span)>,
}

/// `budget { money X; bytes Y; requests Z; seconds S }` (A-D22, D-7.1): caps
/// on what resolving the demand may spend, each unbounded when absent but
/// money, which is zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Budget {
    pub money: Option<f64>,
    pub bytes: Option<u64>,
    pub requests: Option<u64>,
    pub seconds: Option<f64>,
}

/// `need chain of UNDERLYING within EXPIRY_RANGE and MONEYNESS_RANGE [right
/// R] [spot RELATION]` (D-7.2): the option contracts a program queries per
/// barrier, by expiry from the bar and strike over the spot.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainNeed {
    pub underlying: String,
    pub expiry: (Duration, Duration),
    pub moneyness: (f64, f64),
    /// `call` or `put`; both when absent.
    pub right: Option<String>,
    /// The relation the spot is read from; the class's `close` when absent.
    pub spot: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NeedKind {
    Bars { res: Option<Resolution>, from: Option<i64> },
    Actions,
    Listing,
    Lists(Vec<String>),
    Series(Vec<(String, Option<Resolution>)>),
    Sessions,
    PointValue,
    Commission,
    Instruments { attr: String, value: String },
    Chain(ChainNeed),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Need {
    pub kind: NeedKind,
    pub complete: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Require {
    /// Every version the binding names carries a reviewer's `reviewed`
    /// attribute (K-2.1).
    Reviewed,
    /// Versions no reviewer vouched for are admitted, warned on every run.
    AllowUnreviewed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prefer {
    Latest,
    Version(String),
    BaseFinest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CalendarsBlock {
    pub span: Span,
    pub decls: Vec<(String, Cal, Span)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClockBlock {
    pub span: Span,
    pub on: (String, Span),
    /// `at session + OFFSET`.
    pub offset: Option<(Lit, Span)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UniverseBlock {
    pub span: Span,
    pub instruments: Option<InstrumentsTable>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamsBlock {
    pub span: Span,
    pub params: Vec<ParamDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamDecl {
    pub name: String,
    pub ty: TyName,
    pub value: Lit,
    pub range: Option<(Lit, Lit)>,
    pub span: Span,
}

/// A surface type name: `Price<USD>`, `Count`, `Equity`, `D`.
#[derive(Clone, Debug, PartialEq)]
pub struct TyName {
    pub name: String,
    pub arg: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RulesBlock {
    pub span: Span,
    pub rels: Vec<RelDecl>,
    pub rules: Vec<Rule>,
    pub observes: Vec<Observe>,
}

/// `rel name[INPUT: TYPE, ...](A: Equity, @T[: CAL | CAL], X: TYPE, ...) [complete]`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelDecl {
    pub name: String,
    pub inputs: Vec<InputDecl>,
    pub args: Vec<ArgDecl>,
    pub complete: bool,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputTy {
    Value(TyName),
    /// `rel(Entity, @T, D)`: a relation of that shape.
    Rel(Vec<ArgDecl>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputDecl {
    pub name: String,
    pub ty: InputTy,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArgDecl {
    pub name: String,
    /// `None` for the temporal key `@T`.
    pub ty: Option<TyName>,
    pub key: bool,
    /// `@T: weekly`, `@X: base | session`.
    pub cals: Vec<(String, Span)>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observe {
    pub rel: String,
    pub wheres: Vec<(String, Lit, Span)>,
    pub trace: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecisionsBlock {
    pub span: Span,
    pub silence: Option<(String, Span)>,
    pub orders: Option<(String, Span)>,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionBlock {
    /// `execution NAME [with { FIELD VALUE ... }]`.
    Preset {
        name: String,
        span: Span,
        with: Vec<ExecField>,
    },
    Fields {
        span: Span,
        fields: Vec<ExecField>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecField {
    pub name: String,
    pub value: ExecValue,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecValue {
    /// `price close`, `price tr_index@weekly`.
    Price {
        rel: String,
        cal: Option<String>,
    },
    /// `orders moc`, `lot whole`, `actions apply`, `oversize halt`, `ruin halt`, `numerics two_pass`, `delisting last_price`.
    Word(String),
    /// `capital 1_000_000 USD fixed|compounding`.
    Capital {
        amount: f64,
        compounding: bool,
    },
    CommissionPerShare {
        per_share: f64,
        min: f64,
    },
    CommissionPerContract,
    /// `per_contract P USD`: a fixed commission per contract per side (an
    /// option's, and its exercise fee, K-5.5.4).
    CommissionPerContractFixed(f64),
    CommissionBps(f64),
    SlippageFixed(f64),
    SlippageVol {
        mult: f64,
        bars: u32,
    },
    Impact {
        coef: f64,
        days: u32,
    },
    Participation(f64),
    Financing {
        cash: f64,
        debit: f64,
        short: f64,
    },
    LeverageGross {
        max: f64,
        policy: String,
    },
    Haircuts(Vec<(String, f64)>),
    /// `limits { gross <= 1.5 reject; weight <= 0.1 per name clamp; ... }`
    /// (K-3.5).
    Limits(Vec<LimitItem>),
    /// `option_fill touch [+ F spread] | mid [+ F spread] | last` (K-5.5.3).
    OptionFill {
        base: String,
        spread: f64,
    },
    /// `margin published, reg_t, approx 0.2` (K-5.6): the models in order of
    /// preference, `approx` with its fraction.
    Margin(Vec<(String, Option<f64>)>),
    /// `exercise { itm_at_expiry; early_short ...; early_long ...;
    /// atm_tolerance X }` (K-5.5.4).
    Exercise(ExerciseRule),
    None,
}

/// When a short American option is assigned before its expiry (K-5.5.4,
/// ERRATA E-6).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EarlyShort {
    /// `when extrinsic < DIVIDEND`: a call on the bar before an ex-date when
    /// its extrinsic value is below the dividend; a put when its extrinsic
    /// value is below the financing of its strike to expiry.
    Dividend,
    /// `when extrinsic < X`: either right, against a fixed threshold.
    Threshold(f64),
    Never,
}

/// The `exercise` field (K-5.5.4).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExerciseRule {
    /// In-the-money options are exercised (long) or assigned (short) at
    /// expiry; otherwise every option expires.
    pub itm_at_expiry: bool,
    pub early_short: EarlyShort,
    /// `early_long decide`: the program's `exercise(A, Q)` is executed;
    /// `never`: it is dropped.
    pub early_long_decide: bool,
    /// A fraction of the strike within which an option is at the money and
    /// expires.
    pub atm_tolerance: f64,
}

/// One pre-trade limit as written: `KIND <= BOUND [per WORD] [POLICY]`.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitItem {
    /// `gross`, `net`, `weight`, `loss`, `drawdown`, `turnover`,
    /// `participation` or `names`.
    pub kind: String,
    pub bound: f64,
    /// `name` (weight), `day` (loss), the period of a turnover.
    pub per: Option<String>,
    /// `halt`, `reject`, `clamp` or `warn`; absent, `reject`.
    pub policy: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportBlock {
    pub span: Span,
    pub calendar: Option<(String, Span)>,
    pub periods: Option<(f64, Span)>,
    pub window: Option<(i64, i64, Span)>,
}

/// A surface atom: `name[args]@cal(terms)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Atom {
    pub name: String,
    pub cal: Option<(String, Span)>,
    pub brackets: Vec<BracketArg>,
    pub terms: Vec<Term>,
    pub span: Span,
}

/// A bracketed argument at a call: a parameter, a literal, or a relation
/// (a name, itself possibly bracketed); a variable names an unbound input (M).
#[derive(Clone, Debug, PartialEq)]
pub enum BracketArg {
    Lit(Lit, Span),
    Name(String, Span),
    Nested(Box<Atom>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum WindowSpec {
    Rows { n: Expr, min: Option<Expr> },
    PriorRows { n: Expr, min: Option<Expr> },
    Window { dur: Expr, min: Option<Expr> },
    PriorWindow { dur: Expr, min: Option<Expr> },
    Bucket { cal: (String, Span), base: Term },
    Session { base: Term, lo: Expr, hi: Expr },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub guard: Vec<Literal>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Atom(Atom),
    Neg(Atom),
    Prev {
        t: Term,
        t1: Term,
        span: Span,
    },
    Lag {
        t: Term,
        n: Expr,
        t1: Term,
        span: Span,
    },
    MonthStart {
        t: Term,
        span: Span,
    },
    DayStart {
        t: Term,
        span: Span,
    },
    SessionOf {
        a: Term,
        t: Term,
        s: Term,
        span: Span,
    },
    Window {
        var: Term,
        spec: WindowSpec,
        span: Span,
    },
    Cmp {
        op: CmpOp,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    },
    Assign {
        var: String,
        expr: Expr,
        span: Span,
    },
    Agg {
        var: String,
        agg: String,
        args: Vec<Expr>,
        conj: Vec<Literal>,
        within: Option<Vec<(String, Span)>>,
        span: Span,
    },
    Top {
        n: Option<Expr>,
        atom: Atom,
        by: Option<Vec<(String, Dir, Span)>>,
        rank: Option<(String, Ties, Span)>,
        within: Option<Vec<(String, Span)>>,
        span: Span,
    },
    AsOf {
        atom: Atom,
        at: Term,
        span: Span,
    },
    Case {
        var: String,
        branches: Vec<Branch>,
        otherwise: Option<Expr>,
        span: Span,
    },
}

impl Literal {
    pub fn span(&self) -> Span {
        match self {
            Literal::Atom(a) | Literal::Neg(a) => a.span,
            Literal::Prev { span, .. }
            | Literal::Lag { span, .. }
            | Literal::MonthStart { span, .. }
            | Literal::DayStart { span, .. }
            | Literal::SessionOf { span, .. }
            | Literal::Window { span, .. }
            | Literal::Cmp { span, .. }
            | Literal::Assign { span, .. }
            | Literal::Agg { span, .. }
            | Literal::Top { span, .. }
            | Literal::AsOf { span, .. }
            | Literal::Case { span, .. } => *span,
        }
    }
}

/// `head[inputs](terms) :- body.`
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub head: Atom,
    pub body: Vec<Literal>,
    pub span: Span,
}
