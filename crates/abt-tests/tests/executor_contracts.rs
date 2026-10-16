//! Contract terms and book conventions (data-bundle doc, sections 3 to 5):
//! a future's multiplier and per-contract commission, back-adjusted prices
//! that cross zero, delisting at the last price, dividends reinvested, and
//! a commission in basis points on both sides.

use abt_tests as common;

use abt_kernel::data::{business_days, load_parquet_dir};
use abt_kernel::kernel::{Action, Contract, Dataset, ExecConfig, Lot, Sym, Value};
use abt_kernel::table::write_text_table;
use abt_tests::run;

/// The decide rules of a round trip: buy `qty` once when flat, sell it all
/// on the day `day3` is bound.
const ROUND_TRIP_RULES: &str = "    rel day3(@T)\n    day3(T) :- bar(T), prev(T, T1), prev(T1, T2), prev(T2, _).";
const ROUND_TRIP_DECIDES: &str = "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T), not day3(T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), day3(T), Q = -Q0.";
/// Buy and hold.
const HOLD_DECIDES: &str = "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).";

/// A daily equity strategy over the catalog (bars, actions and the listing).
fn equity(name: &str, params: &str, rules: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        needs: "need bars @1d   need actions   need listing".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        ..Default::default()
    }
    .render()
}

/// A futures strategy on the minute base, deciding at every bar on every
/// root that traded, with the futures execution preset.
fn future(name: &str, params: &str, rules: &str, decides: &str) -> String {
    common::StrategyParts {
        name: name.into(),
        class: "future".into(),
        needs: "need bars @1m   need sessions, point_value, commission".into(),
        uses: "calendar, book".into(),
        calendars: Some("session = sessions".into()),
        universe: "universe(A, T) :- traded@base(A, T).".into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        execution: "futures_fixed_base".into(),
        ..Default::default()
    }
    .render()
}

fn equity_round_trip() -> String {
    equity("contracts_round_trip", "    qty : Quantity<Shares> = 2 shares", ROUND_TRIP_RULES, ROUND_TRIP_DECIDES)
}

fn equity_hold() -> String {
    equity("contracts_hold", "    qty : Quantity<Shares> = 10 shares", "", HOLD_DECIDES)
}

fn future_round_trip() -> String {
    future("contracts_round_trip", "    qty : Quantity<Contracts> = 2 contracts", ROUND_TRIP_RULES, ROUND_TRIP_DECIDES)
}

fn future_hold() -> String {
    future("contracts_hold", "    qty : Quantity<Contracts> = 10 contracts", "", HOLD_DECIDES)
}

/// One symbol X over consecutive weekdays from 2024-01-08 (open, high, low
/// and close at the given closes, volume a million), listed every day.
fn market(closes: &[f64]) -> (Dataset, Sym, Vec<i64>) {
    let days = business_days((2024, 1, 8), closes.len());
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    for (t, p) in days.iter().zip(closes) {
        common::add_bar(&mut ds, x, *t, *p, 1_000_000.0);
    }
    ds.derive_tickers();
    (ds, x, days)
}

/// One root X over consecutive weekdays from 2024-01-08, each session one
/// minute bar at 09:31 (open, high, low and close at the given close,
/// volume a million) labelled as the session with its 09:30 open, the point
/// value and the commission per contract as daily facts, and the contract
/// terms in the security table when given (a share otherwise).
fn futures_market(closes: &[f64], contract: Option<Contract>) -> (Dataset, Sym) {
    let days = business_days((2024, 1, 8), closes.len());
    let mut ds = Dataset::new();
    let x = ds.add_security("X", "X", days[0], None);
    let point_value = contract.as_ref().map(|c| c.multiplier).unwrap_or(1.0);
    let commission = contract.as_ref().and_then(|c| c.commission_per_contract).unwrap_or(0.0);
    for (d, p) in days.iter().zip(closes) {
        let open = d + 9 * abt_kernel::kernel::time::HOUR + 30 * abt_kernel::kernel::time::MIN;
        let t = open + abt_kernel::kernel::time::MIN;
        for rel in ["open", "high", "low", "close"] {
            ds.add(rel, vec![Value::Equity(x), Value::Time(t), Value::Num(*p)]);
        }
        ds.add("volume", vec![Value::Equity(x), Value::Time(t), Value::Num(1_000_000.0)]);
        ds.add("session", vec![Value::Equity(x), Value::Time(t), Value::Time(open)]);
        ds.add("point_value", vec![Value::Equity(x), Value::Time(*d), Value::Num(point_value)]);
        ds.add("commission", vec![Value::Equity(x), Value::Time(*d), Value::Num(commission)]);
    }
    if let Some(c) = contract {
        ds.contracts.insert(x, c);
    }
    ds.derive_tickers();
    (ds, x)
}

