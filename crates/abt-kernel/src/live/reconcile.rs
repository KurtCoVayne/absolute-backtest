//! Reconciliation (K-6.4): at every session open and on a reconnect, the
//! broker's account against the book. Under `halt` (the default) a
//! difference halts the run; under `adopt` the book becomes the broker's
//! account, a `reconciliation` event; under `warn` it is reported and the
//! book stands.

use serde::{Deserialize, Serialize};

use super::broker::Snapshot;
use crate::kernel::events::Book;
use crate::kernel::value::Symbols;
use crate::kernel::Sym;

/// What a difference does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    #[default]
    Halt,
    Adopt,
    Warn,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "halt" => Some(Policy::Halt),
            "adopt" => Some(Policy::Adopt),
            "warn" => Some(Policy::Warn),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Policy::Halt => "halt",
            Policy::Adopt => "adopt",
            Policy::Warn => "warn",
        }
    }
}

/// Where the book and the broker's account differ.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Difference {
    /// Each instrument held differently: the book's position and the
    /// broker's.
    pub positions: Vec<(Sym, f64, f64)>,
    /// What the broker holds of instruments the run does not know, by
    /// name.
    pub unknown: Vec<(String, f64)>,
    /// The book's cash and the broker's, and whether they differ by more
    /// than the tolerance.
    pub cash: (f64, f64),
    pub cash_differs: bool,
}

impl Difference {
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty() && self.unknown.is_empty() && !self.cash_differs
    }

    /// The difference in words.
    pub fn describe(&self, symbols: &Symbols) -> String {
        let mut parts: Vec<String> = self
            .positions
            .iter()
            .map(|(s, book, broker)| format!("{} {} in the book, {} at the broker", symbols.name(*s), book, broker))
            .collect();
        parts.extend(self.unknown.iter().map(|(name, q)| format!("{} {} at the broker, an instrument the run does not know", name, q)));
        if self.cash_differs {
            parts.push(format!("cash {} in the book, {} at the broker", self.cash.0, self.cash.1));
        }
        parts.join("; ")
    }
}

/// The book against the broker's snapshot: positions that differ by more
/// than a billionth of a unit, and cash by more than `cash_tolerance`.
pub fn compare(book: &Book, symbols: &Symbols, snapshot: &Snapshot, cash_tolerance: f64) -> Difference {
    let mut d = Difference {
        cash: (book.cash, snapshot.cash),
        cash_differs: (book.cash - snapshot.cash).abs() > cash_tolerance,
        ..Difference::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    for (name, &q) in &snapshot.positions {
        match symbols.get(name) {
            Some(s) => {
                seen.insert(s);
                let held = book.position(s);
                if (held - q).abs() > 1e-9 {
                    d.positions.push((s, held, q));
                }
            }
            None if q.abs() > 1e-9 => d.unknown.push((name.clone(), q)),
            None => {}
        }
    }
    for (&s, &q) in &book.positions {
        if !seen.contains(&s) && q.abs() > 1e-9 {
            d.positions.push((s, q, 0.0));
        }
    }
    d.positions.sort_by_key(|p| p.0);
    d
}
