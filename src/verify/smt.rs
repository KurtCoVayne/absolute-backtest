//! Tier 3's interface (compiler design, section 6.4): the crate emits
//! SMT-LIB2 and drives a solver as a subprocess, with a replay file per
//! obligation so that any verdict can be reproduced from the file alone,
//! and no link against a solver library. Solver absence yields *undecided*
//! for every obligation and a note in the report. Budgets: one per
//! obligation and one per compilation, both from the compile options.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::compiler::CompileOptions;

/// An SMT-LIB2 script: a logic, declarations, assertions and the closing
/// commands, rendered as text that a solver reads from a file.
#[derive(Clone, Debug)]
pub struct Script {
    pub logic: String,
    /// Comment lines at the head of the file (the obligation, the meaning
    /// of each answer).
    pub header: Vec<String>,
    decls: Vec<String>,
    declared: std::collections::BTreeSet<String>,
    asserts: Vec<String>,
    /// Commands after `(check-sat)`: `(get-model)` for a certificate.
    pub tail: Vec<String>,
}

impl Script {
    pub fn new(logic: &str) -> Script {
        Script {
            logic: logic.into(),
            header: Vec::new(),
            decls: Vec::new(),
            declared: Default::default(),
            asserts: Vec::new(),
            tail: Vec::new(),
        }
    }
    pub fn comment(&mut self, line: impl Into<String>) {
        self.header.push(line.into());
    }
    pub fn declare(&mut self, name: &str, sort: &str) {
        if self.declared.insert(name.to_string()) {
            self.decls.push(format!("(declare-const {} {})", name, sort));
        }
    }
    pub fn declare_fun(&mut self, name: &str, args: &[&str], sort: &str) {
        if self.declared.insert(name.to_string()) {
            self.decls.push(format!("(declare-fun {} ({}) {})", name, args.join(" "), sort));
        }
    }
    pub fn declared(&self, name: &str) -> bool {
        self.declared.contains(name)
    }
    pub fn assert(&mut self, term: impl Into<String>) {
        self.asserts.push(term.into());
    }
    pub fn assertions(&self) -> &[String] {
        &self.asserts
    }
    pub fn declarations(&self) -> &[String] {
        &self.decls
    }
    /// The file's text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for h in &self.header {
            out.push_str("; ");
            out.push_str(h);
            out.push('\n');
        }
        out.push_str(&format!("(set-logic {})\n", self.logic));
        for d in &self.decls {
            out.push_str(d);
            out.push('\n');
        }
        for a in &self.asserts {
            out.push_str(&format!("(assert {})\n", a));
        }
        out.push_str("(check-sat)\n");
        for t in &self.tail {
            out.push_str(t);
            out.push('\n');
        }
        out.push_str("(exit)\n");
        out
    }
}

/// What the solver answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SolverAnswer {
    Sat,
    Unsat,
    Unknown,
    /// The per-obligation budget passed; the process was stopped.
    Timeout,
    /// The compilation budget was spent before this obligation.
    BudgetSpent,
    /// No solver named.
    Absent,
    /// The solver could not be run, or rejected the script.
    Error(String),
}

impl SolverAnswer {
    pub fn text(&self) -> String {
        match self {
            SolverAnswer::Sat => "sat".into(),
            SolverAnswer::Unsat => "unsat".into(),
            SolverAnswer::Unknown => "unknown".into(),
            SolverAnswer::Timeout => "timeout".into(),
            SolverAnswer::BudgetSpent => "compilation budget spent".into(),
            SolverAnswer::Absent => "no solver".into(),
            SolverAnswer::Error(e) => format!("solver error: {}", e),
        }
    }
}

/// One solver run: the answer, its cost, the replay file and the solver's
/// output (a model for a certificate).
#[derive(Clone, Debug)]
pub struct Outcome {
    pub answer: SolverAnswer,
    pub elapsed_ms: u64,
    pub replay_file: Option<String>,
    pub output: String,
}

