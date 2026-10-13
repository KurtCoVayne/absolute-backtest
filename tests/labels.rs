//! The `Label` type (data-bundle doc, section 3: delisting reasons,
//! classification schemes and codes, index names are names from a closed
//! vocabulary the bundle defines, compared with `=` only). A string literal
//! is an entity or a `Label` from context, the way a bare integer is a
//! Count or a Scalar; the checker resolves it, so the kernel never guesses.

mod common;

use std::fs;

use absolute_backtest::check::Code;
use absolute_backtest::data::{load_parquet_dir, write_parquet_dir};
use absolute_backtest::kernel::time::format_timestamp;
use absolute_backtest::kernel::{run, Dataset, ExecConfig, Value};
use absolute_backtest::table::to_text;
use absolute_backtest::{Lit, Ty};

/// Buy the index members, sell a name delisted for the reason `why`.
const EXIT_ON_BANKRUPTCY: &str = r#"
strategy exit_on_bankruptcy {
  data { class equity   need bars @1d   need actions, listing   need lists "SPX"   require tested   prefer latest }
  uses calendar, book
  clock { decide on base }
  universe { universe(A, T) :- listed(A, T), traded@base(A, T). }
  params {
    why : Label = "bankruptcy"
    qty : Quantity<Shares> = 10 shares
  }
  rules {
    rel gone(A: Equity, @T)
    gone(A, T) :- delisted(A, T, R), R = why.
  }
  decisions {
    silence hold
    orders market
    decide(T, trade(A, qty)) :- universe(A, T), member(A, T, "SPX"), flat(A, T), not delisted(A, T, _).
    decide(T, trade(A, Q)) :- held(A, T, Q0), gone(A, T), Q = -Q0.
  }
  execution equity_default
  report { calendar base   periods 252 per year }
}
"#;

