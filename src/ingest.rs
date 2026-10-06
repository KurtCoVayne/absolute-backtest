//! Vendor ingestion (data-bundle doc, sections 3, 8 and 10): adapters from
//! a vendor's export layout to the catalog's relations, behind `abt bundle
//! build --from-norgate DIR` and `--from-databento DIR`. Each adapter reads
//! plain CSV files in the layout documented on its function, maps vendor
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
use crate::kernel::time::{format_timestamp, parse_timestamp};
use crate::kernel::{Dataset, Value};

/// A CSV file as rows of named fields (header lower-cased, fields trimmed).
fn read_csv(path: &Path) -> Result<Vec<BTreeMap<String, String>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(header) = lines.next() else { return Ok(vec![]) };
    let names: Vec<String> = header.split(',').map(|h| h.trim().to_lowercase()).collect();
    let mut out = Vec::new();
    for (i, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split(',').map(|f| f.trim()).collect();
        if fields.len() < names.len() {
            return Err(format!("{}:{}: {} fields, {} expected", path.display(), i + 2, fields.len(), names.len()));
        }
        out.push(names.iter().cloned().zip(fields.iter().map(|f| f.to_string())).collect());
    }
    Ok(out)
}

fn field<'a>(row: &'a BTreeMap<String, String>, name: &str, path: &Path) -> Result<&'a str, String> {
    row.get(name).map(|s| s.as_str()).ok_or_else(|| format!("{}: header lacks column `{}`", path.display(), name))
}

fn number(row: &BTreeMap<String, String>, name: &str, path: &Path) -> Result<f64, String> {
    let raw = field(row, name, path)?;
    raw.parse().map_err(|_| format!("{}: `{}` is not a number in column `{}`", path.display(), raw, name))
}

fn date(row: &BTreeMap<String, String>, name: &str, path: &Path) -> Result<i64, String> {
    let raw = field(row, name, path)?;
    parse_timestamp(raw).ok_or_else(|| format!("{}: `{}` is not a timestamp in column `{}`", path.display(), raw, name))
}

/// The symbol history of a vendor: which identifier carried a symbol over
/// which dates. Read from `symbols.csv` (`id,symbol,from,to`, `to` empty
/// while current); without the file, the symbol is its own identifier.
struct SymbolHistory {
    rows: Vec<(String, String, i64, Option<i64>)>,
}

impl SymbolHistory {
    fn load(path: &Path) -> Result<SymbolHistory, String> {
        if !path.exists() {
            return Ok(SymbolHistory { rows: vec![] });
        }
        let mut rows = Vec::new();
        for row in read_csv(path)? {
            let to = field(&row, "to", path)?;
            rows.push((
                field(&row, "id", path)?.to_string(),
                field(&row, "symbol", path)?.to_string(),
                date(&row, "from", path)?,
                if to.is_empty() {
                    None
                } else {
                    Some(parse_timestamp(to).ok_or_else(|| format!("{}: `{}` is not a timestamp", path.display(), to))?)
                },
            ));
        }
        Ok(SymbolHistory { rows })
    }

