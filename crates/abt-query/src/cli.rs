//! The query layer's commands (Q-9). `abt metrics --run ID`: a run's
//! metrics, computed from the tables the kernel wrote to the store (Q-3.1).

use std::collections::BTreeMap;

use abt_wire::transport::Connection;

use crate::data::Data;
use crate::metrics;
use crate::run::Run;

/// Where a command writes: the process's standard output and error (the
/// front), or buffers the query service answers with (P-3.5).
pub trait Io {
    fn out(&mut self, s: &str);
    fn err(&mut self, s: &str);
}

/// Standard output and error.
pub struct Console;

impl Io for Console {
    fn out(&mut self, s: &str) {
        print!("{}", s);
    }

    fn err(&mut self, s: &str) {
        eprint!("{}", s);
    }
}

/// What a command wrote, kept.
#[derive(Clone, Debug, Default)]
pub struct Captured {
    pub out: String,
    pub err: String,
}

impl Io for Captured {
    fn out(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn err(&mut self, s: &str) {
        self.err.push_str(s);
    }
}

/// A line to a command's output.
macro_rules! say {
    ($io:expr) => {
        $io.out("\n")
    };
    ($io:expr, $($t:tt)*) => {
        $io.out(&format!("{}\n", format_args!($($t)*)))
    };
}

/// Text to a command's output, as it is.
macro_rules! put {
    ($io:expr, $($t:tt)*) => {
        $io.out(&format!($($t)*))
    };
}

/// A line to a command's error output.
macro_rules! complain {
    ($io:expr, $($t:tt)*) => {
        $io.err(&format!("{}\n", format_args!($($t)*)))
    };
}

/// A run's metrics as one record (`--json`), with the versions of the
/// tables they were computed from (Q-3.1: every metric is recorded with its
/// tables' versions).
#[derive(Clone, Debug, serde::Serialize)]
pub struct RunMetrics {
    pub run: String,
    pub periods_per_year: f64,
    pub returns: metrics::ReturnMetrics,
    pub trading: metrics::TradingMetrics,
    pub convention: metrics::ConventionMetrics,
    /// The NAV at which the annual impact drag reaches 5 %, when the run
    /// paid impact.
    pub capacity: Option<f64>,
    pub subperiods: Vec<(i64, f64)>,
    pub book: metrics::BookMetrics,
    /// Present when the run held an option (E-73).
    pub options: Option<metrics::OptionsMetrics>,
    pub versions: BTreeMap<String, String>,
}

/// The metrics of `run`.
pub fn run_metrics(data: &Data, run: &str) -> Result<RunMetrics, crate::data::Error> {
    let r = Run::read(data, run)?;
    let ppy = r.periods_per_year();
    let (by_day, days) = r.periods();
    Ok(RunMetrics {
        run: run.to_string(),
        periods_per_year: ppy,
        returns: metrics::return_metrics(&r.metric_curve(), ppy),
        trading: metrics::trading_metrics(&r, ppy),
        convention: metrics::convention_metrics_on(&r, ppy, by_day, days.as_ref()),
        capacity: metrics::capacity(&r, ppy, 0.05),
        subperiods: metrics::subperiod_sharpes(&r.metric_curve(), ppy, 20),
        book: metrics::book_metrics(&r),
        options: metrics::options_metrics(&r, ppy),
        versions: data.versions(&format!("kernel.{}.", run))?,
    })
}

/// What `abt run` prints of a run's returns, and the returns per reporting
/// period `--returns` writes (`t`, return, P&L).
#[derive(Clone, Debug)]
pub struct RunLines {
    pub lines: Vec<String>,
    pub returns: Vec<(i64, f64, f64)>,
}

/// `abt run`'s account of the run `run` the kernel wrote (the front's
/// reporter, E-66): the NAV's start, end, total return and deepest
/// drawdown, the accounting, and the report block's period metrics over
/// `calendar`, as the kernel printed them before the metrics moved here.
pub fn run_lines(conn: Connection, run: &str, calendar: &str) -> Result<RunLines, String> {
    let data = Data::connect(conn).map_err(|e| e.to_string())?;
    let r = Run::read(&data, run).map_err(|e| e.to_string())?;
    let ppy = r.periods_per_year();
    let (by_day, days) = r.periods();
    let mut lines: Vec<String> = metrics::summary(&r.metric_curve()).into_iter().map(|(k, v)| format!("{:>14}: {:.4}", k, v)).collect();
    if let Some(note) = r.report.get("period_note").and_then(|n| n.as_str()) {
        lines.push(format!("note: {}", note));
    }
    let cm = metrics::convention_metrics_on(&r, ppy, by_day, days.as_ref());
    lines.push(match r.base_capital() {
        Some(b) => format!(
            "accounting: fixed base {:.2}; a period's return is the NAV change over it (not reinvested); end_equity and total_return above compound those period returns, total_pnl below is additive on the base",
            b
        ),
        None => "accounting: compounding; a period's return is the NAV change over the previous NAV".to_string(),
    });
    lines.push(format!(
        "metrics per {} period ({} a year, {} periods): cagr {:.6}   max_drawdown {:.6}   sharpe {:.4} (population {:.4})   volatility {:.4}",
        calendar, ppy, cm.periods, cm.cagr, cm.max_drawdown, cm.sharpe, cm.sharpe_population, cm.volatility_population
    ));
    lines.push(format!(
        "additive: annual_return {:.6}   max_drawdown {:.6}   total_pnl {:.2}   profit_factor {:.3} per period, {:.3} per trade   trades {}   win_rate {:.3}",
        cm.annual_return, cm.max_drawdown_additive, cm.total_pnl, cm.profit_factor, cm.profit_factor_trades, cm.trades, cm.win_rate
    ));
    Ok(RunLines {
        lines,
        returns: metrics::period_returns_on(&r, by_day, days.as_ref()),
    })
}

fn usage(io: &mut dyn Io) -> i32 {
    complain!(io, "usage:\n  abt metrics --run RUN (--data SOCKET | --store DIR | --query SOCKET) [--json]   (a run's metrics, from the tables it wrote to the store: returns, trading, the report block's periods, capacity, sub-period Sharpe ratios)");
    2
}

/// `abt metrics` over a connection to the data service; the exit code.
pub fn metrics_main(args: &[String], conn: Connection) -> i32 {
    metrics_main_io(args, conn, &mut Console)
}

/// `abt metrics`, written to `io`.
pub fn metrics_main_io(args: &[String], conn: Connection, io: &mut dyn Io) -> i32 {
    let mut run = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--run" => {
                run = args.get(i + 1).cloned();
                i += 1;
            }
            "--json" => json = true,
            "--data" | "--store" => i += 1,
            _ => return usage(io),
        }
        i += 1;
    }
    let Some(run) = run else { return usage(io) };
    let data = match Data::connect(conn) {
        Ok(d) => d,
        Err(e) => {
            complain!(io, "{}", e);
            return 2;
        }
    };
    let m = match run_metrics(&data, &run) {
        Ok(m) => m,
        Err(e) => {
            complain!(io, "{}", e);
            return 1;
        }
    };
    if json {
        say!(io, "{}", serde_json::to_string(&m).unwrap_or_default());
        return 0;
    }
    let r = &m.returns;
    say!(io, "run {}", m.run);
    say!(io,
        "returns ({} a year, {} returns): total {:.6}   cagr {:.6}   volatility {:.6}   sharpe {:.4} (Lo {:.4}, se {:.4})   sortino {:.4}   max_drawdown {:.6} over {} bars   cvar_5 {:.6}   skew {:.4}   kurtosis {:.4}   worst_month {:.6}   t {:.4}   rho_1 {:.4}",
        m.periods_per_year, r.n, r.total_return, r.cagr, r.volatility, r.sharpe, r.sharpe_lo, r.sharpe_se, r.sortino, r.max_drawdown, r.max_drawdown_duration, r.cvar_5, r.skew, r.kurtosis, r.worst_month, r.t_stat, r.autocorrelation_1
    );
    let t = &m.trading;
    say!(io,
        "trading: decisions {}   fills {}   dropped {}   turnover {:.4}   fill_ratio {:.4}   participation {:.4} (max {:.4})   gross {:.4}   net {:.4}   max_leverage {:.4}   cost_drag {:.6}   impact_drag {:.6}",
        t.decisions, t.fills, t.dropped, t.turnover, t.fill_ratio, t.avg_participation, t.max_participation, t.avg_gross, t.avg_net, t.max_leverage, t.cost_drag, t.impact_drag
    );
    let c = &m.convention;
    say!(io,
        "periods ({} a year, {} periods): cagr {:.6}   max_drawdown {:.6}   sharpe {:.4} (population {:.4})   annual_return {:.6}   max_drawdown_additive {:.6}   total_pnl {:.2}   profit_factor {:.3} per period, {:.3} per trade   trades {}   win_rate {:.3}",
        c.periods_per_year, c.periods, c.cagr, c.max_drawdown, c.sharpe, c.sharpe_population, c.annual_return, c.max_drawdown_additive, c.total_pnl, c.profit_factor, c.profit_factor_trades, c.trades, c.win_rate
    );
    match m.capacity {
        Some(x) => say!(io, "capacity at 5% impact drag: {:.0}", x),
        None => say!(io, "capacity: unmeasured (the run paid no impact)"),
    }
    for (y, s) in &m.subperiods {
        say!(io, "sharpe {}: {:.4}", y, s);
    }
    let b = &m.book;
    say!(
        io,
        "book: margin_utilisation {:.4} (max {:.4}, maintenance max {:.4})   max_weight {:.4}   effective_names {:.2}   funding_paid {:.2}",
        b.margin_utilisation,
        b.max_margin_utilisation,
        b.max_maintenance_utilisation,
        b.max_weight,
        b.effective_names,
        b.funding_paid
    );
    if let Some(o) = &m.options {
        let days = |d: Option<f64>| d.map(|d| format!("{:.1}", d)).unwrap_or_else(|| "-".into());
        say!(
            io,
            "options: premium paid {:.2} collected {:.2}   exercises {}   assignments {}   expiries {}   days to expiry at entry {} at exit {}",
            o.premium_paid,
            o.premium_collected,
            o.exercises,
            o.assignments,
            o.expiries,
            days(o.days_to_expiry_entry),
            days(o.days_to_expiry_exit)
        );
        let range = |f: &dyn Fn(&metrics::options::NetGreeks) -> f64| {
            let v: Vec<f64> = o.greeks.iter().map(f).collect();
            let mean = if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
            (mean, v.iter().map(|x| x.abs()).fold(0.0, f64::max))
        };
        let (d, g, v, t) = (range(&|x| x.delta_money), range(&|x| x.gamma), range(&|x| x.vega), range(&|x| x.theta));
        say!(
            io,
            "options greeks over {} bars (mean, max |.|): delta·S {:.2}, {:.2}   gamma {:.4}, {:.4}   vega {:.2}, {:.2}   theta {:.2}, {:.2}",
            o.greeks.len(),
            d.0,
            d.1,
            g.0,
            g.1,
            v.0,
            v.1,
            t.0,
            t.1
        );
        let a = &o.attribution;
        say!(
            io,
            "options attribution over {} bars: pnl {:.2} = delta {:.2} + gamma {:.2} + theta {:.2} + vega {:.2} + residual {:.2}   unattributed {} bars, {:.2}",
            a.bars,
            a.pnl,
            a.delta,
            a.gamma,
            a.theta,
            a.vega,
            a.residual,
            a.unattributed,
            a.unattributed_pnl
        );
        for u in &o.volatility {
            let v = |x: Option<f64>| x.map(|x| format!("{:.4}", x)).unwrap_or_else(|| "-".into());
            say!(io, "volatility of {}: realised {} over {} returns, implied {}", u.underlying, v(u.realised), u.returns, v(u.implied));
        }
    }
    for (rel, v) in &m.versions {
        say!(io, "read {} at {}", rel, v);
    }
    0
}

