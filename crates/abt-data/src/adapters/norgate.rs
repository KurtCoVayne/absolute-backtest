//! The Norgate adapter (D-7.7): NDLake, the Norgate data lake, read
//! directly through the Parquet reader, and turned into the equity class's
//! relations keyed by Norgate's `asset_id`. It carries the decisions
//! `docs/handoff-data.md` (section 4) records, which the retired
//! `scripts/ingest/norgate_lake_to_inputs.py` and the kernel's
//! `norgate_daily` made in two steps (both retired, ERRATA E-68):
//!
//! 1. The universe is point in time: every asset that was ever a member of
//!    one of the indexes over the span, never the current list.
//! 2. Membership intervals, half-open in the lake, are made inclusive,
//!    clipped to the asset's bars and split around trading days the asset
//!    has no bar on.
//! 3. The delisting date is the first trading day after the last print.
//! 4. The delisting reason is inferred (the lake has none): a ticker ending
//!    in Q, or a last close under $1, is `bankruptcy`; a last close under
//!    half the 60-bar high is `other`; else `acquisition`. The versions
//!    state `delisting_reason = inferred`.
//! 5. A dividend is announced, goes ex and is paid on its ex-date (the lake
//!    has no other date): `dividend_dates = ex_only`.
//! 6. A dividend on the ex-date of a split is divided by the split factor.
//! 7. Spin-offs stay as the lake books them, cash dividends.
//! 8. Classification is one snapshot, emitted from its snapshot date only.
//! 9. Prices are rounded to six decimals, and a missing or non-positive
//!    open, high or low is the close; a bar without a positive close is
//!    dropped; of two rows of one (asset, date) the latest ingest is kept.
//! 10. Tickers are the asset's final ticker per bar (`ticker_history =
//!     final`); of two assets carrying a ticker over overlapping dates, the
//!     one ending first is renamed to its source symbol.
//!
//! An asset in several lists is a member of each (`member(A, T, "SP500")`,
//! `member(A, T, "SP400")`).
//!
//! Index series the lake holds (`$SPXTR`, a `series_type = index` asset)
//! become the series class's `value` under a name of its own (`SPXTR`),
//! keyed at the day like the bars.
//!
//! The lake's tables (each a Parquet file `X.parquet` or a directory of
//! them, hive partitions included): `curated/bars_by_date`,
//! `registry/corporate_actions`, `indexes/index_membership`,
//! `classification/classification_full`. A reviewed exceptions file
//! (`test, symbol, date, reason`) becomes the versions'
//! `reviewed_exceptions` attribute. The sessions are the days the bars
//! have, as the bundles' were: `registry/trading_calendar` is not read
//! (ERRATA E-68).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arrow::array::{Array, ArrayRef, AsArray, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Float64Type, Int64Type, Schema, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ProjectionMask;

use super::{Adapter, FetchRequest, Fetched};
use crate::adapter::Security;
use crate::error::DataError;
use crate::resolver::catalog::Offer;

const DAY: i64 = 86_400_000_000_000;

/// What to read of the lake.
#[derive(Clone, Debug, PartialEq)]
pub struct LakeOptions {
    /// The indexes whose members, ever, are the universe (`S&P 500`), each
    /// with the list label its members carry (`SPX`).
    pub lists: Vec<(String, String)>,
    /// The index series to read (the lake's ticker, `$SPXTR`), each with
    /// its name in the series class (`SPXTR`).
    pub series: Vec<(String, String)>,
    /// The first and last day (days since 1970-01-01); the last defaults to
    /// the lake's last bar.
    pub from: i64,
    pub to: Option<i64>,
    /// Only the first `n` assets by id (smoke runs).
    pub limit: Option<usize>,
    /// The classification scheme whose level 1 codes are emitted.
    pub scheme: String,
    /// A reviewed exceptions file.
    pub exceptions: Option<PathBuf>,
}

impl Default for LakeOptions {
    fn default() -> Self {
        LakeOptions {
            lists: vec![("S&P 500".into(), "SPX".into())],
            series: vec![],
            from: days_of("2014-01-01").unwrap_or(0),
            to: None,
            limit: None,
            scheme: "GICS".into(),
            exceptions: None,
        }
    }
}

/// The days since 1970-01-01 of a `YYYY-MM-DD` date.
pub fn days_of(s: &str) -> Option<i64> {
    abt_wire::scalar::parse_timestamp(s).map(|t| t.div_euclid(DAY))
}

fn ymd(d: i64) -> String {
    let (y, m, dd) = abt_wire::scalar::civil_from_days(d);
    format!("{:04}-{:02}-{:02}", y, m, dd)
}

/// What a read of the lake did: its counts and its decisions, the
/// `ingest-report.json` of the retired script.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Report {
    pub universe_assets: usize,
    pub rows_in: usize,
    pub rows_out: usize,
    pub drops: BTreeMap<String, usize>,
    pub filled: BTreeMap<String, usize>,
    pub first_day: Option<String>,
    pub last_day: Option<String>,
    /// The trading days: the days the bars have.
    pub days: usize,
    pub symbol_intervals: usize,
    pub ticker_collisions_renamed: usize,
    pub splits: usize,
    pub dividends: usize,
    pub dividends_rescaled_for_same_day_split: usize,
    pub delisting_reasons: BTreeMap<String, usize>,
    pub membership_intervals: usize,
    pub membership_intervals_split_at_missing_bars: usize,
    pub classification_snapshot: Option<String>,
    pub exceptions: usize,
    pub series_rows: BTreeMap<String, usize>,
}

