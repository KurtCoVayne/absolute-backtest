//! A run as the query layer reads it (Q-3.1): the relations the kernel wrote
//! under `kernel.<run>.*` (K-7.1, P-4), read back through the data layer.
//! The metrics are pure functions of these tables.

use std::collections::BTreeMap;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Float64Type, Int64Type, TimestampNanosecondType, UInt64Type};

use crate::data::{Data, Error};

/// A row of the book at a decision barrier (K-5.1).
#[derive(Clone, Debug, PartialEq)]
pub struct BookRow {
    pub t: i64,
    pub cash: f64,
    pub nav: f64,
    pub gross: f64,
    pub net: f64,
    pub leverage: f64,
    pub receivables: f64,
    pub capital: f64,
}

/// A fill of the event log (K-5.2).
#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub seq: u64,
    pub t: i64,
    pub instrument: Option<u64>,
    pub quantity: f64,
    pub price: f64,
    pub commission: f64,
    pub fee: f64,
    pub slippage: f64,
    pub impact: f64,
    pub participation: f64,
    pub multiplier: f64,
}

/// A run's tables.
#[derive(Clone, Debug, Default)]
pub struct Run {
    pub id: String,
    pub book: Vec<BookRow>,
    pub fills: Vec<Fill>,
    pub decisions: usize,
    pub dropped: usize,
    /// The report's rows, their JSON values parsed.
    pub report: BTreeMap<String, serde_json::Value>,
}

fn concat(batches: Vec<RecordBatch>) -> Result<Option<RecordBatch>, Error> {
    let Some(schema) = batches.first().map(|b| b.schema()) else { return Ok(None) };
    arrow::compute::concat_batches(&schema, &batches).map(Some).map_err(|e| Error::Malformed(e.to_string()))
}

fn column<'a>(b: &'a RecordBatch, rel: &str, name: &str) -> Result<&'a dyn Array, Error> {
    b.column_by_name(name).map(|c| c.as_ref()).ok_or_else(|| Error::Malformed(format!("{} has no column `{}`", rel, name)))
}

fn nums(b: &RecordBatch, rel: &str, name: &str) -> Result<Vec<Option<f64>>, Error> {
    let c = column(b, rel, name)?;
    let a = c.as_primitive_opt::<Float64Type>().ok_or_else(|| Error::Malformed(format!("{}.{} is not a number", rel, name)))?;
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

fn times(b: &RecordBatch, rel: &str, name: &str) -> Result<Vec<i64>, Error> {
    let c = column(b, rel, name)?;
    let a = c
        .as_primitive_opt::<TimestampNanosecondType>()
        .ok_or_else(|| Error::Malformed(format!("{}.{} is not a time", rel, name)))?;
    Ok((0..a.len()).map(|i| a.value(i)).collect())
}

fn ids(b: &RecordBatch, rel: &str, name: &str) -> Result<Vec<Option<u64>>, Error> {
    let c = column(b, rel, name)?;
    let a = c.as_primitive_opt::<UInt64Type>().ok_or_else(|| Error::Malformed(format!("{}.{} is not an entity", rel, name)))?;
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

#[allow(dead_code)]
fn counts(b: &RecordBatch, rel: &str, name: &str) -> Result<Vec<Option<i64>>, Error> {
    let c = column(b, rel, name)?;
    let a = c.as_primitive_opt::<Int64Type>().ok_or_else(|| Error::Malformed(format!("{}.{} is not a count", rel, name)))?;
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

/// A label or text column's values (a label reads as a dictionary).
fn texts(b: &RecordBatch, rel: &str, name: &str) -> Result<Vec<Option<String>>, Error> {
    let c = column(b, rel, name)?;
    let s = match c.data_type() {
        DataType::Binary => {
            let a = c.as_binary::<i32>();
            return Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| String::from_utf8_lossy(a.value(i)).into_owned())).collect());
        }
        _ => arrow::compute::cast(c, &DataType::Utf8).map_err(|e| Error::Malformed(format!("{}.{}: {}", rel, name, e)))?,
    };
    let a = s.as_string::<i32>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i).to_string())).collect())
}

