//! The Analyse stage (compiler design, section 3): the structural facts
//! (components, the open-loop partition, keyedness labels, demand sets,
//! window lengths) and the abstract interpretation with its warnings A001
//! to A010, asserted on programs written to trigger each code, and run over
//! every corpus strategy.

mod common;

use std::fs;

use absolute_backtest::analysis::{analyse, Analysis, Keyedness, RelationFacts};
use absolute_backtest::check::Program;
use absolute_backtest::compiler::normalise;
use absolute_backtest::compiler::Stage;
use absolute_backtest::plan::cost::Statistics;

fn analysed(src: &str, name: &str) -> (Program, Analysis) {
    let p = common::program(src, name);
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    (p, a)
}

fn codes(a: &Analysis) -> Vec<String> {
    a.warnings.iter().map(|w| w.code.clone()).collect()
}

/// A library instance by the prefix of its name (`bought_within[` finds
/// `bought_within[cooldown]`), when exactly one matches.
fn instance<'a>(a: &'a Analysis, prefix: &str) -> &'a RelationFacts {
    let found: Vec<&RelationFacts> = a.relations.iter().filter(|r| r.name.starts_with(prefix)).collect();
    assert_eq!(found.len(), 1, "{}: {:?}", prefix, found.iter().map(|r| &r.name).collect::<Vec<_>>());
    found[0]
}

/// Every corpus strategy and both books analyse without a panic, print,
/// and carry the structural labels.
#[test]
fn every_corpus_program_analyses() {
    for sub in ["strategies", "company"] {
        for f in common::dsl_files(sub) {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            let p = common::program(&fs::read_to_string(&f).unwrap(), &name);
            let nir = normalise(&p);
            let a = analyse(&p, &nir, None, -40);
            let text = a.text();
            assert!(text.contains("relations:") && text.contains("rules ("), "{}", name);
            let json = a.json();
            assert_eq!(json["stage"], "analysed");
            assert!(a.relation("decide").map(|r| r.demanded).unwrap_or(false), "{}: decide is demanded", name);
            // Every relation in a recursive component carries a keyedness label.
            for r in &a.relations {
                if r.recursive && r.kind == "derived" {
                    assert_ne!(r.keyedness, Keyedness::NotRequired, "{}: {} is recursive without a label", name, r.name);
                }
            }
        }
    }
}

/// Section 3.2, 3.3, 3.4 and 3.5 on the corpus: the open-loop partition,
/// keyedness by guard on the company book's carried relations, the demand
/// of the parametric library relations, window lengths.
#[test]
fn structural_facts_on_the_corpus() {
    let p = common::corpus_program("breakout_52w");
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    assert!(!a.relation("breakout").unwrap().closed_loop, "breakout reads market data only");
    assert!(a.relation("flat").unwrap().closed_loop, "flat reads position");
    // A bracketed input is substituted at link time (language-v2, section
    // 7): the demand of a parametric relation is the set of its instances,
    // one per call signature, each named by its arguments, in place of the
    // static demand tuples of the first formulation.
    let bought: Vec<&RelationFacts> = a.relations.iter().filter(|r| r.name.starts_with("bought_within[")).collect();
    let names: Vec<&str> = bought.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"bought_within[cooldown]") && names.contains(&"bought_within[hold]"), "{:?}", names);
    for r in &bought {
        assert!(r.closed_loop, "{} reads decided", r.name);
        assert!(r.demanded, "{} is reached by decide", r.name);
    }
    let highest = instance(&a, "max_prior_window[");
    assert_eq!(highest.name, "max_prior_window[lookback, lookback_min, close]");
    assert!(highest.demanded);
    assert!(!highest.closed_loop);
    let highest_rule = (0..p.rules.len()).find(|&i| p.rules[i].head.name == highest.name).expect("the rule of the instance");
    let w = a.windows.iter().find(|w| w.rule == highest_rule).expect("the window of the instance");
    assert_eq!(w.k, Some(200.0));
    assert_eq!(w.w_max, Some(366.0), "a year of daily bars at most");
    // The instances momentum_top_n reaches through its liquidity floor.
    let p = common::corpus_program("momentum_top_n");
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    assert!(a.relation("momentum").unwrap().demanded);
    assert!(instance(&a, "liquid[").demanded);
    assert!(instance(&a, "adv[").demanded);
    assert_eq!(instance(&a, "adv[").name, "adv[20]");
    // Keyedness on the weekly book: the latch is keyed by its complementary
    // guards (`enter` against `not enter`); the library's row recursions
    // step through `first_row` and `prev_row`, a pair complementary only
    // through `first_row`'s definition (`not prev_row`), which the
    // syntactic test does not unfold, so they are keyed at runtime.
    let src = common::corpus_source("company", "mw14");
    let p = common::program(&src, "mw14");
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    for (name, expected) in [("latch[enter, stay]", Keyedness::ByGuard), ("cum_split", Keyedness::AtRuntime), ("row_number", Keyedness::AtRuntime)] {
        let r = a.relation(name).unwrap_or_else(|| panic!("{} is a relation of mw14", name));
        assert!(r.recursive, "{} is temporally recursive", name);
        assert_eq!(r.keyedness, expected, "{}: {}", name, r.keyedness_note);
    }
    assert_eq!(a.relation("split_factor").unwrap().keyedness, Keyedness::NotRequired);
    // A010: the row counter has no finite bound.
    assert!(a.warnings.iter().any(|w| w.code == "A010" && w.message.contains("row_number.K")), "{:?}", codes(&a));
}

