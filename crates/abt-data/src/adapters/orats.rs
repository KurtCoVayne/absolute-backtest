//! ORATS (D-7.7): the option class from ORATS's strikes, end-of-day or
//! intraday snapshots per underlying, read from files in the layout of its
//! Data API or from the API itself (`/datav2/hist/strikes`, one request per
//! underlying and trade date, the token in the query string).
//!
//! A row of the strikes layout is one underlying's (trade date, expiry,
//! strike), with its call's and its put's quotes and the call's greeks. It
//! becomes two contracts, each an instrument keyed by its OCC symbol
//! (D-2.4's assignment rule for options):
//!
//! - `option.quote`: the side's bid, ask, sizes, volume and open interest
//!   (the layout has no last trade: `Last` is null);
//! - `option.greeks`: the side's mid IV (`callMidIv`, `putMidIv`, else the
//!   smoothed `smvVol`), and the call's delta, gamma, theta, vega and rho.
//!   The put's delta is the call's less one, its gamma and vega the call's;
//!   the layout gives no put theta or rho, which are null. ORATS states theta
//!   per day and vega and rho per point of volatility and rate; they are
//!   stored per year and per unit, as the kernel's model computes them
//!   (ERRATA E-70, for the owner to check against a real file);
//! - `option_spec`: the contract's terms, from its first row. The style and
//!   the settlement are a setting per underlying (American, physical, PM
//!   when unstated: an equity option's);
//! - `chain_index`: the contracts of the underlying's snapshot;
//! - `series.value`: the underlying's price at the snapshot (`stockPrice`),
//!   named by its ticker.
//!
//! The underlying is the equity instrument whose ticker span carries the
//! ticker at the row's date (A-D24: an unknown ticker rejects the read).
//! A row is received at its quote time (`quoteDate`, else `updatedAt`, else
//! 21:00 UTC of the trade date, an hour after a summer close in New York).
//! End-of-day rows are labelled at their trade date; with `snapshots` at
//! their quote time.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arrow::array::{ArrayRef, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{Field, Schema};

use super::{Adapter, FetchRequest, Fetched, Keys};
use crate::adapter::{Security, Terms};
use crate::error::DataError;
use crate::options::occ_symbol;
use crate::resolver::catalog::Offer;

const DAY: i64 = 86_400_000_000_000;
const HOUR: i64 = 3_600_000_000_000;

/// What a read keeps and how it states the contracts.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The underlyings read; every one a file holds when empty.
    pub tickers: Vec<String>,
    /// The first and last trade dates kept.
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Shares per contract.
    pub multiplier: f64,
    /// Per underlying: its options' style, settlement and settlement time
    /// (`SPX=european:cash:am`).
    pub terms: BTreeMap<String, (String, String, String)>,
    /// Rows labelled at their quote time (intraday snapshots), not their
    /// trade date.
    pub snapshots: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            tickers: Vec::new(),
            from: None,
            to: None,
            multiplier: 100.0,
            terms: BTreeMap::new(),
            snapshots: false,
        }
    }
}

/// One side of a strike's row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Side {
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub bid_size: Option<f64>,
    pub ask_size: Option<f64>,
    pub volume: Option<f64>,
    pub open_interest: Option<f64>,
    pub iv: Option<f64>,
}

/// A row of the strikes layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub ticker: String,
    pub trade_date: i64,
    pub expiry: i64,
    pub strike: f64,
    pub stock_price: Option<f64>,
    pub call: Side,
    pub put: Side,
    pub smv_vol: Option<f64>,
    pub delta: Option<f64>,
    pub gamma: Option<f64>,
    pub theta: Option<f64>,
    pub vega: Option<f64>,
    pub rho: Option<f64>,
    pub quoted_at: Option<i64>,
}

/// What a read did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub files: usize,
    pub rows_in: usize,
    pub rows_kept: usize,
    pub contracts: usize,
    pub snapshots: usize,
    pub tickers: BTreeSet<String>,
}