/// The solver driver: which program, what budgets, where the replay files go.
#[derive(Debug)]
pub struct Driver {
    pub solver: Option<String>,
    pub budget_ms: u64,
    pub total_budget_ms: u64,
    spent_ms: Cell<u64>,
    pub replay_dir: Option<PathBuf>,
    /// The replay files' name prefix (the strategy).
    pub prefix: String,
}

impl Driver {
    pub fn new(solver: Option<String>, budget_ms: u64, total_budget_ms: u64, replay_dir: Option<PathBuf>, prefix: &str) -> Driver {
        Driver {
            solver: solver.filter(|s| !s.is_empty()),
            budget_ms,
            total_budget_ms,
            spent_ms: Cell::new(0),
            replay_dir,
            prefix: sanitise(prefix),
        }
    }
    pub fn from_options(opts: &CompileOptions, prefix: &str) -> Driver {
        Driver::new(opts.solver.clone(), opts.smt_budget_ms, opts.compile_budget_ms, opts.replay_dir.clone(), prefix)
    }
    /// The same driver with another per-obligation budget (the CHC tier's).
    pub fn with_budget(&self, budget_ms: u64) -> Driver {
        Driver {
            solver: self.solver.clone(),
            budget_ms,
            total_budget_ms: self.total_budget_ms,
            spent_ms: Cell::new(self.spent_ms.get()),
            replay_dir: self.replay_dir.clone(),
            prefix: self.prefix.clone(),
        }
    }
    pub fn spent_ms(&self) -> u64 {
        self.spent_ms.get()
    }
    /// The report's note when no solver is named.
    pub fn absent_note(&self) -> Option<String> {
        if self.solver.is_none() {
            Some(match &self.replay_dir {
                Some(d) => format!(
                    "no solver named: every obligation is undecided; the scripts are in {} (run them with z3 or cvc5, or name --solver)",
                    d.display()
                ),
                None => "no solver named: every obligation is undecided (name --solver z3 or --solver cvc5, on the PATH; --replay-dir DIR keeps the scripts)".into(),
            })
        } else {
            None
        }
    }

    /// Run a script under the per-obligation budget. The script is written
    /// to its replay file (or a scratch file when no directory is named),
    /// the solver reads that file, and its output is kept.
    pub fn run(&self, id: &str, script: &Script) -> Outcome {
        let file_name = format!("{}-{}.smt2", self.prefix, sanitise(id));
        let (dir, keep) = match &self.replay_dir {
            Some(d) => (d.clone(), true),
            None => (std::env::temp_dir().join(format!("abt-verify-{}", std::process::id())), false),
        };
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(&file_name);
        let out_path = dir.join(format!("{}-{}.out", self.prefix, sanitise(id)));
        let replay_file = keep.then(|| path.display().to_string());
        if let Err(e) = fs::write(&path, script.text()) {
            return Outcome {
                answer: SolverAnswer::Error(format!("cannot write {}: {}", path.display(), e)),
                elapsed_ms: 0,
                replay_file,
                output: String::new(),
            };
        }
        let Some(solver) = &self.solver else {
            if !keep {
                let _ = fs::remove_file(&path);
            }
            return Outcome {
                answer: SolverAnswer::Absent,
                elapsed_ms: 0,
                replay_file,
                output: String::new(),
            };
        };
        if self.spent_ms.get() >= self.total_budget_ms {
            return Outcome {
                answer: SolverAnswer::BudgetSpent,
                elapsed_ms: 0,
                replay_file,
                output: String::new(),
            };
        }
        let budget = self.budget_ms.min(self.total_budget_ms - self.spent_ms.get()).max(1);
        let (answer, elapsed, output) = run_solver(solver, &path, &out_path, budget);
        self.spent_ms.set(self.spent_ms.get() + elapsed);
        if !keep {
            let _ = fs::remove_file(&path);
        }
        let _ = fs::remove_file(&out_path);
        Outcome {
            answer,
            elapsed_ms: elapsed,
            replay_file,
            output,
        }
    }

    /// Keep a certificate (a model) next to the replay file.
    pub fn keep_certificate(&self, id: &str, text: &str) -> Option<String> {
        let dir = self.replay_dir.as_ref()?;
        let path = dir.join(format!("{}-{}.model", self.prefix, sanitise(id)));
        fs::write(&path, text).ok()?;
        Some(path.display().to_string())
    }
}