// ------------------------------------------------------------ reading

/// The Parquet files of a lake table: `X.parquet`, or every `.parquet`
/// under the directory `X` (hive partitions such as `year=2024/`).
fn table_files(lake: &Path, table: &str) -> Result<Vec<PathBuf>, DataError> {
    let file = lake.join(format!("{}.parquet", table));
    if file.is_file() {
        return Ok(vec![file]);
    }
    let dir = lake.join(table);
    if !dir.is_dir() {
        return Err(DataError::NotFound(format!("the lake {} has no table `{}`", lake.display(), table)));
    }
    let mut out = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for e in std::fs::read_dir(d)? {
            let p = e?.path();
            if p.is_dir() {
                walk(&p, out)?;
            } else if p.extension().is_some_and(|x| x == "parquet") {
                out.push(p);
            }
        }
        Ok(())
    }
    walk(&dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// A file's `year=YYYY` partition, when its path has one.
fn year_of(p: &Path) -> Option<i64> {
    p.components().find_map(|c| c.as_os_str().to_str()?.strip_prefix("year=")?.parse().ok())
}

/// The batches of a table with the named columns (those the files have).
fn read(lake: &Path, table: &str, columns: &[&str], keep: impl Fn(&Path) -> bool) -> Result<Vec<RecordBatch>, DataError> {
    let mut out = Vec::new();
    for f in table_files(lake, table)? {
        if !keep(&f) {
            continue;
        }
        let file = std::fs::File::open(&f)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let schema = builder.parquet_schema();
        let idx: Vec<usize> = columns.iter().filter_map(|c| (0..schema.num_columns()).find(|&i| schema.column(i).name() == *c)).collect();
        let mask = ProjectionMask::leaves(schema, idx);
        for b in builder.with_projection(mask).build()? {
            out.push(b?);
        }
    }
    Ok(out)
}

fn column<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a ArrayRef, DataError> {
    b.column_by_name(name).ok_or_else(|| DataError::Schema(format!("the lake's table has no column `{}`", name)))
}

fn strings(b: &RecordBatch, name: &str) -> Result<Vec<Option<String>>, DataError> {
    let c = arrow::compute::cast(column(b, name)?, &DataType::Utf8)?;
    let a = c.as_string::<i32>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i).to_string())).collect())
}

fn ints(b: &RecordBatch, name: &str) -> Result<Vec<Option<i64>>, DataError> {
    let c = arrow::compute::cast(column(b, name)?, &DataType::Int64)?;
    let a = c.as_primitive::<Int64Type>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

fn nums(b: &RecordBatch, name: &str) -> Result<Vec<Option<f64>>, DataError> {
    let c = arrow::compute::cast(column(b, name)?, &DataType::Float64)?;
    let a = c.as_primitive::<Float64Type>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

/// A date or time column as days since 1970-01-01.
fn days(b: &RecordBatch, name: &str) -> Result<Vec<Option<i64>>, DataError> {
    let c = column(b, name)?;
    let t = match c.data_type() {
        DataType::Utf8 | DataType::LargeUtf8 => {
            return Ok(strings(b, name)?.into_iter().map(|s| s.and_then(|s| days_of(&s))).collect());
        }
        _ => arrow::compute::cast(c, &DataType::Timestamp(TimeUnit::Nanosecond, None))?,
    };
    let a = t.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i).div_euclid(DAY))).collect())
}

// ------------------------------------------------------------ the lake's facts