fn number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

fn text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

/// A row from the layout's fields, by their ORATS names.
fn row_of(get: &dyn Fn(&str) -> Option<serde_json::Value>, what: &str) -> Result<Row, DataError> {
    let num = |k: &str| get(k).as_ref().and_then(number);
    let date = |k: &str| -> Result<i64, DataError> {
        let s = get(k).as_ref().and_then(text).ok_or_else(|| DataError::Malformed(format!("{}: no `{}`", what, k)))?;
        abt_wire::scalar::parse_timestamp(&s).ok_or_else(|| DataError::Malformed(format!("{}: `{}` is `{}`, not a date", what, k, s)))
    };
    let instant = |k: &str| get(k).as_ref().and_then(text).and_then(|s| abt_wire::scalar::parse_timestamp(&s));
    let side = |p: &str| Side {
        bid: num(&format!("{}BidPrice", p)),
        ask: num(&format!("{}AskPrice", p)),
        bid_size: num(&format!("{}BidSize", p)),
        ask_size: num(&format!("{}AskSize", p)),
        volume: num(&format!("{}Volume", p)),
        open_interest: num(&format!("{}OpenInterest", p)),
        iv: num(&format!("{}MidIv", p)),
    };
    let trade_date = date("tradeDate")?;
    Ok(Row {
        ticker: get("ticker").as_ref().and_then(text).ok_or_else(|| DataError::Malformed(format!("{}: no `ticker`", what)))?,
        trade_date: trade_date.div_euclid(DAY) * DAY,
        expiry: date("expirDate")?.div_euclid(DAY) * DAY,
        strike: num("strike").filter(|k| *k > 0.0).ok_or_else(|| DataError::Malformed(format!("{}: no positive `strike`", what)))?,
        stock_price: num("stockPrice").or_else(|| num("spotPrice")),
        call: side("call"),
        put: side("put"),
        smv_vol: num("smvVol"),
        delta: num("delta"),
        gamma: num("gamma"),
        theta: num("theta"),
        vega: num("vega"),
        rho: num("rho"),
        quoted_at: instant("quoteDate").or_else(|| instant("updatedAt")),
    })
}

/// The values of a CSV line, a field in double quotes holding commas.
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// The rows of a strikes file or response: JSON (`{"data": [...]}`, or an
/// array of rows) or CSV with a header line.
pub fn parse(text: &str, what: &str) -> Result<Vec<Row>, DataError> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| DataError::Malformed(format!("{}: {}", what, e)))?;
        let rows = match &v {
            serde_json::Value::Array(a) => a.clone(),
            serde_json::Value::Object(o) => match o.get("data") {
                Some(serde_json::Value::Array(a)) => a.clone(),
                _ => return Err(DataError::Malformed(format!("{}: a JSON object without its `data` array", what))),
            },
            _ => return Err(DataError::Malformed(format!("{}: not a strikes response", what))),
        };
        return rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let get = |k: &str| r.get(k).cloned();
                row_of(&get, &format!("{} row {}", what, i))
            })
            .collect();
    }
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(header) = lines.next() else { return Ok(Vec::new()) };
    let names: Vec<String> = csv_fields(header).into_iter().map(|s| s.trim().to_string()).collect();
    lines
        .enumerate()
        .map(|(i, line)| {
            let values = csv_fields(line);
            if values.len() != names.len() {
                return Err(DataError::Malformed(format!("{} line {}: {} fields, the header has {}", what, i + 2, values.len(), names.len())));
            }
            let get = |k: &str| names.iter().position(|n| n == k).map(|j| serde_json::Value::String(values[j].clone()));
            row_of(&get, &format!("{} line {}", what, i + 2))
        })
        .collect()
}

