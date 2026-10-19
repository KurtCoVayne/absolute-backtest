//! The Databento adapter (D-7.7): historical data as DBN (Databento's
//! binary encoding, `adapters::dbn`; a `.dbn.zst` file decompressed first),
//! as the Parquet files of the company's bucket (`docs/handoff-data.md`,
//! section 3: `GLBX.MDP3/ohlcv-1m/<ROOT>.FUT/<date>.parquet` and
//! `definition/<ROOT>.parquet`), or fetched from the historical API
//! (`timeseries.get_range`, `adapters::http`). The live gateway is
//! `adapters::databento_live`.
//!
//! Bars become the class's `open`, `high`, `low`, `close` and `volume`,
//! keyed at the bar's close: an OHLCV record's `ts_event` is the start of
//! its interval, so `t = ts_event + interval`. A bar is received at
//! `ts_recv` when its rows carry one (the bar's close when that is later),
//! else at its close, plus the configured processing delay (B-10). A bar is
//! not complete before it closes, so `ts_event + delay` alone would be read
//! before the bar ended (ERRATA E-68).
//!
//! The identity rules (D-2.4):
//!
//! - **Futures** (`class = future`) are contracts by `instrument_id`, the
//!   root (`asset`) from the instrument's definition: a bar of an instrument
//!   without a definition is refused, naming it. Spreads and options of a
//!   parent symbol are left out (counted in the report). Each root's
//!   contracts are rolled into one back-adjusted continuous series, the
//!   entity of the class's bars, keyed by Databento's continuous symbol
//!   (`ES.v.0` rolled by volume, `ES.c.0` by expiry):
//!   - the root's sessions are its bars' runs, a gap longer than
//!     `session_gap` (45 minutes: CME Globex's daily break is an hour)
//!     ending one; a session is labelled at its last bar's close and opens
//!     at its first bar's start (`session(A, S, Open)`);
//!   - by volume, a session holds the contract that traded the most in the
//!     session before, never one expiring before the contract held; by
//!     expiry, the contract nearest its expiry that trades in the session;
//!   - at a roll from O to N, the gap is N's last close minus O's last close
//!     in the session before (N's first open in the roll's session when N
//!     did not trade before it); the series before the roll is shifted by
//!     the later gaps, so its last contract is unadjusted and a roll bar's
//!     change is the new contract's (K-5.4);
//!   - `continuous(root, t, contract, adjustment)` names the contract each
//!     bar of the series is of and the amount added to its prices;
//!   - `future_spec` holds each contract's terms: `first_trade` its
//!     activation, `expiry` and `last_trade` its expiration, `tick` its
//!     minimum increment, `point_value` its `unit_of_measure_qty` unless the
//!     configuration gives the root's (a bond's points are a percent of its
//!     size), `settlement` as configured per root, else `unstated`;
//!   - `point_value` is stated per root at each session's day, and
//!     `commission` when the configuration gives one.
//! - **Equities** (`class = equity`) are named by `raw_symbol` (the
//!   metadata's symbol mappings, or the Parquet's `symbol`) and joined to
//!   the instruments whose tickers carry the symbol at the bar's date (a
//!   Norgate asset, D-2.4): a symbol no ticker span carries is refused,
//!   naming the row (A-D24).
//!
//! Statistics (settlement prices, open interest) are decoded and not yet
//! stored: the future class gains `settlement` and `open_interest` with the
//! futures model (M-7, ERRATA E-68).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arrow::array::{Array, ArrayRef, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Float64Type, Int64Type, Schema, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use super::dbn::{self, Decoded, Record};
use super::{Adapter, FetchRequest, Fetched, Keys};
use crate::adapter::{Security, Terms};
use crate::error::DataError;
use crate::resolver::catalog::Offer;

pub const DAY: i64 = 86_400_000_000_000;
const SECOND: i64 = 1_000_000_000;
const MINUTE: i64 = 60 * SECOND;

/// How a root's continuous series moves from contract to contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roll {
    /// The contract that traded the most in the session before (`.v.0`).
    Volume,
    /// The contract nearest its expiry (`.c.0`).
    Expiry,
}

impl Roll {
    fn code(self) -> &'static str {
        match self {
            Roll::Volume => "v",
            Roll::Expiry => "c",
        }
    }
}

/// What the adapter makes of the data.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// `future` or `equity`.
    pub class: String,
    /// Added to every row's receipt time (B-10), in nanoseconds.
    pub processing_delay: i64,
    pub roll: Roll,
    /// The longest gap within a session, in nanoseconds.
    pub session_gap: i64,
    /// A root's currency per point, where its contracts' size is not it.
    pub point_values: BTreeMap<String, f64>,
    /// A root's settlement (`cash`, `physical`).
    pub settlement: BTreeMap<String, String>,
    /// The commission per contract per side, for every root.
    pub commission: Option<f64>,
    /// The bars kept: those closing in `[from, to]`, in nanoseconds.
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// A bar whose instrument has no definition: refused (D-2.4, the
    /// default) or dropped and counted (`dropped.no_definition`), for
    /// files whose definitions do not cover every instrument they hold.
    pub undefined: Undefined,
}

/// What a bar without a definition does to an ingest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Undefined {
    #[default]
    Refuse,
    Drop,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            class: "future".into(),
            processing_delay: 0,
            roll: Roll::Volume,
            session_gap: 45 * MINUTE,
            point_values: BTreeMap::new(),
            settlement: BTreeMap::new(),
            commission: None,
            from: None,
            to: None,
            undefined: Undefined::Refuse,
        }
    }
}

/// What a read did.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Report {
    pub files: usize,
    pub bars_in: usize,
    pub bars_out: usize,
    pub definitions: usize,
    pub first: Option<String>,
    pub last: Option<String>,
    /// Rows left out, by why.
    pub dropped: BTreeMap<String, usize>,
    /// Per root: its contracts, sessions and rolls (`2024-03-08 ESH4 -> ESM4, gap 12.25`).
    pub roots: BTreeMap<String, RootReport>,
    /// The symbols of an equity read.
    pub symbols: usize,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct RootReport {
    pub contracts: usize,
    pub sessions: usize,
    pub rolls: Vec<String>,
}

/// A statistic the venue published: a settlement price (`StatType` 3) or
/// an open interest (9), for the trading day of its reference time.
#[derive(Clone, Debug)]
struct Stat {
    id: u32,
    day: i64,
    recv: i64,
    kind: u16,
    value: f64,
}

const STAT_SETTLEMENT: u16 = 3;
const STAT_OPEN_INTEREST: u16 = 9;