// ---- plans, lineages and disputes (Q-9) ----

use std::path::{Path, PathBuf};

use abt_wire::generated::abt::wire::SourceFile;

use crate::kernel::{Kernel, Program};
use crate::plan::{self, exec, Plan, ProgramRef, Step};

/// A command's arguments: its positional words and its options.
struct Args {
    positional: Vec<String>,
    opts: BTreeMap<String, String>,
    flags: Vec<String>,
    /// Every `--with NAME=VALUE`, in order.
    with: Vec<(String, String)>,
}

/// The options every command takes or the front consumes; the flags.
const OPTS: [&str; 15] = [
    "study", "threads", "reason", "data", "store", "kernel", "kernels", "run", "binding", "over", "relation", "seed", "briefs", "attempts", "out",
];
const FLAGS: [&str; 2] = ["json", "dry-run"];

fn args(raw: &[String]) -> Result<Args, String> {
    let mut a = Args {
        positional: vec![],
        opts: BTreeMap::new(),
        flags: vec![],
        with: vec![],
    };
    let mut i = 0;
    while i < raw.len() {
        match raw[i].strip_prefix("--") {
            Some(f) if FLAGS.contains(&f) => a.flags.push(f.to_string()),
            Some("with") => {
                let v = raw.get(i + 1).ok_or("--with needs NAME=VALUE")?;
                let (k, x) = v.split_once('=').ok_or_else(|| format!("--with takes NAME=VALUE, not `{}`", v))?;
                a.with.push((k.trim().to_string(), x.trim().to_string()));
                i += 1;
            }
            Some(o) if OPTS.contains(&o) => {
                let v = raw.get(i + 1).ok_or_else(|| format!("--{} needs a value", o))?;
                a.opts.insert(o.to_string(), v.clone());
                i += 1;
            }
            Some(o) => return Err(format!("unknown option --{}", o)),
            None => a.positional.push(raw[i].clone()),
        }
        i += 1;
    }
    Ok(a)
}

