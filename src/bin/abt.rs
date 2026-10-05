//! `abt`: check strategies, run backtests, explain rules.
//!
//!   abt check <files...>
//!   abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])
//!           [--cash X] [--slippage-bps X] [--commission X] [--price-relation REL] [--param NAME=VALUE]...
//!           [--verify-causality] [--all] [--fills] [--quiet] <files...>
//!   abt explain --strategy NAME --rule LABEL --at TIMESTAMP [--inputs V1,V2,...] [--bind VAR=VALUE]... [--param NAME=VALUE]...
//!           (--data DIR | --synthetic ...) [--price-relation REL] <files...>
//!   abt synth --env equities_1d|equities_1m --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>
//!
//! Exit codes: 0 success; 1 the strategy does not check, the run halted or
//! the usage is wrong; 2 an input could not be read or parsed (a source
//! file, a data directory, an option value).

#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::str::FromStr;

use absolute_backtest::check::{check_program, check_workspace, Severity, Workspace};
use absolute_backtest::data;
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{ExecConfig, Kernel, RunError, Value};

struct Args {
    cmd: String,
    files: Vec<PathBuf>,
    opts: HashMap<String, String>,
    /// Repeatable options (`--bind`, `--param`), in the order given.
    multi: HashMap<String, Vec<String>>,
    flags: HashSet<String>,
}

const FLAGS: [&str; 5] = ["synthetic", "verify-causality", "quiet", "all", "fills"];
/// Options that may repeat.
const MULTI: [&str; 2] = ["bind", "param"];
const OPTIONS: [&str; 16] = [
    "strategy",
    "inputs",
    "bind",
    "param",
    "data",
    "rule",
    "at",
    "env",
    "out",
    "days",
    "symbols",
    "seed",
    "cash",
    "slippage-bps",
    "commission",
    "price-relation",
];

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
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
                eprintln!("unknown option --{}", name);
                usage(1)
            }
        } else {
            files.push(PathBuf::from(a));
        }
        i += 1;
    }
    Args { cmd, files, opts, multi, flags }
}

