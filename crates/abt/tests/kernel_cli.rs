//! `abt kernel` (P-3.4) through the binary, as three processes: a data
//! service over a store, a kernel service reaching it, and the client
//! commands that check, run, query and inspect a run the kernel holds.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(abt_tests::repo_root()).output().expect("abt runs")
}

fn text(o: &Output) -> (String, String) {
    (String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-kernel-cli-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn serve(args: &[&str], socket: &Path) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_abt"))
        .args(args)
        .current_dir(abt_tests::repo_root())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..400 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    child
}

const FILES: [&str; 3] = ["corpus-v2/catalog", "corpus-v2/lib", "corpus-v2/strategies/sma_crossover.dsl"];

#[test]
fn a_kernel_service_runs_and_answers_over_a_data_service() {
    let dir = scratch("three");
    let store = dir.join("store").to_string_lossy().to_string();
    let mut args = vec!["data", "ingest", "--store", &store, "--adapter", "synthetic", "--class", "equity", "--days", "130"];
    args.extend(&FILES[..2]);
    assert_eq!(abt(&args).status.code(), Some(0));
    let data = dir.join("d.sock");
    let kernel = dir.join("k.sock");
    let (d, k) = (data.to_string_lossy().to_string(), kernel.to_string_lossy().to_string());
    let mut data_service = serve(&["data", "serve", "--store", &store, "--socket", &d], &data);
    let mut kernel_service = serve(&["kernel", "serve", "--socket", &k, "--data", &d], &kernel);

    let with = |cmd: &[&str]| {
        let mut a = vec!["kernel"];
        a.extend(cmd);
        a.extend(["--kernel", k.as_str()]);
        abt(&a)
    };
    let o = with(&["check", "--strategy", "sma_crossover", FILES[0], FILES[1], FILES[2]]);
    assert!(o.status.success() && text(&o).0.starts_with("sma_crossover "), "{:?}", text(&o));
    let o = with(&["run", "--strategy", "sma_crossover", "--progress", "50", FILES[0], FILES[1], FILES[2]]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{}\n{}", out, err);
    let run = out.lines().next().and_then(|l| l.strip_prefix("run ")).unwrap_or_else(|| panic!("{}", out)).to_string();
    assert!(out.contains(&format!("{}: completed", run)), "{}", out);
    assert_eq!(err.lines().filter(|l| l.starts_with("barrier ")).count(), 2, "{}", err);
    // The run in process over the same data decides the same.
    let local = abt(&["run", "--strategy", "sma_crossover", "--synthetic", "--days", "130", "--quiet", FILES[0], FILES[1], FILES[2]]);
    let decisions = text(&local)
        .0
        .lines()
        .find_map(|l| l.strip_prefix("decisions: "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_string();
    assert!(out.contains(&format!("\"decisions\":{}", decisions)), "{}", out);

    let o = with(&["query", "--run", &run, "--text", "? decide(T, D) at T"]);
    assert!(text(&o).0.contains(&format!("{} row(s)", decisions)), "{:?}", text(&o));
    let o = with(&["query", "--run", &run, "--text", "show calendars"]);
    assert!(text(&o).0.contains("base"), "{:?}", text(&o));
    let o = with(&["status", "--run", &run]);
    assert!(text(&o).0.contains("completed"), "{:?}", text(&o));
    let o = with(&["control", "--run", &run, "halt"]);
    assert!(!o.status.success() && text(&o).1.contains("not running"), "{:?}", text(&o));
    // Its outputs are in the store.
    let o = abt(&["data", "query", "--store", &store, &format!("kernel.{}.report", run)]);
    assert!(text(&o).0.contains("\"completed\""), "{:?}", text(&o));
    // Checkpoints through the data layer, each named by its key; a run from
    // one of them is the same run, and ends as it did (K-4.7).
    let o = with(&["run", "--strategy", "sma_crossover", "--checkpoint-every", "month", FILES[0], FILES[1], FILES[2]]);
    let (again, err) = text(&o);
    assert!(o.status.success(), "{}\n{}", again, err);
    let keys: Vec<&str> = err.lines().filter_map(|l| l.strip_prefix("checkpoint ")?.split_whitespace().next()).collect();
    assert!(keys.len() >= 3 && keys.iter().all(|k| k.len() == 64), "{}", err);
    let o = with(&["run", "--strategy", "sma_crossover", "--resume", keys[keys.len() / 2], FILES[0], FILES[1], FILES[2]]);
    let (resumed, err) = text(&o);
    assert!(o.status.success(), "{}\n{}", resumed, err);
    assert!(resumed.starts_with(&format!("run {}", run)), "{}", resumed);
    assert!(resumed.contains(&format!("\"decisions\":{}", decisions)), "{}", resumed);
    // `abt run` over the data service: checkpoints as cache objects, each
    // printed with its key; a run resumed from one, taking the same
    // checkpoints again, ends where the unbroken run ends.
    let run_over = |extra: &[&str]| {
        let mut a = vec!["run", "--strategy", "sma_crossover", "--data", d.as_str(), "--quiet"];
        a.extend(extra);
        a.extend(FILES);
        abt(&a)
    };
    let full = run_over(&["--checkpoint-every", "month"]);
    let (out, err) = text(&full);
    assert!(full.status.success(), "{}\n{}", out, err);
    let keys: Vec<&str> = err.lines().filter_map(|l| l.strip_prefix("checkpoint ")?.split_whitespace().next()).collect();
    assert!(keys.len() >= 3 && keys.iter().all(|k| k.len() == 64), "{}", err);
    let again = run_over(&["--checkpoint-every", "month", "--resume", keys[1]]);
    let (rout, rerr) = text(&again);
    assert!(again.status.success(), "{}\n{}", rout, rerr);
    assert!(rerr.contains(&format!("resuming from {}", keys[1])), "{}", rerr);
    let final_line = |s: &str| s.lines().find(|l| l.starts_with("final cash")).unwrap_or_default().to_string();
    assert!(!final_line(&out).is_empty(), "{}", out);
    assert_eq!(final_line(&out), final_line(&rout));

    let _ = kernel_service.kill();
    let _ = data_service.kill();
    let _ = kernel_service.wait();
    let _ = data_service.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
