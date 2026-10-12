//! The Normalise stage (compiler design, sections 2.5 to 2.10): the
//! canonical form re-parses to itself (the round trip), and the hash is
//! invariant under renaming, reordering and the written form of derived
//! forms (Proposition 2.1) but not under a change of constant.

mod corpus;

use std::fs;
use std::path::Path;

use absolute_backtest::check::{check_program, Program, Severity, Workspace};
use absolute_backtest::compiler::normalise::{normalise, NirStage};
use absolute_backtest::compiler::Stage;

fn program(ws: &Workspace, name: &str) -> Program {
    let (p, diags) = check_program(ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn strategy_name(src: &str) -> String {
    let units = absolute_backtest::parse_units(src).unwrap();
    units.iter().find(|u| u.kind == absolute_backtest::UnitKind::Strategy).map(|u| u.name.clone()).expect("a strategy unit")
}

fn corpus_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let mut out = Vec::new();
    for sub in ["strategies", "company"] {
        let mut files: Vec<_> = fs::read_dir(root.join(sub))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().map(|e| e == "dsl").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            let src = fs::read_to_string(&f).unwrap();
            out.push((strategy_name(&src), src));
        }
    }
    out
}

/// The canonical rendering of a program parses, checks without errors, and
/// normalises to the same hash and the same core text.
#[test]
fn the_canonical_form_round_trips_on_the_corpus() {
    for (name, src) in corpus_sources() {
        let mut ws = corpus::base_workspace();
        ws.add_source(&src).unwrap();
        let prog = program(&ws, &name);
        let nir = normalise(&prog);
        assert_eq!(nir.hash.len(), 64, "{}", name);
        let stage = NirStage { nir: &nir, prog: &prog };
        let text = stage.text();
        // The rendering is a complete workspace: the environment, the
        // libraries and the strategy.
        let mut ws2 = Workspace::new();
        ws2.add_source(&text).unwrap_or_else(|e| panic!("{}: the canonical rendering does not parse: {}\n{}", name, e, text));
        let (p2, diags) = check_program(&ws2, &name);
        let errors: Vec<String> = diags.iter().filter(|d| d.severity == Severity::Error).map(|d| d.to_string()).collect();
        assert!(errors.is_empty(), "{}: the canonical rendering does not check:\n{}\n{}", name, errors.join("\n"), text);
        let p2 = p2.unwrap();
        let nir2 = normalise(&p2);
        assert_eq!(nir.core_text(), nir2.core_text(), "{}: the core text changed on re-parsing", name);
        assert_eq!(nir.hash, nir2.hash, "{}: the hash changed on re-parsing", name);
        // Every rule has a body in canonical order and a name map.
        assert_eq!(nir.rules.len(), prog.rules.len());
        for r in &nir.rules {
            assert!(r.id.contains("::"), "{}", r.id);
        }
        let json = stage.json();
        assert_eq!(json["hash"], nir.hash);
    }
}

