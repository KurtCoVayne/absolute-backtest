//! The core intermediate representation of a linked program: domains,
//! dimensional types, calendars, relation signatures, rules and the literal
//! forms of `docs/language-v2.md` (sections 3 to 5). The front end
//! (`syntax`) parses the blocks of a program into an AST; the elaborator
//! (`check`) links the libraries, instantiates their templates and produces
//! a program over these types, which the kernel, the compiler stages and the
//! query facility read.

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

/// A base resolution (section 3.1): the finest bar length a bundle holds for
/// a class, or the native resolution of a series. Resolutions are totally
/// ordered from the finest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    M1,
    M5,
    M15,
    M30,
    H1,
    D1,
    W1,
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
            "1w" => Resolution::W1,
            _ => return None,
        })
    }
    /// `self` is strictly finer than `other`.
    pub fn finer_than(self, other: Resolution) -> bool {
        self < other
    }
    /// Length of one bar in nanoseconds; `None` for the daily and weekly
    /// resolutions, whose buckets are civil periods.
    pub fn nanos(self) -> Option<i64> {
        use crate::kernel::time::{HOUR, MIN};
        Some(match self {
            Resolution::M1 => MIN,
            Resolution::M5 => 5 * MIN,
            Resolution::M15 => 15 * MIN,
            Resolution::M30 => 30 * MIN,
            Resolution::H1 => HOUR,
            Resolution::D1 | Resolution::W1 => return None,
        })
    }
    pub fn is_sub_daily(self) -> bool {
        self.nanos().is_some()
    }
    /// The calendar a resolution corresponds to over a finer base.
    pub fn as_calendar(self) -> Cal {
        match self {
            Resolution::D1 => Cal::Days,
            Resolution::W1 => Cal::Weeks(1),
            _ => Cal::Base,
        }
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
            Resolution::W1 => "@1w",
        };
        f.write_str(s)
    }
}

/// A calendar over the class's base domain (Definition 3.2): the base
/// singletons, civil periods, or the instrument's sessions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Cal {
    Base,
    Days,
    Weeks(u32),
    Months(u32),
    Years(u32),
    /// Each instrument's own sessions (a class property).
    Sessions,
    /// The sessions of one root, a calendar every rule may share.
    SessionsOf(String),
}

impl Cal {
    /// A civil-period calendar's rank in the refinement chain base < days <
    /// weeks < months < years; sessions are comparable with base only.
    fn rank(&self) -> Option<u32> {
        Some(match self {
            Cal::Base => 0,
            Cal::Days => 1,
            Cal::Weeks(_) => 2,
            Cal::Months(_) => 3,
            Cal::Years(_) => 4,
            Cal::Sessions | Cal::SessionsOf(_) => return None,
        })
    }
    /// `self` ≼ `other`: every class of `other` is a union of classes of
    /// `self` (Proposition 3.3). Two civil calendars of one period kind are
    /// comparable when one's count divides the other's.
    pub fn refines(&self, other: &Cal) -> bool {
        if self == other || *self == Cal::Base {
            return true;
        }
        match (self, other) {
            (Cal::Weeks(a), Cal::Weeks(b)) | (Cal::Months(a), Cal::Months(b)) | (Cal::Years(a), Cal::Years(b)) => b % a == 0,
            (Cal::Months(a), Cal::Years(b)) => (12 * b) % a == 0,
            (Cal::Sessions | Cal::SessionsOf(_), _) | (_, Cal::Sessions | Cal::SessionsOf(_)) => false,
            (a, b) => match (a.rank(), b.rank()) {
                (Some(x), Some(y)) => x < y && !matches!((a, b), (Cal::Weeks(_), Cal::Months(_)) | (Cal::Weeks(_), Cal::Years(_))),
                _ => false,
            },
        }
    }
    pub fn is_sessions(&self) -> bool {
        matches!(self, Cal::Sessions | Cal::SessionsOf(_))
    }
    /// Per-instrument: its domain depends on the entity.
    pub fn per_entity(&self) -> bool {
        matches!(self, Cal::Sessions)
    }
}

impl fmt::Display for Cal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cal::Base => write!(f, "base"),
            Cal::Days => write!(f, "days"),
            Cal::Weeks(n) => write!(f, "weeks({})", n),
            Cal::Months(n) => write!(f, "months({})", n),
            Cal::Years(n) => write!(f, "years({})", n),
            Cal::Sessions => write!(f, "sessions"),
            Cal::SessionsOf(r) => write!(f, "sessions of \"{}\"", r),
        }
    }
}

