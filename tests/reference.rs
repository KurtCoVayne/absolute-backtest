//! The pandas reference (data-bundle doc, section 8): every daily corpus
//! strategy, and the tiny action markets, replayed through
//! `reference/engine.py` from a kernel dump and compared by
//! `reference/diff.py` (fills identical in quantity, price to floating
//! tolerance, the book and NAV within tolerance). Skipped, with a note,
//! where `python3` with pandas is not installed.

mod corpus;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use absolute_backtest::check::{check_program, Program};
use absolute_backtest::data::{business_days, synthetic_daily, synthetic_daily_v2};
use absolute_backtest::dump::write_dump;
use absolute_backtest::kernel::{run, Dataset, ExecConfig, OnLeverage, OnOversize, Value};

fn program(src: &str, name: &str) -> Program {
    let mut ws = corpus::base_workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    let text: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text.join("\n")))
}

fn reference_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("reference")
}

/// Whether the reference can run here: python3 with pandas.
fn python_with_pandas() -> bool {
    Command::new("python3").args(["-c", "import pandas"]).output().map(|o| o.status.success()).unwrap_or(false)
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-reference-{}-{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Run the kernel, dump, replay with the reference and compare; the
/// diff's text on disagreement.
fn diff(tag: &str, prog: &Program, ds: &Dataset, cfg: ExecConfig) -> Result<(), String> {
    let result = run(prog, ds, cfg.clone()).map_err(|e| format!("{}: {}", tag, e))?;
    let dir = temp_dir(tag);
    write_dump(prog, ds, &cfg, &result, &dir)?;
    let out = Command::new("python3")
        .arg(reference_dir().join("diff.py"))
        .arg(&dir)
        .current_dir(reference_dir())
        .output()
        .map_err(|e| format!("python3: {}", e))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_dir_all(&dir);
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{}: kernel and reference disagree:\n{}", tag, text))
    }
}

const SYMS: [&str; 5] = ["AAA", "BBB", "CCC", "DDD", "SPY"];

#[test]
fn every_daily_corpus_strategy_agrees_with_the_reference() {
    if !python_with_pandas() {
        eprintln!("skipped: python3 with pandas is not installed");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies");
    let mut files: Vec<PathBuf> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let mut compared = 0;
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let src = fs::read_to_string(&f).unwrap();
        let prog = program(&src, &name);
        if prog.relations.values().any(|s| s.res == Some(absolute_backtest::Resolution::M1)) {
            // The reference executor is written for the daily contract.
            continue;
        }
        let ds = if prog.relations.contains_key("split") {
            synthetic_daily_v2(&SYMS, (2022, 1, 3), 320, 11)
        } else {
            synthetic_daily(&SYMS, (2022, 1, 3), 320, 11)
        };
        // A run the kernel's policies halt is the policy tests' business;
        // the comparison wants completed runs.
        if let Err(absolute_backtest::kernel::RunError::Risk { message, .. }) = run(&prog, &ds, ExecConfig::default()) {
            eprintln!("{}: halted ({}), not compared", name, message);
            continue;
        }
        diff(&name, &prog, &ds, ExecConfig::default()).unwrap_or_else(|e| panic!("{}", e));
        compared += 1;
    }
    assert!(compared >= 14, "{} strategies compared", compared);
}

#[test]
fn policy_and_model_variants_agree_with_the_reference() {
    if !python_with_pandas() {
        eprintln!("skipped: python3 with pandas is not installed");
        return;
    }
    let read = |name: &str| fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("corpus/strategies/{}.dsl", name))).unwrap();
    let ds = synthetic_daily(&SYMS, (2022, 1, 3), 400, 11);
    let lsq = program(&read("long_short_quantile"), "long_short_quantile");
    diff("reg-t", &lsq, &ds, ExecConfig::reg_t()).unwrap_or_else(|e| panic!("{}", e));
    // Both accountings: the default fixed base, and weights of equity.
    diff(
        "compounding",
        &lsq,
        &ds,
        ExecConfig {
            compounding: true,
            ..ExecConfig::reg_t()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    let top = program(&read("momentum_top_n"), "momentum_top_n");
    diff(
        "compounding-long",
        &top,
        &ds,
        ExecConfig {
            compounding: true,
            ..ExecConfig::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    diff(
        "liquidate",
        &lsq,
        &ds,
        ExecConfig {
            max_gross: 3.0,
            on_margin_call: absolute_backtest::kernel::OnMarginCall::Liquidate,
            ..ExecConfig::reg_t()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    let spike = program(&read("volume_spike"), "volume_spike");
    let long = synthetic_daily(&SYMS, (2022, 1, 3), 600, 11);
    diff(
        "clamp-reject",
        &spike,
        &long,
        ExecConfig {
            on_oversize: OnOversize::Clamp,
            on_leverage: OnLeverage::Reject,
            ..ExecConfig::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    diff(
        "partial-fills",
        &top,
        &ds,
        // Capped sells with full buys are leverage by design: rejected here.
        ExecConfig {
            participation_cap: 0.002,
            on_leverage: OnLeverage::Reject,
            ..ExecConfig::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    let monthly = program(&read("monthly_rebalance"), "monthly_rebalance");
    diff(
        "fractional",
        &monthly,
        &ds,
        ExecConfig {
            lot: absolute_backtest::kernel::Lot::Fractional,
            ..ExecConfig::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    let pairs = program(&read("pairs_trading"), "pairs_trading");
    diff(
        "fixed-slippage-rates",
        &pairs,
        &ds,
        ExecConfig {
            impact_coef: 0.0,
            slippage_vol_mult: 0.0,
            slippage_bps: 5.0,
            cash_rate: 0.02,
            short_rebate: 0.01,
            ..ExecConfig::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    diff("frictionless", &top, &ds, ExecConfig::frictionless()).unwrap_or_else(|e| panic!("{}", e));
}

// ---- the action markets (section 8: a split, a dividend, a spin-off as a
// dividend in kind, a delisting), each with a hand-computed NAV ----

const HOLD: &str = r#"
strategy hold {
  env equities_1d_v2
  uses catalog
  resolution @1d
  mode delta
  param qty : Quantity<Shares> = 10 shares
  decide(T, buy(A, qty)) :- universe(A, T), flat(A, T).
}
"#;

fn market(closes: &[Option<f64>]) -> (Dataset, u32, Vec<i64>) {
    let days = business_days((2024, 1, 8), closes.len());
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (t, p) in days.iter().zip(closes) {
        if let Some(p) = p {
            for rel in ["open", "high", "low", "close"] {
                ds.add(rel, vec![Value::Equity(x), Value::Time(*t), Value::Num(*p)]);
            }
            ds.add("volume", vec![Value::Equity(x), Value::Time(*t), Value::Num(1_000_000.0)]);
            ds.add("universe", vec![Value::Equity(x), Value::Time(*t)]);
        }
    }
    ds.derive_tickers();
    (ds, x, days)
}

fn frictionless(cash: f64) -> ExecConfig {
    ExecConfig {
        initial_cash: cash,
        cash_rate: 0.0,
        margin_rate: 0.0,
        borrow: vec![],
        ..ExecConfig::frictionless()
    }
}

#[test]
fn the_action_markets_agree_with_the_reference_and_their_hand_computed_nav() {
    if !python_with_pandas() {
        eprintln!("skipped: python3 with pandas is not installed");
        return;
    }
    let prog = program(HOLD, "hold");
    // A 3-for-1 split on day 3: 10 shares at 30 become 30 at 10; NAV 300 throughout.
    let (mut ds, x, days) = market(&[Some(30.0), Some(30.0), Some(10.0), Some(10.0), Some(10.0)]);
    ds.add("split", vec![Value::Equity(x), Value::Time(days[2]), Value::Num(3.0)]);
    let r = run(&prog, &ds, frictionless(1000.0)).unwrap();
    assert_eq!(r.final_positions.get(&x), Some(&30.0));
    assert_eq!(r.equity_curve.last().unwrap().1, 1000.0);
    diff("split", &prog, &ds, frictionless(1000.0)).unwrap_or_else(|e| panic!("{}", e));
    // A dividend of 1.5 announced day 1, ex day 3, paid day 4: 10 shares earn 15.
    let (mut ds, x, days) = market(&[Some(20.0), Some(20.0), Some(18.5), Some(18.5), Some(18.5)]);
    ds.add("dividend", vec![Value::Equity(x), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[3]), Value::Num(1.5)]);
    let r = run(&prog, &ds, frictionless(1000.0)).unwrap();
    assert_eq!(r.equity_curve.last().unwrap().1, 1000.0);
    diff("dividend", &prog, &ds, frictionless(1000.0)).unwrap_or_else(|e| panic!("{}", e));
    // A spin-off valued in cash: a dividend in kind of 4 a share on the ex-date.
    let (mut ds, x, days) = market(&[Some(20.0), Some(20.0), Some(16.0), Some(16.0), Some(16.0)]);
    ds.add("dividend", vec![Value::Equity(x), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[2]), Value::Num(4.0)]);
    let r = run(&prog, &ds, frictionless(1000.0)).unwrap();
    assert_eq!(r.equity_curve.last().unwrap().1, 1000.0);
    diff("spin-off", &prog, &ds, frictionless(1000.0)).unwrap_or_else(|e| panic!("{}", e));
    // A bankruptcy on day 4: the position is written off at the full haircut.
    let (mut ds, x, days) = market(&[Some(20.0), Some(20.0), Some(5.0), None, None]);
    let bankrupt = ds.labels.intern("bankruptcy");
    ds.add("delisted", vec![Value::Equity(x), Value::Time(days[3]), Value::Label(bankrupt)]);
    let r = run(&prog, &ds, frictionless(1000.0)).unwrap();
    assert_eq!(r.equity_curve.last().unwrap().1, 800.0);
    diff("delisting", &prog, &ds, frictionless(1000.0)).unwrap_or_else(|e| panic!("{}", e));
}
