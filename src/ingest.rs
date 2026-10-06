//! Vendor ingestion (data-bundle doc, sections 3, 8 and 10): adapters from
//! a vendor's export layout to the catalog's relations, behind `abt bundle
//! build --from-norgate DIR` and `--from-databento DIR`. Each adapter reads
//! Parquet files (`table`) in the layout documented on its function, maps vendor
//! symbols to stable security identifiers through a symbol-history table,
//! and records the schema decisions of section 10 in the manifest
//! (consolidated bars; the @1d availability offset as a processing delay).
//!
//! The vendors' own export formats vary by product and account; these
//! adapters fix one plain layout each, close to the vendors' documented
//! column names, and a conversion from a vendor's native export to it is
//! a one-line projection the operator owns.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::check::Program;
use crate::kernel::time::format_timestamp;
use crate::kernel::{Dataset, Value};
use crate::table::Table;

/// A table's file in `dir`: `<stem>.parquet`.
fn file(dir: &Path, stem: &str) -> std::path::PathBuf {
    dir.join(format!("{}.parquet", stem))
}

/// The symbol history of a vendor: which identifier carried a symbol over
/// which dates. Read from `symbols.parquet` (`id,symbol,from,to`, `to` null
/// while current); without the file, the symbol is its own identifier.
struct SymbolHistory {
    rows: Vec<(String, String, i64, Option<i64>)>,
    /// The rows of each symbol, so a lookup scans one symbol's history.
    by_symbol: BTreeMap<String, Vec<usize>>,
}

impl SymbolHistory {
    fn load(path: &Path) -> Result<SymbolHistory, String> {
        if !path.exists() {
            return Ok(SymbolHistory {
                rows: vec![],
                by_symbol: BTreeMap::new(),
            });
        }
        let t = Table::read(path)?;
        let (ci, cs, cf, ct) = (t.col("id")?, t.col("symbol")?, t.col("from")?, t.col("to")?);
        let mut rows = Vec::with_capacity(t.len());
        for r in 0..t.len() {
            rows.push((t.string(ci, r)?, t.string(cs, r)?, t.time(cf, r)?, t.time_opt(ct, r)?));
        }
        let mut by_symbol: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, row) in rows.iter().enumerate() {
            by_symbol.entry(row.1.clone()).or_default().push(i);
        }
        Ok(SymbolHistory { rows, by_symbol })
    }

    /// The identifier carrying `symbol` at `t`.
    fn id_at(&self, symbol: &str, t: i64) -> Result<String, String> {
        if self.rows.is_empty() {
            return Ok(symbol.to_string());
        }
        self.by_symbol
            .get(symbol)
            .into_iter()
            .flatten()
            .map(|&i| &self.rows[i])
            .find(|(_, _, from, to)| *from <= t && to.map(|x| t <= x).unwrap_or(true))
            .map(|(id, ..)| id.clone())
            .ok_or_else(|| format!("symbol `{}` is carried by no identifier at {} (symbol history)", symbol, format_timestamp(t)))
    }

    fn install(&self, ds: &mut Dataset) {
        for (id, symbol, from, to) in &self.rows {
            ds.add_security(id, symbol, *from, *to);
        }
    }
}

/// What an adapter produced, with the notes it wants in the manifest.
pub struct Ingested {
    pub dataset: Dataset,
    /// The schema decisions taken (section 10), for the manifest's `source`.
    pub source: String,
    pub notes: Vec<String>,
    /// Reviewed bundle-test exceptions, keyed by identifier (`exceptions`).
    pub exceptions: Vec<crate::bundle::Exception>,
}

