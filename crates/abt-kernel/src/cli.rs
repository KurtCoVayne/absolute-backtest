//! `abt`: the command line of the crate. `check` judges a workspace;
//! `run` executes a strategy on data of its class under the program's own
//! `execution` and `report` blocks (no option sets a cost, a policy or the
//! periods); `query` (the query language, V-8), `show`, `graph` and `table`
//! read the model; `ir`, `plan`, `rewrite-log` and `verify`
//! print the compiler's stages; `synth` and `bundle` manage data and
//! bundles. A run's metrics, plans and the briefs harness are the query
//! layer's commands, which the front serves. `usage()` holds the exact
//! grammar of every command.
// `RunError` carries the rule, the decision and the message of a halt; the
// library allows the same lint for the same reason.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::str::FromStr;

use crate::check::{check_program, check_workspace, Severity, Workspace};
use crate::data;
use crate::kernel::time::{format_timestamp, parse_timestamp};
use crate::kernel::{ExecConfig, Kernel, RunError, RunResult, Value};
use crate::{observe, Ty};

struct Args {
    cmd: String,
    files: Vec<PathBuf>,
    opts: HashMap<String, String>,
    /// Repeatable options (`--bind`, `--param`), in the order given.
    multi: HashMap<String, Vec<String>>,
    flags: HashSet<String>,
}

const FLAGS: [&str; 11] = ["synthetic", "verify-causality", "quiet", "all", "fills", "timing", "json", "observed", "no-pushdown", "show", "paced"];
/// Options that may repeat.
const MULTI: [&str; 2] = ["text", "param"];
/// Every option. The executor's fields are not among them: a run is
/// configured by the program's `execution` block, and its reporting by the
/// `report` block (language-v2, 4.3 and 4.5); what remains here is what
/// describes the run itself (its data, its window, the bundle date), the
/// compiler's choices that keep the result, and the commands' arguments.
const OPTIONS: [&str; 40] = [
    "strategy",
    "mode",
    "session",
    "replay-of",
    "deadline",
    "memory-budget",
    "table",
    "tables",
    "stage",
    "plan",
    "tier",
    "solver",
    "replay-dir",
    "rel",
    "symbol",
    "ledger",
    "ledger-out",
    "start",
    "end",
    "returns",
    "dump",
    "nav",
    "checkpoint-every",
    "checkpoint-dir",
    "resume",
    "kernel",
    "bundle",
    "from",
    "version",
    "as-of",
    "param",
    "data",
    "text",
    "at",
    "class",
    "base",
    "out",
    "days",
    "symbols",
    "seed",
];

fn parse_args(raw: Vec<String>) -> Args {
    let mut it = raw.into_iter();
    let cmd = it.next().unwrap_or_else(|| usage(1));
    let mut files = Vec::new();
    let mut opts = HashMap::new();
    let mut multi: HashMap<String, Vec<String>> = HashMap::new();
    let mut flags = HashSet::new();
    let mut rest: Vec<String> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        let a = std::mem::take(&mut rest[i]);
        if let Some(name) = a.strip_prefix("--") {
            if FLAGS.contains(&name) {
                flags.insert(name.to_string());
            } else if OPTIONS.contains(&name) {
                i += 1;
                let v = rest.get(i).cloned().unwrap_or_else(|| {
                    eprintln!("--{} needs a value", name);
                    usage(1)
                });
                if MULTI.contains(&name) {
                    multi.entry(name.to_string()).or_default().push(v);
                } else {
                    opts.insert(name.to_string(), v);
                }
            } else {
                eprintln!(
                    "unknown option --{} (the executor's fields are set by the program's execution block, the reporting by its report block)",
                    name
                );
                usage(1)
            }
        } else {
            files.push(PathBuf::from(a));
        }
        i += 1;
    }
    Args { cmd, files, opts, multi, flags }
}

/// Whether a path is a Unix socket (`--data SOCKET`: a data service's).
fn is_socket(p: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(p).map(|m| m.file_type().is_socket()).unwrap_or(false)
}

/// The data service a run reads: the one at `--data SOCKET`, else one in
/// this process holding the dataset the options loaded (E-36).
static DATA_SOCKET: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn data_client(prog: &crate::check::Program, dataset: &crate::kernel::Dataset) -> Result<crate::kernel::DataClient, RunError> {
    match DATA_SOCKET.get() {
        Some(path) => {
            let path = path.clone();
            crate::kernel::DataClient::connect_with(std::sync::Arc::new(move || {
                abt_wire::transport::Connection::connect(&path).map_err(|e| format!("--data {}: {}", path.display(), e))
            }))
        }
        None => crate::kernel::connect_dataset(dataset, prog),
    }
}

/// Run and keep the kernel afterwards for what reads the model after the
/// run: the explanations of the coverage report's root causes, the ledger
/// and the state tables.
fn run_keeping(
    prog: &crate::check::Program,
    lowered: Option<&crate::lower::Lowered>,
    client: &crate::kernel::DataClient,
    cfg: ExecConfig,
    ledger: &[String],
    ledger_out: Option<&Path>,
    tables: Option<(&Path, &crate::plan::Plan)>,
) -> Result<(RunResult, Vec<String>), RunError> {
    // A run writes its outputs to the data service it reads (K-7.1), what
    // its metrics are computed from (E-66).
    let opts = crate::kernel::Replay {
        record_summaries: tables.is_some(),
        write_outputs: true,
        ..Default::default()
    };
    crate::kernel::run_keeping(prog, lowered, client, cfg, &opts, |result, k| {
        let mut notes = Vec::new();
        let (_, causes) = observe::coverage_report(prog, &result.coverage, result.bars.len());
        for c in &causes {
            for ri in prog.rules_for(&c.relation) {
                let label = prog.rule_label(ri);
                let cal = prog.cal_of(&c.relation);
                let first = result.bars[0];
                let last = *result.bars.last().unwrap();
                let bars: Vec<i64> = k.bars_at(&cal).into_iter().filter(|&b| b >= first && b <= last).collect();
                let Some(&t) = bars.get(bars.len() / 2) else {
                    notes.push(format!("rule {}: no bar of {} inside the run to explain it at", label, prog.cal_name(&cal)));
                    continue;
                };
                match k.explain(ri, t, &[]) {
                    Ok(ex) => notes.push(format!("{} (a sample bar; abt query --text 'why not {}(A, BAR)' for another)", ex, c.relation)),
                    Err(e) => notes.push(format!("rule {} at {}: {}", label, format_timestamp(t), e)),
                }
            }
        }
        if let Some(dir) = ledger_out {
            write_ledger(k, prog, result, ledger, dir)?;
        }
        if let Some((dir, plan)) = tables {
            let n = write_tables(k, prog, result, plan, dir)?;
            notes.push(format!(
                "tables: {} ({} Arrow IPC file(s): coverage, plan, book, summary, binding, extension/, carried/, derivation, failure)",
                dir.display(),
                n
            ));
        }
        Ok(notes)
    })
}

/// The program that covers a class whole (every relation, list and series
/// the catalog declares), for the bundle and synthetic-market commands.
fn class_program_or_exit(ws: &Workspace, class: &str, base: Option<crate::Resolution>) -> crate::check::Program {
    crate::check::class_program_at(ws, class, base).unwrap_or_else(|diags| {
        for d in &diags {
            eprintln!("{}", d);
        }
        eprintln!(
            "the catalog's class `{}` does not check as a program; pass the class file (and the standard library) among the files",
            class
        );
        exit(1)
    })
}

/// The strategy named on the command line, checked (every diagnostic
/// printed; an error exits).
fn checked_program(args: &Args) -> (crate::check::Program, Vec<crate::check::Diagnostic>) {
    let ws = workspace(&args.files);
    let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
    let (prog, diags) = check_program(&ws, &name);
    for d in &diags {
        eprintln!("{}", d);
    }
    let Some(prog) = prog else {
        eprintln!("strategy `{}` does not check; fix the errors above", name);
        exit(1)
    };
    (prog, diags)
}

/// A run under the plan whose kernel, at the end of the stream, `f` reads
/// with the run (the state tables and the provenance commands).
fn with_kernel<T: Send>(
    prog: &crate::check::Program,
    dataset: &crate::kernel::Dataset,
    lowered: &crate::lower::Lowered,
    cfg: ExecConfig,
    record_summaries: bool,
    f: impl for<'k> FnOnce(&RunResult, &mut Kernel<'k>) -> Result<T, RunError> + Send,
) -> Result<T, RunError> {
    let client = data_client(prog, dataset)?;
    let opts = crate::kernel::Replay {
        record_summaries,
        ..Default::default()
    };
    crate::kernel::run_keeping(prog, Some(lowered), &client, cfg, &opts, f).map(|(_, t)| t)
}

/// `--at`, parsed.
fn at_option(args: &Args) -> Option<i64> {
    args.opts.get("at").map(|s| {
        parse_timestamp(s).unwrap_or_else(|| {
            eprintln!("--at: `{}` is not a timestamp (YYYY-MM-DD, optionally THH:MM[:SS])", s);
            exit(2)
        })
    })
}

