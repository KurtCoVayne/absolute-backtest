//! The margin models against hand-computed answers (docs/spec/02-kernel.md,
//! K-9.3 cases 14 to 18 and the rules of K-5.6): every expected number is
//! worked in the comment beside it. `reg_t`'s minimum over pairings is
//! checked against brute-force enumeration on random small books, with the
//! rules written out again here.

use std::collections::BTreeMap;

use abt_kernel::functions::{Right, Style};
use abt_kernel::sim::margin::{liquidation_order, requirement, Context, Holding, IndexClass, InterCredit, Margin, Model, PublishedRow, Requirement, RiskArray, Source, SpanParams, Terms, Underlying};

/// A day in nanoseconds, and the barrier (2023-11-14).
const DAY: i64 = 86_400_000_000_000;
const T0: i64 = 1_700_000_000_000_000_000;

/// The data a context borrows.
#[derive(Default)]
struct World {
    underlyings: BTreeMap<u32, Underlying>,
    published: BTreeMap<u32, PublishedRow>,
    risk_arrays: BTreeMap<u32, RiskArray>,
    span: SpanParams,
}

impl World {
    fn underlying(mut self, id: u32, price: f64, class: IndexClass) -> World {
        self.underlyings.insert(id, Underlying { id, price, class });
        self
    }

    fn margin(&self, models: &[Model], holdings: &[Holding]) -> Margin {
        self.margin_priced(models, holdings, &|_, _| None)
    }

    fn margin_priced(&self, models: &[Model], holdings: &[Holding], reprice: &dyn Fn(&Holding, f64) -> Option<f64>) -> Margin {
        let ctx = Context {
            t: T0,
            underlyings: &self.underlyings,
            published: &self.published,
            risk_arrays: &self.risk_arrays,
            span: &self.span,
            reprice,
        };
        requirement(models, holdings, &ctx)
    }
}

fn stock(id: u32, qty: f64, mark: f64) -> Holding {
    Holding {
        instrument: id,
        qty,
        mark,
        terms: Terms::Stock,
    }
}

fn future(id: u32, qty: f64, mark: f64, point_value: f64) -> Holding {
    Holding {
        instrument: id,
        qty,
        mark,
        terms: Terms::Future { point_value },
    }
}

/// An option of multiplier 100 on `underlying`, expiring `days` after the
/// barrier.
fn option(id: u32, underlying: u32, right: Right, strike: f64, days: i64, qty: f64, mark: f64) -> Holding {
    Holding {
        instrument: id,
        qty,
        mark,
        terms: Terms::Option {
            underlying,
            right,
            strike,
            expiry: T0 + days * DAY,
            multiplier: 100.0,
            style: Style::American,
        },
    }
}

fn row(m: &Margin, id: u32) -> Requirement {
    *m.rows.iter().find(|r| r.instrument == id).unwrap_or_else(|| panic!("no row for {id} in {m:?}"))
}

#[track_caller]
fn assert_close(got: f64, want: f64) {
    assert!((got - want).abs() <= 1e-6 * (1.0 + want.abs()), "got {got}, want {want}");
}

/// Every row's maintenance, in instrument order.
fn maintenance(m: &Margin) -> Vec<(u32, f64)> {
    m.rows.iter().map(|r| (r.instrument, (r.maintenance * 1e6).round() / 1e6)).collect()
}

// ---------------------------------------------------------------------------
// K-9.3, cases 14 to 18.

#[test]
fn case_14_naked_short_put_under_reg_t() {
    // S = 100, K = 95, premium 1.50, m = 100, Q = 2: OTM = max(100 − 95, 0) =
    // 5, max(20 % · 100 − 5 + 1.50, 10 % · 95 + 1.50) = max(16.50, 11.00) =
    // 16.50, × 100 × 2 = 3,300; initial = maintenance.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Put, 95.0, 30, -2.0, 1.5)]);
    assert_close(m.maintenance, 3300.0);
    assert_close(m.initial, 3300.0);
    assert_eq!(m.rows.len(), 1);
    assert_eq!(row(&m, 10).source, Source::RegT);
    assert!(!m.approx_used && m.skipped.is_empty() && m.fallbacks.is_empty());
}

#[test]
fn case_15_put_credit_spread_needs_its_maximum_loss() {
    // S = 102: short the 100 put at 3.00, long the 95 put at 1.20, m = 100, Q =
    // 1. Apart, the short needs (3.00 + max(20.40 − 2, 10)) × 100 = 2,140 and
    // the long 1.20 × 100 = 120: 2,260. Paired, (1.20 − 3.00 + 5) × 100 =
    // 320, carried by the short.
    let w = World::default().underlying(1, 102.0, IndexClass::Equity);
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Put, 100.0, 30, -1.0, 3.0), option(11, 1, Right::Put, 95.0, 30, 1.0, 1.2)]);
    assert_close(m.maintenance, 320.0);
    assert_close(m.initial, 320.0);
    assert_eq!(maintenance(&m), [(10, 320.0), (11, 0.0)]);
}

