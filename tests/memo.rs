//! Memo eviction (src/kernel/mod.rs `evict_memo`): forced at every bar, it
//! changes no decision, fill or cash figure of any daily corpus strategy.

#![allow(clippy::result_large_err)]

mod corpus;

use std::fs;
use std::path::Path;

use absolute_backtest::check::check_program;
use absolute_backtest::data::{synthetic_daily, synthetic_daily_v2};
use absolute_backtest::kernel::{ExecConfig, Kernel};

#[test]
fn forced_eviction_changes_no_result() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/strategies");
    let v1 = synthetic_daily(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 300, 3);
    let v2 = synthetic_daily_v2(&["AAA", "BBB", "CCC", "DDD", "SPY"], (2022, 1, 3), 300, 3);
    let mut checked = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let src = fs::read_to_string(entry.unwrap().path()).unwrap();
        if !src.contains("resolution @1d") {
            continue;
        }
        let name = src.split("strategy ").nth(1).unwrap().split_whitespace().next().unwrap().to_string();
        let mut ws = corpus::base_workspace();
        ws.add_source(&src).unwrap();
        let (Some(p), _) = check_program(&ws, &name) else { continue };
        let ds = if src.contains("equities_1d_v2") { &v2 } else { &v1 };
        let run = |force: bool| {
            let mut k = Kernel::new(&p, ds, ExecConfig::frictionless()).unwrap();
            if force {
                k.force_memo_eviction();
            }
            k.run()
        };
        let (a, b) = (run(false), run(true));
        match (a, b) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a.decisions.len(), b.decisions.len(), "{}", name);
                assert_eq!(a.fills.len(), b.fills.len(), "{}", name);
                assert_eq!(a.final_cash.to_bits(), b.final_cash.to_bits(), "{}", name);
                checked += 1;
            }
            (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{}", name),
            (a, b) => panic!("{}: {:?} vs {:?}", name, a.map(|r| r.final_cash), b.map(|r| r.final_cash)),
        }
    }
    assert!(checked >= 10, "{} strategies checked", checked);
}