#[derive(Clone, Debug)]
struct Bar {
    asset: i64,
    ticker: String,
    source: String,
    day: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

/// An asset's ticker over a span of days (both included; `to` open while
/// current).
#[derive(Clone, Debug)]
struct Interval {
    asset: i64,
    ticker: String,
    source: String,
    from: i64,
    to: Option<i64>,
}

impl Interval {
    fn covers(&self, d: i64) -> bool {
        d >= self.from && self.to.is_none_or(|t| d <= t)
    }
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn bad(x: Option<f64>) -> bool {
    x.is_none_or(|v| v.is_nan() || v <= 0.0)
}

/// Whether a symbol reads as a bankruptcy: a final `Q`, before an optional
/// `-YYYYMM` delisting month, on a root of four letters or more.
fn bankrupt_symbol(s: &str) -> bool {
    let root = match s.rsplit_once('-') {
        Some((r, m)) if m.len() == 6 && m.chars().all(|c| c.is_ascii_digit()) => r,
        _ => s,
    };
    root.ends_with('Q') && root.chars().count() >= 4
}

/// The lake read: the universe's relations and the report.
pub fn read_lake(lake: &Path, opts: &LakeOptions) -> Result<(Fetched, Report), DataError> {
    let mut report = Report::default();
    // 1. The point-in-time universe.
    let label_of: BTreeMap<&str, &str> = opts.lists.iter().map(|(i, l)| (i.as_str(), l.as_str())).collect();
    let mut members: Vec<(i64, &str, i64, Option<i64>)> = Vec::new();
    for b in read(lake, "indexes/index_membership", &["index_name", "asset_id", "member_from", "member_to"], |_| true)? {
        let (name, asset, from, to) = (strings(&b, "index_name")?, ints(&b, "asset_id")?, days(&b, "member_from")?, days(&b, "member_to")?);
        for i in 0..b.num_rows() {
            let (Some(n), Some(a), Some(f)) = (&name[i], asset[i], from[i]) else { continue };
            let Some(&label) = label_of.get(n.as_str()) else { continue };
            if to[i].is_some_and(|t| t <= opts.from) || opts.to.is_some_and(|t| f > t) {
                continue;
            }
            members.push((a, label, f, to[i]));
        }
    }
    let mut ids: Vec<i64> = members.iter().map(|m| m.0).collect::<BTreeSet<_>>().into_iter().collect();
    if let Some(n) = opts.limit {
        ids.truncate(n);
    }
    let idset: BTreeSet<i64> = ids.iter().copied().collect();
    report.universe_assets = ids.len();
    // 2. The universe's equity bars over the span.
    // Only the year files of the span are read: the lake holds every name
    // of every year, and a year of it is about half a gigabyte in Arrow.
    let first_year = abt_wire::scalar::civil_from_days(opts.from).0;
    let last_year = opts.to.map(|t| abt_wire::scalar::civil_from_days(t).0);
    let mut latest: HashMap<(i64, i64), (i64, Bar)> = HashMap::new();
    let series_of: BTreeMap<&str, &str> = opts.series.iter().map(|(t, n)| (t.as_str(), n.as_str())).collect();
    // The index series' closes by (name, day), the latest ingest kept.
    let mut series: BTreeMap<(String, i64), (i64, f64)> = BTreeMap::new();
    let mut rows_in = 0;
    let cols = ["asset_id", "ticker", "source_symbol", "date", "open", "high", "low", "close", "volume", "ingest_ts", "series_type"];
    for b in read(lake, "curated/bars_by_date", &cols, |p| {
        year_of(p).is_none_or(|y| y >= first_year && last_year.is_none_or(|ly| y <= ly))
    })? {
        let (asset, ticker, source, day) = (ints(&b, "asset_id")?, strings(&b, "ticker")?, strings(&b, "source_symbol")?, days(&b, "date")?);
        let (open, high, low, close, volume) = (nums(&b, "open")?, nums(&b, "high")?, nums(&b, "low")?, nums(&b, "close")?, nums(&b, "volume")?);
        let ingest = if b.column_by_name("ingest_ts").is_some() {
            days_ns(&b, "ingest_ts")?
        } else {
            vec![None; b.num_rows()]
        };
        let kind = strings(&b, "series_type")?;
        for i in 0..b.num_rows() {
            let (Some(a), Some(d)) = (asset[i], day[i]) else { continue };
            if d < opts.from || opts.to.is_some_and(|t| d > t) {
                continue;
            }
            if kind[i].as_deref() == Some("index") {
                let name = ticker[i].as_deref().and_then(|t| series_of.get(t));
                if let (Some(name), Some(c)) = (name, close[i].filter(|c| c.is_finite())) {
                    let ts = ingest[i].unwrap_or(i64::MIN);
                    let slot = series.entry((name.to_string(), d)).or_insert((i64::MIN, c));
                    if ts >= slot.0 {
                        *slot = (ts, c);
                    }
                }
                continue;
            }
            if !idset.contains(&a) || kind[i].as_deref() != Some("equity") {
                continue;
            }
            rows_in += 1;
            let bar = Bar {
                asset: a,
                ticker: ticker[i].clone().unwrap_or_default(),
                source: source[i].clone().unwrap_or_default(),
                day: d,
                open: open[i].unwrap_or(f64::NAN),
                high: high[i].unwrap_or(f64::NAN),
                low: low[i].unwrap_or(f64::NAN),
                close: close[i].unwrap_or(f64::NAN),
                volume: volume[i].unwrap_or(f64::NAN),
            };
            // Of two rows of one (asset, date), the latest ingest.
            let ts = ingest[i].unwrap_or(i64::MIN);
            match latest.get(&(a, d)) {
                Some((t, _)) if *t >= ts => {}
                _ => {
                    latest.insert((a, d), (ts, bar));
                }
            }
        }
    }
    report.rows_in = rows_in;
    report.drops.insert("duplicate_asset_date".into(), rows_in - latest.len());
    let mut bars: Vec<Bar> = Vec::new();
    let mut dropped = 0;
    let mut filled: BTreeMap<String, usize> = BTreeMap::new();
    for (_, (_, mut b)) in latest {
        if bad(Some(b.close)) {
            dropped += 1;
            continue;
        }
        for (name, v) in [("open", &mut b.open), ("high", &mut b.high), ("low", &mut b.low)] {
            if bad(Some(*v)) {
                *filled.entry(name.into()).or_default() += 1;
                *v = b.close;
            }
        }
        if b.volume.is_nan() {
            *filled.entry("volume_null_to_zero".into()).or_default() += 1;
            b.volume = 0.0;
        }
        b.open = round6(b.open);
        b.high = round6(b.high);
        b.low = round6(b.low);
        b.close = round6(b.close);
        bars.push(b);
    }
    report.drops.insert("missing_or_nonpositive_close".into(), dropped);
    report.filled = filled;
    report.rows_out = bars.len();
    if bars.is_empty() {
        let names: Vec<&str> = opts.lists.iter().map(|l| l.0.as_str()).collect();
        return Err(DataError::NotFound(format!(
            "the lake holds no equity bar of the members of {} from {}",
            names.join(", "),
            ymd(opts.from)
        )));
    }
    bars.sort_by_key(|b| (b.asset, b.day));
    let days_all: Vec<i64> = bars.iter().map(|b| b.day).collect::<BTreeSet<_>>().into_iter().collect();
    let day_no: HashMap<i64, usize> = days_all.iter().enumerate().map(|(i, d)| (*d, i)).collect();
    let (first_day, last_day) = (days_all[0], *days_all.last().unwrap());
    report.first_day = Some(ymd(first_day));
    report.last_day = Some(ymd(last_day));
    report.days = days_all.len();
    let next_day = |d: i64| day_no.get(&d).and_then(|&i| days_all.get(i + 1)).copied();
    let prev_day = |d: i64| day_no.get(&d).and_then(|&i| i.checked_sub(1)).map(|i| days_all[i]);
    // 3. The symbol runs, each asset's span, and who is gone.
    let mut by_asset: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, b) in bars.iter().enumerate() {
        by_asset.entry(b.asset).or_default().push(i);
    }
    let mut span: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    let mut gone: BTreeMap<i64, (i64, i64)> = BTreeMap::new(); // asset -> (last bar, delisting date)
    let mut intervals: Vec<Interval> = Vec::new();
    for (&a, idx) in &by_asset {
        let (f, l) = (bars[idx[0]].day, bars[*idx.last().unwrap()].day);
        span.insert(a, (f, l));
        if l < last_day {
            gone.insert(a, (l, next_day(l).expect("a day after a bar before the last")));
        }
        let mut runs: Vec<(String, String, i64, i64)> = Vec::new();
        for &i in idx {
            let b = &bars[i];
            match runs.last_mut() {
                Some(r) if r.0 == b.ticker => r.3 = b.day,
                _ => runs.push((b.ticker.clone(), b.source.clone(), b.day, b.day)),
            }
        }
        for (k, r) in runs.iter().enumerate() {
            let to = match runs.get(k + 1) {
                Some(next) => prev_day(next.2),
                None => gone.get(&a).map(|g| g.1),
            };
            intervals.push(Interval {
                asset: a,
                ticker: r.0.clone(),
                source: r.1.clone(),
                from: r.2,
                to,
            });
        }
    }
    // 4. Ticker collisions: the side ending first takes its source symbol.
    let end = |i: &Interval| i.to.unwrap_or(i64::MAX);
    let collisions = |iv: &[Interval]| -> BTreeSet<(i64, String)> {
        let mut by_ticker: BTreeMap<&str, Vec<&Interval>> = BTreeMap::new();
        for i in iv {
            by_ticker.entry(&i.ticker).or_default().push(i);
        }
        let mut out = BTreeSet::new();
        for list in by_ticker.values() {
            for (x, a) in list.iter().enumerate() {
                for b in &list[x + 1..] {
                    if a.asset == b.asset || a.from > end(b) || b.from > end(a) {
                        continue;
                    }
                    let (lo, hi) = if a.asset < b.asset { (a, b) } else { (b, a) };
                    let loser = if end(lo) <= end(hi) { lo } else { hi };
                    out.insert((loser.asset, loser.ticker.clone()));
                }
            }
        }
        out
    };
    let renamed = collisions(&intervals);
    report.ticker_collisions_renamed = renamed.len();
    for i in &mut intervals {
        if renamed.contains(&(i.asset, i.ticker.clone())) {
            i.ticker = if i.source != i.ticker && !i.source.is_empty() {
                i.source.clone()
            } else {
                format!("{}-{}", i.ticker, i.asset)
            };
        }
    }
    let left = collisions(&intervals);
    if !left.is_empty() {
        return Err(DataError::Identity(format!("{} ticker collisions remain after renaming: {:?}", left.len(), left)));
    }
    report.symbol_intervals = intervals.len();
    let mut ivs_of: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (k, i) in intervals.iter().enumerate() {
        ivs_of.entry(i.asset).or_default().push(k);
    }
    let symbol_at = |a: i64, d: i64| -> Option<&Interval> { ivs_of.get(&a)?.iter().map(|&k| &intervals[k]).find(|i| i.covers(d)) };
    // 5. Corporate actions over the span.
    let mut splits: Vec<(i64, i64, f64)> = Vec::new();
    let mut split_on: HashMap<(i64, i64), f64> = HashMap::new();
    let mut divs_raw: Vec<(i64, i64, f64)> = Vec::new();
    let mut split_all = 0;
    let mut div_all = 0;
    let bar_days: BTreeSet<(i64, i64)> = bars.iter().map(|b| (b.asset, b.day)).collect();
    for b in read(lake, "registry/corporate_actions", &["asset_id", "action_type", "ex_date", "div_cash", "split_ratio"], |_| true)? {
        let (asset, kind, ex, cash, ratio) = (
            ints(&b, "asset_id")?,
            strings(&b, "action_type")?,
            days(&b, "ex_date")?,
            nums(&b, "div_cash")?,
            nums(&b, "split_ratio")?,
        );
        for i in 0..b.num_rows() {
            let (Some(a), Some(k), Some(e)) = (asset[i], &kind[i], ex[i]) else { continue };
            if !idset.contains(&a) || e < opts.from || e > last_day {
                continue;
            }
            match k.as_str() {
                "SPLIT" => {
                    split_all += 1;
                    if let Some(r) = ratio[i].filter(|r| *r > 0.0 && *r != 1.0) {
                        split_on.insert((a, e), r);
                        if bar_days.contains(&(a, e)) {
                            splits.push((a, e, r));
                        }
                    }
                }
                "DIV" => {
                    div_all += 1;
                    if let Some(c) = cash[i].filter(|c| *c > 0.0) {
                        divs_raw.push((a, e, c));
                    }
                }
                _ => {}
            }
        }
    }
    splits.sort_by_key(|s| (s.1, s.0));
    report.splits = splits.len();
    report.drops.insert("split_not_on_a_bar_or_unit".into(), split_all - splits.len());
    let mut divs: Vec<(i64, i64, f64)> = Vec::new();
    let mut rescaled = 0;
    for (a, e, c) in divs_raw {
        if symbol_at(a, e).is_none() {
            continue;
        }
        let factor = split_on.get(&(a, e)).copied();
        rescaled += usize::from(factor.is_some());
        divs.push((a, e, round6(c / factor.unwrap_or(1.0))));
    }
    divs.sort_by_key(|d| (d.1, d.0));
    report.dividends = divs.len();
    report.dividends_rescaled_for_same_day_split = rescaled;
    report.drops.insert("dividend_outside_symbol_span_or_nonpositive".into(), div_all - divs.len());
    // 6. Delistings with their inferred reasons.
    let mut delisted: Vec<(i64, i64, String)> = Vec::new();
    for (&a, &(last_bar, date)) in &gone {
        let idx = &by_asset[&a];
        let last_close = bars[*idx.last().unwrap()].close;
        let max60 = idx.iter().rev().take(60).map(|&i| bars[i].close).fold(f64::NEG_INFINITY, f64::max);
        let symbol = symbol_at(a, date).map(|i| i.ticker.clone()).unwrap_or_default();
        let reason = if bankrupt_symbol(&symbol) || last_close < 1.0 {
            "bankruptcy"
        } else if last_close < 0.5 * max60 {
            "other"
        } else {
            "acquisition"
        };
        let _ = last_bar;
        *report.delisting_reasons.entry(reason.into()).or_default() += 1;
        delisted.push((a, date, reason.to_string()));
    }
    // 7. Membership, inclusive, clipped and split around the asset's halts.
    let mut membership: Vec<(i64, &str, i64, Option<i64>)> = Vec::new();
    let mut clipped = 0;
    for &(a, label, mf, mt) in &members {
        if !idset.contains(&a) {
            continue;
        }
        let Some(&(first_bar, last_bar)) = span.get(&a) else { continue };
        for &k in ivs_of.get(&a).into_iter().flatten() {
            let iv = &intervals[k];
            let f = mf.max(iv.from).max(first_bar);
            let member_end = match mt {
                None => i64::MAX,
                Some(t) => prev_day(t).unwrap_or(t - 1),
            };
            let t = member_end.min(iv.to.unwrap_or(i64::MAX)).min(last_bar);
            if f > t {
                continue;
            }
            clipped += 1;
            // The asset's bars in [f, t], in islands of consecutive trading days.
            let mut island: Option<(i64, i64, usize)> = None;
            for &i in &by_asset[&a] {
                let d = bars[i].day;
                if d < f || d > t {
                    continue;
                }
                let n = day_no[&d];
                match island {
                    Some((s, _, last_n)) if n == last_n + 1 => island = Some((s, d, n)),
                    Some((s, e, _)) => {
                        membership.push((a, label, s, Some(e)));
                        island = Some((d, d, n));
                    }
                    None => island = Some((d, d, n)),
                }
            }
            if let Some((s, e, _)) = island {
                let open = mt.is_none() && e >= last_day;
                membership.push((a, label, s, (!open).then_some(e)));
            }
        }
    }
    report.membership_intervals = membership.len();
    report.membership_intervals_split_at_missing_bars = membership.len().saturating_sub(clipped);
    // 8. Classification: the snapshot, from its date on.
    let mut classification: Vec<(i64, i64, Option<i64>, String)> = Vec::new();
    let mut snapshot: Option<i64> = None;
    if let Ok(cb) = read(lake, "classification/classification_full", &["asset_id", "scheme", "level", "code", "snapshot_date"], |_| true) {
        for b in &cb {
            let (asset, scheme, level, code, snap) = (ints(b, "asset_id")?, strings(b, "scheme")?, ints(b, "level")?, strings(b, "code")?, days(b, "snapshot_date")?);
            for i in 0..b.num_rows() {
                let (Some(a), Some(s), Some(l), Some(c), Some(d)) = (asset[i], &scheme[i], level[i], &code[i], snap[i]) else {
                    continue;
                };
                if *s != opts.scheme {
                    continue;
                }
                snapshot = Some(snapshot.map_or(d, |x| x.max(d)));
                if l != 1 || d > last_day || !idset.contains(&a) {
                    continue;
                }
                for &k in ivs_of.get(&a).into_iter().flatten() {
                    let iv = &intervals[k];
                    if iv.to.is_none_or(|t| t >= d) {
                        classification.push((a, d.max(iv.from), iv.to, c.clone()));
                    }
                }
            }
        }
    }
    report.classification_snapshot = snapshot.map(ymd);
    // 9. Reviewed exceptions whose symbol carries an asset on their date.
    let mut exceptions: Vec<serde_json::Value> = Vec::new();
    if let Some(path) = &opts.exceptions {
        let file = std::fs::File::open(path).map_err(|e| DataError::NotFound(format!("{}: {}", path.display(), e)))?;
        for b in ParquetRecordBatchReaderBuilder::try_new(file)?.build()? {
            let b = b?;
            let (test, symbol, date, reason) = (strings(&b, "test")?, strings(&b, "symbol")?, days(&b, "date")?, strings(&b, "reason")?);
            for i in 0..b.num_rows() {
                let (Some(t), Some(s), Some(d), Some(r)) = (&test[i], &symbol[i], date[i], &reason[i]) else {
                    continue;
                };
                if let Some(iv) = intervals.iter().find(|iv| iv.ticker == *s && iv.covers(d)) {
                    exceptions.push(serde_json::json!({ "test": t, "key": iv.asset.to_string(), "symbol": s, "date": ymd(d), "reason": r }));
                }
            }
        }
    }
    report.exceptions = exceptions.len();
    // The relations, keyed by asset_id; daily facts on every trading day.
    let key = |a: i64| a.to_string();
    let t_of = |d: i64| d * DAY;
    let keyed = |keys: Vec<String>, t: Vec<i64>, values: Vec<(&str, ArrayRef)>| -> Result<RecordBatch, DataError> {
        let mut fields = vec![Field::new("A", DataType::Utf8, false), Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false)];
        let mut cols: Vec<ArrayRef> = vec![Arc::new(StringArray::from(keys)), Arc::new(TimestampNanosecondArray::from(t))];
        for (n, a) in values {
            fields.push(Field::new(n, a.data_type().clone(), true));
            cols.push(a);
        }
        Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?)
    };
    let mut relations: Vec<(String, RecordBatch)> = Vec::new();
    let bk: Vec<String> = bars.iter().map(|b| key(b.asset)).collect();
    let bt: Vec<i64> = bars.iter().map(|b| t_of(b.day)).collect();
    for (rel, col, f) in [
        ("equity.open", "O", (|b: &Bar| b.open) as fn(&Bar) -> f64),
        ("equity.high", "H", |b: &Bar| b.high),
        ("equity.low", "L", |b: &Bar| b.low),
        ("equity.close", "P", |b: &Bar| b.close),
        ("equity.volume", "V", |b: &Bar| b.volume),
    ] {
        let v: ArrayRef = Arc::new(Float64Array::from(bars.iter().map(f).collect::<Vec<_>>()));
        relations.push((rel.to_string(), keyed(bk.clone(), bt.clone(), vec![(col, v)])?));
    }
    // The ticker each asset carries on every day of its span through its
    // last bar.
    let mut tk: (Vec<String>, Vec<i64>, Vec<String>) = Default::default();
    for iv in &intervals {
        let last = span[&iv.asset].1;
        for &d in days_all.iter().filter(|d| iv.covers(**d) && **d <= last) {
            tk.0.push(key(iv.asset));
            tk.1.push(t_of(d));
            tk.2.push(iv.ticker.clone());
        }
    }
    relations.push(("equity.ticker".into(), keyed(tk.0, tk.1, vec![("S", Arc::new(StringArray::from(tk.2)) as ArrayRef)])?));
    // Listed on every bar before its delisting.
    let listed: Vec<&Bar> = bars.iter().filter(|b| gone.get(&b.asset).is_none_or(|g| b.day < g.1)).collect();
    relations.push((
        "equity.listed".into(),
        keyed(listed.iter().map(|b| key(b.asset)).collect(), listed.iter().map(|b| t_of(b.day)).collect(), vec![])?,
    ));
    relations.push((
        "equity.split".into(),
        keyed(
            splits.iter().map(|s| key(s.0)).collect(),
            splits.iter().map(|s| t_of(s.1)).collect(),
            vec![("Factor", Arc::new(Float64Array::from(splits.iter().map(|s| s.2).collect::<Vec<_>>())) as ArrayRef)],
        )?,
    ));
    let ex: ArrayRef = Arc::new(TimestampNanosecondArray::from(divs.iter().map(|d| t_of(d.1)).collect::<Vec<_>>()));
    relations.push((
        "equity.dividend".into(),
        keyed(
            divs.iter().map(|d| key(d.0)).collect(),
            divs.iter().map(|d| t_of(d.1)).collect(),
            vec![
                ("Ex", ex.clone()),
                ("Pay", ex),
                ("Amount", Arc::new(Float64Array::from(divs.iter().map(|d| d.2).collect::<Vec<_>>())) as ArrayRef),
            ],
        )?,
    ));
    let mut dl: (Vec<String>, Vec<i64>, Vec<String>) = Default::default();
    for (a, d0, reason) in &delisted {
        for &d in days_all.iter().filter(|d| **d >= *d0) {
            dl.0.push(key(*a));
            dl.1.push(t_of(d));
            dl.2.push(reason.clone());
        }
    }
    relations.push(("equity.delisted".into(), keyed(dl.0, dl.1, vec![("Reason", Arc::new(StringArray::from(dl.2)) as ArrayRef)])?));
    // A member of two lists on a day is two rows, in (day, asset, list)
    // order as the keyed rows are sorted.
    let mut mem: Vec<(i64, i64, &str)> = Vec::new();
    for (a, label, f, t) in &membership {
        for &d in days_all.iter().filter(|d| **d >= *f && t.is_none_or(|t| **d <= t)) {
            mem.push((d, *a, label));
        }
    }
    mem.sort();
    mem.dedup();
    let lists: ArrayRef = Arc::new(StringArray::from(mem.iter().map(|m| m.2).collect::<Vec<_>>()));
    relations.push((
        "equity.member".into(),
        keyed(mem.iter().map(|m| key(m.1)).collect(), mem.iter().map(|m| t_of(m.0)).collect(), vec![("List", lists)])?,
    ));
    let mut cls: (Vec<String>, Vec<i64>, Vec<String>) = Default::default();
    for (a, f, t, code) in &classification {
        for &d in days_all.iter().filter(|d| **d >= *f && t.is_none_or(|t| **d <= t)) {
            cls.0.push(key(*a));
            cls.1.push(t_of(d));
            cls.2.push(code.clone());
        }
    }
    let schemes: ArrayRef = Arc::new(StringArray::from(vec![opts.scheme.clone(); cls.0.len()]));
    relations.push((
        "equity.classification".into(),
        keyed(cls.0, cls.1, vec![("Scheme", schemes), ("Code", Arc::new(StringArray::from(cls.2)) as ArrayRef)])?,
    ));
    if !opts.series.is_empty() {
        for (_, name) in &opts.series {
            report.series_rows.insert(name.clone(), series.keys().filter(|(n, _)| n == name).count());
        }
        let fields = vec![
            Field::new("Name", DataType::Utf8, false),
            Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
            Field::new("V", DataType::Float64, true),
        ];
        let cols: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(series.keys().map(|(n, _)| n.as_str()).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(series.keys().map(|(_, d)| t_of(*d)).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(series.values().map(|v| v.1).collect::<Vec<_>>())),
        ];
        relations.push(("series.value".into(), RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?));
    }
    let securities: Vec<Security> = intervals
        .iter()
        .map(|i| Security {
            key: key(i.asset),
            ticker: i.ticker.clone(),
            from: t_of(i.from),
            to: i.to.map(t_of),
        })
        .collect();
    let mut attributes = BTreeMap::new();
    attributes.insert("calendar".to_string(), "1d".to_string());
    attributes.insert("prices".to_string(), "as_traded".to_string());
    attributes.insert("ticker_history".to_string(), "final".to_string());
    let names: Vec<&str> = opts.lists.iter().map(|l| l.0.as_str()).collect();
    attributes.insert("universe".to_string(), format!("{} members since {}", names.join(", "), ymd(opts.from)));
    if !exceptions.is_empty() {
        attributes.insert("reviewed_exceptions".to_string(), serde_json::Value::Array(exceptions).to_string());
    }
    let mut per: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    per.entry("equity.dividend".into()).or_default().insert("dividend_dates".into(), "ex_only".into());
    per.entry("equity.delisted".into()).or_default().insert("delisting_reason".into(), "inferred".into());
    if let Some(s) = snapshot {
        per.entry("equity.classification".into()).or_default().insert("classification".into(), format!("snapshot {}", ymd(s)));
    }
    Ok((
        Fetched {
            securities,
            terms: vec![],
            relations,
            attributes,
            relation_attributes: per,
            keys: super::Keys::Vendor,
            ticker_columns: vec![],
        },
        report,
    ))
}

