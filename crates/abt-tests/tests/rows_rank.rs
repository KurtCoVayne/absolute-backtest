//! Row windows and ranks (semantic model, section 4; language-v2, 7.2):
//! `T1 in rows[N] min K` aggregates over the entity's own last N rows, as a
//! rolling window over a table does, skipping bars where the entity has
//! none; `K = rank(R(...) by (...)) within (T)` binds each tuple's 1-based
//! rank in its group, with ties by position (`rank`) or shared as the mean
//! position (`rank_average`).

use abt_tests as common;

use abt_kernel::data::business_days;
use abt_kernel::kernel::{run, run_fold, Dataset, ExecConfig, Kernel, Value};

fn rolling() -> String {
    common::strategy(
        "rolling",
        "    n : Count = 3
    w : Scalar = 0.1
    cut_avg : Scalar = 2.0
    cut_ord : Count = 3",
        "    rel avg3(A: Equity, @T, M: Price<USD>)
    avg3(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows[n], close(A, T1, P)).
    rel px(A: Equity, @T, P: Price<USD>)
    px(A, T, P) :- universe(A, T), close(A, T, P).
    rel rk(A: Equity, @T, K: Scalar)
    rk(A, T, K) :- bar(T), K = rank_average(px(A, T, P) by (P desc)) within (T).
    rel ordk(A: Equity, @T, K: Count)
    ordk(A, T, K) :- bar(T), K = rank(px(A, T, P) by (P desc, A asc)) within (T).",
        "    decide(T, hold(A, w)) :- rk(A, T, K), ordk(A, T, K2), avg3(A, T, _), K <= cut_avg, K2 <= cut_ord.",
    )
}

/// AAA every day; BBB missing on days 3 and 4 (no bar at all).
fn market() -> (Dataset, Vec<i64>) {
    let days = business_days((2024, 1, 8), 6);
    let mut ds = Dataset::new();
    let names: [(&str, [f64; 6]); 3] = [
        ("AAA", [10.0, 11.0, 12.0, 13.0, 14.0, 15.0]),
        ("BBB", [20.0, 21.0, 0.0, 0.0, 24.0, 25.0]),
        ("CCC", [10.0, 11.0, 12.0, 13.0, 14.0, 15.0]),
    ];
    for (name, closes) in names {
        let s = ds.add_security(name, name, days[0], None);
        for (t, p) in days.iter().zip(closes) {
            if p == 0.0 {
                continue;
            }
            common::add_bar(&mut ds, s, *t, p, 1e6);
        }
    }
    ds.derive_tickers();
    (ds, days)
}

fn query(k: &mut Kernel, rel: &str, t: i64, sym: &str) -> Option<f64> {
    let id = k.symbol_names().iter().position(|n| n == sym).unwrap() as u32;
    let tus = k.query(rel, t, &[]).unwrap();
    tus.iter().find(|tu| tu[0] == Value::Equity(id)).and_then(|tu| tu[2].as_f64())
}

#[test]
fn a_rows_window_counts_the_groups_own_bars_and_skips_its_gaps() {
    let p = common::program(&rolling(), "rolling");
    let (ds, days) = market();
    let mut k = Kernel::new(&p, &ds, ExecConfig::frictionless()).unwrap();
    k.run().unwrap();
    // AAA on day 3: its last three closes 10, 11, 12.
    assert_eq!(query(&mut k, "avg3", days[2], "AAA"), Some(11.0));
    // BBB has no bar on days 3 and 4; on day 5 its last three rows are days 1, 2 and 5.
    assert_eq!(query(&mut k, "avg3", days[4], "BBB"), Some((20.0 + 21.0 + 24.0) / 3.0));
    // Fewer than three rows: withheld (BBB has two by day 2, none on day 3).
    assert_eq!(query(&mut k, "avg3", days[1], "BBB"), None);
    // A calendar window of the same span would read days 3 to 5 only.
    assert_eq!(query(&mut k, "avg3", days[5], "BBB"), Some((21.0 + 24.0 + 25.0) / 3.0));
}

#[test]
fn rank_numbers_each_tuple_with_average_or_ordinal_ties() {
    let p = common::program(&rolling(), "rolling");
    let (ds, days) = market();
    let mut k = Kernel::new(&p, &ds, ExecConfig::frictionless()).unwrap();
    k.run().unwrap();
    // Day 1: BBB 20 first; AAA and CCC tie at 10 for places 2 and 3.
    assert_eq!(query(&mut k, "rk", days[0], "BBB"), Some(1.0));
    assert_eq!(query(&mut k, "rk", days[0], "AAA"), Some(2.5));
    assert_eq!(query(&mut k, "rk", days[0], "CCC"), Some(2.5));
    // Ordinal ties break by the order's last key, the identifier.
    assert_eq!(query(&mut k, "ordk", days[0], "AAA"), Some(2.0));
    assert_eq!(query(&mut k, "ordk", days[0], "CCC"), Some(3.0));
    // Day 3 without BBB: the tie is now for first place.
    assert_eq!(query(&mut k, "rk", days[2], "AAA"), Some(1.5));
}

