//! The data block as a demand (K-2.1, D-7.2, M-3): `require reviewed` and
//! `allow unreviewed`, the `budget` clause and the dynamic `need chain`
//! parse, print and check; the kernel sends the demand the block writes,
//! and the data layer's answer binds, refuses or warns (K-3.2).

#![allow(clippy::result_large_err)]

use abt_kernel::check::Code;
use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::bind::demand_of;
use abt_kernel::kernel::{ExecConfig, RunError};
use abt_kernel::parse_units;
use abt_kernel::syntax::ast::{NeedKind, Require, Unit};
use abt_kernel::syntax::print::{render_strategy, Options};
use abt_tests as common;
use abt_tests::StrategyParts;
use abt_wire::generated::abt::wire::PreferenceKind;

fn data_block(src: &str) -> abt_kernel::syntax::ast::DataBlock {
    let units = parse_units(src).unwrap_or_else(|e| panic!("{}\n{}", e, src));
    match units.into_iter().next() {
        Some(Unit::Strategy(s)) => s.data.expect("a data block"),
        _ => panic!("not a strategy"),
    }
}

fn with_data(data: &str) -> String {
    StrategyParts::default().render().replacen("need bars @1d   need listing   require reviewed   prefer latest", data, 1)
}

#[test]
fn the_budget_and_the_chain_need_parse_and_print_back() {
    let src = with_data(
        "need bars @1d   need listing   need chain of \"SPY\" within 20d..60d and 0.9..1.1 right call spot close   require reviewed   prefer latest   budget { money 0 USD; bytes 10 GB; requests 100 }",
    );
    let d = data_block(&src);
    let (b, _) = d.budget.clone().expect("the budget");
    assert_eq!((b.money, b.bytes, b.requests, b.seconds), (Some(0.0), Some(10_000_000_000), Some(100), None));
    let chain = d
        .needs
        .iter()
        .find_map(|n| match &n.kind {
            NeedKind::Chain(c) => Some(c.clone()),
            _ => None,
        })
        .expect("the chain need");
    assert_eq!(chain.underlying, "SPY");
    assert_eq!((chain.expiry.0.days, chain.expiry.1.days), (20, 60));
    assert_eq!(chain.moneyness, (0.9, 1.1));
    assert_eq!((chain.right.as_deref(), chain.spot.as_deref()), (Some("call"), Some("close")));
    assert_eq!(d.require.map(|r| r.0), Some(Require::Reviewed));
    // Printed and parsed again, the block is the same.
    let Some(Unit::Strategy(s)) = parse_units(&src).unwrap().into_iter().next() else { panic!() };
    let printed = render_strategy(
        &s,
        &Options {
            execution: None,
            report: None,
            rule_style: None,
        },
    );
    let again = data_block(&printed);
    assert_eq!(again.budget.map(|b| b.0), d.budget.map(|b| b.0));
    assert_eq!(again.needs.len(), d.needs.len());
    assert!(printed.contains("need chain of \"SPY\" within 20d..60d and 0.9..1.1 right call spot close"), "{}", printed);
}

#[test]
fn the_retired_words_name_their_replacements() {
    for (old, new) in [("require tested", "require reviewed"), ("allow untested", "allow unreviewed")] {
        let src = with_data(&format!("need bars @1d   need listing   {}   prefer latest", old));
        let err = parse_units(&src).err().unwrap_or_else(|| panic!("`{}` parses", old)).to_string();
        assert!(err.contains(new), "{}: {}", old, err);
    }
    let src = with_data("need bars @1d   need listing   require reviewed   prefer latest   budget { money 1; money 2 }");
    assert!(parse_units(&src).err().unwrap().to_string().contains("twice"));
    let src = with_data("need bars @1d   need listing   require reviewed   prefer latest   budget { coins 3 }");
    assert!(parse_units(&src).err().unwrap().to_string().contains("not a budget"));
}

#[test]
fn a_chain_is_the_option_classs_and_its_ranges_are_not_empty() {
    let src = with_data("need bars @1d   need listing   need chain of \"SPY\" within 60d..20d and 1.1..0.9   require reviewed   prefer latest");
    let (_, diags) = common::check(&src, "s");
    let errs = common::errors(&diags);
    let q: Vec<&String> = errs.iter().filter(|(c, _)| *c == Code::Q).map(|(_, m)| m).collect();
    assert!(q.iter().any(|m| m.contains("class option")), "{:?}", errs);
    assert!(q.iter().any(|m| m.contains("expiry range") && m.contains("empty")), "{:?}", errs);
    assert!(q.iter().any(|m| m.contains("moneyness range")), "{:?}", errs);
}

