//! Live-sim through the binary, across processes (K-6.2, D-5.4, D-5.5):
//! `abt data serve --record synthetic` records a session on a clock running
//! 240 times the wall clock while it serves; `abt run --mode live-sim` reads
//! the session as its bars arrive, and `abt run --replay-of RUN` replays the
//! stream that run recorded: the two decide and fill alike (G4).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).current_dir(abt_tests::repo_root()).output().expect("abt runs")
}

fn text(o: &Output) -> (String, String) {
    (String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-live-cli-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const STRATEGY: &str = "strategy g4 {
  data { class equity   need bars @1m   allow unreviewed   prefer latest }
  uses calendar, bars, book
  clock { decide on base }
  universe { universe(A, T) :- traded@base(A, T). }
  params {
    qty : Quantity<Shares> = 10 shares
  }
  rules {
    rel up(A: Equity, @T)
    up(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P > P0.
    rel down(A: Equity, @T)
    down(A, T) :- universe(A, T), close(A, T, P), prev(T, T0), close(A, T0, P0), P <= P0.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- up(A, T), flat(A, T).
    decide(T, trade(A, Q)) :- down(A, T), held(A, T, Q0), Q = -Q0.
  }
  execution equity_default
  report { calendar base   periods 98280 per year }
}
";

/// The lines of a run's summary that hold what it decided and where it
/// ended.
fn outcome(o: &Output) -> Vec<String> {
    text(o).0.lines().filter(|l| l.starts_with("decisions: ") || l.starts_with("final cash")).map(str::to_string).collect()
}

fn run_id(o: &Output) -> String {
    let out = text(o).0;
    let line = out.lines().find(|l| l.starts_with("run ")).unwrap_or_else(|| panic!("no run line: {}", out));
    line.trim_start_matches("run ").split(':').next().unwrap().to_string()
}

#[test]
fn a_live_sim_run_and_the_replay_of_its_recording_decide_alike() {
    let dir = scratch("g4");
    let store = dir.join("store").to_string_lossy().to_string();
    let socket = dir.join("data.sock");
    let sock = socket.to_string_lossy().to_string();
    let strategy = dir.join("g4.dsl");
    std::fs::write(&strategy, STRATEGY).unwrap();
    let strategy = strategy.to_string_lossy().to_string();
    let mut server = Command::new(env!("CARGO_BIN_EXE_abt"))
        .args([
            "data",
            "serve",
            "--store",
            &store,
            "--socket",
            &sock,
            "--record",
            "synthetic",
            "--session",
            "s1",
            "--symbols",
            "AAA,BBB",
            "--start",
            "2024-03-11T14:31:00",
            "--end",
            "2024-03-11T14:40:00",
            "--clock-rate",
            "240",
            "--grace",
            "base=20",
        ])
        .current_dir(abt_tests::repo_root())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..400 {
        if Path::new(&socket).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    // The clocks of the two processes differ (the service's runs fast): no
    // deadline is measured.
    let files = ["corpus-v2/catalog", "corpus-v2/lib", strategy.as_str()];
    let mut live_args = vec!["run", "--strategy", "g4", "--mode", "live-sim", "--session", "s1", "--deadline", "none", "--data", &sock, "--quiet"];
    live_args.extend(files);
    let live = abt(&live_args);
    assert_eq!(live.status.code(), Some(0), "{}\n{}", text(&live).0, text(&live).1);
    let run = run_id(&live);
    let mut replay_args = vec!["run", "--strategy", "g4", "--replay-of", &run, "--data", &sock, "--quiet"];
    replay_args.extend(files);
    let replay = abt(&replay_args);
    let wrong = abt(&["run", "--strategy", "g4", "--mode", "live-sim", "--data", &sock, "--quiet", files[0], files[1], files[2]]);
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(replay.status.code(), Some(0), "{}\n{}", text(&replay).0, text(&replay).1);
    assert_ne!(run_id(&replay), run);
    assert_eq!(outcome(&live).len(), 2, "{}", text(&live).0);
    assert_eq!(outcome(&live), outcome(&replay));
    // A live-sim run names its session.
    assert_eq!(wrong.status.code(), Some(2));
    assert!(text(&wrong).1.contains("--session"), "{}", text(&wrong).1);
    let _ = std::fs::remove_dir_all(&dir);
}
