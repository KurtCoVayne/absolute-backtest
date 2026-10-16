//! The order an aggregate reads its window in (P-1.2, Definition V-3.5): the
//! window's time order. `first` and `last` are the window's end rows, so a
//! bar view's open is the bucket's first open and its close the bucket's
//! last close, whatever the values; and a sum adds in time order (ERRATA
//! E-46). Hand-computed on one week of closes 12, 14, 15, 11, 9, whose first
//! and last rows are neither the smallest nor the largest.

#![allow(clippy::result_large_err)]

use abt_kernel::kernel::time::parse_timestamp;
use abt_kernel::kernel::{ExecConfig, Value};
use abt_tests as common;
use abt_tests::StrategyParts;

fn program() -> abt_kernel::check::Program {
    let src = StrategyParts {
        name: "o".into(),
        calendars: Some("weekly = weeks(1)".into()),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        rules: [
            "    rel ends(A: Equity, @T: weekly, O: Price<USD>, C: Price<USD>)",
            "    ends(A, T, O, C) :- open@weekly(A, T, O), close@weekly(A, T, C).",
            "    rel edges(A: Equity, @T, F: Price<USD>, L: Price<USD>)",
            "    edges(A, T, F, L) :- universe(A, T), F = first(X1) over (T1 in rows[3], close(A, T1, X1)), L = last(X2) over (T2 in rows[3], close(A, T2, X2)).",
            "    rel span_edges(A: Equity, @T, F: Price<USD>, L: Price<USD>)",
            "    span_edges(A, T, F, L) :- universe(A, T), F = first(X1) over (T1 in window[4d] min 1, close(A, T1, X1)), L = last(X2) over (T2 in window[4d] min 1, close(A, T2, X2)).",
        ]
        .join("\n"),
        decides: "    decide(T, trade(A, qty)) :- edges(A, T, F, L), span_edges(A, T, F2, L2), L > F, L2 > F2, flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "o")
}

fn nums(k: &mut abt_kernel::kernel::Kernel, rel: &str, t: i64) -> Vec<Vec<f64>> {
    k.query(rel, t, &[])
        .unwrap()
        .into_iter()
        .map(|tu| {
            tu.iter()
                .filter_map(|v| match v {
                    Value::Num(x) => Some(*x),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

#[test]
fn a_bar_view_and_a_window_read_their_end_rows_in_time_order() {
    let p = program();
    let ds = common::market("AAA", &[12.0, 14.0, 15.0, 11.0, 9.0, 10.0]);
    let (_, mut k) = common::run_kernel(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    let friday = parse_timestamp("2024-01-12").unwrap();
    // The week's open is Monday's, its close Friday's: not its lowest open
    // nor its highest close.
    assert_eq!(nums(&mut k, "ends", friday), vec![vec![12.0, 9.0]]);
    // Friday's last three rows are 15, 11, 9: the first is 15, the last 9.
    assert_eq!(nums(&mut k, "edges", friday), vec![vec![15.0, 9.0]]);
    // The four days ending on the next Monday ([T - 4d, T]) hold Thursday's
    // 11, Friday's 9 and Monday's 10: the first is 11, the last 10.
    let monday = parse_timestamp("2024-01-15").unwrap();
    assert_eq!(nums(&mut k, "span_edges", monday), vec![vec![11.0, 10.0]]);
}
