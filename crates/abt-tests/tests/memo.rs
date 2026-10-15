//! Memo eviction (src/kernel/mod.rs `evict_memo`, run by the fold at every
//! decision bar): forced at every bar, it changes no decision, fill or cash
//! figure of any daily corpus strategy.

#![allow(clippy::result_large_err)]

use abt_tests as common;

use abt_kernel::data::synthetic_daily;
use abt_kernel::kernel::ExecConfig;

#[test]
fn forced_eviction_changes_no_result() {
    // The daily market renames its second name and delists its last; the
    // names the strategies cite ("AAA", "BBB", "SPY") stay listed.
    let ds = synthetic_daily(&["AAA", "XYZ", "BBB", "SPY", "DDD"], (2022, 1, 3), 300, 3);
    let mut checked = 0;
    for f in common::dsl_files("strategies") {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&f).unwrap();
        if !src.contains("bars @1d") {
            continue;
        }
        let p = common::program(&src, &name);
        let client = common::client(&p, &ds);
        let run = |force: bool| {
            let opts = abt_kernel::kernel::Replay {
                force_memo_eviction: force,
                ..Default::default()
            };
            abt_kernel::kernel::run_on(&p, None, &client, ExecConfig::frictionless(), &opts)
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
