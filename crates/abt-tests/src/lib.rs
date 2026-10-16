//! Shared fixtures of the integration tests (language-v2): the corpus-v2
//! catalog and standard library as the base workspace, strategies checked
//! into programs, a strategy skeleton the tests fill in, and crafted
//! markets with the facts a program's demand reads.
#![allow(dead_code)]
// RunError is the kernel's, deliberately rich (see abt-kernel's lib.rs).
#![allow(clippy::result_large_err)]

use std::fs;
use std::path::{Path, PathBuf};

use abt_kernel::check::{check_program, Code, Diagnostic, Program, Severity, Workspace};
use abt_kernel::data::business_days;
use abt_kernel::kernel::time::parse_timestamp;
use abt_kernel::kernel::{Dataset, Sym, Value};

/// The repository root: the corpus, the reference engine and the briefs
/// live there, not in this crate.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/abt-tests sits two levels below the root")
        .to_path_buf()
}

pub fn corpus_dir() -> PathBuf {
    repo_root().join("corpus-v2")
}

/// The `.dsl` files of a corpus subdirectory, sorted.
pub fn dsl_files(sub: &str) -> Vec<PathBuf> {
    let dir = corpus_dir().join(sub);
    let mut v: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {}", dir.display(), e))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|e| e == "dsl").unwrap_or(false))
        .collect();
    v.sort();
    v
}

/// A workspace holding the catalog classes and the standard library.
pub fn base_workspace() -> Workspace {
    let mut ws = Workspace::new();
    for f in dsl_files("catalog").into_iter().chain(dsl_files("lib")) {
        let src = fs::read_to_string(&f).unwrap();
        ws.add_source(&src).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
    }
    ws
}

/// The source of a corpus program: `corpus_source("strategies", "sma_crossover")`.
pub fn corpus_source(sub: &str, name: &str) -> String {
    let p = corpus_dir().join(sub).join(format!("{}.dsl", name));
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

/// Check a strategy source against the base workspace.
pub fn check(src: &str, name: &str) -> (Option<Program>, Vec<Diagnostic>) {
    let mut ws = base_workspace();
    ws.add_source(src).unwrap_or_else(|e| panic!("parse error at {}: {}", e.span, e.message));
    check_program(&ws, name)
}

/// The program of a strategy source that must check clean of errors.
pub fn program(src: &str, name: &str) -> Program {
    let (p, diags) = check(src, name);
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text(&diags)))
}

/// The program of a corpus strategy by name.
pub fn corpus_program(name: &str) -> Program {
    program(&corpus_source("strategies", name), name)
}

/// A corpus-v2 program file (under `strategies/` or `company/`) checked
/// with the catalog and the library, and the seeded synthetic market of its
/// class the golden runs read (`tests/tv_golden.rs`): five names over 400
/// days, or 30 days of minutes on a sub-daily base, the futures roots for a
/// future program. R8L's features need 250 sessions and the market holds
/// 30, so its windows are shortened, as `tests/corpus_v2.rs` does. Fails
/// with the checker's diagnostics.
pub fn golden_inputs(file: &Path) -> Result<(Program, Dataset), String> {
    let src = fs::read_to_string(file).map_err(|e| format!("{}: {}", file.display(), e))?;
    golden_inputs_of(&src, &file.display().to_string())
}

/// `golden_inputs` of a program's text (`origin` names it in a failure).
pub fn golden_inputs_of(src: &str, origin: &str) -> Result<(Program, Dataset), String> {
    let name = src
        .lines()
        .find_map(|l| l.trim().strip_prefix("strategy ").map(|r| r.split_whitespace().next().unwrap_or("").trim_end_matches('{').to_string()))
        .ok_or_else(|| format!("{}: no strategy", origin))?;
    let mut ws = base_workspace();
    ws.add_source(src).map_err(|e| format!("{}: {}", origin, e))?;
    let (prog, diags) = check_program(&ws, &name);
    let mut prog = prog.ok_or_else(|| format!("{}: does not check:\n{}", name, text(&diags)))?;
    let syms: Vec<&str> = if prog.class == "future" {
        vec![
            "ES", "NQ", "KC", "6J", "ZB", "ZF", "ZN", "RTY", "ZT", "CL", "GC", "HE", "HO", "LE", "NG", "PL", "RB", "SI", "ZC", "ZL", "ZM", "ZS", "ZW", "GF", "SB", "ETH",
        ]
    } else {
        vec!["AAA", "XYZ", "BBB", "SPY", "DDD"]
    };
    let days = if prog.base.is_sub_daily() { 30 } else { 400 };
    let ds = abt_kernel::data::synthetic_for_program(&prog, &syms, (2022, 1, 3), days, 11);
    if name == "r8l" {
        for (p, v) in [("atr_rows", 3), ("med_rows", 6), ("med_min", 3)] {
            for params in prog.params.values_mut() {
                if let Some(x) = params.get_mut(p) {
                    x.value = abt_kernel::Lit::Int(v);
                }
            }
        }
    }
    Ok((prog, ds))
}