#[test]
fn case_15_call_debit_spread_needs_its_net_debit() {
    // S = 100: long the 100 call at 5.00, short the 105 call at 2.00: width =
    // max(0, 100 − 105) = 0, so (5.00 − 2.00 + 0) × 100 = 300, the net debit.
    // Apart: 500 + (2.00 + max(20 − 5, 10)) × 100 = 500 + 1,700 = 2,200.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Call, 100.0, 30, 1.0, 5.0), option(11, 1, Right::Call, 105.0, 30, -1.0, 2.0)]);
    assert_close(m.maintenance, 300.0);
    assert_eq!(maintenance(&m), [(10, 0.0), (11, 300.0)]);
}

#[test]
fn spreads_need_the_long_to_expire_last_and_pair_fractional_lots_in_part() {
    let w = World::default().underlying(1, 102.0, IndexClass::Equity);
    // Case 15's credit spread with the long expiring at 20 days, before the
    // short's 30: no spread, 2,140 + 120 = 2,260.
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Put, 100.0, 30, -1.0, 3.0), option(11, 1, Right::Put, 95.0, 20, 1.0, 1.2)]);
    assert_close(m.maintenance, 2260.0);
    // Short 1.5 against the one long: one unit pairs (320), the half unit
    // left is naked (0.5 × 2,140 = 1,070): 1,390 on the short.
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Put, 100.0, 30, -1.5, 3.0), option(11, 1, Right::Put, 95.0, 30, 1.0, 1.2)]);
    assert_eq!(maintenance(&m), [(10, 1390.0), (11, 0.0)]);
    // Another multiplier is no pair: the long at m = 150 is paid, 1.20 × 150
    // = 180, and the short is naked, 2,140: 2,320.
    let mut long = option(11, 1, Right::Put, 95.0, 30, 1.0, 1.2);
    if let Terms::Option { ref mut multiplier, .. } = long.terms {
        *multiplier = 150.0;
    }
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Put, 100.0, 30, -1.0, 3.0), long]);
    assert_close(m.maintenance, 2320.0);
}

/// The error function: its Maclaurin series below 2.5, the continued
/// fraction of erfc above.
fn erf(x: f64) -> f64 {
    let z = x.abs();
    if z < 2.5 {
        // erf x = 2/√π Σ (−1)ⁿ x²ⁿ⁺¹ / (n! (2n + 1)).
        let (mut p, mut sum, mut n) = (x, x, 0.0);
        loop {
            n += 1.0;
            p *= -x * x / n;
            let term = p / (2.0 * n + 1.0);
            sum += term;
            if term.abs() <= 1e-17 * sum.abs() {
                return sum * std::f64::consts::FRAC_2_SQRT_PI;
            }
        }
    }
    // erfc z = e^(−z²)/√π · 1/(z + (1/2)/(z + 1/(z + (3/2)/(z + …)))).
    let mut f = z;
    for k in (1..=60).rev() {
        f = z + (k as f64 / 2.0) / f;
    }
    let erfc = (-z * z).exp() * std::f64::consts::FRAC_2_SQRT_PI / (2.0 * f);
    (1.0 - erfc).copysign(x)
}

fn cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Black–Scholes–Merton without dividends.
fn bsm(right: Right, s: f64, k: f64, tau: f64, r: f64, sigma: f64) -> f64 {
    let sd = sigma * tau.sqrt();
    let d1 = ((s / k).ln() + (r + 0.5 * sigma * sigma) * tau) / sd;
    let d2 = d1 - sd;
    let df = (-r * tau).exp();
    match right {
        Right::Call => s * cdf(d1) - k * df * cdf(d2),
        Right::Put => k * df * cdf(-d2) - s * cdf(-d1),
    }
}

/// `reprice` by Black–Scholes–Merton at r = 3 % and each option's implied
/// volatility in `iv` (None for an option without one).
fn bsm_reprice(iv: &BTreeMap<u32, f64>) -> impl Fn(&Holding, f64) -> Option<f64> + '_ {
    move |h: &Holding, s: f64| match h.terms {
        Terms::Option { right, strike, expiry, .. } => Some(bsm(right, s, strike, (expiry - T0) as f64 / (365.0 * DAY as f64), 0.03, *iv.get(&h.instrument)?)),
        _ => None,
    }
}

#[test]
fn case_16_portfolio_on_a_straddle_takes_the_worst_point() {
    // A short straddle at S = 100: the 100 call and put, 30 days, IV 25 %, r
    // = 3 %, marked at their model values (2.9800 and 2.7338).
    let tau = 30.0 / 365.0;
    let value = |right: Right, s: f64| bsm(right, s, 100.0, tau, 0.03, 0.25);
    let (call0, put0) = (value(Right::Call, 100.0), value(Right::Put, 100.0));
    let holdings = [option(20, 1, Right::Call, 100.0, 30, -1.0, call0), option(21, 1, Right::Put, 100.0, 30, -1.0, put0)];
    let iv: BTreeMap<u32, f64> = [(20, 0.25), (21, 0.25)].into();
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin_priced(&[Model::Portfolio], &holdings, &bsm_reprice(&iv));
    // The same eleven points by hand: the loss is Σ −qty × (value(S′) −
    // value(S)) × 100. It is 909.86 at −15 % (the put at 14.7831) and 966.83
    // at +15 % (the call at 15.3142, the put at 0.0679), the worst.
    let mut worst = (f64::NEG_INFINITY, 0.0);
    for x in [-0.15, -0.12, -0.09, -0.06, -0.03, 0.0, 0.03, 0.06, 0.09, 0.12, 0.15] {
        let s = 100.0 * (1.0 + x);
        let loss = (value(Right::Call, s) - call0) * 100.0 + (value(Right::Put, s) - put0) * 100.0;
        if loss > worst.0 {
            worst = (loss, x);
        }
    }
    assert_eq!(worst.1, 0.15);
    assert!((worst.0 - 966.83).abs() < 0.01, "{worst:?}");
    // Far above the floor 0.375 × 100 × 2 = 75.
    assert_close(m.maintenance, worst.0);
    assert_close(m.initial, worst.0);
    // At +15 % the call loses 1,233.42 and the put gains 266.58: the call
    // carries it all.
    assert_close(row(&m, 20).maintenance, worst.0);
    assert_close(row(&m, 21).maintenance, 0.0);
    assert_eq!(row(&m, 20).source, Source::Portfolio);
}

