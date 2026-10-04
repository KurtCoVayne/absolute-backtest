//! The typed intermediate representation: domains, dimensional types,
//! relation signatures, rules and the seven literal forms (spec sections 2 to 4).

use std::fmt;

/// Source position of a construct, for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// The fixed, totally ordered set of bar resolutions (section 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Resolution {
    M1,
    M5,
    M15,
    M30,
    H1,
    D1,
}

impl Resolution {
    pub fn parse(s: &str) -> Option<Resolution> {
        Some(match s {
            "1m" => Resolution::M1,
            "5m" => Resolution::M5,
            "15m" => Resolution::M15,
            "30m" => Resolution::M30,
            "1h" => Resolution::H1,
            "1d" => Resolution::D1,
            _ => return None,
        })
    }
    /// `self` is strictly finer than `other` (every `other` bar is a union of whole `self` bars).
    pub fn finer_than(self, other: Resolution) -> bool {
        self < other
    }
    /// Length of one bar in seconds; `None` for the daily resolution, whose
    /// bucket is the civil date rather than a fixed number of seconds.
    pub fn seconds(self) -> Option<i64> {
        Some(match self {
            Resolution::M1 => 60,
            Resolution::M5 => 300,
            Resolution::M15 => 900,
            Resolution::M30 => 1800,
            Resolution::H1 => 3600,
            Resolution::D1 => return None,
        })
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Resolution::M1 => "@1m",
            Resolution::M5 => "@5m",
            Resolution::M15 => "@15m",
            Resolution::M30 => "@30m",
            Resolution::H1 => "@1h",
            Resolution::D1 => "@1d",
        };
        f.write_str(s)
    }
}

/// A dimension vector over currency, shares and time. Exponents are stored
/// doubled so that half-integer exponents (from `sqrt`) are exact integers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Dim {
    pub c2: i32,
    pub s2: i32,
    pub t2: i32,
    /// Currency code when the currency exponent is non-zero.
    pub currency: Option<String>,
}

impl Dim {
    pub const fn scalar() -> Dim {
        Dim { c2: 0, s2: 0, t2: 0, currency: None }
    }
    pub fn price(code: &str) -> Dim {
        Dim {
            c2: 2,
            s2: -2,
            t2: 0,
            currency: Some(code.to_string()),
        }
    }
    pub fn notional(code: &str) -> Dim {
        Dim {
            c2: 2,
            s2: 0,
            t2: 0,
            currency: Some(code.to_string()),
        }
    }
    pub const fn shares() -> Dim {
        Dim { c2: 0, s2: 2, t2: 0, currency: None }
    }
    pub fn is_scalar(&self) -> bool {
        self.c2 == 0 && self.s2 == 0 && self.t2 == 0
    }
    fn normalise(mut self) -> Dim {
        if self.c2 == 0 {
            self.currency = None;
        }
        self
    }
    /// Dimension of a product; `None` when the currency codes disagree.
    pub fn mul(&self, o: &Dim) -> Option<Dim> {
        let currency = match (&self.currency, &o.currency) {
            (Some(a), Some(b)) if a != b => return None,
            (Some(a), _) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        Some(
            Dim {
                c2: self.c2 + o.c2,
                s2: self.s2 + o.s2,
                t2: self.t2 + o.t2,
                currency,
            }
            .normalise(),
        )
    }
    /// Dimension of a quotient; `None` when the currency codes disagree.
    pub fn div(&self, o: &Dim) -> Option<Dim> {
        let currency = match (&self.currency, &o.currency) {
            (Some(a), Some(b)) if a != b => return None,
            (Some(a), _) => Some(a.clone()),
            (None, b) => b.clone(),
        };
        Some(
            Dim {
                c2: self.c2 - o.c2,
                s2: self.s2 - o.s2,
                t2: self.t2 - o.t2,
                currency,
            }
            .normalise(),
        )
    }
    /// Dimension of a square root; `None` when an exponent would leave ½ℤ.
    pub fn sqrt(&self) -> Option<Dim> {
        if self.c2 % 2 != 0 || self.s2 % 2 != 0 || self.t2 % 2 != 0 {
            return None;
        }
        Some(
            Dim {
                c2: self.c2 / 2,
                s2: self.s2 / 2,
                t2: self.t2 / 2,
                currency: self.currency.clone(),
            }
            .normalise(),
        )
    }
}

impl fmt::Display for Dim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cur = self.currency.as_deref().unwrap_or("?");
        match (self.c2, self.s2, self.t2) {
            (0, 0, 0) => write!(f, "Scalar"),
            (2, -2, 0) => write!(f, "Price<{}>", cur),
            (0, 2, 0) => write!(f, "Quantity<Shares>"),
            (2, 0, 0) => write!(f, "Notional<{}>", cur),
            (0, 0, 2) => write!(f, "Duration"),
            _ => {
                let h = |x: i32| -> String {
                    if x % 2 == 0 {
                        format!("{}", x / 2)
                    } else {
                        format!("{}/2", x)
                    }
                };
                write!(
                    f,
                    "Dim(C^{} S^{} Θ^{}{})",
                    h(self.c2),
                    h(self.s2),
                    h(self.t2),
                    if self.c2 != 0 { format!(", {}", cur) } else { String::new() }
                )
            }
        }
    }
}