/// Every corpus-v2 strategy and both company books, in that order.
pub fn golden_programs() -> Vec<PathBuf> {
    dsl_files("strategies").into_iter().chain(dsl_files("company")).collect()
}

/// The errors of a diagnostic list as (code, text).
pub fn errors(diags: &[Diagnostic]) -> Vec<(Code, String)> {
    diags.iter().filter(|d| d.severity == Severity::Error).map(|d| (d.code, d.to_string())).collect()
}

pub fn text(diags: &[Diagnostic]) -> String {
    diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")
}

/// A timestamp from its text (`2024-01-08`, `2024-01-08T10:30:00`).
pub fn day(s: &str) -> i64 {
    parse_timestamp(s).unwrap_or_else(|| panic!("`{}` is not a timestamp", s))
}

/// The blocks of a daily equity strategy, each with a default a test
/// overrides as needed; `render` writes the program.
pub struct StrategyParts {
    pub name: String,
    pub class: String,
    pub needs: String,
    pub uses: String,
    pub calendars: Option<String>,
    pub clock: String,
    pub universe: String,
    pub params: String,
    pub rules: String,
    pub silence: String,
    pub orders: String,
    pub decides: String,
    pub execution: String,
    pub report: String,
}

impl Default for StrategyParts {
    fn default() -> StrategyParts {
        StrategyParts {
            name: "s".into(),
            class: "equity".into(),
            needs: "need bars @1d   need listing".into(),
            uses: "calendar, bars, book".into(),
            calendars: None,
            clock: "decide on base".into(),
            universe: "universe(A, T) :- listed(A, T), traded@base(A, T).".into(),
            params: String::new(),
            rules: String::new(),
            silence: "hold".into(),
            orders: "market".into(),
            decides: String::new(),
            execution: "equity_default".into(),
            report: "calendar base   periods 252 per year".into(),
        }
    }
}

impl StrategyParts {
    pub fn render(&self) -> String {
        let calendars = self.calendars.as_ref().map(|c| format!("  calendars {{ {} }}\n", c)).unwrap_or_default();
        // A preset name, a preset with overrides, or a block of fields.
        let execution = format!("  execution {}\n", self.execution);
        format!(
            "strategy {name} {{\n  data {{ class {class}   {needs}   require reviewed   prefer latest }}\n  uses {uses}\n{calendars}  clock {{ {clock} }}\n  universe {{ {universe} }}\n  params {{\n{params}\n  }}\n  rules {{\n{rules}\n  }}\n  decisions {{\n    silence {silence}\n    orders {orders}\n{decides}\n  }}\n{execution}  report {{ {report} }}\n}}\n",
            name = self.name,
            class = self.class,
            needs = self.needs,
            uses = self.uses,
            calendars = calendars,
            clock = self.clock,
            universe = self.universe,
            params = self.params,
            rules = self.rules,
            silence = self.silence,
            orders = self.orders,
            decides = self.decides,
            execution = execution,
            report = self.report,
        )
    }
}

/// A daily equity strategy named `name` with the given `params`, `rules`
/// and decide rules, around the standard blocks (`class equity`, daily bars
/// and the listing, `uses calendar, bars, book`, the universe of listed
/// names that traded, `silence hold`, `orders market`, `equity_default`
/// execution, a daily report).
pub fn strategy(name: &str, params: &str, rules: &str, decides: &str) -> String {
    StrategyParts {
        name: name.into(),
        params: params.into(),
        rules: rules.into(),
        decides: decides.into(),
        ..Default::default()
    }
    .render()
}