/// The solver's command line: the file as the input, the budget as the
/// solver's own soft timeout where it has one (the process is stopped at
/// the budget regardless).
fn command(solver: &str, file: &Path, budget_ms: u64) -> Command {
    let mut c = Command::new(solver);
    let base = Path::new(solver).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    match base.as_str() {
        "z3" => {
            c.arg(format!("-t:{}", budget_ms));
            c.arg("-smt2");
            c.arg(file);
        }
        "cvc5" | "cvc4" => {
            c.arg(format!("--tlimit={}", budget_ms));
            c.arg("--lang=smt2");
            c.arg(file);
        }
        "yices-smt2" | "yices" => {
            c.arg(format!("--timeout={}", budget_ms.div_ceil(1000)));
            c.arg(file);
        }
        _ => {
            c.arg(file);
        }
    }
    c
}

fn run_solver(solver: &str, file: &Path, out_path: &Path, budget_ms: u64) -> (SolverAnswer, u64, String) {
    let out_file = match fs::File::create(out_path) {
        Ok(f) => f,
        Err(e) => return (SolverAnswer::Error(format!("cannot write {}: {}", out_path.display(), e)), 0, String::new()),
    };
    let err_file = match out_file.try_clone() {
        Ok(f) => f,
        Err(e) => return (SolverAnswer::Error(e.to_string()), 0, String::new()),
    };
    let start = Instant::now();
    let mut child = match command(solver, file, budget_ms)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (SolverAnswer::Error(format!("cannot run `{}`: {}", solver, e)), 0, String::new()),
    };
    // The solver's own timeout ends most runs; the hard stop is the budget
    // plus a grace period for the solver to print its answer.
    let deadline = Duration::from_millis(budget_ms + 500);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return (SolverAnswer::Error(e.to_string()), start.elapsed().as_millis() as u64, String::new()),
        }
    };
    let elapsed = start.elapsed().as_millis() as u64;
    let output = fs::read_to_string(out_path).unwrap_or_default();
    let answer = parse_answer(&output);
    let answer = match answer {
        Some(a) => a,
        None if timed_out => SolverAnswer::Timeout,
        None => match status {
            Some(s) if !s.success() => SolverAnswer::Error(format!("exit {}: {}", s.code().unwrap_or(-1), first_line(&output))),
            _ => SolverAnswer::Error(format!("no answer in the output: {}", first_line(&output))),
        },
    };
    (answer, elapsed, output)
}

/// The first `sat`, `unsat` or `unknown` line of a solver's output; an
/// error line before it is the answer.
pub fn parse_answer(output: &str) -> Option<SolverAnswer> {
    for line in output.lines() {
        let l = line.trim();
        match l {
            "sat" => return Some(SolverAnswer::Sat),
            "unsat" => return Some(SolverAnswer::Unsat),
            "unknown" => return Some(SolverAnswer::Unknown),
            _ if l.starts_with("(error") => return Some(SolverAnswer::Error(l.trim_start_matches("(error").trim().trim_end_matches(')').trim_matches('"').to_string())),
            _ if l.contains("timeout") && l.starts_with("timeout") => return Some(SolverAnswer::Timeout),
            _ => {}
        }
    }
    None
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(120).collect()
}

/// A file-name-safe form of an identifier.
pub fn sanitise(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('x');
    }
    out
}

/// An SMT-LIB2 symbol from a program name: letters, digits and `_`.
pub fn symbol(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        out.insert(0, 'v');
    }
    out
}