#[test]
fn case_16_the_floor_binds_for_a_tiny_position() {
    // A strangle far out of the money with 2 days left: the 150 call and the
    // 60 put at IV 20 %, S = 100. Neither is worth a cent anywhere on ±15 %,
    // so the worst loss is 0 and the floor 0.375 × 100 × 2 = 75 binds, split
    // by |contracts × m|: 37.50 each.
    let holdings = [option(20, 1, Right::Call, 150.0, 2, -1.0, 0.0), option(21, 1, Right::Put, 60.0, 2, -1.0, 0.0)];
    let iv: BTreeMap<u32, f64> = [(20, 0.2), (21, 0.2)].into();
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin_priced(&[Model::Portfolio], &holdings, &bsm_reprice(&iv));
    assert_close(m.maintenance, 75.0);
    assert_eq!(maintenance(&m), [(20, 37.5), (21, 37.5)]);
}

#[test]
fn portfolio_attributes_by_the_losses_at_the_worst_point() {
    // Long 100 shares at 100 and short case 16's 100 put (2.7338). Both lose
    // as S falls: at −15 % the shares lose 100 × 15 = 1,500 and the put (14.7831
    // at 85) (14.7831 − 2.7338) × 100 = 1,204.93, the worst point, 2,704.93
    // (at −12 % it is 1,200 + 912.88). Each carries its own loss there.
    let tau = 30.0 / 365.0;
    let put = |s: f64| bsm(Right::Put, s, 100.0, tau, 0.03, 0.25);
    let holdings = [stock(1, 100.0, 100.0), option(21, 1, Right::Put, 100.0, 30, -1.0, put(100.0))];
    let iv: BTreeMap<u32, f64> = [(21, 0.25)].into();
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin_priced(&[Model::Portfolio], &holdings, &bsm_reprice(&iv));
    let put_loss = (put(85.0) - put(100.0)) * 100.0;
    assert!((put_loss - 1204.93).abs() < 0.01, "{put_loss}");
    assert_close(row(&m, 1).maintenance, 1500.0);
    assert_close(row(&m, 21).maintenance, put_loss);
    assert_close(m.maintenance, 1500.0 + put_loss);
}

#[test]
fn case_17_published_where_present_reg_t_elsewhere() {
    // Option 30 has a published row of 600 initial and 450 maintenance a
    // contract: short 2 needs 1,200 and 900. Option 31 has none and falls to
    // reg_t: a naked short call, S = 100, K = 110, premium 1.00, Q = 1: OTM =
    // 10, max(20 − 10, 10) = 10, (1.00 + 10) × 100 = 1,100.
    let mut w = World::default().underlying(1, 100.0, IndexClass::Equity);
    w.published.insert(30, PublishedRow { initial: 600.0, maintenance: 450.0 });
    let m = w.margin(
        &[Model::Published, Model::RegT],
        &[option(30, 1, Right::Put, 95.0, 30, -2.0, 1.5), option(31, 1, Right::Call, 110.0, 30, -1.0, 1.0)],
    );
    assert_eq!((row(&m, 30).source.name(), row(&m, 31).source.name()), ("published", "reg_t"));
    assert_close(row(&m, 30).initial, 1200.0);
    assert_close(row(&m, 30).maintenance, 900.0);
    assert_close(row(&m, 31).maintenance, 1100.0);
    // Totals: 1,200 + 1,100 = 2,300 initial, 900 + 1,100 = 2,000 maintenance.
    assert_close(m.initial, 2300.0);
    assert_close(m.maintenance, 2000.0);
    assert_eq!(m.fallbacks, [(31, Source::Published, "no published row".to_string())]);
    assert!(m.skipped.is_empty());
}

