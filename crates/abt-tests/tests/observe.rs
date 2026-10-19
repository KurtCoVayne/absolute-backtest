//! The observability prototype (`docs/language-v2.md`, section 8): the
//! program graph, the coverage report with its never-fired root causes, the
//! bindings an explanation reports, and the ledger query; observation
//! changes no decision (Proposition V-8.4) on every corpus program and both
//! books.

use abt_tests as common;

use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::{ExecConfig, Value};
use abt_kernel::observe::{coverage_report, program_graph, show_program};

/// Buy 10 on an up bar when flat, sell the bar after the buy.
fn up_down() -> String {
    common::strategy(
        "up_down",
        "    qty : Quantity<Shares> = 10 shares",
        "    rel up(A: Equity, @T)\n    up(A, T) :- universe(A, T), log_price_return(A, T, R), R > 0.",
        "    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).\n    decide(T, trade(A, Q)) :- held(A, T, Q0), prev(T, T0), decided(T0, trade(A, Q1)), Q1 > 0 shares, Q = -Q0.",
    )
}

/// A strategy whose entry can never hold: the price floor is above every close.
fn never() -> String {
    common::strategy(
        "never",
        "    qty : Quantity<Shares> = 10 shares\n    floor : Price<USD> = 100000 USD/share",
        "    rel rich(A: Equity, @T)\n    rich(A, T) :- universe(A, T), close(A, T, P), P > floor.\n    rel go(A: Equity, @T)\n    go(A, T) :- rich(A, T), flat(A, T).",
        "    decide(T, trade(A, qty)) :- go(A, T).",
    )
}

/// The graph reaches every relation decide depends on, from decide upward,
/// with the executor views in the closed loop and the bar features outside it.
#[test]
fn program_graph_reaches_and_annotates() {
    let prog = common::program(&up_down(), "up_down");
    let g = program_graph(&prog);
    assert_eq!(g.nodes[0].name, "decide");
    for name in ["up", "flat", "held", "log_price_return", "price_return", "position", "decided", "universe", "close"] {
        assert!(g.node(name).is_some(), "graph lacks {}", name);
    }
    // A library relation decide never reaches is not even instantiated
    // (`uses book` brings in `drawdown` only when a rule reads it), and a
    // primitive of the demand decide never reads is not in the graph.
    assert!(!prog.relations.contains_key("drawdown"), "{:?}", prog.relations.keys().collect::<Vec<_>>());
    assert!(g.node("drawdown").is_none(), "drawdown is a library relation decide never reaches");
    assert!(prog.relations.contains_key("volume") && g.node("volume").is_none(), "volume is a primitive decide never reaches");
    assert!(g.node("decide").unwrap().closed_loop);
    assert!(g.node("flat").unwrap().closed_loop, "flat reads position");
    assert!(!g.node("up").unwrap().closed_loop, "up reads market data only");
    // Depth is the longest path from a primitive, along the chain close ->
    // price_return -> log_price_return -> up -> decide (the return is a
    // library rule over the entity's previous row, not a builtin).
    let depth = |n: &str| g.node(n).unwrap().depth;
    assert_eq!(depth("close"), 0);
    assert!(depth("price_return") >= 1);
    assert_eq!(depth("log_price_return"), depth("price_return") + 1);
    assert_eq!(depth("up"), depth("log_price_return") + 1);
    assert_eq!(g.depth, depth("decide"));
    assert!(depth("decide") > depth("up"));
    let flat = g.node("flat").unwrap();
    assert!(flat.reads.contains(&("position".to_string(), '-')), "{:?}", flat.reads);
    let text = show_program(&prog);
    // A signature carries no binding modes (language-v2, 4.2).
    assert!(text.contains("decide(@T: Timestamp, D: Decision)"), "{}", text);
    // The execution block is printed expanded, the preset's defaults
    // included, and the report block after it (4.3, 4.5).
    assert!(
        text.contains("execution {") && text.contains("leverage      gross <= 1 halt") && text.contains("commission    per_share 0.005 USD min 1 USD"),
        "{}",
        text
    );
    assert!(text.contains("report { calendar base   periods 252 per year }"), "{}", text);
    // A decision prints with its order, the block's `market` when omitted.
    assert!(text.contains("up_down::decide#1: decide(T, trade(A, qty, market)) :- up(A, T), flat(A, T)."), "{}", text);
    assert!(text.contains("closed loop"), "{}", text);
}

/// A run counts what every relation derived; a strategy that decides
/// nothing gets the first empty relation on the path named, not the
/// relations behind it that were never demanded.
#[test]
fn coverage_names_the_first_empty_relation() {
    let prog = common::program(&never(), "never");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let (result, mut k) = abt_tests::run_kernel(&prog, &ds, ExecConfig::frictionless()).unwrap();
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
    let prog = common::program(&up_down(), "up_down");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let result = abt_tests::run(&prog, &ds, ExecConfig::frictionless()).unwrap();
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
    let prog = common::program(&up_down(), "up_down");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let (result, mut k) = abt_tests::run_kernel(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let up = (0..prog.rules.len()).find(|&i| prog.rule_label(i) == "up_down::up#1").unwrap();
    // Find a bar where `up` fired, then read the bindings.
    let t = result.bars.iter().copied().find(|&t| !k.query("up", t, &[]).unwrap().is_empty()).unwrap();
    let ex = k.explain(up, t, &[]).unwrap();
    assert!(ex.failed_at.is_none());
    let names: Vec<&str> = ex.solution.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["A", "R", "T"], "{}", ex);
    assert!(ex.to_string().contains("first solution: A = "), "{}", ex);
    // Bound to one instrument the solution is that instrument's: the
    // synthetic market's securities are `E1`, `E2`, ... carrying the tickers.
    let a = k.symbols.get("E1").expect("the security carrying AAA");
    if let Ok(for_a) = k.explain_with(up, t, &[], &[("A".to_string(), Value::Equity(a))]) {
        if for_a.failed_at.is_none() {
            assert_eq!(for_a.solution[0], ("A".to_string(), "E1".to_string()));
        }
    }
}

