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

fn equity_src(needs: &str, rules: &str, decide_on: &str) -> String {
    common::StrategyParts {
        name: "vocab".into(),
        class: "equity".into(),
        needs: needs.into(),
        uses: "calendar, book".into(),
        universe: "universe(A, T) :- close(A, T, _).".into(),
        params: "    w : Scalar = 0.01".into(),
        rules: rules.into(),
        decides: format!("    decide(T, hold(A, w)) :- universe(A, T), {}.", decide_on),
        execution: "equities_house with { orders moc }".into(),
        ..Default::default()
    }
    .render()
}

/// E-103: the index lists and the daily index series the full store stages
/// are in the catalog's vocabularies (WF-15), and `VIX` stays the weekly
/// series MW14 reads while the lake's daily one is `VIX_D`.
#[test]
fn the_full_stores_lists_and_series_are_in_the_vocabulary() {
    for (needs, rules, on) in [
        ("need bars @1d   need lists \"R2000\"", "", "member(A, T, \"R2000\")"),
        ("need bars @1d   need lists \"NASDAQ\", \"SPDA\"", "", "member(A, T, \"SPDA\")"),
        ("need bars @1d   need series \"NDX\" @1d", "    rel up(@T)\n    up(T) :- value(\"NDX\", T, V), V > 0.\n", "up(T)"),
        (
            "need bars @1d   need series \"VIX_D\" @1d",
            "    rel calm(@T)\n    calm(T) :- value(\"VIX_D\", T, V), V < 20.\n",
            "calm(T)",
        ),
    ] {
        let (p, diags) = common::check(&equity_src(needs, rules, on), "vocab");
        assert!(p.is_some(), "{}: {}", needs, common::text(&diags));
    }
    let (p, diags) = common::check(&equity_src("need bars @1d   need lists \"R5000\"", "", "member(A, T, \"R5000\")"), "vocab");
    assert!(p.is_none(), "an unknown list label checks");
    assert!(common::text(&diags).contains("R5000"), "{}", common::text(&diags));
}
