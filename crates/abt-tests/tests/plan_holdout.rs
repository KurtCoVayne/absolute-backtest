//! The hold-out forms of Q-2.1 beyond `trailing:Ny` (ERRATA E-93): `after
//! TIME` truncates every run at the last barrier closed by the time, so no
//! non-reveal run reads a bar received after it, and a reveal reports
//! exactly the bars after it (Proposition Q-2.1 unchanged); `trailing:N%`
//! embargoes the last N % of the span between the window's first and last
//! decision bars; `trailing:Nmo` counts months; the parser rejects the
//! malformed forms and every form round-trips through its description.

#![allow(clippy::result_large_err)]

use abt_kernel::data::synthetic_daily;
use abt_query::data::Data;
use abt_query::kernel::{Kernel, Program};
use abt_query::plan::exec::{execute, Endpoints, Execution, Kind, Options};
use abt_query::plan::parse::holdout;
use abt_query::plan::{self, Holdout, Plan};
use abt_query::time::DAY;
use abt_tests as common;
use abt_wire::scalar::parse_timestamp;

use abt_tests::{momentum as momentum_with, plan_program as program, plan_services as services, FRICTIONLESS};

fn study(text: &str, name: &str) -> Plan {
    plan::plan(&plan::parse(text).unwrap(), name).unwrap()
}

fn run(p: &Plan, prog: &Program, data: &Data, kernels: &[Kernel], reveal: Option<&str>) -> Execution {
    let opts = Options {
        threads: 2,
        reveal: reveal.map(str::to_string),
        ..Options::default()
    };
    execute(p, prog, &Endpoints { data, kernels }, &opts).unwrap_or_else(|e| panic!("{}: {}", p.name, e))
}

fn ts(s: &str) -> i64 {
    parse_timestamp(s).unwrap()
}

#[test]
fn an_after_hold_out_truncates_every_run_at_its_time_and_a_reveal_reports_the_bars_after_it() {
    let src = momentum_with("after", FRICTIONLESS);
    let prog = common::program(&src, "after");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 600, 9);
    let (data, kernels) = services(&prog, &ds, 1);
    // A daily bar labelled at midnight closes at the end of its day (E-13):
    // the bar labelled on the time's own day is received after it.
    let time = ts("2023-09-15");
    let p = study(
        "study after { program after\n holdout after 2023-09-15\n grid { n = 1, 2 }\n steps { grid main\n reveal final of best(main) } }",
        "after",
    );
    assert_eq!(p.holdout, Holdout::After { time });
    assert!(plan::normal_form(&p, "0").unwrap().text.contains("  holdout after 2023-09-15\n"));
    let pg = program(&src, "after");
    let ex = run(&p, &pg, &data, &kernels, None);
    assert!(ex.embargo.truncates);
    let start = ex.embargo.start().expect("bars after the time are embargoed");
    assert_eq!(start, time, "the embargo begins at the first bar whose barrier closes after the time");
    // No run reads a bar received after the time: every bar it folded is
    // labelled before the time's day, and the embargo holds the rest.
    let mut kept = 0;
    for o in &ex.outcomes {
        assert_eq!(o.kind, Kind::Trial);
        let run = abt_query::run::Run::read(&data, &o.run_id).unwrap();
        assert!(!run.book.is_empty());
        assert!(run.book.iter().all(|b| b.t + DAY - 1 <= time), "run {} read a bar received after {}", o.run_id, p.holdout.describe());
        assert!(run.book.iter().all(|b| !ex.embargo.contains(b.t)));
        kept = run.book.len();
    }
    // The reveal runs the whole sample and reports exactly the bars after
    // the time.
    let best = ex.outcomes[ex.grids[0].runs[ex.grids[0].best]].clone();
    let r = run(&p, &pg, &data, &kernels, Some("final"));
    let revealed = &r.outcomes[0];
    assert_eq!(revealed.kind, Kind::Reveal);
    assert_eq!(revealed.point, best.point);
    let whole = abt_query::run::Run::read(&data, &revealed.run_id).unwrap();
    assert_eq!(whole.book.len(), 600, "the reveal runs the whole sample");
    let after = whole.book.iter().filter(|b| b.t + DAY - 1 > time).count();
    assert_eq!(kept + after, 600, "the bars read and the bars revealed partition the sample");
    assert_eq!(revealed.returns.n + 1, after);
    assert_eq!(whole.book.iter().filter(|b| ex.embargo.contains(b.t)).count(), after);
    // A time with a clock and nanoseconds cuts inside a day: the bar of that
    // day closes after it, so it is embargoed too, and the plan's text keeps
    // the nanoseconds.
    let fine = study("study fine { program after\n holdout after 2023-09-15T10:30:00.000000005\n steps { run main } }", "fine");
    assert_eq!(
        fine.holdout,
        Holdout::After {
            time: time + 10 * 3_600_000_000_000 + 30 * 60_000_000_000 + 5
        }
    );
    assert_eq!(fine.holdout.describe(), "after 2023-09-15T10:30:00.000000005");
    let fx = run(&fine, &pg, &data, &kernels, None);
    assert_eq!(fx.embargo.start(), Some(time));
    let read = abt_query::run::Run::read(&data, &fx.outcomes[0].run_id).unwrap();
    assert_eq!(read.book.len(), kept);
    // A time after the sample embargoes nothing, and there is nothing to
    // reveal; one before it leaves no bar to read.
    let late = study("study late { program after\n holdout after 2030-01-01\n steps { run main\n reveal final of main } }", "late");
    let lx = run(&late, &pg, &data, &kernels, None);
    assert!(lx.embargo.is_empty());
    assert_eq!(abt_query::run::Run::read(&data, &lx.outcomes[0].run_id).unwrap().book.len(), 600);
    let opts = Options {
        reveal: Some("final".into()),
        ..Options::default()
    };
    assert!(execute(&late, &pg, &Endpoints { data: &data, kernels: &kernels }, &opts).is_err_and(|e| e.to_string().contains("nothing to reveal")));
}