/// A value type (section 2). `Quantity` carries a dimension vector; `IntLit`
/// is the checker's type for a bare integer literal, which coerces to `Count`
/// or `Scalar` from context and never survives into a signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Equity,
    Timestamp,
    Duration,
    Count,
    Quantity(Dim),
    Decision,
    IntLit,
}

impl Ty {
    pub fn scalar() -> Ty {
        Ty::Quantity(Dim::scalar())
    }
    pub fn is_entity(&self) -> bool {
        matches!(self, Ty::Equity)
    }
    /// Parse a surface type name such as `Price<USD>`.
    pub fn parse(name: &str, arg: Option<&str>) -> Option<Ty> {
        Some(match (name, arg) {
            ("Equity", None) => Ty::Equity,
            ("Timestamp", None) => Ty::Timestamp,
            ("Duration", None) => Ty::Duration,
            ("Count", None) => Ty::Count,
            ("Scalar", None) => Ty::scalar(),
            ("Decision", None) => Ty::Decision,
            ("Price", Some(code)) => Ty::Quantity(Dim::price(code)),
            ("Notional", Some(code)) => Ty::Quantity(Dim::notional(code)),
            ("Quantity", Some("Shares")) => Ty::Quantity(Dim::shares()),
            _ => return None,
        })
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Equity => write!(f, "Equity"),
            Ty::Timestamp => write!(f, "Timestamp"),
            Ty::Duration => write!(f, "Duration"),
            Ty::Count => write!(f, "Count"),
            Ty::Quantity(d) => write!(f, "{}", d),
            Ty::Decision => write!(f, "Decision"),
            Ty::IntLit => write!(f, "integer literal"),
        }
    }
}

/// Argument mode (section 3). The temporal key behaves as an output for
/// binding purposes and is the argument causality is judged on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    In,
    Out,
    Key,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: String,
    pub mode: Mode,
    pub ty: Ty,
}

/// Who defines a relation (section 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Supplied by the named environment.
    Primitive { env: String },
    /// `position`, `cash`, `fill`: supplied by the executor at the decision resolution.
    Executor,
    /// `decided`: supplied by the kernel from the strategy's own output.
    KernelState,
    /// Defined by rules in the named unit.
    Derived { unit: String },
    /// `decide`.
    Output,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub name: String,
    pub args: Vec<Arg>,
    /// Native resolution; `None` in a declaration means "the unit's resolution".
    pub res: Option<Resolution>,
    /// Declared complete (meaningful on primitives only).
    pub complete: bool,
    pub kind: Kind,
    pub span: Span,
}

impl Signature {
    pub fn key_pos(&self) -> Option<usize> {
        self.args.iter().position(|a| a.mode == Mode::Key)
    }
    /// Identity columns: entity-typed arguments plus the temporal key
    /// (section 3; value outputs never identify). An entity-typed output is
    /// counted too, so that a reduction over an enumerable relation still
    /// has to name it as a tie-break.
    pub fn identity_positions(&self) -> Vec<usize> {
        self.args.iter().enumerate().filter(|(_, a)| a.mode == Mode::Key || a.ty.is_entity()).map(|(i, _)| i).collect()
    }
}

/// A calendar duration (section 2): days (weeks are 7 days) and months (years are 12 months).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Duration {
    pub months: i64,
    pub days: i64,
}