/// A dimension vector over currency, the class's unit and time. Exponents
/// are stored doubled so that half-integer exponents (from `sqrt`) are
/// exact integers.
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
    /// The class's unit: a share, a contract.
    pub const fn shares() -> Dim {
        Dim { c2: 0, s2: 2, t2: 0, currency: None }
    }
    pub fn is_scalar(&self) -> bool {
        self.c2 == 0 && self.s2 == 0 && self.t2 == 0
    }
    pub fn is_price(&self) -> bool {
        self.c2 == 2 && self.s2 == -2 && self.t2 == 0
    }
    pub fn is_notional(&self) -> bool {
        self.c2 == 2 && self.s2 == 0 && self.t2 == 0
    }
    pub fn is_quantity(&self) -> bool {
        self.c2 == 0 && self.s2 == 2 && self.t2 == 0
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
            (0, 2, 0) => write!(f, "Quantity<Unit>"),
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
                    "Dim(C^{} U^{} Θ^{}{})",
                    h(self.c2),
                    h(self.s2),
                    h(self.t2),
                    if self.c2 != 0 { format!(", {}", cur) } else { String::new() }
                )
            }
        }
    }
}

/// A value type (section 2 of the semantic model, with the second
/// formulation's entity sort). `IntLit` is the checker's type for a bare
/// integer literal, which coerces to `Count` or `Scalar` from context and
/// never survives into a signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// The class's entity sort (`Equity`, `Future`, or `Entity` in a library).
    Entity,
    Timestamp,
    Duration,
    Count,
    Quantity(Dim),
    Decision,
    IntLit,
    /// A name from a closed vocabulary the bundle defines (a list, a
    /// delisting reason, a classification code, a series name): interned,
    /// compared with `=` only.
    Label,
    /// A string literal before the checker resolves it: an entity or a
    /// label from context.
    StrLit,
    /// A dimension variable of a library signature (`D`), resolved at
    /// instantiation.
    DimVar(String),
}

impl Ty {
    pub fn scalar() -> Ty {
        Ty::Quantity(Dim::scalar())
    }
    pub fn is_entity(&self) -> bool {
        matches!(self, Ty::Entity)
    }
    /// Parse a surface type name such as `Price<USD>`; `entity` names the
    /// class's entity sort.
    pub fn parse(name: &str, arg: Option<&str>) -> Option<Ty> {
        Some(match (name, arg) {
            ("Equity" | "Future" | "Option" | "Entity", None) => Ty::Entity,
            ("Timestamp", None) => Ty::Timestamp,
            ("Duration", None) => Ty::Duration,
            ("Count", None) => Ty::Count,
            ("Scalar", None) => Ty::scalar(),
            ("Decision", None) => Ty::Decision,
            ("Label", None) => Ty::Label,
            ("Price", Some(code)) => Ty::Quantity(Dim::price(code)),
            ("Notional", Some(code)) => Ty::Quantity(Dim::notional(code)),
            ("Quantity", Some(_)) => Ty::Quantity(Dim::shares()),
            _ => return None,
        })
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Entity => write!(f, "Entity"),
            Ty::Timestamp => write!(f, "Timestamp"),
            Ty::Duration => write!(f, "Duration"),
            Ty::Count => write!(f, "Count"),
            Ty::Quantity(d) => write!(f, "{}", d),
            Ty::Decision => write!(f, "Decision"),
            Ty::IntLit => write!(f, "integer literal"),
            Ty::Label => write!(f, "Label"),
            Ty::StrLit => write!(f, "string literal"),
            Ty::DimVar(d) => write!(f, "{}", d),
        }
    }
}

/// The role of an argument in a signature (Definition 4.1): the temporal
/// key, or an identity or value argument. There are no binding modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
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
    /// Supplied by the bundle for the program's class.
    Primitive { class: String },
    /// `position`, `cash`, `nav`, `capital`, `mark`, `fill`: supplied by the
    /// executor at the decision calendar.
    Executor,
    /// `decided` and `clock`: supplied by the kernel.
    KernelState,
    /// Defined by rules in the named unit (a strategy, a library instance, a
    /// bar view or the universe block).
    Derived { unit: String },
    /// `decide`.
    Output,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub name: String,
    pub args: Vec<Arg>,
    /// The calendar the relation is keyed on.
    pub cal: Cal,
    /// Declared complete (primitives: a reviewed store or a catalog
    /// relation; derived: closed by construction, the universe).
    pub complete: bool,
    pub kind: Kind,
    pub span: Span,
    /// The native resolution of a primitive that is read as of the bar when
    /// the rule's calendar is finer (a series, a daily catalog relation in a
    /// sub-daily program); `None` for a relation read at its own calendar.
    pub native: Option<Resolution>,
}