fn cfg() -> ExecConfig {
    ExecConfig {
        initial_cash: 100_000.0,
        margin_rate: 0.0,
        lot: Lot::Fractional,
        ..ExecConfig::frictionless()
    }
}

#[test]
fn a_future_pays_its_multiplier_on_every_point_and_its_commission_per_contract() {
    // Bought 2 at 100 on day 2, sold at 103 on day 5: 2 x 3 x 50 = 300,
    // less 2.5 a contract a side on 4 contracts.
    let closes = [100.0, 100.0, 101.0, 102.0, 103.0, 103.0];
    let (ds, _) = futures_market(
        &closes,
        Some(Contract {
            multiplier: 50.0,
            future: true,
            commission_per_contract: Some(2.5),
        }),
    );
    let r = run(&common::program(&future_round_trip(), "contracts_round_trip"), &ds, cfg()).unwrap();
    let qty: Vec<f64> = r.fills.iter().map(|f| f.quantity).collect();
    assert_eq!(qty, vec![2.0, -2.0], "{:?}", r.fills);
    assert!((r.final_cash - (100_000.0 + 300.0 - 10.0)).abs() < 1e-9, "{}", r.final_cash);
    assert!((r.costs.commissions - 10.0).abs() < 1e-12);
    // The book is marked at the multiplier: day 3's equity holds 2 x 1 x 50.
    let eq: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert!((eq[2] - (100_000.0 - 5.0 + 100.0)).abs() < 1e-9, "{:?}", eq);
    // Without the contract terms the same trade is two shares.
    let (plain, _) = futures_market(&closes, None);
    let r = run(&common::program(&future_round_trip(), "contracts_round_trip"), &plain, cfg()).unwrap();
    assert!((r.final_cash - 100_006.0).abs() < 1e-9, "{}", r.final_cash);
}

#[test]
fn a_target_weight_of_a_future_is_sized_by_its_notional() {
    let src = future("future_weight", "    w : Scalar = 0.5", "", "    decide(T, hold(A, w)) :- universe(A, T).");
    let (ds, _) = futures_market(
        &[100.0, 100.0, 100.0],
        Some(Contract {
            multiplier: 50.0,
            future: true,
            commission_per_contract: None,
        }),
    );
    let r = run(&common::program(&src, "future_weight"), &ds, cfg()).unwrap();
    // Half of 100,000 at 100 x 50 a contract: 10 contracts.
    assert_eq!(r.fills[0].quantity, 10.0, "{:?}", r.fills);
}