/// A decimal or rational numeral: `3.0`, `(- 3.0)`, `(/ 1.0 3.0)`.
pub fn numeral(q: super::lp::Q) -> String {
    let (n, d) = q.parts();
    let int = |x: i128| if x < 0 { format!("(- {}.0)", -x) } else { format!("{}.0", x) };
    if d == 1 {
        int(n)
    } else if n < 0 {
        format!("(- (/ {}.0 {}.0))", -n, d)
    } else {
        format!("(/ {}.0 {}.0)", n, d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_render_and_answers_parse() {
        let mut s = Script::new("QF_LRA");
        s.comment("obligation t");
        s.declare("x", "Real");
        s.declare("x", "Real");
        s.assert("(> x 1.0)");
        s.assert("(< x 0.0)");
        let t = s.text();
        assert!(
            t.starts_with("; obligation t\n(set-logic QF_LRA)\n(declare-const x Real)\n(assert (> x 1.0))\n(assert (< x 0.0))\n(check-sat)\n(exit)\n"),
            "{}",
            t
        );
        assert_eq!(parse_answer("unsat\n"), Some(SolverAnswer::Unsat));
        assert_eq!(parse_answer("sat\n(model)\n"), Some(SolverAnswer::Sat));
        assert_eq!(parse_answer("unknown"), Some(SolverAnswer::Unknown));
        assert!(matches!(parse_answer("(error \"line 3: unknown sort\")"), Some(SolverAnswer::Error(_))));
        assert_eq!(parse_answer(""), None);
        assert_eq!(numeral(super::super::lp::Q::new(-1, 3).unwrap()), "(- (/ 1.0 3.0))");
        assert_eq!(numeral(super::super::lp::Q::int(5)), "5.0");
        assert_eq!(numeral(super::super::lp::Q::int(-5)), "(- 5.0)");
        assert_eq!(symbol("param:lib::k"), "param_lib__k");
        assert_eq!(symbol("1x"), "v1x");
    }

    #[test]
    fn an_absent_solver_is_undecided_and_the_script_is_kept() {
        let dir = std::env::temp_dir().join(format!("abt-smt-test-{}", std::process::id()));
        let d = Driver::new(None, 100, 1000, Some(dir.clone()), "s");
        let mut s = Script::new("QF_LRA");
        s.declare("x", "Real");
        s.assert("(> x 1.0)");
        let o = d.run("t3-o1/a b", &s);
        assert_eq!(o.answer, SolverAnswer::Absent);
        let f = o.replay_file.expect("replay file");
        assert!(f.ends_with("s-t3-o1_a_b.smt2"), "{}", f);
        assert!(fs::read_to_string(&f).unwrap().contains("(check-sat)"));
        assert!(d.absent_note().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A stand-in solver (a shell script) exercises the subprocess path:
    /// it answers what its name says, and a sleeping one is stopped at the
    /// budget.
    #[cfg(unix)]
    #[test]
    fn the_subprocess_driver_reads_answers_and_stops_at_the_budget() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("abt-smt-fake-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-unsat");
        fs::write(&fake, "#!/bin/sh\ngrep -q check-sat \"$1\" && echo unsat\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let slow = dir.join("fake-slow");
        fs::write(&slow, "#!/bin/sh\nsleep 5\necho sat\n").unwrap();
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
        let mut s = Script::new("QF_LRA");
        s.declare("x", "Real");
        s.assert("(> x 1.0)");
        let d = Driver::new(Some(fake.display().to_string()), 2000, 10_000, Some(dir.clone()), "f");
        let o = d.run("one", &s);
        assert_eq!(o.answer, SolverAnswer::Unsat, "{:?}", o);
        assert!(o.replay_file.is_some());
        let d = Driver::new(Some(slow.display().to_string()), 100, 10_000, None, "f");
        let o = d.run("two", &s);
        assert_eq!(o.answer, SolverAnswer::Timeout, "{:?}", o);
        assert!(o.elapsed_ms < 3000, "{}", o.elapsed_ms);
        assert!(o.replay_file.is_none());
        // The compilation budget: once spent, nothing more runs.
        let d = Driver::new(Some(fake.display().to_string()), 2000, 1, None, "f");
        let _ = d.run("three", &s);
        let o = d.run("four", &s);
        assert!(matches!(o.answer, SolverAnswer::Unsat | SolverAnswer::BudgetSpent), "{:?}", o);
        let d2 = Driver::new(Some("/nonexistent/solver".into()), 100, 1000, None, "f");
        assert!(matches!(d2.run("five", &s).answer, SolverAnswer::Error(_)));
        let _ = fs::remove_dir_all(&dir);
    }
}
