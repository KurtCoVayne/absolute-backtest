//! `abt`: check strategies, run backtests, explain rules.
//!
//!   abt check <files...>
//!   abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])
//!           [--cash X] [--slippage-bps X] [--slippage-vol X] [--vol-window N] [--commission X] [--commission-min X] [--fee-bps X] [--frictionless]
//!           [--participation X] [--impact X] [--adv-window N] [--volume-relation REL]
//!           [--margin none|reg-t] [--max-gross X] [--maintenance X] [--on-margin-call halt|liquidate|allow]
//!           [--cash-rate X] [--margin-rate X] [--short-rebate X]
//!           [--price-relation REL] [--as-of DATE] [--haircut REASON=X]... [--param NAME=VALUE]...
//!           [--on-leverage halt|reject|allow] [--on-oversize halt|clamp|allow] [--on-ruin halt|continue] [--lot whole|fractional]
//!           [--verify-causality] [--all] [--fills] [--nav] [--quiet] <files...>
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
use absolute_backtest::kernel::{ExecConfig, Kernel, Lot, OnOversize, OnRuin, RunError, Value};

struct Args {
    cmd: String,
    files: Vec<PathBuf>,
    opts: HashMap<String, String>,
    /// Repeatable options (`--bind`, `--param`), in the order given.
    multi: HashMap<String, Vec<String>>,
    flags: HashSet<String>,
}

