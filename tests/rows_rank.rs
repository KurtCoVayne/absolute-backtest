//! Row windows and ranks (semantic model, section 4): `T1 in rows(T, N, min
//! K)` aggregates over the group's own last N rows, as a rolling window over
//! a table does, skipping bars where the group has none; `rank(R(...), by
//! (...), as K)` binds each tuple's 1-based rank in its group, with ties by
//! position (`ordinal`) or shared as the mean position (`ties average`).

mod corpus;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::business_days;
use absolute_backtest::kernel::{run, run_fold, Dataset, ExecConfig, Kernel, Value};

fn checked(src: &str, name: &str) -> (Option<Program>, Vec<String>) {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    (p, diags.iter().map(|d| d.to_string()).collect())
}

fn program(src: &str, name: &str) -> Program {
    let (p, diags) = checked(src, name);
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, diags.join("\n")))
}

const ROLLING: &str = r#"
strategy rolling {
  env equities_1d
  uses features
  resolution @1d
  mode target
  rel avg3(+A: Equity, @T: Timestamp, -M: Price<USD>)
  avg3(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows(T, 3, min 3), close(A, T1, P)).
  rel px(-A: Equity, @T: Timestamp, -P: Price<USD>)
  px(A, T, P) :- universe(A, T), close(A, T, P).
  rel rk(-A: Equity, @T: Timestamp, -K: Scalar)
  rk(A, T, K) :- bar(T), rank(px(A, T, P), by (P desc), ties average, as K).
  rel ordk(-A: Equity, @T: Timestamp, -K: Scalar)
  ordk(A, T, K) :- bar(T), rank(px(A, T, P), by (P desc, A asc), as K).
  decide(T, target_weight(A, 0.1)) :- rk(A, T, K), ordk(A, T, K2), avg3(A, T, _), K <= 1.5, K2 <= 3.
}
"#;

/// AAA every day; BBB missing on days 3 and 4 (no bar at all).
fn market() -> (Dataset, Vec<i64>) {
    let days = business_days((2024, 1, 8), 6);
    let mut ds = Dataset::new();
    let (a, b, c) = (ds.intern("AAA"), ds.intern("BBB"), ds.intern("CCC"));
    let pa = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0];
    let pb = [20.0, 21.0, 0.0, 0.0, 24.0, 25.0];
    let pc = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0];
    for (i, t) in days.iter().enumerate() {
        for (s, p) in [(a, pa[i]), (b, pb[i]), (c, pc[i])] {
            if p == 0.0 {
                continue;
            }
            ds.add("close", vec![Value::Equity(s), Value::Time(*t), Value::Num(p)]);
            ds.add("volume", vec![Value::Equity(s), Value::Time(*t), Value::Num(1e6)]);
            ds.add("universe", vec![Value::Equity(s), Value::Time(*t)]);
        }
    }
    ds.derive_tickers();
    (ds, days)
}

fn query(k: &mut Kernel, rel: &str, t: i64, sym: &str) -> Option<f64> {
    let id = k.symbol_names().iter().position(|n| n == sym).unwrap() as u32;
    // `avg3` takes the equity as an input; the ranks enumerate it.
    let inputs = if rel == "avg3" { vec![Value::Equity(id)] } else { vec![] };
    let tus = k.query(rel, t, &inputs).unwrap();
    tus.iter().find(|tu| tu[0] == Value::Equity(id)).and_then(|tu| tu[2].as_f64())
}

#[test]
fn a_rows_window_counts_the_groups_own_bars_and_skips_its_gaps() {
    let p = program(ROLLING, "rolling");
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
    let p = program(ROLLING, "rolling");
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
    let p = program(ROLLING, "rolling");
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
    let bad_len = ROLLING.replace("rows(T, 3, min 3)", "rows(T, 3d, min 3)");
    let (p, diags) = checked(&bad_len, "rolling");
    assert!(p.is_none() && diags.iter().any(|d| d.contains("a Count (the number of rows)")), "{:?}", diags);
    // Ordinal ties need a total order; averaged ones do not.
    let partial = ROLLING.replace("by (P desc, A asc), as K", "by (P desc), as K");
    let (p, diags) = checked(&partial, "rolling");
    assert!(p.is_none() && diags.iter().any(|d| d.contains("not total")), "{:?}", diags);
    let rebound = ROLLING.replace("rank(px(A, T, P), by (P desc), ties average, as K)", "rank(px(A, T, P), by (P desc), ties average, as P)");
    let (p, diags) = checked(&rebound, "rolling");
    assert!(p.is_none() && diags.iter().any(|d| d.contains("fresh")), "{:?}", diags);
}
