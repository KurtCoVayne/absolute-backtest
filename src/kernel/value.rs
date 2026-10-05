//! Runtime values. Equities are interned symbols whose ids follow identifier
//! order, so that `by (A asc)` is the order "by identifier" of section 2.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::ir::{DecisionMode, Duration};

pub type Sym = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ctor {
    Buy,
    Sell,
    Short,
    Cover,
    TargetWeight,
    TargetQuantity,
}

impl Ctor {
    pub fn parse(name: &str) -> Option<Ctor> {
        Some(match name {
            "buy" => Ctor::Buy,
            "sell" => Ctor::Sell,
            "short" => Ctor::Short,
            "cover" => Ctor::Cover,
            "target_weight" => Ctor::TargetWeight,
            "target_quantity" => Ctor::TargetQuantity,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Ctor::Buy => "buy",
            Ctor::Sell => "sell",
            Ctor::Short => "short",
            Ctor::Cover => "cover",
            Ctor::TargetWeight => "target_weight",
            Ctor::TargetQuantity => "target_quantity",
        }
    }
    pub fn mode(self) -> DecisionMode {
        match self {
            Ctor::Buy | Ctor::Sell | Ctor::Short | Ctor::Cover => DecisionMode::Delta,
            Ctor::TargetWeight | Ctor::TargetQuantity => DecisionMode::Target,
        }
    }
}

/// A decision value (section 4): a constructor, an equity and an amount
/// (shares for delta constructors and `target_quantity`, a weight for
/// `target_weight`).
#[derive(Clone, Debug)]
pub struct Decision {
    pub ctor: Ctor,
    pub equity: Sym,
    pub amount: f64,
}

#[derive(Clone, Debug)]
pub enum Value {
    Equity(Sym),
    Time(i64),
    Num(f64),
    Count(i64),
    Dur(Duration),
    Decision(Decision),
    /// A name from the bundle's vocabulary (`Ty::Label`), interned in the
    /// dataset's label table, distinct from an equity of the same spelling.
    Label(Sym),
}

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
            (Value::Decision(a), Value::Decision(b)) => a.ctor == b.ctor && a.equity == b.equity && bits(a.amount) == bits(b.amount),
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
                bits(d.amount).hash(h)
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
            (Value::Decision(a), Value::Decision(b)) => a.ctor.cmp(&b.ctor).then(a.equity.cmp(&b.equity)).then(a.amount.total_cmp(&b.amount)),
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