/// Every `.dsl` file under the named files and directories, in path order.
fn dsl_sources(paths: &[String]) -> Result<Vec<SourceFile>, String> {
    fn collect(p: &Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    collect(&e.path(), out);
                }
            }
        } else if p.extension().is_some_and(|x| x == "dsl") {
            out.push(p.to_path_buf());
        }
    }
    let mut files = Vec::new();
    for f in paths {
        if !Path::new(f).exists() {
            return Err(format!("{}: no such file or directory", f));
        }
        collect(Path::new(f), &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|p| {
            Ok(SourceFile {
                text: std::fs::read_to_string(&p).map_err(|e| format!("{}: {}", p.display(), e))?,
                path: Some(p.to_string_lossy().into_owned()),
            })
        })
        .collect()
}

/// The first strategy a source declares.
fn strategy_of(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("strategy ")
                .map(|r| r.split(|c: char| c.is_whitespace() || c == '{').next().unwrap_or("").to_string())
        })
        .filter(|s| !s.is_empty())
}

/// The plan a file holds (`--study NAME` among several), and its program:
/// a path is relative to the plan file and joins the workspace the other
/// positional sources make.
fn load(a: &Args) -> Result<(Plan, Program), String> {
    let file = a.positional.first().ok_or("a plan file names the study")?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {}", file, e))?;
    let p = select(&text, file, a.opts.get("study"))?;
    let rest: Vec<String> = a.positional.iter().skip(1).cloned().collect();
    let mut sources = dsl_sources(&rest)?;
    let strategy = match &p.program {
        ProgramRef::Name(n) => n.clone(),
        ProgramRef::Path(path) => {
            let full = Path::new(file).parent().unwrap_or(Path::new(".")).join(path);
            let text = std::fs::read_to_string(&full).map_err(|e| format!("study {}: program {}: {}", p.name, full.display(), e))?;
            let s = strategy_of(&text).ok_or_else(|| format!("study {}: {} declares no strategy", p.name, full.display()))?;
            let full_s = full.to_string_lossy().into_owned();
            if !sources.iter().any(|f| f.path.as_deref() == Some(full_s.as_str()) || f.text == text) {
                sources.push(SourceFile { text, path: Some(full_s) });
            }
            s
        }
    };
    Ok((p, Program { sources, strategy }))
}

/// The plan among a text's units (`origin` names the text): the one
/// `--study` names, else its only study.
fn select(text: &str, origin: &str, study: Option<&String>) -> Result<Plan, String> {
    let units = plan::parse(text).map_err(|e| format!("{}: {}", origin, e))?;
    let name = match study {
        Some(n) => n.clone(),
        None => {
            let studies: Vec<&str> = units.iter().filter(|u| !matches!(u, plan::Unit::Template { .. })).map(plan::Unit::name).collect();
            match studies.as_slice() {
                [one] => one.to_string(),
                [] => return Err(format!("{} holds no study", origin)),
                _ => return Err(format!("{} holds several studies ({}): name one with --study", origin, studies.join(", "))),
            }
        }
    };
    plan::plan(&units, &name).map_err(|e| format!("{}: {}", origin, e))
}

fn kernels(conns: Vec<abt_wire::transport::Connection>) -> Result<Vec<Kernel>, String> {
    conns.into_iter().map(|c| Kernel::connect(c).map_err(|e| e.to_string())).collect()
}

fn short(h: &str) -> &str {
    &h[..16.min(h.len())]
}

fn plan_usage(io: &mut dyn Io) -> i32 {
    complain!(io,
        "usage:\n  abt plan check  PLAN [SOURCES...] [--study NAME] [--json]   (the plan's normal form, its hash H(Π), its steps in order and the trials it will count)\n  abt plan run    PLAN [SOURCES...] [--study NAME] [--threads N] [--dry-run] [--json]   (every step but the reveals; the runs, trials and outcomes written to the data layer)\n  abt plan reveal PLAN STEP [SOURCES...] [--study NAME] [--json]   (one reveal step: the committed point over the whole sample, the embargoed bars reported)\n  abt plan report PLAN [SOURCES...] [--study NAME] [--json]   (the latest execution's outcomes, from the data layer)\n  abt plan drop   PLAN [SOURCES...] [--study NAME]   (the plan leaves query.plans; its bindings are unpinned; its trials stay)\n  (--store DIR | --data SOCKET) [--kernel SOCKET | --kernels N]: the data layer, and the kernels the runs are spread over; or --query SOCKET, the query service (abt query serve) that runs the command"
    );
    2
}

/// What a plan will count: per step, its runs (a walk-forward's per fold).
fn expected(p: &Plan, folds: Option<usize>) -> Vec<(String, String)> {
    let points = p.grid_points().len();
    p.steps
        .iter()
        .filter_map(|s| match s {
            Step::Run { name, .. } => Some((name.clone(), "1 run".to_string())),
            Step::Grid { name } => Some((name.clone(), format!("{} runs, one per point", points))),
            Step::WalkForward { name } => Some((
                name.clone(),
                match folds {
                    Some(f) => format!("{} runs: {} folds of {} train runs and one test", f * (points + 1), f, points),
                    None => format!("{} runs per fold", points + 1),
                },
            )),
            Step::Reveal { name, .. } => Some((name.clone(), "1 run when revealed".to_string())),
            Step::Test { name, .. } => Some((name.clone(), "1 test".to_string())),
            _ => None,
        })
        .chain(p.claims.iter().map(|c| (c.name.clone(), "1 claim judged, and one more per reveal".to_string())))
        .collect()
}

/// `abt plan`; the exit code.
pub fn plan_main(raw: &[String], data: Connection, kernel_conns: Vec<Connection>) -> i32 {
    plan_main_io(raw, data, kernel_conns, &mut Console)
}