impl Signature {
    pub fn key_pos(&self) -> Option<usize> {
        self.args.iter().position(|a| a.mode == Mode::Key)
    }
    /// Identity columns: entity-typed arguments plus the temporal key
    /// (Definition 4.1); a label argument before the key identifies too (a
    /// series' name).
    pub fn identity_positions(&self) -> Vec<usize> {
        let key = self.key_pos().unwrap_or(usize::MAX);
        self.args
            .iter()
            .enumerate()
            .filter(|(i, a)| a.mode == Mode::Key || a.ty.is_entity() || (a.ty == Ty::Label && *i < key))
            .map(|(i, _)| i)
            .collect()
    }
    pub fn is_primitive(&self) -> bool {
        matches!(self.kind, Kind::Primitive { .. })
    }
}

/// A calendar duration: months (years are 12 months), days (weeks are 7
/// days) and a clock part in nanoseconds (`30min`, `2h`, `5s`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Duration {
    pub months: i64,
    pub days: i64,
    #[serde(default)]
    pub nanos: i64,
}

impl Duration {
    pub const fn days(d: i64) -> Duration {
        Duration { months: 0, days: d, nanos: 0 }
    }
    pub const fn months(m: i64) -> Duration {
        Duration { months: m, days: 0, nanos: 0 }
    }
    /// A clock duration of `s` whole seconds.
    pub const fn secs(s: i64) -> Duration {
        Duration {
            months: 0,
            days: 0,
            nanos: s * crate::kernel::time::SEC,
        }
    }
    pub const fn nanos(n: i64) -> Duration {
        Duration { months: 0, days: 0, nanos: n }
    }
    pub fn is_zero(&self) -> bool {
        self.months == 0 && self.days == 0 && self.nanos == 0
    }
    /// Approximate length in days, for ordering durations in the kernel.
    pub fn approx_days(&self) -> f64 {
        self.months as f64 * 30.4375 + self.days as f64 + crate::kernel::time::secs_f64(self.nanos) / 86_400.0
    }
    /// The shortest calendar length in nanoseconds this duration can take.
    pub fn min_nanos(&self) -> i64 {
        ((self.months / 12) * 365 + (self.months % 12) * 28 + self.days) * crate::kernel::time::DAY + self.nanos
    }
    /// The longest calendar length in nanoseconds this duration can take.
    pub fn max_nanos(&self) -> i64 {
        ((self.months / 12) * 366 + (self.months % 12) * 31 + self.days) * crate::kernel::time::DAY + self.nanos
    }
    pub fn min_days(&self) -> i64 {
        self.min_nanos() / crate::kernel::time::DAY
    }
    pub fn max_days(&self) -> i64 {
        self.max_nanos() / crate::kernel::time::DAY
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::kernel::time::{HOUR, MIN, SEC};
        match (self.months, self.days, self.nanos) {
            (0, 0, n) if n % HOUR == 0 && n != 0 => write!(f, "{}h", n / HOUR),
            (0, 0, n) if n % MIN == 0 && n != 0 => write!(f, "{}min", n / MIN),
            (0, 0, n) if n % SEC == 0 && n != 0 => write!(f, "{}s", n / SEC),
            (0, 0, n) if n != 0 => write!(f, "{}ns", n),
            (0, d, 0) => write!(f, "{}d", d),
            (m, 0, 0) if m % 12 == 0 => write!(f, "{}y", m / 12),
            (m, 0, 0) => write!(f, "{}mo", m),
            (m, d, 0) => write!(f, "{}mo+{}d", m, d),
            (m, d, n) if n % SEC == 0 => write!(f, "{}mo+{}d+{}s", m, d, n / SEC),
            (m, d, n) => write!(f, "{}mo+{}d+{}ns", m, d, n),
        }
    }
}