#[test]
fn case_18_approx_only() {
    // approx 0.2: future 40, 3 at 4,000 × 50 → 0.2 × 3 × 4,000 × 50 = 120,000;
    // option 41, short 2, m = 100, S = 100 → 0.2 × 2 × 100 × 100 = 4,000; stock
    // 42, short 10 at 50 → 0.2 × |−500| = 100. Total 124,100, initial =
    // maintenance, and the model is flagged.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(
        &[Model::Approx(0.2)],
        &[future(40, 3.0, 4000.0, 50.0), option(41, 1, Right::Call, 105.0, 30, -2.0, 1.0), stock(42, -10.0, 50.0)],
    );
    assert!(m.approx_used);
    assert_eq!(maintenance(&m), [(40, 120_000.0), (41, 4000.0), (42, 100.0)]);
    assert_close(m.maintenance, 124_100.0);
    assert_close(m.initial, 124_100.0);
    assert!(m.rows.iter().all(|r| r.source == Source::Approx && r.source.name() == "approx"));
    // Without the underlying's price the strike stands in: the 105 call is
    // 0.2 × 2 × 100 × 105 = 4,200.
    let m = World::default().margin(&[Model::Approx(0.2)], &[option(41, 1, Right::Call, 105.0, 30, -2.0, 1.0)]);
    assert_close(m.maintenance, 4200.0);
}

// ---------------------------------------------------------------------------
// reg_t's strategies and stock rules.

#[test]
fn straddle_needs_the_greater_side_plus_the_other_premium() {
    // S = 100: short the 105 call at 2.00 and the 95 put at 1.50, Q = 1.
    // Naked, the call needs (2.00 + max(20 − 5, 10)) × 100 = 1,700 and the put
    // (1.50 + max(20 − 5, 9.5)) × 100 = 1,650; straddled, 1,700 + 1.50 × 100 =
    // 1,850 (against 3,350 apart), carried by the call.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Call, 105.0, 30, -1.0, 2.0), option(11, 1, Right::Put, 95.0, 30, -1.0, 1.5)]);
    assert_eq!(maintenance(&m), [(10, 1850.0), (11, 0.0)]);
    // Two calls against one put: one straddle and a naked call, 1,850 +
    // 1,700 = 3,550.
    let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Call, 105.0, 30, -2.0, 2.0), option(11, 1, Right::Put, 95.0, 30, -1.0, 1.5)]);
    assert_close(m.maintenance, 3550.0);
}

#[test]
fn covered_call_needs_nothing_beyond_the_stock() {
    // Long 100 shares at 100 and short the 105 call at 2.00: the call needs
    // nothing; the stock 50 % × 10,000 = 5,000 initial, 25 % = 2,500
    // maintenance.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::RegT], &[stock(1, 100.0, 100.0), option(10, 1, Right::Call, 105.0, 30, -1.0, 2.0)]);
    assert_eq!(maintenance(&m), [(1, 2500.0), (10, 0.0)]);
    assert_close(m.initial, 5000.0);
    // 150 shares against two calls: 1.5 contracts covered and the half left
    // naked, 0.5 × 1,700 = 850; the stock 7,500 initial, 3,750 maintenance.
    // Maintenance 4,600, initial 8,350.
    let m = w.margin(&[Model::RegT], &[stock(1, 150.0, 100.0), option(10, 1, Right::Call, 105.0, 30, -2.0, 2.0)]);
    assert_eq!(maintenance(&m), [(1, 3750.0), (10, 850.0)]);
    assert_close(m.initial, 8350.0);
}

#[test]
fn covered_put_needs_nothing_beyond_the_short_stock() {
    // Short 100 shares at 100 and short the 95 put at 1.50: the put needs
    // nothing; the stock 50 % × 10,000 = 5,000 initial and max(30 % × 10,000,
    // $5 × 100) = 3,000 maintenance.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::RegT], &[stock(1, -100.0, 100.0), option(10, 1, Right::Put, 95.0, 30, -1.0, 1.5)]);
    assert_eq!(maintenance(&m), [(1, 3000.0), (10, 0.0)]);
    assert_close(m.initial, 5000.0);
    // Long stock covers no put: the put is naked, (1.50 + 15) × 100 = 1,650,
    // and the stock 2,500.
    let m = w.margin(&[Model::RegT], &[stock(1, 100.0, 100.0), option(10, 1, Right::Put, 95.0, 30, -1.0, 1.5)]);
    assert_eq!(maintenance(&m), [(1, 2500.0), (10, 1650.0)]);
}

#[test]
fn long_options_are_paid_in_full_within_nine_months() {
    // Long 2 calls at 10.00, m = 100: MV 2,000. At 274 days, 100 %: 2,000; at
    // 275 or 300 days, 75 %: 1,500. Initial = maintenance.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    for (days, want) in [(30, 2000.0), (274, 2000.0), (275, 1500.0), (300, 1500.0)] {
        let m = w.margin(&[Model::RegT], &[option(10, 1, Right::Call, 100.0, days, 2.0, 10.0)]);
        assert_close(m.maintenance, want);
        assert_close(m.initial, want);
    }
}