impl Duration {
    pub fn is_zero(&self) -> bool {
        self.months == 0 && self.days == 0
    }
    /// Approximate length in days, for ordering durations in the kernel.
    pub fn approx_days(&self) -> f64 {
        self.months as f64 * 30.4375 + self.days as f64
    }
    /// The shortest calendar length in days this duration can take: a month
    /// spans 28 to 31 days and twelve consecutive months 365 or 366, so a
    /// parameter's range (section 3) rejects a default only when it falls
    /// outside under every length.
    pub fn min_days(&self) -> i64 {
        (self.months / 12) * 365 + (self.months % 12) * 28 + self.days
    }
    /// The longest calendar length in days this duration can take.
    pub fn max_days(&self) -> i64 {
        (self.months / 12) * 366 + (self.months % 12) * 31 + self.days
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.months, self.days) {
            (0, d) => write!(f, "{}d", d),
            (m, 0) if m % 12 == 0 => write!(f, "{}y", m / 12),
            (m, 0) => write!(f, "{}mo", m),
            (m, d) => write!(f, "{}mo+{}d", m, d),
        }
    }
}

/// A literal value with its unit.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Shares(f64),
    Money(f64, String),
    Duration(Duration),
    Equity(String),
}

impl Lit {
    pub fn ty(&self) -> Ty {
        match self {
            Lit::Int(_) => Ty::IntLit,
            Lit::Float(_) => Ty::scalar(),
            Lit::Shares(_) => Ty::Quantity(Dim::shares()),
            Lit::Money(_, c) => Ty::Quantity(Dim::notional(c)),
            Lit::Duration(_) => Ty::Duration,
            Lit::Equity(_) => Ty::Equity,
        }
    }
}

impl fmt::Display for Lit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lit::Int(i) => write!(f, "{}", i),
            Lit::Float(x) => write!(f, "{}", x),
            Lit::Shares(x) => write!(f, "{} shares", x),
            Lit::Money(x, c) => write!(f, "{} {}", x, c),
            Lit::Duration(d) => write!(f, "{}", d),
            Lit::Equity(s) => write!(f, "\"{}\"", s),
        }
    }
}

/// A term in an atom or head.
#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Var(String, Span),
    Wild(Span),
    Param(String, Span),
    Lit(Lit, Span),
    /// A decision constructor such as `buy(A, Q)`.
    Ctor(String, Vec<Term>, Span),
}

impl Term {
    pub fn span(&self) -> Span {
        match self {
            Term::Var(_, s) | Term::Wild(s) | Term::Param(_, s) | Term::Lit(_, s) | Term::Ctor(_, _, s) => *s,
        }
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Term::Var(v, _) => write!(f, "{}", v),
            Term::Wild(_) => write!(f, "_"),
            Term::Param(p, _) => write!(f, "{}", p),
            Term::Lit(l, _) => write!(f, "{}", l),
            Term::Ctor(c, args, _) => {
                write!(f, "{}(", c)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
        })
    }
}

/// A scalar-function expression (section 2).
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Var(String, Span),
    Param(String, Span),
    Lit(Lit, Span),
    Neg(Box<Expr>, Span),
    Bin(BinOp, Box<Expr>, Box<Expr>, Span),
    Call(String, Vec<Expr>, Span),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Var(_, s) | Expr::Param(_, s) | Expr::Lit(_, s) | Expr::Neg(_, s) | Expr::Bin(_, _, _, s) | Expr::Call(_, _, s) => *s,
        }
    }
    pub fn vars(&self, out: &mut Vec<(String, Span)>) {
        match self {
            Expr::Var(v, s) => out.push((v.clone(), *s)),
            Expr::Param(..) | Expr::Lit(..) => {}
            Expr::Neg(e, _) => e.vars(out),
            Expr::Bin(_, a, b, _) => {
                a.vars(out);
                b.vars(out);
            }
            Expr::Call(_, args, _) => args.iter().for_each(|a| a.vars(out)),
        }
    }
    pub fn params(&self, out: &mut Vec<String>) {
        match self {
            Expr::Param(p, _) => out.push(p.clone()),
            Expr::Var(..) | Expr::Lit(..) => {}
            Expr::Neg(e, _) => e.params(out),
            Expr::Bin(_, a, b, _) => {
                a.params(out);
                b.params(out);
            }
            Expr::Call(_, args, _) => args.iter().for_each(|a| a.params(out)),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Var(v, _) => write!(f, "{}", v),
            Expr::Param(p, _) => write!(f, "{}", p),
            Expr::Lit(l, _) => write!(f, "{}", l),
            Expr::Neg(e, _) => write!(f, "-{}", e),
            Expr::Bin(op, a, b, _) => write!(f, "({} {} {})", a, op, b),
            Expr::Call(name, args, _) => {
                write!(f, "{}(", name)?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", a)?;
                }
                write!(f, ")")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Le,
    Eq,
    Gt,
    Ge,
}

impl fmt::Display for CmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Eq => "=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        })
    }
}