/// Add one as-traded daily bar (open, high, low, close, volume) and the
/// listing of `sym` at `t`.
pub fn add_bar(ds: &mut Dataset, sym: Sym, t: i64, close: f64, volume: f64) {
    ds.add("open", vec![Value::Equity(sym), Value::Time(t), Value::Num(close)]);
    ds.add("high", vec![Value::Equity(sym), Value::Time(t), Value::Num(close)]);
    ds.add("low", vec![Value::Equity(sym), Value::Time(t), Value::Num(close)]);
    ds.add("close", vec![Value::Equity(sym), Value::Time(t), Value::Num(close)]);
    ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(volume)]);
    ds.add("listed", vec![Value::Equity(sym), Value::Time(t)]);
}

/// One name over consecutive weekdays from `start` with the given closes
/// (open, high and low at the close, volume 1000), listed every day; the
/// name is a security carrying its own ticker, so a literal resolves to it.
pub fn market_from(symbol: &str, start: (i64, u32, u32), closes: &[f64]) -> Dataset {
    let mut ds = Dataset::new();
    let bars = business_days(start, closes.len());
    let sym = ds.add_security(symbol, symbol, bars[0], None);
    for (t, p) in bars.iter().zip(closes) {
        add_bar(&mut ds, sym, *t, *p, 1000.0);
    }
    ds.derive_tickers();
    ds
}

/// `market_from` over the week of 2024-01-08.
pub fn market(symbol: &str, closes: &[f64]) -> Dataset {
    market_from(symbol, (2024, 1, 8), closes)
}

/// Several names over consecutive weekdays from `start`, each with its own
/// closes (volume 1000), every name listed on every bar it has.
pub fn markets(start: (i64, u32, u32), names: &[(&str, &[f64])]) -> Dataset {
    let mut ds = Dataset::new();
    let n = names.iter().map(|(_, c)| c.len()).max().unwrap_or(0);
    let bars = business_days(start, n);
    for (symbol, closes) in names {
        let sym = ds.add_security(symbol, symbol, bars[0], None);
        for (t, p) in bars.iter().zip(closes.iter()) {
            add_bar(&mut ds, sym, *t, *p, 1000.0);
        }
    }
    ds.derive_tickers();
    ds
}

/// Registers the in-process data service the kernel reads a dataset
/// through (ERRATA E-36); every helper that runs a program calls it.
pub fn init() {
    abt::install();
}

/// A client of a data service holding `ds`.
pub fn client(prog: &Program, ds: &Dataset) -> abt_kernel::kernel::DataClient {
    init();
    abt_kernel::kernel::connect_dataset(ds, prog).expect("an in-process data service")
}

/// A run over a dataset: the fold over the stream of a data service holding
/// it.
pub fn run(prog: &Program, ds: &Dataset, cfg: abt_kernel::kernel::ExecConfig) -> Result<abt_kernel::kernel::RunResult, abt_kernel::kernel::RunError> {
    init();
    abt_kernel::kernel::run(prog, ds, cfg)
}

/// `run` under a plan.
pub fn run_planned(prog: &Program, lowered: &abt_kernel::lower::Lowered, ds: &Dataset, cfg: abt_kernel::kernel::ExecConfig) -> Result<abt_kernel::kernel::RunResult, abt_kernel::kernel::RunError> {
    init();
    abt_kernel::kernel::run_planned(prog, lowered, ds, cfg)
}

/// A run that keeps the kernel at the end of the stream, on this thread.
pub fn run_kernel<'p>(prog: &'p Program, ds: &Dataset, cfg: abt_kernel::kernel::ExecConfig) -> Result<(abt_kernel::kernel::RunResult, abt_kernel::kernel::Kernel<'p>), abt_kernel::kernel::RunError> {
    let c = client(prog, ds);
    let mut exec = abt_kernel::kernel::SimExecutor::new(cfg.clone());
    abt_kernel::kernel::replay(prog, None, &c, cfg, &mut exec, &abt_kernel::kernel::Replay::default(), &mut |_| {})
}