#[test]
fn stock_rules_long_short_and_under_five_dollars() {
    // Each stock is its own underlying.
    let m = World::default().margin(
        &[Model::RegT],
        &[stock(1, 100.0, 50.0), stock(2, -100.0, 50.0), stock(3, -100.0, 10.0), stock(4, -1000.0, 3.0), stock(5, -1000.0, 2.0)],
    );
    // 1: long 100 at 50, MV 5,000: initial 2,500, maintenance 25 % = 1,250.
    // 2: short 100 at 50: initial 2,500, maintenance max(30 % × 5,000, $5 × 100) = 1,500.
    // 3: short 100 at 10, MV 1,000: initial 500, maintenance max(300, 500) = 500.
    // 4: short 1,000 at 3.00, MV 3,000: initial 1,500, maintenance max(100 % × 3,000, $2.50 × 1,000) = 3,000.
    // 5: short 1,000 at 2.00, MV 2,000: initial 1,000, maintenance max(2,000, 2,500) = 2,500.
    assert_eq!(maintenance(&m), [(1, 1250.0), (2, 1500.0), (3, 500.0), (4, 3000.0), (5, 2500.0)]);
    let initial: Vec<f64> = m.rows.iter().map(|r| r.initial).collect();
    assert_eq!(initial, [2500.0, 2500.0, 500.0, 1500.0, 1000.0]);
}

#[test]
fn broad_based_indices_take_15_percent() {
    // S = 4,000. The 3,800 put at 20.00: OTM = 200. Broad-based, max(15 % ×
    // 4,000 − 200, 10 % × 3,800) = max(400, 380) = 400: (20 + 400) × 100 =
    // 42,000. An equity or a narrow-based index, 20 %: max(800 − 200, 380) =
    // 600, 62,000.
    let put = [option(10, 1, Right::Put, 3800.0, 30, -1.0, 20.0)];
    for (class, want) in [(IndexClass::BroadIndex, 42_000.0), (IndexClass::Equity, 62_000.0), (IndexClass::NarrowIndex, 62_000.0)] {
        assert_close(World::default().underlying(1, 4000.0, class).margin(&[Model::RegT], &put).maintenance, want);
    }
    // The 4,600 call at 2.00, broad-based: OTM = 600, max(600 − 600, 10 % ×
    // 4,000) = 400, the floor: (2 + 400) × 100 = 40,200.
    let w = World::default().underlying(1, 4000.0, IndexClass::BroadIndex);
    assert_close(w.margin(&[Model::RegT], &[option(10, 1, Right::Call, 4600.0, 30, -1.0, 2.0)]).maintenance, 40_200.0);
}

// ---------------------------------------------------------------------------
// reg_t's minimum over pairings against every whole pairing.

/// SplitMix64: a small deterministic generator.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n
    }
}

/// One option leg of a random book (m = 100).
#[derive(Clone, Copy, Debug)]
struct Leg {
    right: Right,
    strike: f64,
    days: i64,
    q: f64,
    mark: f64,
}

/// A use of whole units other than naked: (short, long), (call, put), or a
/// short covered by the stock.
#[derive(Clone, Copy, Debug)]
enum Use {
    Spread(usize, usize),
    Straddle(usize, usize),
    Covered(usize),
}

/// What the legs have left during an enumeration.
struct Left {
    shorts: Vec<f64>,
    longs: Vec<f64>,
    covers: f64,
}

impl Left {
    fn room(&self, u: Use) -> f64 {
        match u {
            Use::Spread(s, l) => self.shorts[s].min(self.longs[l]),
            Use::Straddle(c, p) => self.shorts[c].min(self.shorts[p]),
            Use::Covered(s) => self.shorts[s].min(self.covers),
        }
    }

    fn take(&mut self, u: Use, n: f64) {
        match u {
            Use::Spread(s, l) => {
                self.shorts[s] -= n;
                self.longs[l] -= n;
            }
            Use::Straddle(c, p) => {
                self.shorts[c] -= n;
                self.shorts[p] -= n;
            }
            Use::Covered(s) => {
                self.shorts[s] -= n;
                self.covers -= n;
            }
        }
    }
}

/// Every whole pairing's requirement for the options.
struct Pairings {
    uses: Vec<(Use, f64)>,
    naked: Vec<f64>,
    paid: Vec<f64>,
    totals: Vec<f64>,
}

impl Pairings {
    fn walk(&mut self, k: usize, left: &mut Left, cost: f64) {
        let Some(&(u, per_unit)) = self.uses.get(k) else {
            let naked: f64 = left.shorts.iter().zip(&self.naked).map(|(q, n)| q * n).sum();
            let paid: f64 = left.longs.iter().zip(&self.paid).map(|(q, p)| q * p).sum();
            self.totals.push(cost + naked + paid);
            return;
        };
        for n in 0..=left.room(u) as usize {
            let n = n as f64;
            left.take(u, n);
            self.walk(k + 1, left, cost + per_unit * n);
            left.take(u, -n);
        }
    }
}

/// The rules of K-5.6 written out again: a naked contract, a long one paid
/// for, a spread unit.
fn naked_ref(l: &Leg, s: f64, rate: f64) -> f64 {
    let (otm, floor) = match l.right {
        Right::Call => ((l.strike - s).max(0.0), 0.1 * s),
        Right::Put => ((s - l.strike).max(0.0), 0.1 * l.strike),
    };
    100.0 * (l.mark + (rate * s - otm).max(floor))
}

fn paid_ref(l: &Leg) -> f64 {
    100.0 * l.mark * if l.days <= 274 { 1.0 } else { 0.75 }
}

fn spread_ref(short: &Leg, long: &Leg) -> Option<f64> {
    let width = match short.right {
        Right::Call => (long.strike - short.strike).max(0.0),
        Right::Put => (short.strike - long.strike).max(0.0),
    };
    (short.right == long.right && long.days >= short.days).then(|| (100.0 * (long.mark - short.mark + width)).max(0.0))
}

