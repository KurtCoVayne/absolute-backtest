//! Environment instances from Parquet files, and a deterministic synthetic
//! market for tests and demos.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::check::Program;
use crate::ir::*;
use crate::kernel::time::{bucket, days_from_civil, format_timestamp, weekday, DAY};
use crate::kernel::{Dataset, Security, Sym, Value};
use crate::table::{write_table, Col, Table};

/// Load one Parquet file per primitive relation of the program's environment
/// from `dir` (`<relation>.parquet`, columns named after the signature's
/// arguments; typed or text columns, see `table`).
///
/// A row is identified by its inputs, its key and its entity-typed outputs
/// (the identity columns of section 3; `universe(-A, @T)` enumerates A), and
/// the value outputs are bound by the call: a second row for the same
/// identity with different outputs is an error naming both rows, and an
/// identical row is dropped. The temporal key is stored as the label of the
/// bucket containing it at the relation's resolution (section 3
/// "Resolution": at @1d the trading date), so `2022-01-03T16:00:00` and
/// `2022-01-03` label one @1d bar. A missing or empty file leaves the
/// relation empty and is reported in the returned notes.
///
/// Timestamps are bar labels, and a label is the bar's close instant
/// (section 3): a 09:30 to 09:31 minute bar is `09:31`, a session's last bar
/// `16:00`, a daily bar its date. The loader does not check the intraday
/// convention; open-labelled minute data misaligns every resampled bucket by
/// one bar.
pub fn load_parquet_dir(prog: &Program, dir: &Path) -> Result<(Dataset, Vec<String>), String> {
    if !dir.is_dir() {
        return Err(format!("{}: directory not found", dir.display()));
    }
    let mut ds = Dataset::new();
    let mut notes = Vec::new();
    // The security table (data-bundle doc, section 3): `securities.parquet`
    // with `id,ticker,from,to` (`to` null while the ticker is still carried)
    // and the optional contract columns. With it, every equity field is a
    // security id; without it, the ticker is the id.
    let table = dir.join("securities.parquet");
    if table.exists() {
        read_securities(&mut ds, &table)?;
    }
    let ids: HashSet<Sym> = ds.securities.iter().map(|s| s.id).collect();
    for (name, sig) in &prog.relations {
        if !matches!(sig.kind, Kind::Primitive { .. }) {
            continue;
        }
        let path = dir.join(format!("{}.parquet", name));
        if !path.exists() {
            if name == "ticker" {
                // Derived from the security table after the other relations load.
                continue;
            }
            notes.push(format!("no file {}; relation left empty", path.display()));
            continue;
        }
        let t = Table::read(&path)?;
        let cols: Vec<usize> = sig
            .args
            .iter()
            .map(|a| t.col(&a.name.to_lowercase()).map_err(|_| format!("{}: no column `{}`", path.display(), a.name)))
            .collect::<Result<_, _>>()?;
        // An optional `available_at` column (data-bundle doc, section 2): when
        // the tuple could be acted on, at or after its own bar's close.
        let avail_col = t.col("available_at").ok();
        let key_pos = sig.key_pos();
        // Inputs, the key and entity-typed outputs identify a row (section 3:
        // `universe(-A, @T)` enumerates A); the value outputs are a function of them.
        let identity: Vec<usize> = sig.args.iter().enumerate().filter(|(_, a)| a.mode != Mode::Out || a.ty.is_entity()).map(|(i, _)| i).collect();
        // The identity columns of every row kept so far, with its row and tuple.
        let mut seen: HashMap<Vec<Value>, (usize, Vec<Value>)> = HashMap::new();
        for row in 0..t.len() {
            let mut tuple = Vec::with_capacity(sig.args.len());
            for (i, (arg, &c)) in sig.args.iter().zip(cols.iter()).enumerate() {
                let mut v = cell_value(&mut ds, &arg.ty, &t, c, row)?;
                if let (true, Value::Equity(s)) = (!ids.is_empty(), &v) {
                    if !ids.contains(s) {
                        return Err(format!("{}: `{}` is not a security id of securities.parquet", t.at(row), ds.symbols.name(*s)));
                    }
                }
                if let (Some(res), Value::Time(tt)) = (sig.res.filter(|_| key_pos == Some(i)), &v) {
                    v = Value::Time(bucket(res, *tt));
                }
                tuple.push(v);
            }
            // A price is positive (section 6, executor policy: a non-positive
            // price is a data error, never a book state), except a future's,
            // whose back-adjusted series may cross zero.
            for (i, arg) in sig.args.iter().enumerate() {
                if let (Ty::Quantity(d), Value::Num(x)) = (&arg.ty, &tuple[i]) {
                    if d.c2 == 2 && d.s2 == -2 && d.t2 == 0 && *x <= 0.0 && !tuple.iter().any(|v| matches!(v, Value::Equity(s) if ds.is_future(*s))) {
                        return Err(format!("{}: `{}` is not a positive price for `{}`", t.at(row), x, arg.name));
                    }
                }
            }
            let id: Vec<Value> = identity.iter().map(|&i| tuple[i].clone()).collect();
            let avail = match avail_col {
                Some(c) => t.time_opt(c, row)?,
                None => None,
            };
            match seen.get(&id) {
                Some((_, prev)) if *prev == tuple => continue,
                Some((first, _)) => {
                    let shown: Vec<String> = id.iter().map(|v| field(&ds, v)).collect();
                    return Err(format!(
                        "{}: duplicate tuple for ({}) with different outputs; row {} already binds them (`{}` is a function of its inputs and key)",
                        t.at(row),
                        shown.join(", "),
                        first + 1,
                        name
                    ));
                }
                None => {
                    seen.insert(id, (row, tuple.clone()));
                    match avail {
                        Some(a) => ds.add_available(name, tuple, a),
                        None => ds.add(name, tuple),
                    }
                }
            }
        }
        if t.is_empty() {
            notes.push(format!("{} has no rows; relation left empty", path.display()));
        }
    }
    if prog.relations.get("ticker").map(|s| matches!(s.kind, Kind::Primitive { .. })).unwrap_or(false) && !dir.join("ticker.parquet").exists() {
        ds.derive_tickers();
    }
    Ok((ds, notes))
}