/// The Norgate-style daily layout (catalog v1, `equities_1d_v2`), one
/// Parquet file per table in `dir`; dates are date or timestamp columns (or
/// `YYYY-MM-DD` text):
///
/// - `prices`: `symbol,date,open,high,low,close,volume`, as traded
///   (never adjusted; the library adjusts causally).
/// - `symbols` (optional): `id,symbol,from,to`, the symbol history.
/// - `splits` (optional): `symbol,ex_date,factor` (new shares per old).
/// - `dividends` (optional): `symbol,announce_date,ex_date,pay_date,amount`.
/// - `delistings` (optional): `symbol,date,reason`.
/// - `membership` (optional): `symbol,index,from,to` (`to` null while
///   current), expanded to every trading day of the price data.
/// - `classification` (optional): `symbol,scheme,code,from,to`.
/// - `exceptions` (optional): `test,symbol,date,reason`, problems a person
///   reviewed and accepted; the bundle keeps them by identifier.
///
/// `universe(A, T)` holds every security with a price on T that is not
/// delisted by T; `delisted` holds from the delisting date through the
/// last trading day of the data.
pub fn norgate_daily(dir: &Path, prog: &Program) -> Result<Ingested, String> {
    if prog.environment != "equities_1d_v2" {
        return Err(format!("the Norgate adapter produces `equities_1d_v2`; the program is written against `{}`", prog.environment));
    }
    let mut ds = Dataset::new();
    let history = SymbolHistory::load(&file(dir, "symbols"))?;
    history.install(&mut ds);
    let prices = Table::read(&file(dir, "prices"))?;
    if prices.is_empty() {
        return Err(format!("{}: no price rows", prices.path().display()));
    }
    let (cs, cd, cv) = (prices.col("symbol")?, prices.col("date")?, prices.col("volume")?);
    let ohlc = [
        ("open", prices.col("open")?),
        ("high", prices.col("high")?),
        ("low", prices.col("low")?),
        ("close", prices.col("close")?),
    ];
    let mut days: BTreeSet<i64> = BTreeSet::new();
    let mut priced: BTreeMap<i64, BTreeSet<u32>> = BTreeMap::new();
    for r in 0..prices.len() {
        let t = prices.time(cd, r)?;
        let id = history.id_at(&prices.string(cs, r)?, t)?;
        let sym = ds.intern(&id);
        for (rel, c) in ohlc {
            let p = prices.number(c, r)?;
            if p <= 0.0 {
                return Err(format!("{}: non-positive {} {} for {} at {}", prices.at(r), rel, p, id, format_timestamp(t)));
            }
            ds.add(rel, vec![Value::Equity(sym), Value::Time(t), Value::Num(p)]);
        }
        ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(prices.number(cv, r)?)]);
        days.insert(t);
        priced.entry(t).or_default().insert(sym);
    }
    let n_prices = prices.len();
    drop(prices);
    if let Some(t) = optional(dir, "splits")? {
        let (cs, ce, cf) = (t.col("symbol")?, t.col("ex_date")?, t.col("factor")?);
        for r in 0..t.len() {
            let ex = t.time(ce, r)?;
            let sym = ds.intern(&history.id_at(&t.string(cs, r)?, ex)?);
            ds.add("split", vec![Value::Equity(sym), Value::Time(ex), Value::Num(t.number(cf, r)?)]);
        }
    }
    if let Some(t) = optional(dir, "dividends")? {
        let (cs, ca, ce, cp, cm) = (t.col("symbol")?, t.col("announce_date")?, t.col("ex_date")?, t.col("pay_date")?, t.col("amount")?);
        for r in 0..t.len() {
            let ex = t.time(ce, r)?;
            let sym = ds.intern(&history.id_at(&t.string(cs, r)?, ex)?);
            ds.add(
                "dividend",
                vec![
                    Value::Equity(sym),
                    Value::Time(t.time(ca, r)?),
                    Value::Time(ex),
                    Value::Time(t.time(cp, r)?),
                    Value::Num(t.number(cm, r)?),
                ],
            );
        }
    }
    let mut gone: BTreeMap<u32, i64> = BTreeMap::new();
    if let Some(t) = optional(dir, "delistings")? {
        let (cs, cd, cr) = (t.col("symbol")?, t.col("date")?, t.col("reason")?);
        for r in 0..t.len() {
            let d0 = t.time(cd, r)?;
            let sym = ds.intern(&history.id_at(&t.string(cs, r)?, d0)?);
            let reason = ds.labels.intern(&t.string(cr, r)?);
            gone.insert(sym, d0);
            for &d in days.range(d0..) {
                ds.add("delisted", vec![Value::Equity(sym), Value::Time(d), Value::Label(reason)]);
            }
        }
    }
    for (&t, syms) in &priced {
        for &sym in syms {
            if gone.get(&sym).map(|g| t >= *g).unwrap_or(false) {
                continue;
            }
            ds.add("universe", vec![Value::Equity(sym), Value::Time(t)]);
        }
    }
    if let Some(t) = optional(dir, "membership")? {
        let (cs, ci, cf, ct) = (t.col("symbol")?, t.col("index")?, t.col("from")?, t.col("to")?);
        for r in 0..t.len() {
            let (from, to) = (t.time(cf, r)?, t.time_opt(ct, r)?.unwrap_or(i64::MAX));
            let index = ds.labels.intern(&t.string(ci, r)?);
            let symbol = t.string(cs, r)?;
            for &d in days.range(from..=to) {
                let Ok(id) = history.id_at(&symbol, d) else { continue };
                let sym = ds.intern(&id);
                ds.add("member", vec![Value::Equity(sym), Value::Time(d), Value::Label(index)]);
            }
        }
    }
    if let Some(t) = optional(dir, "classification")? {
        let (cs, csc, cc, cf, ct) = (t.col("symbol")?, t.col("scheme")?, t.col("code")?, t.col("from")?, t.col("to")?);
        for r in 0..t.len() {
            let (from, to) = (t.time(cf, r)?, t.time_opt(ct, r)?.unwrap_or(i64::MAX));
            let scheme = ds.labels.intern(&t.string(csc, r)?);
            let code = ds.labels.intern(&t.string(cc, r)?);
            let symbol = t.string(cs, r)?;
            for &d in days.range(from..=to) {
                let Ok(id) = history.id_at(&symbol, d) else { continue };
                let sym = ds.intern(&id);
                ds.add("classification", vec![Value::Equity(sym), Value::Time(d), Value::Label(scheme), Value::Label(code)]);
            }
        }
    }
    let mut exceptions = Vec::new();
    if let Some(t) = optional(dir, "exceptions")? {
        let (ct, cs, cd, cr) = (t.col("test")?, t.col("symbol")?, t.col("date")?, t.col("reason")?);
        for r in 0..t.len() {
            let d = t.time(cd, r)?;
            let reason = t.text(cr, r).ok_or_else(|| format!("{}: an exception needs a reason", t.at(r)))?;
            exceptions.push(crate::bundle::Exception {
                test: t.string(ct, r)?,
                security: history.id_at(&t.string(cs, r)?, d)?,
                t: d,
                reason,
            });
        }
    }
    ds.derive_tickers();
    let mut notes = vec![format!("{} price rows over {} trading days; {} securities", n_prices, days.len(), ds.symbols.len())];
    if !exceptions.is_empty() {
        notes.push(format!("{} reviewed bundle-test exceptions", exceptions.len()));
    }
    Ok(Ingested {
        dataset: ds,
        source: "norgate daily export: OHLCV as traded, actions as events, membership and classification expanded to trading days; availability at bar close".into(),
        notes,
        exceptions,
    })
}