/// The bar a query is about: `--at`, or the last bar of the relation's
/// resolution inside the run.
fn bar_for(k: &Kernel, prog: &crate::check::Program, rel: &str, at: Option<i64>, bars: &[i64]) -> Result<i64, RunError> {
    let cal = prog.cal_of(rel);
    let last = bars.last().copied().ok_or(RunError::NoBars)?;
    let t = match at {
        Some(t) => t,
        None => k.bars_at(&cal).into_iter().rfind(|&b| b <= last).unwrap_or(last),
    };
    k.check_bar(&cal, t)?;
    Ok(t)
}

/// `--symbol`, parsed as an instrument.
fn symbol_for(k: &Kernel, args: &Args) -> Result<Option<Value>, RunError> {
    match args.opts.get("symbol") {
        Some(s) => k.parse_binding(s).map(Some).map_err(|m| RunError::Request(format!("--symbol {}: {}", s, m))),
        None => Ok(None),
    }
}

/// The relation named by --rel, or an input error.
fn rel_option(args: &Args, what: &str) -> String {
    args.opts.get("rel").cloned().unwrap_or_else(|| {
        eprintln!("{} needs --rel RELATION", what);
        usage(1)
    })
}

/// Write the state tables of a run as Arrow IPC files under `dir`
/// (compiler design, section 7.2): coverage, plan with the observed
/// counts, book, summary, the extension of every demanded derived
/// relation, the carried values of the recursive ones, and the derivation
/// and failure tables of `decide` at the sampled bars. Returns the number
/// of files.
fn write_tables(k: &mut Kernel, prog: &crate::check::Program, result: &RunResult, plan: &crate::plan::Plan, dir: &Path) -> Result<usize, RunError> {
    use crate::state::tables;
    std::fs::create_dir_all(dir).map_err(|e| RunError::Request(format!("--tables {}: {}", dir.display(), e)))?;
    let mut files = 0usize;
    let mut write = |t: &tables::Table, name: &str| -> Result<(), RunError> {
        let path = dir.join(format!("{}.arrow", name));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RunError::Request(format!("{}: {}", parent.display(), e)))?;
        }
        t.write(&path).map_err(RunError::Request)?;
        files += 1;
        Ok(())
    };
    write(&tables::coverage(result), "coverage")?;
    write(&tables::plan(&plan.clone().with_observed(k.literal_counts())), "plan")?;
    write(&tables::book(result), "book")?;
    write(&tables::summary(k, prog), "summary")?;
    let graph = observe::program_graph(prog);
    let names: Vec<String> = prog
        .relations
        .iter()
        .filter(|(_, s)| matches!(s.kind, crate::Kind::Derived { .. } | crate::Kind::Output))
        .filter(|(n, _)| plan.relations.iter().find(|r| &r.name == *n).map(|r| r.demanded && !r.elided).unwrap_or(false))
        .map(|(n, _)| n.clone())
        .collect();
    for name in &names {
        let ext = tables::extension(k, prog, name, &result.bars, None)?;
        write(&ext, &format!("extension/{}", name))?;
        if graph.node(name).map(|n| n.recursive).unwrap_or(false) {
            let car = tables::carried(k, prog, name, &result.bars, None)?;
            write(&car, &format!("carried/{}", name))?;
        }
    }
    let mut derivation: Option<tables::Table> = None;
    let mut failure: Option<tables::Table> = None;
    for bar in tables::sampled_bars(&result.bars, 10) {
        let d = tables::derivation(k, prog, "decide", bar, None)?;
        let f = tables::failure(k, prog, "decide", bar, None, None)?;
        match &mut derivation {
            Some(t) => t.rows.extend(d.rows),
            None => derivation = Some(d),
        }
        match &mut failure {
            Some(t) => t.rows.extend(f.rows),
            None => failure = Some(f),
        }
    }
    if let Some(mut d) = derivation {
        d.notes.push("the sampled bars of the run (section 7.3); `abt table --table derivation --at BAR` for another".into());
        write(&d, "derivation")?;
    }
    if let Some(mut f) = failure {
        f.notes.push("the sampled bars of the run (section 7.3); `abt table --table failure --at BAR` for another".into());
        write(&f, "failure")?;
    }
    Ok(files)
}

/// Write every tuple of the named relations at every bar of the run (of the
/// relation's own time domain inside the run's window) as DIR/<relation>.parquet,
/// one column per argument of the signature, and the coverage as
/// DIR/coverage.parquet.
fn write_ledger(k: &mut Kernel, prog: &crate::check::Program, result: &RunResult, ledger: &[String], dir: &Path) -> Result<(), RunError> {
    use crate::table::{write_table, Col};
    std::fs::create_dir_all(dir).map_err(|e| RunError::Request(format!("--ledger-out {}: {}", dir.display(), e)))?;
    let first = result.bars[0];
    let last = *result.bars.last().unwrap();
    for rel in ledger {
        let Some(sig) = prog.relations.get(rel) else {
            let names: Vec<&str> = prog.relations.keys().map(|s| s.as_str()).collect();
            return Err(RunError::Request(format!(
                "--ledger: `{}` is not a relation of {}; its relations are {}",
                rel,
                prog.strategy,
                names.join(", ")
            )));
        };
        let bars: Vec<i64> = k.bars_at(&sig.cal).into_iter().filter(|&b| b >= first && b <= last).collect();
        let width = sig.args.len();
        let mut times: Vec<Option<i64>> = Vec::new();
        let mut cols: Vec<Col> = sig
            .args
            .iter()
            .map(|a| match a.ty {
                Ty::Timestamp => Col::Time(Vec::new()),
                Ty::Count => Col::Int(Vec::new()),
                Ty::Quantity(_) => Col::Float(Vec::new()),
                _ => Col::Str(Vec::new()),
            })
            .collect();
        for t in bars {
            for tu in k.query(rel, t, &[])? {
                times.push(Some(t));
                for j in 0..width {
                    match (&mut cols[j], &tu[j]) {
                        (Col::Time(v), Value::Time(x)) => v.push(Some(*x)),
                        (Col::Time(v), _) => v.push(None),
                        (Col::Int(v), Value::Count(x)) => v.push(Some(*x)),
                        (Col::Int(v), _) => v.push(None),
                        (Col::Float(v), Value::Num(x)) => v.push(Some(*x)),
                        (Col::Float(v), Value::Count(x)) => v.push(Some(*x as f64)),
                        (Col::Float(v), _) => v.push(None),
                        (Col::Str(v), x) => v.push(Some(k.show(x))),
                        (Col::Bool(v), _) => v.push(None),
                    }
                }
            }
        }
        let mut table: Vec<(&str, Col)> = vec![("bar", Col::Time(times))];
        for (a, c) in sig.args.iter().zip(cols) {
            table.push((a.name.as_str(), c));
        }
        write_table(&dir.join(format!("{}.parquet", rel)), table).map_err(RunError::Request)?;
    }
    let cov = &result.coverage;
    write_table(
        &dir.join("coverage.parquet"),
        vec![
            ("relation", Col::Str(cov.iter().map(|c| Some(c.name.clone())).collect())),
            ("derived", Col::Bool(cov.iter().map(|c| Some(c.derived)).collect())),
            ("calls", Col::Int(cov.iter().map(|c| Some(c.calls as i64)).collect())),
            ("nonempty", Col::Int(cov.iter().map(|c| Some(c.nonempty as i64)).collect())),
            ("tuples", Col::Int(cov.iter().map(|c| Some(c.tuples as i64)).collect())),
        ],
    )
    .map_err(RunError::Request)?;
    Ok(())
}

/// What runs a checked program over a data service: the fold, or the fold
/// with checkpoints, truncated at a horizon when one is given.
type Runner = dyn Fn(&crate::check::Program, &crate::kernel::DataClient, ExecConfig, Option<i64>) -> Result<RunResult, RunError> + Sync;

/// What prints a run's returns: the query layer's metrics over the tables
/// the run wrote (Q-3.1), which the front installs; the kernel computes
/// none (E-66). Given a connection to the run's data service, the run and
/// the report block's calendar, it answers the lines to print and the
/// returns per reporting period (`t`, return, P&L).
pub type Reporter = dyn Fn(abt_wire::transport::Connection, &str, &str) -> Result<(Vec<String>, Vec<(i64, f64, f64)>), String> + Send + Sync;

static REPORTER: std::sync::OnceLock<Box<Reporter>> = std::sync::OnceLock::new();

/// Registers the reporter; the first registration wins.
pub fn install_reporter(r: Box<Reporter>) -> bool {
    REPORTER.set(r).is_ok()
}