/// Read a security table (`id,ticker,from,to` and optionally `multiplier`,
/// `asset_class`, `commission_per_contract`) into the dataset and check its
/// identities.
pub fn read_securities(ds: &mut Dataset, path: &Path) -> Result<(), String> {
    let t = Table::read(path)?;
    let (ci, ct, cf, cto) = (t.col("id")?, t.col("ticker")?, t.col("from")?, t.col("to")?);
    let (cm, ca, cc) = (t.col("multiplier").ok(), t.col("asset_class").ok(), t.col("commission_per_contract").ok());
    for row in 0..t.len() {
        let (Some(id), Some(ticker)) = (t.text(ci, row), t.text(ct, row)) else {
            return Err(format!("{}: id and ticker must be non-empty", t.at(row)));
        };
        let from = t.time(cf, row)?;
        let to = t.time_opt(cto, row)?;
        ds.add_security(&id, &ticker, from, to);
        let mut contract = crate::kernel::Contract::default();
        if let Some(c) = cm {
            contract.multiplier = t.number_opt(c, row)?.unwrap_or(1.0);
        }
        if let Some(c) = ca {
            contract.future = t.text(c, row).map(|s| s == "future").unwrap_or(false);
        }
        if let Some(c) = cc {
            contract.commission_per_contract = t.number_opt(c, row)?;
        }
        if contract != crate::kernel::Contract::default() {
            let sym = ds.intern(&id);
            ds.contracts.insert(sym, contract);
        }
    }
    let problems = check_identities(ds);
    if !problems.is_empty() {
        return Err(format!("{}: {}", path.display(), problems.join("; ")));
    }
    Ok(())
}

/// Write the security table (the inverse of `read_securities`); nothing
/// without securities.
pub fn write_securities(ds: &Dataset, path: &Path) -> Result<(), String> {
    if ds.securities.is_empty() {
        return Ok(());
    }
    let ss = &ds.securities;
    let contract = |s: &Security| ds.contracts.get(&s.id).cloned().unwrap_or_default();
    let mut cols = vec![
        ("id", Col::Str(ss.iter().map(|s| Some(ds.symbols.name(s.id).to_string())).collect())),
        ("ticker", Col::Str(ss.iter().map(|s| Some(s.ticker.clone())).collect())),
        ("from", Col::Time(ss.iter().map(|s| Some(s.from)).collect())),
        ("to", Col::Time(ss.iter().map(|s| s.to).collect())),
    ];
    if !ds.contracts.is_empty() {
        cols.push(("multiplier", Col::Float(ss.iter().map(|s| Some(contract(s).multiplier)).collect())));
        cols.push((
            "asset_class",
            Col::Str(ss.iter().map(|s| Some(if contract(s).future { "future" } else { "equity" }.to_string())).collect()),
        ));
        cols.push(("commission_per_contract", Col::Float(ss.iter().map(|s| contract(s).commission_per_contract).collect())));
    }
    write_table(path, cols)
}