#[test]
fn the_kernel_sends_the_demand_its_data_block_writes() {
    let src = trading("need bars @1d from 2020-01-01 complete   need listing   require reviewed   prefer version \"2026.10\"   budget { requests 5 }");
    let p = common::program(&src, "s");
    assert!(p.demand.require_reviewed);
    let d = demand_of(&p, &|n| Some(format!("equity.{}", n)), None);
    let needs = d.needs.clone().unwrap();
    let bars = needs.iter().find(|n| n.relations.contains(&"equity.close".to_string())).expect("the bars need");
    assert_eq!(bars.label.as_deref(), Some("bars @1d from 2020-01-01 complete"));
    assert_eq!(bars.calendar.as_deref(), Some("1d"));
    assert!(bars.from.is_some());
    let attrs: Vec<(String, String)> = bars.attributes.clone().unwrap().into_iter().map(|a| (a.key, a.value)).collect();
    assert_eq!(attrs, vec![("reviewed".to_string(), "complete".to_string())]);
    let listing = needs.iter().find(|n| n.relations.contains(&"equity.listed".to_string())).expect("the listing need");
    assert!(listing.attributes.is_none());
    assert_eq!(d.require.clone().unwrap()[0].key, "reviewed");
    let prefer = &d.prefer.clone().unwrap()[0];
    assert_eq!((prefer.kind, prefer.version.as_deref()), (PreferenceKind::Version, Some("2026.10")));
    assert_eq!(d.budget.as_ref().unwrap().requests, Some(5));
    assert_eq!(d.budget.as_ref().unwrap().money, 0.0);
}

fn trading(data: &str) -> String {
    StrategyParts {
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render()
    .replacen("need bars @1d   need listing   require reviewed   prefer latest", data, 1)
}

#[test]
fn the_binding_refuses_or_warns_as_the_preconditions_say() {
    let mut ds = synthetic_daily(&["AAA", "BBB"], (2022, 1, 3), 40, 3);
    // Reviewed data under `require reviewed`: bound, and a `from` before the
    // data's first bar is a warm-up warning, not a refusal (E-41).
    let p = common::program(&trading("need bars @1d from 2015-01-01   need listing   require reviewed   prefer latest"), "s");
    let r = common::run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(r.warnings.iter().any(|w| w.bias == "warm-up" && w.message.contains("2015-01-01")), "{:?}", r.warnings);
    // Unreviewed data: refused under `require reviewed`, naming the need.
    ds.unreviewed = true;
    let p = common::program(&trading("need bars @1d   need listing   require reviewed   prefer latest"), "s");
    match common::run(&p, &ds, ExecConfig::from_program(&p)) {
        Err(RunError::Unbound(m)) => assert!(m.contains("need `bars @1d`") && m.contains("no `reviewed` attribute"), "{}", m),
        other => panic!("{:?}", other.map(|r| r.decisions.len())),
    }
    // Admitted under `allow unreviewed`, warned.
    let p = common::program(&trading("need bars @1d   need listing   allow unreviewed   prefer latest"), "s");
    let r = common::run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
    assert!(r.warnings.iter().any(|w| w.bias == "unreviewed data"), "{:?}", r.warnings);
    // A `complete` need asks for `reviewed = complete` under either stance.
    let p = common::program(&trading("need bars @1d complete   need listing   allow unreviewed   prefer latest"), "s");
    assert!(matches!(common::run(&p, &ds, ExecConfig::from_program(&p)), Err(RunError::Unbound(_))));
    // A version the store does not hold.
    ds.unreviewed = false;
    ds.version = Some("2026.10".into());
    let p = common::program(&trading("need bars @1d   need listing   require reviewed   prefer version \"2026.11\""), "s");
    match common::run(&p, &ds, ExecConfig::from_program(&p)) {
        Err(RunError::Unbound(m)) => assert!(m.contains("2026.11"), "{}", m),
        other => panic!("{:?}", other.map(|r| r.decisions.len())),
    }
    let p = common::program(&trading("need bars @1d   need listing   require reviewed   prefer version \"2026.10\""), "s");
    common::run(&p, &ds, ExecConfig::from_program(&p)).unwrap();
}
