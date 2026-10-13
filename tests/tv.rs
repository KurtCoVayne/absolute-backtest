//! Translation validation (compiler design, section 6.6, tier 5): the plan
//! with every rewrite disabled and the written literal order, and the plan
//! the full pipeline produces, run on the same binding and must agree to
//! the bit on `decide` at every bar, on the ledgers of every demanded
//! relation and on the book. Step 3 of section 9: the unoptimised path
//! through the kernel's plan hooks equals the current kernel, so the hooks
//! change no behaviour. The corpus is run on the seeded synthetic markets;
//! the two company books run when `ABT_MW14_BUNDLE` and `ABT_R8L_BUNDLE`
//! name their bundles, and are skipped with a note otherwise.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use absolute_backtest::check::Program;
use absolute_backtest::compiler::{compile, CompileOptions};
use absolute_backtest::data::{synthetic_daily, synthetic_minute};
use absolute_backtest::kernel::{run, run_fold, run_fold_planned, run_planned, Dataset, ExecConfig, Kernel, RunError, RunResult, Value};
use absolute_backtest::lower::{Lowered, Numerics};
use absolute_backtest::Kind;

fn corpus_strategies() -> Vec<(String, Program)> {
    common::dsl_files("strategies")
        .into_iter()
        .map(|f| {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            (name.clone(), common::corpus_program(&name))
        })
        .collect()
}

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

/// The synthetic market of a program's base: minute bars for a sub-daily
/// base, else the daily market with its catalog.
fn dataset_for(p: &Program, seed: u64) -> Dataset {
    if p.base.is_sub_daily() {
        synthetic_minute(&SYMS, (2024, 1, 2), 30, 320, seed)
    } else {
        synthetic_daily(&SYMS, (2022, 1, 3), 320, seed)
    }
}

/// What a run produced, as comparable text and bits: `decide` at every
/// bar, the fills, the book at every bar, the final state.
fn digest(r: &RunResult) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("bars {:?}", r.bars));
    for d in &r.decisions {
        out.push(format!("decision {} {} {}", d.t, r.describe_decision(&d.decision), d.rule));
    }
    for f in &r.fills {
        out.push(format!(
            "fill {} {} {:x} {:x} {} {:x} {:x} {:x} {:x} {:x} {} {}",
            f.t,
            r.symbols[f.equity as usize],
            f.quantity.to_bits(),
            f.price.to_bits(),
            f.at_last_price,
            f.commission.to_bits(),
            f.fee.to_bits(),
            f.slippage.to_bits(),
            f.impact.to_bits(),
            f.participation.to_bits(),
            f.partial,
            f.forced
        ));
    }
    for (t, d, why) in &r.dropped {
        out.push(format!("dropped {} {} {}", t, r.describe_decision(d), why));
    }
    for e in &r.exposure {
        out.push(format!(
            "exposure {} {:x} {:x} {:x} {:x} {:x}",
            e.t,
            e.cash.to_bits(),
            e.gross.to_bits(),
            e.net.to_bits(),
            e.equity.to_bits(),
            e.leverage.to_bits()
        ));
    }
    for a in &r.actions {
        out.push(format!("action {} {} {:?} {:x}", a.t, r.symbols[a.equity as usize], a.action, a.cash.to_bits()));
    }
    out.push(format!("costs {:?}", r.costs));
    out.push(format!("funding {:?}", r.funding));
    out.push(format!("final_cash {:x}", r.final_cash.to_bits()));
    out.push(format!("final_positions {:?}", r.final_positions));
    out
}

/// The ledgers of every derived relation (a bracketed input is substituted
/// at link time, so every instance is enumerable): every tuple at every bar
/// of its calendar, as text with bits.
fn ledgers(k: &mut Kernel, prog: &Program, bars: &[i64]) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let first = bars[0];
    let last = *bars.last().unwrap();
    let names: Vec<String> = prog
        .relations
        .iter()
        .filter(|(_, s)| matches!(s.kind, Kind::Derived { .. } | Kind::Output))
        .map(|(n, _)| n.clone())
        .collect();
    for rel in names {
        let sig = &prog.relations[&rel];
        let own: Vec<i64> = k.bars_at(&sig.cal).into_iter().filter(|&b| b >= first && b <= last).collect();
        let mut rows = Vec::new();
        for t in own {
            let found: Vec<Vec<Value>> = match k.query(&rel, t, &[]) {
                Ok(tus) => tus,
                Err(RunError::Arithmetic { .. }) => vec![vec![Value::Count(-1)]],
                Err(e) => panic!("{}: {}", rel, e),
            };
            for tu in found {
                let cells: Vec<String> = tu
                    .iter()
                    .map(|v| match v {
                        Value::Num(x) => format!("{:x}", x.to_bits()),
                        v => k.show(v),
                    })
                    .collect();
                rows.push(format!("{} {}", t, cells.join(" ")));
            }
        }
        out.insert(rel, rows);
    }
    out
}

fn first_difference(a: &[String], b: &[String]) -> Option<(usize, String, String)> {
    let n = a.len().max(b.len());
    (0..n)
        .find(|&i| a.get(i) != b.get(i))
        .map(|i| (i, a.get(i).cloned().unwrap_or_default(), b.get(i).cloned().unwrap_or_default()))
}

