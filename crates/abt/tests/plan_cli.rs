//! `abt plan`, `abt lineage` and `abt dispute` (Q-9) through the binary,
//! over a store served in the process and a kernel service beside it: a
//! plan checked (its hash, its steps, the trials it will count), run, its
//! outcomes reported from the data layer, its lineage counted and disputed,
//! and dropped; the compiler's plan of a program is `abt show plan`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(abt_tests::repo_root()).output().expect("abt runs")
}

fn text(o: &Output) -> (String, String) {
    (String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-plan-cli-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const LIBRARY: [&str; 2] = ["corpus-v2/catalog", "corpus-v2/lib"];

#[test]
fn a_plan_is_checked_run_reported_and_dropped() {
    let dir = scratch("plan");
    let store = dir.join("store").to_string_lossy().to_string();
    let mut args = vec!["data", "ingest", "--store", &store, "--adapter", "synthetic", "--class", "equity", "--days", "400"];
    args.extend(LIBRARY);
    assert_eq!(abt(&args).status.code(), Some(0));
    let program = abt_tests::repo_root().join("corpus-v2/strategies/sma_crossover.dsl");
    let plan = dir.join("sweep.plan");
    std::fs::write(
        &plan,
        format!(
            "# two speeds of the crossover\nstudy sweep {{\n  program {}\n  grid {{ fast = 10d, 20d }}\n  holdout trailing:1y\n}}\n",
            program.display()
        ),
    )
    .unwrap();
    let plan = plan.to_string_lossy().to_string();
    let with = |cmd: &[&str]| {
        let mut a: Vec<&str> = cmd.to_vec();
        a.push(&plan);
        a.extend(LIBRARY);
        a.extend(["--store", store.as_str()]);
        abt(&a)
    };
    let o = with(&["plan", "check"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.starts_with("plan sweep "), "{}", out);
    assert!(out.contains("program sma_crossover "), "{}", out);
    assert!(out.contains("s0   grid main") && out.contains("main: 2 runs, one per point"), "{}", out);
    assert!(out.contains("holdout trailing:1y"), "the normal form: {}", out);
    let hash = out.lines().next().unwrap().split_whitespace().nth(2).unwrap().to_string();
    let o = with(&["plan", "run", "--json"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    let ex: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(ex["hash"].as_str(), Some(hash.as_str()));
    assert_eq!(ex["outcomes"].as_array().map(|a| a.len()), Some(2));
    assert_eq!(ex["trials"].as_u64(), Some(2));
    let o = with(&["plan", "report"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert_eq!(out.lines().filter(|l| l.starts_with("  main ")).count(), 2, "{}", out);
    assert_eq!(out.matches("(best)").count(), 1, "{}", out);
    // The lineage: one member, two trials; a dispute is a row.
    let p = program.to_string_lossy().to_string();
    let lineage = |cmd: &[&str]| {
        let mut a: Vec<&str> = cmd.to_vec();
        a.push(&p);
        a.extend(LIBRARY);
        a.extend(["--store", store.as_str()]);
        abt(&a)
    };
    let o = lineage(&["lineage"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.contains("sma_crossover root") && out.contains("trials 2: 2 trial"), "{}", out);
    let o = lineage(&["dispute", "--reason", "a revision, not a copy"]);
    assert_eq!(o.status.code(), Some(0), "{:?}", text(&o));
    let (out, _) = text(&lineage(&["lineage"]));
    assert!(out.contains("dispute ") && out.contains("a revision, not a copy"), "{}", out);
    // Dropped, the plan has no report row in query.plans; its trials stay.
    let o = with(&["plan", "drop"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.starts_with("dropped plan sweep"), "{}", out);
    assert!(text(&lineage(&["lineage"])).0.contains("trials 2"));
    // A plan file with an unknown step is refused with its reason.
    std::fs::write(&plan, format!("study sweep {{ program {}\n steps {{ reveal r of nowhere }} }}", program.display())).unwrap();
    let o = with(&["plan", "check"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).1.contains("no step is"), "{:?}", text(&o));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_compilers_plan_is_show_plan() {
    let o = abt(&["show", "plan", "--strategy", "sma_crossover", LIBRARY[0], LIBRARY[1], "corpus-v2/strategies/sma_crossover.dsl"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.contains("sma_crossover::above"), "{}", out);
}
