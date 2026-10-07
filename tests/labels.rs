//! The `Label` type (data-bundle doc, section 3: delisting reasons,
//! classification schemes and codes, index names are names from a closed
//! vocabulary the bundle defines, compared with `=` only). A string literal
//! is an `Equity` or a `Label` from context, the way a bare integer is a
//! Count or a Scalar; the checker resolves it, so the kernel never guesses.

mod corpus;

use std::fs;

use absolute_backtest::check::{check_program, Code, Diagnostic, Program, Severity, Workspace};
use absolute_backtest::data::{load_parquet_dir, write_parquet_dir};
use absolute_backtest::kernel::time::{format_timestamp, parse_timestamp};
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Value};
use absolute_backtest::table::to_text;
use absolute_backtest::{Lit, Ty};

const ENV: &str = r#"
environment labelled_1d {
  close(+A: Equity, @T: Timestamp, -P: Price<USD>) @1d
  volume(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1d
  universe(-A: Equity, @T: Timestamp) @1d complete
  delisted(-A: Equity, @T: Timestamp, -Reason: Label) @1d complete
  member(+A: Equity, @T: Timestamp, +Idx: Label) @1d complete
}
"#;

const EXIT_ON_BANKRUPTCY: &str = r#"
strategy exit_on_bankruptcy {
  env labelled_1d
  resolution @1d
  mode delta
  param why : Label = "bankruptcy"
  param qty : Quantity<Shares> = 10 shares
  rel gone(-A: Equity, @T: Timestamp)
  gone(A, T) :- delisted(A, T, R), R = why.
  rel held(-A: Equity, @T: Timestamp, -Q: Quantity<Shares>)
  held(A, T, Q) :- position(A, T, Q), Q > 0 shares.
  rel flat(+A: Equity, @T: Timestamp)
  flat(A, T) :- universe(A, T), not position(A, T, _).
  decide(T, buy(A, qty)) :- universe(A, T), member(A, T, "SPX"), flat(A, T), not delisted(A, T, _).
  decide(T, sell(A, Q)) :- held(A, T, Q), gone(A, T).
}
"#;

fn workspace() -> Workspace {
    let mut ws = corpus::base_workspace();
    ws.add_source(ENV).unwrap();
    ws
}

fn check(src: &str, name: &str) -> Vec<Diagnostic> {
    let mut ws = workspace();
    ws.add_source(src).unwrap();
    check_program(&ws, name).1
}

fn program(src: &str, name: &str) -> Program {
    let mut ws = workspace();
    ws.add_source(src).unwrap();
    let (p, diags) = check_program(&ws, name);
    p.unwrap_or_else(|| panic!("{} should check:\n{}", name, text(&diags)))
}

fn text(diags: &[Diagnostic]) -> String {
    diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")
}

fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.severity == Severity::Error).collect()
}

