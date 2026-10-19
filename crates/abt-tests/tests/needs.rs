//! The needs that admit the contract terms and the classification's levels
//! (E-102, E-98): `tick` under `need tick`, refused without it, naming the
//! need; `industry` under `need lists`, as `classification` is.

use abt_tests as common;

fn future_src(needs: &str) -> String {
    common::StrategyParts {
        name: "ticked".into(),
        class: "future".into(),
        needs: needs.into(),
        uses: "calendar, book".into(),
        universe: "universe(A, T) :- close(A, T, _).".into(),
        params: "    qty : Quantity<Contracts> = 1 contracts".into(),
        rules: "    rel min_move(A: Future, @T, F: Price<USD>)\n    min_move(A, T, F) :- tick(A, T, S), F = 10 * S.\n".into(),
        decides: "    decide(T, hold(A, qty)) :- close(A, T, P), min_move(A, T, F), P > F.".into(),
        execution: "futures_house with { orders moo }".into(),
        ..Default::default()
    }
    .render()
}

#[test]
fn tick_is_admitted_by_need_tick_and_refused_without_it() {
    let (p, diags) = common::check(&future_src("need bars @1d   need point_value, tick"), "ticked");
    assert!(p.is_some(), "{}", common::text(&diags));
    let (p, diags) = common::check(&future_src("need bars @1d   need point_value"), "ticked");
    assert!(p.is_none(), "a program reading tick without `need tick` checks");
    let text = common::text(&diags);
    assert!(text.contains("need tick"), "the diagnostic names the need: {}", text);
}

#[test]
fn industry_is_admitted_by_need_lists() {
    let src = common::StrategyParts {
        name: "industries".into(),
        class: "equity".into(),
        needs: "need bars @1d   need lists \"SPX\"".into(),
        uses: "calendar, book".into(),
        universe: "universe(A, T) :- member(A, T, \"SPX\").".into(),
        params: "    w : Scalar = 0.01".into(),
        rules: "    rel software(A: Equity, @T)\n    software(A, T) :- industry(A, T, _, \"451030\", _).\n".into(),
        decides: "    decide(T, hold(A, w)) :- universe(A, T), software(A, T).".into(),
        execution: "equities_house with { orders moc }".into(),
        ..Default::default()
    }
    .render();
    let (p, diags) = common::check(&src, "industries");
    assert!(p.is_some(), "{}", common::text(&diags));
}
