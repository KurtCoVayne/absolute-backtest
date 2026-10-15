//! Shared fixtures of the integration tests (language-v2): the corpus-v2
//! catalog and standard library as the base workspace, strategies checked
//! into programs, a strategy skeleton the tests fill in, and crafted
//! markets with the facts a program's demand reads.
#![allow(dead_code)]

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
            "strategy {name} {{\n  data {{ class {class}   {needs}   require tested   prefer latest }}\n  uses {uses}\n{calendars}  clock {{ {clock} }}\n  universe {{ {universe} }}\n  params {{\n{params}\n  }}\n  rules {{\n{rules}\n  }}\n  decisions {{\n    silence {silence}\n    orders {orders}\n{decides}\n  }}\n{execution}  report {{ {report} }}\n}}\n",
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
