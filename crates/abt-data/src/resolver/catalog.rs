//! The source catalog (D-7.1): the sources a demand may be satisfied from,
//! what each offers, at what price and under what quota, read from a
//! configuration file in which A-8's placeholder prices (`databento.gb`,
//! `orats.req`, …) are given values. The store is itself a source, of kind
//! `store`, whose every price is zero; what it holds of each source is the
//! registry's versions (`cached`), not a part of the file.
//!
//! The file is JSON:
//!
//! ```json
//! { "prices": { "databento.gb": 0.5 },
//!   "sources": [ { "name": "databento", "kind": "vendor", "adapter": "databento",
//!                  "freshness": "historical", "throughput": 2.0e7,
//!                  "offers": [ { "class": "future", "relations": ["open", "close"],
//!                                "calendar": "1m", "from": "2010-06-06",
//!                                "has_receipt": false, "instruments": 30 } ],
//!                  "price": [ { "unit": "gb", "money": "databento.gb" } ],
//!                  "quota": { "window": "1s", "requests": 100 } } ] }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use abt_wire::scalar::parse_timestamp;

use crate::error::DataError;

/// What a source is (D-7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Store,
    Tier,
    Vendor,
    Feed,
}

/// What a source can provide (D-7.1's `offers`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Offer {
    pub class: String,
    /// The relations, by their names in the class (`close`).
    pub relations: Vec<String>,
    /// The bars it delivers (`1d`, `1m`).
    pub calendar: String,
    /// The first and last day it covers (`2010-06-06`); open when absent.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// `all`, `list(L)`, `chain` or `by_request`.
    #[serde(default = "all")]
    pub scope: String,
    /// Whether its rows carry their receipt time.
    #[serde(default)]
    pub has_receipt: bool,
    /// What it supplies beside prices (`iv`, `greeks`, `margin`, `oi`).
    #[serde(default)]
    pub supplies: Vec<String>,
    /// What it states of its rows (`reviewed = complete`); its versions
    /// carry these.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// The instruments a request covers, for the cost's estimate (one when
    /// absent).
    #[serde(default)]
    pub instruments: Option<u64>,
}

fn all() -> String {
    "all".to_string()
}

fn historical() -> String {
    "historical".to_string()
}

/// A priced unit (D-7.1's `price`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Request,
    Gb,
    SymbolDay,
    Month,
}

/// A unit price: a number, or the name of one of the file's `prices`
/// (A-8's placeholders).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Money {
    Value(f64),
    Named(String),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Price {
    pub unit: Unit,
    pub money: Money,
    #[serde(default = "usd")]
    pub currency: String,
}

fn usd() -> String {
    "USD".to_string()
}

/// A rate limit (D-7.1's `quota`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Quota {
    /// The window the limits count over (`1s`, `1m`).
    pub window: String,
    #[serde(default)]
    pub requests: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
}

/// A source (D-7.1's `source`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    /// The adapter that fetches from it (D-7.7).
    pub adapter: String,
    /// `historical`, `delayed(d)` or `live`.
    #[serde(default = "historical")]
    pub freshness: String,
    #[serde(default)]
    pub offers: Vec<Offer>,
    #[serde(default)]
    pub price: Vec<Price>,
    #[serde(default)]
    pub quota: Option<Quota>,
    /// The bytes a second it delivers, the start of the measured average
    /// the seconds of a plan are estimated from.
    #[serde(default)]
    pub throughput: Option<f64>,
}

/// The catalog: A-8's prices and the sources beside the store.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub prices: BTreeMap<String, f64>,
    #[serde(default)]
    pub sources: Vec<Source>,
}

/// The store's own name as a source.
pub const STORE: &str = "store";

impl Catalog {
    /// The store alone: every need is bound to what the store holds.
    pub fn store_only() -> Catalog {
        Catalog::default()
    }

    /// The catalog of a configuration file, checked.
    pub fn load(path: &Path) -> Result<Catalog, DataError> {
        let text = std::fs::read_to_string(path).map_err(|e| DataError::Malformed(format!("{}: {}", path.display(), e)))?;
        Catalog::parse(&text).map_err(|e| DataError::Malformed(format!("{}: {}", path.display(), e)))
    }

    /// The catalog of a configuration text, checked: unique source names
    /// (none the store's), offers with a known calendar and days, prices
    /// that are numbers or named in `prices`.
    pub fn parse(text: &str) -> Result<Catalog, String> {
        let c: Catalog = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut names = BTreeSet::new();
        for s in &c.sources {
            if s.name == STORE {
                return Err(format!("`{}` is the store's own name", STORE));
            }
            if !names.insert(s.name.as_str()) {
                return Err(format!("the source `{}` is listed twice", s.name));
            }
            for o in &s.offers {
                if crate::resolver::calendar_nanos(&o.calendar).is_none() {
                    return Err(format!("source `{}`: `{}` is not a calendar (1d, 1m, …)", s.name, o.calendar));
                }
                for d in o.from.iter().chain(&o.to) {
                    if parse_timestamp(d).is_none() {
                        return Err(format!("source `{}`: `{}` is not a date", s.name, d));
                    }
                }
            }
            for p in &s.price {
                if let Money::Named(n) = &p.money {
                    if !c.prices.contains_key(n) {
                        return Err(format!("source `{}`: the price `{}` is not given in `prices`", s.name, n));
                    }
                }
            }
        }
        Ok(c)
    }

    pub fn source(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.name == name)
    }

    /// A unit price's value.
    pub fn money(&self, p: &Price) -> f64 {
        match &p.money {
            Money::Value(v) => *v,
            Money::Named(n) => self.prices.get(n).copied().unwrap_or(f64::NAN),
        }
    }

    /// The catalog as `abt data catalog --sources` prints it: the store,
    /// then every source with its offers, prices and quota.
    pub fn render(&self) -> String {
        let mut out = format!("source {} (store): every price 0; its versions are what it holds (abt data stats)\n", STORE);
        for s in &self.sources {
            out.push_str(&format!(
                "source {} ({:?}, adapter {}, {}){}\n",
                s.name,
                s.kind,
                s.adapter,
                s.freshness,
                s.throughput.map(|t| format!(", {:.0} B/s", t)).unwrap_or_default()
            ));
            for o in &s.offers {
                out.push_str(&format!(
                    "  offers {}.{{{}}} @{} from {} to {}, scope {}{}{}\n",
                    o.class,
                    o.relations.join(", "),
                    o.calendar,
                    o.from.as_deref().unwrap_or("the start"),
                    o.to.as_deref().unwrap_or("the present"),
                    o.scope,
                    if o.has_receipt { ", with receipt times" } else { "" },
                    if o.attributes.is_empty() {
                        String::new()
                    } else {
                        format!(", {}", o.attributes.iter().map(|(k, v)| format!("{} = {}", k, v)).collect::<Vec<_>>().join(", "))
                    }
                ));
            }
            for p in &s.price {
                let named = match &p.money {
                    Money::Named(n) => format!(" ({})", n),
                    Money::Value(_) => String::new(),
                };
                out.push_str(&format!("  price {} {} per {:?}{}\n", self.money(p), p.currency, p.unit, named));
            }
            if let Some(q) = &s.quota {
                out.push_str(&format!(
                    "  quota per {}: {} requests, {} bytes\n",
                    q.window,
                    q.requests.map(|r| r.to_string()).unwrap_or_else(|| "any".into()),
                    q.bytes.map(|b| b.to_string()).unwrap_or_else(|| "any".into())
                ));
            }
        }
        out
    }
}
