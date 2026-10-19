//! The three-process deployment (docs/spec/00-architecture.md, 7.1; the gate
//! of 05-migration's M-9): the data layer, the kernel and the query layer as
//! three programs on sockets (`abt-data serve`, `abt-kernel serve`,
//! `abt-query serve`, each the layer's own binary, Definition A-2.1) give
//! the runs the front gives in one process. A plan run through the query
//! service, and a run through the kernel service, equal the same commands in
//! process: the plan's execution but for its instant, and every run's
//! outputs byte for byte (G3, Proposition P-5.1, across processes).

use abt_tests as common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

fn abt(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_abt")).args(args).output().expect("abt runs")
}

/// A layer's program, built beside the front's binary by `cargo test
/// --workspace` (`cargo build -p abt-data --bin abt-data` builds one alone).
fn layer(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_abt")).with_file_name(name);
    assert!(path.exists(), "{} is not built: cargo build -p {} --bin {}", path.display(), name, name);
    path
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn ok(o: Output, what: &str) -> String {
    assert!(o.status.success(), "{}: {}\n{}", what, stdout(&o), String::from_utf8_lossy(&o.stderr));
    stdout(&o)
}

/// The services, stopped when dropped (a failed assertion included).
struct Services(Vec<Child>);

impl Services {
    fn start(&mut self, binary: &str, args: &[&str], socket: &Path) {
        let child = Command::new(layer(binary))
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{} serves: {}", binary, e));
        self.0.push(child);
        for _ in 0..200 {
            if socket.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("{} never listened", socket.display());
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A fresh directory with a short path: a socket's must be under 108 bytes.
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abt-deploy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The plan's execution as JSON, without the instant it ran at.
fn execution(text: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{}: {}", e, text));
    v.as_object_mut().expect("an execution").remove("at");
    v
}

#[test]
fn three_processes_give_the_runs_the_front_gives_in_one() {
    let dir = scratch();
    let lib = [common::corpus_dir().join("catalog"), common::corpus_dir().join("lib")].map(|p| p.to_string_lossy().to_string());
    let program = common::corpus_dir().join("strategies/sma_crossover.dsl").to_string_lossy().to_string();
    let path = |name: &str| dir.join(name).to_string_lossy().to_string();
    let (served, here) = (path("served"), path("here"));
    // Two stores of one synthetic market.
    for store in [&served, &here] {
        ok(
            Command::new(layer("abt-data"))
                .args(["ingest", "--store", store, "--adapter", "synthetic", "--class", "equity", "--days", "500"])
                .output()
                .expect("abt-data runs"),
            "ingest",
        );
    }
    let plan = path("sweep.plan");
    std::fs::write(&plan, format!("study sweep {{\n  program {}\n  grid {{ fast = 10d, 20d }}\n  holdout trailing:1y\n}}\n", program)).unwrap();
    let files = [lib[0].as_str(), lib[1].as_str(), program.as_str()];

    // The three services over the first store, and the commands asked of
    // them.
    let (data_sock, kernel_sock, query_sock) = (path("d.sock"), path("k.sock"), path("q.sock"));
    let (plan_served, run_served) = {
        let mut services = Services(Vec::new());
        services.start("abt-data", &["serve", "--store", &served, "--socket", &data_sock], Path::new(&data_sock));
        services.start("abt-kernel", &["serve", "--socket", &kernel_sock, "--data", &data_sock], Path::new(&kernel_sock));
        services.start("abt-query", &["serve", "--socket", &query_sock, "--data", &data_sock, "--kernel", &kernel_sock], Path::new(&query_sock));
        // The query layer's own program asks its service; the front asks it
        // the same way.
        let plan_out = ok(
            Command::new(layer("abt-query"))
                .args(["plan", "run", &plan, &lib[0], &lib[1], "--query", &query_sock, "--json"])
                .output()
                .expect("abt-query runs"),
            "plan run through the query service",
        );
        let plan_front = ok(
            abt(&["plan", "run", &plan, &lib[0], &lib[1], "--query", &query_sock, "--json"]),
            "plan run through the query service, by the front",
        );
        assert_eq!(execution(&plan_out), execution(&plan_front), "the front and abt-query ask the service alike");
        let mut args = vec!["kernel", "run", "--kernel", kernel_sock.as_str(), "--strategy", "sma_crossover"];
        args.extend(files);
        let run_out = ok(
            Command::new(layer("abt-kernel")).args(&args).output().expect("abt-kernel runs"),
            "kernel run through the kernel service",
        );
        (plan_out, run_out)
    };

    // The front in one process over the second store.
    let plan_here = ok(abt(&["plan", "run", &plan, &lib[0], &lib[1], "--store", &here, "--json"]), "plan run in process");
    let mut args = vec!["run", "--strategy", "sma_crossover", "--data", here.as_str(), "--quiet"];
    args.extend(files);
    let run_here = ok(abt(&args), "run in process");

    // The plan's execution: the same runs, outcomes, statistics and report.
    let (a, b) = (execution(&plan_served), execution(&plan_here));
    assert_eq!(a, b, "the plan's execution through the services and in process");
    let mut runs: Vec<String> = a["outcomes"]
        .as_array()
        .expect("outcomes")
        .iter()
        .map(|o| o["run_id"].as_str().expect("a run id").to_string())
        .collect();
    assert_eq!(runs.len(), 2, "{}", plan_served);

    // The single run: one id, through the kernel service or in process.
    // `run <id>` (the kernel service) or `run <id>: ...` (in process).
    let id_of = |text: &str| {
        text.lines()
            .filter_map(|l| l.strip_prefix("run "))
            .map(|r| r.chars().take_while(|c| c.is_ascii_hexdigit()).collect::<String>())
            .find(|id| id.len() == 64)
    };
    let single = id_of(&run_served).unwrap_or_else(|| panic!("no run id in {}", run_served));
    assert_eq!(id_of(&run_here).as_deref(), Some(single.as_str()), "{}", run_here);
    runs.push(single);

    // Every run's outputs as the data layer answers them. A relation keyed
    // by time and an entity or the run's own seq is answered in the same
    // order from any store: byte for byte. One keyed by a label is ordered
    // by the store's label codes (ERRATA E-32), which two stores written in
    // a different interleaving need not share: the same rows.
    let sorted = |s: &str| {
        let mut l: Vec<&str> = s.lines().collect();
        l.sort_unstable();
        l.join("\n")
    };
    for run in &runs {
        for relation in ["events", "book", "holdings", "decisions", "fills", "coverage", "contract", "binding", "plans"] {
            let rel = format!("kernel.{}.{}", run, relation);
            let a = ok(abt(&["data", "query", "--store", &served, &rel]), &rel);
            let b = ok(abt(&["data", "query", "--store", &here, &rel]), &rel);
            // Every run has a book and a binding; a grid point may decide
            // nothing (`fast = 10d` cannot hold its 12 rows).
            if matches!(relation, "book" | "binding") {
                assert!(a.lines().count() > 2, "{} is empty", rel);
            }
            if matches!(relation, "events" | "book" | "holdings" | "decisions" | "fills") {
                assert_eq!(a, b, "{} through the services and in process", rel);
            } else {
                assert_eq!(sorted(&a), sorted(&b), "{} through the services and in process", rel);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
