//! The observability prototype (`docs/language-v2.md`, section 8, on the v1
//! kernel): the program graph, the coverage report with its never-fired root
//! causes, the bindings an explanation reports, and the ledger query.

mod corpus;

use absolute_backtest::check::{check_program, Program, Workspace};
use absolute_backtest::data::synthetic_daily;
use absolute_backtest::kernel::time::parse_timestamp;
use absolute_backtest::kernel::{ExecConfig, Kernel, Value};
use absolute_backtest::observe::{coverage_report, program_graph, show_program};

fn program(extra: &str, name: &str) -> (Program, Workspace) {
    let mut ws = corpus::base_workspace();
    ws.add_source(extra).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    (p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n"))), ws)
}

const UP_DOWN: &str = r#"
strategy up_down {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  rel up(-A: Equity, @T: Timestamp)
  up(A, T) :- universe(A, T), logret(A, T, R), R > 0.
  decide(T, buy(A, qty)) :- up(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), prev(T, T0), decided(T0, buy(A, _)).
}
"#;

/// A strategy whose entry can never hold: the price floor is above every close.
const NEVER: &str = r#"
strategy never {
  env equities_1d
  uses features
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  param floor : Price<USD> = 100000 USD/share
  rel rich(-A: Equity, @T: Timestamp)
  rich(A, T) :- universe(A, T), close(A, T, P), P > floor.
  rel go(-A: Equity, @T: Timestamp)
  go(A, T) :- rich(A, T), flat(A, T).
  decide(T, buy(A, qty)) :- go(A, T).
}
"#;

/// The graph reaches every relation decide depends on, from decide upward,
/// with the executor views in the closed loop and the features outside it.
#[test]
fn program_graph_reaches_and_annotates() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let g = program_graph(&prog);
    assert_eq!(g.nodes[0].name, "decide");
    for name in ["up", "flat", "held", "logret", "position", "decided", "universe", "close"] {
        assert!(g.node(name).is_some(), "graph lacks {}", name);
    }
    assert!(g.node("sma").is_none(), "sma is a library relation decide never reaches");
    assert!(g.node("decide").unwrap().closed_loop);
    assert!(g.node("flat").unwrap().closed_loop, "flat reads position");
    assert!(!g.node("up").unwrap().closed_loop, "up reads market data only");
    assert_eq!(g.node("close").unwrap().depth, 0);
    assert_eq!(g.node("logret").unwrap().depth, 1);
    assert_eq!(g.node("up").unwrap().depth, 2);
    assert_eq!(g.depth, 3);
    let flat = g.node("flat").unwrap();
    assert!(flat.reads.contains(&("position".to_string(), '-')), "{:?}", flat.reads);
    let text = show_program(&prog);
    assert!(text.contains("decide(@T: Timestamp, -D: Decision)"), "{}", text);
    assert!(text.contains("up_down::decide#1: decide(T, buy(A, qty)) :- up(A, T), flat(A, T)."), "{}", text);
    assert!(text.contains("closed loop"), "{}", text);
}

/// A run counts what every relation derived; a strategy that decides
/// nothing gets the first empty relation on the path named, not the
/// relations behind it that were never demanded.
#[test]
fn coverage_names_the_first_empty_relation() {
    let (prog, _) = program(NEVER, "never");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    let by_name = |n: &str| result.coverage.iter().find(|c| c.name == n).cloned().unwrap();
    assert_eq!(by_name("decide").tuples, 0);
    assert_eq!(by_name("decide").calls, 60);
    assert_eq!(by_name("rich").tuples, 0);
    assert!(by_name("rich").calls > 0);
    // `flat` sits after `rich` in go's body: never demanded once rich is empty.
    assert_eq!(by_name("flat").calls, 0);
    // The primitives report what the data holds.
    assert_eq!(by_name("close").tuples, 120);
    assert!(!by_name("close").derived);
    let (text, causes) = coverage_report(&prog, &result.coverage, result.bars.len());
    assert_eq!(causes.len(), 1, "{}", text);
    assert_eq!(causes[0].relation, "rich");
    assert_eq!(causes[0].path, vec!["decide".to_string(), "go".to_string(), "rich".to_string()]);
    assert!(text.contains("never fired: decide derived no tuple over 60 bars"), "{}", text);
    assert!(text.contains("`rich` is the first empty relation"), "{}", text);
    assert!(text.contains("close+ (120 tuples)"), "{}", text);
    // The rule's explanation at a bar names the comparison.
    let rich = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "never::rich#1").unwrap();
    let ex = k.explain(rich, result.bars[30], &[]).unwrap();
    assert_eq!(ex.failed_at.as_ref().map(|(i, l)| (*i, l.clone())), Some((2, "P > floor".to_string())));
}

/// A strategy that trades has no root cause, and a run with decisions still
/// reports the relations demanded and never derived.
#[test]
fn coverage_of_a_trading_strategy() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    assert!(!result.decisions.is_empty());
    let (text, causes) = coverage_report(&prog, &result.coverage, result.bars.len());
    assert!(causes.is_empty(), "{}", text);
    assert!(!text.contains("never fired"), "{}", text);
    let up = result.coverage.iter().find(|c| c.name == "up").unwrap();
    assert!(up.tuples > 0 && up.nonempty <= up.calls && up.calls == 60, "{:?}", up);
}

/// An explanation of a rule that fired carries its first solution's bindings,
/// every variable by name.
#[test]
fn explanation_reports_the_bindings() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    let up = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "up_down::up#1").unwrap();
    // Find a bar where `up` fired, then read the bindings.
    let t = result.bars.iter().copied().find(|&t| !k.query("up", t, &[]).unwrap().is_empty()).unwrap();
    let ex = k.explain(up, t, &[]).unwrap();
    assert!(ex.failed_at.is_none());
    let names: Vec<&str> = ex.solution.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["A", "R", "T"], "{}", ex);
    assert!(ex.to_string().contains("first solution: A = "), "{}", ex);
    // Bound to one instrument the solution is that instrument's.
    let a = k.symbols.get("AAA").unwrap();
    if let Ok(for_a) = k.explain_with(up, t, &[], &[("A".to_string(), Value::Equity(a))]) {
        if for_a.failed_at.is_none() {
            assert_eq!(for_a.solution[0], ("A".to_string(), "AAA".to_string()));
        }
    }
}

/// The ledger of a relation is its query at every bar; the bars are the
/// relation's own domain and a bar off the domain is refused by name.
#[test]
fn query_every_bar_is_the_ledger() {
    let (prog, _) = program(UP_DOWN, "up_down");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let mut k = Kernel::new(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let result = k.run().unwrap();
    let bars = k.bars_at(prog.resolution);
    assert_eq!(bars, result.bars);
    let total: usize = bars.iter().map(|&t| k.query("up", t, &[]).unwrap().len()).sum();
    let cov = result.coverage.iter().find(|c| c.name == "up").unwrap();
    assert_eq!(total, cov.tuples, "the ledger holds what the run derived");
    let saturday = parse_timestamp("2022-01-08").unwrap();
    let err = k.check_bar(prog.resolution, saturday).unwrap_err().to_string();
    assert!(err.contains("2022-01-08 is not a bar at @1d"), "{}", err);
}