/// A daily strategy named `warn` with ten-share lots around the given rules
/// and decide rules.
fn with_rules(params: &str, rules: &str, decides: &str) -> String {
    let params = format!("    qty : Quantity<Shares> = 10 shares\n{}", params);
    common::strategy("warn", &params, rules, decides)
}

/// A001 to A005 and A008, A009 on crafted rules.
#[test]
fn the_warnings_of_the_abstract_interpretation() {
    // A001: a divisor that may be zero.
    let src = with_rules(
        "",
        r#"
    rel bad(A: Equity, @T, X: Scalar)
    bad(A, T, X) :- universe(A, T), close(A, T, P), X = P / (P - P).
"#,
        "    decide(T, trade(A, qty)) :- bad(A, T, X), X > 0, flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A001".to_string()), "{:?}", a.warnings);
    assert!(a.rules.iter().any(|r| r.label == "warn::bad#1" && r.may_undef));
    // A002 and A003.
    let src = with_rules(
        "",
        r#"
    rel bad(A: Equity, @T, X: Scalar, Y: Scalar)
    bad(A, T, X, Y) :- universe(A, T), log_price_return(A, T, R), X = log(R + 2), Y = sqrt(R).
"#,
        "    decide(T, trade(A, qty)) :- bad(A, T, X, Y), X > 0, flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    let c = codes(&a);
    assert!(c.contains(&"A002".to_string()) && c.contains(&"A003".to_string()), "{:?}", a.warnings);
    // A004 (never true) and A005 (always true, from the loader's positive prices).
    let src = with_rules(
        "",
        r#"
    rel never(A: Equity, @T)
    never(A, T) :- universe(A, T), close(A, T, P), P > 0 USD/share, P > 50 USD/share, P < 10 USD/share.
"#,
        "    decide(T, trade(A, qty)) :- never(A, T), flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    let c = codes(&a);
    assert!(c.contains(&"A004".to_string()), "{:?}", a.warnings);
    assert!(c.contains(&"A005".to_string()), "{:?}", a.warnings);
    assert!(a.never_true.iter().any(|n| !n.within_margin), "{:?}", a.never_true);
    assert!(a.warnings.iter().any(|w| w.code == "A004" && !w.note), "{:?}", a.warnings);
    let never = a.rules.iter().find(|r| r.label == "warn::never#1").unwrap();
    assert_eq!(never.literals[4].verdict.as_deref(), Some("never true"), "{:?}", never.literals);
    assert_eq!(never.literals[2].verdict.as_deref(), Some("always true"), "{:?}", never.literals);
    // Touching intervals are within the rounding margin (6.8): the verdict
    // stays a note and the literal is kept.
    let src = with_rules(
        "",
        r#"
    rel never(A: Equity, @T)
    never(A, T) :- universe(A, T), close(A, T, P), P < 0 USD/share.
"#,
        "    decide(T, trade(A, qty)) :- never(A, T), flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    assert!(a.never_true.iter().all(|n| n.within_margin), "{:?}", a.never_true);
    assert!(a.warnings.iter().any(|w| w.code == "A004" && w.note), "{:?}", a.warnings);
    // A006: a minimum no window can meet.
    let src = with_rules(
        "",
        r#"
    rel m(A: Equity, @T, M: Price<USD>)
    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window[20d] min 100, close(A, T1, P)).
"#,
        "    decide(T, trade(A, qty)) :- m(A, T, M), flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A006".to_string()), "{:?}", a.warnings);
    // A007: a window that cannot fill before the first decision bar.
    let src = with_rules(
        "",
        r#"
    rel m(A: Equity, @T, M: Price<USD>)
    m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows[10] min 10, close(A, T1, P)).
"#,
        "    decide(T, trade(A, qty)) :- m(A, T, M), flat(A, T).",
    );
    let p = common::program(&src, "warn");
    let nir = normalise(&p);
    let stats = Statistics {
        warmup_bars: Some(5.0),
        ..Statistics::default()
    };
    let a = analyse(&p, &nir, Some(&stats), -40);
    assert!(codes(&a).contains(&"A007".to_string()), "{:?}", a.warnings);
    assert!(a.bundle_relative);
    // A008: corr and ols_slope notes.
    let src = with_rules(
        "    bench : Equity = \"SPY\"",
        r#"
    rel b(A: Equity, @T, B: Scalar, C: Scalar)
    b(A, T, B, C) :- universe(A, T),
        B = ols_slope(R, R1) over (T1 in window[60d] min 20, log_price_return(A, T1, R), log_price_return(bench, T1, R1)),
        C = corr(R, R1) over (T1 in window[60d] min 20, log_price_return(A, T1, R), log_price_return(bench, T1, R1)).
"#,
        "    decide(T, trade(A, qty)) :- b(A, T, B, C), B > 1, flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    let notes: Vec<&absolute_backtest::analysis::Warning> = a.warnings.iter().filter(|w| w.code == "A008").collect();
    assert_eq!(notes.len(), 2, "{:?}", a.warnings);
    assert!(notes.iter().all(|w| w.note));
    // A009: a ranged parameter that makes a guard infeasible.
    let src = with_rules(
        "    delta : Scalar = 0.05 in 0.01..0.1",
        r#"
    rel g(A: Equity, @T)
    g(A, T) :- universe(A, T), delta < 0.
"#,
        "    decide(T, trade(A, qty)) :- g(A, T), flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A009".to_string()), "{:?}", a.warnings);
}