/// The ledger of a relation is its query at every bar; the bars are the
/// relation's own domain and a bar off the domain is refused by name.
#[test]
fn query_every_bar_is_the_ledger() {
    let prog = common::program(&up_down(), "up_down");
    let ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 60, 7);
    let (result, mut k) = abt_tests::run_kernel(&prog, &ds, ExecConfig::frictionless()).unwrap();
    let bars = k.bars_at(&prog.decision_cal);
    assert_eq!(bars, result.bars);
    let total: usize = bars.iter().map(|&t| k.query("up", t, &[]).unwrap().len()).sum();
    let cov = result.coverage.iter().find(|c| c.name == "up").unwrap();
    assert_eq!(total, cov.tuples, "the ledger holds what the run derived");
    let saturday = common::day("2022-01-08");
    let err = k.check_bar(&prog.decision_cal, saturday).unwrap_err().to_string();
    assert!(err.contains("2022-01-08 is not a bar of calendar base"), "{}", err);
}

/// A program's text with an `observe` of every relation its rules block
/// declares (a template, `rel R[...]`, is observed at its uses only), and
/// the number of its observations.
fn observing_everything(src: &str) -> (String, usize) {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|l| l.trim_end() == "  rules {").expect("a rules block");
    let end = start + lines[start..].iter().position(|l| l.trim_end() == "  }").expect("the rules block closes");
    let block = &lines[start..end];
    let observed: std::collections::BTreeSet<&str> = block.iter().filter_map(|l| l.trim().strip_prefix("observe ")?.split_whitespace().next()).collect();
    let added: Vec<&str> = block
        .iter()
        .filter_map(|l| l.trim().strip_prefix("rel ")?.split('(').next())
        .map(str::trim)
        .filter(|n| !n.contains('['))
        .filter(|n| !observed.contains(n))
        .collect();
    let mut out: Vec<String> = lines[..end].iter().map(|l| l.to_string()).collect();
    out.extend(added.iter().map(|n| format!("    observe {}", n)));
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    (out.join("\n"), observed.len() + added.len())
}

/// What a run decided and did, to the bit, with each decision's rule by its
/// label.
fn decided(p: &abt_kernel::check::Program, r: &abt_kernel::kernel::RunResult) -> Vec<String> {
    let mut out = vec![format!("bars {}", r.bars.len())];
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), p.rule_label(d.rule)));
    }
    for f in &r.fills {
        out.push(format!("fill {} {} {:x} {:x}", f.t, f.equity, f.quantity.to_bits(), f.price.to_bits()));
    }
    for (t, e) in &r.equity_curve {
        out.push(format!("equity {} {:x}", t, e.to_bits()));
    }
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out
}

/// Proposition V-8.4 on every corpus program and both books: the program
/// observing every relation it declares decides as the program does, to
/// the bit, and writes a ledger per observation (Definition V-8.3).
#[test]
fn observing_every_relation_changes_no_decision() {
    for path in common::golden_programs() {
        let src = std::fs::read_to_string(&path).unwrap();
        let (p, ds) = common::golden_inputs(&path).unwrap();
        let base = common::run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
        let (text, written) = observing_everything(&src);
        let (q, _) = common::golden_inputs_of(&text, &path.display().to_string()).unwrap();
        // An observation per line, or per calendar instance of a relation
        // declared on several calendars (R8L's `open0`).
        assert!(q.observes.len() >= written, "{}", p.strategy);
        if p.strategy == "r8l" {
            let open0: Vec<&str> = q.observes.iter().map(|o| o.rel.as_str()).filter(|r| r.starts_with("open0@")).collect();
            assert_eq!(open0.len(), 2, "`open0` is observed on each of its calendars: {:?}", open0);
        }
        let n = q.observes.len();
        let client = common::client(&q, &ds);
        let opts = abt_kernel::kernel::Replay {
            write_outputs: true,
            ..Default::default()
        };
        let r = abt_kernel::kernel::run_on(&q, None, &client, ExecConfig::from_program(&q), &opts).unwrap_or_else(|e| panic!("{}: {}", p.strategy, e));
        assert_eq!(decided(&q, &r), decided(&p, &base), "{}: observing changed what it decided", p.strategy);
        let ledgers = r.contract.get("post.ledgers").map(|c| c.status.clone());
        assert!(
            matches!(ledgers, Some(abt_kernel::contract::Status::Checked { times }) if times == n as u64),
            "{}: {:?}",
            p.strategy,
            ledgers
        );
    }
}