/// A literal value with its unit.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Lit {
    Int(i64),
    Float(f64),
    /// `100 shares`, `2 contracts`, `0 units`: a quantity in the class's
    /// unit, with the word it was spelled with.
    Quantity(f64, String),
    /// `1_000_000 USD`: a notional.
    Money(f64, String),
    /// `60 USD/share`, `0 USD/contract`: currency per unit, Price<code>.
    Price(f64, String),
    Duration(Duration),
    /// A date, `2000-01-01`; `i64::MIN` is "from the first bar".
    Time(i64),
    /// A string literal as parsed, `"SPY"` or `"SP500"`; the checker resolves
    /// it to an entity or a label by the type its context expects.
    Str(String),
    Entity(String),
    Label(String),
}

impl Lit {
    pub fn ty(&self) -> Ty {
        match self {
            Lit::Int(_) => Ty::IntLit,
            Lit::Float(_) => Ty::scalar(),
            Lit::Quantity(..) => Ty::Quantity(Dim::shares()),
            Lit::Money(_, c) => Ty::Quantity(Dim::notional(c)),
            Lit::Price(_, c) => Ty::Quantity(Dim::price(c)),
            Lit::Duration(_) => Ty::Duration,
            Lit::Time(_) => Ty::Timestamp,
            Lit::Str(_) => Ty::StrLit,
            Lit::Entity(_) => Ty::Entity,
            Lit::Label(_) => Ty::Label,
        }
    }
    /// The numeric magnitude of a number, quantity, money or price literal.
    pub fn magnitude(&self) -> Option<f64> {
        match self {
            Lit::Int(i) => Some(*i as f64),
            Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Some(*x),
            _ => None,
        }
    }
}

impl fmt::Display for Lit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lit::Int(i) => write!(f, "{}", i),
            Lit::Float(x) => write!(f, "{}", x),
            Lit::Quantity(x, u) => write!(f, "{} {}", x, u),
            Lit::Money(x, c) => write!(f, "{} {}", x, c),
            Lit::Price(x, c) => write!(f, "{} {}/unit", x, c),
            Lit::Duration(d) => write!(f, "{}", d),
            Lit::Time(t) if *t == i64::MIN => write!(f, "first"),
            Lit::Time(t) => write!(f, "{}", crate::kernel::time::format_timestamp(*t)),
            Lit::Str(s) | Lit::Entity(s) | Lit::Label(s) => write!(f, "\"{}\"", s),
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
    /// A decision constructor such as `hold(A, W)` (with its order term
    /// third), or an order with a price, `limit(P, gtc)`.
    Ctor(String, Vec<Term>, Span),
}

/// The constructor name a bracketed list of terms parses to (`combo([leg(A,
/// Q), leg(B, R)])`, K-2.3).
pub const LIST: &str = "[]";

/// A constructor application as the program writes it: `name(a, b)`, or
/// `[a, b]` for a list.
pub fn ctor_text(name: &str, args: &[String]) -> String {
    if name == LIST {
        format!("[{}]", args.join(", "))
    } else {
        format!("{}({})", name, args.join(", "))
    }
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
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                f.write_str(&ctor_text(c, &args))
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

/// A scalar-function expression.
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

impl CmpOp {
    /// The comparison that holds exactly when this one does not (on
    /// defined values).
    pub fn negate(self) -> CmpOp {
        match self {
            CmpOp::Lt => CmpOp::Ge,
            CmpOp::Le => CmpOp::Gt,
            CmpOp::Gt => CmpOp::Le,
            CmpOp::Ge => CmpOp::Lt,
            CmpOp::Eq => CmpOp::Eq,
        }
    }
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

/// The temporal builtins (section 4 of the semantic model, 3.5 of the
/// second formulation).
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
    /// `session_of(A, T, S)`: S is the label of the session of base bar T.
    SessionOf { a: Term, t: Term, s: Term },
}

/// The temporal constraints of an aggregation group (section 7.2, 3.3, 3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowKind {
    /// `T1 in window[D] min K`: the bars of the head's calendar in [T−D, T].
    Window,
    /// `T1 in prior_window[D] min K`: the bars in [T−D, T).
    Prior,
    /// `T1 in rows[N] [min K]`: the entity's last N rows at or before T.
    Rows,
    /// `T1 in prior_rows[N] [min K]`: the entity's last N rows before T.
    PriorRows,
    /// `T1 in bucket(C, T)`: the base bars whose C bucket is T.
    Bucket(Cal),
    /// `T1 in session(X, [a, b))`: the base bars of X's session with offset
    /// in [a, b), at or before X.
    Session,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// How `rank` numbers tied tuples: by their position in the total order
/// (`ordinal`, the order must be total), or the mean of the positions a run
/// of equal keys spans (`average`, the order need not be total).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ties {
    Ordinal,
    Average,
}