/// One bar as read: its instrument, the start of its interval and its
/// close, its prices, and what names it.
#[derive(Clone, Debug, PartialEq)]
struct Bar {
    id: u32,
    start: i64,
    t: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    recv: Option<i64>,
    symbol: Option<String>,
}

/// A definition's facts the adapter uses.
#[derive(Clone, Debug, PartialEq)]
struct Def {
    raw_symbol: String,
    root: String,
    class: u8,
    activation: Option<i64>,
    expiration: Option<i64>,
    tick: Option<f64>,
    size: Option<f64>,
    currency: String,
    recv: i64,
}

/// What files or responses held, decoded.
#[derive(Clone, Debug, Default)]
pub struct Raw {
    bars: Vec<Bar>,
    defs: BTreeMap<u32, Def>,
    /// The venue's settlement prices and open interest (`statistics`).
    stats: Vec<Stat>,
    /// `(instrument, from, to, symbol)`: an instrument's symbol over
    /// `[from, to)`.
    mappings: Vec<(u32, i64, i64, String)>,
    dataset: Option<String>,
    files: usize,
}

fn ts(x: u64) -> Option<i64> {
    (x != dbn::UNDEF_TIMESTAMP).then_some(x as i64)
}

/// A bar's interval from its record type.
fn interval_of(rtype: u8) -> Option<i64> {
    match rtype {
        dbn::rtype::OHLCV_1S => Some(SECOND),
        dbn::rtype::OHLCV_1M => Some(MINUTE),
        dbn::rtype::OHLCV_1H => Some(60 * MINUTE),
        dbn::rtype::OHLCV_1D => Some(DAY),
        _ => None,
    }
}

/// A bar's interval from a stream's schema (DBN's `Schema`: `ohlcv-1s` is
/// 5, `ohlcv-1m` 6, `ohlcv-1h` 7, `ohlcv-1d` 8).
fn schema_interval(schema: u16) -> Option<i64> {
    match schema {
        5 => Some(SECOND),
        6 => Some(MINUTE),
        7 => Some(60 * MINUTE),
        8 => Some(DAY),
        _ => None,
    }
}

/// The calendar label of an interval.
fn calendar_of(interval: i64) -> String {
    match interval {
        SECOND => "1s".into(),
        MINUTE => "1m".into(),
        x if x == 60 * MINUTE => "1h".into(),
        DAY => "1d".into(),
        x if x % MINUTE == 0 => format!("{}m", x / MINUTE),
        x => format!("{}s", x / SECOND),
    }
}

/// The nanoseconds of a `YYYYMMDD` date.
fn yyyymmdd(d: u32) -> i64 {
    abt_wire::scalar::days_from_civil((d / 10_000) as i64, d / 100 % 100, d % 100) * DAY
}

fn iso(t: i64) -> String {
    abt_wire::scalar::Time(t).iso()
}

fn day_of(t: i64) -> String {
    let (y, m, d) = abt_wire::scalar::civil_from_days(t.div_euclid(DAY));
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// The bytes of a file or a response, a Zstandard frame decompressed.
pub fn unpack(bytes: Vec<u8>) -> Result<Vec<u8>, DataError> {
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        let mut out = Vec::new();
        zstd::stream::read::Decoder::new(&bytes[..])?.read_to_end(&mut out)?;
        return Ok(out);
    }
    Ok(bytes)
}

impl Raw {
    /// Adds a DBN stream's records.
    pub fn add_dbn(&mut self, bytes: &[u8], what: &str) -> Result<(), DataError> {
        let (meta, records) = dbn::decode(bytes).map_err(|e| DataError::Malformed(format!("{}: {}", what, e)))?;
        self.files += 1;
        self.dataset.get_or_insert(meta.dataset.clone());
        // The metadata's mappings: a requested symbol to instrument ids by
        // date (stype_out = instrument_id), or the reverse.
        for m in &meta.mappings {
            for iv in &m.intervals {
                let (from, to) = (yyyymmdd(iv.start_date), yyyymmdd(iv.end_date));
                if let Ok(id) = iv.symbol.parse::<u32>() {
                    self.mappings.push((id, from, to, m.raw_symbol.clone()));
                } else if let Ok(id) = m.raw_symbol.parse::<u32>() {
                    self.mappings.push((id, from, to, iv.symbol.clone()));
                }
            }
        }
        let schema_step = meta.schema.and_then(schema_interval);
        self.add_records_with(&records, schema_step);
        Ok(())
    }

    /// Adds decoded records (a file's, or a live session's).
    pub fn add_records(&mut self, records: &[Decoded]) {
        self.add_records_with(records, None)
    }

    /// [`Raw::add_records`], a bar of the deprecated record type of the
    /// interval `step` (the stream's schema).
    fn add_records_with(&mut self, records: &[Decoded], step: Option<i64>) {
        for d in records {
            match &d.record {
                Record::Ohlcv { hd, open, high, low, close, volume } => {
                    let start = hd.ts_event as i64;
                    let interval = if hd.rtype == dbn::rtype::OHLCV_DEPRECATED { step } else { interval_of(hd.rtype) };
                    let (start, t) = match interval {
                        Some(i) => (start, start + i),
                        // A session bar is stamped at the session's end.
                        None => (start - DAY, start),
                    };
                    let p = |x: i64| dbn::price(x).unwrap_or(f64::NAN);
                    self.bars.push(Bar {
                        id: hd.instrument_id,
                        start,
                        t,
                        open: p(*open),
                        high: p(*high),
                        low: p(*low),
                        close: p(*close),
                        volume: *volume as f64,
                        recv: None,
                        symbol: None,
                    });
                }
                Record::InstrumentDef(def) => {
                    if def.security_update_action == b'D' {
                        continue;
                    }
                    let recv = ts(def.ts_recv).or(ts(def.hd.ts_event)).unwrap_or(i64::MIN);
                    let fresh = Def {
                        raw_symbol: def.raw_symbol.clone(),
                        root: def.asset.clone(),
                        class: def.instrument_class,
                        activation: ts(def.activation),
                        expiration: ts(def.expiration),
                        tick: dbn::price(def.min_price_increment),
                        size: dbn::price(def.unit_of_measure_qty),
                        currency: def.currency.clone(),
                        recv,
                    };
                    match self.defs.get(&def.hd.instrument_id) {
                        Some(old) if old.recv > recv => {}
                        _ => {
                            self.defs.insert(def.hd.instrument_id, fresh);
                        }
                    }
                }
                Record::SymbolMapping {
                    hd,
                    stype_in_symbol,
                    start_ts,
                    end_ts,
                    ..
                } => {
                    let to = ts(*end_ts).unwrap_or(i64::MAX);
                    self.mappings.push((hd.instrument_id, ts(*start_ts).unwrap_or(i64::MIN), to, stype_in_symbol.clone()));
                }
                // A statistic new or updated (2 deletes one, left out).
                Record::Statistics {
                    hd,
                    ts_recv,
                    ts_ref,
                    price,
                    quantity,
                    stat_type,
                    update_action,
                    ..
                } if *update_action != 2 && (*stat_type == STAT_SETTLEMENT || *stat_type == STAT_OPEN_INTEREST) => {
                    let value = if *stat_type == STAT_SETTLEMENT {
                        dbn::price(*price)
                    } else {
                        (*quantity != dbn::UNDEF_STAT_QUANTITY).then_some(*quantity as f64)
                    };
                    let Some(value) = value else { continue };
                    let at = ts(*ts_ref).unwrap_or(hd.ts_event as i64);
                    self.stats.push(Stat {
                        id: hd.instrument_id,
                        day: at.div_euclid(DAY) * DAY,
                        recv: ts(*ts_recv).unwrap_or(hd.ts_event as i64),
                        kind: *stat_type,
                        value,
                    });
                }
                _ => {}
            }
        }
    }

