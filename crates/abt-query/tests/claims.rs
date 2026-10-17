//! Claims (Q-4.3, Q-5): Benjamini and Hochberg's adjustment on a hand
//! example and its monotonicity in the number of claims (Q-4.6); every
//! expectation's verdict, and `undecidable` with its reason; a claim's
//! parameters typed by its test's, an unknown one refused; the seed a
//! function of the plan and the claim; the input tables' column names.

use abt_query::plan::claims::{benjamini_hochberg, seed, typed, verdict, Verdict};
use abt_query::plan::inputs::{rule_column, trial_column};
use abt_query::plan::parse::claim;
use abt_query::tests::{get, Param, TestResult};

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-15)
}

#[test]
fn benjamini_hochberg_on_a_hand_example() {
    // Sorted: 0.005, 0.01, 0.03, 0.04 of m = 4: 0.02, 0.02, 0.04, 0.04
    // after the step-up minimum.
    let adj = benjamini_hochberg(&[Some(0.01), Some(0.04), Some(0.03), Some(0.005), None]);
    assert!(close(adj[0], 0.02) && close(adj[1], 0.04) && close(adj[2], 0.04) && close(adj[3], 0.02), "{:?}", adj);
    assert_eq!(adj[4], None, "a claim without a p-value is not counted");
    // Capped at 1, and a single p-value is its own.
    assert!(close(benjamini_hochberg(&[Some(0.9), Some(0.95)])[0], 0.95));
    assert!(close(benjamini_hochberg(&[Some(0.2)])[0], 0.2));
    // Never below the raw p-value.
    let raw = [0.001, 0.2, 0.03, 0.5, 0.04, 0.011];
    let adj = benjamini_hochberg(&raw.map(Some));
    assert!(raw.iter().zip(&adj).all(|(p, a)| a.unwrap() >= *p));
}

#[test]
fn the_adjusted_p_value_does_not_fall_as_claims_are_added() {
    // Q-4.6: a claim added never makes another's adjusted p-value smaller
    // when it is no smaller than the others (the step-up reads ranks).
    let mut ps = vec![Some(0.003), Some(0.02), Some(0.04)];
    let mut last = benjamini_hochberg(&ps);
    for extra in [0.05, 0.3, 0.7, 1.0] {
        ps.push(Some(extra));
        let now = benjamini_hochberg(&ps);
        for i in 0..last.len() {
            assert!(now[i].unwrap() >= last[i].unwrap(), "claim {} fell from {:?} to {:?}", i, last[i], now[i]);
        }
        last = now;
    }
}

fn result(statistic: f64, p: Option<f64>, effect: Option<f64>, interval: Option<(f64, f64, f64)>) -> TestResult {
    TestResult {
        statistic,
        p_value: p,
        effect,
        interval,
        ..Default::default()
    }
}

#[test]
fn every_expectation_has_its_verdict() {
    let c = |text: &str| claim(text).unwrap();
    let r = result(2.5, Some(0.01), Some(0.2), Some((0.1, 0.3, 0.95)));
    assert_eq!(verdict(&c("x : adf on returns of m expect reject"), &r, Some(0.01)), Verdict::Holds);
    assert_eq!(verdict(&c("x : adf on returns of m expect reject at 0.005"), &r, Some(0.01)), Verdict::Fails);
    assert_eq!(verdict(&c("x : adf on returns of m expect accept"), &r, Some(0.01)), Verdict::Fails);
    // The adjusted p-value decides, not the raw one.
    assert_eq!(verdict(&c("x : adf on returns of m expect reject"), &r, Some(0.2)), Verdict::Fails);
    // A comparison alone, or with its level.
    assert_eq!(verdict(&c("x : ic on returns of m expect statistic > 2"), &r, Some(0.2)), Verdict::Holds);
    assert_eq!(verdict(&c("x : ic on returns of m expect statistic > 2 at 0.05"), &r, Some(0.2)), Verdict::Fails);
    assert_eq!(verdict(&c("x : ic on returns of m expect effect < 0.3"), &r, None), Verdict::Holds);
    assert_eq!(verdict(&c("x : ic on returns of m expect interval excludes 0"), &r, None), Verdict::Holds);
    assert_eq!(verdict(&c("x : ic on returns of m expect interval excludes 0.2"), &r, None), Verdict::Fails);
    // What the result lacks makes the claim undecidable, with the reason.
    let bare = result(f64::NAN, None, None, None);
    for (text, why) in [
        ("x : adf on returns of m expect reject", "no p-value"),
        ("x : adf on returns of m expect statistic > 0", "not finite"),
        ("x : adf on returns of m expect effect > 0", "no effect"),
        ("x : adf on returns of m expect interval excludes 0", "no interval"),
    ] {
        match verdict(&c(text), &bare, None) {
            Verdict::Undecidable(r) => assert!(r.contains(why), "{}: {}", text, r),
            v => panic!("{}: {:?}", text, v),
        }
    }
    // The level, when not written, is none.
    assert_eq!(c("x : adf on returns of m expect reject").level, None);
    assert_eq!(c("x : adf on returns of m expect reject at 0.01").level, Some(0.01));
}

#[test]
fn parameters_are_typed_by_the_test_and_the_seed_is_the_plans() {
    let ljung = get("ljung_box").unwrap();
    let p = typed(ljung.as_ref(), &vec![("lags".to_string(), "5".to_string())]).unwrap();
    assert_eq!(p.get("lags"), Some(&Param::Int(5)));
    assert!(typed(ljung.as_ref(), &vec![("lags".to_string(), "five".to_string())]).is_err_and(|e| e.contains("not an integer")));
    assert!(typed(ljung.as_ref(), &vec![("nowhere".to_string(), "1".to_string())]).is_err_and(|e| e.contains("not a parameter")));
    let co = get("coactivation").unwrap();
    let p = typed(co.as_ref(), &vec![("rules".to_string(), "(decide_1, decide_2)".to_string())]).unwrap();
    assert_eq!(p.get("rules"), Some(&Param::Text("(decide_1, decide_2)".into())));
    assert_eq!(seed("h", "c0"), seed("h", "c0"));
    assert_ne!(seed("h", "c0"), seed("h", "c1"));
    assert_ne!(seed("h", "c0"), seed("g", "c0"));
}

#[test]
fn the_input_tables_name_their_columns() {
    assert_eq!(rule_column("momentum::decide#2", "momentum"), "decide_2");
    assert_eq!(rule_column("book::rebalance#1", "momentum"), "book__rebalance_1");
    // Trial columns sort in the grid's order.
    let names: Vec<String> = (0..12).map(|i| trial_column(i, 12)).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(trial_column(3, 4), "trial3");
}