/// One branch of a case form: a guard (a conjunction of literals) and the
/// expression the variable takes when the guard holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub guard: Vec<Literal>,
    pub value: Expr,
    pub span: Span,
}

/// The literal forms of a body (section 5 and corpus-v2/README.md).
#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Atom(Atom),
    Neg(Atom),
    Builtin(Builtin, Span),
    /// A temporal constraint of an aggregation group: `var` ranges over the
    /// bars the kind selects relative to `base`; `dur` is the duration, row
    /// count or upper offset; `min` the minimum count; `lo` the lower offset
    /// of a session constraint.
    Window {
        var: Term,
        kind: WindowKind,
        base: Term,
        dur: Expr,
        min: Expr,
        lo: Option<Expr>,
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
    /// `X = agg(e, ...) over (conj) [within (vars)]`.
    Agg {
        var: String,
        agg: String,
        args: Vec<Expr>,
        conj: Vec<Literal>,
        within: Option<Vec<String>>,
        span: Span,
    },
    /// `top[n](R(...) by (...)) within (vars)` keeps at most n tuples per
    /// group; `K = rank(R(...) by (...)) within (vars)` keeps every tuple
    /// and binds K to its 1-based rank in the group (`n` is then `None`).
    Top {
        n: Option<Expr>,
        atom: Atom,
        by: Option<Vec<(String, Dir, Span)>>,
        rank: Option<(String, Ties, Span)>,
        within: Option<Vec<String>>,
        span: Span,
    },
    /// `R(..., T0, ...) asof T`: the tuples of R at the latest key at or
    /// before T; with `within_bucket`, only a key in T's bucket of that
    /// calendar (a bar view of a finer series).
    AsOf {
        atom: Atom,
        at: Term,
        within_bucket: Option<Cal>,
        span: Span,
    },
    /// `X = case { G -> e ; ... ; else -> e }`: the first branch whose guard
    /// holds binds X; with no `else`, an instance no guard admits is
    /// excluded (Definition 5.9).
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
            Literal::Builtin(_, s) => *s,
            Literal::Window { span, .. }
            | Literal::Cmp { span, .. }
            | Literal::Assign { span, .. }
            | Literal::Agg { span, .. }
            | Literal::Top { span, .. }
            | Literal::AsOf { span, .. }
            | Literal::Case { span, .. } => *span,
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
                Builtin::SessionOf { a, t, s } => format!("session_of({}, {}, {})", a, t, s),
            },
            Literal::Window { var, kind, base, dur, min, lo, .. } => match kind {
                WindowKind::Window => format!("{} in window[{}] min {}", var, dur, min),
                WindowKind::Prior => format!("{} in prior_window[{}] min {}", var, dur, min),
                WindowKind::Rows => format!("{} in rows[{}] min {}", var, dur, min),
                WindowKind::PriorRows => format!("{} in prior_rows[{}] min {}", var, dur, min),
                WindowKind::Bucket(c) => format!("{} in bucket({}, {})", var, c, base),
                WindowKind::Session => format!("{} in session({}, [{}, {}))", var, base, lo.as_ref().map(|e| e.to_string()).unwrap_or_default(), dur),
            },
            Literal::Cmp { op, lhs, rhs, .. } => format!("{} {} {}", lhs, op, rhs),
            Literal::Assign { var, expr, .. } => format!("{} = {}", var, expr),
            Literal::Agg { var, agg, args, within, .. } => {
                let a: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                let w = within.as_ref().map(|w| format!(" within ({})", w.join(", "))).unwrap_or_default();
                format!("{} = {}({}) over (...){}", var, agg, a.join(", "), w)
            }
            Literal::Top { n: Some(n), atom, within, .. } => {
                let w = within.as_ref().map(|w| format!(" within ({})", w.join(", "))).unwrap_or_default();
                format!("top[{}]({} by ...){}", n, atom, w)
            }
            Literal::Top { atom, rank, within, .. } => {
                let w = within.as_ref().map(|w| format!(" within ({})", w.join(", "))).unwrap_or_default();
                format!(
                    "{} = {}({} by ...){}",
                    rank.as_ref().map(|(k, _, _)| k.as_str()).unwrap_or("_"),
                    rank.as_ref().map(|(_, t, _)| if *t == Ties::Average { "rank_average" } else { "rank" }).unwrap_or("rank"),
                    atom,
                    w
                )
            }
            Literal::AsOf { atom, at, .. } => format!("{} asof {}", atom, at),
            Literal::Case { var, branches, otherwise, .. } => {
                let mut parts: Vec<String> = branches
                    .iter()
                    .map(|b| {
                        let g: Vec<String> = b.guard.iter().map(|l| l.describe()).collect();
                        format!("{} -> {}", g.join(", "), b.value)
                    })
                    .collect();
                if let Some(e) = otherwise {
                    parts.push(format!("else -> {}", e));
                }
                format!("{} = case {{ {} }}", var, parts.join(" ; "))
            }
        }
    }
    /// The literals nested in this one (an aggregation's conjunction, a case
    /// form's guards), depth first, the literal itself first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Literal>) {
        out.push(self);
        match self {
            Literal::Agg { conj, .. } => conj.iter().for_each(|l| l.walk(out)),
            Literal::Case { branches, .. } => branches.iter().for_each(|b| b.guard.iter().for_each(|l| l.walk(out))),
            _ => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub head: Atom,
    pub body: Vec<Literal>,
    pub span: Span,
    /// Name of the unit (library instance or strategy) the rule is written in.
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

/// The decision constructors of the one decision algebra (section 4.4), as
/// the checker resolves them: `hold(A, W)` with a Scalar is a target weight,
/// with a quantity a target quantity; `trade(A, Q)` is a signed delta.
pub const DECISION_CTORS: &[&str] = &["hold_weight", "hold_quantity", "trade"];

/// Decision constructor field types.
pub fn ctor_fields(name: &str) -> Option<Vec<Ty>> {
    Some(match name {
        "hold_quantity" | "trade" => vec![Ty::Entity, Ty::Quantity(Dim::shares())],
        "hold_weight" => vec![Ty::Entity, Ty::scalar()],
        _ => return None,
    })
}

/// The closed set of aggregates.
pub const AGGREGATES: &[&str] = &[
    "sum",
    "mean",
    "std",
    "var",
    "median",
    "quantile",
    "max",
    "min",
    "count",
    "corr",
    "cov",
    "ols_slope",
    "ols_r",
    "first",
    "last",
];
/// Scalar functions (section 7.3).
pub const SCALAR_FUNCTIONS: &[&str] = &[
    "log",
    "exp",
    "sqrt",
    "abs",
    "least",
    "greatest",
    "sign",
    "floor",
    "ceil",
    "clamp",
    "decile",
    "bs_price",
    "bs_delta",
    "bs_gamma",
    "bs_vega",
    "bs_theta",
    "bs_rho",
    "b76_price",
    "b76_delta",
    "b76_gamma",
    "b76_vega",
    "b76_theta",
    "b76_rho",
    "am_price",
    "implied_vol",
];

/// The option pricing functions of K-2.5 (a subset of `SCALAR_FUNCTIONS`).
pub const PRICING_FUNCTIONS: &[&str] = &[
    "bs_price",
    "bs_delta",
    "bs_gamma",
    "bs_vega",
    "bs_theta",
    "bs_rho",
    "b76_price",
    "b76_delta",
    "b76_gamma",
    "b76_vega",
    "b76_theta",
    "b76_rho",
    "am_price",
    "implied_vol",
];
/// The order kinds a decision term may end with.
pub const ORDER_KINDS: &[&str] = &["market", "moo", "moc", "moo_moc"];

/// Names a relation may not be declared with: the temporal builtins, the
/// literal-form and block keywords, the kernel's relations and the decision
/// constructors. The aggregates and scalar functions are reserved too, so
/// that a body atom is never mistaken for one.
pub const RESERVED_NAMES: &[&str] = &[
    "prev",
    "lag",
    "month_start",
    "day_start",
    "session_of",
    "window",
    "prior_window",
    "rows",
    "prior_rows",
    "bucket",
    "session",
    "top",
    "rank",
    "rank_average",
    "case",
    "else",
    "within",
    "decide",
    "decided",
    "position",
    "cash",
    "nav",
    "capital",
    "mark",
    "fill",
    "clock",
    "spec",
    "chain",
    "greeks",
    "margin_req",
    "margin_by",
    "contract",
    "universe",
    "instruments",
    "not",
    "in",
    "min",
    "by",
    "over",
    "asof",
    "asc",
    "desc",
    "strategy",
    "library",
    "class",
    "data",
    "uses",
    "calendars",
    "params",
    "rules",
    "decisions",
    "execution",
    "report",
    "observe",
    "rel",
    "complete",
    "need",
    "require",
    "prefer",
    "silence",
    "orders",
    "hold",
    "trade",
    "market",
    "moo",
    "moc",
    "moo_moc",
    "limit",
    "stop",
    "sum",
    "mean",
    "std",
    "var",
    "median",
    "quantile",
    "max",
    "count",
    "corr",
    "cov",
    "ols_slope",
    "ols_r",
    "first",
    "last",
    "log",
    "exp",
    "sqrt",
    "abs",
    "least",
    "greatest",
    "sign",
    "floor",
    "ceil",
    "clamp",
    "decile",
];

pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMES.contains(&name) || PRICING_FUNCTIONS.contains(&name)
}