fn usage(code: i32) -> ! {
    eprintln!(
        "usage:\n  abt check <files...>\n  abt run --strategy NAME (--data DIR|SOCKET | --bundle DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])\n          [--start DATE] [--end DATE] [--as-of DATE] [--param NAME=VALUE]... [--plan written|optimised]\n          [--mode replay-sim|live-sim --session S] [--replay-of RUN [--paced]] [--deadline SECONDS|none]\n          [--verify-causality] [--no-pushdown] [--memory-budget BYTES] [--all] [--fills] [--nav FILE] [--returns FILE] [--dump DIR] [--tables DIR] [--ledger R1,R2,... --ledger-out DIR] [--quiet] [--timing] <files...>\n          (the executor is the program's `execution` block and the reporting its `report` block: no option sets a cost, a policy, a price relation or the periods)\n  abt show --strategy NAME <files...>   (the program graph: every relation decide reaches, from decide upward, with its signature, kind, calendar, completeness, stratum, loop, depth, reads and rules)\n  abt ir --strategy NAME [--stage parsed|checked|normalised|analysed|rewritten|planned] [--json] <files...>   (a compiler stage's result; the normalised stage carries the program hash and the name map)\n  abt hash --strategy NAME [--json | --show] <files...>   (H(P): BLAKE3 over the program's normal form without its report block, the program's identity; --json adds the normal form as source, --show prints the bytes hashed: the RFC 8785 form of the normal form's hash view)\n  abt rewrite-log --strategy NAME [--json] <files...>   (every rewrite applied: rule, span, before, after, the law, the measure; JSON lines with --json)\n  abt show plan --strategy NAME [--observed] [--json] [--plan written|optimised] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>   (the compiler's plan: literal order and cost per rule, aggregate implementations, stores, strata; --observed runs it and adds the counts)\n  abt contract --strategy NAME [--json] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>   (the contract derived from the program, K-3: preconditions, invariants, postconditions and limits with their source, discharge and policy; with data, the run's status of each)\n  abt verify --strategy NAME [--tier N] [--solver z3|cvc5] [--replay-dir DIR] [--json] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>   (the verification report, tiers 0 to N: every obligation with its verdict, budget use and replay file; tiers 5 and 6 need a binding; exit 1 on a refuted obligation)\n  abt graph --strategy NAME <files...>   (the program graph of `show` with the compiler's facts: keyedness label, open or closed loop, demanded or elided, the abstract value of every column)\n  abt table --strategy NAME --table coverage|plan|book|binding|summary|extension|carried|derivation|failure [--rel R] [--at T] [--symbol SYM] [--out FILE] [--json] (--data DIR | --bundle DIR | --synthetic ...) <files...>   (a state table after the run, as text, JSON, or written to FILE as Arrow IPC, or Parquet for a .parquet name)\n  abt query --strategy NAME --text QUERY [--text QUERY]... [--json] [--out FILE] (--data DIR|SOCKET | --bundle DIR | --synthetic ...) <files...>\n          (the query language over the run, V-8: `? BODY at T = DATE | at T | over D1 .. D2 [for A = KEY]` (a rule body checked with the program, its solutions at the bars), `why R(...)` (the derivation down to supplied tuples), `why not R(A, T) [in written order]`, `trace R at DATE` (every instance attempted, fired or the literal that excluded it), `coverage [over D1 .. D2]`, `show program|plan [R]|schedule|calendars|contract`, `ledger R into NAME` (written through --data SOCKET as kernel.<run>.NAME))\n  abt synth --class CLASS --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>   (a synthetic market of the class, as Parquet; the catalog's class file suffices)\n  abt bundle build (--from DIR | --synthetic ...) --class CLASS [--base @1m] --version V --out DIR <files...>   (a vendor's data is ingested into a store by `abt data ingest --adapter norgate|databento`)\n  abt bundle test DIR <files...>\n  abt plan|metrics|test|claim|feed|activation|lineage|dispute|briefs ...   (the query layer's commands: plans and their trials, a run's metrics, statistical tests and claims, feed and activation statistics, lineages, the briefs harness; each prints its usage)\n\n  --param NAME=VALUE    override a parameter's default (repeatable; a library's as unit::name)\n  --memory-budget B     the memory the derived state may take (K-4.6; default half the machine's): a program whose state bound exceeds it is refused before its first barrier\n  --no-pushdown         evaluate every aggregate in the kernel rather than having the data layer answer the eligible ones (K-4.5); a test switch: both give the same bits\n  --start DATE, --end DATE  the run's window: the executor runs and the strategy decides only at bars within it; earlier bars stay data (warm-up)\n  --as-of DATE          the bundle date a ticker literal or a command-line name resolves at (default: the data's last bar)\n  --plan KIND           written (the written literal order, no rewrite) or optimised (default): the plan every run and report uses; the program's `numerics` field selects the aggregate implementation\n  --mode MODE           replay-sim (the default: the data layer's replay of what it holds) or live-sim (--session S: a live session the data service at --data SOCKET records, its barriers emitted as its clock passes them, K-6.2)\n  --replay-of RUN       replay a live-sim run's recorded stream (its session's rows and the barriers its stream emitted, D-5.5); --paced at the data layer's clock's pace\n  --deadline S          how long after its barrier a live decision bar may be evaluated before its decisions are late (K-6.3; default one bar of the decision calendar; `none` measures nothing): the execution block's `late send|reject` decides what they become\n  --checkpoint-every P  write the fold's state at the end of every month (`month`) or every N bars: over --data SOCKET as a cache object of the data layer, its key printed; in process into --checkpoint-dir DIR as <bar>.json\n  --resume KEY|FILE     continue from a checkpoint (a cache key over --data SOCKET, else a file) of the same program, binding and window (the stream resumes at its cursor)\n  --bundle DIR          run on a bundle (manifest.json, securities.parquet, log/<relation>/<YYYY-MM>.parquet); one whose tests have not passed is unreviewed, refused under `require reviewed` and warned under `allow unreviewed`\n  --nav FILE            write the book at every bar to FILE as Parquet: t,equity,cash,gross,net,leverage\n  --returns FILE        write the returns per reporting period (the report block's calendar) to FILE as Parquet: t,ret,pnl\n  --dump DIR            write the data, configuration, decisions, fills, dropped decisions, actions, book and final state to DIR (what reference/ replays)\n  --tables DIR          with `run`: write every state table of the run under DIR as Arrow IPC\n  --ledger R1,R2,...    with --ledger-out DIR: write the named relations' tuples at every bar of the run as Parquet\n  --stage STAGE         the stage `ir` prints (default normalised); --json prints a stage, a plan, a log or a report as JSON\n  --tier N              the last verification tier `verify` runs (0 checker, 1 abstract interpretation, 2 linear feasibility, 3 SMT, 4 CHC, 5 translation validation, 6 runtime; default 6)\n  --solver NAME         the SMT solver (z3, cvc5; a program on the PATH) tiers 3 and 4 drive as a subprocess; absent, they are undecided\n  --replay-dir DIR      keep every solver script as DIR/<strategy>-<obligation>.smt2 (and a CHC certificate as .model), so a verdict replays from the file alone\n  --table NAME          the state table `table` prints; --out FILE writes it (Arrow IPC, or Parquet for a .parquet name)\n  --rel RELATION        the relation `table` reads; --symbol SYM keeps one instrument's tuples\n  --text QUERY          a query of `abt query` (repeatable: one run answers them all)\n  --base RES            the bars a built bundle holds (default: the class's base)\n  --timing              print one stderr line: load_s (data), build_s (parse and check), run_s (kernel build and evaluation), total_s, bars, symbols, tuples, decisions, bars_per_s, peak_rss_mb and the kernel's statistics (pushed_answers: the aggregates the data layer answered)"
    );
    exit(code)
}

/// The value of `--name` parsed as `T`, or `default` when absent. A value
/// that does not parse is an input error (exit 2), never a silent default.
fn option<T: FromStr>(opts: &HashMap<String, String>, name: &str, what: &str, default: T) -> T {
    match opts.get(name) {
        None => default,
        Some(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("--{}: `{}` is not {}", name, v, what);
            exit(2)
        }),
    }
}

fn workspace(files: &[PathBuf]) -> Workspace {
    let mut ws = Workspace::new();
    let mut paths = Vec::new();
    for f in files {
        if f.is_dir() {
            collect_dsl(f, &mut paths);
        } else {
            paths.push(f.clone());
        }
    }
    paths.sort();
    for p in paths {
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| {
            eprintln!("{}: {}", p.display(), e);
            exit(2)
        });
        if let Err(e) = ws.add_source(&src) {
            eprintln!("{}:{}: parse error: {}", p.display(), e.span, e.message);
            exit(2)
        }
    }
    ws
}

fn collect_dsl(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_dsl(&p, out);
            } else if p.extension().map(|x| x == "dsl").unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

