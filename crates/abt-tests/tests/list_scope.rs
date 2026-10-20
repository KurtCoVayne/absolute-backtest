//! E-120: a universe its lists gate binds the instruments those lists ever
//! held, and runs as the same universe bound to every instrument does. On a
//! store of every US equity 1990–2026 an S&P 500 program's binding read and
//! streamed every instrument, and took the data service past 16 GB.

use abt_kernel::kernel::bind::{bind_at, point_of};
use abt_kernel::kernel::{DataClient, ExecConfig, Replay, RunResult, Value};
use abt_tests as common;

/// A program holding a tenth of each name of its universe, the universe
/// as `universe` writes it.
fn program(name: &str, universe: &str, rules: &str) -> String {
    format!(
        "strategy {name} {{
  data {{ class equity   need bars @1d   need actions, listing   need lists \"SPX\"   require reviewed   prefer latest }}
  uses calendar, bars, book
  clock {{ decide on base }}
  universe {{
    {universe}
  }}
  params {{ w : Scalar = 0.1 }}
  rules {{
    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), close(A, T, P), P > 0 USD/share.
{rules}  }}
  decisions {{
    silence hold
    orders  market
    decide(T, hold(A, w)) :- up(A, T).
  }}
  execution equity_default
  report {{ calendar base   periods 252 per year }}
}}"
    )
}

fn digest(r: &RunResult) -> Vec<String> {
    let mut out: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", d.t, r.describe_decision(&d.decision))).collect();
    out.extend(
        r.fills
            .iter()
            .map(|f| format!("fill {} {} {:x} {:x}", f.t, r.symbols[f.equity as usize], f.quantity.to_bits(), f.price.to_bits())),
    );
    out.push(format!("cash {:x}", r.final_cash.to_bits()));
    out
}

#[test]
fn a_list_gated_universe_binds_its_lists_members_and_runs_the_same() {
    // Six names (keyed E1 to E6), three of them ever in SPX.
    let mut ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "EEE", "FFF"], (2022, 1, 3), 80, 3);
    let members: Vec<Value> = ["E1", "E2", "E3"].iter().map(|n| Value::Equity(ds.symbols.get(n).unwrap())).collect();
    if let Some(rows) = ds.facts.get_mut("member") {
        rows.retain(|tu| members.contains(&tu[0]));
    }
    ds.available_at.remove("member");
    let gated = program("gated", "universe(A, T) :- listed(A, T), traded@base(A, T), member(A, T, \"SPX\").", "");
    // The same universe through a derived relation: not gated by a list.
    let open = program(
        "open",
        "universe(A, T) :- listed(A, T), traded@base(A, T), in_spx(A, T).",
        "    rel in_spx(A: Equity, @T)\n    in_spx(A, T) :- member(A, T, \"SPX\").\n",
    );
    let (pg, po) = (common::program(&gated, "gated"), common::program(&open, "open"));
    let svc = abt::serve_dataset(&ds, &pg).unwrap();
    let client = || DataClient::connect(abt_data::service::inproc(&svc)).unwrap();
    // The gated binding is its members'; the other every instrument's.
    let bg = bind_at(&pg, &client(), &point_of(&pg, &[]), None).unwrap();
    let bo = bind_at(&po, &client(), &point_of(&po, &[]), None).unwrap();
    let scope: Vec<String> = bg
        .scope
        .clone()
        .expect("a scoped binding")
        .iter()
        .map(|&id| bg.symbols.name(bg.ids.iter().position(|&x| x == id).unwrap() as u32).to_string())
        .collect();
    assert_eq!(scope, vec!["E1", "E2", "E3"]);
    assert_eq!(bo.scope, None);
    // And both runs decide, fill and end alike.
    let run = |p: &abt_kernel::check::Program| abt_kernel::kernel::run_on(p, None, &client(), ExecConfig::from_program(p), &Replay::default()).unwrap();
    let (rg, ro) = (run(&pg), run(&po));
    assert!(!rg.decisions.is_empty());
    assert_eq!(digest(&rg), digest(&ro));
}