    /// Adds a Parquet file of bars (`ts_event, instrument_id, open, high,
    /// low, close, volume`, optional `symbol` and `ts_recv`; prices as
    /// floats, or fixed-point integers at 1e-9) or of definitions
    /// (`instrument_id, raw_symbol, asset` or `root`, `expiration`,
    /// `activation`, `min_price_increment`, `unit_of_measure_qty`,
    /// `currency`, optional `instrument_class` and `ts_recv`). A bar's
    /// interval is the path's schema (`ohlcv-1m`) or its `rtype`.
    pub fn add_parquet(&mut self, path: &Path) -> Result<(), DataError> {
        let file = std::fs::File::open(path).map_err(|e| DataError::NotFound(format!("{}: {}", path.display(), e)))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
        self.files += 1;
        let interval = path.components().find_map(|c| match c.as_os_str().to_str()? {
            "ohlcv-1s" => Some(SECOND),
            "ohlcv-1m" => Some(MINUTE),
            "ohlcv-1h" => Some(60 * MINUTE),
            "ohlcv-1d" => Some(DAY),
            _ => None,
        });
        for b in reader {
            let b = b?;
            let what = path.display().to_string();
            if b.column_by_name("expiration").is_some() || b.column_by_name("raw_symbol").is_some() && b.column_by_name("close").is_none() {
                self.parquet_defs(&b, &what)?;
            } else {
                self.parquet_bars(&b, interval, &what)?;
            }
        }
        Ok(())
    }

    fn parquet_bars(&mut self, b: &RecordBatch, interval: Option<i64>, what: &str) -> Result<(), DataError> {
        let ids = ints(b, "instrument_id", what)?;
        let event = times(b, "ts_event", what)?;
        let recv = if b.column_by_name("ts_recv").is_some() { Some(times(b, "ts_recv", what)?) } else { None };
        let rtype = if b.column_by_name("rtype").is_some() { Some(ints(b, "rtype", what)?) } else { None };
        let symbols = if b.column_by_name("symbol").is_some() { Some(strings(b, "symbol", what)?) } else { None };
        let (o, h, l, c, v) = (
            prices(b, "open", what)?,
            prices(b, "high", what)?,
            prices(b, "low", what)?,
            prices(b, "close", what)?,
            nums(b, "volume", what)?,
        );
        for i in 0..b.num_rows() {
            let (Some(id), Some(start)) = (ids[i], event[i]) else {
                return Err(DataError::Schema(format!("{} row {}: no instrument_id or ts_event", what, i)));
            };
            let step = interval
                .or_else(|| rtype.as_ref().and_then(|r| r[i]).and_then(|r| interval_of(r as u8)))
                .ok_or_else(|| DataError::Schema(format!("{}: the bars' interval is neither in the path (ohlcv-1m) nor an `rtype` column", what)))?;
            self.bars.push(Bar {
                id: id as u32,
                start,
                t: start + step,
                open: o[i].unwrap_or(f64::NAN),
                high: h[i].unwrap_or(f64::NAN),
                low: l[i].unwrap_or(f64::NAN),
                close: c[i].unwrap_or(f64::NAN),
                volume: v[i].unwrap_or(0.0),
                recv: recv.as_ref().and_then(|r| r[i]),
                symbol: symbols.as_ref().and_then(|s| s[i].clone()),
            });
        }
        Ok(())
    }

    fn parquet_defs(&mut self, b: &RecordBatch, what: &str) -> Result<(), DataError> {
        let ids = ints(b, "instrument_id", what)?;
        let raw = strings(b, "raw_symbol", what)?;
        let root = if b.column_by_name("asset").is_some() {
            strings(b, "asset", what)?
        } else {
            strings(b, "root", what)?
        };
        let opt_times = |n: &str| -> Result<Vec<Option<i64>>, DataError> {
            if b.column_by_name(n).is_some() {
                times(b, n, what)
            } else {
                Ok(vec![None; b.num_rows()])
            }
        };
        let opt_prices = |n: &str| -> Result<Vec<Option<f64>>, DataError> {
            if b.column_by_name(n).is_some() {
                prices(b, n, what)
            } else {
                Ok(vec![None; b.num_rows()])
            }
        };
        let (activation, expiration, recv) = (opt_times("activation")?, opt_times("expiration")?, opt_times("ts_recv")?);
        let (tick, size) = (opt_prices("min_price_increment")?, opt_prices("unit_of_measure_qty")?);
        let currency = if b.column_by_name("currency").is_some() {
            strings(b, "currency", what)?
        } else {
            vec![None; b.num_rows()]
        };
        let class = if b.column_by_name("instrument_class").is_some() {
            strings(b, "instrument_class", what)?
        } else {
            vec![None; b.num_rows()]
        };
        for i in 0..b.num_rows() {
            let Some(id) = ids[i] else { continue };
            let symbol = raw[i].clone().unwrap_or_default();
            // A spread's symbol joins its legs (`ESH4-ESM4`).
            let class = class[i]
                .as_deref()
                .and_then(|c| c.bytes().next())
                .unwrap_or(if symbol.contains('-') || symbol.contains(':') { b'S' } else { b'F' });
            let fresh = Def {
                raw_symbol: symbol,
                root: root[i].clone().unwrap_or_default(),
                class,
                activation: activation[i],
                expiration: expiration[i],
                tick: tick[i],
                size: size[i],
                currency: currency[i].clone().unwrap_or_else(|| "USD".into()),
                recv: recv[i].unwrap_or(i64::MIN),
            };
            match self.defs.get(&(id as u32)) {
                Some(old) if old.recv > fresh.recv => {}
                _ => {
                    self.defs.insert(id as u32, fresh);
                }
            }
        }
        Ok(())
    }

