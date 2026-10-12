//! The Analyse stage (compiler design, section 3): the structural facts
//! (components, the open-loop partition, keyedness labels, demand sets,
//! window lengths) and the abstract interpretation with its warnings A001
//! to A011, asserted on programs written to trigger each code, and run over
//! every corpus strategy.

mod corpus;

use std::fs;
use std::path::Path;

use absolute_backtest::analysis::{analyse, Analysis, DemandSet, Keyedness};
use absolute_backtest::check::{check_program, Program};
use absolute_backtest::compiler::normalise;
use absolute_backtest::compiler::Stage;
use absolute_backtest::plan::cost::Statistics;

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn analysed(src: &str, name: &str) -> (Program, Analysis) {
    let p = program(src, name);
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    (p, a)
}

fn codes(a: &Analysis) -> Vec<String> {
    a.warnings.iter().map(|w| w.code.clone()).collect()
}

fn corpus_program(sub: &str, name: &str) -> Program {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/{}/{}.dsl", sub, name))).unwrap();
    program(&src, name)
}

/// Every corpus strategy and both books analyse without a panic, print,
/// and carry the structural labels.
#[test]
fn every_corpus_program_analyses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    for sub in ["strategies", "company"] {
        let mut files: Vec<_> = fs::read_dir(root.join(sub)).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        for f in files {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            let p = corpus_program(sub, &name);
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
/// keyedness by guard on the company book's carried relations, static
/// demand sets, window lengths.
#[test]
fn structural_facts_on_the_corpus() {
    let p = corpus_program("strategies", "breakout_52w");
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    assert!(!a.relation("breakout").unwrap().closed_loop, "breakout reads market data only");
    assert!(a.relation("flat").unwrap().closed_loop, "flat reads position");
    assert!(a.relation("bought_within").unwrap().closed_loop, "bought_within reads decided");
    assert_eq!(a.relation("highest").unwrap().demand, DemandSet::Static(vec![vec!["1y".into(), "200".into()]]));
    match &a.relation("bought_within").unwrap().demand {
        DemandSet::Static(ts) => {
            assert!(ts.contains(&vec!["60d".to_string()]) && ts.contains(&vec!["20d".to_string()]), "{:?}", ts);
        }
        other => panic!("{:?}", other),
    }
    assert!(!a.relation("sma").unwrap().demanded, "sma is not reached by decide");
    assert!(a.relation("highest").unwrap().demanded);
    let highest = (0..p.rules.len()).find(|&i| p.rule_label(i) == "features::highest#1").unwrap();
    let w = a.windows.iter().find(|w| w.rule == highest).expect("the window of highest");
    assert_eq!(w.k, Some(200.0));
    assert_eq!(w.w_max, Some(366.0), "a year of daily bars at most");
    // The transitive demand of momentum_top_n.
    let p = corpus_program("strategies", "momentum_top_n");
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    assert_eq!(a.relation("momentum").unwrap().demand, DemandSet::Static(vec![vec!["1y".into(), "1mo".into()]]));
    assert_eq!(a.relation("adv").unwrap().demand, DemandSet::Static(vec![vec!["20d".into(), "10".into()]]));
    // Keyedness on the weekly book: the carried relations are keyed by
    // their complementary guards.
    let p = corpus_program("company", "mw14");
    let nir = normalise(&p);
    let a = analyse(&p, &nir, None, -40);
    for name in ["cumf", "row", "lp"] {
        let r = a.relation(name).unwrap();
        assert!(r.recursive, "{} is temporally recursive", name);
        assert_eq!(r.keyedness, Keyedness::ByGuard, "{}: {}", name, r.keyedness_note);
    }
    assert_eq!(a.relation("split1").unwrap().keyedness, Keyedness::NotRequired);
    // A010: the row counter has no finite bound.
    assert!(a.warnings.iter().any(|w| w.code == "A010" && w.message.contains("row.K")), "{:?}", codes(&a));
}

const HEAD: &str = r#"
strategy warn {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
"#;

fn with_rules(rules: &str) -> String {
    format!("{}{}\n}}\n", HEAD, rules)
}

/// A001 to A005 and A008, A009, A011 on crafted rules.
#[test]
fn the_warnings_of_the_abstract_interpretation() {
    // A001: a divisor that may be zero.
    let src = with_rules(
        r#"
  rel bad(-A: Equity, @T: Timestamp, -X: Scalar)
  bad(A, T, X) :- universe(A, T), close(A, T, P), X = P / (P - P).
  decide(T, buy(A, qty)) :- bad(A, T, X), X > 0, flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A001".to_string()), "{:?}", a.warnings);
    assert!(a.rules.iter().any(|r| r.label == "warn::bad#1" && r.may_undef));
    // A002 and A003.
    let src = with_rules(
        r#"
  rel bad(-A: Equity, @T: Timestamp, -X: Scalar, -Y: Scalar)
  bad(A, T, X, Y) :- universe(A, T), logret(A, T, R), X = log(R + 2), Y = sqrt(R).
  decide(T, buy(A, qty)) :- bad(A, T, X, Y), X > 0, flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    let c = codes(&a);
    assert!(c.contains(&"A002".to_string()) && c.contains(&"A003".to_string()), "{:?}", a.warnings);
    // A004 (never true) and A005 (always true, from the loader's positive prices).
    let src = with_rules(
        r#"
  rel never(-A: Equity, @T: Timestamp)
  never(A, T) :- universe(A, T), close(A, T, P), P > 0 USD/share, P > 50 USD/share, P < 10 USD/share.
  decide(T, buy(A, qty)) :- never(A, T), flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    let c = codes(&a);
    assert!(c.contains(&"A004".to_string()), "{:?}", a.warnings);
    assert!(c.contains(&"A005".to_string()), "{:?}", a.warnings);
    assert!(a.never_true.iter().any(|n| !n.within_margin), "{:?}", a.never_true);
    assert!(a.warnings.iter().any(|w| w.code == "A004" && !w.note), "{:?}", a.warnings);
    let never = a.rules.iter().find(|r| r.label == "warn::never#1").unwrap();
    assert_eq!(never.literals[4].verdict.as_deref(), Some("never true"));
    assert_eq!(never.literals[2].verdict.as_deref(), Some("always true"));
    // Touching intervals are within the rounding margin (6.8): the verdict
    // stays a note and the literal is kept.
    let src = with_rules(
        r#"
  rel never(-A: Equity, @T: Timestamp)
  never(A, T) :- universe(A, T), close(A, T, P), P < 0 USD/share.
  decide(T, buy(A, qty)) :- never(A, T), flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    assert!(a.never_true.iter().all(|n| n.within_margin), "{:?}", a.never_true);
    assert!(a.warnings.iter().any(|w| w.code == "A004" && w.note), "{:?}", a.warnings);
    // A006: a minimum no window can meet.
    let src = with_rules(
        r#"
  rel m(-A: Equity, @T: Timestamp, -M: Price<USD>)
  m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in window(T, 20d, min 100), close(A, T1, P)).
  decide(T, buy(A, qty)) :- m(A, T, M), flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A006".to_string()), "{:?}", a.warnings);
    // A007: a window that cannot fill before the first decision bar.
    let src = with_rules(
        r#"
  rel m(-A: Equity, @T: Timestamp, -M: Price<USD>)
  m(A, T, M) :- universe(A, T), M = mean(P) over (T1 in rows(T, 10, min 10), close(A, T1, P)).
  decide(T, buy(A, qty)) :- m(A, T, M), flat(A, T).
"#,
    );
    let p = program(&src, "warn");
    let nir = normalise(&p);
    let stats = Statistics {
        warmup_bars: Some(5.0),
        ..Statistics::default()
    };
    let a = analyse(&p, &nir, Some(&stats), -40);
    assert!(codes(&a).contains(&"A007".to_string()), "{:?}", a.warnings);
    assert!(a.bundle_relative);
    // A008: corr and ols_beta notes.
    let src = with_rules(
        r#"
  rel b(-A: Equity, @T: Timestamp, -B: Scalar, -C: Scalar)
  b(A, T, B, C) :- universe(A, T),
      B = ols_beta(R, R1) over (T1 in window(T, 60d, min 20), logret(A, T1, R), logret("SPY", T1, R1)),
      C = corr(R, R1) over (T1 in window(T, 60d, min 20), logret(A, T1, R), logret("SPY", T1, R1)).
  decide(T, buy(A, qty)) :- b(A, T, B, C), B > 1, flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    let notes: Vec<&absolute_backtest::analysis::Warning> = a.warnings.iter().filter(|w| w.code == "A008").collect();
    assert_eq!(notes.len(), 2, "{:?}", a.warnings);
    assert!(notes.iter().all(|w| w.note));
    // A009: a ranged parameter that makes a guard infeasible.
    let src = with_rules(
        r#"
  param delta : Scalar = 0.05 in 0.01..0.1
  rel g(-A: Equity, @T: Timestamp)
  g(A, T) :- universe(A, T), delta < 0.
  decide(T, buy(A, qty)) :- g(A, T), flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A009".to_string()), "{:?}", a.warnings);
    // A011: a delta quantity that may not be positive.
    let src = with_rules(
        r#"
  decide(T, sell(A, Q)) :- held(A, T, Q0), Q = Q0 - 200 shares.
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
"#,
    );
    let (_, a) = analysed(&src, "warn");
    assert!(codes(&a).contains(&"A011".to_string()), "{:?}", a.warnings);
}

/// Comparisons refine the state (backward propagation), and the refined
/// intervals give the planner its selectivities.
#[test]
fn comparisons_refine_and_select() {
    let src = with_rules(
        r#"
  param floor : Price<USD> = 20 USD/share in 10 USD/share..30 USD/share
  rel rich(-A: Equity, @T: Timestamp, -P: Price<USD>)
  rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, P < 100 USD/share, X = P - floor, X > 0 USD/share.
  decide(T, buy(A, qty)) :- rich(A, T, _), flat(A, T).
"#,
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