/// A delta is signed (language-v2, 4.4): the sale of a held quantity is a
/// negative delta bound in the body, and the abstract interpretation does
/// not warn on it.
#[test]
#[ignore = "src: analysis/ai.rs A011 still judges a delta by the first formulation (`trade(A, Q)` with `Q = -Q0` warns `may carry a non-positive quantity ... the kernel halts on it`); a v2 delta is signed and the kernel fills it"]
fn a_negative_delta_is_not_warned() {
    let src = with_rules(
        "",
        r#"
    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), price_return(A, T, R), R > 0.
"#,
        "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), price_return(A, T, R), R <= 0, Q = -Q0.",
    );
    let (_, a) = analysed(&src, "warn");
    assert!(!codes(&a).contains(&"A011".to_string()), "{:?}", a.warnings);
}

/// A008 is a warning, not a note, when an argument is a constant at every
/// row (a point interval): the aggregate is undefined at every bar and the
/// run halts at the first full window (the latent defect of the Oct 6
/// study's MW14 attempt, `experiments/llm-usability/review/REVIEW.md`).
#[test]
fn a_constant_regressor_is_a_definite_a008() {
    let src = with_rules(
        "",
        r#"
    rel b(A: Equity, @T, B: Scalar, C: Scalar)
    b(A, T, B, C) :- universe(A, T),
        B = ols_slope(R, I) over (T1 in window[60d] min 20, log_price_return(A, T1, R), I = 1),
        C = corr(R, J) over (T1 in window[60d] min 20, log_price_return(A, T1, R), J = 2.5).
"#,
        "    decide(T, trade(A, qty)) :- b(A, T, B, C), B > 1, flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    let a008: Vec<&absolute_backtest::analysis::Warning> = a.warnings.iter().filter(|w| w.code == "A008").collect();
    assert_eq!(a008.len(), 2, "{:?}", a.warnings);
    assert!(a008.iter().all(|w| !w.note), "definite, not a note: {:?}", a008);
    assert!(a008.iter().any(|w| w.message.contains("regressor `I` is the constant 1")), "{:?}", a008);
    assert!(a008.iter().any(|w| w.message.contains("`corr(R, J)` is the constant 2.5")), "{:?}", a008);
}

/// Comparisons refine the state (backward propagation), and the refined
/// intervals give the planner its selectivities.
#[test]
fn comparisons_refine_and_select() {
    let src = with_rules(
        "    floor : Price<USD> = 20 USD/share in 10 USD/share..30 USD/share",
        r#"
    rel rich(A: Equity, @T, P: Price<USD>)
    rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, P < 100 USD/share, X = P - floor, X > 0 USD/share.
"#,
        "    decide(T, trade(A, qty)) :- rich(A, T, _), flat(A, T).",
    );
    let (_, a) = analysed(&src, "warn");
    let rich = a.rules.iter().find(|r| r.label == "warn::rich#1").unwrap();
    // After `P > floor`, P is above 10; after `P < 100`, below 100.
    let p_after = |i: usize| rich.literals[i].state.iter().find(|(n, _)| n == "P").map(|(_, v)| v.interval).unwrap();
    assert!(p_after(2).lo >= 10.0, "{:?}", p_after(2));
    assert!(p_after(3).hi <= 100.0 && p_after(3).lo >= 10.0, "{:?}", p_after(3));
    // X = P - floor lies in (-20, 90]; `X > 0` refines it and reaches back into P.
    let x_after = rich.literals[5].state.iter().find(|(n, _)| n == "X").map(|(_, v)| v.interval).unwrap();
    assert!(x_after.lo >= 0.0, "{:?}", x_after);
    assert!(rich.literals[5].selectivity.is_some(), "{:?}", rich.literals[5]);
    assert!(rich.literals[3].selectivity.is_none(), "an unbounded side has no uniform selectivity");
    assert!(a.relation("rich").unwrap().columns[2].value.interval.lo >= 10.0);
}