fn day(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

#[test]
fn label_is_a_type_and_a_string_literal_resolves_by_context() {
    assert_eq!(Ty::parse("Label", None), Some(Ty::Label));
    assert!(!Ty::Label.is_entity(), "a label never identifies a tuple");
    let diags = check(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    assert!(diags.is_empty(), "a label literal is not a ticker snapshot:\n{}", text(&diags));
    let p = program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    // The checked program carries resolved literals: no bare string is left.
    assert_eq!(p.param("exit_on_bankruptcy", "why").unwrap().value, Lit::Label("bankruptcy".into()));
    let mut seen = Vec::new();
    for r in &p.rules {
        for (l, _) in absolute_backtest::check::dof::literals_in_rule(r) {
            match &l {
                Lit::Str(s) => panic!("unresolved string literal `{}` in {}", s, r.head.name),
                Lit::Label(s) | Lit::Equity(s) => seen.push((r.head.name.clone(), l.ty(), s.clone())),
                _ => {}
            }
        }
    }
    assert_eq!(seen, vec![("decide".to_string(), Ty::Label, "SPX".to_string())]);
}

#[test]
fn a_ticker_literal_is_still_an_equity_and_warned() {
    let src = EXIT_ON_BANKRUPTCY.replace(
        "decide(T, sell(A, Q)) :- held(A, T, Q), gone(A, T).",
        "decide(T, sell(A, Q)) :- held(A, T, Q), gone(A, T), A = \"AAA\".",
    );
    let diags = check(&src, "exit_on_bankruptcy");
    assert!(errors(&diags).is_empty(), "{}", text(&diags));
    assert_eq!(diags.len(), 1, "{}", text(&diags));
    assert_eq!(diags[0].code, Code::W6);
    let p = program(&src, "exit_on_bankruptcy");
    let lits: Vec<Lit> = p.rules.iter().flat_map(absolute_backtest::check::dof::literals_in_rule).map(|(l, _)| l).collect();
    assert!(lits.contains(&Lit::Equity("AAA".into())) && lits.contains(&Lit::Label("SPX".into())), "{:?}", lits);
}

#[test]
fn a_string_literal_without_a_typed_context_is_a_type_error() {
    // Assigned to a fresh variable: nothing says Equity or Label.
    let src = EXIT_ON_BANKRUPTCY.replace("gone(A, T) :- delisted(A, T, R), R = why.", "gone(A, T) :- delisted(A, T, R), W = \"bankruptcy\", R = W.");
    let diags = check(&src, "exit_on_bankruptcy");
    let errs = errors(&diags);
    assert!(!errs.is_empty() && errs.iter().all(|d| d.code == Code::T), "{}", text(&diags));
    assert!(errs[0].message.contains("Equity") && errs[0].message.contains("Label"), "{}", errs[0].message);
    // Two literals compared: the same.
    let src = EXIT_ON_BANKRUPTCY.replace("R = why.", "R = why, \"a\" = \"b\".");
    let diags = check(&src, "exit_on_bankruptcy");
    assert!(errors(&diags).iter().all(|d| d.code == Code::T) && !errors(&diags).is_empty(), "{}", text(&diags));
    // A label where a price is expected, and a number where a label is.
    let src = EXIT_ON_BANKRUPTCY.replace("param qty : Quantity<Shares> = 10 shares", "param qty : Quantity<Shares> = \"ten\"");
    let diags = check(&src, "exit_on_bankruptcy");
    assert!(errors(&diags).iter().any(|d| d.code == Code::T && d.message.contains("`\"ten\"`")), "{}", text(&diags));
    let src = EXIT_ON_BANKRUPTCY.replace("param why : Label = \"bankruptcy\"", "param why : Label = 5");
    let diags = check(&src, "exit_on_bankruptcy");
    assert!(errors(&diags).iter().any(|d| d.code == Code::T && d.message.contains("Label")), "{}", text(&diags));
    // Labels admit `=` only.
    let src = EXIT_ON_BANKRUPTCY.replace("R = why.", "R < why.");
    let diags = check(&src, "exit_on_bankruptcy");
    assert!(errors(&diags).iter().any(|d| d.code == Code::T && d.message.contains("`=`")), "{}", text(&diags));
    // A label in an Equity position.
    let src = EXIT_ON_BANKRUPTCY.replace("member(A, T, \"SPX\")", "member(why, T, \"SPX\")");
    let diags = check(&src, "exit_on_bankruptcy");
    assert!(errors(&diags).iter().any(|d| d.code == Code::T), "{}", text(&diags));
}

/// Two names over four weekdays; BBB is delisted for bankruptcy on day 3.
fn market() -> Dataset {
    let mut ds = Dataset::new();
    let a = ds.intern("AAA");
    let b = ds.intern("BBB");
    let spx = ds.intern_label("SPX");
    let bankrupt = ds.intern_label("bankruptcy");
    for (i, d) in ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11"].iter().enumerate() {
        let t = day(d);
        for (sym, p) in [(a, 10.0), (b, 5.0)] {
            ds.add("close", vec![Value::Equity(sym), Value::Time(t), Value::Num(p)]);
            ds.add("volume", vec![Value::Equity(sym), Value::Time(t), Value::Num(1_000_000.0)]);
            ds.add("universe", vec![Value::Equity(sym), Value::Time(t)]);
            ds.add("member", vec![Value::Equity(sym), Value::Time(t), Value::Label(spx)]);
        }
        // Delisted from day 3 on: a point-in-time status, present at every later bar.
        if i >= 2 {
            ds.add("delisted", vec![Value::Equity(b), Value::Time(t), Value::Label(bankrupt)]);
        }
    }
    ds
}

#[test]
fn labels_flow_through_the_kernel() {
    let p = program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    let r = run(
        &p,
        &market(),
        ExecConfig {
            initial_cash: 10_000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    // Day 1 buys both; on day 3 the executor force-closes BBB (delisted for
    // bankruptcy, a total loss) before the strategy can sell it, and the
    // strategy never re-enters it.
    assert_eq!(decisions, vec!["2024-01-08 buy(AAA, 10)", "2024-01-08 buy(BBB, 10)"]);
    assert!(r.fills.iter().any(|f| f.forced && f.quantity == -10.0 && format_timestamp(f.t) == "2024-01-10"), "{:?}", r.fills);
    // A label is a value in its own right, distinct from an equity of the same spelling.
    assert_ne!(Value::Label(0), Value::Equity(0));
    assert!(Value::Equity(0) < Value::Label(0), "values of different kinds order by kind");
    // Explain shows the label by name.
    let p2 = program(&EXIT_ON_BANKRUPTCY.replace("\"bankruptcy\"", "\"acquisition\""), "exit_on_bankruptcy");
    let r2 = run(
        &p2,
        &market(),
        ExecConfig {
            initial_cash: 10_000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(r2.decisions.len(), 2, "{:?}", r2.decisions.len());
    assert!(r2.fills.iter().any(|f| f.forced), "the delisting closes the name whatever the strategy's exit says");
}

#[test]
fn a_label_column_loads_from_parquet_and_round_trips() {
    let p = program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    let dir = std::env::temp_dir().join(format!("abt-labels-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_parquet_dir(&p, &market(), &dir).unwrap();
    let delisted = to_text(&dir.join("delisted.parquet")).unwrap();
    assert!(delisted.contains("BBB,2024-01-10,bankruptcy"), "{}", delisted);
    let (ds, notes) = load_parquet_dir(&p, &dir).unwrap();
    assert!(notes.is_empty(), "{:?}", notes);
    let reasons: Vec<&Value> = ds.facts["delisted"].iter().map(|tu| &tu[2]).collect();
    assert_eq!(reasons.len(), 2);
    assert!(matches!(reasons[0], Value::Label(_)), "{:?}", reasons[0]);
    assert_eq!(ds.label_name(reasons[0]), Some("bankruptcy"));
    let r = run(
        &p,
        &ds,
        ExecConfig {
            initial_cash: 10_000.0,
            ..ExecConfig::frictionless()
        },
    )
    .unwrap();
    assert_eq!(r.decisions.len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_parameter_override_resolves_a_bare_name_against_the_label_type() {
    let p = program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    let cfg = ExecConfig {
        initial_cash: 10_000.0,
        param_overrides: vec![("why".to_string(), Lit::Str("acquisition".into()))],
        ..ExecConfig::frictionless()
    };
    let r = run(&p, &market(), cfg).unwrap();
    assert_eq!(r.decisions.len(), 2, "overridden to acquisition, the bankruptcy exit does not fire");
    let cfg = ExecConfig {
        initial_cash: 10_000.0,
        param_overrides: vec![("why".to_string(), Lit::Int(3))],
        ..ExecConfig::frictionless()
    };
    let err = run(&p, &market(), cfg).err().unwrap().to_string();
    assert!(err.contains("Label"), "{}", err);
}
