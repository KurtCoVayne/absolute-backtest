//! The cost of a plan (D-7.3): a vector of money, seconds, bytes and
//! requests, with the count of sources a plan draws on, compared
//! lexicographically in the order the demand's preferences give (default
//! money, seconds, bytes, requests), and checked against the demand's
//! budget, which caps it (Proposition D-7.4).

use std::cmp::Ordering;
use std::ops::Add;

use abt_wire::generated::abt::wire::{Budget, CostVector, Preference, PreferenceKind};

/// Definition D-7.2's vector, with the number of purchases a plan makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct Cost {
    pub money: f64,
    pub seconds: f64,
    pub bytes: u64,
    pub requests: u64,
    /// The sources other than the store a plan draws on (what
    /// `fewest_sources` minimises).
    pub sources: u64,
}

impl Add for Cost {
    type Output = Cost;
    fn add(self, o: Cost) -> Cost {
        Cost {
            money: self.money + o.money,
            seconds: self.seconds + o.seconds,
            bytes: self.bytes + o.bytes,
            requests: self.requests + o.requests,
            sources: self.sources + o.sources,
        }
    }
}

impl Cost {
    pub fn wire(&self) -> CostVector {
        CostVector {
            money: self.money,
            seconds: self.seconds,
            bytes: self.bytes as i64,
            requests: self.requests as i64,
        }
    }
}

/// A component a plan's costs are compared on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Money,
    Seconds,
    Bytes,
    Requests,
    Sources,
}

/// The order costs compare in (Definition D-7.3): `fewest_sources` first
/// when preferred, then money (`cheapest` is the default), seconds, bytes
/// and requests.
pub fn order(prefer: &[Preference]) -> Vec<Key> {
    let mut keys = Vec::new();
    if prefer.iter().any(|p| p.kind == PreferenceKind::FewestSources) {
        keys.push(Key::Sources);
    }
    keys.extend([Key::Money, Key::Seconds, Key::Bytes, Key::Requests]);
    keys
}

fn component(c: &Cost, k: Key) -> f64 {
    match k {
        Key::Money => c.money,
        Key::Seconds => c.seconds,
        Key::Bytes => c.bytes as f64,
        Key::Requests => c.requests as f64,
        Key::Sources => c.sources as f64,
    }
}

/// Two costs compared in `order`.
pub fn compare(a: &Cost, b: &Cost, order: &[Key]) -> Ordering {
    for k in order {
        match component(a, *k).total_cmp(&component(b, *k)) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    Ordering::Equal
}

/// What of `budget` a cost exceeds, if anything: money is 0 unless the
/// demand gives it (A-D22), the rest unbounded unless given.
pub fn exceeds(c: &Cost, budget: Option<&Budget>) -> Option<String> {
    let money = budget.map(|b| b.money).unwrap_or(0.0);
    if c.money > money {
        return Some(format!("money {:.2} over the budget's {:.2}", c.money, money));
    }
    let b = budget?;
    if let Some(x) = b.bytes.filter(|x| c.bytes as i64 > *x) {
        return Some(format!("{} bytes over the budget's {}", c.bytes, x));
    }
    if let Some(x) = b.requests.filter(|x| c.requests as i64 > *x) {
        return Some(format!("{} requests over the budget's {}", c.requests, x));
    }
    if let Some(x) = b.seconds.filter(|x| c.seconds > *x) {
        return Some(format!("{:.1} seconds over the budget's {:.1}", c.seconds, x));
    }
    None
}