    /// Adds every DBN and Parquet file at a path (a file, or a directory
    /// searched through).
    pub fn add_path(&mut self, path: &Path) -> Result<(), DataError> {
        let mut files = Vec::new();
        fn walk(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
            if p.is_dir() {
                for e in std::fs::read_dir(p)? {
                    walk(&e?.path(), out)?;
                }
            } else {
                out.push(p.to_path_buf());
            }
            Ok(())
        }
        if !path.exists() {
            return Err(DataError::NotFound(format!("{}: no such file or directory", path.display())));
        }
        walk(path, &mut files)?;
        files.sort();
        for f in files {
            let name = f.to_string_lossy().to_string();
            if name.ends_with(".dbn") || name.ends_with(".dbn.zst") {
                let bytes = unpack(std::fs::read(&f)?)?;
                self.add_dbn(&bytes, &name)?;
            } else if name.ends_with(".parquet") {
                self.add_parquet(&f)?;
            }
        }
        if self.files == 0 {
            return Err(DataError::NotFound(format!("{}: no .dbn, .dbn.zst or .parquet file", path.display())));
        }
        Ok(())
    }

    /// The symbol an instrument carries at `t`.
    fn symbol_at(&self, id: u32, t: i64) -> Option<&str> {
        self.mappings.iter().rev().find(|(i, from, to, _)| *i == id && *from <= t && t < *to).map(|m| m.3.as_str())
    }
}

fn column<'a>(b: &'a RecordBatch, name: &str, what: &str) -> Result<&'a ArrayRef, DataError> {
    b.column_by_name(name).ok_or_else(|| DataError::Schema(format!("{}: no column `{}`", what, name)))
}

fn ints(b: &RecordBatch, name: &str, what: &str) -> Result<Vec<Option<i64>>, DataError> {
    let c = arrow::compute::cast(column(b, name, what)?, &DataType::Int64)?;
    let a = c.as_primitive::<Int64Type>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

fn nums(b: &RecordBatch, name: &str, what: &str) -> Result<Vec<Option<f64>>, DataError> {
    let c = arrow::compute::cast(column(b, name, what)?, &DataType::Float64)?;
    let a = c.as_primitive::<Float64Type>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

/// Prices: floats as they are, integers as fixed-point at 1e-9 (the
/// undefined `i64::MAX` as none).
fn prices(b: &RecordBatch, name: &str, what: &str) -> Result<Vec<Option<f64>>, DataError> {
    let c = column(b, name, what)?;
    if matches!(c.data_type(), DataType::Int64 | DataType::UInt64) {
        return Ok(ints(b, name, what)?.into_iter().map(|x| x.and_then(dbn::price)).collect());
    }
    nums(b, name, what)
}

fn strings(b: &RecordBatch, name: &str, what: &str) -> Result<Vec<Option<String>>, DataError> {
    let c = arrow::compute::cast(column(b, name, what)?, &DataType::Utf8)?;
    let a = c.as_string::<i32>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i).to_string())).collect())
}

/// A time column in nanoseconds (timestamps of any unit, or integers in
/// nanoseconds).
fn times(b: &RecordBatch, name: &str, what: &str) -> Result<Vec<Option<i64>>, DataError> {
    let c = column(b, name, what)?;
    let t = match c.data_type() {
        DataType::Timestamp(_, _) => arrow::compute::cast(&arrow::compute::cast(c, &DataType::Timestamp(TimeUnit::Nanosecond, None))?, &DataType::Int64)?,
        _ => arrow::compute::cast(c, &DataType::Int64)?,
    };
    let a = t.as_primitive::<Int64Type>();
    Ok((0..a.len()).map(|i| (!a.is_null(i) && a.value(i) != i64::MAX && a.value(i) != -1).then(|| a.value(i))).collect())
}

/// A relation's rows: `A`, the time column, the values, and `r`.
fn keyed(time: &str, keys: Vec<String>, t: Vec<i64>, values: Vec<(&str, ArrayRef)>, r: Vec<i64>) -> Result<RecordBatch, DataError> {
    let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
    let mut fields = vec![Field::new("A", DataType::Utf8, false), Field::new(time, ts.clone(), false)];
    let mut cols: Vec<ArrayRef> = vec![Arc::new(StringArray::from(keys)), Arc::new(TimestampNanosecondArray::from(t))];
    for (n, a) in values {
        fields.push(Field::new(n, a.data_type().clone(), true));
        cols.push(a);
    }
    fields.push(Field::new("r", ts, false));
    cols.push(Arc::new(TimestampNanosecondArray::from(r)));
    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?)
}

fn f64s(v: Vec<f64>) -> ArrayRef {
    Arc::new(Float64Array::from(v))
}

/// The bars kept by the options' window, in (instrument, close) order; a
/// bar without a positive volume and a finite close is dropped.
fn window(raw: &Raw, opts: &Options, report: &mut Report) -> Vec<Bar> {
    let mut bars: Vec<Bar> = raw.bars.iter().filter(|b| opts.from.is_none_or(|f| b.t >= f) && opts.to.is_none_or(|x| b.t <= x)).cloned().collect();
    report.bars_in = bars.len();
    let before = bars.len();
    bars.retain(|b| b.close.is_finite() && b.open.is_finite() && b.high.is_finite() && b.low.is_finite());
    *report.dropped.entry("undefined_price".into()).or_default() += before - bars.len();
    bars.sort_by_key(|b| (b.id, b.t));
    let before = bars.len();
    bars.dedup_by(|a, b| a.id == b.id && a.t == b.t);
    *report.dropped.entry("duplicate_instrument_close".into()).or_default() += before - bars.len();
    bars
}

/// The receipt time of a bar (B-10).
fn received(b: &Bar, delay: i64) -> i64 {
    b.recv.map_or(b.t, |r| r.max(b.t)) + delay
}