/// A time column as nanoseconds (an ingest timestamp's order).
fn days_ns(b: &RecordBatch, name: &str) -> Result<Vec<Option<i64>>, DataError> {
    let t = arrow::compute::cast(column(b, name)?, &DataType::Timestamp(TimeUnit::Nanosecond, None))?;
    let a = t.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
    Ok((0..a.len()).map(|i| (!a.is_null(i)).then(|| a.value(i))).collect())
}

/// The lake as a source: the equity class's daily relations of the
/// members of its lists, and its index series, fetched over a request's
/// span. The last read is kept, so the relations of one purchase read the
/// lake once.
pub struct Norgate {
    lake: PathBuf,
    opts: LakeOptions,
    last: Mutex<Option<(LakeOptions, Fetched)>>,
}

impl Norgate {
    pub fn new(lake: &Path, opts: LakeOptions) -> Norgate {
        Norgate {
            lake: lake.to_path_buf(),
            opts,
            last: Mutex::new(None),
        }
    }

    /// The whole of what the options select, with the read's report.
    pub fn read(&self) -> Result<(Fetched, Report), DataError> {
        read_lake(&self.lake, &self.opts)
    }

    /// The adapter a catalog's source configures (D-7.1): `lake` (the
    /// lake's root), and optionally `lists` (`S&P 500=SPX;S&P MidCap
    /// 400=SP400`), `series` (`$SPXTR=SPXTR`), `from` and `to` (dates),
    /// `limit`, `scheme`, `exceptions` (a reviewed exceptions file).
    pub fn from_settings(settings: &BTreeMap<String, String>) -> Result<Norgate, DataError> {
        let lake = settings.get("lake").ok_or_else(|| DataError::Malformed("a norgate source names its `lake` in its settings".into()))?;
        let mut opts = LakeOptions::default();
        let pairs = |key: &str, v: &str| -> Result<Vec<(String, String)>, DataError> {
            v.split(';')
                .filter(|x| !x.trim().is_empty())
                .map(|x| {
                    x.split_once('=')
                        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                        .ok_or_else(|| DataError::Malformed(format!("the norgate setting `{}` is NAME=LABEL[;NAME=LABEL...], not `{}`", key, x)))
                })
                .collect()
        };
        let date = |key: &str, v: &str| days_of(v).ok_or_else(|| DataError::Malformed(format!("the norgate setting `{}` is a date YYYY-MM-DD, not `{}`", key, v)));
        for (k, v) in settings {
            match k.as_str() {
                "lake" => {}
                "lists" => opts.lists = pairs(k, v)?,
                "series" => opts.series = pairs(k, v)?,
                "from" => opts.from = date(k, v)?,
                "to" => opts.to = Some(date(k, v)?),
                "limit" => opts.limit = Some(v.parse().map_err(|_| DataError::Malformed(format!("the norgate setting `limit` is a count, not `{}`", v)))?),
                "scheme" => opts.scheme = v.clone(),
                "exceptions" => opts.exceptions = Some(PathBuf::from(v)),
                _ => return Err(DataError::Malformed(format!("the norgate adapter has no setting `{}`", k))),
            }
        }
        Ok(Norgate::new(Path::new(lake), opts))
    }
}

