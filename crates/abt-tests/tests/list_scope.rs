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

/// The clock of what a run asks of the data layer before its stream: a
/// client notes every call it makes, with its relation, its form, the rows
/// answered and the seconds taken; `ABT_TIMING_RELS` prints a stage's.
#[test]
fn a_client_logs_every_call_a_binding_and_its_preconditions_make() {
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "CCC", "DDD"], (2022, 1, 3), 60, 3);
    let gated = common::program(&program("gated", "universe(A, T) :- listed(A, T), traded@base(A, T), member(A, T, \"SPX\").", ""), "gated");
    let open = common::program(
        &program(
            "open",
            "universe(A, T) :- listed(A, T), traded@base(A, T), in_spx(A, T).",
            "    rel in_spx(A: Equity, @T)\n    in_spx(A, T) :- member(A, T, \"SPX\").\n",
        ),
        "open",
    );
    let svc = abt::serve_dataset(&ds, &gated).unwrap();
    let client = DataClient::connect(abt_data::service::inproc(&svc)).unwrap();
    assert_eq!(client.calls_made(), 0);
    let bound = bind_at(&open, &client, &point_of(&open, &[]), None).unwrap();
    let bind = client.calls();
    assert_eq!(client.calls_made(), bind.len() as u64);
    let of = |calls: &[abt_kernel::kernel::bind::DataCall], call: &str| calls.iter().filter(|c| c.call == call).count();
    // The binding's calls in the order it makes them: the schemas, the
    // demand resolved, the store's versions once, then its queries.
    assert_eq!(bind.iter().take(3).map(|c| c.call).collect::<Vec<_>>(), vec!["schemas", "resolve", "stats"]);
    assert_eq!((of(&bind, "schemas"), of(&bind, "resolve"), of(&bind, "stats")), (1, 1, 1));
    assert!(bind.iter().skip(3).all(|c| c.call == "query"), "{:?}", bind.iter().map(|c| c.line()).collect::<Vec<_>>());
    let asked = |calls: &[abt_kernel::kernel::bind::DataCall], relation: &str, detail: &str| calls.iter().filter(|c| c.relation == relation && c.detail == detail).count();
    assert_eq!(asked(&bind, "instrument", "catalog"), 1);
    assert_eq!(asked(&bind, "vendor_id", "catalog"), 1);
    // The labels of a membership and the base bars of the closes are whole-
    // history `distinct`s; each answers the values, not the rows.
    assert_eq!(asked(&bind, "equity.member", "distinct(List)"), 1);
    assert_eq!(asked(&bind, "equity.close", "distinct(T)"), 1);
    let bars = bind.iter().find(|c| c.relation == "equity.close" && c.detail == "distinct(T)").unwrap();
    assert_eq!(bars.rows, bound.base_bars.len() as u64);
    assert_eq!(bars.rows, 60);
    assert!(bind.iter().all(|c| c.secs >= 0.0 && c.secs.is_finite()));
    // A line of the log: five fields, the first `data`.
    for c in &bind {
        let line = c.line();
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(fields.len(), 6, "{}", line);
        assert_eq!((fields[0], fields[1], fields[2], fields[3]), ("data", c.call, c.relation.as_str(), c.detail.as_str()));
        assert_eq!(fields[4], format!("rows={}", c.rows));
        assert!(fields[5].starts_with("secs="));
    }
    // A list-gated universe's binding scans the membership for its lists'
    // instruments, and reads its few members a series each.
    let mark = client.calls_made();
    bind_at(&gated, &client, &point_of(&gated, &[]), None).unwrap();
    let scoped = client.calls_since(mark);
    assert_eq!(client.calls_made(), mark + scoped.len() as u64);
    assert_eq!(asked(&scoped, "equity.member", "scan(A;where=List.in(1))"), 1);
    assert!(scoped.iter().any(|c| c.relation == "equity.close" && c.detail.starts_with("series(")));
    assert_eq!(client.calls().len(), bind.len() + scoped.len());
    // The preconditions' calls follow the binding's on the same log.
    let mark = client.calls_made();
    let mut contract = abt_kernel::contract::contract(&open);
    abt_kernel::contract::check::preconditions(&mut contract, &open, &bound, &client, &ExecConfig::from_program(&open)).unwrap();
    let checks = client.calls_since(mark);
    assert!(
        !checks.is_empty() && checks.iter().all(|c| c.call == "query" && c.detail.starts_with("scan")),
        "{:?}",
        checks.iter().map(|c| c.line()).collect::<Vec<_>>()
    );
    // The price check asks for the instrument and the time of the closes
    // at or below zero, and the action check for the closes' three columns
    // (E-139).
    assert_eq!(asked(&checks, "equity.close", "scan(A,T;where=P.le(1))"), 1);
    assert_eq!(asked(&checks, "equity.close", "scan(A,T,P)"), 1);
    assert_eq!(client.calls_since(client.calls_made()), vec![]);
}