/// Names a relation may not take. A function's name is admissible for a
/// relation with bracketed inputs: every call of it is written `name[...]`,
/// which no function call is (the xsec library's `decile[R]` beside the
/// function `decile`).
pub fn is_reserved_relation(name: &str, bracketed: bool) -> bool {
    is_reserved(name) && !(bracketed && SCALAR_FUNCTIONS.contains(&name))
}

/// Names a parameter may not take: the keywords and temporal builtins that
/// would be ambiguous as a bare identifier. A function, an aggregate, a
/// decision constructor or an order kind is only ever written applied or
/// inside a decision, so `floor`, `hold` and `stop` are admissible.
pub fn is_reserved_param(name: &str) -> bool {
    if name == "min" {
        return true;
    }
    is_reserved(name) && !(name == "hold" || name == "trade" || name == "limit" || name == "stop" || ORDER_KINDS.contains(&name) || AGGREGATES.contains(&name) || SCALAR_FUNCTIONS.contains(&name))
}

/// The option class's kernel relations (K-2.4) at the decision calendar,
/// written by the executor at every decision barrier: a contract's terms as
/// of the bar (`spec`, adjusted by corporate actions), the contracts of an
/// underlying's chain the program's dynamic need admits (`chain`), and the
/// greeks, from the binding's rows (`greeks data`) or the model's.
pub fn option_relations(cal: &Cal) -> Vec<Signature> {
    let arg = |name: &str, ty: Ty| Arg {
        name: name.to_string(),
        mode: if name == "T" { Mode::Key } else { Mode::Out },
        ty,
    };
    let sig = |name: &str, args: Vec<Arg>| Signature {
        name: name.to_string(),
        args,
        cal: cal.clone(),
        complete: true,
        kind: Kind::Executor,
        span: Span::default(),
        native: None,
    };
    vec![
        sig(
            "spec",
            vec![
                arg("A", Ty::Entity),
                arg("T", Ty::Timestamp),
                arg("Underlying", Ty::Entity),
                arg("Right", Ty::Label),
                arg("Strike", Ty::Quantity(Dim::price("USD"))),
                arg("Expiry", Ty::Timestamp),
                arg("Style", Ty::Label),
                arg("Settlement", Ty::Label),
                arg("Multiplier", Ty::scalar()),
            ],
        ),
        sig("chain", vec![arg("U", Ty::Entity), arg("T", Ty::Timestamp), arg("A", Ty::Entity)]),
        sig(
            "greeks",
            vec![
                arg("A", Ty::Entity),
                arg("T", Ty::Timestamp),
                arg("IV", Ty::scalar()),
                arg("Delta", Ty::scalar()),
                arg("Gamma", Ty::scalar()),
                arg("Theta", Ty::scalar()),
                arg("Vega", Ty::scalar()),
                arg("Rho", Ty::scalar()),
            ],
        ),
    ]
}