impl Run {
    /// The run's tables from the store.
    pub fn read(data: &Data, run: &str) -> Result<Run, Error> {
        let rel = |name: &str| format!("kernel.{}.{}", run, name);
        let mut out = Run {
            id: run.to_string(),
            ..Default::default()
        };
        // The report first: a run that wrote none is not in the store.
        let report = concat(data.scan(&rel("report"))?)?.ok_or_else(|| Error::Missing(format!("the store holds no run {}", run)))?;
        let r = rel("report");
        let keys = texts(&report, &r, "key")?;
        let values = texts(&report, &r, "value")?;
        for (k, v) in keys.into_iter().zip(values) {
            if let (Some(k), Some(v)) = (k, v) {
                let parsed = serde_json::from_str(&v).map_err(|e| Error::Malformed(format!("{}: `{}` is not JSON: {}", r, k, e)))?;
                out.report.insert(k, parsed);
            }
        }
        if let Some(b) = concat(data.scan(&rel("book"))?)? {
            let r = rel("book");
            let (t, cash, nav, gross, net, leverage, receivables, capital) = (
                times(&b, &r, "t")?,
                nums(&b, &r, "cash")?,
                nums(&b, &r, "nav")?,
                nums(&b, &r, "gross")?,
                nums(&b, &r, "net")?,
                nums(&b, &r, "leverage")?,
                nums(&b, &r, "receivables")?,
                nums(&b, &r, "capital")?,
            );
            for i in 0..t.len() {
                out.book.push(BookRow {
                    t: t[i],
                    cash: cash[i].unwrap_or(0.0),
                    nav: nav[i].unwrap_or(0.0),
                    gross: gross[i].unwrap_or(0.0),
                    net: net[i].unwrap_or(0.0),
                    leverage: leverage[i].unwrap_or(0.0),
                    receivables: receivables[i].unwrap_or(0.0),
                    capital: capital[i].unwrap_or(0.0),
                });
            }
        }
        if let Some(b) = concat(data.scan(&rel("fills"))?)? {
            let r = rel("fills");
            let (seq, t, ins, q, p, c, f, s, im, pa) = (
                ids(&b, &r, "seq")?,
                times(&b, &r, "t")?,
                ids(&b, &r, "instrument")?,
                nums(&b, &r, "quantity")?,
                nums(&b, &r, "price")?,
                nums(&b, &r, "commission")?,
                nums(&b, &r, "fee")?,
                nums(&b, &r, "slippage")?,
                nums(&b, &r, "impact")?,
                nums(&b, &r, "participation")?,
            );
            let mult = nums(&b, &r, "multiplier").unwrap_or_else(|_| vec![Some(1.0); t.len()]);
            for i in 0..t.len() {
                out.fills.push(Fill {
                    seq: seq[i].unwrap_or(0),
                    t: t[i],
                    instrument: ins[i],
                    quantity: q[i].unwrap_or(0.0),
                    price: p[i].unwrap_or(0.0),
                    commission: c[i].unwrap_or(0.0),
                    fee: f[i].unwrap_or(0.0),
                    slippage: s[i].unwrap_or(0.0),
                    impact: im[i].unwrap_or(0.0),
                    participation: pa[i].unwrap_or(0.0),
                    multiplier: mult[i].unwrap_or(1.0),
                });
            }
            out.fills.sort_by_key(|f| f.seq);
        }
        out.decisions = concat(data.scan(&rel("decisions"))?)?.map(|b| b.num_rows()).unwrap_or(0);
        if let Some(b) = concat(data.scan(&rel("events"))?)? {
            out.dropped = texts(&b, &rel("events"), "kind")?.iter().filter(|k| k.as_deref() == Some("dropped")).count();
        }
        Ok(out)
    }

    /// The NAV at every decision barrier: the equity curve the metrics read.
    pub fn curve(&self) -> Vec<(i64, f64)> {
        self.book.iter().map(|b| (b.t, b.nav)).collect()
    }

    /// The report's number under `key`.
    pub fn number(&self, key: &str) -> Option<f64> {
        self.report.get(key).and_then(|v| v.as_f64())
    }

    /// A field of one of the report's summaries (`costs`, `liquidity`,
    /// `funding`), 0 when absent.
    pub fn figure(&self, summary: &str, key: &str) -> f64 {
        self.report.get(summary).and_then(|s| s.get(key)).and_then(|v| v.as_f64()).unwrap_or(0.0)
    }

    /// The periods a year the run's report block states.
    pub fn periods_per_year(&self) -> f64 {
        self.number("periods_per_year").unwrap_or(252.0)
    }

    /// How the report block's calendar partitions the run (K-7.1, E-58):
    /// per calendar day or per decision bar, and the days of a sessions
    /// calendar.
    pub fn periods(&self) -> (bool, Option<std::collections::BTreeSet<i64>>) {
        let by_day = self.report.get("period_by_day").and_then(|v| v.as_bool()).unwrap_or(false);
        let days = self.report.get("period_days").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|d| d.as_i64()).collect());
        (by_day, days)
    }

    /// The fixed capital a period's return is over (no compounding), or
    /// `None` when returns compound.
    pub fn base_capital(&self) -> Option<f64> {
        self.number("base_capital")
    }

    /// The return of each decision bar over the last: over the fixed
    /// capital, or over the previous NAV when returns compound.
    pub fn bar_returns(&self) -> Vec<(i64, f64)> {
        let base_capital = self.base_capital();
        self.book
            .windows(2)
            .map(|w| {
                let base = base_capital.unwrap_or(w[0].nav);
                (w[1].t, if base > 0.0 { (w[1].nav - w[0].nav) / base } else { 0.0 })
            })
            .collect()
    }

    /// The curve the metrics read: the NAV when returns compound, otherwise
    /// the capital compounded by the fixed-base bar returns.
    pub fn metric_curve(&self) -> Vec<(i64, f64)> {
        let Some(base) = self.base_capital() else { return self.curve() };
        let Some(t0) = self.book.first().map(|b| b.t) else { return vec![] };
        let mut e = base;
        let mut out = vec![(t0, e)];
        for (t, r) in self.bar_returns() {
            e *= 1.0 + r;
            out.push((t, e));
        }
        out
    }
}
