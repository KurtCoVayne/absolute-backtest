//! The synthetic markets (the test adapter of D-7.7): the deterministic
//! daily, minute and futures markets the corpus and the suites run on, as a
//! `Market` the keyed ingest takes. They are, bit for bit, the markets the
//! kernel's `data::synthetic_*` generators make for a `Dataset` held in
//! memory (`crates/abt-tests/tests/synthetic.rs` holds the two equal), so a
//! store `abt-data ingest --adapter synthetic` fills is the store a run in
//! one process reads.

use abt_wire::scalar::{days_from_civil, NANOS_PER_DAY, NANOS_PER_HOUR, NANOS_PER_SECOND};

use crate::adapter::Terms;
use crate::adapters::market::{Market, V};

/// Nanoseconds in a day.
pub const DAY: i64 = NANOS_PER_DAY;
const HOUR: i64 = NANOS_PER_HOUR;
const MIN: i64 = 60 * NANOS_PER_SECOND;

/// A splitmix64 generator: the kernel's.
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

fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Day-of-week with Monday = 0.
fn weekday(t: i64) -> u32 {
    (floor_div(t, DAY).rem_euclid(7) + 3) as u32 % 7
}

/// The civil week of `t` (Monday-based), as a count of weeks since the epoch's Monday.
fn week_key(t: i64) -> i64 {
    floor_div(floor_div(t, DAY) + 3, 7)
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

/// A synthetic daily equity market for the `equity` class: stable keys
/// `E1..` carrying the given tickers (the second name changes ticker
/// halfway), prices as traded with a 2:1 split of the second name at two
/// fifths of the sample, quarterly dividends on the first name (announced
/// ten bars before the ex-date, paid fifteen after; the as-traded price
/// drops by the dividend), the last name delisted for bankruptcy at four
/// fifths (its rows stop; `delisted` holds from then on), open, high and
/// low around the close, every name listed and a member of `SPX` (and of
/// `SP500`), a `sector` classification cycling over three codes, and two
/// series: a daily `SPXTR` index level and a weekly `VIX`. A name's drift
/// and volatility come from one of ten profiles, so a market of hundreds of
/// names stays priced. Prices are on the cent and volumes whole, so every
/// value is a tick (ST-3).
pub fn daily(symbols: &[&str], start: (i64, u32, u32), days: usize, seed: u64) -> Market {
    let mut m = Market::default();
    let mut rng = Rng::new(seed);
    let bars = business_days(start, days);
    let sectors = ["tech", "fin", "energy"];
    let n = symbols.len();
    let split_bar = days * 2 / 5;
    let rename_bar = days / 2;
    let delist_bar = days * 4 / 5;
    for (i, s) in symbols.iter().enumerate() {
        let id = format!("E{}", i + 1);
        if i == 1 && n > 1 {
            m.add_security(&id, s, bars[0], Some(bars[rename_bar.min(days - 1)]));
            m.add_security(&id, &format!("{}X", s), bars[rename_bar.min(days - 1)], None);
        } else if i + 1 == n && n > 2 {
            m.add_security(&id, s, bars[0], Some(bars[delist_bar.min(days - 1)]));
        } else {
            m.add_security(&id, s, bars[0], None);
        }
        let e = || V::E(id.clone());
        let mut price = 50.0 + 10.0 * i as f64;
        // A9 of docs/assessment.md: drift and volatility bounded whatever
        // the number of names (names cycle through ten profiles; up to ten
        // names keep the profiles they always had).
        let (k, mm) = ((i % 10) as f64, n.min(10) as f64);
        let drift = 0.0002 * (k - mm / 2.0);
        let vol = 0.01 + 0.004 * k;
        for (k, &t) in bars.iter().enumerate() {
            if i + 1 == n && n > 2 && k >= delist_bar {
                m.add("delisted", vec![e(), V::T(t), V::L("bankruptcy".into())]);
                continue;
            }
            let spike = rng.uniform() < 0.03;
            let shock = if spike { 0.03 * if rng.uniform() < 0.5 { 1.0 } else { -1.0 } } else { 0.0 };
            price *= (drift + shock + vol * rng.normal()).exp();
            if i == 1 && n > 1 && k == split_bar {
                price /= 2.0;
                m.add("split", vec![e(), V::T(t), V::N(2.0)]);
            }
            if i == 0 && k >= 10 && (k - 10) % 63 == 0 && k + 15 < days {
                // Announced at k, ex at k + 10, paid at k + 25 (clamped to the data).
                let amount = (price * 0.005 * 100.0).round() / 100.0;
                m.add("dividend", vec![e(), V::T(t), V::T(bars[k + 10]), V::T(bars[(k + 25).min(days - 1)]), V::N(amount)]);
            }
            // The as-traded price drops by a dividend going ex today.
            let ex_today: f64 = m
                .facts
                .get("dividend")
                .map(|rows| rows.iter().filter(|r| r[0] == e() && r[2] == V::T(t)).filter_map(|r| r[4].as_f64()).sum())
                .unwrap_or(0.0);
            price = (price - ex_today).max(0.01);
            let c = (price * 100.0).round() / 100.0;
            let o = (c * (1.0 + 0.003 * rng.normal()) * 100.0).round() / 100.0;
            let h = c.max(o) * (1.0 + 0.004 * rng.uniform());
            let l = c.min(o) * (1.0 - 0.004 * rng.uniform());
            let volume = (1_000_000.0 * (0.3 * rng.normal()).exp() * if spike { 3.0 } else { 1.0 }).round();
            m.add("open", vec![e(), V::T(t), V::N(o)]);
            m.add("high", vec![e(), V::T(t), V::N((h * 100.0).round() / 100.0)]);
            m.add("low", vec![e(), V::T(t), V::N((l * 100.0).round() / 100.0)]);
            m.add("close", vec![e(), V::T(t), V::N(c)]);
            m.add("volume", vec![e(), V::T(t), V::N(volume)]);
            m.add("listed", vec![e(), V::T(t)]);
            m.add("member", vec![e(), V::T(t), V::L("SPX".into())]);
            m.add("member", vec![e(), V::T(t), V::L("SP500".into())]);
            m.add("classification", vec![e(), V::T(t), V::L("sector".into()), V::L(sectors[i % 3].into())]);
        }
    }
    // The series: a daily index level, and a weekly volatility index keyed
    // at the week's last business day.
    let mut level = 1000.0;
    let mut vix_level = 18.0;
    let mut last_week: Option<i64> = None;
    for (k, &t) in bars.iter().enumerate() {
        level *= (0.0003 + 0.009 * rng.normal()).exp();
        m.add("value", vec![V::L("SPXTR".into()), V::T(t), V::N((level * 100.0).round() / 100.0)]);
        let wk = week_key(t);
        let ends_week = bars.get(k + 1).map(|&n| week_key(n) != wk).unwrap_or(true);
        if ends_week && last_week != Some(wk) {
            vix_level = (vix_level * (0.03 * rng.normal()).exp()).clamp(9.0, 80.0);
            m.add("value", vec![V::L("VIX".into()), V::T(t), V::N((vix_level * 100.0).round() / 100.0)]);
            last_week = Some(wk);
        }
    }
    m.derive_tickers();
    m
}

/// A synthetic minute equity market: `bars_per_day` one-minute bars from
/// 09:31 on each business day (open, high, low, close, volume), with the
/// daily catalog (listing and membership) keyed at the date.
pub fn minute(symbols: &[&str], start: (i64, u32, u32), days: usize, bars_per_day: usize, seed: u64) -> Market {
    let mut m = Market::default();
    let mut rng = Rng::new(seed);
    let day_starts = business_days(start, days);
    for (i, s) in symbols.iter().enumerate() {
        let e = || V::E(s.to_string());
        let mut price = 50.0 + 10.0 * i as f64;
        for &d in &day_starts {
            // An overnight move, occasionally a sizeable gap.
            let gap = 0.01 * rng.normal() + if rng.uniform() < 0.1 { -0.03 } else { 0.0 };
            price *= (gap).exp();
            m.add("listed", vec![e(), V::T(d)]);
            m.add("member", vec![e(), V::T(d), V::L("SPX".into())]);
            for b in 0..bars_per_day {
                let t = d + 9 * HOUR + 31 * MIN + (b as i64) * MIN;
                let o = (price * 100.0).round() / 100.0;
                price *= (0.0006 * rng.normal()).exp();
                let c = (price * 100.0).round() / 100.0;
                let h = c.max(o) * (1.0 + 0.0005 * rng.uniform());
                let l = c.min(o) * (1.0 - 0.0005 * rng.uniform());
                m.add("open", vec![e(), V::T(t), V::N(o)]);
                m.add("high", vec![e(), V::T(t), V::N((h * 100.0).round() / 100.0)]);
                m.add("low", vec![e(), V::T(t), V::N((l * 100.0).round() / 100.0)]);
                m.add("close", vec![e(), V::T(t), V::N(c)]);
                m.add("volume", vec![e(), V::T(t), V::N((3000.0 * (0.3 * rng.normal()).exp()).round())]);
            }
        }
    }
    m.derive_tickers();
    m
}

/// A synthetic minute futures market for the `future` class: per root,
/// `bars_per_session` one-minute bars from 09:31 on each business day, a
/// `session(A, S, Open)` row per day labelled at the session's last bar
/// with the open at 09:30, a daily `point_value`, `commission` and
/// `usable`, membership of `HOME-26`, and contract terms in the security
/// table (a back-adjusted price that may cross zero for the last root).
pub fn futures(roots: &[&str], start: (i64, u32, u32), days: usize, bars_per_session: usize, seed: u64) -> Market {
    let mut m = Market::default();
    let mut rng = Rng::new(seed);
    let day_starts = business_days(start, days);
    for (i, r) in roots.iter().enumerate() {
        m.add_security(r, r, day_starts[0], None);
        m.terms.push(Terms {
            key: r.to_string(),
            class: "future".to_string(),
            multiplier: Some(50.0 - 10.0 * (i % 4) as f64),
            commission_per_contract: Some(2.0 + 0.5 * i as f64),
        });
        let e = || V::E(r.to_string());
        let mut price = if i + 1 == roots.len() && roots.len() > 1 { 2.0 } else { 100.0 + 25.0 * i as f64 };
        for &d in &day_starts {
            let open_t = d + 9 * HOUR + 30 * MIN;
            let gap = 0.004 * rng.normal();
            price += price.abs().max(1.0) * gap;
            m.add("point_value", vec![e(), V::T(d), V::N(50.0 - 10.0 * (i % 4) as f64)]);
            m.add("commission", vec![e(), V::T(d), V::N(2.0 + 0.5 * i as f64)]);
            m.add("usable", vec![e(), V::T(d)]);
            m.add("member", vec![e(), V::T(d), V::L("HOME-26".into())]);
            let mut last = open_t;
            for b in 0..bars_per_session {
                let t = open_t + MIN + (b as i64) * MIN;
                let o = (price * 100.0).round() / 100.0;
                price += price.abs().max(1.0) * 0.0008 * rng.normal();
                let c = (price * 100.0).round() / 100.0;
                let h = c.max(o) + 0.01 * rng.uniform();
                let l = c.min(o) - 0.01 * rng.uniform();
                m.add("open", vec![e(), V::T(t), V::N(o)]);
                m.add("high", vec![e(), V::T(t), V::N((h * 100.0).round() / 100.0)]);
                m.add("low", vec![e(), V::T(t), V::N((l * 100.0).round() / 100.0)]);
                m.add("close", vec![e(), V::T(t), V::N(c)]);
                m.add("volume", vec![e(), V::T(t), V::N((500.0 * (0.3 * rng.normal()).exp()).round())]);
                last = t;
            }
            m.add("session", vec![e(), V::T(last), V::T(open_t)]);
        }
    }
    m
}

/// The synthetic market of a class at a base calendar: minute futures with
/// sessions, minute equities, or daily equities with the catalog.
pub fn market(class: &str, sub_daily: bool, symbols: &[&str], start: (i64, u32, u32), days: usize, seed: u64) -> Market {
    match class {
        "future" => futures(symbols, start, days, 60, seed),
        _ if sub_daily => minute(symbols, start, days, 60, seed),
        _ => daily(symbols, start, days, seed),
    }
}

/// The defaults of `abt data ingest --adapter synthetic`: the names, days,
/// start and seed a class takes when none is given.
pub fn defaults(sub_daily: bool) -> (Vec<&'static str>, usize, (i64, u32, u32), u64) {
    (
        vec!["AAA", "XYZ", "BBB", "SPY", "DDD"],
        if sub_daily { 40 } else { 500 },
        if sub_daily { (2024, 1, 2) } else { (2022, 1, 3) },
        7,
    )
}

/// Whether a calendar label (`1m`, `@1d`) is finer than a day.
pub fn is_sub_daily(calendar: &str) -> bool {
    let c = calendar.trim_start_matches('@');
    c.ends_with('s') || c.ends_with("min") || c.ends_with('h') || (c.ends_with('m') && !c.ends_with("mo"))
}