/// A contract's terms as `option_spec` records them, from its first row.
struct SpecRow<'a> {
    occ: &'a String,
    listed: i64,
    received: i64,
    row: &'a Row,
    right: &'a str,
    style: String,
    settlement: String,
    time: String,
}

/// A contract's side of a row: `call` or `put`.
fn sides(r: &Row) -> [(&'static str, &Side); 2] {
    [("call", &r.call), ("put", &r.put)]
}

/// The option class's relations of a read's rows (the module's mapping).
pub fn strikes(rows: &[Row], opts: &Options) -> Result<(Fetched, Report), DataError> {
    let mut report = Report {
        rows_in: rows.len(),
        ..Report::default()
    };
    let kept: Vec<&Row> = rows
        .iter()
        .filter(|r| opts.tickers.is_empty() || opts.tickers.contains(&r.ticker))
        .filter(|r| opts.from.is_none_or(|f| r.trade_date >= f) && opts.to.is_none_or(|t| r.trade_date <= t))
        .collect();
    report.rows_kept = kept.len();
    // The quote's column vectors.
    #[derive(Default)]
    struct Cols {
        key: Vec<String>,
        t: Vec<i64>,
        r: Vec<i64>,
        v: Vec<Vec<Option<f64>>>,
    }
    let mut quote = Cols::default();
    let mut greeks = Cols::default();
    let mut spec: BTreeMap<String, (i64, i64, &Row, &'static str)> = BTreeMap::new();
    let mut chain: BTreeSet<(String, i64, String, i64)> = BTreeSet::new();
    let mut value: BTreeMap<(String, i64), (f64, i64)> = BTreeMap::new();
    let mut snapshots: BTreeSet<(String, i64)> = BTreeSet::new();
    for r in kept {
        let received = r.quoted_at.unwrap_or(r.trade_date + 21 * HOUR);
        let t = if opts.snapshots { received } else { r.trade_date };
        report.tickers.insert(r.ticker.clone());
        snapshots.insert((r.ticker.clone(), t));
        if let Some(s) = r.stock_price {
            value.insert((r.ticker.clone(), t), (s, received));
        }
        for (right, side) in sides(r) {
            let occ = occ_symbol(&r.ticker, r.expiry, right, r.strike);
            spec.entry(occ.clone()).or_insert((t, received, r, right));
            chain.insert((r.ticker.clone(), t, occ.clone(), received));
            quote.key.push(occ.clone());
            quote.t.push(t);
            quote.r.push(received);
            quote.v.push(vec![side.bid, side.ask, side.bid_size, side.ask_size, None, side.volume, side.open_interest]);
            let call = right == "call";
            greeks.key.push(occ);
            greeks.t.push(t);
            greeks.r.push(received);
            greeks.v.push(vec![
                side.iv.or(r.smv_vol),
                r.delta.map(|d| if call { d } else { d - 1.0 }),
                r.gamma,
                r.theta.filter(|_| call).map(|x| x * 365.0),
                r.vega.map(|x| x * 100.0),
                r.rho.filter(|_| call).map(|x| x * 100.0),
            ]);
        }
    }
    report.contracts = spec.len();
    report.snapshots = snapshots.len();
    let times = |v: Vec<i64>| -> ArrayRef { Arc::new(TimestampNanosecondArray::from(v)) };
    let texts = |v: Vec<String>| -> ArrayRef { Arc::new(StringArray::from(v)) };
    let nums = |v: Vec<Option<f64>>| -> ArrayRef { Arc::new(Float64Array::from(v)) };
    let batch = |cols: Vec<(&str, ArrayRef)>| -> Result<RecordBatch, DataError> {
        let fields: Vec<Field> = cols.iter().map(|(n, a)| Field::new(*n, a.data_type().clone(), true)).collect();
        Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), cols.into_iter().map(|(_, a)| a).collect())?)
    };
    let keyed = |c: Cols, names: &[&'static str]| -> Result<RecordBatch, DataError> {
        let mut cols: Vec<(&str, ArrayRef)> = vec![("A", texts(c.key)), ("T", times(c.t))];
        for (j, n) in names.iter().enumerate() {
            cols.push((n, nums(c.v.iter().map(|v| v[j]).collect())));
        }
        cols.push(("r", times(c.r)));
        batch(cols)
    };
    let mut relations = vec![
        ("option.quote".to_string(), keyed(quote, &["Bid", "Ask", "BidSize", "AskSize", "Last", "Volume", "OI"])?),
        ("option.greeks".to_string(), keyed(greeks, &["IV", "Delta", "Gamma", "Theta", "Vega", "Rho"])?),
    ];
    // The contracts' terms, at their first row.
    let terms_of = |ticker: &str| -> (String, String, String) { opts.terms.get(ticker).cloned().unwrap_or_else(|| ("american".into(), "physical".into(), "pm".into())) };
    let mut terms = Vec::new();
    let mut spec_rows: Vec<SpecRow> = Vec::new();
    for (occ, (t, received, r, right)) in &spec {
        let (style, settlement, time) = terms_of(&r.ticker);
        spec_rows.push(SpecRow {
            occ,
            listed: *t,
            received: *received,
            row: r,
            right,
            style,
            settlement,
            time,
        });
        terms.push(Terms {
            key: occ.clone(),
            class: "option".into(),
            multiplier: Some(opts.multiplier),
            commission_per_contract: None,
        });
    }
    let col_s = |f: &dyn Fn(&SpecRow) -> String| texts(spec_rows.iter().map(f).collect());
    relations.push((
        "option_spec".to_string(),
        batch(vec![
            ("id", col_s(&|x| x.occ.clone())),
            ("listed", times(spec_rows.iter().map(|x| x.listed).collect())),
            ("underlying", col_s(&|x| x.row.ticker.clone())),
            ("right", col_s(&|x| x.right.to_string())),
            ("strike", nums(spec_rows.iter().map(|x| Some(x.row.strike)).collect())),
            ("expiry", times(spec_rows.iter().map(|x| x.row.expiry).collect())),
            ("style", col_s(&|x| x.style.clone())),
            ("settlement", col_s(&|x| x.settlement.clone())),
            ("multiplier", nums(vec![Some(opts.multiplier); spec_rows.len()])),
            ("occ", col_s(&|x| x.occ.clone())),
            ("settlement_time", col_s(&|x| x.time.clone())),
            ("r", times(spec_rows.iter().map(|x| x.received).collect())),
        ])?,
    ));
    let chain: Vec<&(String, i64, String, i64)> = chain.iter().collect();
    relations.push((
        "chain_index".to_string(),
        batch(vec![
            ("underlying", texts(chain.iter().map(|c| c.0.clone()).collect())),
            ("t", times(chain.iter().map(|c| c.1).collect())),
            ("id", texts(chain.iter().map(|c| c.2.clone()).collect())),
            ("r", times(chain.iter().map(|c| c.3).collect())),
        ])?,
    ));
    relations.push((
        "series.value".to_string(),
        batch(vec![
            ("Name", texts(value.keys().map(|k| k.0.clone()).collect())),
            ("T", times(value.keys().map(|k| k.1).collect())),
            ("V", nums(value.values().map(|v| Some(v.0)).collect())),
            ("r", times(value.values().map(|v| v.1).collect())),
        ])?,
    ));
    let mut attributes = BTreeMap::new();
    attributes.insert("receipt".to_string(), "recorded".to_string());
    attributes.insert("calendar".to_string(), if opts.snapshots { "snapshot" } else { "1d" }.to_string());
    let mut per: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    per.entry("option.greeks".into())
        .or_default()
        .insert("greeks_units".into(), "theta per year, vega and rho per unit, from orats per day and per point".into());
    per.entry("option.quote".into()).or_default().insert("last".into(), "absent".into());
    // A contract's ticker is its OCC symbol from its first row on (D-2.4):
    // what a program's literal names it by. The symbol carries the expiry,
    // so it names no other contract after this one expires.
    let securities: Vec<Security> = spec
        .iter()
        .map(|(occ, (t, ..))| Security {
            key: occ.clone(),
            ticker: occ.clone(),
            from: *t,
            to: None,
        })
        .collect();
    Ok((
        Fetched {
            securities,
            terms,
            relations,
            attributes,
            relation_attributes: per,
            keys: Keys::Vendor,
            ticker_columns: vec![("option_spec".into(), "underlying".into()), ("chain_index".into(), "underlying".into())],
        },
        report,
    ))
}