#[test]
fn the_minimum_over_pairings_beats_every_whole_pairing() {
    let mut rng = Rng(2024);
    let mut gained = 0;
    for book in 0..400 {
        let s = 90.0 + rng.below(21) as f64;
        let (class, rate) = [(IndexClass::Equity, 0.2), (IndexClass::BroadIndex, 0.15), (IndexClass::NarrowIndex, 0.2)][rng.below(3) as usize];
        let shares = 100.0 * (rng.below(7) as f64 - 3.0);
        let (mut shorts, mut longs) = (Vec::new(), Vec::new());
        for _ in 0..=rng.below(4) {
            let right = if rng.below(2) == 0 { Right::Call } else { Right::Put };
            let leg = Leg {
                right,
                strike: s - 10.0 + 5.0 * rng.below(5) as f64,
                days: [30, 60, 400][rng.below(3) as usize],
                q: 1.0 + rng.below(3) as f64,
                mark: 0.5 + 0.25 * rng.below(30) as f64,
            };
            if rng.below(2) == 0 {
                shorts.push(leg);
            } else {
                longs.push(leg);
            }
        }
        let mut holdings = vec![stock(1, shares, s)];
        let legs = shorts.iter().map(|l| (l, -l.q)).chain(longs.iter().map(|l| (l, l.q)));
        holdings.extend(legs.zip(10..).map(|((l, qty), id)| option(id, 1, l.right, l.strike, l.days, qty, l.mark)));
        let m = World::default().underlying(1, s, class).margin(&[Model::RegT], &holdings);

        // Every whole pairing, by enumeration.
        let naked: Vec<f64> = shorts.iter().map(|l| naked_ref(l, s, rate)).collect();
        let mut uses = Vec::new();
        for (i, short) in shorts.iter().enumerate() {
            uses.extend(longs.iter().enumerate().filter_map(|(j, long)| Some((Use::Spread(i, j), spread_ref(short, long)?))));
            for (j, put) in shorts.iter().enumerate().filter(|(_, p)| short.right == Right::Call && p.right == Right::Put) {
                let (call_n, put_n) = (naked[i], naked[j]);
                uses.push((Use::Straddle(i, j), if call_n >= put_n { call_n + 100.0 * put.mark } else { put_n + 100.0 * short.mark }));
            }
            if (short.right == Right::Call && shares > 0.0) || (short.right == Right::Put && shares < 0.0) {
                uses.push((Use::Covered(i), 0.0));
            }
        }
        let mut pairings = Pairings {
            uses,
            naked,
            paid: longs.iter().map(paid_ref).collect(),
            totals: Vec::new(),
        };
        let mut left = Left {
            shorts: shorts.iter().map(|l| l.q).collect(),
            longs: longs.iter().map(|l| l.q).collect(),
            covers: shares.abs() / 100.0,
        };
        pairings.walk(0, &mut left, 0.0);

        // The stock's own requirement (S ≥ 5): long 25 %, short 30 %.
        let mv = shares.abs() * s;
        let stock_maintenance = if shares > 0.0 { 0.25 * mv } else { 0.3 * mv };
        let options = m.maintenance - stock_maintenance;
        let best = pairings.totals.iter().copied().fold(f64::INFINITY, f64::min);
        for total in &pairings.totals {
            assert!(options <= total + 1e-6, "book {book}: {options} above a pairing's {total}");
        }
        assert!((options - best).abs() <= 1e-6 * (1.0 + best), "book {book}: {options} against the least pairing {best}");
        assert_close(m.initial, options + 0.5 * mv);
        gained += usize::from(best < pairings.totals[0] - 1e-6);
    }
    // The all-naked pairing is the first enumerated; most books do better.
    assert!(gained > 100, "only {gained} books gained by pairing");
}

// ---------------------------------------------------------------------------
// portfolio across underlyings and classes.

#[test]
fn portfolio_takes_no_offsets_across_underlyings() {
    // Long 100 of A at 100 and short 100 of B at 100, both equities: A's worst
    // point is −15 %, 100 × 100 × 15 % = 1,500; B's is +15 %, 1,500. Netted
    // they would cancel; per underlying they need 3,000.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity).underlying(2, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::Portfolio], &[stock(1, 100.0, 100.0), stock(2, -100.0, 100.0)]);
    assert_eq!(maintenance(&m), [(1, 1500.0), (2, 1500.0)]);
    // The classes' ranges: broad-based ±8 % (long 100 at 100: 800), narrow-
    // based −6 %..+8 % (long: 600 at −6 %, short: 800 at +8 %).
    let w = World::default()
        .underlying(1, 100.0, IndexClass::BroadIndex)
        .underlying(2, 100.0, IndexClass::NarrowIndex)
        .underlying(3, 100.0, IndexClass::NarrowIndex);
    let m = w.margin(&[Model::Portfolio], &[stock(1, 100.0, 100.0), stock(2, 100.0, 100.0), stock(3, -100.0, 100.0)]);
    assert_eq!(maintenance(&m), [(1, 800.0), (2, 600.0), (3, 800.0)]);
}