/// An atom: a relation name applied to terms.
#[derive(Clone, Debug, PartialEq)]
pub struct Atom {
    pub name: String,
    pub terms: Vec<Term>,
    pub span: Span,
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.name)?;
        for (i, t) in self.terms.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", t)?;
        }
        write!(f, ")")
    }
}

/// The temporal builtins (section 4).
#[derive(Clone, Debug, PartialEq)]
pub enum Builtin {
    /// `prev(T, T1)`
    Prev { t: Term, t1: Term },
    /// `lag(T, N, T1)`
    Lag { t: Term, n: Expr, t1: Term },
    /// `month_start(T)`
    MonthStart { t: Term },
    /// `day_start(T)`
    DayStart { t: Term },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Window,
    Prior,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// The seven literal forms of section 4, plus the temporal builtins and the
/// window literal that only occurs inside an aggregation.
#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Atom(Atom),
    Neg(Atom),
    Builtin(Builtin, Span),
    /// `T1 in window(T, N, min K)` / `T1 in prior_window(T, N, min K)`
    Window {
        var: Term,
        kind: WindowKind,
        base: Term,
        dur: Expr,
        min: Expr,
        span: Span,
    },
    Cmp {
        op: CmpOp,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    },
    /// `X = e`: an assignment when X is unbound, a comparison otherwise.
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
        span: Span,
    },
    Top {
        n: Expr,
        atom: Atom,
        by: Option<Vec<(String, Dir, Span)>>,
        span: Span,
    },
    Resample {
        inner: Atom,
        to: Resolution,
        as_var: String,
        min: Expr,
        aggs: Vec<(String, String, Expr)>,
        span: Span,
    },
}

impl Literal {
    pub fn span(&self) -> Span {
        match self {
            Literal::Atom(a) | Literal::Neg(a) => a.span,
            Literal::Builtin(_, s) => *s,
            Literal::Window { span, .. } | Literal::Cmp { span, .. } | Literal::Assign { span, .. } | Literal::Agg { span, .. } | Literal::Top { span, .. } | Literal::Resample { span, .. } => *span,
        }
    }
    pub fn describe(&self) -> String {
        match self {
            Literal::Atom(a) => format!("{}", a),
            Literal::Neg(a) => format!("not {}", a),
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => format!("prev({}, {})", t, t1),
                Builtin::Lag { t, n, t1 } => format!("lag({}, {}, {})", t, n, t1),
                Builtin::MonthStart { t } => format!("month_start({})", t),
                Builtin::DayStart { t } => format!("day_start({})", t),
            },
            Literal::Window { var, kind, base, dur, min, .. } => format!("{} in {}({}, {}, min {})", var, if *kind == WindowKind::Window { "window" } else { "prior_window" }, base, dur, min),
            Literal::Cmp { op, lhs, rhs, .. } => format!("{} {} {}", lhs, op, rhs),
            Literal::Assign { var, expr, .. } => format!("{} = {}", var, expr),
            Literal::Agg { var, agg, args, .. } => {
                let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                format!("{} = {}({}) over (...)", var, agg, a.join(", "))
            }
            Literal::Top { n, atom, .. } => format!("top({}, {}, ...)", n, atom),
            Literal::Resample { inner, to, as_var, .. } => format!("resample({} to {} as {}, ...)", inner, to, as_var),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub head: Atom,
    pub body: Vec<Literal>,
    pub span: Span,
    /// Name of the unit (library or strategy) the rule is written in.
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    pub value: Lit,
    pub range: Option<(Lit, Lit)>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DecisionMode {
    Delta,
    Target,
}

impl DecisionMode {
    pub fn ctors(self) -> &'static [&'static str] {
        match self {
            DecisionMode::Delta => &["buy", "sell", "short", "cover"],
            DecisionMode::Target => &["target_weight", "target_quantity"],
        }
    }
}

impl fmt::Display for DecisionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecisionMode::Delta => "delta",
            DecisionMode::Target => "target",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    Environment,
    Library,
    Strategy,
}

impl fmt::Display for UnitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UnitKind::Environment => "environment",
            UnitKind::Library => "library",
            UnitKind::Strategy => "strategy",
        })
    }
}

/// A parsed environment, library or strategy.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub kind: UnitKind,
    pub name: String,
    pub span: Span,
    pub env: Option<(String, Span)>,
    pub uses: Vec<(String, Span)>,
    pub resolution: Option<(Resolution, Span)>,
    pub mode: Option<(DecisionMode, Span)>,
    /// Header lines written more than once (`env`, `resolution` or `mode`):
    /// the keyword and the span of each later line. The first stays in
    /// effect; the checker reports the others.
    pub redeclared: Vec<(String, Span)>,
    pub params: Vec<Param>,
    /// Primitive signatures (environment) or derived relation declarations (library, strategy).
    pub rels: Vec<Signature>,
    pub rules: Vec<Rule>,
}