/// Where the adapter reads from.
#[derive(Clone, Debug, PartialEq)]
pub enum Locator {
    /// Strikes files (`.csv`, `.json`): a file, or a directory searched
    /// through.
    Files(PathBuf),
    /// The Data API at `base` (`https://api.orats.io`), the token read from
    /// the environment variable `token_env`.
    Api { base: String, token_env: String },
}

/// ORATS as a source.
pub struct Orats {
    locator: Locator,
    opts: Options,
    last: Mutex<Option<(Options, Fetched)>>,
}

fn files_under(p: &Path, out: &mut Vec<PathBuf>) -> Result<(), DataError> {
    if p.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for e in entries {
            files_under(&e, out)?;
        }
    } else if p.extension().is_some_and(|x| x == "csv" || x == "json") {
        out.push(p.to_path_buf());
    }
    Ok(())
}

/// The weekdays of `[from, to]` (the trade dates a request asks for).
fn weekdays(from: i64, to: i64) -> Vec<i64> {
    let mut out = Vec::new();
    let mut d = from.div_euclid(DAY);
    while d <= to.div_euclid(DAY) {
        // 1970-01-01 was a Thursday.
        if !matches!((d + 4).rem_euclid(7), 0 | 6) {
            out.push(d * DAY);
        }
        d += 1;
    }
    out
}

