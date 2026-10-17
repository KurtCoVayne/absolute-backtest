//! Runtime values. Equities are interned symbols whose ids follow identifier
//! order, so that `by (A asc)` is the order "by identifier" of section 2.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::ir::Duration;

pub type Sym = u32;

/// The one decision algebra (language-v2, 4.4; K-2.3): a target weight of
/// capital, a target quantity, a signed delta, an all-or-none multi-leg
/// delta, or an option exercised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Ctor {
    HoldWeight,
    HoldQuantity,
    Trade,
    /// `combo([leg(A1, Q1), ...])`: the decision's `legs`, filled all or
    /// none at one barrier (K-5.5.3).
    Combo,
    /// `exercise(A, Q)`: Q long contracts of the option A exercised at the
    /// bar's close (K-5.5.4).
    Exercise,
}

impl Ctor {
    pub fn parse(name: &str) -> Option<Ctor> {
        Some(match name {
            "hold_weight" => Ctor::HoldWeight,
            "hold_quantity" => Ctor::HoldQuantity,
            "trade" => Ctor::Trade,
            "combo" => Ctor::Combo,
            "exercise" => Ctor::Exercise,
            _ => return None,
        })
    }
    /// The constructor as the program writes it.
    pub fn name(self) -> &'static str {
        match self {
            Ctor::HoldWeight | Ctor::HoldQuantity => "hold",
            Ctor::Trade => "trade",
            Ctor::Combo => "combo",
            Ctor::Exercise => "exercise",
        }
    }
    /// The resolved name, distinguishing the two `hold` forms.
    pub fn resolved_name(self) -> &'static str {
        match self {
            Ctor::HoldWeight => "hold_weight",
            Ctor::HoldQuantity => "hold_quantity",
            Ctor::Trade => "trade",
            Ctor::Combo => "combo",
            Ctor::Exercise => "exercise",
        }
    }
    /// A target (the book is driven to it) rather than a delta.
    pub fn is_target(self) -> bool {
        matches!(self, Ctor::HoldWeight | Ctor::HoldQuantity)
    }
    /// Whether a written constructor name matches this constructor: `hold`
    /// matches both of its forms.
    pub fn matches(self, name: &str) -> bool {
        match name {
            "hold" => self.is_target(),
            "trade" => self == Ctor::Trade,
            "combo" => self == Ctor::Combo,
            "exercise" => self == Ctor::Exercise,
            other => Ctor::parse(other) == Some(self),
        }
    }
}

/// How an order executes (section 6, order types): at the next bar's
/// close (`Market`, the default contract), at the open of the instrument's next
/// bar (`Moo`), at the close of the last bar of the session containing the
/// decision (`Moc`), at the next open and back to flat at the close of that
/// session (`MooMoc`, an intraday position), or when a bar trades through a
/// price (`Limit`, `Stop`).
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OrderKind {
    #[default]
    Market,
    Moo,
    Moc,
    MooMoc,
    Limit(f64),
    Stop(f64),
}

/// How long a resting order works: the session of the first bar it could
/// fill at (`Day`), until filled or superseded (`Gtc`), or a number of
/// decision bars (`Bars`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Tif {
    #[default]
    Day,
    Gtc,
    Bars(u32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Order {
    pub kind: OrderKind,
    pub tif: Tif,
}

impl Order {
    /// A total key for equality, hashing and ordering.
    fn key(&self) -> (u8, u64, u8, u32) {
        let (k, p) = match self.kind {
            OrderKind::Market => (0, 0),
            OrderKind::Moo => (1, 0),
            OrderKind::Moc => (2, 0),
            OrderKind::MooMoc => (5, 0),
            OrderKind::Limit(p) => (3, bits(p)),
            OrderKind::Stop(p) => (4, bits(p)),
        };
        let (t, n) = match self.tif {
            Tif::Day => (0, 0),
            Tif::Gtc => (1, 0),
            Tif::Bars(n) => (2, n),
        };
        (k, p, t, n)
    }
    pub fn is_market(&self) -> bool {
        self.kind == OrderKind::Market
    }
}

impl std::fmt::Display for Order {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tif = match self.tif {
            Tif::Day => "day".to_string(),
            Tif::Gtc => "gtc".to_string(),
            Tif::Bars(n) => format!("bars({})", n),
        };
        match self.kind {
            OrderKind::Market => f.write_str("market"),
            OrderKind::Moo => f.write_str("moo"),
            OrderKind::Moc => f.write_str("moc"),
            OrderKind::MooMoc => f.write_str("moo_moc"),
            OrderKind::Limit(p) => write!(f, "limit({}, {})", p, tif),
            OrderKind::Stop(p) => write!(f, "stop({}, {})", p, tif),
        }
    }
}

/// A decision value (section 4): a constructor, an equity, an amount
/// (shares for delta constructors and `target_quantity`, a weight for
/// `target_weight`) and how the order executes.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    pub ctor: Ctor,
    /// The instrument; a combo's first leg's.
    pub equity: Sym,
    /// The amount; a combo's is its number of legs.
    pub amount: f64,
    #[serde(default)]
    pub order: Order,
    /// A combo's legs: each instrument with its signed delta (K-2.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legs: Vec<(Sym, f64)>,
}

impl Decision {
    /// A decision of one instrument (every constructor but `combo`).
    pub fn single(ctor: Ctor, equity: Sym, amount: f64, order: Order) -> Decision {
        Decision {
            ctor,
            equity,
            amount,
            order,
            legs: Vec::new(),
        }
    }

    /// The instruments the decision acts on: a combo's legs, else its own.
    pub fn instruments(&self) -> Vec<Sym> {
        if self.ctor == Ctor::Combo {
            self.legs.iter().map(|(s, _)| *s).collect()
        } else {
            vec![self.equity]
        }
    }