/// The identity bundle tests (data-bundle doc, section 3): every interval
/// ends after it starts, one id carries one ticker at a time, and one ticker
/// is carried by one id at a time.
pub fn check_identities(ds: &Dataset) -> Vec<String> {
    let mut out = Vec::new();
    let show = |s: &Security| -> String {
        format!(
            "{} {} {}..{}",
            ds.symbols.name(s.id),
            s.ticker,
            format_timestamp(s.from),
            s.to.map(format_timestamp).unwrap_or_default()
        )
    };
    for s in &ds.securities {
        if s.to.map(|to| to <= s.from).unwrap_or(false) {
            out.push(format!("{}: the interval ends before it starts", show(s)));
        }
    }
    let overlap = |a: &Security, b: &Security| -> bool { a.from < b.to.unwrap_or(i64::MAX) && b.from < a.to.unwrap_or(i64::MAX) };
    for (i, a) in ds.securities.iter().enumerate() {
        for b in &ds.securities[i + 1..] {
            if a.id == b.id && overlap(a, b) {
                out.push(format!("id {}: ticker intervals overlap ({} and {})", ds.symbols.name(a.id), show(a), show(b)));
            }
            if a.id != b.id && a.ticker == b.ticker && overlap(a, b) {
                out.push(format!(
                    "ticker {} is carried by {} and {} at once ({} and {})",
                    a.ticker,
                    ds.symbols.name(a.id),
                    ds.symbols.name(b.id),
                    show(a),
                    show(b)
                ));
            }
        }
    }
    out
}

/// A value as text, for messages.
fn field(ds: &Dataset, v: &Value) -> String {
    match v {
        Value::Equity(s) => ds.symbols.name(*s).to_string(),
        Value::Label(s) => ds.labels.name(*s).to_string(),
        Value::Time(t) => format_timestamp(*t),
        Value::Num(x) => format!("{}", x),
        Value::Count(c) => format!("{}", c),
        Value::Dur(d) => format!("{}", d),
        Value::Decision(_) => String::new(),
    }
}

/// A cell of a table as a value of the argument's type.
fn cell_value(ds: &mut Dataset, ty: &Ty, t: &Table, c: usize, row: usize) -> Result<Value, String> {
    Ok(match ty {
        Ty::Equity => Value::Equity(ds.intern(&t.string(c, row)?)),
        Ty::Label => Value::Label(ds.intern_label(&t.string(c, row)?)),
        Ty::Timestamp => Value::Time(t.time(c, row)?),
        Ty::Count => Value::Count(t.integer(c, row)?),
        Ty::Quantity(_) => Value::Num(t.number(c, row)?),
        other => return Err(format!("{}: a {} column cannot be read", t.at(row), other)),
    })
}

/// Write a dataset as one Parquet file per relation (the inverse of
/// `load_parquet_dir`), with typed columns.
pub fn write_parquet_dir(prog: &Program, ds: &Dataset, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write_securities(ds, &dir.join("securities.parquet"))?;
    for (name, tuples) in &ds.facts {
        let Some(sig) = prog.relations.get(name) else { continue };
        if name == "ticker" {
            // Derived from the security table (or from the symbols) at load.
            continue;
        }
        let mut cols: Vec<(&str, Col)> = Vec::new();
        for (i, arg) in sig.args.iter().enumerate() {
            let col = match &arg.ty {
                Ty::Equity | Ty::Label => Col::Str(tuples.iter().map(|tu| Some(field(ds, &tu[i]))).collect()),
                Ty::Timestamp => Col::Time(tuples.iter().map(|tu| tu[i].as_time()).collect()),
                Ty::Count => Col::Int(tuples.iter().map(|tu| if let Value::Count(c) = tu[i] { Some(c) } else { None }).collect()),
                Ty::Quantity(_) => Col::Float(tuples.iter().map(|tu| tu[i].as_f64()).collect()),
                other => return Err(format!("`{}`: a {} column cannot be written", name, other)),
            };
            cols.push((arg.name.as_str(), col));
        }
        if let Some(a) = ds.availability_of(name) {
            cols.push(("available_at", Col::Time((0..tuples.len()).map(|i| a.get(i).copied().filter(|x| *x != i64::MIN)).collect())));
        }
        write_table(&dir.join(format!("{}.parquet", name)), cols)?;
    }
    Ok(())
}