/// The book's margin (K-2.4, ERRATA E-4) at the decision calendar:
/// `margin_req(@T, Initial, Maintenance)`, the book's requirement, and
/// `margin_by(A, @T, Initial, Maintenance, Source)`, each instrument's with
/// the model that priced it. The second only where the execution block
/// lists margin models.
pub fn margin_relations(cal: &Cal, by_instrument: bool) -> Vec<Signature> {
    let arg = |name: &str, ty: Ty| Arg {
        name: name.to_string(),
        mode: if name == "T" { Mode::Key } else { Mode::Out },
        ty,
    };
    let sig = |name: &str, args: Vec<Arg>| Signature {
        name: name.to_string(),
        args,
        cal: cal.clone(),
        complete: true,
        kind: Kind::Executor,
        span: Span::default(),
        native: None,
    };
    let usd = || Ty::Quantity(Dim::notional("USD"));
    let mut out = vec![sig("margin_req", vec![arg("T", Ty::Timestamp), arg("Initial", usd()), arg("Maintenance", usd())])];
    if by_instrument {
        out.push(sig(
            "margin_by",
            vec![
                arg("A", Ty::Entity),
                arg("T", Ty::Timestamp),
                arg("Initial", usd()),
                arg("Maintenance", usd()),
                arg("Source", Ty::Label),
            ],
        ));
    }
    out
}

