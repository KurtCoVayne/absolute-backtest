//! The pandas reference (data-bundle doc, section 8): every daily corpus-v2
//! strategy under its own execution block, a set of policy and model
//! variants, and the tiny action markets, replayed through
//! `reference/engine.py` from a kernel dump and compared by
//! `reference/diff.py` (fills identical in quantity, price to floating
//! tolerance, the book and NAV within tolerance). Skipped, with a note,
//! where `python3` with pandas is not installed.

use abt_tests as common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use abt_kernel::check::Program;
use abt_kernel::data::{business_days, synthetic_daily};
use abt_kernel::dump::write_dump;
use abt_kernel::kernel::{run, Dataset, ExecConfig, OnLeverage, OnOversize, Value};

fn reference_dir() -> PathBuf {
    abt_tests::repo_root().join("reference")
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

/// The names of the synthetic market: its second name is renamed halfway
/// and its last delisted at four fifths, so the names the strategies cite
/// ("AAA", "BBB", "SPY") stay listed throughout.
const SYMS: [&str; 5] = ["AAA", "XYZ", "BBB", "SPY", "DDD"];

/// The synthetic daily market with the catalog, over `days` bars.
fn market(days: usize) -> Dataset {
    synthetic_daily(&SYMS, (2022, 1, 3), days, 11)
}

#[test]
fn every_daily_corpus_strategy_agrees_with_the_reference() {
    if !python_with_pandas() {
        eprintln!("skipped: python3 with pandas is not installed");
        return;
    }
    let mut compared = 0;
    for f in common::dsl_files("strategies") {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let prog = common::corpus_program(&name);
        if prog.base.is_sub_daily() {
            // The reference executor is written for the daily contract.
            continue;
        }
        let ds = market(400);
        // The run is the program: its execution block configures the executor.
        let cfg = ExecConfig::from_program(&prog);
        // A run the kernel's policies halt is the policy tests' business;
        // the comparison wants completed runs.
        if let Err(abt_kernel::kernel::RunError::Risk { message, .. }) = run(&prog, &ds, cfg.clone()) {
            eprintln!("{}: halted ({}), not compared", name, message);
            continue;
        }
        diff(&name, &prog, &ds, cfg).unwrap_or_else(|e| panic!("{}", e));
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
    let ds = market(400);
    let lsq = common::corpus_program("long_short_quantile");
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
    let top = common::corpus_program("momentum_top_n");
    diff(
        "compounding-long",
        &top,
        &ds,
        // Three equal weights of equity, filled at a cost, are leverage by
        // a hair under compounding: rejected, so the comparison completes.
        ExecConfig {
            compounding: true,
            on_leverage: OnLeverage::Reject,
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
            on_margin_call: abt_kernel::kernel::OnMarginCall::Liquidate,
            ..ExecConfig::reg_t()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    let spike = common::corpus_program("volume_spike");
    let long = market(600);
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
    let monthly = common::corpus_program("monthly_rebalance");
    diff(
        "fractional",
        &monthly,
        &ds,
        ExecConfig {
            lot: abt_kernel::kernel::Lot::Fractional,
            ..ExecConfig::default()
        },
    )
    .unwrap_or_else(|e| panic!("{}", e));
    let pairs = common::corpus_program("pairs_trading");
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

/// A daily strategy that buys ten shares of every listed name it does not
/// hold; it reads the actions, so the executor applies them to the book.
fn hold_program() -> Program {
    let src = common::StrategyParts {
        name: "hold".into(),
        needs: "need bars @1d   need actions, listing".into(),
        params: "    qty : Quantity<Shares> = 10 shares".into(),
        decides: "    decide(T, trade(A, qty)) :- universe(A, T), flat(A, T).".into(),
        ..Default::default()
    }
    .render();
    common::program(&src, "hold")
}

/// One name `X` over consecutive weekdays from 2024-01-08: a bar (open,
/// high and low at the close, volume a million) and the listing on every
/// day with a close, nothing on a day without one.
fn market_x(closes: &[Option<f64>]) -> (Dataset, u32, Vec<i64>) {
    let days = business_days((2024, 1, 8), closes.len());
    let mut ds = Dataset::new();
    let x = ds.intern("X");
    for (t, p) in days.iter().zip(closes) {
        if let Some(p) = p {
            common::add_bar(&mut ds, x, *t, *p, 1_000_000.0);
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

/// The four action markets, each with the NAV the hold strategy ends at
/// from 1000 of cash: a 3-for-1 split on day 3 (10 shares at 30 become 30
/// at 10; NAV 300 throughout), a dividend of 1.5 announced day 1, ex day 3,
/// paid day 4 (10 shares earn 15), a spin-off valued in cash (a dividend in
/// kind of 4 a share on the ex-date), and a bankruptcy on day 4 (the
/// position is written off at the full haircut).
fn action_markets() -> Vec<(&'static str, Dataset, u32, f64)> {
    let (mut split, x, days) = market_x(&[Some(30.0), Some(30.0), Some(10.0), Some(10.0), Some(10.0)]);
    split.add("split", vec![Value::Equity(x), Value::Time(days[2]), Value::Num(3.0)]);
    let (mut dividend, x, days) = market_x(&[Some(20.0), Some(20.0), Some(18.5), Some(18.5), Some(18.5)]);
    dividend.add("dividend", vec![Value::Equity(x), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[3]), Value::Num(1.5)]);
    let (mut spin_off, x, days) = market_x(&[Some(20.0), Some(20.0), Some(16.0), Some(16.0), Some(16.0)]);
    spin_off.add("dividend", vec![Value::Equity(x), Value::Time(days[0]), Value::Time(days[2]), Value::Time(days[2]), Value::Num(4.0)]);
    let (mut delisting, x, days) = market_x(&[Some(20.0), Some(20.0), Some(5.0), None, None]);
    let bankrupt = delisting.labels.intern("bankruptcy");
    delisting.add("delisted", vec![Value::Equity(x), Value::Time(days[3]), Value::Label(bankrupt)]);
    vec![
        ("split", split, x, 1000.0),
        ("dividend", dividend, x, 1000.0),
        ("spin-off", spin_off, x, 1000.0),
        ("delisting", delisting, x, 800.0),
    ]
}

#[test]
fn the_action_markets_end_at_their_hand_computed_nav() {
    let prog = hold_program();
    for (tag, ds, x, nav) in action_markets() {
        let r = run(&prog, &ds, frictionless(1000.0)).unwrap_or_else(|e| panic!("{}: {}", tag, e));
        assert_eq!(r.equity_curve.last().unwrap().1, nav, "{}", tag);
        if tag == "split" {
            assert_eq!(r.final_positions.get(&x), Some(&30.0));
        }
    }
}

#[test]
fn the_action_markets_agree_with_the_reference() {
    if !python_with_pandas() {
        eprintln!("skipped: python3 with pandas is not installed");
        return;
    }
    let prog = hold_program();
    for (tag, ds, _, _) in action_markets() {
        diff(tag, &prog, &ds, frictionless(1000.0)).unwrap_or_else(|e| panic!("{}", e));
    }
}