/// The equity relations the lake provides (the tickers also come with the
/// securities, into the registry's `ticker` spans).
pub const RELATIONS: [&str; 12] = [
    "open",
    "high",
    "low",
    "close",
    "volume",
    "listed",
    "ticker",
    "split",
    "dividend",
    "delisted",
    "member",
    "classification",
];

impl Adapter for Norgate {
    fn vendor(&self) -> &str {
        "norgate"
    }

    fn offers(&self) -> Vec<Offer> {
        let labels: Vec<&str> = self.opts.lists.iter().map(|l| l.1.as_str()).collect();
        let offer = |class: &str, relations: Vec<String>, scope: String| Offer {
            class: class.into(),
            relations,
            calendar: "1d".into(),
            from: Some(ymd(self.opts.from)),
            to: self.opts.to.map(ymd),
            scope,
            has_receipt: false,
            supplies: vec![],
            attributes: BTreeMap::new(),
            instruments: None,
        };
        let mut out = vec![offer("equity", RELATIONS.iter().map(|r| r.to_string()).collect(), format!("list({})", labels.join(", ")))];
        if !self.opts.series.is_empty() {
            let names: Vec<&str> = self.opts.series.iter().map(|s| s.1.as_str()).collect();
            out.push(offer("series", vec!["value".into()], format!("names({})", names.join(", "))));
        }
        out
    }

    fn fetch(&self, req: &FetchRequest) -> Result<Fetched, DataError> {
        let mut opts = self.opts.clone();
        opts.from = opts.from.max(req.from.div_euclid(DAY));
        opts.to = Some(opts.to.map_or(req.to.div_euclid(DAY), |t| t.min(req.to.div_euclid(DAY))));
        let cached = self.last.lock().ok().and_then(|l| l.as_ref().filter(|(o, _)| *o == opts).map(|(_, f)| f.clone()));
        let mut got = match cached {
            Some(f) => f,
            None => {
                let (f, _) = read_lake(&self.lake, &opts)?;
                if let Ok(mut l) = self.last.lock() {
                    *l = Some((opts, f.clone()));
                }
                f
            }
        };
        got.relations.retain(|(r, _)| req.relations.contains(r));
        Ok(got)
    }
}