fn synthetic_for(prog: &crate::check::Program, opts: &HashMap<String, String>) -> crate::kernel::Dataset {
    // The instruments table names the market when the program writes one.
    let symbols: Vec<String> = match opts.get("symbols") {
        None if !prog.instruments.is_empty() => prog.instruments.iter().map(|(n, _)| n.clone()).collect(),
        None => vec!["AAA".into(), "XYZ".into(), "BBB".into(), "SPY".into(), "DDD".into()],
        Some(s) => {
            let syms: Vec<String> = s.split(',').map(|x| x.trim().to_string()).collect();
            if syms.iter().any(|x| x.is_empty()) {
                eprintln!("--symbols: `{}` is not a comma-separated list of symbols", s);
                exit(2)
            }
            syms
        }
    };
    let syms: Vec<&str> = symbols.iter().map(|s| s.as_str()).collect();
    let seed: u64 = option(opts, "seed", "a non-negative integer", 7);
    // The market of the program's class and base: minute futures with
    // sessions, minute equities, or daily equities with the catalog.
    let days: usize = option(opts, "days", "a number of days", if prog.base.is_sub_daily() { 40 } else { 500 });
    let start = if prog.base.is_sub_daily() { (2024, 1, 2) } else { (2022, 1, 3) };
    data::synthetic_for_program(prog, &syms, start, days, seed)
}

/// The dataset a run uses: synthetic, a bundle or a loose Parquet directory, as the options say.
fn load_dataset(args: &Args, prog: &crate::check::Program) -> (crate::kernel::Dataset, Option<String>) {
    let mut bundle_label: Option<String> = None;
    let dataset = if args.flags.contains("synthetic") {
        synthetic_for(prog, &args.opts)
    } else if let Some(dir) = args.opts.get("bundle") {
        let (ds, m) = crate::bundle::load_bundle(prog, Path::new(dir)).unwrap_or_else(|e| {
            eprintln!("{}", e);
            exit(2)
        });
        bundle_label = Some(format!("{}@{}", m.name, m.version));
        ds
    } else if let Some(dir) = args.opts.get("data").filter(|d| is_socket(Path::new(d))) {
        // A data service already holds the data: the run reads it there.
        let _ = DATA_SOCKET.set(PathBuf::from(dir));
        crate::kernel::Dataset::new()
    } else if let Some(dir) = args.opts.get("data") {
        let (ds, notes) = data::load_parquet_dir(prog, Path::new(dir)).unwrap_or_else(|e| {
            eprintln!("{}", e);
            exit(2)
        });
        for n in notes {
            eprintln!("note: {}", n);
        }
        ds
    } else {
        usage(1)
    };
    (dataset, bundle_label)
}

/// The executor configuration of a run: the program's `execution` block
/// (every field that changes the result is in the program, language-v2
/// 4.3), with what describes the run itself: its window, the bundle date a
/// ticker literal resolves at, and parameter overrides.
fn exec_config(args: &Args, prog: &crate::check::Program) -> ExecConfig {
    let mut cfg = ExecConfig::from_program(prog);
    cfg.pushdown = !args.flags.contains("no-pushdown");
    cfg.memory_budget = args.opts.get("memory-budget").map(|s| {
        s.parse::<u64>().unwrap_or_else(|_| {
            eprintln!("--memory-budget: `{}` is not a number of bytes", s);
            exit(2)
        })
    });
    cfg.as_of = args.opts.get("as-of").map(|s| {
        parse_timestamp(s).unwrap_or_else(|| {
            eprintln!("--as-of: `{}` is not a timestamp (YYYY-MM-DD)", s);
            exit(2)
        })
    });
    cfg.start = args.opts.get("start").map(|s| {
        parse_timestamp(s).unwrap_or_else(|| {
            eprintln!("--start: `{}` is not a timestamp (YYYY-MM-DD)", s);
            exit(2)
        })
    });
    cfg.end = args.opts.get("end").map(|s| {
        parse_timestamp(s).unwrap_or_else(|| {
            eprintln!("--end: `{}` is not a timestamp (YYYY-MM-DD)", s);
            exit(2)
        })
    });
    cfg.param_overrides = args
        .multi
        .get("param")
        .map(|ps| {
            ps.iter()
                .map(|p| {
                    let (name, raw) = p.split_once('=').unwrap_or_else(|| {
                        eprintln!("--param takes NAME=VALUE, not `{}`", p);
                        exit(1)
                    });
                    let (name, raw) = (name.trim(), raw.trim());
                    // A literal in the language's grammar; a bare identifier is an instrument.
                    let lit = crate::syntax::parse_lit(raw).unwrap_or_else(|e| {
                        if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') && !raw.starts_with(|c: char| c.is_ascii_digit()) {
                            crate::Lit::Str(raw.to_string())
                        } else {
                            eprintln!("--param {}: `{}` is not a literal (such as 20d, 100 shares, 0.02 or \"SPY\"): {}", name, raw, e.message);
                            exit(1)
                        }
                    });
                    (name.to_string(), lit)
                })
                .collect()
        })
        .unwrap_or_default();
    cfg
}

/// The executor mode a run's options name (K-6.2): `--mode replay-sim`
/// (the default) or `live-sim` with `--session S` (a live session of the
/// data service at `--data SOCKET`); `--replay-of RUN` replays a live-sim
/// run's recorded stream, `--paced` at the data layer's clock's pace;
/// `--deadline SECONDS|none` overrides one bar of the decision calendar.
fn run_mode(args: &Args) -> (crate::kernel::RunMode, Option<String>, bool, Option<i64>) {
    let fail = |m: String| -> ! {
        eprintln!("{}", m);
        exit(2)
    };
    let mode = match args.opts.get("mode").map(String::as_str) {
        None | Some("replay-sim") => crate::kernel::RunMode::ReplaySim,
        Some("live-sim") => crate::kernel::RunMode::LiveSim {
            session: args
                .opts
                .get("session")
                .cloned()
                .unwrap_or_else(|| fail("--mode live-sim reads a live session: name it with --session S".into())),
        },
        Some("live-broker") => fail("--mode live-broker: the broker interface is the live deployment's (K-6.4, M-10)".into()),
        Some(m) => fail(format!("--mode: `{}` is not replay-sim or live-sim", m)),
    };
    if args.opts.contains_key("session") && mode == crate::kernel::RunMode::ReplaySim {
        fail("--session names the live session of --mode live-sim".into());
    }
    let live = mode != crate::kernel::RunMode::ReplaySim || args.opts.contains_key("replay-of");
    if live && DATA_SOCKET.get().is_none() {
        fail("a live session and its recording are the data service's: name it with --data SOCKET".into());
    }
    let deadline = args.opts.get("deadline").map(|s| match s.as_str() {
        "none" => i64::MAX,
        x => x
            .parse::<f64>()
            .ok()
            .filter(|v| *v >= 0.0)
            .map(|v| (v * 1e9).round() as i64)
            .unwrap_or_else(|| fail(format!("--deadline: `{}` is not a number of seconds or `none`", x))),
    });
    (mode, args.opts.get("replay-of").cloned(), args.flags.contains("paced"), deadline)
}

/// What runs the program, under a plan: the fold over the data layer's
/// stream is the only driver (M-2); `--checkpoint-every` writes its
/// checkpoints and `--resume` continues one.
fn make_runner_with(args: &Args, lowered: Option<crate::lower::Lowered>) -> Box<Runner> {
    if let Some(k) = args.opts.get("kernel") {
        eprintln!("--kernel {}: the batch kernel is gone; every run is the fold over the data layer's stream", k);
        exit(2)
    }
    let checkpoint_every = args.opts.get("checkpoint-every").map(|s| match s.as_str() {
        "month" => crate::kernel::CheckpointEvery::Month,
        n => crate::kernel::CheckpointEvery::Bars(n.parse().unwrap_or_else(|_| {
            eprintln!("--checkpoint-every: `{}` is not `month` or a number of bars", n);
            exit(2)
        })),
    });
    let checkpoint_dir = args.opts.get("checkpoint-dir").map(PathBuf::from);
    let resume = args.opts.get("resume").map(PathBuf::from);
    // A run writes its outputs to its data service (K-7.1, E-66).
    let writes = args.cmd == "run";
    let (mode, replay_of, paced, deadline) = run_mode(args);
    Box::new(move |prog, client, cfg, horizon| {
        let mut opts = crate::kernel::Replay {
            checkpoint_every,
            horizon,
            write_outputs: writes,
            mode: mode.clone(),
            replay_of: replay_of.clone(),
            paced,
            deadline,
            ..Default::default()
        };
        if let Some(path) = &resume {
            // Over a data service, a checkpoint is a cache object named by its
            // key (K-4.7); in process, a file.
            let key = path.to_string_lossy().to_string();
            let cp: crate::kernel::Checkpoint = if DATA_SOCKET.get().is_some() && key.len() == 64 && key.chars().all(|c| c.is_ascii_hexdigit()) {
                crate::kernel::checkpoints::read(client, &key)?
            } else {
                let text = std::fs::read_to_string(path).map_err(|e| RunError::Request(format!("{}: {}", path.display(), e)))?;
                serde_json::from_str(&text).map_err(|e| RunError::Request(format!("{}: {}", path.display(), e)))?
            };
            eprintln!("resuming from {} (checkpoint of {})", path.display(), format_timestamp(cp.last_bar));
            opts.from = Some(cp);
        }
        crate::kernel::on_kernel_thread(|| {
            let mut exec = crate::kernel::SimExecutor::new(cfg.clone());
            let mut written: Result<(), RunError> = Ok(());
            let mut save = |cp: crate::kernel::Checkpoint| {
                // Over a data service the checkpoint is a cache object (K-4.7).
                if DATA_SOCKET.get().is_some() {
                    match crate::kernel::checkpoints::write(client, &cp) {
                        Ok(key) => eprintln!("checkpoint {} of {} (seq {})", key, format_timestamp(cp.last_bar), cp.cursor),
                        Err(e) => {
                            if written.is_ok() {
                                written = Err(e);
                            }
                        }
                    }
                    return;
                }
                let Some(dir) = &checkpoint_dir else { return };
                let r = (|| {
                    std::fs::create_dir_all(dir).map_err(|e| RunError::Request(format!("{}: {}", dir.display(), e)))?;
                    let path = dir.join(format!("{}.json", format_timestamp(cp.last_bar)));
                    let text = serde_json::to_string(&cp).map_err(|e| RunError::Internal(e.to_string()))?;
                    std::fs::write(&path, text).map_err(|e| RunError::Request(format!("{}: {}", path.display(), e)))?;
                    eprintln!("checkpoint {} written", path.display());
                    Ok(())
                })();
                if r.is_err() && written.is_ok() {
                    written = r;
                }
            };
            let (r, k) = crate::kernel::replay(prog, lowered.as_ref(), client, cfg.clone(), &mut exec, &opts, &mut save)?;
            std::mem::forget(k);
            written.map(|_| r)
        })
    })
}