const BASE: &str = r#"
strategy canon {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  param floor : Price<USD> = 20 USD/share
  rel rich(-A: Equity, @T: Timestamp, -P: Price<USD>)
  rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, logret(A, T, R), R > 0.0.
  rel entry(-A: Equity, @T: Timestamp)
  entry(A, T) :- rich(A, T, _), flat(A, T), not bought_within(A, T, 20d).
  decide(T, buy(A, qty)) :- entry(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}
"#;

fn hash_of(src: &str, name: &str) -> (String, String) {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let prog = program(&ws, name);
    let nir = normalise(&prog);
    let core = nir.core_text();
    (nir.hash, core)
}

/// Proposition 2.1: renaming variables, reordering literals and rules, and
/// renaming the strategy keep the hash; a constant does not.
#[test]
fn the_hash_is_invariant_under_renaming_and_reordering() {
    let (base, core) = hash_of(BASE, "canon");
    // Variables renamed.
    let renamed = BASE
        .replace(
            "rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, logret(A, T, R), R > 0.0.",
            "rich(X, S, Px) :- universe(X, S), close(X, S, Px), Px > floor, logret(X, S, Ret), Ret > 0.0.",
        )
        .replace(
            "decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).",
            "decide(Now, sell(Name, Qty)) :- held(Name, Now, Qty), prev(Now, Before), decided(Before, buy(Name, _)).",
        );
    assert_eq!(hash_of(&renamed, "canon"), (base.clone(), core.clone()), "renaming changed the hash");
    // Literals reordered (a void reordering: tests and generators move).
    let reordered = BASE
        .replace(
            "rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, logret(A, T, R), R > 0.0.",
            "rich(A, T, P) :- universe(A, T), logret(A, T, R), close(A, T, P), R > 0.0, P > floor.",
        )
        .replace(
            "entry(A, T) :- rich(A, T, _), flat(A, T), not bought_within(A, T, 20d).",
            "entry(A, T) :- rich(A, T, _), not bought_within(A, T, 20d), flat(A, T).",
        );
    assert_eq!(hash_of(&reordered, "canon"), (base.clone(), core.clone()), "reordering literals changed the hash");
    // Rules reordered.
    let rules_reordered = BASE.replace(
        "  decide(T, buy(A, qty)) :- entry(A, T).\n  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).",
        "  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).\n  decide(T, buy(A, qty)) :- entry(A, T).",
    );
    assert_eq!(hash_of(&rules_reordered, "canon").0, base, "reordering rules changed the hash");
    // The strategy renamed.
    let strategy_renamed = BASE.replace("strategy canon {", "strategy other_name {");
    assert_eq!(hash_of(&strategy_renamed, "other_name").0, base, "renaming the strategy changed the hash");
    // A constant changed.
    let changed = BASE.replace("20 USD/share", "25 USD/share");
    assert_ne!(hash_of(&changed, "canon").0, base, "a different constant kept the hash");
    let changed = BASE.replace("R > 0.0", "R > 0.01");
    assert_ne!(hash_of(&changed, "canon").0, base, "a different literal kept the hash");
}

/// The derived forms reduce to the core forms of section 2.6.
#[test]
fn derived_forms_reduce_to_core_forms() {
    let src = r#"
strategy forms {
  env equities_1d
  uses features
  resolution @1d
  mode target
  param n : Count = 3
  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), momentum(A, T, 6mo, 1mo, M).
  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).
  rel ranked(-A: Equity, @T: Timestamp, -K: Scalar)
  ranked(A, T, K) :- bar(T), rank(candidate(A, T, M), by (M desc, A asc), as K).
  rel last_sel(+A: Equity, @T: Timestamp)
  last_sel(A, T) :- universe(A, T), prev(T, T0), selected(A, _) asof T0.
  decide(T, target_weight(A, W)) :- selected(A, T), ranked(A, T, K), W = 1 / K.
}
"#;
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let prog = program(&ws, "forms");
    let nir = normalise(&prog);
    let core = nir.core_text();
    assert!(core.contains("count() over (candidate(") && core.contains("<lex"), "{}", core);
    assert!(core.contains("= (") && core.contains(" + 1)"), "rank is a count plus one:\n{}", core);
    assert!(core.contains("asof(selected, V0, V2)") || core.contains("asof(selected"), "{}", core);
    assert!(core.contains("prev(V1)"), "{}", core);
    // The surface rendering keeps the derived forms and re-parses.
    let text = NirStage { nir: &nir, prog: &prog }.text();
    assert!(text.contains("top(n, candidate(") && text.contains("rank(candidate(") && text.contains(" asof "), "{}", text);
    let mut ws2 = Workspace::new();
    ws2.add_source(&text).unwrap();
    let (p2, diags) = check_program(&ws2, "forms");
    assert!(diags.iter().all(|d| d.severity != Severity::Error), "{:?}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>());
    assert_eq!(normalise(&p2.unwrap()).hash, nir.hash);
}