const FLAGS: [&str; 7] = ["synthetic", "verify-causality", "quiet", "all", "fills", "frictionless", "nav"];
/// Options that may repeat.
const MULTI: [&str; 3] = ["bind", "param", "haircut"];
const OPTIONS: [&str; 37] = [
    "strategy",
    "haircut",
    "as-of",
    "margin",
    "max-gross",
    "maintenance",
    "on-margin-call",
    "cash-rate",
    "margin-rate",
    "short-rebate",
    "participation",
    "impact",
    "adv-window",
    "volume-relation",
    "commission-min",
    "fee-bps",
    "slippage-vol",
    "vol-window",
    "on-leverage",
    "on-oversize",
    "on-ruin",
    "lot",
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
        "usage:\n  abt check <files...>\n  abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])\n          [--cash X] [--slippage-bps X] [--slippage-vol X] [--vol-window N] [--commission X] [--commission-min X] [--fee-bps X] [--frictionless]\n          [--participation X] [--impact X] [--adv-window N] [--volume-relation REL]\n          [--margin none|reg-t] [--max-gross X] [--maintenance X] [--on-margin-call halt|liquidate|allow] [--cash-rate X] [--margin-rate X] [--short-rebate X]\n          [--price-relation REL] [--as-of DATE] [--haircut REASON=X]... [--param NAME=VALUE]...\n          [--on-leverage halt|reject|allow] [--on-oversize halt|clamp|allow] [--on-ruin halt|continue] [--lot whole|fractional]\n          [--verify-causality] [--all] [--fills] [--nav] [--quiet] <files...>\n  abt explain --strategy NAME --rule LABEL --at TIMESTAMP [--inputs V1,V2,...] [--bind VAR=VALUE]... [--param NAME=VALUE]... (--data DIR | --synthetic ...) [--price-relation REL] <files...>\n  abt synth --env NAME --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>\n\n  --price-relation REL  the primitive the executor fills at (default: the `close`-like relation at the decision resolution)\n  --param NAME=VALUE    override a parameter's default (repeatable; a library's as unit::name)\n  --inputs V1,V2,...    the rule's `+` arguments for explain, in signature order\n  --bind VAR=VALUE      pre-bind a body variable for explain (repeatable)\n  --on-leverage POLICY  when a fill would borrow or put gross exposure above equity: halt (default), reject, allow\n  --on-oversize POLICY  when a sell or cover would cross zero: halt (default), clamp, allow\n  --on-ruin POLICY      when equity is not positive with orders pending: halt (default), continue\n  --lot ROUNDING        order quantities: whole shares (default) or fractional\n  --commission X        commission per share (default 0.005), --commission-min X per-order minimum (default 1.00)\n  --fee-bps X           regulatory fee on sells, in basis points of notional (default 0.278)\n  --slippage-bps X      fixed slippage against the order (default 0); --slippage-vol X adds X times the fill bar's realized volatility (default 0.1)\n  --vol-window N        bars of log returns behind the realized volatility (default 20; below 10 returns only the fixed part applies)\n  --participation X     a fill is at most X of the bar's volume (default 0.1; 0 is no cap); a delta remainder expires, a target re-issues itself\n  --impact X            fill price moves against the order by X * sqrt(filled / ADV) (default 0.1; 0 is none); --adv-window N bars behind ADV (default 20)\n  --volume-relation REL the primitive that supplies bar volumes (default: the `volume`-like relation at the decision resolution)\n  --margin PRESET       none (default: no borrowing, 1x gross, halt) or reg-t (2x gross, 25% maintenance, reject beyond)\n  --max-gross X         gross exposure may reach X times equity (default 1); --maintenance X margin call below X of gross (default 0.25)\n  --on-margin-call P    at a margin call: halt (default), liquidate pro rata, allow\n  --cash-rate X         annual rate on positive cash (default 0, warned); --margin-rate X on a debit (default 0.05); --short-rebate X on short notional (default 0)\n  --as-of DATE          the bundle date a ticker literal or a command-line name resolves at (default: the data's last bar)\n  --haircut REASON=X    the haircut on the last trade of a name delisted for REASON (repeatable; defaults: bankruptcy 1, regulatory 1, acquisition 0, voluntary 0, other 1)\n  --nav                 print the book at every bar as CSV: t,equity,cash,gross,net,leverage\n  --frictionless        every cost and liquidity model off (the run is warned)"
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
    } else if prog.relations.contains_key("split") {
        let days: usize = option(opts, "days", "a number of days", 500);
        data::synthetic_daily_v2(&syms, (2022, 1, 3), days, seed)
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
            let preset = args.opts.get("margin").map(|s| s.as_str()).unwrap_or("none");
            let base = match preset {
                "none" => ExecConfig::default(),
                "reg-t" => ExecConfig::reg_t(),
                other => {
                    eprintln!("--margin: `{}` is not one of none, reg-t", other);
                    exit(2)
                }
            };
            let base = if args.flags.contains("frictionless") {
                ExecConfig { ..ExecConfig::frictionless() }.with_margin_of(&base)
            } else {
                base
            };
            let cfg = ExecConfig {
                initial_cash: option(&args.opts, "cash", "an amount", base.initial_cash),
                slippage_bps: option(&args.opts, "slippage-bps", "a number of basis points", base.slippage_bps),
                slippage_vol_mult: option(&args.opts, "slippage-vol", "a multiple of realized volatility", base.slippage_vol_mult),
                vol_window: option(&args.opts, "vol-window", "a number of bars", base.vol_window),
                vol_min_obs: base.vol_min_obs,
                commission_per_share: option(&args.opts, "commission", "an amount per share", base.commission_per_share),
                commission_min_per_order: option(&args.opts, "commission-min", "an amount per order", base.commission_min_per_order),
                fee_bps_on_sells: option(&args.opts, "fee-bps", "a number of basis points", base.fee_bps_on_sells),
                participation_cap: option(&args.opts, "participation", "a fraction of bar volume", base.participation_cap),
                impact_coef: option(&args.opts, "impact", "an impact coefficient", base.impact_coef),
                adv_window: option(&args.opts, "adv-window", "a number of bars", base.adv_window),
                participation_warn: base.participation_warn,
                volume_relation: args.opts.get("volume-relation").cloned(),
                max_gross: option(&args.opts, "max-gross", "a multiple of equity", base.max_gross),
                maintenance_margin: option(&args.opts, "maintenance", "a fraction of gross exposure", base.maintenance_margin),
                on_margin_call: option(&args.opts, "on-margin-call", "one of halt, liquidate, allow", base.on_margin_call),
                cash_rate: option(&args.opts, "cash-rate", "an annual rate", base.cash_rate),
                margin_rate: option(&args.opts, "margin-rate", "an annual rate", base.margin_rate),
                short_rebate: option(&args.opts, "short-rebate", "an annual rate", base.short_rebate),
                borrow: base.borrow.clone(),
                delisting_haircut_default: base.delisting_haircut_default,
                delisting_haircuts: {
                    let mut hs = base.delisting_haircuts.clone();
                    for h in args.multi.get("haircut").cloned().unwrap_or_default() {
                        let (reason, x) = h.split_once('=').unwrap_or_else(|| {
                            eprintln!("--haircut takes REASON=FRACTION, not `{}`", h);
                            exit(2)
                        });
                        let x: f64 = x.trim().parse().unwrap_or_else(|_| {
                            eprintln!("--haircut {}: `{}` is not a fraction", reason, x);
                            exit(2)
                        });
                        hs.retain(|(r, _)| r != reason.trim());
                        hs.push((reason.trim().to_string(), x));
                    }
                    hs
                },
                as_of: args.opts.get("as-of").map(|s| {
                    parse_timestamp(s).unwrap_or_else(|| {
                        eprintln!("--as-of: `{}` is not a timestamp (YYYY-MM-DD)", s);
                        exit(2)
                    })
                }),
                price_relation: args.opts.get("price-relation").cloned(),
                on_leverage: option(&args.opts, "on-leverage", "one of halt, reject, allow", base.on_leverage),
                on_oversize: option(&args.opts, "on-oversize", "one of halt, clamp, allow", OnOversize::Halt),
                on_ruin: option(&args.opts, "on-ruin", "one of halt, continue", OnRuin::Halt),
                lot: option(&args.opts, "lot", "one of whole, fractional", Lot::Whole),
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
                                        absolute_backtest::Lit::Str(raw.to_string())
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
            let count = |f: &dyn Fn(&absolute_backtest::kernel::Action) -> bool| result.actions.iter().filter(|a| f(&a.action)).count();
            println!(
                "actions: {} splits   {} dividends   {} delistings",
                count(&|a| matches!(a, absolute_backtest::kernel::Action::Split { .. })),
                count(&|a| matches!(a, absolute_backtest::kernel::Action::Dividend { .. })),
                count(&|a| matches!(a, absolute_backtest::kernel::Action::Delisting { .. }))
            );
            let max_lev = result.exposure.iter().map(|e| e.leverage).fold(0.0, f64::max);
            let max_gross = result.exposure.iter().map(|e| e.gross).fold(0.0, f64::max);
            println!("exposure: max gross {:.2}   max leverage {:.3}", max_gross, max_lev);
            if args.flags.contains("nav") {
                println!("t,equity,cash,gross,net,leverage");
                for e in &result.exposure {
                    println!("{},{:.4},{:.4},{:.4},{:.4},{:.6}", format_timestamp(e.t), e.equity, e.cash, e.gross, e.net, e.leverage);
                }
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