fn ymd(t: i64) -> String {
    let (y, m, d) = abt_wire::scalar::civil_from_days(t.div_euclid(DAY));
    format!("{:04}-{:02}-{:02}", y, m, d)
}

impl Orats {
    pub fn new(locator: Locator, opts: Options) -> Orats {
        Orats {
            locator,
            opts,
            last: Mutex::new(None),
        }
    }

    /// The adapter a catalog's source configures (D-7.1): `files` (a path)
    /// or `api` (the Data API's base URL) with `token_env` (`ORATS_TOKEN` by
    /// default); `tickers` (`SPY,SPX`), `terms` (`SPX=european:cash:am`),
    /// `multiplier`, `snapshots` (`true` for intraday rows), `from` and `to`
    /// (dates).
    pub fn from_settings(settings: &BTreeMap<String, String>) -> Result<Orats, DataError> {
        let bad = |k: &str, v: &str, want: &str| DataError::Malformed(format!("the orats setting `{}` is {}, not `{}`", k, want, v));
        let mut opts = Options::default();
        let (mut files, mut base, mut token_env) = (None, None, "ORATS_TOKEN".to_string());
        for (k, v) in settings {
            match k.as_str() {
                "files" => files = Some(PathBuf::from(v)),
                "api" => base = Some(v.clone()),
                "token_env" => token_env = v.clone(),
                "tickers" => opts.tickers = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                "multiplier" => opts.multiplier = v.parse().map_err(|_| bad(k, v, "a number"))?,
                "snapshots" => opts.snapshots = v == "true",
                "from" => opts.from = Some(abt_wire::scalar::parse_timestamp(v).ok_or_else(|| bad(k, v, "a date"))?),
                "to" => opts.to = Some(abt_wire::scalar::parse_timestamp(v).ok_or_else(|| bad(k, v, "a date"))?),
                "terms" => {
                    for part in v.split(';').filter(|x| !x.trim().is_empty()) {
                        let (ticker, rest) = part.split_once('=').ok_or_else(|| bad(k, v, "TICKER=STYLE:SETTLEMENT:TIME[;...]"))?;
                        let f: Vec<&str> = rest.split(':').map(str::trim).collect();
                        match f.as_slice() {
                            [style, settlement, time] if ["american", "european"].contains(style) && ["physical", "cash"].contains(settlement) && ["am", "pm"].contains(time) => {
                                opts.terms.insert(ticker.trim().to_string(), (style.to_string(), settlement.to_string(), time.to_string()));
                            }
                            _ => return Err(bad(k, part, "TICKER=american|european:physical|cash:am|pm")),
                        }
                    }
                }
                _ => return Err(DataError::Malformed(format!("the orats adapter has no setting `{}`", k))),
            }
        }
        let locator = match (files, base) {
            (Some(f), None) => Locator::Files(f),
            (None, Some(b)) => Locator::Api { base: b, token_env },
            _ => return Err(DataError::Malformed("an orats source names `files` or `api`, one of them".into())),
        };
        Ok(Orats::new(locator, opts))
    }