#[test]
fn the_fold_and_the_uncached_evaluation_agree_with_the_batch_kernel() {
    let p = common::program(&rolling(), "rolling");
    let (ds, _) = market();
    let a = run(&p, &ds, ExecConfig::frictionless()).unwrap();
    let b = run_fold(&p, &ds, ExecConfig::frictionless()).unwrap();
    let c = run(
        &p,
        &ds,
        ExecConfig {
            window_cache: false,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(a.decisions.len(), b.decisions.len());
    assert_eq!(a.decisions.len(), c.decisions.len());
    assert!(!a.decisions.is_empty());
    assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits());
    assert_eq!(a.final_cash.to_bits(), c.final_cash.to_bits());
}

#[test]
fn the_checker_judges_rows_and_rank() {
    let src = rolling();
    let bad_len = src.replace("rows[n]", "rows[3d]");
    let (p, diags) = common::check(&bad_len, "rolling");
    assert!(p.is_none() && common::text(&diags).contains("a Count (the number of rows)"), "{}", common::text(&diags));
    // Ordinal ties need a total order; averaged ones do not.
    let partial = src.replace("by (P desc, A asc)) within (T)", "by (P desc)) within (T)");
    let (p, diags) = common::check(&partial, "rolling");
    assert!(p.is_none() && diags.iter().any(|d| d.code == abt_kernel::check::Code::D), "{}", common::text(&diags));
    let rebound = src.replace("K = rank_average(px(A, T, P) by (P desc)) within (T)", "P = rank_average(px(A, T, P) by (P desc)) within (T)");
    let (p, diags) = common::check(&rebound, "rolling");
    assert!(p.is_none() && common::text(&diags).contains("fresh"), "{}", common::text(&diags));
}

/// A rows window counts the entity's own rows (the bars at which it is in
/// the universe) and aggregates the rows where the whole conjunction holds,
/// as a rolling window with a minimum of periods: a row without a value
/// still takes a place in the window.
#[test]
fn a_rows_window_counts_the_anchors_rows_and_aggregates_the_valid_ones() {
    let src = common::strategy(
        "sparse",
        "    floor : Price<USD> = 11 USD/share
    w : Scalar = 0.1",
        "    # A value only on days with a close above the floor.
    rel high_close(A: Equity, @T, P: Price<USD>)
    high_close(A, T, P) :- close(A, T, P), P > floor.
    rel last3(A: Equity, @T, M: Price<USD>)
    last3(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows[3] min 1, high_close(A, T1, P)).",
        "    decide(T, hold(A, w)) :- universe(A, T), last3(A, T, _).",
    );
    let p = common::program(&src, "sparse");
    let (ds, days) = market();
    let mut k = Kernel::new(&p, &ds, ExecConfig::frictionless()).unwrap();
    k.run().unwrap();
    // AAA closes 10, 11, 12, 13: on day 4 the window is days 2 to 4 and only
    // 12 and 13 have values.
    assert_eq!(query(&mut k, "last3", days[3], "AAA"), Some(12.5));
    // Day 2: the window is days 1 and 2, neither above 11: no value at all.
    assert_eq!(query(&mut k, "last3", days[1], "AAA"), None);
}

/// The xsec library's `decile[R]` is callable (a relation with bracketed
/// inputs may share a function's name, `decile`), and ten names with
/// increasing returns fall in the ten deciles in order.
#[test]
fn the_xsec_decile_ranks_ten_names_into_ten_tenths() {
    let src = common::StrategyParts {
        name: "tenths".into(),
        uses: "calendar, bars, book, xsec".into(),
        params: "    w : Scalar = 0.1".into(),
        rules: "    rel dec(A: Equity, @T, K: Count)
    dec(A, T, K) :- universe(A, T), decile[price_return](A, T, K)."
            .into(),
        decides: "    decide(T, hold(A, w)) :- dec(A, T, K), K >= 9.".into(),
        ..Default::default()
    }
    .render();
    let prog = common::program(&src, "tenths");
    let names: Vec<String> = (0..10).map(|i| format!("N{}", i)).collect();
    let closes: Vec<[f64; 3]> = (0..10).map(|i| [10.0, 10.0 + 0.1 * (i + 1) as f64, 10.0 + 0.1 * (i + 1) as f64]).collect();
    let series: Vec<(&str, &[f64])> = names.iter().zip(&closes).map(|(n, c)| (n.as_str(), &c[..])).collect();
    let ds = common::markets((2024, 1, 8), &series);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    k.run().unwrap();
    let t = common::day("2024-01-09");
    let mut got: Vec<(String, i64)> = k
        .query("dec", t, &[])
        .unwrap()
        .iter()
        .map(|tu| (k.show(&tu[0]), tu[2].as_f64().map(|x| x as i64).unwrap_or(-1)))
        .collect();
    got.sort();
    let want: Vec<(String, i64)> = (0..10).map(|i| (format!("N{}", i), i + 1)).collect();
    assert_eq!(got, want);
}