/// Decision constructor field types (section 4).
pub fn ctor_fields(name: &str) -> Option<Vec<Ty>> {
    Some(match name {
        "buy" | "sell" | "short" | "cover" | "target_quantity" => vec![Ty::Equity, Ty::Quantity(Dim::shares())],
        "target_weight" => vec![Ty::Equity, Ty::scalar()],
        _ => return None,
    })
}

pub fn ctor_mode(name: &str) -> Option<DecisionMode> {
    match name {
        "buy" | "sell" | "short" | "cover" => Some(DecisionMode::Delta),
        "target_weight" | "target_quantity" => Some(DecisionMode::Target),
        _ => None,
    }
}

/// The closed set of aggregates (section 1).
pub const AGGREGATES: &[&str] = &["sum", "mean", "std", "median", "quantile", "max", "min", "count", "corr", "cov", "ols_beta", "first", "last"];
/// Aggregates permitted inside a resample form (section 4).
pub const RESAMPLE_AGGREGATES: &[&str] = &["first", "last", "max", "min", "sum", "mean", "count"];
/// Scalar functions (section 2).
pub const SCALAR_FUNCTIONS: &[&str] = &["log", "exp", "sqrt", "abs", "least", "greatest"];

/// Names a relation may not be declared with: the temporal builtins, the
/// literal-form and unit keywords, the kernel's output and state relations
/// and the decision constructors (section 4). The aggregates and scalar
/// functions are reserved too, so that a body atom is never mistaken for one.
pub const RESERVED_NAMES: &[&str] = &[
    "prev",
    "lag",
    "month_start",
    "day_start",
    "window",
    "prior_window",
    "top",
    "resample",
    "decide",
    "decided",
    "not",
    "in",
    "min",
    "by",
    "over",
    "as",
    "to",
    "asc",
    "desc",
    "environment",
    "library",
    "strategy",
    "env",
    "uses",
    "resolution",
    "mode",
    "param",
    "rel",
    "complete",
    "delta",
    "target",
    "buy",
    "sell",
    "short",
    "cover",
    "target_weight",
    "target_quantity",
    "sum",
    "mean",
    "std",
    "median",
    "quantile",
    "max",
    "count",
    "corr",
    "cov",
    "ols_beta",
    "first",
    "last",
    "log",
    "exp",
    "sqrt",
    "abs",
    "least",
    "greatest",
];

pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
}

/// The executor and kernel-state relations every strategy sees at its
/// decision resolution (section 6).
pub fn kernel_relations(res: Resolution) -> Vec<Signature> {
    let arg = |name: &str, mode: Mode, ty: Ty| Arg { name: name.to_string(), mode, ty };
    vec![
        Signature {
            name: "position".into(),
            args: vec![arg("A", Mode::Out, Ty::Equity), arg("T", Mode::Key, Ty::Timestamp), arg("Q", Mode::Out, Ty::Quantity(Dim::shares()))],
            res: Some(res),
            complete: true,
            kind: Kind::Executor,
            span: Span::default(),
        },
        Signature {
            name: "cash".into(),
            args: vec![arg("T", Mode::Key, Ty::Timestamp), arg("C", Mode::Out, Ty::Quantity(Dim::notional("USD")))],
            res: Some(res),
            complete: true,
            kind: Kind::Executor,
            span: Span::default(),
        },
        Signature {
            name: "fill".into(),
            args: vec![
                arg("A", Mode::Out, Ty::Equity),
                arg("T", Mode::Key, Ty::Timestamp),
                arg("Q", Mode::Out, Ty::Quantity(Dim::shares())),
                arg("P", Mode::Out, Ty::Quantity(Dim::price("USD"))),
            ],
            res: Some(res),
            complete: true,
            kind: Kind::Executor,
            span: Span::default(),
        },
        Signature {
            name: "decided".into(),
            args: vec![arg("T0", Mode::Key, Ty::Timestamp), arg("D", Mode::Out, Ty::Decision)],
            res: Some(res),
            complete: true,
            kind: Kind::KernelState,
            span: Span::default(),
        },
        Signature {
            name: "decide".into(),
            args: vec![arg("T", Mode::Key, Ty::Timestamp), arg("D", Mode::Out, Ty::Decision)],
            res: Some(res),
            complete: true,
            kind: Kind::Output,
            span: Span::default(),
        },
    ]
}
