//! Environment instances from CSV files, and a deterministic synthetic
//! market for tests and demos.

use std::collections::BTreeMap;
use std::path::Path;

use crate::check::Program;
use crate::ir::*;
use crate::kernel::time::{days_from_civil, format_timestamp, parse_timestamp, weekday, DAY};
use crate::kernel::{Dataset, Value};

/// Load one CSV per primitive relation of the program's environment from
/// `dir` (`<relation>.csv`, header row naming the signature's arguments).
/// Missing files leave the relation empty and are reported in the result.
pub fn load_csv_dir(prog: &Program, dir: &Path) -> Result<(Dataset, Vec<String>), String> {
    let mut ds = Dataset::new();
    let mut missing = Vec::new();
    for (name, sig) in &prog.relations {
        if !matches!(sig.kind, Kind::Primitive { .. }) {
            continue;
        }
        let path = dir.join(format!("{}.csv", name));
        if !path.exists() {
            missing.push(path.display().to_string());
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: Vec<String> = lines
            .next()
            .ok_or_else(|| format!("{}: empty file", path.display()))?
            .split(',')
            .map(|h| h.trim().to_lowercase())
            .collect();
        let cols: Vec<usize> = sig
            .args
            .iter()
            .map(|a| {
                header
                    .iter()
                    .position(|h| *h == a.name.to_lowercase())
                    .ok_or_else(|| format!("{}: header lacks column `{}`", path.display(), a.name))
            })
            .collect::<Result<_, _>>()?;
        for (ln, line) in lines.enumerate() {
            let fields: Vec<&str> = line.split(',').map(|f| f.trim()).collect();
            let mut tuple = Vec::with_capacity(sig.args.len());
            for (arg, &c) in sig.args.iter().zip(cols.iter()) {
                let raw = fields.get(c).ok_or_else(|| format!("{}:{}: missing column `{}`", path.display(), ln + 2, arg.name))?;
                tuple.push(parse_value(&mut ds, &arg.ty, raw).ok_or_else(|| format!("{}:{}: `{}` is not a {}", path.display(), ln + 2, raw, arg.ty))?);
            }
            ds.add(name, tuple);
        }
    }
    Ok((ds, missing))
}

fn parse_value(ds: &mut Dataset, ty: &Ty, raw: &str) -> Option<Value> {
    Some(match ty {
        Ty::Equity => Value::Equity(ds.intern(raw)),
        Ty::Timestamp => Value::Time(parse_timestamp(raw)?),
        Ty::Count => Value::Count(raw.parse().ok()?),
        Ty::Quantity(_) => Value::Num(raw.parse().ok()?),
        Ty::Duration => return None,
        Ty::Decision => return None,
        Ty::IntLit => return None,
    })
}

/// Write a dataset as one CSV per relation (the inverse of `load_csv_dir`).
pub fn write_csv_dir(prog: &Program, ds: &Dataset, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for (name, tuples) in &ds.facts {
        let Some(sig) = prog.relations.get(name) else { continue };
        let mut out = String::new();
        out.push_str(&sig.args.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(","));
        out.push('\n');
        for tu in tuples {
            let fields: Vec<String> = tu
                .iter()
                .map(|v| match v {
                    Value::Equity(s) => ds.symbols.name(*s).to_string(),
                    Value::Time(t) => format_timestamp(*t),
                    Value::Num(x) => format!("{}", x),
                    Value::Count(c) => format!("{}", c),
                    Value::Dur(d) => format!("{}", d),
                    Value::Decision(_) => String::new(),
                })
                .collect();
            out.push_str(&fields.join(","));
            out.push('\n');
        }
        std::fs::write(dir.join(format!("{}.csv", name)), out).map_err(|e| e.to_string())?;
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
