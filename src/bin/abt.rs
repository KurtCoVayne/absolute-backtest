//! `abt`: check strategies, run backtests, explain rules.
//!
//!   abt check <files...>
//!   abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])
//!           [--cash X | --capital X] [--compounding on|off] [--slippage-bps X] [--slippage-vol X] [--vol-window N] [--commission X] [--commission-min X] [--commission-bps X] [--fee-bps X] [--frictionless]
//!           [--participation X] [--impact X] [--adv-window N] [--volume-relation REL]
//!           [--margin none|reg-t] [--max-gross X] [--maintenance X] [--on-margin-call halt|liquidate|allow]
//!           [--cash-rate X] [--margin-rate X] [--short-rebate X]
//!           [--price-relation REL] [--as-of DATE] [--start DATE] [--end DATE] [--haircut REASON=X]... [--delist-proceeds by-reason|last-price] [--dividends cash|reinvest] [--actions apply|in-prices] [--param NAME=VALUE]...
//!           [--on-leverage halt|reject|allow] [--on-oversize halt|clamp|allow] [--on-ruin halt|continue] [--lot whole|fractional]
//!           [--verify-causality] [--all] [--fills] [--nav FILE] [--quiet] <files...>
//!   abt explain --strategy NAME --rule LABEL --at TIMESTAMP [--inputs V1,V2,...] [--bind VAR=VALUE]... [--param NAME=VALUE]...
//!           (--data DIR | --synthetic ...) [--price-relation REL] <files...>
//!   abt synth --env equities_1d|equities_1m --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>
//!   abt bundle build (--from DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N]) --env NAME --version V --out DIR <files...>
//!   abt bundle test DIR <files...>
//!   abt run ... --bundle DIR [--untested]
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

