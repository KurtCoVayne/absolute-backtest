//! E-99: a program that names its instruments (its universe's `instruments`
//! table, or `need instruments where symbol = "…"`) binds and streams those
//! instruments only, and decides exactly as the unscoped program decides for
//! them.

use abt_tests as common;

use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::bind::{bind, scope_entities};
use abt_kernel::kernel::{run_on, ExecConfig, Replay, RunResult};

const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// sma_crossover with its universe narrowed by an instruments table, or by
/// the data block's need (its universe unchanged: the other names' rows
/// never reach the run).
fn scoped(by_table: bool) -> String {
    let src = common::corpus_source("strategies", "sma_crossover");
    let src = src.replace("strategy sma_crossover {", "# allow: W9\nstrategy sma_scoped {");
    if by_table {
        src.replace(
            "    universe(A, T) :- listed(A, T), traded@base(A, T).",
            "    instruments { AAA from 2022-01-03; BBB from 2022-01-03 }\n    universe(A, T) :- instruments(A, From), listed(A, T), traded@base(A, T), T >= From.",
        )
    } else {
        src.replace(
            "    need    actions, listing\n",
            "    need    actions, listing\n    need    instruments where symbol = \"AAA\", instruments where symbol = \"BBB\"\n",
        )
    }
}

/// What a run decided and filled for the named instruments, without the
/// rules' names (the programs differ in name only).
fn digest(r: &RunResult, keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for d in &r.decisions {
        let text = r.describe_decision(&d.decision);
        if keys.iter().any(|k| text.contains(&format!("({},", k))) {
            out.push(format!("decision {} {}", d.t, text));
        }
    }
    for f in &r.fills {
        let sym = &r.symbols[f.equity as usize];
        if keys.contains(sym) {
            out.push(format!("fill {} {} {:x} {:x} {:x}", f.t, sym, f.quantity.to_bits(), f.price.to_bits(), f.commission.to_bits()));
        }
    }
    out
}

#[test]
fn a_scoped_program_reads_its_instruments_only_and_decides_as_the_unscoped_one() {
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 300, 13);
    let whole = common::corpus_program("sma_crossover");
    let client = common::client(&whole, &ds);
    let unscoped = run_on(&whole, None, &client, ExecConfig::from_program(&whole), &Replay::default()).unwrap();
    // The instruments' keys in the data layer, through the ticker table.
    let whole_bound = bind(&whole, &client).unwrap();
    let key = |t: &str| whole_bound.symbols.name(whole_bound.resolve_ticker(t, None).unwrap()).to_string();
    let named = [key("AAA"), key("BBB")];
    let others: Vec<String> = whole_bound.symbols.names().iter().filter(|n| !named.contains(n)).cloned().collect();
    let expected = digest(&unscoped, &named);
    assert!(expected.iter().any(|l| l.starts_with("fill")), "the unscoped run trades AAA or BBB: {:?}", expected);
    for by_table in [true, false] {
        let src = scoped(by_table);
        let prog = common::program(&src, "sma_scoped");
        let bound = bind(&prog, &client).unwrap();
        let ids = scope_entities(&prog, &bound).expect("the program names its instruments");
        assert_eq!(ids.len(), 2, "by_table {}: {:?}", by_table, ids);
        let r = run_on(&prog, None, &client, ExecConfig::from_program(&prog), &Replay::default()).unwrap();
        assert_eq!(digest(&r, &named), expected, "by_table {}", by_table);
        // Nothing of the other names reached the run.
        assert!(digest(&r, &others).is_empty(), "by_table {}", by_table);
    }
    // A program that names nothing reads everything.
    assert!(scope_entities(&whole, &whole_bound).is_none());
}