fn assert_same(name: &str, what: &str, a: &[String], b: &[String]) {
    if let Some((i, x, y)) = first_difference(a, b) {
        panic!("{}: {} differ at line {} ({} / {} lines):\n  a: {}\n  b: {}", name, what, i, a.len(), b.len(), x, y);
    }
}

/// The kernel's plan hooks change nothing: a run under the written plan
/// equals the plain run to the bit, on the batch driver and the fold.
#[test]
fn the_written_plan_equals_the_plain_kernel() {
    for (name, p) in corpus_strategies() {
        let ds = dataset_for(&p, 11);
        let cfg = ExecConfig::from_program(&p);
        let plain = run(&p, &ds, cfg.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let written = Lowered::written(&p, Numerics::from_running_sums(cfg.running_sums));
        let planned = run_planned(&p, &written, &ds, cfg.clone()).unwrap_or_else(|e| panic!("{} (planned): {}", name, e));
        assert_same(&name, "batch runs", &digest(&plain), &digest(&planned));
        let fold_plain = run_fold(&p, &ds, cfg.clone()).unwrap();
        let fold_planned = run_fold_planned(&p, &written, &ds, cfg).unwrap();
        assert_same(&name, "fold runs", &digest(&fold_plain), &digest(&fold_planned));
    }
}

/// A run's digest and ledgers, or its halt.
type Outcome = Result<(Vec<String>, BTreeMap<String, Vec<String>>), String>;

/// A run under a plan: the digest and the ledgers (through the kernel kept
/// after the run), or the halt.
fn run_with_ledgers(p: &Program, lowered: &Lowered, ds: &Dataset, cfg: ExecConfig) -> Outcome {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn_scoped(s, || {
                let mut k = Kernel::new_planned(p, lowered, ds, cfg).map_err(|e| e.to_string())?;
                let r = k.run().map_err(|e| e.to_string())?;
                let l = ledgers(&mut k, p, &r.bars);
                Ok((digest(&r), l))
            })
            .unwrap()
            .join()
            .unwrap()
    })
}

/// Section 6.6: the optimised plan equals the written plan on every corpus
/// strategy: `decide` at every bar, the ledgers of the demanded relations,
/// the book.
#[test]
fn the_optimised_plan_equals_the_written_plan_on_the_corpus() {
    for (name, p) in corpus_strategies() {
        let ds = dataset_for(&p, 11);
        let cfg = ExecConfig::from_program(&p);
        let numerics = Numerics::from_running_sums(cfg.running_sums);
        let compiled = compile(
            &p,
            &CompileOptions {
                numerics,
                ..CompileOptions::default()
            },
        );
        let written = Lowered::written(&p, numerics);
        let a = run_with_ledgers(&p, &written, &ds, cfg.clone());
        let b = run_with_ledgers(&p, &compiled.lowered, &ds, cfg);
        match (a, b) {
            (Ok((da, la)), Ok((db, lb))) => {
                assert_same(&name, "runs", &da, &db);
                assert_eq!(la.keys().collect::<Vec<_>>(), lb.keys().collect::<Vec<_>>(), "{}: ledger sets differ", name);
                for (rel, rows) in &la {
                    assert_same(&name, &format!("ledger of {}", rel), rows, &lb[rel]);
                }
            }
            (Err(ea), Err(eb)) => assert_eq!(ea.split(" at tuple").next(), eb.split(" at tuple").next(), "{}: both halted, differently", name),
            (a, b) => panic!("{}: one plan halted and the other did not: written {:?}, optimised {:?}", name, a.map(|_| ()), b.map(|_| ())),
        }
    }
}

/// The two company books under the run configurations their execution
/// blocks write, when their bundles are present.
#[test]
fn the_optimised_plan_equals_the_written_plan_on_the_books() {
    for (name, var) in [("mw14", "ABT_MW14_BUNDLE"), ("r8l", "ABT_R8L_BUNDLE")] {
        let Ok(dir) = std::env::var(var) else {
            eprintln!("note: {} not set; the {} book is not validated", var, name);
            continue;
        };
        let p = common::program(&common::corpus_source("company", name), name);
        let (ds, _) = absolute_backtest::bundle::load_bundle(&p, Path::new(&dir), true).unwrap_or_else(|e| panic!("{}: {}", var, e));
        let cfg = ExecConfig::from_program(&p);
        let numerics = Numerics::from_running_sums(cfg.running_sums);
        let compiled = compile(
            &p,
            &CompileOptions {
                numerics,
                ..CompileOptions::default()
            },
        );
        let written = Lowered::written(&p, numerics);
        let a = run_planned(&p, &written, &ds, cfg.clone()).unwrap_or_else(|e| panic!("{}: {}", name, e));
        let b = run_planned(&p, &compiled.lowered, &ds, cfg).unwrap_or_else(|e| panic!("{} (optimised): {}", name, e));
        assert_same(name, "book runs", &digest(&a), &digest(&b));
    }
}