/// The future class's kernel relation (K-2.4): `contract(A, @T, Root,
/// Expiry, PointValue)`, the contract A a root's continuous series holds at
/// T (D-2.4 `continuous`), with its expiry and point value.
pub fn future_relations(cal: &Cal) -> Vec<Signature> {
    let arg = |name: &str, ty: Ty| Arg {
        name: name.to_string(),
        mode: if name == "T" { Mode::Key } else { Mode::Out },
        ty,
    };
    vec![Signature {
        name: "contract".to_string(),
        args: vec![
            arg("A", Ty::Entity),
            arg("T", Ty::Timestamp),
            arg("Root", Ty::Entity),
            arg("Expiry", Ty::Timestamp),
            arg("PointValue", Ty::scalar()),
        ],
        cal: cal.clone(),
        complete: true,
        kind: Kind::Executor,
        span: Span::default(),
        native: None,
    }]
}

/// The executor and kernel-state relations every strategy sees at its
/// decision calendar (corpus-v2/README.md, "Kernel relations").
pub fn kernel_relations(cal: &Cal) -> Vec<Signature> {
    let arg = |name: &str, mode: Mode, ty: Ty| Arg { name: name.to_string(), mode, ty };
    let sig = |name: &str, args: Vec<Arg>, kind: Kind| Signature {
        name: name.to_string(),
        args,
        cal: cal.clone(),
        complete: true,
        kind,
        span: Span::default(),
        native: None,
    };
    vec![
        sig(
            "position",
            vec![arg("A", Mode::Out, Ty::Entity), arg("T", Mode::Key, Ty::Timestamp), arg("Q", Mode::Out, Ty::Quantity(Dim::shares()))],
            Kind::Executor,
        ),
        sig(
            "cash",
            vec![arg("T", Mode::Key, Ty::Timestamp), arg("C", Mode::Out, Ty::Quantity(Dim::notional("USD")))],
            Kind::Executor,
        ),
        sig("nav", vec![arg("T", Mode::Key, Ty::Timestamp), arg("N", Mode::Out, Ty::Quantity(Dim::notional("USD")))], Kind::Executor),
        sig(
            "capital",
            vec![arg("T", Mode::Key, Ty::Timestamp), arg("C", Mode::Out, Ty::Quantity(Dim::notional("USD")))],
            Kind::Executor,
        ),
        sig(
            "mark",
            vec![
                arg("A", Mode::Out, Ty::Entity),
                arg("T", Mode::Key, Ty::Timestamp),
                arg("P", Mode::Out, Ty::Quantity(Dim::price("USD"))),
            ],
            Kind::Executor,
        ),
        sig(
            "fill",
            vec![
                arg("A", Mode::Out, Ty::Entity),
                arg("T", Mode::Key, Ty::Timestamp),
                arg("Q", Mode::Out, Ty::Quantity(Dim::shares())),
                arg("P", Mode::Out, Ty::Quantity(Dim::price("USD"))),
            ],
            Kind::Executor,
        ),
        sig("decided", vec![arg("T0", Mode::Key, Ty::Timestamp), arg("D", Mode::Out, Ty::Decision)], Kind::KernelState),
        sig("decide", vec![arg("T", Mode::Key, Ty::Timestamp), arg("D", Mode::Out, Ty::Decision)], Kind::Output),
    ]
}