/// The process's peak resident set in MB: `VmHWM` on Linux, `getrusage`'s
/// `ru_maxrss` (bytes) on macOS, unavailable elsewhere.
fn peak_rss_mb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
        let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb / 1024.0)
    }
    #[cfg(target_os = "macos")]
    {
        // struct rusage: two timevals, then ru_maxrss as the first long.
        #[repr(C)]
        struct Rusage {
            times: [i64; 4],
            longs: [i64; 14],
        }
        extern "C" {
            fn getrusage(who: i32, usage: *mut Rusage) -> i32;
        }
        let mut u = Rusage { times: [0; 4], longs: [0; 14] };
        // SAFETY: getrusage writes one struct rusage, whose layout on 64-bit macOS this mirrors.
        let rc = unsafe { getrusage(0, &mut u) };
        (rc == 0).then(|| u.longs[0] as f64 / (1024.0 * 1024.0))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

pub fn main() {
    main_with(std::env::args().skip(1).collect())
}

/// The commands, their arguments given (the front's `abt show plan` is the
/// compiler's `plan` here).
pub fn main_with(raw: Vec<String>) {
    let args = parse_args(raw);
    match args.cmd.as_str() {
        "check" => {
            let ws = workspace(&args.files);
            let diags = check_workspace(&ws);
            for d in &diags {
                println!("{}", d);
            }
            let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
            let warnings = diags.len() - errors;
            // Degrees of freedom per strategy (data-bundle doc, section 6):
            // what the study report counts, printed for every strategy that
            // checks.
            let mut names: Vec<&str> = ws.strategies().map(|s| s.name.as_str()).collect();
            names.sort_unstable();
            for name in names {
                if let (Some(p), _) = check_program(&ws, name) {
                    println!("{}: {}", name, p.degrees_of_freedom.summary());
                }
            }
            let strategies = ws.strategies().count();
            println!("{} strategies, {} libraries checked: {} error(s), {} warning(s)", strategies, ws.libraries().count(), errors, warnings);
            exit(if errors > 0 { 1 } else { 0 })
        }
        "ir" => {
            // abt ir --strategy NAME --stage parsed|checked|normalised [--json] <files...>
            let ws = workspace(&args.files);
            let stage = args.opts.get("stage").cloned().unwrap_or_else(|| "normalised".into());
            let json = args.flags.contains("json");
            if stage == "parsed" {
                if json {
                    println!("{}", serde_json::json!({"stage": "parsed", "source": crate::compiler::print::render_parsed(&ws.units)}));
                } else {
                    print!("{}", crate::compiler::print::render_parsed(&ws.units));
                }
                return;
            }
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            let text = stage_text(&args, &prog, &stage, json).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            print!("{}", text);
        }
        "hash" => {
            // abt hash --strategy NAME [--json | --show] <files...>: H(P), the
            // program's identity (V-8.4 form 3, A-6); --json adds the normal
            // form as source, --show prints the bytes hashed (P-2).
            let (prog, _) = checked_program(&args);
            let nir = crate::compiler::normalise(&prog);
            if args.flags.contains("show") {
                println!("{}", crate::compiler::normalise::hashed_text(&nir));
            } else if args.flags.contains("json") {
                println!("{}", serde_json::json!({"strategy": prog.strategy, "program_hash": nir.hash, "normal_form": nir.source(&prog)}));
            } else {
                println!("{}  {}", nir.hash, prog.strategy);
            }
        }
        "plan" => {
            // abt show plan --strategy NAME [--observed] [--json] [--plan written|optimised] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            let has_data = args.flags.contains("synthetic") || args.opts.contains_key("data") || args.opts.contains_key("bundle");
            let observed = args.flags.contains("observed");
            if observed && !has_data {
                eprintln!("--observed runs the strategy: name --data DIR, --bundle DIR or --synthetic");
                exit(2)
            }
            let loaded = if has_data { Some(load_dataset(&args, &prog)) } else { None };
            let opts = match &loaded {
                Some((ds, label)) => compile_options_with(
                    &args,
                    &prog,
                    Some((ds, label.as_deref().unwrap_or(if args.flags.contains("synthetic") { "synthetic" } else { "data" }))),
                ),
                None => compile_options(&args, &prog),
            };
            let compiled = crate::compiler::compile(&prog, &opts);
            let mut plan = compiled.plan.clone();
            if observed {
                let (ds, _) = loaded.as_ref().unwrap();
                let cfg = exec_config(&args, &prog);
                match crate::kernel::run_planned(&prog, &compiled.lowered, ds, cfg) {
                    Ok(r) => plan = plan.with_observed(&r.literal_counts),
                    Err(e) => {
                        eprintln!("run halted: {}", e);
                        exit(1)
                    }
                }
            }
            if args.flags.contains("json") {
                println!("{}", serde_json::to_string_pretty(&plan.json()).unwrap_or_default());
            } else {
                print!("{}", plan.text());
            }
        }
        "rewrite-log" => {
            // abt rewrite-log --strategy NAME [--json] <files...>
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            let opts = compile_options(&args, &prog);
            let compiled = crate::compiler::compile(&prog, &opts);
            if args.flags.contains("json") {
                print!("{}", compiled.rewritten.log.json_lines());
            } else {
                print!("{}", compiled.rewritten.log.table());
            }
        }
        "contract" => {
            // abt contract --strategy NAME [--json] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            // Without data, the contract of the program (K-3.7): its static
            // clauses proven, the others open; with data, a run's statuses.
            let has_data = args.flags.contains("synthetic") || args.opts.contains_key("data") || args.opts.contains_key("bundle");
            let contract = if has_data {
                let (ds, _) = load_dataset(&args, &prog);
                let cfg = exec_config(&args, &prog);
                match crate::kernel::run(&prog, &ds, cfg) {
                    Ok(r) => r.contract,
                    Err(e) => {
                        eprintln!("run halted: {}", e);
                        exit(match e {
                            RunError::Unbound(_) | RunError::Precondition { .. } => 2,
                            _ => 1,
                        })
                    }
                }
            } else {
                crate::contract::contract(&prog)
            };
            if args.flags.contains("json") {
                println!("{}", serde_json::to_string_pretty(&contract).unwrap_or_default());
            } else {
                print!("{}", contract.table());
            }
        }
        "verify" => {
            // abt verify --strategy NAME [--tier N] [--solver z3|cvc5] [--replay-dir DIR] [--json] [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            let max_tier: u8 = option(&args.opts, "tier", "a tier from 0 to 6", 6);
            if max_tier > 6 {
                eprintln!("--tier: `{}` is not a tier from 0 to 6", max_tier);
                exit(2)
            }
            let has_data = args.flags.contains("synthetic") || args.opts.contains_key("data") || args.opts.contains_key("bundle");
            let loaded = if has_data { Some(load_dataset(&args, &prog)) } else { None };
            let opts = match &loaded {
                Some((ds, label)) => compile_options_with(
                    &args,
                    &prog,
                    Some((ds, label.as_deref().unwrap_or(if args.flags.contains("synthetic") { "synthetic" } else { "data" }))),
                ),
                None => compile_options(&args, &prog),
            };
            let mut compiled = crate::compiler::compile(&prog, &opts);
            let cfg = exec_config(&args, &prog);
            let inputs = crate::verify::VerifyInputs {
                prog: &prog,
                diags: &diags,
                opts: &opts,
                max_tier,
                data: loaded.as_ref().map(|(ds, _)| (ds, cfg.clone())),
            };
            let report = crate::verify::verify(&inputs, &mut compiled);
            use crate::compiler::Stage;
            if args.flags.contains("json") {
                println!("{}", serde_json::to_string_pretty(&report.json()).unwrap_or_default());
            } else {
                print!("{}", report.text());
            }
            if report.refuted() {
                exit(1)
            }
        }
        "graph" => {
            // abt graph --strategy NAME [(--data DIR | --bundle DIR | --synthetic ...)] <files...>
            let (prog, _) = checked_program(&args);
            let opts = compile_options(&args, &prog);
            let compiled = crate::compiler::compile(&prog, &opts);
            print!("{}", observe::show_graph(&prog, &compiled));
        }
        "table" => {
            // abt table --strategy NAME --table coverage|plan|book|binding|summary|extension|carried|derivation|failure [--rel R] [--at T] [--symbol SYM] [--out FILE] [--json] (--data DIR | --bundle DIR | --synthetic ...) <files...>
            let (prog, _) = checked_program(&args);
            let which = args.opts.get("table").cloned().unwrap_or_else(|| {
                eprintln!("table needs --table NAME (coverage, plan, book, binding, summary, extension, carried, derivation, failure)");
                usage(1)
            });
            let (dataset, label) = load_dataset(&args, &prog);
            let opts = compile_options_with(
                &args,
                &prog,
                Some((&dataset, label.as_deref().unwrap_or(if args.flags.contains("synthetic") { "synthetic" } else { "data" }))),
            );
            let compiled = crate::compiler::compile(&prog, &opts);
            let cfg = exec_config(&args, &prog);
            let at = at_option(&args);
            let json = args.flags.contains("json");
            let out = args.opts.get("out").map(PathBuf::from);
            let needs_rel = matches!(which.as_str(), "extension" | "carried" | "derivation" | "failure");
            let rel = if needs_rel { Some(rel_option(&args, "this table")) } else { args.opts.get("rel").cloned() };
            let record = which == "summary";
            let text = with_kernel(&prog, &dataset, &compiled.lowered, cfg, record, |result, k| {
                use crate::state::tables;
                let result = result.clone();
                let bars = result.bars.clone();
                let table = match which.as_str() {
                    "coverage" => tables::coverage(&result),
                    "plan" => tables::plan(&compiled.plan.clone().with_observed(k.literal_counts())),
                    "book" => tables::book(&result),
                    "binding" => tables::binding(&prog, &dataset),
                    "summary" => tables::summary(k, &prog),
                    "extension" | "carried" | "derivation" | "failure" => {
                        let rel = rel.clone().unwrap();
                        match which.as_str() {
                            "extension" => tables::extension(k, &prog, &rel, &bars, None)?,
                            "carried" => tables::carried(k, &prog, &rel, &bars, None)?,
                            "derivation" => {
                                let t = bar_for(k, &prog, &rel, at, &bars)?;
                                tables::derivation(k, &prog, &rel, t, None)?
                            }
                            _ => {
                                let t = bar_for(k, &prog, &rel, at, &bars)?;
                                let sym = symbol_for(k, &args)?;
                                tables::failure(k, &prog, &rel, t, None, sym)?
                            }
                        }
                    }
                    other => {
                        return Err(RunError::Request(format!(
                            "--table: `{}` is not one of coverage, plan, book, binding, summary, extension, carried, derivation, failure",
                            other
                        )))
                    }
                };
                if let Some(p) = &out {
                    table.write(p).map_err(RunError::Request)?;
                }
                Ok(if json {
                    format!("{}\n", serde_json::to_string_pretty(&table.json()).unwrap_or_default())
                } else {
                    table.text()
                })
            });
            match text {
                Ok(t) => print!("{}", t),
                Err(e) => {
                    eprintln!("{}", e);
                    exit(1)
                }
            }
        }
        "explain" | "why-not" | "materialise" | "materialize" => {
            eprintln!(
                "`abt {}` is retired into the query language: abt query --strategy NAME (--data ...) --text 'why R(...)' | 'why not R(...)' | 'trace decide at T' | '? R(...) at T' <files...>",
                args.cmd
            );
            exit(2)
        }
        "show" => {
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            print!("{}", observe::show_program(&prog));
        }
        "query" => {
            // abt query --strategy NAME (--data ... | --bundle DIR | --synthetic ...) --text QUERY [--text QUERY]... [--json] [--out FILE] <files...>
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            let texts: Vec<String> = args.multi.get("text").cloned().unwrap_or_default();
            if texts.is_empty() {
                eprintln!("query needs --text QUERY (repeatable): ? R(A, T, X) at T = DATE, why R(...), why not R(...), trace decide at DATE, coverage over D1 .. D2, show program|plan|schedule|calendars|contract, ledger R into NAME");
                exit(2)
            }
            let queries: Vec<crate::query::Query> = texts
                .iter()
                .map(|t| {
                    crate::query::parse(t).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    })
                })
                .collect();
            // A `?` query is a rule the run holds beside the program's (V-8.1).
            let prepared = crate::query::prepare(&ws, &name, &prog, &queries).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            let prog = &prepared.program;
            let (dataset, label) = load_dataset(&args, prog);
            let cfg = exec_config(&args, prog);
            let opts = compile_options_with(
                &args,
                prog,
                Some((&dataset, label.as_deref().unwrap_or(if args.flags.contains("synthetic") { "synthetic" } else { "data" }))),
            );
            let compiled = crate::compiler::compile(prog, &opts);
            let json = args.flags.contains("json");
            let answers = with_kernel(prog, &dataset, &compiled.lowered, cfg, false, |result, k| {
                // `ledger ... into` writes through the data service the run read.
                let client = match DATA_SOCKET.get() {
                    Some(_) => Some(data_client(prog, &dataset)?),
                    None => None,
                };
                let cx = crate::query::Context {
                    prog,
                    result,
                    prepared: &prepared,
                    plan: Some(&compiled.plan),
                    out: client.as_ref().map(|c| (c, result.run_id.as_str())),
                };
                Ok(queries.iter().enumerate().map(|(i, q)| crate::query::answer(k, &cx, i, q)).collect::<Vec<_>>())
            })
            .unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(1)
            });
            let mut failed = false;
            for (text, a) in texts.iter().zip(answers) {
                match a {
                    Ok(a) => {
                        if json {
                            println!("{}", serde_json::json!({"query": text, "table": a.table.as_ref().map(|t| t.json()), "text": a.text}));
                        } else {
                            if texts.len() > 1 {
                                println!("{}", text);
                            }
                            print!("{}", a.text);
                        }
                        if let (Some(path), Some(t)) = (args.opts.get("out"), &a.table) {
                            if let Err(e) = t.write(Path::new(path)) {
                                eprintln!("--out {}: {}", path, e);
                                failed = true;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: {}", text, e);
                        failed = true;
                    }
                }
            }
            if failed {
                exit(1)
            }
        }
        "run" => {
            let started = std::time::Instant::now();
            let ws = workspace(&args.files);
            let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
            let (prog, diags) = check_program(&ws, &name);
            for d in &diags {
                eprintln!("{}", d);
            }
            let Some(prog) = prog else {
                eprintln!("strategy `{}` does not check; fix the errors above", name);
                exit(1)
            };
            let checked_at = started.elapsed().as_secs_f64();
            let (dataset, bundle_label) = load_dataset(&args, &prog);
            let loaded_at = started.elapsed().as_secs_f64();
            let cfg = exec_config(&args, &prog);
            let verify = args.flags.contains("verify-causality");
            // What the report needs from the data, read before a run may take it over.
            let tuple_count: usize = dataset.facts.values().map(|tus| tus.len()).sum();
            // The plan (compiler design, "Lower"): the optimised one unless
            // `--plan written` asks for the written order and no rewrite.
            let opts = compile_options_with(
                &args,
                &prog,
                Some((&dataset, bundle_label.as_deref().unwrap_or(if args.flags.contains("synthetic") { "synthetic" } else { "data" }))),
            );
            let compiled = crate::compiler::compile(&prog, &opts);
            let lowered = compiled.lowered.clone();
            // The batch kernel takes the facts over when nothing reads the
            // dataset after the run (one copy of the data in memory).
            let runner = make_runner_with(&args, Some(lowered.clone()));
            let ledger: Vec<String> = args
                .opts
                .get("ledger")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default();
            let ledger_out = args.opts.get("ledger-out").map(PathBuf::from);
            if !ledger.is_empty() && ledger_out.is_none() {
                eprintln!("--ledger needs --ledger-out DIR");
                exit(2)
            }
            let tables_out = args.opts.get("tables").map(PathBuf::from);
            if let Some(dir) = &tables_out {
                // The binding table reads the dataset, which the kernel may
                // take over for the run: written first.
                std::fs::create_dir_all(dir).unwrap_or_else(|e| {
                    eprintln!("--tables {}: {}", dir.display(), e);
                    exit(2)
                });
                if let Err(e) = crate::state::tables::binding(&prog, &dataset).write(&dir.join("binding.arrow")) {
                    eprintln!("--tables: {}", e);
                    exit(2)
                }
            }
            // A live-sim run and a replay of one go through the runner, which
            // reads the mode's options.
            let live = ["mode", "session", "replay-of", "deadline"].iter().any(|o| args.opts.contains_key(*o)) || args.flags.contains("paced");
            let keeps = !live && (!ledger.is_empty() || tables_out.is_some() || !args.opts.contains_key("checkpoint-every") && !args.opts.contains_key("resume"));
            let mut notes: Vec<String> = Vec::new();
            // The kernel is kept after the run for the coverage report's
            // explanations, the ledger and the state tables; a run that
            // writes or resumes checkpoints goes through the runner.
            // The data service the run reads and writes its outputs to; the
            // reporter reads them there after the run.
            let client = data_client(&prog, &dataset).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            let outcome = if keeps {
                if let Some(k) = args.opts.get("kernel") {
                    eprintln!("--kernel {}: the batch kernel is gone; every run is the fold over the data layer's stream", k);
                    exit(2)
                }
                run_keeping(
                    &prog,
                    Some(&lowered),
                    &client,
                    cfg.clone(),
                    &ledger,
                    ledger_out.as_deref(),
                    tables_out.as_deref().map(|d| (d, &compiled.plan)),
                )
                .map(|(r, n)| {
                    notes = n;
                    r
                })
            } else {
                runner(&prog, &client, cfg.clone(), None)
            };
            let result = outcome.unwrap_or_else(|e| {
                match e {
                    RunError::Request(m) => eprintln!("{}", m),
                    // Refused before the first barrier (K-3.2).
                    RunError::Unbound(_) | RunError::Precondition { .. } => {
                        eprintln!("{}", e);
                        exit(2)
                    }
                    e => eprintln!("run halted: {}", e),
                }
                exit(1)
            });
            if args.flags.contains("timing") {
                let total = started.elapsed().as_secs_f64();
                let run_s = total - loaded_at;
                let s = &result.stats;
                eprintln!(
                    "timing load_s={:.3} build_s={:.3} run_s={:.3} total_s={:.3} bars={} symbols={} tuples={} decisions={} bars_per_s={:.1} peak_rss_mb={} window_calls={} window_bars_solved={} window_groups={} window_rows_cached={} late_tuples={} memo_entries={} pushed_answers={}",
                    loaded_at - checked_at,
                    checked_at,
                    run_s,
                    total,
                    result.bars.len(),
                    result.symbols.len(),
                    tuple_count,
                    result.decisions.len(),
                    if run_s > 0.0 { result.bars.len() as f64 / run_s } else { 0.0 },
                    peak_rss_mb().map(|m| format!("{:.1}", m)).unwrap_or_else(|| "unavailable".into()),
                    s.window_calls,
                    s.window_bars_solved,
                    s.window_groups,
                    s.window_rows_cached,
                    s.late_tuples,
                    s.memo_entries,
                    s.pushed_answers
                );
            }
            println!(
                "strategy {} over {} bars ({} to {}), {} symbols",
                prog.strategy,
                result.bars.len(),
                format_timestamp(result.bars[0]),
                format_timestamp(*result.bars.last().unwrap()),
                result.symbols.len()
            );
            println!("decisions: {}   fills: {}   dropped: {}", result.decisions.len(), result.fills.len(), result.dropped.len());
            println!(
                "plan: {} ({} of {} rules reordered, {} infeasible, {} elided, {} rewrites; abt show plan --observed prints the estimates against this run)",
                lowered.label,
                compiled.plan.totals.reordered,
                compiled.plan.totals.rules,
                compiled.plan.totals.infeasible,
                compiled.plan.totals.elided,
                compiled.rewritten.log.entries.len()
            );
            for n in &lowered.notes {
                println!("plan note: {}", n);
            }
            // The metrics: the query layer's, over the tables the run wrote
            // (Q-3.1, E-66).
            let reported = match (REPORTER.get(), client.reconnect()) {
                (Some(report), Some(Ok(conn))) => Some(report(conn, &result.run_id, &prog.report.calendar)),
                (Some(_), Some(Err(e))) => Some(Err(e)),
                _ => None,
            };
            match &reported {
                Some(Ok((lines, _))) => {
                    for l in lines {
                        println!("{}", l);
                    }
                }
                Some(Err(e)) => println!("metrics: not computed ({})", e),
                None => println!("metrics: the query layer's (abt metrics --run {})", result.run_id),
            }
            if let Some(path) = args.opts.get("returns") {
                let Some(Ok((_, rows))) = &reported else {
                    eprintln!("error: --returns: the returns are the query layer's, which this run could not reach");
                    std::process::exit(1);
                };
                use crate::table::{write_table, Col};
                write_table(
                    Path::new(path),
                    vec![
                        ("t", Col::Time(rows.iter().map(|r| Some(r.0)).collect())),
                        ("ret", Col::Float(rows.iter().map(|r| Some(r.1)).collect())),
                        ("pnl", Col::Float(rows.iter().map(|r| Some(r.2)).collect())),
                    ],
                )
                .unwrap_or_else(|e| {
                    eprintln!("error: --returns: {}", e);
                    std::process::exit(1);
                });
            }
            println!(
                "costs: commissions {:.2}   fees {:.2}   slippage {:.2}   impact {:.2}   turnover {:.2}",
                result.costs.commissions, result.costs.fees, result.costs.slippage, result.costs.impact, result.costs.turnover
            );
            println!(
                "liquidity: fill ratio {:.3}   participation avg {:.2}%   max {:.2}%",
                result.liquidity.fill_ratio,
                result.liquidity.avg_participation * 100.0,
                result.liquidity.max_participation * 100.0
            );
            println!(
                "funding: cash interest {:.2}   margin interest {:.2}   borrow fees {:.2}   short rebate {:.2}",
                result.funding.cash_interest, result.funding.margin_interest, result.funding.borrow_fees, result.funding.short_rebate
            );
            let count = |f: &dyn Fn(&crate::kernel::Action) -> bool| result.actions.iter().filter(|a| f(&a.action)).count();
            println!(
                "actions: {} splits   {} dividends   {} delistings",
                count(&|a| matches!(a, crate::kernel::Action::Split { .. })),
                count(&|a| matches!(a, crate::kernel::Action::Dividend { .. })),
                count(&|a| matches!(a, crate::kernel::Action::Delisting { .. }))
            );
            let max_lev = result.exposure.iter().map(|e| e.leverage).fold(0.0, f64::max);
            let max_gross = result.exposure.iter().map(|e| e.gross).fold(0.0, f64::max);
            println!("exposure: max gross {:.2}   max leverage {:.3}", max_gross, max_lev);
            let (coverage_text, causes) = observe::coverage_report(&prog, &result.coverage, result.bars.len());
            print!("{}", coverage_text);
            for n in &notes {
                println!("  {}", n);
            }
            if !causes.is_empty() && notes.is_empty() {
                println!("  (abt query --text 'why not RELATION(A, BAR)' names the first literal with no solution)");
            }
            if let Some(dir) = &ledger_out {
                println!("ledger written to {}", dir.display());
            }
            if DATA_SOCKET.get().is_some() {
                let mut names: Vec<String> = crate::kernel::outputs::RELATIONS.iter().map(|r| r.to_string()).collect();
                names.extend(prog.observes.iter().map(|o| format!("ledger.{}", o.rel)));
                println!(
                    "run {}: the outputs are kernel.{}.{{{}}} in the data service's store (abt data query 'kernel.{}.book')",
                    result.run_id,
                    result.run_id,
                    names.join(", "),
                    result.run_id
                );
            }
            if let Some(path) = args.opts.get("nav") {
                crate::dump::write_nav(&result, Path::new(path)).unwrap_or_else(|e| {
                    eprintln!("error: --nav: {}", e);
                    std::process::exit(1);
                });
            }
            for w in &result.warnings {
                println!("warning ({}): {}", w.bias, w.message);
            }
            if !args.flags.contains("quiet") {
                let shown = if args.flags.contains("all") { result.decisions.len() } else { 20 };
                for d in result.decisions.iter().take(shown) {
                    println!("  {} {} (rule {})", format_timestamp(d.t), result.describe_decision(&d.decision), prog.rule_label(d.rule));
                }
                if result.decisions.len() > shown {
                    println!("  ... {} more (--all prints every decision)", result.decisions.len() - shown);
                }
                if args.flags.contains("fills") {
                    for f in &result.fills {
                        println!(
                            "  fill {} {} {:+} @ {:.4}{}",
                            format_timestamp(f.t),
                            result.symbols[f.equity as usize],
                            f.quantity,
                            f.price,
                            match (f.forced, f.at_last_price, f.partial) {
                                (true, _, _) => " (forced: margin call or delisting)".to_string(),
                                (_, true, true) => format!(" (last price, partial at {:.1}% of volume)", f.participation * 100.0),
                                (_, true, false) => " (last price)".to_string(),
                                (_, false, true) => format!(" (partial at {:.1}% of volume)", f.participation * 100.0),
                                (_, false, false) => String::new(),
                            }
                        );
                    }
                }
                for (t, d, reason) in &result.dropped {
                    println!("  dropped {} {}: {}", format_timestamp(*t), result.describe_decision(d), reason);
                }
            }
            println!(
                "final cash {:.2}; positions: {}",
                result.final_cash,
                result
                    .final_positions
                    .iter()
                    .map(|(s, q)| format!("{}={}", result.symbols[*s as usize], q))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if let Some(dir) = args.opts.get("dump") {
                crate::dump::write_dump(&prog, &dataset, &cfg, &result, Path::new(dir)).unwrap_or_else(|e| {
                    eprintln!("{}", e);
                    exit(2)
                });
                eprintln!("dump written to {}", dir);
            }
            // A run outside a plan is a trial of its program's lineage once its
            // outputs are in a store that lasts (Proposition Q-2.2).
            if DATA_SOCKET.get().is_some() {
                println!("warning (data-snooping): this run is a trial its program's lineage counts (abt lineage); a plan counts its own (abt plan run)");
            } else {
                println!("warning (data-snooping): this run's store is temporary, so nothing counts it; run it over a data service (--data SOCKET) for its lineage to, or in a plan (abt plan run)");
            }
            if verify {
                let n = result.bars.len();
                let samples: Vec<i64> = (1..=5).map(|i| result.bars[(n * i / 6).min(n - 1)]).collect();
                match crate::kernel::verify_causality(&prog, &dataset, cfg, &samples) {
                    Ok(m) if m.is_empty() => println!("causality verified at {} sampled bars", samples.len()),
                    Ok(m) => {
                        for x in m {
                            println!("causality mismatch at {}: full {:?} vs truncated {:?}", format_timestamp(x.t), x.full, x.truncated);
                        }
                        exit(1)
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(1)
                    }
                }
            }
        }
        "bundle" => {
            // abt bundle build (--from DIR | --synthetic ...) --class CLASS --version V --out DIR <files...>
            // abt bundle test DIR <files...>
            let sub = args.files.first().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let rest: Vec<PathBuf> = args.files.iter().skip(1).cloned().collect();
            match sub.as_str() {
                "build" => {
                    let ws = workspace(&rest);
                    let env = args.opts.get("class").cloned().unwrap_or_else(|| usage(1));
                    let version = args.opts.get("version").cloned().unwrap_or_else(|| usage(1));
                    let out = args.opts.get("out").cloned().unwrap_or_else(|| usage(1));
                    // The bars the bundle holds: --base, else the class's own.
                    let base = args.opts.get("base").map(|b| {
                        crate::Resolution::parse(b.trim_start_matches('@')).unwrap_or_else(|| {
                            eprintln!("--base: `{}` is not a resolution (@1m, @5m, @15m, @30m, @1h, @1d, @1w)", b);
                            exit(2)
                        })
                    });
                    let prog = class_program_or_exit(&ws, &env, base);
                    let ds = if args.flags.contains("synthetic") {
                        synthetic_for(&prog, &args.opts)
                    } else if let Some(from) = args.opts.get("from") {
                        let (ds, notes) = data::load_parquet_dir(&prog, Path::new(from)).unwrap_or_else(|e| {
                            eprintln!("{}", e);
                            exit(2)
                        });
                        for n in notes {
                            eprintln!("note: {}", n);
                        }
                        ds
                    } else {
                        usage(1)
                    };
                    let m = crate::bundle::write_bundle(&prog, &ds, Path::new(&out), &env, &version).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    });
                    println!(
                        "wrote bundle `{}@{}` to {}: {} relations, untested (run `abt bundle test {}`)",
                        m.name,
                        m.version,
                        out,
                        m.relations.len(),
                        out
                    );
                }
                "test" => {
                    let dir = rest.first().cloned().unwrap_or_else(|| usage(1));
                    let files: Vec<PathBuf> = rest.iter().skip(1).cloned().collect();
                    let ws = workspace(&files);
                    let m = crate::bundle::read_manifest(&dir).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    });
                    let prog = class_program_or_exit(&ws, &m.name, None);
                    let (m, results) = crate::bundle::test_bundle(&prog, &dir).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    });
                    for r in &results {
                        println!("{} {}: {}", if r.passed { "pass" } else { "FAIL" }, r.name, r.detail);
                    }
                    let failed = results.iter().filter(|r| !r.passed).count();
                    println!(
                        "bundle `{}@{}`: {} tests, {} failed{}",
                        m.name,
                        m.version,
                        results.len(),
                        failed,
                        if failed == 0 { "; recorded in the manifest" } else { "; not marked tested" }
                    );
                    if failed > 0 {
                        exit(1)
                    }
                }
                _ => usage(1),
            }
        }
        "synth" => {
            let ws = workspace(&args.files);
            let env = args.opts.get("class").cloned().unwrap_or_else(|| usage(1));
            let out = args.opts.get("out").cloned().unwrap_or_else(|| usage(1));
            // The writer needs a program whose demand covers the class whole.
            let prog = class_program_or_exit(&ws, &env, None);
            let ds = synthetic_for(&prog, &args.opts);
            data::write_parquet_dir(&prog, &ds, Path::new(&out)).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            println!("wrote {} relations to {}", ds.facts.len(), out);
        }
        _ => usage(1),
    }
}