/// The equity class's bars of a read, keyed by symbol (joined to the
/// store's tickers at ingest).
pub fn equities(raw: &Raw, opts: &Options) -> Result<(Fetched, Report), DataError> {
    let mut report = Report {
        files: raw.files,
        ..Report::default()
    };
    let bars = window(raw, opts, &mut report);
    let mut rows: Vec<(String, &Bar)> = Vec::with_capacity(bars.len());
    let mut intervals = BTreeSet::new();
    for b in &bars {
        let symbol = match &b.symbol {
            Some(s) => s.clone(),
            None => raw
                .symbol_at(b.id, b.start)
                .map(str::to_string)
                .ok_or_else(|| DataError::Identity(format!("the bar of instrument {} at {} has no symbol: the data's symbol mappings do not name it", b.id, iso(b.t))))?,
        };
        intervals.insert(b.t - b.start);
        rows.push((symbol, b));
    }
    if intervals.len() > 1 {
        return Err(DataError::Malformed(format!("bars of {} intervals in one read; read one schema at a time", intervals.len())));
    }
    report.bars_out = rows.len();
    report.symbols = rows.iter().map(|r| &r.0).collect::<BTreeSet<_>>().len();
    report.first = rows.iter().map(|r| r.1.t).min().map(iso);
    report.last = rows.iter().map(|r| r.1.t).max().map(iso);
    let keys: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let t: Vec<i64> = rows.iter().map(|r| r.1.t).collect();
    let r: Vec<i64> = rows.iter().map(|r| received(r.1, opts.processing_delay)).collect();
    let mut relations = Vec::new();
    for (rel, col, f) in [
        ("equity.open", "O", (|b: &Bar| b.open) as fn(&Bar) -> f64),
        ("equity.high", "H", |b: &Bar| b.high),
        ("equity.low", "L", |b: &Bar| b.low),
        ("equity.close", "P", |b: &Bar| b.close),
        ("equity.volume", "V", |b: &Bar| b.volume),
    ] {
        relations.push((
            rel.to_string(),
            keyed("T", keys.clone(), t.clone(), vec![(col, f64s(rows.iter().map(|x| f(x.1)).collect()))], r.clone())?,
        ));
    }
    let mut attributes = BTreeMap::new();
    if let Some(i) = intervals.iter().next() {
        attributes.insert("calendar".to_string(), calendar_of(*i));
    }
    if let Some(d) = &raw.dataset {
        attributes.insert("dataset".to_string(), d.clone());
    }
    attributes.insert("receipt".to_string(), receipt(&bars, opts));
    Ok((
        Fetched {
            relations,
            attributes,
            keys: Keys::Ticker,
            ..Fetched::default()
        },
        report,
    ))
}

/// How the rows' receipt times were set.
fn receipt(bars: &[Bar], opts: &Options) -> String {
    let recorded = bars.iter().any(|b| b.recv.is_some());
    format!(
        "{}{}",
        if recorded { "ts_recv" } else { "bar close" },
        if opts.processing_delay != 0 { format!(" + {} ns", opts.processing_delay) } else { String::new() }
    )
}

/// A session of a root: its first bar's start, its last bar's close.
#[derive(Clone, Copy, Debug)]
struct Session {
    open: i64,
    close: i64,
}

