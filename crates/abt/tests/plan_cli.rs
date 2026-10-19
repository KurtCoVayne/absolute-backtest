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
    // The report the default steps' `report` wrote: its tables, the forty
    // biases among them.
    let o = with(&["plan", "report"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.contains("\nsummary\n") && out.contains("\nbiases\n"), "{}", out);
    let summary: Vec<&str> = out.lines().skip_while(|l| *l != "summary").skip(2).take_while(|l| l.starts_with("  ")).collect();
    assert_eq!(summary.len(), 2, "{}", out);
    assert!(summary.iter().all(|l| l.starts_with("  main ")), "{:?}", summary);
    assert!(out.contains("look-ahead") && out.contains("tail-risk neglect"), "{}", out);
    let o = with(&["plan", "report", "--json"]);
    assert_eq!(text(&o).0.matches("\"best\":true").count(), 1, "{:?}", text(&o));
    // One test and one claim outside the plan, on one of its runs, each a
    // counted trial; a feed statistic and the activation of the run.
    // The point that traded (a run that never trades has constant returns,
    // which a test refuses).
    let traded = ex["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["returns"]["sharpe"].as_f64().is_some_and(|s| s != 0.0))
        .expect("a point that traded");
    let run_id = traded["run_id"].as_str().unwrap().to_string();
    let q = |cmd: &[&str]| {
        let mut a: Vec<&str> = cmd.to_vec();
        a.extend(["--store", store.as_str()]);
        abt(&a)
    };
    let returns = format!("returns of {}", run_id);
    let mut test = vec!["test", "ljung_box", "on"];
    test.extend(returns.split(' '));
    test.extend(["--with", "lags=5"]);
    let o = q(&test);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.starts_with("ljung_box@") && out.contains("statistic ") && out.contains("counted in lineage"), "{}", out);
    let claim = "positive : nw_mean on returns of the_run expect statistic > -1000";
    let o = q(&["claim", claim, "--run", &run_id]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.contains("verdict: holds"), "{}", out);
    let o = q(&["feed", "coverage", "--run", &run_id]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.starts_with("coverage\n") && out.contains("close"), "{}", out);
    let o = q(&["activation", "--run", &run_id]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.contains("activation.rules") && out.contains("activation.effective"), "{}", out);
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
    assert!(out.contains("sma_crossover root") && out.contains("trials 4: 1 claim, 1 test, 2 trial"), "{}", out);
    let o = lineage(&["dispute", "--reason", "a revision, not a copy"]);
    assert_eq!(o.status.code(), Some(0), "{:?}", text(&o));
    let (out, _) = text(&lineage(&["lineage"]));
    assert!(out.contains("dispute ") && out.contains("a revision, not a copy"), "{}", out);
    // Dropped, the plan has no report row in query.plans; its trials stay.
    let o = with(&["plan", "drop"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out, err);
    assert!(out.starts_with("dropped plan sweep"), "{}", out);
    assert!(text(&lineage(&["lineage"])).0.contains("trials 4"));
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

#[test]
fn a_plan_check_prints_the_after_and_percentage_hold_outs() {
    let dir = scratch("holdouts");
    let store = dir.join("store").to_string_lossy().to_string();
    let mut args = vec!["data", "ingest", "--store", &store, "--adapter", "synthetic", "--class", "equity", "--days", "400"];
    args.extend(LIBRARY);
    assert_eq!(abt(&args).status.code(), Some(0));
    let program = abt_tests::repo_root().join("corpus-v2/strategies/sma_crossover.dsl");
    let plan = dir.join("holdouts.plan");
    let plan_text = plan.to_string_lossy().to_string();
    let check = |holdout: &str| {
        std::fs::write(
            &plan,
            format!(
                "study h {{\n  program {}\n  holdout {}\n  steps {{ run main\n reveal later of main }}\n}}\n",
                program.display(),
                holdout
            ),
        )
        .unwrap();
        let mut a = vec!["plan", "check", plan_text.as_str()];
        a.extend(LIBRARY);
        a.extend(["--store", store.as_str()]);
        abt(&a)
    };
    // The forms of E-93, each in the normal form as one text.
    for (written, shown) in [
        ("after 2023-01-02", "holdout after 2023-01-02"),
        ("after 2023-01-02T09:30:00.000000001", "holdout after 2023-01-02T09:30:00.000000001"),
        ("trailing:20%", "holdout trailing:20%"),
        ("trailing:6mo", "holdout trailing:6mo"),
        ("trailing:24mo", "holdout trailing:2y"),
    ] {
        let o = check(written);
        let (out, err) = text(&o);
        assert_eq!(o.status.code(), Some(0), "{}: {}\n{}", written, out, err);
        assert!(out.contains(shown), "{}: {}", written, out);
    }
    let o = check("after soon");
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).1.contains("not a time"), "{:?}", text(&o));
    std::fs::remove_dir_all(&dir).unwrap();
}