/// A small deterministic pseudo-random generator (SplitMix64).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Approximately standard normal (sum of twelve uniforms).
    pub fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.uniform()).sum::<f64>() - 6.0
    }
}

/// Business days (Monday to Friday) starting at a civil date.
pub fn business_days(start: (i64, u32, u32), count: usize) -> Vec<i64> {
    let mut t = days_from_civil(start.0, start.1, start.2) * DAY;
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        if weekday(t) < 5 {
            out.push(t);
        }
        t += DAY;
    }
    out
}

/// Synthetic daily market for the `equities_1d` environment: random-walk
/// closes with per-symbol drift and volatility, log-normal volumes, and a
/// universe holding every symbol on every day.
pub fn synthetic_daily(symbols: &[&str], start: (i64, u32, u32), days: usize, seed: u64) -> Dataset {
    let mut ds = Dataset::new();
    let mut rng = Rng::new(seed);
    let bars = business_days(start, days);
    for (i, s) in symbols.iter().enumerate() {
        let sym = ds.intern(s);
        let mut price = 50.0 + 10.0 * i as f64;
        let drift = 0.0002 * (i as f64 - symbols.len() as f64 / 2.0);
        let vol = 0.01 + 0.004 * i as f64;
        for &t in &bars {
            let spike = rng.uniform() < 0.03;
            let shock = if spike { 0.03 * if rng.uniform() < 0.5 { 1.0 } else { -1.0 } } else { 0.0 };
            price *= (drift + shock + vol * rng.normal()).exp();
            let volume = (1_000_000.0 * (0.3 * rng.normal()).exp() * if spike { 3.0 } else { 1.0 }).round();
            ds.add("close", vec![Value::Equity(sym), Value::Time(t), Value::Num((price * 100.0).round() / 100.0)]);
            ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(volume)]);
            ds.add("universe", vec![Value::Equity(sym), Value::Time(t)]);
        }
    }
    ds.derive_tickers();
    ds
}

/// Synthetic daily market for the `equities_1d_v2` catalog environment:
/// stable ids `E1..` with the given tickers (the second name changes ticker
/// halfway), prices as traded with a 2:1 split of the second name at two
/// fifths of the sample, quarterly dividends on the first name (announced
/// ten bars before the ex-date, paid fifteen after; the as-traded price drops
/// by the dividend), the last name delisted for bankruptcy at four fifths
/// (its rows stop; `delisted` holds from then on), open/high/low around the
/// close, every listed name a member of `SPX`, and a `sector` classification
/// cycling over three codes.
pub fn synthetic_daily_v2(symbols: &[&str], start: (i64, u32, u32), days: usize, seed: u64) -> Dataset {
    let mut ds = Dataset::new();
    let mut rng = Rng::new(seed);
    let bars = business_days(start, days);
    let spx = ds.intern_label("SPX");
    let scheme = ds.intern_label("sector");
    let sectors: Vec<Sym> = ["tech", "fin", "energy"].iter().map(|s| ds.intern_label(s)).collect();
    let bankrupt = ds.intern_label("bankruptcy");
    let n = symbols.len();
    let split_bar = days * 2 / 5;
    let rename_bar = days / 2;
    let delist_bar = days * 4 / 5;
    for (i, s) in symbols.iter().enumerate() {
        let id = format!("E{}", i + 1);
        let sym = if i == 1 && n > 1 {
            let sym = ds.add_security(&id, s, bars[0], Some(bars[rename_bar.min(days - 1)]));
            ds.add_security(&id, &format!("{}X", s), bars[rename_bar.min(days - 1)], None);
            sym
        } else if i + 1 == n && n > 2 {
            ds.add_security(&id, s, bars[0], Some(bars[delist_bar.min(days - 1)]))
        } else {
            ds.add_security(&id, s, bars[0], None)
        };
        let mut price = 50.0 + 10.0 * i as f64;
        let drift = 0.0002 * (i as f64 - n as f64 / 2.0);
        let vol = 0.01 + 0.004 * i as f64;
        for (k, &t) in bars.iter().enumerate() {
            if i + 1 == n && n > 2 && k >= delist_bar {
                ds.add("delisted", vec![Value::Equity(sym), Value::Time(t), Value::Label(bankrupt)]);
                continue;
            }
            let spike = rng.uniform() < 0.03;
            let shock = if spike { 0.03 * if rng.uniform() < 0.5 { 1.0 } else { -1.0 } } else { 0.0 };
            price *= (drift + shock + vol * rng.normal()).exp();
            if i == 1 && n > 1 && k == split_bar {
                price /= 2.0;
                ds.add("split", vec![Value::Equity(sym), Value::Time(t), Value::Num(2.0)]);
            }
            if i == 0 && k >= 10 && (k - 10) % 63 == 0 && k + 15 < days {
                // Announced at k, ex at k + 10, paid at k + 25 (clamped to the data).
                let amount = (price * 0.005 * 100.0).round() / 100.0;
                ds.add(
                    "dividend",
                    vec![
                        Value::Equity(sym),
                        Value::Time(t),
                        Value::Time(bars[k + 10]),
                        Value::Time(bars[(k + 25).min(days - 1)]),
                        Value::Num(amount),
                    ],
                );
            }
            // The as-traded price drops by a dividend going ex today.
            let ex_today: f64 = ds
                .facts
                .get("dividend")
                .map(|tus| tus.iter().filter(|tu| tu[0] == Value::Equity(sym) && tu[2] == Value::Time(t)).filter_map(|tu| tu[4].as_f64()).sum())
                .unwrap_or(0.0);
            price = (price - ex_today).max(0.01);
            let c = (price * 100.0).round() / 100.0;
            let o = (c * (1.0 + 0.003 * rng.normal()) * 100.0).round() / 100.0;
            let h = c.max(o) * (1.0 + 0.004 * rng.uniform());
            let l = c.min(o) * (1.0 - 0.004 * rng.uniform());
            let volume = (1_000_000.0 * (0.3 * rng.normal()).exp() * if spike { 3.0 } else { 1.0 }).round();
            ds.add("open", vec![Value::Equity(sym), Value::Time(t), Value::Num(o)]);
            ds.add("high", vec![Value::Equity(sym), Value::Time(t), Value::Num((h * 100.0).round() / 100.0)]);
            ds.add("low", vec![Value::Equity(sym), Value::Time(t), Value::Num((l * 100.0).round() / 100.0)]);
            ds.add("close", vec![Value::Equity(sym), Value::Time(t), Value::Num(c)]);
            ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(volume)]);
            ds.add("universe", vec![Value::Equity(sym), Value::Time(t)]);
            ds.add("member", vec![Value::Equity(sym), Value::Time(t), Value::Label(spx)]);
            ds.add("classification", vec![Value::Equity(sym), Value::Time(t), Value::Label(scheme), Value::Label(sectors[i % 3])]);
        }
    }
    ds.derive_tickers();
    ds
}