/// The future class's relations of a read: each root's continuous series
/// with its contracts and sessions.
pub fn futures(raw: &Raw, opts: &Options) -> Result<(Fetched, Report), DataError> {
    let mut report = Report {
        files: raw.files,
        definitions: raw.defs.len(),
        ..Report::default()
    };
    let bars = window(raw, opts, &mut report);
    let mut by_root: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for b in &bars {
        let d = match raw.defs.get(&b.id) {
            Some(d) => d,
            None if opts.undefined == Undefined::Drop => {
                *report.dropped.entry("no_definition".into()).or_default() += 1;
                continue;
            }
            None => {
                return Err(DataError::Identity(format!(
                    "the bar of instrument {} at {} has no definition: a future's root is its definition's (D-2.4; --undefined drop leaves such bars out and counts them)",
                    b.id,
                    iso(b.t)
                )))
            }
        };
        if d.class != b'F' {
            *report.dropped.entry("not_an_outright_future".into()).or_default() += 1;
            continue;
        }
        by_root.entry(d.root.as_str()).or_default().push(b);
    }
    let roll = opts.roll;
    let mut securities: Vec<Security> = Vec::new();
    let mut terms: Vec<Terms> = Vec::new();
    // The rows of each relation, by name.
    #[derive(Default)]
    struct Rows {
        keys: Vec<String>,
        t: Vec<i64>,
        r: Vec<i64>,
        nums: Vec<Vec<f64>>,
        times: Vec<i64>,
        labels: Vec<String>,
        others: Vec<String>,
    }
    let mut series = Rows::default();
    let mut cont = Rows::default();
    let mut sessions_rows = Rows::default();
    let mut point_rows = Rows::default();
    let mut commission_rows = Rows::default();
    let mut settle_rows = Rows::default();
    let mut oi_rows = Rows::default();
    let mut spec = Rows::default();
    let mut intervals = BTreeSet::new();
    for (root, mut rbars) in by_root {
        rbars.sort_by_key(|b| (b.t, b.id));
        intervals.extend(rbars.iter().map(|b| b.t - b.start));
        // Sessions: runs of the root's bars.
        let mut sessions: Vec<Session> = Vec::new();
        for b in &rbars {
            match sessions.last_mut() {
                Some(s) if b.t - s.close <= opts.session_gap => {
                    s.close = s.close.max(b.t);
                    s.open = s.open.min(b.start);
                }
                _ => sessions.push(Session { open: b.start, close: b.t }),
            }
        }
        let session_of = |t: i64| sessions.partition_point(|s| s.close < t);
        // Per session and contract: its bars, in order.
        let mut per: Vec<BTreeMap<u32, Vec<&Bar>>> = vec![BTreeMap::new(); sessions.len()];
        for b in &rbars {
            per[session_of(b.t)].entry(b.id).or_default().push(b);
        }
        let expiry = |id: u32| raw.defs[&id].expiration.unwrap_or(i64::MAX);
        let volume = |s: usize, id: u32| -> f64 { per[s].get(&id).map_or(0.0, |v| v.iter().map(|b| b.volume).sum()) };
        let mut held: Vec<u32> = Vec::with_capacity(sessions.len());
        for (s, here) in per.iter().enumerate() {
            let mut cands: Vec<u32> = here.keys().copied().collect();
            if let Some(&prev) = held.last() {
                cands.retain(|&c| expiry(c) >= expiry(prev));
            }
            let pick = match roll {
                Roll::Expiry => cands.iter().copied().min_by_key(|&c| (expiry(c), c)),
                // The first session has no session before it: the nearest
                // expiry, as the volume of the session itself is not known
                // until it ends.
                Roll::Volume if s == 0 => cands.iter().copied().min_by_key(|&c| (expiry(c), c)),
                Roll::Volume => {
                    let prev = held.last().copied();
                    let best = cands.iter().copied().max_by(|&a, &b| {
                        volume(s - 1, a)
                            .partial_cmp(&volume(s - 1, b))
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(expiry(b).cmp(&expiry(a)))
                            .then(b.cmp(&a))
                    });
                    match (best, prev) {
                        // Nothing traded the session before: stay, when the
                        // contract held still trades.
                        (Some(b), Some(p)) if volume(s - 1, b) == 0.0 && cands.contains(&p) => Some(p),
                        (Some(b), _) => Some(b),
                        (None, _) => None,
                    }
                }
            };
            match pick {
                Some(c) => held.push(c),
                // Only contracts expiring before the one held trade: the
                // series ends.
                None => break,
            }
        }
        let sessions = &sessions[..held.len()];
        // The root's grid (A-D53): the smallest decimal exponent at which
        // every price of its bars and the held contracts' tick sizes are
        // integers (a definition's tick can be coarser than the prices
        // traded), at which the gaps, the adjustments and the adjusted
        // prices are computed; a root with a price off every grid takes
        // the `f64` arithmetic (such values are raw exceptions in their
        // blocks, A-D54).
        let grid: Option<u8> = {
            let held_set: BTreeSet<u32> = held.iter().copied().collect();
            let mut vals: Vec<f64> = rbars.iter().flat_map(|b| [b.open, b.high, b.low, b.close]).collect();
            vals.extend(held_set.iter().filter_map(|c| raw.defs[c].tick.filter(|t| *t > 0.0)));
            abt_codec::tick::exponent(&vals, abt_codec::tick::Source::F64)
        };
        let p10 = grid.map_or(1.0, |e| abt_agg::exact::pow10(e as u32) as f64);
        let on = |x: f64| -> Option<i64> { grid.and_then(|e| abt_codec::tick::on_grid(x, e)) };
        // The gaps at the rolls, and each session's adjustment: integers on
        // the grid where the prices are, else `f64`.
        let mut gaps = vec![0.0; held.len()];
        let mut gaps_i: Vec<Option<i64>> = vec![Some(0); held.len()];
        let mut rolls = Vec::new();
        for s in 1..held.len() {
            let (o, n) = (held[s - 1], held[s]);
            if o == n {
                continue;
            }
            let o_last = per[s - 1][&o].last().map(|b| b.close).unwrap_or(f64::NAN);
            let n_at = match per[s - 1].get(&n).and_then(|v| v.last()) {
                Some(nb) => nb.close,
                None => per[s][&n][0].open,
            };
            gaps_i[s] = match (on(n_at), on(o_last)) {
                (Some(a), Some(b)) => Some(a - b),
                _ => None,
            };
            gaps[s] = match gaps_i[s] {
                Some(g) => g as f64 / p10,
                None => n_at - o_last,
            };
            rolls.push(format!("{} {} -> {}, gap {}", day_of(sessions[s].close), raw.defs[&o].raw_symbol, raw.defs[&n].raw_symbol, gaps[s]));
        }
        let mut adjustment = vec![0.0; held.len()];
        let mut adjustment_i: Vec<Option<i64>> = vec![Some(0); held.len()];
        for s in (0..held.len().saturating_sub(1)).rev() {
            adjustment_i[s] = match (adjustment_i[s + 1], gaps_i[s + 1]) {
                (Some(a), Some(g)) => Some(a + g),
                _ => None,
            };
            adjustment[s] = match adjustment_i[s] {
                Some(a) => a as f64 / p10,
                None => adjustment[s + 1] + gaps[s + 1],
            };
        }
        // A price shifted by a session's adjustment: on the grid when both
        // are, divided once.
        let adjusted = |x: f64, s: usize| -> f64 {
            match (adjustment_i[s], on(x)) {
                (Some(0), _) => x,
                (Some(a), Some(xi)) => (xi + a) as f64 / p10,
                _ => x + adjustment[s],
            }
        };
        let root_key = format!("{}.{}.0", root, roll.code());
        let contracts: BTreeSet<u32> = rbars.iter().map(|b| b.id).collect();
        let point_of = |id: u32| -> Option<f64> { opts.point_values.get(root).copied().or(raw.defs[&id].size) };
        let first = rbars.first().map(|b| b.t).unwrap_or(0);
        securities.push(Security {
            key: root_key.clone(),
            ticker: root.to_string(),
            from: first,
            to: None,
        });
        terms.push(Terms {
            key: root_key.clone(),
            class: "future".into(),
            multiplier: held.last().and_then(|&c| point_of(c)),
            commission_per_contract: opts.commission,
        });
        for &c in &contracts {
            let d = &raw.defs[&c];
            let key = c.to_string();
            securities.push(Security {
                key: key.clone(),
                ticker: d.raw_symbol.clone(),
                from: d.activation.unwrap_or(first).min(first),
                to: d.expiration,
            });
            terms.push(Terms {
                key: key.clone(),
                class: "future".into(),
                multiplier: point_of(c),
                commission_per_contract: opts.commission,
            });
            spec.keys.push(key);
            spec.t.push(d.activation.unwrap_or(first));
            // A contract's terms are known from its first trade.
            spec.r.push(d.activation.unwrap_or(first));
            spec.labels.push(d.root.clone());
            spec.times.push(d.expiration.unwrap_or(i64::MAX));
            spec.nums.push(vec![point_of(c).unwrap_or(f64::NAN), d.tick.unwrap_or(f64::NAN)]);
            spec.others.push(opts.settlement.get(root).cloned().unwrap_or_else(|| "unstated".into()));
            let _ = &d.currency;
        }
        for (s, sess) in sessions.iter().enumerate() {
            let c = held[s];
            let adj = adjustment[s];
            for b in &per[s][&c] {
                let r = received(b, opts.processing_delay);
                series.keys.push(root_key.clone());
                series.t.push(b.t);
                series.r.push(r);
                series.nums.push(vec![adjusted(b.open, s), adjusted(b.high, s), adjusted(b.low, s), adjusted(b.close, s), b.volume]);
                cont.keys.push(root_key.clone());
                cont.t.push(b.t);
                cont.r.push(r);
                cont.others.push(c.to_string());
                cont.nums.push(vec![adj]);
            }
            let last = per[s][&c].last().map(|b| received(b, opts.processing_delay)).unwrap_or(sess.close);
            sessions_rows.keys.push(root_key.clone());
            sessions_rows.t.push(sess.close);
            sessions_rows.times.push(sess.open);
            sessions_rows.r.push(last.max(sess.close + opts.processing_delay));
            let day = sess.close.div_euclid(DAY) * DAY;
            // The venue's statistics of the contract held, for the
            // session's day: the settlement back-adjusted as the bars, the
            // open interest as published; the last received of each.
            for (kind, rows, shifted) in [(STAT_SETTLEMENT, &mut settle_rows, true), (STAT_OPEN_INTEREST, &mut oi_rows, false)] {
                if let Some(st) = raw.stats.iter().filter(|x| x.id == c && x.kind == kind && x.day == day).max_by_key(|x| x.recv) {
                    rows.keys.push(root_key.clone());
                    rows.t.push(sess.close);
                    rows.r.push(st.recv.max(sess.close));
                    rows.nums.push(vec![if shifted { adjusted(st.value, s) } else { st.value }]);
                }
            }
            if let Some(pv) = point_of(c) {
                point_rows.keys.push(root_key.clone());
                point_rows.t.push(day);
                point_rows.r.push(day);
                point_rows.nums.push(vec![pv]);
            }
            if let Some(cm) = opts.commission {
                commission_rows.keys.push(root_key.clone());
                commission_rows.t.push(day);
                commission_rows.r.push(day);
                commission_rows.nums.push(vec![cm]);
            }
        }
        report.roots.insert(
            root.to_string(),
            RootReport {
                contracts: contracts.len(),
                sessions: held.len(),
                rolls,
            },
        );
    }
    if intervals.len() > 1 {
        return Err(DataError::Malformed(format!("bars of {} intervals in one read; read one schema at a time", intervals.len())));
    }
    report.bars_out = series.keys.len();
    report.first = series.t.iter().min().copied().map(iso);
    report.last = series.t.iter().max().copied().map(iso);
    let col = |rows: &Rows, k: usize| -> ArrayRef { f64s(rows.nums.iter().map(|v| v[k]).collect()) };
    let mut relations = Vec::new();
    for (k, (rel, name)) in [("future.open", "O"), ("future.high", "H"), ("future.low", "L"), ("future.close", "P"), ("future.volume", "V")]
        .into_iter()
        .enumerate()
    {
        relations.push((rel.to_string(), keyed("T", series.keys.clone(), series.t.clone(), vec![(name, col(&series, k))], series.r.clone())?));
    }
    let ts_col = |v: &[i64]| -> ArrayRef { Arc::new(TimestampNanosecondArray::from(v.to_vec())) };
    relations.push((
        "future.session".to_string(),
        keyed(
            "S",
            sessions_rows.keys.clone(),
            sessions_rows.t.clone(),
            vec![("Open", ts_col(&sessions_rows.times))],
            sessions_rows.r.clone(),
        )?,
    ));
    relations.push((
        "future.point_value".to_string(),
        keyed("T", point_rows.keys.clone(), point_rows.t.clone(), vec![("V", col(&point_rows, 0))], point_rows.r.clone())?,
    ));
    if !settle_rows.keys.is_empty() {
        relations.push((
            "future.settlement".to_string(),
            keyed("T", settle_rows.keys.clone(), settle_rows.t.clone(), vec![("P", col(&settle_rows, 0))], settle_rows.r.clone())?,
        ));
    }
    if !oi_rows.keys.is_empty() {
        relations.push((
            "future.open_interest".to_string(),
            keyed("T", oi_rows.keys.clone(), oi_rows.t.clone(), vec![("N", col(&oi_rows, 0))], oi_rows.r.clone())?,
        ));
    }
    if opts.commission.is_some() {
        relations.push((
            "future.commission".to_string(),
            keyed(
                "T",
                commission_rows.keys.clone(),
                commission_rows.t.clone(),
                vec![("C", col(&commission_rows, 0))],
                commission_rows.r.clone(),
            )?,
        ));
    }
    // The registry's relations: which contract each bar is of, and the
    // contracts' terms.
    let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
    relations.push((
        "continuous".to_string(),
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Field::new("root_id", DataType::Utf8, false),
                Field::new("t", ts.clone(), false),
                Field::new("contract_id", DataType::Utf8, true),
                Field::new("adjustment", DataType::Float64, true),
                Field::new("r", ts.clone(), false),
            ])),
            vec![
                Arc::new(StringArray::from(cont.keys.clone())),
                ts_col(&cont.t),
                Arc::new(StringArray::from(cont.others.clone())),
                col(&cont, 0),
                ts_col(&cont.r),
            ],
        )?,
    ));
    relations.push((
        "future_spec".to_string(),
        RecordBatch::try_new(
            Arc::new(Schema::new(vec![
                Field::new("id", DataType::Utf8, false),
                Field::new("first_trade", ts.clone(), false),
                Field::new("root", DataType::Utf8, true),
                Field::new("expiry", ts.clone(), true),
                Field::new("last_trade", ts.clone(), true),
                Field::new("point_value", DataType::Float64, true),
                Field::new("tick", DataType::Float64, true),
                Field::new("settlement", DataType::Utf8, true),
                Field::new("r", ts.clone(), false),
            ])),
            vec![
                Arc::new(StringArray::from(spec.keys.clone())),
                ts_col(&spec.t),
                Arc::new(StringArray::from(spec.labels.clone())),
                ts_col(&spec.times),
                ts_col(&spec.times),
                col(&spec, 0),
                col(&spec, 1),
                Arc::new(StringArray::from(spec.others.clone())),
                ts_col(&spec.r),
            ],
        )?,
    ));
    let mut attributes = BTreeMap::new();
    if let Some(i) = intervals.iter().next() {
        attributes.insert("calendar".to_string(), calendar_of(*i));
    }
    if let Some(d) = &raw.dataset {
        attributes.insert("dataset".to_string(), d.clone());
    }
    attributes.insert("prices".to_string(), "back_adjusted".to_string());
    attributes.insert(
        "roll".to_string(),
        match roll {
            Roll::Volume => "volume".to_string(),
            Roll::Expiry => "expiry".to_string(),
        },
    );
    attributes.insert("receipt".to_string(), receipt(&bars, opts));
    Ok((
        Fetched {
            securities,
            terms,
            relations,
            attributes,
            keys: Keys::Vendor,
            ..Fetched::default()
        },
        report,
    ))
}