/// The text or JSON of a compiler stage for `abt ir --stage`.
fn stage_text(args: &Args, prog: &crate::check::Program, stage: &str, json: bool) -> Result<String, String> {
    use crate::compiler::Stage;
    let render = |s: &dyn Stage| -> String {
        if json {
            format!("{}\n", serde_json::to_string_pretty(&s.json()).unwrap_or_default())
        } else {
            s.text()
        }
    };
    match stage {
        "checked" => Ok(if json {
            format!("{}\n", serde_json::to_string_pretty(&crate::compiler::print::checked_json(prog)).unwrap_or_default())
        } else {
            crate::compiler::print::render_checked(prog)
        }),
        "normalised" | "normalized" => {
            let nir = crate::compiler::normalise(prog);
            Ok(render(&crate::compiler::normalise::NirStage { nir: &nir, prog }))
        }
        "analysed" | "analyzed" => {
            let opts = compile_options(args, prog);
            let compiled = crate::compiler::compile(prog, &opts);
            Ok(render(&compiled.analysis))
        }
        "rewritten" => {
            let opts = compile_options(args, prog);
            let compiled = crate::compiler::compile(prog, &opts);
            Ok(render(&crate::rewrite::RewrittenStage { rewritten: &compiled.rewritten, prog }))
        }
        "planned" => {
            let opts = compile_options(args, prog);
            let compiled = crate::compiler::compile(prog, &opts);
            Ok(if json {
                format!("{}\n", serde_json::to_string_pretty(&compiled.plan.json()).unwrap_or_default())
            } else {
                compiled.plan.text()
            })
        }
        other => Err(format!("--stage: `{}` is not one of parsed, checked, normalised, analysed, rewritten, planned", other)),
    }
}

