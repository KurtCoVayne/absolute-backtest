//! E-115: a run at a parameter point (`--param`, a plan's point, a grid's
//! cell) is compiled at that point. The analysis judged a guard on a
//! parameter at its default (`side > 0` never holds at `side = 0`, so R2
//! dropped the rule) and constant folding computed `1 - w` at the default,
//! so a run at another point ran the default's rules: grids and variants
//! over such a parameter repeated the default. Through the kernel service,
//! as a plan's trials run.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use abt_kernel::kernel::service::{inproc, KernelService};
use abt_kernel::kernel::DataClient;
use abt_tests as common;
use abt_wire::generated::abt::wire::{KernelOp, KernelRequest, KernelResponse, Message, ParamValue, SourceFile};
use abt_wire::rpc::{Client, Reply};
use abt_wire::Layer;

const PROGRAM: &str = "strategy param_point {
  data {
    class   equity
    need    bars @1d
    need    actions, listing
    require reviewed
    prefer  latest
  }
  uses  calendar, bars, book
  clock { decide on base }
  universe {
    universe(A, T) :- listed(A, T), traded@base(A, T).
  }
  params {
    side : Scalar = 0
    w    : Scalar = 1
  }
  rules {
    rel weight(@T, W: Scalar)
    weight(T, W) :- bar(T), side > 0, W = (1 - w) * 0.2 + 0.05.
  }
  decisions {
    silence hold
    orders  market
    decide(T, hold(A, W)) :- universe(A, T), weight(T, W).
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
";

fn request(point: &[(&str, &str)]) -> KernelRequest {
    let mut sources: Vec<SourceFile> = common::dsl_files("catalog")
        .into_iter()
        .chain(common::dsl_files("lib"))
        .map(|p| SourceFile {
            text: std::fs::read_to_string(&p).unwrap(),
            path: Some(p.to_string_lossy().into_owned()),
        })
        .collect();
    sources.push(SourceFile {
        text: PROGRAM.into(),
        path: Some("param_point.dsl".into()),
    });
    KernelRequest {
        op: KernelOp::Run,
        sources: Some(sources),
        strategy: Some("param_point".into()),
        point: Some(
            point
                .iter()
                .map(|(n, v)| ParamValue {
                    name: n.to_string(),
                    value: v.to_string(),
                })
                .collect(),
        ),
        what: None,
        run_id: None,
        text: None,
        to_seq: None,
        start: None,
        end: None,
        memory_budget: None,
        pushdown: true,
        progress_every: None,
        checkpoint_every: None,
        from_checkpoint: None,
        horizon: None,
        received_by: None,
        plan_step: None,
        embargo_mask: None,
        observe: None,
        mode: None,
        session: None,
        deployment: None,
    }
}

fn next(reply: &mut Reply) -> KernelResponse {
    match reply.message() {
        Ok(Message::KernelResponse(m)) => *m,
        other => panic!("not a kernel response: {:?}", other.map(|_| ())),
    }
}

/// A run at a point to its last frame: its report.
fn run(c: &Client, point: &[(&str, &str)]) -> serde_json::Value {
    let mut reply = c.call(Message::KernelRequest(Box::new(request(point)))).unwrap();
    let mut m = next(&mut reply);
    while m.status.as_deref() == Some("running") {
        m = next(&mut reply);
    }
    assert_eq!(m.status.as_deref(), Some("completed"), "{:?}", m.report);
    serde_json::from_str(m.report.as_deref().unwrap()).unwrap()
}

#[test]
fn a_run_at_a_point_is_compiled_at_it() {
    let prog = common::program(PROGRAM, "param_point");
    let ds = abt_kernel::data::synthetic_daily(&["AAA", "BBB", "SPY"], (2022, 1, 3), 120, 5);
    let svc = abt::serve_dataset(&ds, &prog).unwrap();
    let connect = Arc::new(move || DataClient::connect(abt_data::service::inproc(&svc)));
    let kernel = KernelService::new(connect);
    let c = Client::connect(inproc(&kernel), Layer::Front, "test").unwrap();
    // At the defaults the guard never holds.
    assert_eq!(run(&c, &[])["decisions"], 0);
    // At side = 1 it holds: the rule is not dropped.
    let low = run(&c, &[("side", "1"), ("w", "0")]);
    assert!(low["decisions"].as_u64().unwrap() > 0, "{}", low);
    // And `1 - w` is the point's: two weights, two books.
    let high = run(&c, &[("side", "1"), ("w", "0.8")]);
    assert_ne!(low["final_cash"], high["final_cash"], "w = 0 and w = 0.8 ran the same weight");
}