#[test]
fn a_back_adjusted_future_may_cross_zero_and_a_share_may_not() {
    let dir = std::env::temp_dir().join(format!("abt-contracts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |class: &str| {
        write_text_table(
            &dir.join("securities.parquet"),
            &format!("id,ticker,from,to,multiplier,asset_class,commission_per_contract\nCL1,CL,2024-01-01,,1000,{},2.0\n", class),
        )
        .unwrap();
        for (rel, col) in [("open", "O"), ("high", "H"), ("low", "L"), ("close", "P")] {
            let rows = format!("A,T,{}\nCL1,2024-01-08,1.5\nCL1,2024-01-09,-0.5\nCL1,2024-01-10,0.25\n", col);
            write_text_table(&dir.join(format!("{}.parquet", rel)), &rows).unwrap();
        }
    };
    write("future");
    let p = common::program(&future_hold(), "contracts_hold");
    let (ds, _) = abt_tests::load_fixture(&p, &dir).unwrap();
    let sym = ds.symbols.names().iter().position(|n| n == "CL1").unwrap() as u32;
    assert_eq!(ds.contract(sym).multiplier, 1000.0);
    assert!(ds.is_future(sym));
    // 10 contracts bought at -0.5 on day 2, marked at 0.25 on day 3: +7,500 less 20 commission.
    let r = run(&p, &ds, cfg()).unwrap();
    assert_eq!(r.fills[0].price, -0.5);
    let last = r.equity_curve.last().unwrap().1;
    assert!((last - (100_000.0 + 7_500.0 - 20.0)).abs() < 1e-9, "{}", last);
    write("equity");
    let p = common::program(&equity_hold(), "contracts_hold");
    let err = load_parquet_dir(&p, &dir).expect_err("a share's price is positive");
    assert!(err.contains("close.parquet row 2") || err.contains("not a positive price"), "{}", err);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_delisting_at_the_last_price_has_no_haircut_and_no_cost() {
    let (mut ds, x, days) = market(&[10.0, 10.0, 12.0, 12.0]);
    let reason = ds.labels.intern("bankruptcy");
    ds.add("delisted", vec![Value::Equity(x), Value::Time(days[3]), Value::Label(reason)]);
    let p = common::program(&equity_hold(), "contracts_hold");
    let by_reason = run(&p, &ds, ExecConfig { commission_per_share: 0.01, ..cfg() }).unwrap();
    // Bankruptcy's default haircut is a total loss.
    assert!((by_reason.final_cash - (100_000.0 - 100.0 - 0.1 - 0.1)).abs() < 1e-9, "{}", by_reason.final_cash);
    let last = run(
        &p,
        &ds,
        ExecConfig {
            commission_per_share: 0.01,
            delist_at_last_price: true,
            ..cfg()
        },
    )
    .unwrap();
    assert!((last.final_cash - (100_000.0 - 100.0 - 0.1 + 120.0)).abs() < 1e-9, "{}", last.final_cash);
    assert!(last.final_positions.is_empty());
    assert!(last.actions.iter().any(|a| matches!(&a.action, Action::Delisting { haircut, .. } if *haircut == 0.0)));
    assert!(!last.warnings.iter().any(|w| w.bias == "delisting"), "{:?}", last.warnings);
}

#[test]
fn a_reinvested_dividend_buys_shares_at_the_ex_date_close() {
    // 10 shares bought on day 2 at 20; ex day 3 at 19 with 1.00 a share:
    // 10 / 19 shares more, no cash, no receivable.
    let (mut ds, x, days) = market(&[20.0, 20.0, 19.0, 19.0, 19.0]);
    ds.add("dividend", vec![Value::Equity(x), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[4]), Value::Num(1.0)]);
    let p = common::program(&equity_hold(), "contracts_hold");
    let r = run(&p, &ds, ExecConfig { reinvest_dividends: true, ..cfg() }).unwrap();
    let held = r.final_positions[&r.fills[0].equity];
    assert!((held - (10.0 + 10.0 / 19.0)).abs() < 1e-12, "{}", held);
    assert!((r.final_cash - 99_800.0).abs() < 1e-9, "{}", r.final_cash);
    assert!(r.actions.iter().any(|a| matches!(a.action, Action::Reinvest { amount, .. } if amount == 1.0)));
    // The book's value is unchanged by the dividend: 10 x 20 = (10 + 10/19) x 19.
    let eq: Vec<f64> = r.equity_curve.iter().map(|(_, e)| *e).collect();
    assert!((eq[2] - 100_000.0).abs() < 1e-9, "{:?}", eq);
}

#[test]
fn a_basis_point_commission_is_charged_on_both_sides() {
    let (ds, _, _) = market(&[100.0, 100.0, 101.0, 102.0, 103.0, 103.0]);
    let r = run(&common::program(&equity_round_trip(), "contracts_round_trip"), &ds, ExecConfig { commission_bps: 10.0, ..cfg() }).unwrap();
    // 2 x 100 x 10 bps on the buy, 2 x 103 x 10 bps on the sell.
    assert!((r.costs.commissions - (0.2 + 0.206)).abs() < 1e-12, "{}", r.costs.commissions);
}

#[test]
fn actions_already_in_the_prices_are_not_applied_to_the_book() {
    let (mut ds, x, days) = market(&[20.0, 20.0, 10.0, 10.0]);
    ds.add("split", vec![Value::Equity(x), Value::Time(days[2]), Value::Num(2.0)]);
    let p = common::program(&equity_hold(), "contracts_hold");
    let applied = run(&p, &ds, cfg()).unwrap();
    assert_eq!(applied.final_positions.values().copied().collect::<Vec<_>>(), vec![20.0]);
    let in_prices = run(&p, &ds, ExecConfig { actions_in_prices: true, ..cfg() }).unwrap();
    assert_eq!(in_prices.final_positions.values().copied().collect::<Vec<_>>(), vec![10.0]);
    assert!(in_prices.actions.is_empty());
}

#[test]
fn a_future_is_shorted_without_a_borrow_fee_or_a_locate() {
    let src = future(
        "short_future",
        "    qty : Quantity<Contracts> = 2 contracts",
        "",
        "    decide(T, trade(A, Q)) :- universe(A, T), not position(A, T, _), Q = -qty.",
    );
    let (ds, _) = futures_market(
        &[100.0, 100.0, 100.0, 100.0],
        Some(Contract {
            multiplier: 50.0,
            future: true,
            commission_per_contract: None,
        }),
    );
    // Default borrow buckets and margin: a share would pay a fee (or be refused).
    let r = run(
        &common::program(&src, "short_future"),
        &ds,
        ExecConfig {
            lot: Lot::Fractional,
            ..ExecConfig::default()
        },
    )
    .unwrap();
    assert_eq!(r.fills.first().map(|f| f.quantity), Some(-2.0), "{:?}", r.dropped);
    assert_eq!(r.funding.borrow_fees, 0.0);
}