#[test]
fn label_is_a_type_and_a_string_literal_resolves_by_context() {
    assert_eq!(Ty::parse("Label", None), Some(Ty::Label));
    assert!(!Ty::Label.is_entity(), "a label never identifies a tuple");
    let (_, diags) = common::check(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    assert!(diags.is_empty(), "a label literal is not a ticker snapshot:\n{}", common::text(&diags));
    let p = common::program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    // The checked program carries resolved literals: no bare string is left.
    assert_eq!(p.param("exit_on_bankruptcy", "why").unwrap().value, Lit::Label("bankruptcy".into()));
    let mut seen = Vec::new();
    for r in &p.rules {
        for (l, _) in absolute_backtest::check::dof::literals_in_rule(r) {
            match &l {
                Lit::Str(s) => panic!("unresolved string literal `{}` in {}", s, r.head.name),
                Lit::Label(s) | Lit::Entity(s) => seen.push((r.head.name.clone(), l.ty(), s.clone())),
                _ => {}
            }
        }
    }
    assert_eq!(seen, vec![("decide".to_string(), Ty::Label, "SPX".to_string())]);
}

#[test]
fn a_ticker_literal_is_still_an_equity_and_warned() {
    let src = EXIT_ON_BANKRUPTCY.replace(
        "decide(T, trade(A, Q)) :- held(A, T, Q0), gone(A, T), Q = -Q0.",
        "decide(T, trade(A, Q)) :- held(A, T, Q0), gone(A, T), A = \"AAA\", Q = -Q0.",
    );
    let (_, diags) = common::check(&src, "exit_on_bankruptcy");
    assert!(common::errors(&diags).is_empty(), "{}", common::text(&diags));
    assert_eq!(diags.len(), 1, "{}", common::text(&diags));
    assert_eq!(diags[0].code, Code::W6);
    let p = common::program(&src, "exit_on_bankruptcy");
    let lits: Vec<Lit> = p.rules.iter().flat_map(absolute_backtest::check::dof::literals_in_rule).map(|(l, _)| l).collect();
    assert!(lits.contains(&Lit::Entity("AAA".into())) && lits.contains(&Lit::Label("SPX".into())), "{:?}", lits);
}

#[test]
fn a_string_literal_without_a_typed_context_is_a_type_error() {
    let errors_of = |src: &str| common::errors(&common::check(src, "exit_on_bankruptcy").1);
    // Assigned to a fresh variable: nothing says entity or Label.
    let src = EXIT_ON_BANKRUPTCY.replace("gone(A, T) :- delisted(A, T, R), R = why.", "gone(A, T) :- delisted(A, T, R), W = \"bankruptcy\", R = W.");
    let errs = errors_of(&src);
    assert!(!errs.is_empty() && errs.iter().all(|(c, _)| *c == Code::T), "{:?}", errs);
    assert!(errs[0].1.contains("string literal") && errs[0].1.contains("from context"), "{}", errs[0].1);
    // Two literals compared: the same.
    let src = EXIT_ON_BANKRUPTCY.replace("R = why.", "R = why, \"a\" = \"b\".");
    let errs = errors_of(&src);
    assert!(!errs.is_empty() && errs.iter().all(|(c, _)| *c == Code::T), "{:?}", errs);
    // A label where a quantity is expected, and a number where a label is.
    let src = EXIT_ON_BANKRUPTCY.replace("qty : Quantity<Shares> = 10 shares", "qty : Quantity<Shares> = \"ten\"");
    let errs = errors_of(&src);
    assert!(errs.iter().any(|(c, m)| *c == Code::T && m.contains("`\"ten\"`")), "{:?}", errs);
    let src = EXIT_ON_BANKRUPTCY.replace("why : Label = \"bankruptcy\"", "why : Label = 5");
    let errs = errors_of(&src);
    assert!(errs.iter().any(|(c, m)| *c == Code::T && m.contains("Label")), "{:?}", errs);
    // Labels admit `=` only.
    let src = EXIT_ON_BANKRUPTCY.replace("R = why.", "R < why.");
    let errs = errors_of(&src);
    assert!(errs.iter().any(|(c, m)| *c == Code::T && m.contains("`=`")), "{:?}", errs);
    // A label in an entity position.
    let src = EXIT_ON_BANKRUPTCY.replace("member(A, T, \"SPX\")", "member(why, T, \"SPX\")");
    let errs = errors_of(&src);
    assert!(errs.iter().any(|(c, _)| *c == Code::T), "{:?}", errs);
}

/// Two names over four weekdays, both members of SPX; BBB is delisted for
/// bankruptcy on day 3.
fn market() -> Dataset {
    let mut ds = common::markets((2024, 1, 8), &[("AAA", &[10.0; 4]), ("BBB", &[5.0; 4])]);
    let a = ds.intern("AAA");
    let b = ds.intern("BBB");
    let spx = ds.intern_label("SPX");
    let bankrupt = ds.intern_label("bankruptcy");
    for (i, d) in ["2024-01-08", "2024-01-09", "2024-01-10", "2024-01-11"].iter().enumerate() {
        let t = common::day(d);
        for sym in [a, b] {
            ds.add("member", vec![Value::Equity(sym), Value::Time(t), Value::Label(spx)]);
        }
        // Delisted from day 3 on: a point-in-time status, present at every later bar.
        if i >= 2 {
            ds.add("delisted", vec![Value::Equity(b), Value::Time(t), Value::Label(bankrupt)]);
        }
    }
    ds
}

fn cfg() -> ExecConfig {
    ExecConfig {
        initial_cash: 10_000.0,
        ..ExecConfig::frictionless()
    }
}

#[test]
fn labels_flow_through_the_kernel() {
    let p = common::program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    let r = run(&p, &market(), cfg()).unwrap();
    let decisions: Vec<String> = r.decisions.iter().map(|d| format!("{} {}", format_timestamp(d.t), r.describe_decision(&d.decision))).collect();
    // Day 1 buys both; on day 3 the executor force-closes BBB (delisted for
    // bankruptcy, a total loss) before the strategy can sell it, and the
    // strategy never re-enters it.
    assert_eq!(decisions, vec!["2024-01-08 trade(AAA, 10)", "2024-01-08 trade(BBB, 10)"]);
    assert!(r.fills.iter().any(|f| f.forced && f.quantity == -10.0 && format_timestamp(f.t) == "2024-01-10"), "{:?}", r.fills);
    // A label is a value in its own right, distinct from an equity of the same spelling.
    assert_ne!(Value::Label(0), Value::Equity(0));
    assert!(Value::Equity(0) < Value::Label(0), "values of different kinds order by kind");
    // Another reason of the vocabulary: the delisting still closes the name.
    let p2 = common::program(&EXIT_ON_BANKRUPTCY.replace("\"bankruptcy\"", "\"acquisition\""), "exit_on_bankruptcy");
    let r2 = run(&p2, &market(), cfg()).unwrap();
    assert_eq!(r2.decisions.len(), 2, "{:?}", r2.decisions.len());
    assert!(r2.fills.iter().any(|f| f.forced), "the delisting closes the name whatever the strategy's exit says");
}

#[test]
fn a_label_column_loads_from_parquet_and_round_trips() {
    let p = common::program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    let dir = std::env::temp_dir().join(format!("abt-labels-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_parquet_dir(&p, &market(), &dir).unwrap();
    let delisted = to_text(&dir.join("delisted.parquet")).unwrap();
    assert!(delisted.contains("BBB,2024-01-10,bankruptcy"), "{}", delisted);
    let (ds, notes) = load_parquet_dir(&p, &dir).unwrap();
    // The demand brings `split`, `dividend` and `classification` into the
    // program too; the market carries no such facts, so their files are
    // absent and noted. Nothing else is noted.
    let absent = ["split.parquet", "dividend.parquet", "classification.parquet"];
    assert!(notes.iter().all(|n| n.contains("no file") && absent.iter().any(|f| n.contains(f))), "{:?}", notes);
    let reasons: Vec<&Value> = ds.facts["delisted"].iter().map(|tu| &tu[2]).collect();
    assert_eq!(reasons.len(), 2);
    assert!(matches!(reasons[0], Value::Label(_)), "{:?}", reasons[0]);
    assert_eq!(ds.label_name(reasons[0]), Some("bankruptcy"));
    let r = run(&p, &ds, cfg()).unwrap();
    assert_eq!(r.decisions.len(), 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_parameter_override_resolves_a_bare_name_against_the_label_type() {
    let p = common::program(EXIT_ON_BANKRUPTCY, "exit_on_bankruptcy");
    let cfg = ExecConfig {
        param_overrides: vec![("why".to_string(), Lit::Str("acquisition".into()))],
        ..cfg()
    };
    let r = run(&p, &market(), cfg).unwrap();
    assert_eq!(r.decisions.len(), 2, "overridden to acquisition, the bankruptcy exit does not fire");
    let cfg = ExecConfig {
        param_overrides: vec![("why".to_string(), Lit::Int(3))],
        ..self::cfg()
    };
    let err = run(&p, &market(), cfg).err().unwrap().to_string();
    assert!(err.contains("Label"), "{}", err);
}