/// The compiler's options from the command line: the numerics setting
/// (`--numerics`, or `--window-sums`), whether to optimise (`--plan
/// written|optimised`), the statistics of the data when one is named, the
/// solver and its budgets.
fn compile_options(args: &Args, prog: &crate::check::Program) -> crate::compiler::CompileOptions {
    compile_options_with(args, prog, None)
}

/// `compile_options` with a dataset already loaded (its statistics feed the
/// planner and the bundle-relative verdicts).
fn compile_options_with(args: &Args, prog: &crate::check::Program, loaded: Option<(&crate::kernel::Dataset, &str)>) -> crate::compiler::CompileOptions {
    use crate::compiler::CompileOptions;
    // The execution block's `numerics` field selects the aggregate implementation.
    let numerics = crate::lower::Numerics::from_running_sums(exec_config(args, prog).running_sums);
    let optimise = match args.opts.get("plan").map(|s| s.as_str()) {
        None | Some("optimised") | Some("optimized") => true,
        Some("written") => false,
        Some(other) => {
            eprintln!("--plan: `{}` is not one of written, optimised", other);
            exit(2)
        }
    };
    let has_data = args.flags.contains("synthetic") || args.opts.contains_key("data") || args.opts.contains_key("bundle");
    let stats = match loaded {
        Some((ds, label)) => Some(crate::plan::cost::Statistics::from_dataset(prog, ds, label).with_start(prog, ds, exec_config(args, prog).start)),
        None if has_data => {
            let (ds, label) = load_dataset(args, prog);
            let cfg = exec_config(args, prog);
            Some(
                crate::plan::cost::Statistics::from_dataset(prog, &ds, &label.unwrap_or_else(|| if args.flags.contains("synthetic") { "synthetic".into() } else { "data".into() }))
                    .with_start(prog, &ds, cfg.start),
            )
        }
        None => None,
    };
    CompileOptions {
        numerics,
        optimise,
        stats,
        solver: args.opts.get("solver").cloned(),
        replay_dir: args.opts.get("replay-dir").map(PathBuf::from),
        ..CompileOptions::default()
    }
}