#[test]
fn a_percentage_hold_out_embargoes_its_share_of_the_window() {
    let src = momentum_with("share", FRICTIONLESS);
    let prog = common::program(&src, "share");
    let ds = synthetic_daily(&["AAA", "BBB", "CCC"], (2022, 1, 3), 600, 9);
    let (data, kernels) = services(&prog, &ds, 1);
    let pg = program(&src, "share");
    let p = study("study share { program share\n holdout trailing:20%\n steps { run main\n reveal final of main } }", "share");
    assert_eq!(p.holdout, Holdout::TrailingShare { percent: 20 });
    let ex = run(&p, &pg, &data, &kernels, None);
    let (first, last) = ex.sample;
    let span = last - first;
    let start = last - (span / 5 + i64::from(span % 5 != 0));
    assert_eq!(ex.embargo.start(), Some(start), "the last fifth of the span between the first and last decision bars");
    // The reveal's run is the whole sample: the bars at or after the start
    // are the embargoed ones, which the run stopped before and the reveal
    // reports.
    let r = run(&p, &pg, &data, &kernels, Some("final"));
    let whole = abt_query::run::Run::read(&data, &r.outcomes[0].run_id).unwrap();
    assert_eq!(whole.book.len(), 600);
    let embargoed = whole.book.iter().filter(|b| b.t >= start).count();
    assert!(embargoed > 100 && embargoed < 140, "{} of 600 bars", embargoed);
    let read = abt_query::run::Run::read(&data, &ex.outcomes[0].run_id).unwrap();
    assert_eq!(read.book.len(), 600 - embargoed, "the run stops before the embargo");
    assert_eq!(r.outcomes[0].returns.n + 1, embargoed, "the reveal reports the embargoed bars");
    // With a window, the share is of the window's span, not the sample's.
    let w = study(
        "study windowed { program share\n holdout trailing:25%\n window 2022-07-01 .. 2023-07-01\n steps { run main } }",
        "windowed",
    );
    let wx = run(&w, &pg, &data, &kernels, None);
    let (wf, wl) = (ts("2022-07-01").max(first), ts("2023-07-01").min(last));
    let wspan = wl - wf;
    assert_eq!(wx.embargo.start(), Some(wl - (wspan / 4 + i64::from(wspan % 4 != 0))));
    let read = abt_query::run::Run::read(&data, &wx.outcomes[0].run_id).unwrap();
    assert!(read.book.iter().all(|b| wf <= b.t && b.t < wx.embargo.start().unwrap()));
    // Months count back from the sample's last bar as years do.
    let m = study("study months { program share\n holdout trailing:6mo\n steps { run main } }", "months");
    assert_eq!(m.holdout, Holdout::Trailing { months: 6 });
    let mx = run(&m, &pg, &data, &kernels, None);
    assert_eq!(mx.embargo.start(), Some(abt_wire::scalar::add_months(last, -6)));
    assert_eq!(mx.holdout, "trailing:6mo");
}

#[test]
fn the_hold_out_forms_parse_round_trip_and_the_malformed_are_refused() {
    for (text, want) in [
        ("none", Holdout::None),
        ("trailing:2y", Holdout::Trailing { months: 24 }),
        ("trailing:24mo", Holdout::Trailing { months: 24 }),
        ("trailing:6mo", Holdout::Trailing { months: 6 }),
        ("trailing:20%", Holdout::TrailingShare { percent: 20 }),
        ("after 2024-01-01", Holdout::After { time: ts("2024-01-01") }),
        ("after 2024-01-01T09:30", Holdout::After { time: ts("2024-01-01T09:30") }),
        ("after 2024-01-01T09:30:15.25", Holdout::After { time: ts("2024-01-01T09:30:15.25") }),
        (
            "after 2024-01-01T09:30:15.123456789",
            Holdout::After {
                time: ts("2024-01-01T09:30:15.123456789"),
            },
        ),
        ("blocks:2:3mo:5", Holdout::Blocks { count: 2, months: 3, seed: 5 }),
    ] {
        let h = holdout(text).unwrap_or_else(|e| panic!("{}: {}", text, e));
        assert_eq!(h, want, "{}", text);
        assert_eq!(holdout(&h.describe()).unwrap(), h, "{} round-trips through `{}`", text, h.describe());
    }
    assert_eq!(holdout("trailing:24mo").unwrap().describe(), "trailing:2y", "one text per hold-out");
    assert_eq!(holdout("after 2024-01-01T00:00").unwrap().describe(), "after 2024-01-01");
    for bad in [
        "trailing:0%",
        "trailing:100%",
        "trailing:150%",
        "trailing:x%",
        "trailing:0mo",
        "trailing:2d",
        "trailing:",
        "trailing:20 %",
        "after",
        "after yesterday",
        "after 2024-13-01",
        "after 2024-01-01T25:00",
        "after 2024-01-01T09:30:15.1234567890",
        "after 2024-01-01 2024-02-01",
        "before 2024-01-01",
    ] {
        assert!(holdout(bad).is_err(), "`{}` is not a hold-out", bad);
    }
    // In a study: the field's text to the end of its line.
    let p = study("study s { program p.dsl\n holdout after 2024-01-01T09:30\n steps { run a\n reveal r of a } }", "s");
    assert_eq!(p.holdout.describe(), "after 2024-01-01T09:30:00");
    assert!(plan::plan(&plan::parse("study s { program p.dsl\n holdout after soon }").unwrap(), "s").is_err_and(|e| e.contains("not a time")));
}