/// A kernel holding every tuple of the stream and every bucket, nothing
/// decided.
pub fn loaded_kernel<'p>(prog: &'p Program, ds: &Dataset, cfg: abt_kernel::kernel::ExecConfig) -> abt_kernel::kernel::Kernel<'p> {
    abt_kernel::kernel::load(prog, None, &client(prog, ds), cfg).expect("the stream loads")
}

/// A Parquet directory the test wrote, loaded: the test vouches for what it
/// wrote, so the dataset is reviewed (a loose directory otherwise is not,
/// ERRATA E-39).
pub fn load_fixture(prog: &Program, dir: &std::path::Path) -> Result<(Dataset, Vec<String>), String> {
    let (mut ds, notes) = abt_kernel::data::load_parquet_dir(prog, dir)?;
    ds.unreviewed = false;
    Ok((ds, notes))
}

/// The causality check (G5) over a data service holding `ds`.
pub fn verify_causality(prog: &Program, ds: &Dataset, cfg: abt_kernel::kernel::ExecConfig, samples: &[i64]) -> Result<Vec<abt_kernel::kernel::CausalityMismatch>, abt_kernel::kernel::RunError> {
    init();
    abt_kernel::kernel::verify_causality(prog, ds, cfg, samples)
}

/// A run over a data service on a Unix socket holding `ds`: the same frames
/// as in process, so its result equals `run`'s (Proposition P-5.1, G3).
pub fn run_socket(prog: &Program, ds: &Dataset, cfg: abt_kernel::kernel::ExecConfig) -> Result<abt_kernel::kernel::RunResult, abt_kernel::kernel::RunError> {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("abt-run-{}-{}.sock", std::process::id(), n));
    let svc = abt::serve_dataset(ds, prog).map_err(abt_kernel::kernel::RunError::Config)?;
    let _server = abt_data::service::listen(&path, svc).map_err(|e| abt_kernel::kernel::RunError::Config(e.to_string()))?;
    let conn = abt_wire::transport::Connection::connect(&path).map_err(|e| abt_kernel::kernel::RunError::Config(e.to_string()))?;
    let client = abt_kernel::kernel::DataClient::connect(conn)?;
    let out = abt_kernel::kernel::run_on(prog, None, &client, cfg, &abt_kernel::kernel::Replay::default());
    let _ = fs::remove_file(&path);
    out
}

/// A run that hands the kernel at the end of the stream to `f` (on the
/// kernel's thread), recording the windowed groups' summaries when asked.
pub fn run_keeping<T: Send>(
    prog: &Program,
    lowered: Option<&abt_kernel::lower::Lowered>,
    ds: &Dataset,
    cfg: abt_kernel::kernel::ExecConfig,
    opts: &abt_kernel::kernel::Replay,
    f: impl for<'k> FnOnce(&abt_kernel::kernel::RunResult, &mut abt_kernel::kernel::Kernel<'k>) -> Result<T, abt_kernel::kernel::RunError> + Send,
) -> Result<(abt_kernel::kernel::RunResult, T), abt_kernel::kernel::RunError> {
    let c = client(prog, ds);
    abt_kernel::kernel::run_keeping(prog, lowered, &c, cfg, opts, f)
}

/// A study's sample over a data service holding `ds` (what `run_study` and
/// `reveal` read their bars from and run through).
pub struct Sampled<'a> {
    ds: &'a Dataset,
    client: abt_kernel::kernel::DataClient,
}

impl<'a> Sampled<'a> {
    pub fn new(prog: &Program, ds: &'a Dataset) -> Sampled<'a> {
        Sampled { ds, client: client(prog, ds) }
    }

    pub fn sample(&self) -> abt_kernel::study::Sample<'_> {
        abt_kernel::study::Sample { ds: self.ds, client: &self.client }
    }
}

/// The study runner: the fold over the service, truncated at the horizon.
pub fn study_run(
    prog: &Program,
    client: &abt_kernel::kernel::DataClient,
    cfg: abt_kernel::kernel::ExecConfig,
    horizon: Option<i64>,
) -> Result<abt_kernel::kernel::RunResult, abt_kernel::kernel::RunError> {
    abt_kernel::kernel::run_on(prog, None, client, cfg, &abt_kernel::kernel::Replay { horizon, ..Default::default() })
}