/// An optional table of `dir`: `None` without its file.
fn optional(dir: &Path, stem: &str) -> Result<Option<Table>, String> {
    let path = file(dir, stem);
    if path.exists() {
        Table::read(&path).map(Some)
    } else {
        Ok(None)
    }
}

/// The Databento-style minute layout (the catalog target, `equities_1m`),
/// Parquet files in `dir`:
///
/// - `ohlcv-1m`: `ts_event,instrument_id,open,high,low,close,volume` with
///   an optional `ts_recv`, timestamps as timestamp columns (UTC) or text
///   `YYYY-MM-DDTHH:MM:SS`, and prices as decimals (a raw fixed-point
///   export is divided by 1e9 by the operator's projection). `ts_event` is
///   the bar's open; the bar's temporal key is its close, one minute later.
/// - `symbology` (optional): `id,symbol,from,to`, Databento's
///   `instrument_id` history mapped onto the system's identifiers; without
///   it the instrument id is the identifier.
///
/// Availability (section 10, item 4): `ts_recv` plus `processing_delay`
/// seconds when the column exists, recorded per tuple; otherwise the bar
/// close plus the delay. The bars are taken as consolidated (section 10,
/// item 3) and the manifest says so.
pub fn databento_minute(dir: &Path, prog: &Program, processing_delay: i64) -> Result<Ingested, String> {
    if prog.environment != "equities_1m" {
        return Err(format!("the Databento adapter produces `equities_1m`; the program is written against `{}`", prog.environment));
    }
    let mut ds = Dataset::new();
    let history = SymbolHistory::load(&file(dir, "symbology"))?;
    history.install(&mut ds);
    let bars = Table::read(&file(dir, "ohlcv-1m"))?;
    if bars.is_empty() {
        return Err(format!("{}: no bar rows", bars.path().display()));
    }
    let (ce, ci, cc, cv) = (bars.col("ts_event")?, bars.col("instrument_id")?, bars.col("close")?, bars.col("volume")?);
    let recv = bars.col("ts_recv").ok();
    let recorded = recv.is_some();
    for r in 0..bars.len() {
        let t = bars.time(ce, r)? + 60;
        let id = history.id_at(&bars.string(ci, r)?, t)?;
        let sym = ds.intern(&id);
        let close = bars.number(cc, r)?;
        if close <= 0.0 {
            return Err(format!("{}: non-positive close {} for {} at {}", bars.at(r), close, id, format_timestamp(t)));
        }
        let avail = match recv {
            Some(c) => bars.time(c, r)?.max(t) + processing_delay,
            None => t + processing_delay,
        };
        let close_tuple = vec![Value::Equity(sym), Value::Time(t), Value::Num(close)];
        let volume_tuple = vec![Value::Equity(sym), Value::Time(t), Value::Num(bars.number(cv, r)?)];
        let universe_tuple = vec![Value::Equity(sym), Value::Time(t)];
        if recorded || processing_delay > 0 {
            ds.add_available("close_m", close_tuple, avail);
            ds.add_available("volume_m", volume_tuple, avail);
            ds.add_available("universe_m", universe_tuple, avail);
        } else {
            ds.add("close_m", close_tuple);
            ds.add("volume_m", volume_tuple);
            ds.add("universe_m", universe_tuple);
        }
    }
    ds.derive_tickers();
    let notes = vec![format!(
        "{} minute bars; {} securities; availability {}{}",
        bars.len(),
        ds.symbols.len(),
        if recorded { "from ts_recv" } else { "at bar close" },
        if processing_delay > 0 {
            format!(" plus {} s of processing delay", processing_delay)
        } else {
            String::new()
        }
    )];
    Ok(Ingested {
        dataset: ds,
        source: format!(
            "databento ohlcv-1m export, consolidated bars keyed at the bar close; availability {}{}",
            if recorded { "ts_recv per tuple" } else { "bar close" },
            if processing_delay > 0 { format!(" plus {} s", processing_delay) } else { String::new() }
        ),
        notes,
        exceptions: vec![],
    })
}