#[test]
fn portfolio_falls_back_without_an_implied_volatility() {
    // Case 14's put under [portfolio, reg_t] with no IV: reg_t's 3,300, the
    // portfolio passed over with its reason.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let m = w.margin(&[Model::Portfolio, Model::RegT], &[option(10, 1, Right::Put, 95.0, 30, -2.0, 1.5)]);
    assert_close(m.maintenance, 3300.0);
    assert_eq!(row(&m, 10).source, Source::RegT);
    assert_eq!(m.fallbacks.len(), 1);
    assert_eq!((m.fallbacks[0].0, m.fallbacks[0].1), (10, Source::Portfolio));
}

// ---------------------------------------------------------------------------
// span.

/// A risk array of `combined` with the given scenarios and delta.
fn array(combined: &str, scenarios: [f64; 16], delta: f64, short_option_min: f64) -> RiskArray {
    RiskArray {
        combined: combined.to_string(),
        scenarios,
        delta,
        short_option_min,
    }
}

#[test]
fn span_calendar_spread_pays_the_intra_commodity_charge() {
    // CL December (50) long 2 at delta 1, January (51) short 2 at delta 0.96.
    // Σ qty × array_k = 2 × (dec_k − jan_k) = 40, −40, 80, −80, …, 280, −280,
    // 100, −100: the scan risk is 280, at scenario 13 (index 12). The deltas
    // +2 and −1.92 form min(2, 1.92) = 1.92 spreads at 150: 288. Maintenance
    // 280 + 288 = 568, initial × 1.1 = 624.80. At scenario 13 December loses
    // 2 × 3,500 and January gains, so December carries it all.
    let dec = [
        500.0, -500.0, 1000.0, -1000.0, 1500.0, -1500.0, 2000.0, -2000.0, 2500.0, -2500.0, 3000.0, -3000.0, 3500.0, -3500.0, 1200.0, -1200.0,
    ];
    let jan = [
        480.0, -480.0, 960.0, -960.0, 1440.0, -1440.0, 1920.0, -1920.0, 2400.0, -2400.0, 2880.0, -2880.0, 3360.0, -3360.0, 1150.0, -1150.0,
    ];
    let mut w = World::default();
    w.risk_arrays.insert(50, array("CL", dec, 1.0, 0.0));
    w.risk_arrays.insert(51, array("CL", jan, 0.96, 0.0));
    w.span.intra_spread_charge.insert("CL".into(), 150.0);
    w.span.initial_ratio.insert("CL".into(), 1.1);
    let m = w.margin(&[Model::Span], &[future(50, 2.0, 80.0, 1000.0), future(51, -2.0, 79.5, 1000.0)]);
    assert_eq!(maintenance(&m), [(50, 568.0), (51, 0.0)]);
    assert_close(m.initial, 624.8);
    assert!(m.rows.iter().all(|r| r.source.name() == "span"));
}

#[test]
fn span_inter_commodity_credit() {
    // ES long 4 (delta 1): its array peaks at 3,000 (index 10), scan 4 × 3,000
    // = 12,000, net delta +4, price risk 12,000 / 4 = 3,000. NQ short 2 (delta
    // 1): its array's least is −4,000 (index 11), scan −2 × −4,000 = 8,000, net
    // delta −2, price risk 4,000. The ES:NQ credit, 2:1 at 50 %: spreads =
    // min(4 / 2, 2 / 1) = 2; ES is credited 0.5 × 2 × 3,000 × 2 = 6,000 and NQ
    // 0.5 × 2 × 4,000 × 1 = 4,000. ES needs 12,000 − 6,000 = 6,000 and NQ
    // 8,000 − 4,000 = 4,000: 10,000 against 20,000 without the credit.
    let es = [
        500.0, -500.0, 1000.0, -1000.0, 1500.0, -1500.0, 2000.0, -2000.0, 2500.0, -2500.0, 3000.0, -3000.0, 2800.0, -2800.0, 1400.0, -1400.0,
    ];
    let nq = [
        700.0, -700.0, 1400.0, -1400.0, 2100.0, -2100.0, 2800.0, -2800.0, 3500.0, -3500.0, 4000.0, -4000.0, 3600.0, -3600.0, 1800.0, -1800.0,
    ];
    let mut w = World::default();
    w.risk_arrays.insert(60, array("ES", es, 1.0, 0.0));
    w.risk_arrays.insert(61, array("NQ", nq, 1.0, 0.0));
    let holdings = [future(60, 4.0, 4500.0, 50.0), future(61, -2.0, 15_000.0, 20.0)];
    assert_close(w.margin(&[Model::Span], &holdings).maintenance, 20_000.0);
    w.span.inter_credits.push(InterCredit {
        a: "ES".into(),
        b: "NQ".into(),
        ratio_a: 2.0,
        ratio_b: 1.0,
        rate: 0.5,
    });
    let m = w.margin(&[Model::Span], &holdings);
    assert_eq!(maintenance(&m), [(60, 6000.0), (61, 4000.0)]);
    // The same signs earn nothing: NQ long as well.
    let m = w.margin(&[Model::Span], &[future(60, 4.0, 4500.0, 50.0), future(61, 2.0, 15_000.0, 20.0)]);
    // NQ long 2: its scan is 2 × 4,000 = 8,000 (index 10); no credit.
    assert_eq!(maintenance(&m), [(60, 12_000.0), (61, 8000.0)]);
}