/// Synthetic minute market for the `equities_1m` environment: `bars_per_day`
/// one-minute bars from 09:31 on each business day.
pub fn synthetic_minute(symbols: &[&str], start: (i64, u32, u32), days: usize, bars_per_day: usize, seed: u64) -> Dataset {
    let mut ds = Dataset::new();
    let mut rng = Rng::new(seed);
    let day_starts = business_days(start, days);
    for (i, s) in symbols.iter().enumerate() {
        let sym = ds.intern(s);
        let mut price = 50.0 + 10.0 * i as f64;
        for &d in &day_starts {
            // An overnight move, occasionally a sizeable gap.
            let gap = 0.01 * rng.normal() + if rng.uniform() < 0.1 { -0.03 } else { 0.0 };
            price *= (gap).exp();
            for b in 0..bars_per_day {
                let t = d + 9 * 3600 + 31 * 60 + (b as i64) * 60;
                price *= (0.0006 * rng.normal()).exp();
                ds.add("close_m", vec![Value::Equity(sym), Value::Time(t), Value::Num((price * 100.0).round() / 100.0)]);
                ds.add("volume_m", vec![Value::Equity(sym), Value::Time(t), Value::Num((3000.0 * (0.3 * rng.normal()).exp()).round())]);
                ds.add("universe_m", vec![Value::Equity(sym), Value::Time(t)]);
            }
        }
    }
    ds
}

/// Summary statistics of an equity curve, for the command line.
pub fn summarize(curve: &[(i64, f64)]) -> BTreeMap<&'static str, f64> {
    let mut m = BTreeMap::new();
    if curve.is_empty() {
        return m;
    }
    let first = curve[0].1;
    let last = curve[curve.len() - 1].1;
    m.insert("start_equity", first);
    m.insert("end_equity", last);
    m.insert("total_return", last / first - 1.0);
    let mut peak = f64::NEG_INFINITY;
    let mut mdd: f64 = 0.0;
    for &(_, e) in curve {
        peak = peak.max(e);
        mdd = mdd.max(1.0 - e / peak);
    }
    m.insert("max_drawdown", mdd);
    m
}
