//! E-114: the round trips of a window are the run's that close in it. A
//! reveal's window opening on a held position counted none of the market's
//! trades (its fills were the window's alone, the exit read as an entry),
//! while the position's P&L stayed in the net profit.

use abt_query::metrics::trade_metrics;
use abt_query::run::{BookRow, Fill, Run};

const DAY: i64 = 86_400_000_000_000;

fn fill(seq: u64, day: i64, quantity: f64, price: f64) -> Fill {
    Fill {
        seq,
        t: day * DAY,
        instrument: Some(7),
        quantity,
        price,
        commission: 0.0,
        fee: 0.0,
        slippage: 0.0,
        impact: 0.0,
        participation: 0.0,
        multiplier: 1.0,
    }
}

fn book(day: i64, nav: f64) -> BookRow {
    BookRow {
        t: day * DAY,
        cash: nav,
        nav,
        gross: 0.0,
        net: 0.0,
        leverage: 0.0,
        receivables: 0.0,
        capital: 1000.0,
        margin_initial: 0.0,
        margin_maintenance: 0.0,
        variation: 0.0,
    }
}

#[test]
fn a_trade_open_at_the_boundary_is_the_windows_it_closes_in() {
    // One market: long from day 2 to day 8 (+6), long from day 12 to day 14
    // (-1); the window opens on day 5, the position held.
    let run = Run {
        id: "r".into(),
        book: (0..16).map(|d| book(d, 1000.0 + if d >= 8 { 6.0 } else { 0.0 } - if d >= 14 { 1.0 } else { 0.0 })).collect(),
        fills: vec![fill(1, 2, 1.0, 100.0), fill(2, 8, -1.0, 106.0), fill(3, 12, 1.0, 110.0), fill(4, 14, -1.0, 109.0)],
        ..Run::default()
    };
    let window = trade_metrics(&run, 252.0, &|t| t >= 5 * DAY);
    assert_eq!(window.trades, 2, "both trades close in the window");
    assert_eq!(window.hit_rate, 0.5);
    assert_eq!(window.profit_factor, 6.0);
    let before = trade_metrics(&run, 252.0, &|t| t < 5 * DAY);
    assert_eq!(before.trades, 0, "no trade closes before the window");
    assert_eq!(trade_metrics(&run, 252.0, &|_| true).trades, 2);
}