/// `abt plan`, its plan and program read from the files it names, written
/// to `io`.
pub fn plan_main_io(raw: &[String], data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> i32 {
    let Some(cmd) = raw.first().cloned() else { return plan_usage(io) };
    let mut a = match args(&raw[1..]) {
        Ok(a) => a,
        Err(e) => {
            complain!(io, "{}", e);
            return plan_usage(io);
        }
    };
    let Some(reveal) = plan_operands(&cmd, &mut a) else { return plan_usage(io) };
    match load(&a).and_then(|(p, prog)| plan_on(&cmd, &a, reveal, p, prog, data, kernel_conns, io)) {
        Ok(code) => code,
        Err(e) => {
            complain!(io, "{}", e);
            1
        }
    }
}

/// `abt plan` as the query service runs it (P-3.5): the plan's text and its
/// program come with the request, and no file is read.
pub fn plan_served(raw: &[String], text: &str, prog: Program, data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> i32 {
    let Some(cmd) = raw.first().cloned() else { return plan_usage(io) };
    let mut a = match args(&raw[1..]) {
        Ok(a) => a,
        Err(e) => {
            complain!(io, "{}", e);
            return plan_usage(io);
        }
    };
    let Some(reveal) = plan_operands(&cmd, &mut a) else { return plan_usage(io) };
    match select(text, "the request's plan", a.opts.get("study")).and_then(|p| plan_on(&cmd, &a, reveal, p, prog, data, kernel_conns, io)) {
        Ok(code) => code,
        Err(e) => {
            complain!(io, "{}", e);
            1
        }
    }
}

/// What `abt plan` reads from files, for a request to the query service:
/// the plan's text and its program; `None` when the words are not a plan
/// command's, which the service answers with the usage.
pub fn plan_files(raw: &[String]) -> Result<Option<(String, Program)>, String> {
    let Some(cmd) = raw.first() else { return Ok(None) };
    let Ok(mut a) = args(&raw[1..]) else { return Ok(None) };
    if plan_operands(cmd, &mut a).is_none() {
        return Ok(None);
    }
    let file = a.positional.first().ok_or("a plan file names the study")?;
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {}", file, e))?;
    let (_, prog) = load(&a)?;
    Ok(Some((text, prog)))
}

/// A plan command's reveal step, taken out of its operands (`reveal PLAN
/// STEP`); `None` when the command is not a plan command or lacks its step.
fn plan_operands(cmd: &str, a: &mut Args) -> Option<Option<String>> {
    if !matches!(cmd, "check" | "run" | "reveal" | "report" | "drop") {
        return None;
    }
    if cmd != "reveal" {
        return Some(None);
    }
    if a.positional.len() < 2 {
        return None;
    }
    Some(Some(a.positional.remove(1)))
}

#[allow(clippy::too_many_arguments)]
fn plan_on(cmd: &str, a: &Args, reveal: Option<String>, p: Plan, prog: Program, data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> Result<i32, String> {
    let json = a.flags.iter().any(|f| f == "json");
    let data = Data::connect(data).map_err(|e| e.to_string())?;
    let kernels = kernels(kernel_conns)?;
    let kernel = kernels.first().ok_or("a plan needs a kernel")?;
    let checked = kernel.check(&prog).map_err(|e| e.to_string())?;
    let normal = plan::normal_form(&p, &checked.program_hash)?;
    match cmd {
        "check" => {
            let sample = kernel.sample_received_by(&prog, &p.params, exec::received_by(&p.holdout)).map_err(|e| e.to_string())?;
            let folds = match (&p.walk_forward, sample.first, sample.last) {
                (Some(wf), Some(first), Some(last)) => {
                    let e = exec::embargo(&p.holdout, &sample, &p.window);
                    let end = if e.truncates { e.start().map_or(last, |s| (s - 1).min(last)) } else { last };
                    Some(wf.folds(first, end).len())
                }
                _ => None,
            };
            let dag: Vec<serde_json::Value> = normal
                .order
                .iter()
                .map(|(i, c)| {
                    let s = &p.steps[*i];
                    serde_json::json!({"canonical": c, "step": s.name(), "kind": s.kind(), "inputs": s.inputs()})
                })
                .collect();
            let trials = expected(&p, folds);
            if json {
                say!(
                    io,
                    "{}",
                    serde_json::json!({"plan": p.name, "hash": normal.hash, "program_hash": checked.program_hash, "normal_form": normal.text, "dag": dag, "trials_expected": trials, "binding": sample.binding, "bars": sample.bars})
                );
                return Ok(0);
            }
            say!(io, "plan {} {}", p.name, normal.hash);
            say!(io, "program {} {}", prog.strategy, checked.program_hash);
            say!(io, "sample {} decision bars on {} (binding {})", sample.bars, sample.calendar, short(&sample.binding));
            say!(io, "steps, in order:");
            for (i, c) in &normal.order {
                let s = &p.steps[*i];
                let inputs = s.inputs();
                say!(
                    io,
                    "  {:<4} {} {}{}",
                    c,
                    s.kind(),
                    s.name(),
                    if inputs.is_empty() { String::new() } else { format!(" (reads {})", inputs.join(", ")) }
                );
            }
            say!(io, "trials it will count:");
            for (step, what) in &trials {
                say!(io, "  {}: {}", step, what);
            }
            say!(io, "normal form:\n{}", normal.text);
            Ok(0)
        }
        "run" | "reveal" => {
            if a.flags.iter().any(|f| f == "dry-run") {
                say!(
                    io,
                    "plan {} {}: would run {}",
                    p.name,
                    normal.hash,
                    expected(&p, None).iter().map(|(s, w)| format!("{} ({})", s, w)).collect::<Vec<_>>().join(", ")
                );
                return Ok(0);
            }
            let opts = exec::Options {
                threads: a
                    .opts
                    .get("threads")
                    .map(|n| n.parse().map_err(|_| format!("--threads: `{}` is not a count", n)))
                    .transpose()?
                    .unwrap_or_else(|| exec::Options::default().threads),
                reveal,
                ..exec::Options::default()
            };
            let ex = exec::execute(&p, &prog, &exec::Endpoints { data: &data, kernels: &kernels }, &opts).map_err(|e| e.to_string())?;
            if json {
                say!(io, "{}", serde_json::to_string(&ex).map_err(|e| e.to_string())?);
            } else {
                print_execution(&ex, io);
            }
            Ok(0)
        }
        "report" if !json => {
            // The report the plan's latest execution wrote (Q-8): its text
            // when the report block asked for one, else its tables.
            let of = |n: &str| plan::trials::relation_of(&normal.hash, &format!("report.{}", n));
            if let Some(b) = crate::relation::read(&data, &of("text")).map_err(|e| e.to_string())? {
                say!(io, "plan {} {}", p.name, normal.hash);
                put!(
                    io,
                    "{}",
                    crate::relation::texts(&b, "text").map_err(|e| e.to_string())?.into_iter().flatten().next().unwrap_or_default()
                );
                return Ok(0);
            }
            let mut any = false;
            for t in ["summary", "trials", "claims", "tests", "contract", "warnings", "biases", "surface", "folds", "subperiods"] {
                if let Some(b) = crate::relation::read(&data, &of(t)).map_err(|e| e.to_string())? {
                    if !any {
                        say!(io, "plan {} {}", p.name, normal.hash);
                        any = true;
                    }
                    put!(io, "{}", stored_sheet(t, &b)?.text());
                }
            }
            if !any {
                return Err(format!("plan {} ({}) has written no report (`abt plan run` with a report step first)", p.name, short(&normal.hash)));
            }
            Ok(0)
        }
        "report" => {
            let stored = plan::trials::outcomes(&data, &normal.hash).map_err(|e| e.to_string())?;
            if stored.is_empty() {
                return Err(format!("plan {} ({}) has not run", p.name, short(&normal.hash)));
            }
            let b = crate::relation::read(&data, &plan::trials::relation_of(&normal.hash, "outcomes"))
                .map_err(|e| e.to_string())?
                .expect("outcomes");
            let cols = ["sharpe", "cagr", "max_drawdown", "objective"];
            let values: Vec<Vec<Option<f64>>> = cols.iter().map(|c| crate::relation::nums(&b, c)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            if json {
                let rows: Vec<serde_json::Value> = stored
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let mut v = serde_json::json!({"step": s.step, "run_id": s.run_id, "point": s.point, "best": s.best, "kind": s.kind});
                        for (c, col) in cols.iter().zip(&values) {
                            v[*c] = serde_json::json!(col[i]);
                        }
                        v
                    })
                    .collect();
                say!(io, "{}", serde_json::json!({"plan": p.name, "hash": normal.hash, "outcomes": rows}));
                return Ok(0);
            }
            say!(io, "plan {} {}", p.name, normal.hash);
            for (i, s) in stored.iter().enumerate() {
                let point: Vec<String> = s.point.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
                let f = |c: usize| values[c][i].map(|v| format!("{:.4}", v)).unwrap_or_else(|| "-".into());
                say!(
                    io,
                    "  {} {} {}{}  sharpe {}  cagr {}  max_drawdown {}  objective {}",
                    s.step,
                    short(&s.run_id),
                    point.join(" "),
                    if s.best { " (best)" } else { "" },
                    f(0),
                    f(1),
                    f(2),
                    f(3)
                );
            }
            Ok(0)
        }
        _ => {
            let row = plan::trials::drop_plan(&data, &normal.hash).map_err(|e| e.to_string())?;
            say!(
                io,
                "dropped plan {} {}: {} binding(s) no other plan holds are unpinned; its trials stay",
                row.name,
                row.hash,
                row.bindings.len()
            );
            Ok(0)
        }
    }
}

fn print_execution(ex: &exec::Execution, io: &mut dyn Io) {
    say!(io, "plan {} {}", ex.plan, ex.hash);
    say!(
        io,
        "program {} lineage {} holdout {}{}",
        short(&ex.program_hash),
        short(&ex.lineage),
        ex.holdout,
        if ex.embargo.is_empty() { String::new() } else { format!(" (embargo {})", ex.embargo.describe()) }
    );
    for o in &ex.outcomes {
        let point: Vec<String> = o.point.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        let fold = o.fold.as_ref().map(|f| format!(" fold {}/{} {}", f.k, f.of, f.phase)).unwrap_or_default();
        say!(
            io,
            "  {} {} {}{}{}  {}: sharpe {:.4}  cagr {:.4}  max_drawdown {:.4}  objective {:.4}",
            o.step,
            short(&o.run_id),
            point.join(" "),
            fold,
            if o.kind == exec::Kind::Reveal { " (reveal)" } else { "" },
            o.status,
            o.returns.sharpe,
            o.returns.cagr,
            o.returns.max_drawdown,
            o.objective
        );
    }
    for g in &ex.grids {
        let best = &ex.outcomes[g.runs[g.best]];
        say!(io, "grid {}: best {}", g.step, best.point.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(" "));
        if let Some(p) = &g.pbo {
            say!(io, "  PBO {:.3} over {} points", p.pbo, p.trials);
        }
        if let Some(s) = &g.surface {
            say!(io, "  surface: {:.0}% of {} neighbours within {:.3}", s.stability * 100.0, s.neighbours, s.tolerance);
        }
        for (y, s) in &g.subperiods {
            say!(io, "  sharpe {}: {:.4}", y, s);
        }
    }
    for w in &ex.walk_forwards {
        say!(
            io,
            "walk-forward {} ({}): {} folds, efficiency {}, out of sample sharpe {:.4}",
            w.step,
            w.scheme.describe(),
            w.folds.len(),
            w.efficiency.map(|e| format!("{:.3}", e)).unwrap_or_else(|| "undefined".into()),
            w.out_of_sample.sharpe
        );
    }
    for c in &ex.comparisons {
        say!(io, "compare {}:", c.step);
        for (run, values) in &c.rows {
            say!(io, "  {} {}", short(run), values.iter().map(|(m, v)| format!("{} {:.4}", m, v)).collect::<Vec<_>>().join("  "));
        }
    }
    for (step, rel) in &ex.ledgers {
        say!(io, "ledger {}: {}", step, rel);
    }
    for j in ex.claims.iter().chain(&ex.tests) {
        let r = j.result.as_ref();
        say!(
            io,
            "{} {}{}: {}@{} on {}  statistic {}  p {}  adjusted {}{}",
            j.kind,
            j.name,
            if j.in_sample { "" } else { " (out of sample)" },
            j.test,
            j.test_version,
            j.input,
            r.map(|r| format!("{:.4}", r.statistic)).unwrap_or_else(|| "-".into()),
            r.and_then(|r| r.p_value).map(|p| format!("{:.4}", p)).unwrap_or_else(|| "-".into()),
            j.p_adjusted.map(|p| format!("{:.4}", p)).unwrap_or_else(|| "-".into()),
            match (&j.verdict, &j.error) {
                (Some(v), _) => format!("  {}", v.text()),
                (None, Some(e)) => format!("  error: {}", e),
                _ => String::new(),
            }
        );
    }
    for s in &ex.sheets {
        put!(io, "{}", s.text());
    }
    for (step, why) in &ex.skipped {
        say!(io, "step {} did not run: {}", step, why);
    }
    for r in &ex.reports {
        say!(io, "report {}", r);
    }
    if let Some(d) = &ex.dsr {
        say!(io, "deflated sharpe {:.4} over {} trials in the lineage", d.dsr, ex.trials);
    } else {
        say!(io, "{} trials in the lineage", ex.trials);
    }
    for w in &ex.warnings {
        say!(io, "warning [{}]: {}", w.bias, w.message);
    }
}

fn lineage_usage(io: &mut dyn Io) -> i32 {
    complain!(io, "usage:\n  abt lineage PROGRAM [SOURCES...] [--json]   (the lineage of a strategy, a .dsl file or a name among SOURCES: its members, its trials by kind, its disputes)\n  abt dispute PROGRAM --reason TEXT [SOURCES...]   (a dispute of the strategy's attachment to its lineage, a row; the counts stand)\n  (--store DIR | --data SOCKET) [--kernel SOCKET], or --query SOCKET (the query service, abt query serve)");
    2
}

/// The program a `lineage` or `dispute` names: a `.dsl` file, or a
/// strategy among the sources.
fn named_program(a: &Args) -> Result<Program, String> {
    let first = a.positional.first().ok_or("a strategy, by file or by name")?;
    let mut sources = dsl_sources(&a.positional[1..])?;
    if first.ends_with(".dsl") {
        let text = std::fs::read_to_string(first).map_err(|e| format!("{}: {}", first, e))?;
        let strategy = strategy_of(&text).ok_or_else(|| format!("{} declares no strategy", first))?;
        sources.push(SourceFile { text, path: Some(first.clone()) });
        Ok(Program { sources, strategy })
    } else {
        Ok(Program { sources, strategy: first.clone() })
    }
}

/// `abt lineage` and `abt dispute`; the exit code.
pub fn lineage_main(cmd: &str, raw: &[String], data: Connection, kernel_conns: Vec<Connection>) -> i32 {
    lineage_main_io(cmd, raw, data, kernel_conns, &mut Console)
}

/// `abt lineage` and `abt dispute`, the program read from the files they
/// name, written to `io`.
pub fn lineage_main_io(cmd: &str, raw: &[String], data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> i32 {
    let a = match args(raw) {
        Ok(a) if !a.positional.is_empty() => a,
        Ok(_) => return lineage_usage(io),
        Err(e) => {
            complain!(io, "{}", e);
            return lineage_usage(io);
        }
    };
    match named_program(&a).and_then(|prog| lineage_on(cmd, &a, prog, data, kernel_conns, io)) {
        Ok(c) => c,
        Err(e) => {
            complain!(io, "{}", e);
            1
        }
    }
}

/// `abt lineage` and `abt dispute` as the query service runs them (P-3.5):
/// the program comes with the request.
pub fn lineage_served(cmd: &str, raw: &[String], prog: Program, data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> i32 {
    let a = match args(raw) {
        Ok(a) if !a.positional.is_empty() => a,
        Ok(_) => return lineage_usage(io),
        Err(e) => {
            complain!(io, "{}", e);
            return lineage_usage(io);
        }
    };
    match lineage_on(cmd, &a, prog, data, kernel_conns, io) {
        Ok(c) => c,
        Err(e) => {
            complain!(io, "{}", e);
            1
        }
    }
}

/// What `abt lineage` and `abt dispute` read from files, for a request to
/// the query service: the program they name; `None` when the words name
/// none, which the service answers with the usage.
pub fn lineage_files(raw: &[String]) -> Result<Option<Program>, String> {
    match args(raw) {
        Ok(a) if !a.positional.is_empty() => named_program(&a).map(Some),
        _ => Ok(None),
    }
}

fn lineage_on(cmd: &str, a: &Args, prog: Program, data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> Result<i32, String> {
    let data = Data::connect(data).map_err(|e| e.to_string())?;
    let kernels = kernels(kernel_conns)?;
    let checked = kernels.first().ok_or("a lineage needs a kernel")?.check(&prog).map_err(|e| e.to_string())?;
    let members = plan::lineage::members(&data).map_err(|e| e.to_string())?;
    let me = members
        .iter()
        .find(|m| m.program_hash == checked.program_hash)
        .ok_or_else(|| format!("strategy `{}` ({}) is in no lineage: no plan has run it", prog.strategy, short(&checked.program_hash)))?;
    if cmd == "dispute" {
        let reason = a.opts.get("reason").ok_or("a dispute gives its --reason")?;
        let d = plan::lineage::dispute(&data, &checked.program_hash, reason, exec::now()).map_err(|e| e.to_string())?;
        say!(io, "disputed {} in lineage {}: {}", short(&d.program_hash), short(&d.lineage), d.reason);
        return Ok(0);
    }
    let counted = plan::lineage::trials(&data, &me.lineage).map_err(|e| e.to_string())?;
    let disputes: Vec<plan::lineage::Dispute> = plan::lineage::disputes(&data).map_err(|e| e.to_string())?.into_iter().filter(|d| d.lineage == me.lineage).collect();
    let mine: Vec<&plan::lineage::Member> = members.iter().filter(|m| m.lineage == me.lineage).collect();
    if a.flags.iter().any(|f| f == "json") {
        say!(io, "{}", serde_json::json!({"lineage": me.lineage, "members": mine, "trials": counted, "disputes": disputes}));
        return Ok(0);
    }
    say!(io, "lineage {}", me.lineage);
    for m in &mine {
        let how = match (&m.similar_to, m.similarity) {
            (Some(to), Some(s)) => format!("similar to {} ({:.0}%)", short(to), s * 100.0),
            _ => m.attachment.clone(),
        };
        say!(io, "  {} {} {}", short(&m.program_hash), m.strategy, how);
    }
    say!(
        io,
        "trials {}: {}",
        counted.count,
        counted.by_kind.iter().map(|(k, n)| format!("{} {}", n, k)).collect::<Vec<_>>().join(", ")
    );
    for d in &disputes {
        say!(io, "dispute {}: {}", short(&d.program_hash), d.reason);
    }
    Ok(0)
}

/// A stored table as a sheet of text cells (its key columns left out).
fn stored_sheet(name: &str, b: &arrow::record_batch::RecordBatch) -> Result<crate::sheet::Sheet, String> {
    let names: Vec<String> = b.schema().fields().iter().map(|f| f.name().clone()).filter(|n| n != "row" && n != "t").collect();
    let mut sheet = crate::sheet::Sheet::new(name, &names.iter().map(String::as_str).collect::<Vec<_>>());
    let cols: Vec<Vec<Option<String>>> = names.iter().map(|n| crate::relation::texts(b, n).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
    for i in 0..b.num_rows() {
        sheet.push(cols.iter().map(|c| c[i].clone().map(crate::sheet::Cell::Text).unwrap_or(crate::sheet::Cell::Null)).collect());
    }
    Ok(sheet)
}

// ---- tests, claims, feed and activation statistics outside a plan (Q-9) ----

/// What a run's binding records (`kernel.<run>.binding`): its relations
/// with their versions, its program's hash and normal form, its id.
struct Bound {
    relations: Vec<(String, String)>,
    program_hash: String,
    normal_form: String,
    binding: String,
}

fn bound_of_run(data: &Data, run: &str) -> Result<Bound, String> {
    let rel = format!("kernel.{}.binding", run);
    let b = crate::relation::read(data, &rel).map_err(|e| e.to_string())?.ok_or_else(|| format!("the store holds no run {}", run))?;
    let t = |c: &str| crate::relation::texts(&b, c).map_err(|e| e.to_string());
    let (r, v, h, nf, id) = (t("relation")?, t("version")?, t("program_hash")?, t("normal_form")?, t("binding_id")?);
    Ok(Bound {
        relations: r.into_iter().zip(v).filter_map(|(r, v)| Some((r?, v?))).collect(),
        program_hash: h.into_iter().flatten().next().unwrap_or_default(),
        normal_form: nf.into_iter().flatten().next().unwrap_or_default(),
        binding: id.into_iter().flatten().next().unwrap_or_default(),
    })
}

/// A binding by its id: the first run recorded over it.
fn bound_of_binding(data: &Data, binding: &str) -> Result<Bound, String> {
    for (rel, _) in data.relations("kernel.", ".binding").map_err(|e| e.to_string())? {
        let run = rel.trim_start_matches("kernel.").trim_end_matches(".binding");
        let b = bound_of_run(data, run)?;
        if b.binding == binding || b.binding.starts_with(binding) {
            return Ok(b);
        }
    }
    Err(format!("no run in the store was bound to {}", binding))
}

fn window_arg(a: &Args) -> Result<(Option<i64>, Option<i64>), String> {
    match a.opts.get("over") {
        None => Ok((None, None)),
        Some(w) => {
            let (f, t) = w.split_once("..").ok_or_else(|| format!("--over takes FROM..TO, not `{}`", w))?;
            let end = |s: &str| crate::time::parse_timestamp(s).ok_or_else(|| format!("`{}` is not a time", s.trim()));
            Ok((Some(end(f)?), Some(end(t)?)))
        }
    }
}

fn query_usage(io: &mut dyn Io) -> i32 {
    complain!(io,
        "usage:\n  abt test TEST on INPUT [--run RUN] [--with NAME=VALUE]... [--over FROM..TO] [--json]   (one test outside a plan, counted: INPUT as a claim's, its runs named by id)\n  abt claim 'NAME : TEST on INPUT ... expect OUTCOME' --run RUN [--json]   (one claim on a run, counted)\n  abt feed STATISTIC (--binding B | --run RUN) [--relation R] [--over FROM..TO] [--json]   (coverage, revisions, latency, returns, liquidity, survivorship, actions, membership, chains)\n  abt activation --run RUN [--json]   (the rules' firing, their pairs, the cross-lag co-activation, the effective number of rules)\n  (--store DIR | --data SOCKET | --query SOCKET: the query service, abt query serve)"
    );
    2
}

/// `abt test`, `abt claim`, `abt feed` and `abt activation`; the exit code.
pub fn query_main(cmd: &str, raw: &[String], conn: Connection) -> i32 {
    query_main_io(cmd, raw, conn, &mut Console)
}

/// `abt test`, `abt claim`, `abt feed` and `abt activation`, written to
/// `io`.
pub fn query_main_io(cmd: &str, raw: &[String], conn: Connection, io: &mut dyn Io) -> i32 {
    let go = || -> Result<i32, String> {
        let a = args(raw)?;
        let json = a.flags.iter().any(|f| f == "json");
        let data = Data::connect(conn).map_err(|e| e.to_string())?;
        let mut run = a.opts.get("run").cloned();
        match cmd {
            "feed" => {
                let stat = a.positional.first().ok_or("abt feed STATISTIC")?;
                let bound = match (a.opts.get("binding"), &run) {
                    (Some(b), _) => bound_of_binding(&data, b)?,
                    (None, Some(r)) => bound_of_run(&data, r)?,
                    (None, None) => return Ok(query_usage(io)),
                };
                let s = crate::feed::statistic(&data, &bound.relations, stat, a.opts.get("relation").map(String::as_str), window_arg(&a)?).map_err(|e| e.to_string())?;
                if json {
                    say!(io, "{}", serde_json::to_string(&s).map_err(|e| e.to_string())?);
                } else {
                    put!(io, "{}", s.text());
                }
                Ok(0)
            }
            "activation" => {
                let run = run.ok_or("abt activation --run RUN")?;
                let bound = bound_of_run(&data, &run)?;
                let strategy = bound
                    .normal_form
                    .lines()
                    .find_map(|l| {
                        l.trim()
                            .strip_prefix("strategy ")
                            .map(|r| r.split(|c: char| c.is_whitespace() || c == '{').next().unwrap_or("").to_string())
                    })
                    .unwrap_or_default();
                let seed = a.opts.get("seed").map(|s| s.parse().map_err(|_| format!("--seed: `{}` is not a number", s))).transpose()?.unwrap_or(0);
                let act = crate::activation::activation(&data, &run, &strategy, seed, crate::activation::PHI_THRESHOLD).map_err(|e| e.to_string())?;
                if json {
                    say!(io, "{}", serde_json::to_string(&act).map_err(|e| e.to_string())?);
                } else {
                    for s in act.sheets() {
                        put!(io, "{}", s.text());
                    }
                }
                Ok(0)
            }
            "test" | "claim" => {
                // A claim's line, or a test's `TEST on INPUT` as one.
                let (claim, line) = if cmd == "claim" {
                    let line = a.positional.join(" ");
                    (plan::parse::claim(&line)?, line)
                } else {
                    let line = format!("test : {} expect accept", a.positional.join(" "));
                    (plan::parse::claim(&line).map_err(|e| e.replace("claim test: ", ""))?, line)
                };
                let mut params = claim.params.clone();
                params.extend(a.with.iter().cloned());
                // Outside a plan an input names its run by id (`returns of
                // RUN`), or `--run` does.
                if run.is_none() {
                    run = claim.input.steps().into_iter().find(|s| s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit()));
                }
                let run_id = run.clone();
                let bound = match (&run_id, a.opts.get("binding")) {
                    (Some(r), _) => bound_of_run(&data, r)?,
                    (None, Some(b)) => bound_of_binding(&data, b)?,
                    (None, None) => match &claim.input {
                        plan::Input::Feed { .. } => return Err("a feed's input needs --binding B or --run RUN".into()),
                        _ => return Err("the input reads a run: name it with --run RUN".into()),
                    },
                };
                let strategy = bound
                    .normal_form
                    .lines()
                    .find_map(|l| {
                        l.trim()
                            .strip_prefix("strategy ")
                            .map(|r| r.split(|c: char| c.is_whitespace() || c == '{').next().unwrap_or("").to_string())
                    })
                    .unwrap_or_default();
                let r2 = run_id.clone();
                let ctx = plan::claims::Context {
                    data: &data,
                    strategy: &strategy,
                    binding: &bound.relations,
                    // Outside a plan, every step an input names is the run.
                    runs: Box::new(move |_| {
                        r2.clone()
                            .map(|r| vec![r])
                            .ok_or_else(|| crate::data::Error::Missing("the input reads a run: name it with --run RUN".into()))
                    }),
                    ledger: Box::new({
                        let r3 = run_id.clone();
                        move |s: &str| {
                            r3.clone()
                                .map(|r| format!("kernel.{}.{}", r, s))
                                .ok_or_else(|| crate::data::Error::Missing("--run RUN names the run".into()))
                        }
                    }),
                    folds: Box::new(|_: &str| None),
                    defaults: crate::tests::Params::new(),
                    plan_hash: "",
                };
                let window = window_arg(&a)?;
                let keep = move |t: i64| window.0.is_none_or(|f| t >= f) && window.1.is_none_or(|e| t <= e);
                let mut judged = if cmd == "claim" {
                    let mut c = claim.clone();
                    c.params = params;
                    plan::claims::judge(&ctx, &[(&c, c.name.clone())], &keep, true).remove(0)
                } else {
                    plan::claims::apply(&ctx, "test", "test", "test", &claim.test, &claim.input, &claim.over, &params, &keep, true)
                };
                // Counted (Q-9): a trial of the program's lineage.
                let checked = crate::kernel::Checked {
                    program_hash: bound.program_hash.clone(),
                    normal_form: bound.normal_form.clone(),
                    diagnostics: vec![],
                };
                let now = exec::now();
                let opened = plan::lineage::open(&data, &checked, &strategy, now).map_err(|e| e.to_string())?;
                if judged.trial_id.is_empty() {
                    judged.trial_id = abt_wire::Hash::of(format!("{}:{}:{}", line, run_id.clone().unwrap_or_default(), now).as_bytes()).hex();
                }
                plan::trials::write_judged(&data, &opened.lineage, &bound.program_hash, &bound.binding, now, &judged).map_err(|e| e.to_string())?;
                if json {
                    say!(io, "{}", serde_json::to_string(&judged).map_err(|e| e.to_string())?);
                    return Ok(0);
                }
                let r = judged.result.as_ref();
                say!(io, "{}@{} on {}", judged.test, judged.test_version, judged.input);
                if let Some(r) = r {
                    say!(
                        io,
                        "statistic {:.6}  p {}  effect {}  interval {}  n {}",
                        r.statistic,
                        r.p_value.map(|p| format!("{:.6}", p)).unwrap_or_else(|| "-".into()),
                        r.effect.map(|e| format!("{:.6}", e)).unwrap_or_else(|| "-".into()),
                        r.interval.map(|(l, h, c)| format!("[{:.6}, {:.6}] at {}", l, h, c)).unwrap_or_else(|| "-".into()),
                        r.n
                    );
                    for n in &r.notes {
                        say!(io, "note: {}", n);
                    }
                }
                if let Some(e) = &judged.error {
                    say!(io, "error: {}", e);
                }
                if let Some(v) = &judged.verdict {
                    say!(io, "verdict: {}", v.text());
                }
                say!(io, "dependence {}", judged.dependence);
                say!(io, "counted in lineage {}", short(&opened.lineage));
                Ok(if judged.error.is_some() && judged.result.is_none() { 1 } else { 0 })
            }
            _ => Ok(query_usage(io)),
        }
    };
    go().unwrap_or_else(|e| {
        complain!(io, "{}", e);
        1
    })
}

fn briefs_usage(io: &mut dyn Io) -> i32 {
    complain!(io,
        "usage:\n  abt briefs report --briefs DIR --attempts DIR [--out FILE] <files...>   (every brief with the model's attempts at it, ATTEMPTS/<brief>/<n>.dsl, checked against the files' library; the first valid attempt run as an instance of the template `briefs_harness`; the attempts recorded as rows of query.briefs.attempts; --out FILE writes the report as JSON)"
    );
    2
}

/// `abt briefs report` over the data service and kernels the front set up
/// (a market the attempts run on); the exit code.
pub fn briefs_main(raw: &[String], data: Connection, kernel_conns: Vec<Connection>) -> i32 {
    briefs_main_io(raw, data, kernel_conns, &mut Console)
}

/// `abt briefs report`, written to `io`.
pub fn briefs_main_io(raw: &[String], data: Connection, kernel_conns: Vec<Connection>, io: &mut dyn Io) -> i32 {
    let run = || -> Result<i32, String> {
        let mut a = args(raw)?;
        if a.positional.first().map(String::as_str) != Some("report") {
            return Ok(briefs_usage(io));
        }
        a.positional.remove(0);
        let (Some(briefs), Some(attempts)) = (a.opts.get("briefs"), a.opts.get("attempts")) else {
            return Ok(briefs_usage(io));
        };
        let library = dsl_sources(&a.positional)?;
        let data = Data::connect(data).map_err(|e| e.to_string())?;
        let kernels = kernels(kernel_conns)?;
        let r = crate::briefs::report(&data, &kernels, &library, Path::new(briefs), Path::new(attempts))?;
        put!(io, "{}", r.render());
        if let Some(out) = a.opts.get("out") {
            let json = serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?;
            std::fs::write(out, json).map_err(|e| format!("--out {}: {}", out, e))?;
            say!(io, "report written to {}", out);
        }
        Ok(0)
    };
    run().unwrap_or_else(|e| {
        complain!(io, "{}", e);
        1
    })
}
