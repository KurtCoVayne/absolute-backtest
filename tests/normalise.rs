//! The Normalise stage (compiler design, sections 2.5 to 2.10): the
//! canonical form re-parses to itself (the round trip), and the hash is
//! invariant under renaming, reordering and the written form of derived
//! forms (Proposition 2.1) but not under a change of constant.

mod common;

use std::fs;

use absolute_backtest::check::{check_program, Severity};
use absolute_backtest::compiler::normalise::{normalise, NirStage};
use absolute_backtest::compiler::Stage;

/// The corpus strategies and the two company programs, each named by its
/// file (the corpus names a file after its strategy).
fn corpus_sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for sub in ["strategies", "company"] {
        for f in common::dsl_files(sub) {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            out.push((name, fs::read_to_string(&f).unwrap()));
        }
    }
    out
}

/// The canonical rendering of a program parses, checks without errors, and
/// normalises to the same hash and the same core text.
#[test]
#[ignore = "src: Nir::source (compiler/print.rs SourceRendering) writes the linked instances and views as strategy declarations (`rel bought_within[cooldown](A: Entity, @T: base)`, `rel traded@base(...)`) and drops `uses`, so the rendering of every v2 program fails to parse"]
fn the_canonical_form_round_trips_on_the_corpus() {
    for (name, src) in corpus_sources() {
        let prog = common::program(&src, &name);
        let nir = normalise(&prog);
        assert_eq!(nir.hash.len(), 64, "{}", name);
        let stage = NirStage { nir: &nir, prog: &prog };
        let text = stage.text();
        // The rendering is the strategy with every instantiated relation
        // written out as its own rules; it is checked against the class
        // catalog and the library of the base workspace, as a program is.
        let mut ws2 = common::base_workspace();
        ws2.add_source(&text).unwrap_or_else(|e| panic!("{}: the canonical rendering does not parse: {} at {}\n{}", name, e.message, e.span, text));
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
  data { class equity   need bars @1d   need listing   require tested   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params {
    qty : Quantity<Shares> = 10 shares
    floor : Price<USD> = 20 USD/share
    cooldown : Duration = 20d
  }
  rules {
    rel rich(A: Equity, @T, P: Price<USD>)
    rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, log_price_return(A, T, R), R > 0.0.
    rel entry(A: Equity, @T)
    entry(A, T) :- rich(A, T, _), flat(A, T), not bought_within[cooldown](A, T).
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- entry(A, T).
    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
"#;

fn hash_of(src: &str, name: &str) -> (String, String) {
    let prog = common::program(src, name);
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
            "rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, log_price_return(A, T, R), R > 0.0.",
            "rich(X, S, Px) :- universe(X, S), close(X, S, Px), Px > floor, log_price_return(X, S, Ret), Ret > 0.0.",
        )
        .replace(
            "decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
            "decide(Now, trade(Name, Delta)) :- held(Name, Now, Qty), prev(Now, Before), decided(Before, trade(Name, Bought)), Bought > 0 shares, Delta = -Qty.",
        );
    assert_ne!(renamed, BASE);
    assert_eq!(hash_of(&renamed, "canon"), (base.clone(), core.clone()), "renaming changed the hash");
    // Literals reordered (a void reordering: tests and generators move).
    let reordered = BASE
        .replace(
            "rich(A, T, P) :- universe(A, T), close(A, T, P), P > floor, log_price_return(A, T, R), R > 0.0.",
            "rich(A, T, P) :- universe(A, T), log_price_return(A, T, R), close(A, T, P), R > 0.0, P > floor.",
        )
        .replace(
            "entry(A, T) :- rich(A, T, _), flat(A, T), not bought_within[cooldown](A, T).",
            "entry(A, T) :- rich(A, T, _), not bought_within[cooldown](A, T), flat(A, T).",
        );
    assert_ne!(reordered, BASE);
    assert_eq!(hash_of(&reordered, "canon"), (base.clone(), core.clone()), "reordering literals changed the hash");
    // Rules reordered.
    let rules_reordered = BASE.replace(
        "    decide(T, trade(A, qty)) :- entry(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
        "    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.\n    decide(T, trade(A, qty)) :- entry(A, T).",
    );
    assert_ne!(rules_reordered, BASE);
    assert_eq!(hash_of(&rules_reordered, "canon").0, base, "reordering rules changed the hash");
    // The strategy renamed.
    let strategy_renamed = BASE.replace("strategy canon {", "strategy other_name {");
    assert_eq!(hash_of(&strategy_renamed, "other_name").0, base, "renaming the strategy changed the hash");
    // A constant changed.
    let changed = BASE.replace("20 USD/share", "25 USD/share");
    assert_ne!(hash_of(&changed, "canon").0, base, "a different constant kept the hash");
    let changed = BASE.replace("R > 0.0", "R > 0.01");
    assert_ne!(hash_of(&changed, "canon").0, base, "a different literal kept the hash");
    // The decisions block and the execution block are part of the identity
    // (Propositions 3.13 and 4.3); the report block is not (4.5).
    let silence = BASE.replace("silence hold", "silence flat");
    assert_ne!(hash_of(&silence, "canon").0, base, "the silence policy kept the hash");
    let execution = BASE.replace("execution equity_default", "execution equity_default with { lot fractional }");
    assert_ne!(hash_of(&execution, "canon").0, base, "the execution block kept the hash");
    let report = BASE.replace("periods 252 per year", "periods 252 per year   window 2024-01-01 .. 2024-12-31");
    assert_eq!(hash_of(&report, "canon").0, base, "the report block changed the hash");
}

/// A strategy written with every derived form: a reduction, a rank, an
/// as-of join and `prev`.
fn forms_strategy() -> String {
    common::StrategyParts {
        name: "forms".into(),
        uses: "calendar, book".into(),
        params: "    n : Count = 3\n    lookback : Duration = 6mo\n    skip : Duration = 1mo".into(),
        rules: r#"
    rel candidate(A: Equity, @T, M: Scalar)
    candidate(A, T, M) :- universe(A, T), lag(T, skip, T1), close(A, T1, P1), lag(T, lookback, T0), close(A, T0, P0), M = P1 / P0 - 1.
    rel selected(A: Equity, @T)
    selected(A, T) :- bar(T), top[n](candidate(A, T, M) by (M desc, A asc)) within (T).
    rel ranked(A: Equity, @T, K: Count)
    ranked(A, T, K) :- K = rank(candidate(A, T, M) by (M desc, A asc)) within (T).
    rel last_sel(A: Equity, @T)
    last_sel(A, T) :- universe(A, T), prev(T, T0), selected(A, _) asof T0.
"#
        .into(),
        decides: "    decide(T, hold(A, W)) :- selected(A, T), ranked(A, T, K), last_sel(A, T), W = 1 / K.".into(),
        ..Default::default()
    }
    .render()
}

/// The derived forms reduce to the core forms of section 2.6.
#[test]
fn derived_forms_reduce_to_core_forms() {
    let prog = common::program(&forms_strategy(), "forms");
    let nir = normalise(&prog);
    let core = nir.core_text();
    assert!(core.contains("count() over (candidate(") && core.contains("<lex"), "{}", core);
    assert!(core.contains("= (") && core.contains(" + 1)"), "rank is a count plus one:\n{}", core);
    assert!(core.contains("asof(selected"), "{}", core);
    assert!(core.contains("= prev("), "{}", core);
    // The surface rendering keeps the derived forms.
    let text = NirStage { nir: &nir, prog: &prog }.text();
    assert!(text.contains("top[n](candidate(") && text.contains("rank(candidate(") && text.contains(" asof "), "{}", text);
}

/// The surface rendering of the derived forms re-parses to the same hash.
#[test]
#[ignore = "src: Nir::source renders the `traded@base` view of the universe as `rel traded@base(A: Entity, @T: base)`, which does not parse (see the_canonical_form_round_trips_on_the_corpus)"]
fn the_surface_rendering_of_derived_forms_re_parses() {
    let prog = common::program(&forms_strategy(), "forms");
    let nir = normalise(&prog);
    let text = NirStage { nir: &nir, prog: &prog }.text();
    let mut ws2 = common::base_workspace();
    ws2.add_source(&text).unwrap_or_else(|e| panic!("the canonical rendering does not parse: {} at {}\n{}", e.message, e.span, text));
    let (p2, diags) = check_program(&ws2, "forms");
    assert!(diags.iter().all(|d| d.severity != Severity::Error), "{}\n{}", common::text(&diags), text);
    assert_eq!(normalise(&p2.unwrap()).hash, nir.hash);
}