/// Where the adapter reads from.
#[derive(Clone, Debug, PartialEq)]
pub enum Locator {
    /// DBN or Parquet files: a file, or a directory searched through.
    Files(PathBuf),
    /// The historical API.
    Api(super::http::Historical),
}

/// Databento as a source: its files or its API, read for a request's span.
pub struct Databento {
    locator: Locator,
    opts: Options,
    last: Mutex<Option<(Options, Fetched)>>,
}

impl Databento {
    pub fn new(locator: Locator, opts: Options) -> Databento {
        Databento {
            locator,
            opts,
            last: Mutex::new(None),
        }
    }

    /// The adapter a catalog's source configures (D-7.1): `files` (a path)
    /// or `api` (the historical API's base URL, `https://hist.databento.com`)
    /// with `dataset`, `schema` (`ohlcv-1m`), `symbols` (`ES.FUT,NQ.FUT`),
    /// `stype_in` (`parent`) and `key_env` (the environment variable that
    /// holds the key, `DATABENTO_API_KEY` by default); and `class` (`future`
    /// or `equity`), `processing_delay` (seconds), `roll` (`volume` or
    /// `expiry`), `session_gap` (minutes), `point_values` and `settlement`
    /// (`ZN=1000;ES=cash`), `commission`, `from` and `to` (dates).
    pub fn from_settings(settings: &BTreeMap<String, String>) -> Result<Databento, DataError> {
        let bad = |k: &str, v: &str, want: &str| DataError::Malformed(format!("the databento setting `{}` is {}, not `{}`", k, want, v));
        let mut opts = Options::default();
        let mut api = super::http::Historical::default();
        let mut files: Option<PathBuf> = None;
        let pairs = |k: &str, v: &str| -> Result<Vec<(String, String)>, DataError> {
            v.split(';')
                .filter(|x| !x.trim().is_empty())
                .map(|x| {
                    x.split_once('=')
                        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                        .ok_or_else(|| bad(k, v, "ROOT=VALUE[;ROOT=VALUE...]"))
                })
                .collect()
        };
        let date = |k: &str, v: &str| abt_wire::scalar::parse_timestamp(v).ok_or_else(|| bad(k, v, "a date"));
        for (k, v) in settings {
            match k.as_str() {
                "files" => files = Some(PathBuf::from(v)),
                "api" => api.base = v.clone(),
                "dataset" => api.dataset = v.clone(),
                "schema" => api.schema = v.clone(),
                "symbols" => api.symbols = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                "stype_in" => api.stype_in = v.clone(),
                "key_env" => api.key_env = v.clone(),
                "class" if v == "future" || v == "equity" => opts.class = v.clone(),
                "class" => return Err(bad(k, v, "future or equity")),
                "undefined" => {
                    opts.undefined = match v.as_str() {
                        "refuse" => Undefined::Refuse,
                        "drop" => Undefined::Drop,
                        _ => return Err(bad(k, v, "refuse or drop")),
                    }
                }
                "processing_delay" => opts.processing_delay = (v.parse::<f64>().map_err(|_| bad(k, v, "a number of seconds"))? * SECOND as f64).round() as i64,
                "roll" => {
                    opts.roll = match v.as_str() {
                        "volume" => Roll::Volume,
                        "expiry" => Roll::Expiry,
                        _ => return Err(bad(k, v, "volume or expiry")),
                    }
                }
                "session_gap" => opts.session_gap = v.parse::<i64>().map_err(|_| bad(k, v, "a number of minutes"))? * MINUTE,
                "point_values" => {
                    for (r, x) in pairs(k, v)? {
                        opts.point_values.insert(r, x.parse().map_err(|_| bad(k, &x, "a number"))?);
                    }
                }
                "settlement" => opts.settlement = pairs(k, v)?.into_iter().collect(),
                "commission" => opts.commission = Some(v.parse().map_err(|_| bad(k, v, "a number"))?),
                "from" => opts.from = Some(date(k, v)?),
                "to" => opts.to = Some(date(k, v)?.saturating_add(DAY - 1)),
                _ => return Err(DataError::Malformed(format!("the databento adapter has no setting `{}`", k))),
            }
        }
        let locator = match (files, api.base.is_empty()) {
            (Some(f), true) => Locator::Files(f),
            (None, false) => Locator::Api(api),
            _ => return Err(DataError::Malformed("a databento source names `files` or `api`, one of them".into())),
        };
        Ok(Databento::new(locator, opts))
    }