    /// A total key of the legs for equality, hashing and ordering.
    fn legs_key(&self) -> Vec<(Sym, u64)> {
        self.legs.iter().map(|(s, q)| (*s, bits(*q))).collect()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Equity(Sym),
    Time(i64),
    Num(f64),
    Count(i64),
    /// Boxed, as is `Decision`: both are rare as values, and keeping every
    /// variant's payload to 8 bytes makes a `Value` 16 bytes.
    Dur(Box<Duration>),
    Decision(Box<Decision>),
    /// A name from the bundle's vocabulary (`Ty::Label`), interned in the
    /// dataset's label table, distinct from an equity of the same spelling.
    Label(Sym),
}

// Every variant's payload is at most 8 bytes: a value is 16 (the stores hold
// tens of millions of them).
const _: () = assert!(std::mem::size_of::<Value>() == 16);

fn bits(x: f64) -> u64 {
    // Fold -0.0 into 0.0 so that equal numbers hash equally.
    if x == 0.0 {
        0
    } else {
        x.to_bits()
    }
}

impl PartialEq for Value {
    fn eq(&self, o: &Value) -> bool {
        match (self, o) {
            (Value::Equity(a), Value::Equity(b)) => a == b,
            (Value::Time(a), Value::Time(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => bits(*a) == bits(*b),
            (Value::Count(a), Value::Count(b)) => a == b,
            (Value::Dur(a), Value::Dur(b)) => a == b,
            (Value::Decision(a), Value::Decision(b)) => a.ctor == b.ctor && a.equity == b.equity && bits(a.amount) == bits(b.amount) && a.order.key() == b.order.key() && a.legs_key() == b.legs_key(),
            (Value::Label(a), Value::Label(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Value::Equity(a) => {
                0u8.hash(h);
                a.hash(h)
            }
            Value::Time(a) => {
                1u8.hash(h);
                a.hash(h)
            }
            Value::Num(a) => {
                2u8.hash(h);
                bits(*a).hash(h)
            }
            Value::Count(a) => {
                3u8.hash(h);
                a.hash(h)
            }
            Value::Dur(a) => {
                4u8.hash(h);
                a.hash(h)
            }
            Value::Decision(d) => {
                5u8.hash(h);
                d.ctor.hash(h);
                d.equity.hash(h);
                bits(d.amount).hash(h);
                d.order.key().hash(h);
                d.legs_key().hash(h)
            }
            Value::Label(a) => {
                6u8.hash(h);
                a.hash(h)
            }
        }
    }
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Equity(_) => 0,
        Value::Time(_) => 1,
        Value::Num(_) => 2,
        Value::Count(_) => 3,
        Value::Dur(_) => 4,
        Value::Decision(_) => 5,
        Value::Label(_) => 6,
    }
}

impl Ord for Value {
    fn cmp(&self, o: &Value) -> Ordering {
        match (self, o) {
            (Value::Equity(a), Value::Equity(b)) => a.cmp(b),
            (Value::Time(a), Value::Time(b)) => a.cmp(b),
            (Value::Num(a), Value::Num(b)) => a.total_cmp(b),
            (Value::Count(a), Value::Count(b)) => a.cmp(b),
            (Value::Dur(a), Value::Dur(b)) => a.approx_days().total_cmp(&b.approx_days()).then(a.cmp(b)),
            (Value::Decision(a), Value::Decision(b)) => a
                .ctor
                .cmp(&b.ctor)
                .then(a.equity.cmp(&b.equity))
                .then(a.amount.total_cmp(&b.amount))
                .then(a.order.key().cmp(&b.order.key()))
                .then(a.legs_key().cmp(&b.legs_key())),
            (Value::Label(a), Value::Label(b)) => a.cmp(b),
            _ => rank(self).cmp(&rank(o)),
        }
    }
}
impl PartialOrd for Value {
    fn partial_cmp(&self, o: &Value) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(x) => Some(*x),
            Value::Count(c) => Some(*c as f64),
            _ => None,
        }
    }
    pub fn as_equity(&self) -> Option<Sym> {
        if let Value::Equity(s) = self {
            Some(*s)
        } else {
            None
        }
    }
    pub fn as_time(&self) -> Option<i64> {
        if let Value::Time(t) = self {
            Some(*t)
        } else {
            None
        }
    }
}

/// Interned equity identifiers, ordered by identifier.
#[derive(Clone, Debug, Default)]
pub struct Symbols {
    names: Vec<String>,
    map: HashMap<String, Sym>,
}

impl Symbols {
    pub fn new() -> Symbols {
        Symbols::default()
    }
    pub fn intern(&mut self, name: &str) -> Sym {
        if let Some(&s) = self.map.get(name) {
            return s;
        }
        let s = self.names.len() as Sym;
        self.names.push(name.to_string());
        self.map.insert(name.to_string(), s);
        s
    }
    pub fn get(&self, name: &str) -> Option<Sym> {
        self.map.get(name).copied()
    }
    pub fn name(&self, s: Sym) -> &str {
        &self.names[s as usize]
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
    pub fn names(&self) -> &[String] {
        &self.names
    }
    /// A copy whose ids follow identifier order, with the old-to-new map.
    pub fn sorted(&self) -> (Symbols, Vec<Sym>) {
        let mut idx: Vec<usize> = (0..self.names.len()).collect();
        idx.sort_by(|a, b| self.names[*a].cmp(&self.names[*b]));
        let mut out = Symbols::new();
        let mut remap = vec![0; self.names.len()];
        for old in idx {
            remap[old] = out.intern(&self.names[old]);
        }
        (out, remap)
    }
}