#[test]
fn span_short_option_minimum_binds() {
    // GC calls (70) on the future 71, short 10, far out of the money: one long
    // contract loses at most 2 and gains at most 5, so Σ qty × array_k = −10 ×
    // array_k peaks at 50 (index 9), the scan risk. One sign of delta: no
    // intra-commodity charge. The short option minimum is 10 × 25 = 250 > 50:
    // maintenance 250, initial 250 (no ratio given).
    let gc = [2.0, -1.0, 2.0, -2.0, 2.0, -3.0, 2.0, -4.0, 2.0, -5.0, 2.0, -5.0, 2.0, -5.0, 1.0, -3.0];
    let mut w = World::default();
    w.risk_arrays.insert(70, array("GC", gc, 0.02, 25.0));
    let call = Holding {
        instrument: 70,
        qty: -10.0,
        mark: 0.4,
        terms: Terms::Option {
            underlying: 71,
            right: Right::Call,
            strike: 2500.0,
            expiry: T0 + 30 * DAY,
            multiplier: 100.0,
            style: Style::American,
        },
    };
    let m = w.margin(&[Model::Span], &[call]);
    assert_close(m.maintenance, 250.0);
    assert_close(m.initial, 250.0);
    // A future of the same risk array is no option: no minimum, the scan's 50.
    let m = w.margin(&[Model::Span], &[future(70, -10.0, 2400.0, 100.0)]);
    assert_close(m.maintenance, 50.0);
}

// ---------------------------------------------------------------------------
// Selection and liquidation.

#[test]
fn selection_falls_back_in_order_and_skips_what_nothing_fits() {
    // Future 90, 3 at 100 × 1,000, with no published row and no risk array:
    // [published, reg_t, span] fits nothing (three reasons, no row);
    // [published, reg_t, approx 0.1] gives 0.1 × 3 × 100 × 1,000 = 30,000 with
    // two fallbacks.
    let w = World::default();
    let fut = [future(90, 3.0, 100.0, 1000.0)];
    let m = w.margin(&[Model::Published, Model::RegT, Model::Span], &fut);
    assert!(m.rows.is_empty() && m.maintenance == 0.0);
    let skipped: Vec<(u32, Source)> = m.skipped.iter().map(|s| (s.0, s.1)).collect();
    assert_eq!(skipped, [(90, Source::Published), (90, Source::RegT), (90, Source::Span)]);
    assert_eq!(m.skipped[1].2, "a future has no reg_t rule");
    let m = w.margin(&[Model::Published, Model::RegT, Model::Approx(0.1)], &fut);
    assert_close(m.maintenance, 30_000.0);
    assert!(m.approx_used && m.skipped.is_empty() && m.fallbacks.len() == 2);
    // No model at all: skipped once.
    assert_eq!(w.margin(&[], &fut).skipped.len(), 1);
}

#[test]
fn repeated_instruments_are_summed_and_flat_or_broken_ones_left_out() {
    // Case 14's put held as −1.5 and −0.5: one row of 3,300. A stock bought
    // and sold flat has no row; a mark that is not finite fits no model.
    let w = World::default().underlying(1, 100.0, IndexClass::Equity);
    let put = option(10, 1, Right::Put, 95.0, 30, -1.5, 1.5);
    let m = w.margin(
        &[Model::RegT],
        &[put, Holding { qty: -0.5, ..put }, stock(2, 100.0, 50.0), stock(2, -100.0, 50.0), stock(3, 10.0, f64::NAN)],
    );
    assert_eq!(maintenance(&m), [(10, 3300.0)]);
    assert_eq!(m.skipped.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>(), [(3, Source::RegT)]);
}

#[test]
fn liquidation_takes_the_largest_requirement_per_unit_first() {
    // Stock 80, long 100 at 50 (reg_t): 25 % × 5,000 = 1,250, 12.50 a share.
    // Put 81 on it, naked, S = 50, K = 45, premium 0.50, Q = 2: OTM 5, max(10
    // − 5, 4.5) = 5, (0.50 + 5) × 100 = 550 a contract. Futures 82 (3) and 83
    // (−1), published at 4,000 a contract: a tie, broken by id.
    let mut w = World::default().underlying(80, 50.0, IndexClass::Equity);
    w.published.insert(82, PublishedRow { initial: 5000.0, maintenance: 4000.0 });
    w.published.insert(83, PublishedRow { initial: 5000.0, maintenance: 4000.0 });
    let holdings = [
        stock(80, 100.0, 50.0),
        option(81, 80, Right::Put, 45.0, 30, -2.0, 0.5),
        future(83, -1.0, 4500.0, 50.0),
        future(82, 3.0, 4500.0, 50.0),
    ];
    let m = w.margin(&[Model::Published, Model::RegT], &holdings);
    let order = liquidation_order(&m, &holdings);
    assert_eq!(order.iter().map(|o| o.0).collect::<Vec<_>>(), [82, 83, 81, 80]);
    let per_unit: Vec<f64> = order.iter().map(|o| (o.1 * 1e6).round() / 1e6).collect();
    assert_eq!(per_unit, [4000.0, 4000.0, 550.0, 12.5]);
}