const FLAGS: [&str; 8] = ["synthetic", "verify-causality", "quiet", "all", "fills", "frictionless", "untested", "timing"];
/// Options that may repeat.
const MULTI: [&str; 5] = ["bind", "param", "haircut", "grid", "require"];
const OPTIONS: [&str; 70] = [
    "strategy",
    "report-calendar",
    "actions",
    "start",
    "end",
    "periods-per-year",
    "report-by",
    "returns",
    "dump",
    "nav",
    "compounding",
    "capital",
    "commission-bps",
    "delist-proceeds",
    "dividends",
    "briefs",
    "from-norgate",
    "from-databento",
    "processing-delay",
    "attempts",
    "study",
    "walk-forward",
    "holdout",
    "objective",
    "reason",
    "grid",
    "require",
    "checkpoint-every",
    "checkpoint-dir",
    "resume",
    "kernel",
    "bundle",
    "from",
    "version",
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

/// What runs a checked program on a dataset: the batch kernel, the fold, or the fold with checkpoints.
type Runner = dyn Fn(&absolute_backtest::check::Program, &absolute_backtest::kernel::Dataset, ExecConfig) -> Result<absolute_backtest::kernel::RunResult, RunError>;

fn usage(code: i32) -> ! {
    eprintln!(
        "usage:\n  abt check <files...>\n  abt run --strategy NAME (--data DIR | --synthetic [--days N] [--symbols A,B,C] [--seed N])\n          [--cash X | --capital X] [--compounding on|off] [--slippage-bps X] [--slippage-vol X] [--vol-window N] [--commission X] [--commission-min X] [--fee-bps X] [--frictionless]\n          [--participation X] [--impact X] [--adv-window N] [--volume-relation REL]\n          [--margin none|reg-t] [--max-gross X] [--maintenance X] [--on-margin-call halt|liquidate|allow] [--cash-rate X] [--margin-rate X] [--short-rebate X]\n          [--price-relation REL] [--as-of DATE] [--haircut REASON=X]... [--param NAME=VALUE]...\n          [--on-leverage halt|reject|allow] [--on-oversize halt|clamp|allow] [--on-ruin halt|continue] [--lot whole|fractional]\n          [--verify-causality] [--all] [--fills] [--nav FILE] [--returns FILE] [--report-by bar|day] [--periods-per-year X] [--dump DIR] [--quiet] [--timing] <files...>\n  abt explain --strategy NAME --rule LABEL --at TIMESTAMP [--inputs V1,V2,...] [--bind VAR=VALUE]... [--param NAME=VALUE]... (--data DIR | --synthetic ...) [--price-relation REL] <files...>\n  abt synth --env NAME --out DIR [--days N] [--symbols A,B,C] [--seed N] <files...>\n  abt bundle build (--from DIR | --from-norgate DIR | --from-databento DIR [--processing-delay S] | --synthetic ...) --env NAME --version V --out DIR <files...>\n  abt bundle test DIR <files...>\n  abt run --strategy NAME --bundle DIR [--untested] <files...>   (a bundle in place of --data or --synthetic)\n  abt study declare --study DIR --strategy NAME [--holdout none|trailing:Ny] [--objective METRIC] [--require METRIC>=X]... [executor options] <files...>\n  abt study run --study DIR --strategy NAME (--data DIR | --synthetic ... | --bundle DIR) [--grid NAME=V1,V2,...]... [--walk-forward SCHEME] [--param NAME=VALUE]... [--kernel KIND] <files...>\n  abt study reveal --study DIR --strategy NAME (--data DIR | --synthetic ... | --bundle DIR) <files...>\n  abt study metrics --study DIR [--strategy NAME] <files...>\n  abt study report --study DIR --strategy NAME <files...>\n  abt study dispute --study DIR --strategy NAME --reason TEXT <files...>\n  abt briefs report --briefs DIR --attempts DIR [--out FILE] <files...>\n\n  --study DIR           the study directory (lineages.json, studies/, trials.jsonl); with `abt run`, logs the run as an untracked trial\n  --holdout POLICY      none (warned), trailing:Ny (the last N years are truncated away until revealed) or blocks:K:Nmo[:SEED] (K random blocks of N months whose metrics are withheld until revealed)\n  --walk-forward SCHEME anchored:TRAIN:TEST or rolling:TRAIN:TEST (2y, 6mo): each fold picks the grid's best point on the train window and judges it on the test window\n  --objective METRIC    what a grid optimises (default sharpe; one of the report\'s metrics)\n  --require METRIC>=X   a threshold the report checks (repeatable; also METRIC<=X)\n  --grid NAME=V1,V2     a parameter axis of the grid (repeatable; the points are the cartesian product)\n  --reason TEXT         why a lineage attachment is disputed\n  --briefs DIR          the plain-language briefs (*.md); --attempts DIR holds attempts/<brief>/<n>.dsl, the model's successive attempts; --out FILE writes the report as JSON\n  --price-relation REL  the primitive the executor fills at (default: the `close`-like relation at the decision resolution)\n  --param NAME=VALUE    override a parameter's default (repeatable; a library's as unit::name)\n  --inputs V1,V2,...    the rule's `+` arguments for explain, in signature order\n  --bind VAR=VALUE      pre-bind a body variable for explain (repeatable)\n  --on-leverage POLICY  when a fill would borrow or put gross exposure above equity: halt (default), reject, allow\n  --on-oversize POLICY  when a sell or cover would cross zero: halt (default), clamp, allow\n  --on-ruin POLICY      when equity is not positive with orders pending: halt (default), continue\n  --lot ROUNDING        order quantities: whole shares (default) or fractional\n  --commission X        commission per share (default 0.005), --commission-min X per-order minimum (default 1.00)\n  --commission-bps X    commission in basis points of traded notional on both sides (default 0), added to the per-share schedule
  --fee-bps X           regulatory fee on sells, in basis points of notional (default 0.278)\n  --slippage-bps X      fixed slippage against the order (default 0); --slippage-vol X adds X times the fill bar's realized volatility (default 0.1)\n  --vol-window N        bars of log returns behind the realized volatility (default 20; below 10 returns only the fixed part applies)\n  --participation X     a fill is at most X of the bar's volume (default 0.1; 0 is no cap); a delta remainder expires, a target re-issues itself\n  --impact X            fill price moves against the order by X * sqrt(filled / ADV) (default 0.1; 0 is none); --adv-window N bars behind ADV (default 20)\n  --volume-relation REL the primitive that supplies bar volumes (default: the `volume`-like relation at the decision resolution)\n  --margin PRESET       none (default: no borrowing, 1x gross, halt) or reg-t (2x gross, 25% maintenance, reject beyond)\n  --max-gross X         gross exposure may reach X times equity (default 1); --maintenance X margin call below X of gross (default 0.25)\n  --on-margin-call P    at a margin call: halt (default), liquidate pro rata, allow\n  --cash-rate X         annual rate on positive cash (default 0, warned); --margin-rate X on a debit (default 0.05); --short-rebate X on short notional (default 0)\n  --start DATE, --end DATE  the run's window: the executor runs and the strategy decides only at bars within it; earlier bars stay data (warm-up)
  --as-of DATE          the bundle date a ticker literal or a command-line name resolves at (default: the data's last bar)\n  --haircut REASON=X    the haircut on the last trade of a name delisted for REASON (repeatable; defaults: bankruptcy 1, regulatory 1, acquisition 0, voluntary 0, other 1)
  --delist-proceeds P   by-reason (default: last trade less the reason's haircut, with commission) or last-price (last trade, no haircut, no cost)
  --dividends D         cash (default: credited at the pay date) or reinvest (fractional shares at the ex-date close, no cost)
  --actions A           apply (default: splits and dividends adjust the book) or in-prices (the price relation is a total-return series that already carries them; delistings still apply)\n  --bundle DIR          run on a bundle (manifest.json, securities.parquet, log/<relation>/<YYYY-MM>.parquet); --untested admits one whose tests have not passed\n  --kernel KIND         batch (default: the memoised evaluator bar by bar) or fold (the same evaluation driven by an availability-ordered event stream)\n  --checkpoint-every P  with --kernel fold: write the fold's state at the end of every month (`month`) or every N bars into --checkpoint-dir DIR as <bar>.json\n  --resume FILE         with --kernel fold: continue from a checkpoint file over the same data and configuration\n  --nav FILE            write the book at every bar to FILE as Parquet: t,equity,cash,gross,net,leverage
  --returns FILE        write the returns per reporting period to FILE as Parquet: t,ret,pnl
  --report-calendar REL with --report-by day, report on the days of relation REL (zero for a day without activity, days outside it left out)
  --report-by bar|day   the reporting period of the metrics and --returns: the decision bar (default) or the calendar day
  --periods-per-year X  periods a year for annualising (default: by the resolution per bar, 252 per day; 52 for a weekly book)\n  --dump DIR            write the data, configuration, decisions, fills, dropped decisions, actions, book and final state to DIR (what reference/ replays)\n  --from-norgate DIR    build from a Norgate-style daily export of Parquet files (prices, symbols, splits, dividends, delistings, membership, classification, exceptions: reviewed bundle-test exceptions)\n  --from-databento DIR  build from a Databento-style minute export of Parquet files (ohlcv-1m with optional ts_recv, symbology); --processing-delay S adds S seconds to every tuple's availability\n  --capital X           the fixed base under --compounding off, the starting cash otherwise (an alias of --cash; default 1000000)
  --compounding on|off  off (default): weights size against the fixed capital, leverage is judged against it and a bar's return is the NAV change over it; on: weights size against equity and returns compound
  --frictionless        every cost and liquidity model off (the run is warned)\n  --timing              print one stderr line: load_s (data), build_s (parse and check), run_s (kernel build and evaluation), total_s, bars, symbols, tuples, decisions, bars_per_s, peak_rss_mb and the kernel's statistics"
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

/// The dataset a run uses: synthetic, a bundle or a loose Parquet directory, as the options say.
fn load_dataset(args: &Args, prog: &absolute_backtest::check::Program) -> (absolute_backtest::kernel::Dataset, Option<String>) {
    let mut bundle_label: Option<String> = None;
    let dataset = if args.flags.contains("synthetic") {
        synthetic_for(prog, &args.opts)
    } else if let Some(dir) = args.opts.get("bundle") {
        let (ds, m) = absolute_backtest::bundle::load_bundle(prog, Path::new(dir), args.flags.contains("untested")).unwrap_or_else(|e| {
            eprintln!("{}", e);
            exit(2)
        });
        if m.tests.is_none() {
            eprintln!("note: bundle `{}@{}` has not passed its tests; its decisions are not causality-certified", m.name, m.version);
        }
        bundle_label = Some(format!("{}@{}", m.name, m.version));
        ds
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

/// The executor configuration the options describe (a margin preset, then every option over it).
fn exec_config(args: &Args) -> ExecConfig {
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
    let compounding = match args.opts.get("compounding").map(|s| s.as_str()) {
        None => base.compounding,
        Some("on") => true,
        Some("off") => false,
        Some(other) => {
            eprintln!("--compounding: `{}` is not one of on, off", other);
            exit(2)
        }
    };
    if args.opts.contains_key("cash") && args.opts.contains_key("capital") {
        eprintln!("--cash and --capital name the same amount; pass one");
        exit(2)
    }
    let cash_opt = if args.opts.contains_key("capital") { "capital" } else { "cash" };
    let cfg = ExecConfig {
        initial_cash: option(&args.opts, cash_opt, "an amount", base.initial_cash),
        compounding,
        slippage_bps: option(&args.opts, "slippage-bps", "a number of basis points", base.slippage_bps),
        slippage_vol_mult: option(&args.opts, "slippage-vol", "a multiple of realized volatility", base.slippage_vol_mult),
        vol_window: option(&args.opts, "vol-window", "a number of bars", base.vol_window),
        vol_min_obs: base.vol_min_obs,
        commission_per_share: option(&args.opts, "commission", "an amount per share", base.commission_per_share),
        commission_min_per_order: option(&args.opts, "commission-min", "an amount per order", base.commission_min_per_order),
        commission_bps: option(&args.opts, "commission-bps", "a number of basis points", base.commission_bps),
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
        delist_at_last_price: match args.opts.get("delist-proceeds").map(|s| s.as_str()) {
            None | Some("by-reason") => base.delist_at_last_price,
            Some("last-price") => true,
            Some(other) => {
                eprintln!("--delist-proceeds: `{}` is not one of by-reason, last-price", other);
                exit(2)
            }
        },
        actions_in_prices: match args.opts.get("actions").map(|s| s.as_str()) {
            None | Some("apply") => base.actions_in_prices,
            Some("in-prices") => true,
            Some(other) => {
                eprintln!("--actions: `{}` is not one of apply, in-prices", other);
                exit(2)
            }
        },
        reinvest_dividends: match args.opts.get("dividends").map(|s| s.as_str()) {
            None | Some("cash") => base.reinvest_dividends,
            Some("reinvest") => true,
            Some(other) => {
                eprintln!("--dividends: `{}` is not one of cash, reinvest", other);
                exit(2)
            }
        },
        window_cache: base.window_cache,
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
        start: args.opts.get("start").map(|s| {
            parse_timestamp(s).unwrap_or_else(|| {
                eprintln!("--start: `{}` is not a timestamp (YYYY-MM-DD)", s);
                exit(2)
            })
        }),
        end: args.opts.get("end").map(|s| {
            parse_timestamp(s).unwrap_or_else(|| {
                eprintln!("--end: `{}` is not a timestamp (YYYY-MM-DD)", s);
                exit(2)
            })
        }),
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
    cfg
}

/// What runs the program: the batch kernel, the fold, or the fold with checkpoints.
fn make_runner(args: &Args) -> Box<Runner> {
    let kernel_kind = args.opts.get("kernel").map(|s| s.as_str()).unwrap_or("batch");
    let checkpoint_every = args.opts.get("checkpoint-every").map(|s| match s.as_str() {
        "month" => absolute_backtest::kernel::CheckpointEvery::Month,
        n => absolute_backtest::kernel::CheckpointEvery::Bars(n.parse().unwrap_or_else(|_| {
            eprintln!("--checkpoint-every: `{}` is not `month` or a number of bars", n);
            exit(2)
        })),
    });
    let checkpoint_dir = args.opts.get("checkpoint-dir").map(PathBuf::from);
    let resume = args.opts.get("resume").map(PathBuf::from);
    if (checkpoint_every.is_some() || resume.is_some()) && kernel_kind != "fold" {
        eprintln!("--checkpoint-every and --resume need --kernel fold");
        exit(2)
    }
    let runner: Box<Runner> = match kernel_kind {
        "batch" => Box::new(absolute_backtest::kernel::run),
        "fold" if checkpoint_every.is_none() && resume.is_none() => Box::new(absolute_backtest::kernel::run_fold),
        "fold" => Box::new(move |prog, dataset, cfg| {
            use absolute_backtest::kernel::{Checkpoint, Event, EventLog, Fold, Kernel, SimExecutor};
            std::thread::scope(|s| {
                std::thread::Builder::new()
                    .stack_size(512 << 20)
                    .spawn_scoped(s, || {
                        let kernel = Kernel::new_streaming(prog, dataset, cfg.clone())?;
                        let log = EventLog::from_dataset(&kernel, dataset)?;
                        let mut exec = SimExecutor::new(cfg.clone());
                        let (mut fold, cursor) = match &resume {
                            Some(path) => {
                                let text = std::fs::read_to_string(path).map_err(|e| RunError::Request(format!("{}: {}", path.display(), e)))?;
                                let cp: Checkpoint = serde_json::from_str(&text).map_err(|e| RunError::Request(format!("{}: {}", path.display(), e)))?;
                                let cursor = cp.cursor;
                                eprintln!("resuming from {} (checkpoint of {})", path.display(), format_timestamp(cp.last_bar));
                                (Fold::restore(kernel, &mut exec, cp)?, cursor)
                            }
                            None => (Fold::new(kernel, &mut exec), i64::MIN),
                        };
                        if let Some(every) = checkpoint_every {
                            fold = fold.with_checkpoints(every);
                        }
                        for ev in log.events {
                            if let Event::Tuple { avail, .. } = &ev {
                                if *avail < cursor {
                                    continue;
                                }
                            }
                            fold.step(ev)?;
                            if let Some(cp) = fold.take_checkpoint() {
                                if let Some(dir) = &checkpoint_dir {
                                    std::fs::create_dir_all(dir).map_err(|e| RunError::Request(format!("{}: {}", dir.display(), e)))?;
                                    let path = dir.join(format!("{}.json", format_timestamp(cp.last_bar)));
                                    let text = serde_json::to_string(&cp).map_err(|e| RunError::Internal(e.to_string()))?;
                                    std::fs::write(&path, text).map_err(|e| RunError::Request(format!("{}: {}", path.display(), e)))?;
                                    eprintln!("checkpoint {} written", path.display());
                                }
                            }
                        }
                        fold.finish().map(|(r, _)| r)
                    })
                    .map_err(|e| RunError::Internal(format!("cannot spawn kernel thread: {}", e)))?
                    .join()
                    .map_err(|_| RunError::Internal("kernel thread panicked".into()))?
            })
        }),
        other => {
            eprintln!("--kernel: `{}` is not one of batch, fold", other);
            exit(2)
        }
    };
    runner
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
            let (mut dataset, bundle_label) = load_dataset(&args, &prog);
            let loaded_at = started.elapsed().as_secs_f64();
            let cfg = exec_config(&args);
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
            // What the report needs from the data, read before a run may take it over.
            let tuple_count: usize = dataset.facts.values().map(|tus| tus.len()).sum();
            let calendar: Option<std::collections::BTreeSet<i64>> = args.opts.get("report-calendar").map(|rel| {
                let Some(k) = prog.relations.get(rel).and_then(|s| s.key_pos()) else {
                    eprintln!("--report-calendar: `{}` is not a relation of the program", rel);
                    std::process::exit(2)
                };
                dataset
                    .facts
                    .get(rel)
                    .map(|tus| tus.iter().filter_map(|tu| tu[k].as_time()).map(time_day).collect())
                    .unwrap_or_default()
            });
            // The batch kernel takes the facts over when nothing reads the
            // dataset after the run (one copy of the data in memory).
            let runner = make_runner(&args);
            let owned = args.opts.get("kernel").map(|k| k == "batch").unwrap_or(true) && !verify && !args.opts.contains_key("dump") && !args.opts.contains_key("study");
            let outcome = if owned {
                absolute_backtest::kernel::run_owned(&prog, std::mem::take(&mut dataset), cfg.clone())
            } else {
                runner(&prog, &dataset, cfg.clone())
            };
            let result = outcome.unwrap_or_else(|e| {
                match e {
                    RunError::Request(m) => eprintln!("{}", m),
                    e => eprintln!("run halted: {}", e),
                }
                exit(1)
            });
            if args.flags.contains("timing") {
                let total = started.elapsed().as_secs_f64();
                let run_s = total - loaded_at;
                let s = &result.stats;
                eprintln!(
                    "timing load_s={:.3} build_s={:.3} run_s={:.3} total_s={:.3} bars={} symbols={} tuples={} decisions={} bars_per_s={:.1} peak_rss_mb={} window_calls={} window_bars_solved={} window_groups={} window_rows_cached={} late_tuples={}",
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
                    s.late_tuples
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
            for (k, v) in data::summarize(&result.metric_curve()) {
                println!("{:>14}: {:.4}", k, v);
            }
            let by_day = match args.opts.get("report-by").map(|s| s.as_str()) {
                None | Some("bar") => false,
                Some("day") => true,
                Some(other) => {
                    eprintln!("--report-by: `{}` is not one of bar, day", other);
                    std::process::exit(2)
                }
            };
            let default_ppy = if by_day { 252.0 } else { absolute_backtest::study::metrics::periods_per_year(prog.resolution) };
            let ppy: f64 = option(&args.opts, "periods-per-year", "a number of periods", default_ppy);
            let cm = absolute_backtest::study::metrics::convention_metrics_on(&result, ppy, by_day, calendar.as_ref());
            match result.base_capital {
                Some(b) => println!("accounting: fixed base {:.2}; a period's return is the NAV change over it (not reinvested)", b),
                None => println!("accounting: compounding; a period's return is the NAV change over the previous NAV"),
            }
            println!(
                "metrics per {} ({} a year, {} periods): cagr {:.6}   max_drawdown {:.6}   sharpe {:.4} (population {:.4})   volatility {:.4}",
                if by_day { "day" } else { "bar" },
                ppy,
                cm.periods,
                cm.cagr,
                cm.max_drawdown,
                cm.sharpe,
                cm.sharpe_population,
                cm.volatility_population
            );
            println!(
                "additive: annual_return {:.6}   max_drawdown {:.6}   total_pnl {:.2}   profit_factor {:.3} per period, {:.3} per trade   trades {}   win_rate {:.3}",
                cm.annual_return, cm.max_drawdown_additive, cm.total_pnl, cm.profit_factor, cm.profit_factor_trades, cm.trades, cm.win_rate
            );
            if let Some(path) = args.opts.get("returns") {
                let rows = absolute_backtest::study::metrics::period_returns_on(&result, by_day, calendar.as_ref());
                use absolute_backtest::table::{write_table, Col};
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
            if let Some(path) = args.opts.get("nav") {
                absolute_backtest::dump::write_nav(&result, Path::new(path)).unwrap_or_else(|e| {
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
                absolute_backtest::dump::write_dump(&prog, &dataset, &cfg, &result, Path::new(dir)).unwrap_or_else(|e| {
                    eprintln!("{}", e);
                    exit(2)
                });
                eprintln!("dump written to {}", dir);
            }
            // A run outside a study is an untracked trial (data-bundle doc,
            // section 7): logged when the study directory is named, warned
            // either way.
            match args.opts.get("study") {
                Some(dir) => {
                    let project = absolute_backtest::study::Project::open(Path::new(dir));
                    match absolute_backtest::study::log_untracked(&project, &prog, &result, &cfg, bundle_label.clone()) {
                        Ok(t) => println!(
                            "warning (data-snooping): logged as untracked trial #{} of lineage {} in {}; run it with `abt study run` to count it against a study",
                            t.seq, t.lineage, dir
                        ),
                        Err(e) => {
                            eprintln!("{}", e);
                            exit(2)
                        }
                    }
                }
                None => println!("warning (data-snooping): this run is an untracked trial that nothing counts; run it with `abt study run --study DIR`, or name --study DIR here to log it"),
            }
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
        "study" => {
            use absolute_backtest::study::{self, Holdout, Project, Threshold};
            let sub = args.files.first().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let rest: Vec<PathBuf> = args.files.iter().skip(1).cloned().collect();
            let dir = args.opts.get("study").cloned().unwrap_or_else(|| usage(1));
            let project = Project::open(Path::new(&dir));
            let fail = |e: String| -> ! {
                eprintln!("{}", e);
                exit(2)
            };
            let checked = |ws: &Workspace, name: &str| -> absolute_backtest::check::Program {
                let (prog, diags) = check_program(ws, name);
                for d in &diags {
                    eprintln!("{}", d);
                }
                prog.unwrap_or_else(|| {
                    eprintln!("strategy `{}` does not check; fix the errors above", name);
                    exit(1)
                })
            };
            match sub.as_str() {
                "declare" => {
                    let ws = workspace(&rest);
                    let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
                    let prog = checked(&ws, &name);
                    let holdout = Holdout::parse(args.opts.get("holdout").map(|s| s.as_str()).unwrap_or("none")).unwrap_or_else(|e| {
                        eprintln!("--holdout: {}", e);
                        exit(2)
                    });
                    let objective = args.opts.get("objective").cloned().unwrap_or_else(|| "sharpe".into());
                    let thresholds: Vec<Threshold> = args
                        .multi
                        .get("require")
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|r| {
                            Threshold::parse(r).unwrap_or_else(|e| {
                                eprintln!("--require: {}", e);
                                exit(2)
                            })
                        })
                        .collect();
                    let exec = exec_config(&args);
                    let (spec, opened) = project.declare(&prog, holdout, &objective, thresholds, exec).unwrap_or_else(|e| fail(e));
                    println!("study {} declared in {}", spec.id, dir);
                    println!(
                        "  strategy {} ({}) in lineage {}{}",
                        spec.strategy,
                        spec.hash,
                        spec.lineage,
                        match &opened.attachment {
                            study::Attachment::Root => String::new(),
                            study::Attachment::Revises(h) => format!(", revising {}", h),
                            study::Attachment::Similarity { to, score } => format!(", attached by similarity {:.2} with {}", score, to),
                        }
                    );
                    println!(
                        "  hold-out {}   objective {}   thresholds: {}",
                        spec.holdout.describe(),
                        spec.objective,
                        if spec.thresholds.is_empty() {
                            "none".to_string()
                        } else {
                            spec.thresholds.iter().map(|t| t.describe()).collect::<Vec<_>>().join(", ")
                        }
                    );
                    for w in &spec.warnings {
                        println!("warning ({}): {}", w.bias, w.message);
                    }
                }
                "run" => {
                    let ws = workspace(&rest);
                    let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
                    let prog = checked(&ws, &name);
                    let opened = project.open_lineage(&prog).unwrap_or_else(|e| fail(e));
                    let spec = project.study_for(&opened.lineage).unwrap_or_else(|e| fail(e)).unwrap_or_else(|| {
                        eprintln!("no study is declared for lineage {} in {}; run `abt study declare` first", opened.lineage, dir);
                        exit(2)
                    });
                    let (dataset, bundle_label) = load_dataset(&args, &prog);
                    let mut axes = Vec::new();
                    for g in args.multi.get("grid").cloned().unwrap_or_default() {
                        axes.push(study::parse_grid_axis(&g).unwrap_or_else(|e| fail(e)));
                    }
                    let grid = study::Grid::new(axes, &exec_config(&args).param_overrides);
                    let points = grid.points.clone();
                    let scheme = args.opts.get("walk-forward").map(|s| {
                        study::WalkForward::parse(s).unwrap_or_else(|e| {
                            eprintln!("--walk-forward: {}", e);
                            exit(2)
                        })
                    });
                    let runner = make_runner(&args);
                    let (outcomes, report) = study::run_study(&project, &spec, &prog, &dataset, &grid, &*runner, study::Provenance { bundle: bundle_label, scheme }).unwrap_or_else(|e| fail(e));
                    println!(
                        "study {} (lineage {}, objective {}, hold-out {}){}",
                        spec.id,
                        spec.lineage,
                        spec.objective,
                        spec.holdout.describe(),
                        if report.embargo.is_empty() {
                            String::new()
                        } else if report.embargo.truncates {
                            format!("; bars {} embargoed", report.embargo.describe())
                        } else {
                            format!("; metrics withhold the blocks {}", report.embargo.describe())
                        }
                    );
                    for (i, o) in outcomes.iter().enumerate() {
                        let shown: Vec<String> = points[i].iter().map(|(n, l)| format!("{}={}", n, l)).collect();
                        println!(
                            "point {}/{}{}: {} {:.3} (se {:.3})   cagr {:.4}   max drawdown {:.4}   turnover {:.2}   fills {}   trial #{}{}",
                            i + 1,
                            outcomes.len(),
                            if shown.is_empty() { String::new() } else { format!(" [{}]", shown.join(", ")) },
                            spec.objective,
                            study::objective_value(&spec.objective, &o.metrics, &o.trading).unwrap_or(f64::NAN),
                            o.metrics.sharpe_se,
                            o.metrics.cagr,
                            o.metrics.max_drawdown,
                            o.trading.turnover,
                            o.trading.fills,
                            o.trial.seq,
                            if i == report.best && outcomes.len() > 1 { " (best)" } else { "" }
                        );
                    }
                    println!(
                        "lineage {}: {} trials; deflated Sharpe ratio {:.3} for the best point (expected maximum per-period Sharpe {:.4})",
                        spec.lineage, report.trials, report.dsr.dsr, report.dsr.expected_max_sr
                    );
                    if let Some(p) = &report.pbo {
                        println!(
                            "probability of backtest overfitting {:.3} over the grid ({} combinations of {} partitions)",
                            p.pbo, p.combinations, p.partitions
                        );
                    }
                    if let Some(sf) = &report.surface {
                        println!(
                            "parameter surface: smoothness {:.3}; {:.0}% of the best point's {} neighbours within {:.3} of it",
                            sf.smoothness,
                            sf.stability * 100.0,
                            sf.neighbours,
                            sf.tolerance
                        );
                    }
                    if !report.subperiods.is_empty() {
                        println!("sharpe by year: {}", report.subperiods.iter().map(|(y, s)| format!("{} {:.2}", y, s)).collect::<Vec<_>>().join("   "));
                    }
                    if let Some(wf) = &report.walk_forward {
                        println!("walk-forward {}: {} folds", wf.scheme.describe(), wf.folds.len());
                        for (k, f) in wf.folds.iter().enumerate() {
                            let shown: Vec<String> = points[f.best].iter().map(|(n, l)| format!("{}={}", n, l)).collect();
                            println!(
                                "  fold {}: train {}..{} test {}..{}: point {} [{}] {} in {:.3} out {:.3}",
                                k + 1,
                                format_timestamp(f.train.0),
                                format_timestamp(f.train.1),
                                format_timestamp(f.test.0),
                                format_timestamp(f.test.1),
                                f.best + 1,
                                shown.join(", "),
                                spec.objective,
                                f.in_sample,
                                f.out_of_sample
                            );
                        }
                        println!(
                            "  efficiency {}; out-of-sample stitched: sharpe {:.3}   cagr {:.4}   max drawdown {:.4}",
                            wf.efficiency.map(|e| format!("{:.3}", e)).unwrap_or_else(|| "undefined".into()),
                            wf.out_of_sample.sharpe,
                            wf.out_of_sample.cagr,
                            wf.out_of_sample.max_drawdown
                        );
                    }
                    for w in &report.warnings {
                        println!("warning ({}): {}", w.bias, w.message);
                    }
                }
                "reveal" => {
                    let ws = workspace(&rest);
                    let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
                    let prog = checked(&ws, &name);
                    let hash = study::program_hash(&prog);
                    let ls = project.lineages().unwrap_or_else(|e| fail(e));
                    let lineage = ls.lineage_of(&hash).map(|l| l.id.clone()).unwrap_or_else(|| {
                        eprintln!(
                            "strategy `{}` ({}) is not a member of any lineage in {}: a reveal names a committed version, one the study has run",
                            name, hash, dir
                        );
                        exit(2)
                    });
                    let spec = project.study_for(&lineage).unwrap_or_else(|e| fail(e)).unwrap_or_else(|| {
                        eprintln!("no study is declared for lineage {} in {}", lineage, dir);
                        exit(2)
                    });
                    let (dataset, bundle_label) = load_dataset(&args, &prog);
                    let runner = make_runner(&args);
                    let (o, embargo) = study::reveal(&project, &spec, &prog, &dataset, &*runner, study::Provenance { bundle: bundle_label, scheme: None }).unwrap_or_else(|e| fail(e));
                    println!("reveal of {} ({}) over the hold-out {} (study {}): trial #{}", name, hash, embargo.describe(), spec.id, o.trial.seq);
                    println!(
                        "out of sample: {} bars; sharpe {:.3} (se {:.3})   cagr {:.4}   max drawdown {:.4}   cvar5 {:.4}   worst month {:.4}",
                        o.metrics.n + 1,
                        o.metrics.sharpe,
                        o.metrics.sharpe_se,
                        o.metrics.cagr,
                        o.metrics.max_drawdown,
                        o.metrics.cvar_5,
                        o.metrics.worst_month
                    );
                    for t in &spec.thresholds {
                        if let Some(ok) = t.holds(&o.metrics, &o.trading) {
                            println!("  {} {}", t.describe(), if ok { "holds" } else { "fails" });
                        }
                    }
                    let reveals = project
                        .log()
                        .for_lineage(&lineage)
                        .unwrap_or_else(|e| fail(e))
                        .iter()
                        .filter(|t| t.kind == study::TrialKind::Reveal)
                        .count();
                    println!("{} reveal(s) of this lineage so far; each is an out-of-sample trial", reveals);
                }
                "metrics" => {
                    let log = project.log();
                    let trials = match args.opts.get("strategy") {
                        Some(name) => {
                            let ws = workspace(&rest);
                            let prog = checked(&ws, name);
                            let ls = project.lineages().unwrap_or_else(|e| fail(e));
                            let hash = study::program_hash(&prog);
                            match ls.lineage_of(&hash) {
                                Some(l) => log.for_lineage(&l.id).unwrap_or_else(|e| fail(e)),
                                None => {
                                    println!("strategy {} ({}) has no lineage in {}", name, hash, dir);
                                    vec![]
                                }
                            }
                        }
                        None => log.read_all().unwrap_or_else(|e| fail(e)),
                    };
                    for t in &trials {
                        let params: Vec<String> = t.params.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
                        println!(
                            "#{} {:?} {} {} [{}] {}..{} bars {}: sharpe {:.3} (se {:.3}, lo {:.3})   cagr {:.4}   max drawdown {:.4}   cvar5 {:.4}   worst month {:.4}   turnover {:.2}",
                            t.seq,
                            t.kind,
                            t.strategy,
                            &t.hash[..8.min(t.hash.len())],
                            params.join(", "),
                            t.period.as_ref().map(|p| p.0.as_str()).unwrap_or("-"),
                            t.period.as_ref().map(|p| p.1.as_str()).unwrap_or("-"),
                            t.bars,
                            t.metrics.sharpe,
                            t.metrics.sharpe_se,
                            t.metrics.sharpe_lo,
                            t.metrics.cagr,
                            t.metrics.max_drawdown,
                            t.metrics.cvar_5,
                            t.metrics.worst_month,
                            t.trading.turnover
                        );
                    }
                    if let Some(last) = trials.last() {
                        let same: Vec<_> = trials.iter().filter(|t| t.lineage == last.lineage).cloned().collect();
                        let d = study::deflated_sharpe(
                            last.metrics.sharpe_period,
                            last.metrics.n,
                            last.metrics.skew,
                            last.metrics.kurtosis,
                            same.len(),
                            study::sharpe_variance(&same),
                        );
                        println!("lineage {}: {} trials; deflated Sharpe ratio of the latest {:.3}", last.lineage, same.len(), d.dsr);
                    }
                    println!("{} trials", trials.len());
                }
                "report" => {
                    let ws = workspace(&rest);
                    let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
                    let (prog, diags) = check_program(&ws, &name);
                    let Some(prog) = prog else {
                        for d in &diags {
                            eprintln!("{}", d);
                        }
                        eprintln!("strategy `{}` does not check; fix the errors above", name);
                        exit(1)
                    };
                    let report = study::build_report(&project, &prog, &diags).unwrap_or_else(|e| fail(e));
                    print!("{}", report.render());
                }
                "dispute" => {
                    let ws = workspace(&rest);
                    let name = args.opts.get("strategy").cloned().unwrap_or_else(|| usage(1));
                    let reason = args.opts.get("reason").cloned().unwrap_or_else(|| usage(1));
                    let prog = checked(&ws, &name);
                    let hash = study::program_hash(&prog);
                    let mut ls = project.lineages().unwrap_or_else(|e| fail(e));
                    ls.dispute(&hash, &reason, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
                        .unwrap_or_else(|e| fail(e));
                    project.save_lineages(&ls).unwrap_or_else(|e| fail(e));
                    println!("dispute of {}'s attachment logged; the attachment stands (section 6, lineage)", hash);
                }
                _ => usage(1),
            }
        }
        "briefs" => {
            let sub = args.files.first().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let rest: Vec<PathBuf> = args.files.iter().skip(1).cloned().collect();
            if sub != "report" {
                usage(1)
            }
            let briefs = args.opts.get("briefs").cloned().unwrap_or_else(|| usage(1));
            let attempts = args.opts.get("attempts").cloned().unwrap_or_else(|| usage(1));
            let ws = workspace(&rest);
            let report = absolute_backtest::study::briefs::report(&ws, Path::new(&briefs), Path::new(&attempts)).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            print!("{}", report.render());
            if let Some(out) = args.opts.get("out") {
                let text = serde_json::to_string_pretty(&report).unwrap_or_default();
                std::fs::write(out, text).unwrap_or_else(|e| {
                    eprintln!("{}: {}", out, e);
                    exit(2)
                });
                eprintln!("report written to {}", out);
            }
        }
        "bundle" => {
            // abt bundle build (--from DIR | --synthetic ...) --env NAME --version V --out DIR <files...>
            // abt bundle test DIR <files...>
            let sub = args.files.first().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let rest: Vec<PathBuf> = args.files.iter().skip(1).cloned().collect();
            match sub.as_str() {
                "build" => {
                    let ws = workspace(&rest);
                    let env = args.opts.get("env").cloned().unwrap_or_else(|| usage(1));
                    let version = args.opts.get("version").cloned().unwrap_or_else(|| usage(1));
                    let out = args.opts.get("out").cloned().unwrap_or_else(|| usage(1));
                    let prog = ws
                        .strategies()
                        .filter(|s| s.env.as_ref().map(|e| e.0 == env).unwrap_or(false))
                        .find_map(|s| check_program(&ws, &s.name).0)
                        .unwrap_or_else(|| {
                            eprintln!("bundle build needs a strategy in the workspace that uses environment `{}` and checks clean", env);
                            exit(1)
                        });
                    let mut source: Option<String> = None;
                    let mut exceptions = Vec::new();
                    let processing_delay: i64 = option(&args.opts, "processing-delay", "a number of seconds", 0);
                    let ds = if args.flags.contains("synthetic") {
                        synthetic_for(&prog, &args.opts)
                    } else if let Some(from) = args.opts.get("from-norgate") {
                        let ingested = absolute_backtest::ingest::norgate_daily(Path::new(from), &prog).unwrap_or_else(|e| {
                            eprintln!("{}", e);
                            exit(2)
                        });
                        for n in &ingested.notes {
                            eprintln!("note: {}", n);
                        }
                        source = Some(ingested.source);
                        exceptions = ingested.exceptions;
                        ingested.dataset
                    } else if let Some(from) = args.opts.get("from-databento") {
                        let ingested = absolute_backtest::ingest::databento_minute(Path::new(from), &prog, processing_delay).unwrap_or_else(|e| {
                            eprintln!("{}", e);
                            exit(2)
                        });
                        for n in &ingested.notes {
                            eprintln!("note: {}", n);
                        }
                        source = Some(ingested.source);
                        ingested.dataset
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
                    let m = absolute_backtest::bundle::write_bundle(&prog, &ds, Path::new(&out), &env, &version).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    });
                    absolute_backtest::bundle::write_exceptions(Path::new(&out), &exceptions).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    });
                    let m = if source.is_some() || processing_delay != 0 {
                        absolute_backtest::bundle::annotate_manifest(Path::new(&out), source, processing_delay).unwrap_or_else(|e| {
                            eprintln!("{}", e);
                            exit(2)
                        })
                    } else {
                        m
                    };
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
                    let m = absolute_backtest::bundle::read_manifest(&dir).unwrap_or_else(|e| {
                        eprintln!("{}", e);
                        exit(2)
                    });
                    let prog = ws
                        .strategies()
                        .filter(|s| s.env.as_ref().map(|e| e.0 == m.name).unwrap_or(false))
                        .find_map(|s| check_program(&ws, &s.name).0)
                        .unwrap_or_else(|| {
                            eprintln!("bundle test needs a strategy in the workspace that uses environment `{}` and checks clean", m.name);
                            exit(1)
                        });
                    let (m, results) = absolute_backtest::bundle::test_bundle(&prog, &dir).unwrap_or_else(|e| {
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
            data::write_parquet_dir(&prog, &ds, Path::new(&out)).unwrap_or_else(|e| {
                eprintln!("{}", e);
                exit(2)
            });
            println!("wrote {} relations to {}", ds.facts.len(), out);
        }
        _ => usage(1),
    }
}

/// The midnight of a timestamp's day.
fn time_day(t: i64) -> i64 {
    absolute_backtest::kernel::time::day_key(t) * absolute_backtest::kernel::time::DAY
}