    /// What the locator holds over the options' window, read and made the
    /// class's relations, with the read's report.
    pub fn read(&self, opts: &Options) -> Result<(Fetched, Report), DataError> {
        let mut raw = Raw::default();
        match &self.locator {
            Locator::Files(p) => raw.add_path(p)?,
            Locator::Api(api) => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0);
                let (from, to) = (opts.from.unwrap_or(0), opts.to.unwrap_or(now));
                if opts.class == "future" {
                    let defs = api.get_range("definition", from, to)?;
                    raw.add_dbn(&defs, "the definitions of the historical API")?;
                }
                let bars = api.get_range(&api.schema, from, to)?;
                raw.add_dbn(&bars, "the bars of the historical API")?;
            }
        }
        match opts.class.as_str() {
            "equity" => equities(&raw, opts),
            _ => futures(&raw, opts),
        }
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }
}

/// The future relations the adapter provides.
pub const FUTURE_RELATIONS: [&str; 7] = ["open", "high", "low", "close", "volume", "session", "point_value"];
/// The equity relations.
pub const EQUITY_RELATIONS: [&str; 5] = ["open", "high", "low", "close", "volume"];

impl Adapter for Databento {
    fn vendor(&self) -> &str {
        "databento"
    }

    fn offers(&self) -> Vec<Offer> {
        let relations: &[&str] = if self.opts.class == "equity" { &EQUITY_RELATIONS } else { &FUTURE_RELATIONS };
        let calendar = match &self.locator {
            Locator::Api(a) => a.schema.trim_start_matches("ohlcv-").to_string(),
            Locator::Files(_) => "1m".to_string(),
        };
        vec![Offer {
            class: self.opts.class.clone(),
            relations: relations.iter().map(|r| r.to_string()).collect(),
            calendar,
            from: self.opts.from.map(day_of),
            to: self.opts.to.map(day_of),
            scope: "all".into(),
            has_receipt: false,
            supplies: vec![],
            attributes: BTreeMap::new(),
            instruments: None,
        }]
    }

    fn fetch(&self, req: &FetchRequest) -> Result<Fetched, DataError> {
        let mut opts = self.opts.clone();
        opts.from = Some(opts.from.map_or(req.from, |f| f.max(req.from)));
        opts.to = Some(opts.to.map_or(req.to, |t| t.min(req.to)));
        let cached = self.last.lock().ok().and_then(|l| l.as_ref().filter(|(o, _)| *o == opts).map(|(_, f)| f.clone()));
        let mut got = match cached {
            Some(f) => f,
            None => {
                let (f, _) = self.read(&opts)?;
                if let Ok(mut l) = self.last.lock() {
                    *l = Some((opts, f.clone()));
                }
                f
            }
        };
        // The registry's relations travel with the bars they key.
        got.relations.retain(|(r, _)| req.relations.contains(r) || r == "continuous" || r == "future_spec");
        Ok(got)
    }
}