fn usage(code: i32) -> ! {
    eprintln!(
        "usage:\n  abt check <files...>\n  abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])\n          [--cash X] [--slippage-bps X] [--commission X] [--price-relation REL] [--param NAME=VALUE]... [--verify-causality] [--all] [--fills] [--quiet] <files...>\n  abt explain --strategy NAME --rule LABEL --at TIMESTAMP [--inputs V1,V2,...] [--bind VAR=VALUE]... [--param NAME=VALUE]... (--data DIR | --synthetic ...) [--price-relation REL] <files...>\n  abt synth --env NAME --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>\n\n  --price-relation REL  the primitive the executor fills at (default: the `close`-like relation at the decision resolution)\n  --param NAME=VALUE    override a parameter's default (repeatable; a library's as unit::name)\n  --inputs V1,V2,...    the rule's `+` arguments for explain, in signature order\n  --bind VAR=VALUE      pre-bind a body variable for explain (repeatable)"
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

fn synthetic_for(prog: &absolute_backtest::check::Program, opts: &HashMap<String, String>) -> absolute_backtest::kernel::Dataset {
    let symbols: Vec<String> = match opts.get("symbols") {
        None => vec!["AAA".into(), "BBB".into(), "CCC".into(), "DDD".into(), "SPY".into()],
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
    let minute = prog
        .relations
        .values()
        .any(|s| matches!(s.kind, absolute_backtest::Kind::Primitive { .. }) && s.res == Some(absolute_backtest::Resolution::M1));
    if minute {
        let days: usize = option(opts, "days", "a number of days", 40);
        data::synthetic_minute(&syms, (2024, 1, 2), days, 390, seed)
    } else {
        let days: usize = option(opts, "days", "a number of days", 500);
        data::synthetic_daily(&syms, (2022, 1, 3), days, seed)
    }
}

fn main() {
    let args = parse_args();
    match args.cmd.as_str() {
        "check" => {
            let ws = workspace(&args.files);
            let diags = check_workspace(&ws);
            for d in &diags {
                println!("{}", d);
            }
            let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
            let warnings = diags.len() - errors;
            let strategies = ws.strategies().count();
            println!("{} strategies, {} libraries checked: {} error(s), {} warning(s)", strategies, ws.libraries().count(), errors, warnings);
            exit(if errors > 0 { 1 } else { 0 })
        }
        "run" | "explain" => {
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
            let dataset = if args.flags.contains("synthetic") {
                synthetic_for(&prog, &args.opts)
            } else if let Some(dir) = args.opts.get("data") {
                let (ds, notes) = data::load_csv_dir(&prog, Path::new(dir)).unwrap_or_else(|e| {
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
            let cfg = ExecConfig {
                initial_cash: option(&args.opts, "cash", "an amount", 1_000_000.0),
                slippage_bps: option(&args.opts, "slippage-bps", "a number of basis points", 0.0),
                commission_per_share: option(&args.opts, "commission", "an amount per share", 0.0),
                price_relation: args.opts.get("price-relation").cloned(),
                param_overrides: args
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
                                // A literal in the DSL's grammar; a bare identifier is an equity.
                                let lit = absolute_backtest::parser::parse_lit(raw).unwrap_or_else(|e| {
                                    if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') && !raw.starts_with(|c: char| c.is_ascii_digit()) {
                                        absolute_backtest::Lit::Equity(raw.to_string())
                                    } else {
                                        eprintln!("--param {}: `{}` is not a literal (such as 20d, 100 shares, 0.02 or \"SPY\"): {}", name, raw, e.message);
                                        exit(1)
                                    }
                                });
                                (name.to_string(), lit)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            if args.cmd == "explain" {
                let label = args.opts.get("rule").cloned().unwrap_or_else(|| usage(1));
                let at_text = args.opts.get("at").cloned().unwrap_or_else(|| usage(1));
                let at = parse_timestamp(&at_text).unwrap_or_else(|| {
                    eprintln!("--at: `{}` is not a timestamp (YYYY-MM-DD, optionally THH:MM[:SS])", at_text);
                    exit(2)
                });
                let rule_idx = (0..prog.rules.len())
                    .find(|&i| prog.rule_label(i) == label || prog.rule_label(i).ends_with(&format!("::{}", label)))
                    .unwrap_or_else(|| {
                        eprintln!("no rule labelled `{}`; labels are unit::head#n, e.g. {}", label, prog.rule_label(0));
                        exit(1)
                    });
                let raw_inputs: Vec<String> = args
                    .opts
                    .get("inputs")
                    .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                    .unwrap_or_default();
                let raw_binds: Vec<(String, String)> = args
                    .multi
                    .get("bind")
                    .map(|bs| {
                        bs.iter()
                            .map(|b| {
                                let (var, val) = b.split_once('=').unwrap_or_else(|| {
                                    eprintln!("--bind takes VAR=VALUE, not `{}`", b);
                                    exit(1)
                                });
                                (var.trim().to_string(), val.trim().to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let out = std::thread::Builder::new()
                    .stack_size(512 << 20)
                    .spawn(move || {
                        let mut k = Kernel::new(&prog, &dataset, cfg)?;
                        // The rule's `+` arguments come from --inputs, parsed by the
                        // signature's types; body variables from --bind.
                        let need = k.rule_inputs(rule_idx);
                        if need.len() != raw_inputs.len() {
                            let shown: Vec<String> = need.iter().map(|(n, ty)| format!("{}: {}", n, ty)).collect();
                            return Err(RunError::Request(format!(
                                "rule {} takes {} inputs ({}), {} given; pass them in signature order with --inputs V1,V2,...",
                                prog.rule_label(rule_idx),
                                need.len(),
                                shown.join(", "),
                                raw_inputs.len()
                            )));
                        }
                        let inputs: Vec<Value> = need
                            .iter()
                            .zip(&raw_inputs)
                            .map(|((n, ty), raw)| k.parse_input(ty, raw).map_err(|m| RunError::Request(format!("input `{}`: {}", n, m))))
                            .collect::<Result<_, _>>()?;
                        let bindings: Vec<(String, Value)> = raw_binds
                            .iter()
                            .map(|(var, raw)| k.parse_binding(raw).map(|v| (var.clone(), v)).map_err(|m| RunError::Request(format!("--bind {}: {}", var, m))))
                            .collect::<Result<_, _>>()?;
                        // Run up to and including `at`, so executor state exists, then explain.
                        let _ = k.run()?;
                        k.explain_with(rule_idx, at, &inputs, &bindings)
                    })
                    .unwrap()
                    .join()
                    .unwrap();
                match out {
                    Ok(ex) => println!("{}", ex),
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(1)
                    }
                }
                return;
            }
            let verify = args.flags.contains("verify-causality");
            let result = absolute_backtest::kernel::run(&prog, &dataset, cfg.clone()).unwrap_or_else(|e| {
                match e {
                    RunError::Request(m) => eprintln!("{}", m),
                    e => eprintln!("run halted: {}", e),
                }
                exit(1)
            });
            println!(
                "strategy {} over {} bars ({} to {}), {} symbols",
                prog.strategy,
                result.bars.len(),
                format_timestamp(result.bars[0]),
                format_timestamp(*result.bars.last().unwrap()),
                result.symbols.len()
            );
            println!("decisions: {}   fills: {}   dropped: {}", result.decisions.len(), result.fills.len(), result.dropped.len());
            for (k, v) in data::summarize(&result.equity_curve) {
                println!("{:>14}: {:.4}", k, v);
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
                        println!("  fill {} {} {:+} @ {:.4}", format_timestamp(f.t), result.symbols[f.equity as usize], f.quantity, f.price);
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
            if verify {
                let n = result.bars.len();
                let samples: Vec<i64> = (1..=5).map(|i| result.bars[(n * i / 6).min(n - 1)]).collect();
                match absolute_backtest::kernel::verify_causality(&prog, &dataset, cfg, &samples) {
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
        "synth" => {
            let ws = workspace(&args.files);
            let env = args.opts.get("env").cloned().unwrap_or_else(|| usage(1));
            let out = args.opts.get("out").cloned().unwrap_or_else(|| usage(1));
            // The writer needs a checked program over the environment: use the
            // first strategy on it that checks clean.
            let prog = ws
                .strategies()
                .filter(|s| s.env.as_ref().map(|e| e.0 == env).unwrap_or(false))
                .find_map(|s| check_program(&ws, &s.name).0)
                .unwrap_or_else(|| {
                    eprintln!("synth needs a strategy in the workspace that uses environment `{}` and checks clean", env);
                    exit(1)
                });
            let ds = synthetic_for(&prog, &args.opts);
            data::write_csv_dir(&prog, &ds, Path::new(&out)).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            println!("wrote {} relations to {}", ds.facts.len(), out);
        }
        _ => usage(1),
    }
}