    /// What the locator holds over the options' window, made the class's
    /// relations, with the read's report. The API is asked once per
    /// underlying and trade date (D-7.7).
    pub fn read(&self, opts: &Options) -> Result<(Fetched, Report), DataError> {
        let mut rows = Vec::new();
        let mut files = 0;
        match &self.locator {
            Locator::Files(p) => {
                let mut paths = Vec::new();
                files_under(p, &mut paths)?;
                for f in &paths {
                    rows.extend(parse(&std::fs::read_to_string(f)?, &f.display().to_string())?);
                    files += 1;
                }
            }
            Locator::Api { base, token_env } => {
                let token = std::env::var(token_env).map_err(|_| DataError::Unresolved(format!("the ORATS token is read from ${}, which is not set", token_env)))?;
                let (Some(from), Some(to)) = (opts.from, opts.to) else {
                    return Err(DataError::Malformed("an ORATS API read names its span (`from` and `to`)".into()));
                };
                if opts.tickers.is_empty() {
                    return Err(DataError::Malformed("an ORATS API read names its `tickers`".into()));
                }
                for ticker in &opts.tickers {
                    for day in weekdays(from, to) {
                        let url = format!(
                            "{}/datav2/hist/strikes?token={}&ticker={}&tradeDate={}",
                            base.trim_end_matches('/'),
                            super::http::urlencode(&token),
                            super::http::urlencode(ticker),
                            ymd(day)
                        );
                        let body = super::http::get(&url)?;
                        rows.extend(parse(&String::from_utf8_lossy(&body), &format!("orats {} {}", ticker, ymd(day)))?);
                        files += 1;
                    }
                }
            }
        }
        let (f, mut report) = strikes(&rows, opts)?;
        report.files = files;
        Ok((f, report))
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }
}

/// The option relations the adapter provides.
pub const OPTION_RELATIONS: [&str; 2] = ["quote", "greeks"];

impl Adapter for Orats {
    fn vendor(&self) -> &str {
        "orats"
    }

    fn offers(&self) -> Vec<Offer> {
        vec![Offer {
            class: "option".into(),
            relations: OPTION_RELATIONS.iter().map(|r| r.to_string()).collect(),
            calendar: "1d".into(),
            from: self.opts.from.map(ymd),
            to: self.opts.to.map(ymd),
            scope: "chain".into(),
            has_receipt: true,
            supplies: vec!["iv".into(), "greeks".into(), "oi".into()],
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
        // The registry's relations and the underlying's price travel with
        // the quotes they key.
        got.relations
            .retain(|(r, _)| req.relations.contains(r) || matches!(r.as_str(), "option_spec" | "chain_index" | "series.value"));
        Ok(got)
    }
}