    /// The identifier carrying `symbol` at `t`.
    fn id_at(&self, symbol: &str, t: i64) -> Result<String, String> {
        if self.rows.is_empty() {
            return Ok(symbol.to_string());
        }
        self.rows
            .iter()
            .find(|(_, s, from, to)| s == symbol && *from <= t && to.map(|x| t <= x).unwrap_or(true))
            .map(|(id, ..)| id.clone())
            .ok_or_else(|| format!("symbol `{}` is carried by no identifier at {} (symbols.csv)", symbol, format_timestamp(t)))
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
    /// Reviewed bundle-test exceptions, keyed by identifier (`exceptions.csv`).
    pub exceptions: Vec<crate::bundle::Exception>,
}

/// The Norgate-style daily layout (catalog v1, `equities_1d_v2`), one file
/// per table in `dir`, dates as `YYYY-MM-DD`:
///
/// - `prices.csv`: `symbol,date,open,high,low,close,volume`, as traded
///   (never adjusted; the library adjusts causally).
/// - `symbols.csv` (optional): `id,symbol,from,to`, the symbol history.
/// - `splits.csv` (optional): `symbol,ex_date,factor` (new shares per old).
/// - `dividends.csv` (optional): `symbol,announce_date,ex_date,pay_date,amount`.
/// - `delistings.csv` (optional): `symbol,date,reason`.
/// - `membership.csv` (optional): `symbol,index,from,to` (`to` empty while
///   current), expanded to every trading day of the price data.
/// - `classification.csv` (optional): `symbol,scheme,code,from,to`.
/// - `exceptions.csv` (optional): `test,symbol,date,reason`, problems a
///   person reviewed and accepted; the bundle keeps them by identifier.
///
/// `universe(A, T)` holds every security with a price on T that is not
/// delisted by T; `delisted` holds from the delisting date through the
/// last trading day of the data.
pub fn norgate_daily(dir: &Path, prog: &Program) -> Result<Ingested, String> {
    if prog.environment != "equities_1d_v2" {
        return Err(format!("the Norgate adapter produces `equities_1d_v2`; the program is written against `{}`", prog.environment));
    }
    let mut ds = Dataset::new();
    let history = SymbolHistory::load(&dir.join("symbols.csv"))?;
    history.install(&mut ds);
    let prices = dir.join("prices.csv");
    let rows = read_csv(&prices)?;
    if rows.is_empty() {
        return Err(format!("{}: no price rows", prices.display()));
    }
    let mut days: BTreeSet<i64> = BTreeSet::new();
    let mut priced: BTreeMap<i64, BTreeSet<u32>> = BTreeMap::new();
    for row in &rows {
        let t = date(row, "date", &prices)?;
        let id = history.id_at(field(row, "symbol", &prices)?, t)?;
        let sym = ds.intern(&id);
        for (col, rel) in [("open", "open"), ("high", "high"), ("low", "low"), ("close", "close")] {
            let p = number(row, col, &prices)?;
            if p <= 0.0 {
                return Err(format!("{}: non-positive {} {} for {} at {}", prices.display(), col, p, id, format_timestamp(t)));
            }
            ds.add(rel, vec![Value::Equity(sym), Value::Time(t), Value::Num(p)]);
        }
        ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(number(row, "volume", &prices)?)]);
        days.insert(t);
        priced.entry(t).or_default().insert(sym);
    }
    let splits = dir.join("splits.csv");
    if splits.exists() {
        for row in read_csv(&splits)? {
            let t = date(&row, "ex_date", &splits)?;
            let sym = ds.intern(&history.id_at(field(&row, "symbol", &splits)?, t)?);
            ds.add("split", vec![Value::Equity(sym), Value::Time(t), Value::Num(number(&row, "factor", &splits)?)]);
        }
    }
    let dividends = dir.join("dividends.csv");
    if dividends.exists() {
        for row in read_csv(&dividends)? {
            let announce = date(&row, "announce_date", &dividends)?;
            let ex = date(&row, "ex_date", &dividends)?;
            let pay = date(&row, "pay_date", &dividends)?;
            let sym = ds.intern(&history.id_at(field(&row, "symbol", &dividends)?, ex)?);
            ds.add(
                "dividend",
                vec![
                    Value::Equity(sym),
                    Value::Time(announce),
                    Value::Time(ex),
                    Value::Time(pay),
                    Value::Num(number(&row, "amount", &dividends)?),
                ],
            );
        }
    }
    let mut gone: BTreeMap<u32, i64> = BTreeMap::new();
    let delistings = dir.join("delistings.csv");
    if delistings.exists() {
        for row in read_csv(&delistings)? {
            let t = date(&row, "date", &delistings)?;
            let sym = ds.intern(&history.id_at(field(&row, "symbol", &delistings)?, t)?);
            let reason = ds.labels.intern(field(&row, "reason", &delistings)?);
            gone.insert(sym, t);
            for &d in days.range(t..) {
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
    let membership = dir.join("membership.csv");
    if membership.exists() {
        for row in read_csv(&membership)? {
            let from = date(&row, "from", &membership)?;
            let to_raw = field(&row, "to", &membership)?;
            let to = if to_raw.is_empty() {
                i64::MAX
            } else {
                parse_timestamp(to_raw).ok_or_else(|| format!("{}: `{}` is not a timestamp", membership.display(), to_raw))?
            };
            let index = ds.labels.intern(field(&row, "index", &membership)?);
            let symbol = field(&row, "symbol", &membership)?.to_string();
            for &d in days.range(from..=to) {
                let Ok(id) = history.id_at(&symbol, d) else { continue };
                let sym = ds.intern(&id);
                ds.add("member", vec![Value::Equity(sym), Value::Time(d), Value::Label(index)]);
            }
        }
    }
    let classification = dir.join("classification.csv");
    if classification.exists() {
        for row in read_csv(&classification)? {
            let from = date(&row, "from", &classification)?;
            let to_raw = field(&row, "to", &classification)?;
            let to = if to_raw.is_empty() {
                i64::MAX
            } else {
                parse_timestamp(to_raw).ok_or_else(|| format!("{}: `{}` is not a timestamp", classification.display(), to_raw))?
            };
            let scheme = ds.labels.intern(field(&row, "scheme", &classification)?);
            let code = ds.labels.intern(field(&row, "code", &classification)?);
            let symbol = field(&row, "symbol", &classification)?.to_string();
            for &d in days.range(from..=to) {
                let Ok(id) = history.id_at(&symbol, d) else { continue };
                let sym = ds.intern(&id);
                ds.add("classification", vec![Value::Equity(sym), Value::Time(d), Value::Label(scheme), Value::Label(code)]);
            }
        }
    }
    let mut exceptions = Vec::new();
    let path = dir.join("exceptions.csv");
    if path.exists() {
        for row in read_csv(&path)? {
            let t = date(&row, "date", &path)?;
            let reason = field(&row, "reason", &path)?;
            if reason.is_empty() {
                return Err(format!("{}: an exception needs a reason", path.display()));
            }
            exceptions.push(crate::bundle::Exception {
                test: field(&row, "test", &path)?.to_string(),
                security: history.id_at(field(&row, "symbol", &path)?, t)?,
                t,
                reason: reason.to_string(),
            });
        }
    }
    ds.derive_tickers();
    let mut notes = vec![format!("{} price rows over {} trading days; {} securities", rows.len(), days.len(), ds.symbols.len())];
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

/// The Databento-style minute layout (the catalog target, `equities_1m`):
///
/// - `ohlcv-1m.csv`: `ts_event,instrument_id,open,high,low,close,volume`
///   with an optional `ts_recv`, timestamps as `YYYY-MM-DDTHH:MM:SS` (UTC)
///   and prices as decimals (a raw fixed-point export is divided by 1e9
///   by the operator's projection). `ts_event` is the bar's open; the
///   bar's temporal key is its close, one minute later.
/// - `symbology.csv` (optional): `id,symbol,from,to`, Databento's
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
    let history = SymbolHistory::load(&dir.join("symbology.csv"))?;
    history.install(&mut ds);
    let path = dir.join("ohlcv-1m.csv");
    let rows = read_csv(&path)?;
    if rows.is_empty() {
        return Err(format!("{}: no bar rows", path.display()));
    }
    let recorded = rows[0].contains_key("ts_recv");
    for row in &rows {
        let open = date(row, "ts_event", &path)?;
        let t = open + 60;
        let id = history.id_at(field(row, "instrument_id", &path)?, t)?;
        let sym = ds.intern(&id);
        let close = number(row, "close", &path)?;
        if close <= 0.0 {
            return Err(format!("{}: non-positive close {} for {} at {}", path.display(), close, id, format_timestamp(t)));
        }
        let avail = if recorded { date(row, "ts_recv", &path)?.max(t) + processing_delay } else { t + processing_delay };
        let close_tuple = vec![Value::Equity(sym), Value::Time(t), Value::Num(close)];
        let volume_tuple = vec![Value::Equity(sym), Value::Time(t), Value::Num(number(row, "volume", &path)?)];
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
        rows.len(),
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
