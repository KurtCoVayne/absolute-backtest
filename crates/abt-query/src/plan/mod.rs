//! Plans (Q-2): studies as declared DAGs of runs, grids, walk-forwards,
//! reveals, tests, claims and reports. A plan is a unit in the block syntax
//! of the language (`parse`); its normal form, with templates substituted
//! and names canonicalised, is what its hash H(Π) is over (`normal`).

pub mod exec;
pub mod lineage;
pub mod normal;
pub mod parse;
pub mod trials;

pub use normal::{normal_form, Normal};
pub use parse::{parse, plan, Unit};

/// Where a plan's program is: a path to its file, or a strategy's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramRef {
    Path(String),
    Name(String),
}

/// The hold-out policy (Q-2.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holdout {
    None,
    /// The last `years` of the sample are embargoed by truncation.
    Trailing {
        years: u32,
    },
    /// `count` random blocks of `months` months, masked; drawn by `seed`.
    Blocks {
        count: u32,
        months: u32,
        seed: u64,
    },
}

impl Holdout {
    pub fn describe(&self) -> String {
        match self {
            Holdout::None => "none".into(),
            Holdout::Trailing { years } => format!("trailing:{}y", years),
            Holdout::Blocks { count, months, seed } => format!("blocks:{}:{}mo:{}", count, months, seed),
        }
    }
}

/// A walk-forward scheme (Q-2.2): anchored or rolling, with its train and
/// test windows in months.
pub use crate::metrics::validate::WalkForward;

/// A window of dates, `DATE .. DATE`, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub from: String,
    pub to: String,
}

impl Window {
    pub fn describe(&self) -> String {
        format!("{} .. {}", self.from, self.to)
    }
}

/// A parameter point: names with literal texts (the kernel parses them).
pub type Point = Vec<(String, String)>;

/// A step another step reads: the step itself, or the best point of a grid
/// by the plan's objective (`best(GRID)`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ref {
    pub step: String,
    pub best: bool,
}

impl Ref {
    pub fn parse(s: &str) -> Ref {
        let s = s.trim();
        match s.strip_prefix("best(").and_then(|r| r.strip_suffix(')')) {
            Some(g) => Ref {
                step: g.trim().to_string(),
                best: true,
            },
            None => Ref { step: s.to_string(), best: false },
        }
    }

    /// As written, with the step named by `name`.
    pub fn render(&self, name: &dyn Fn(&str) -> String) -> String {
        if self.best {
            format!("best({})", name(&self.step))
        } else {
            name(&self.step)
        }
    }
}

/// The input of a test or a claim (Q-5.1's INPUT, with `grid NAME`, E-22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// A relation of the binding, with an optional entity filter as written.
    Feed { relation: String, filter: Option<String> },
    /// A relation the run observed (its ledger).
    Feature { relation: String, of: Ref },
    /// The run's bar returns.
    Returns(Ref),
    /// The run's event log.
    Events(Ref),
    /// A `ledger` step's table.
    Ledger(String),
    /// A grid's outcomes, point by point.
    Grid(String),
}

impl Input {
    pub fn parse(s: &str) -> Result<Input, String> {
        let s = s.trim();
        let words: Vec<&str> = s.split_whitespace().collect();
        Ok(match words.as_slice() {
            ["feed", ..] => {
                let rest = s["feed".len()..].trim();
                match rest.find('(') {
                    Some(i) if rest.ends_with(')') => Input::Feed {
                        relation: rest[..i].trim().to_string(),
                        filter: Some(rest[i + 1..rest.len() - 1].trim().to_string()),
                    },
                    _ => Input::Feed {
                        relation: rest.to_string(),
                        filter: None,
                    },
                }
            }
            ["feature", r, "of", s] => Input::Feature {
                relation: r.to_string(),
                of: Ref::parse(s),
            },
            ["returns", "of", s] => Input::Returns(Ref::parse(s)),
            ["events", "of", s] => Input::Events(Ref::parse(s)),
            ["ledger", s] => Input::Ledger(s.to_string()),
            ["grid", s] => Input::Grid(s.to_string()),
            _ => {
                return Err(format!(
                    "`{}` is not an input: feed RELATION[(filter)], feature R of RUN, returns of RUN, events of RUN, ledger NAME or grid NAME",
                    s
                ))
            }
        })
    }

    /// The steps the input reads.
    pub fn steps(&self) -> Vec<String> {
        match self {
            Input::Feed { .. } => vec![],
            Input::Feature { of, .. } | Input::Returns(of) | Input::Events(of) => vec![of.step.clone()],
            Input::Ledger(s) | Input::Grid(s) => vec![s.clone()],
        }
    }

    pub fn render(&self, name: &dyn Fn(&str) -> String) -> String {
        match self {
            Input::Feed { relation, filter: None } => format!("feed {}", relation),
            Input::Feed { relation, filter: Some(f) } => format!("feed {}({})", relation, f),
            Input::Feature { relation, of } => format!("feature {} of {}", relation, of.render(name)),
            Input::Returns(of) => format!("returns of {}", of.render(name)),
            Input::Events(of) => format!("events of {}", of.render(name)),
            Input::Ledger(s) => format!("ledger {}", name(s)),
            Input::Grid(s) => format!("grid {}", name(s)),
        }
    }
}

/// A comparison in an expectation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    pub fn holds(self, a: f64, b: f64) -> bool {
        match self {
            Op::Lt => a < b,
            Op::Le => a <= b,
            Op::Gt => a > b,
            Op::Ge => a >= b,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
        }
    }
}

/// What a claim expects of its test (Q-5.1's OUTCOME).
#[derive(Clone, Debug, PartialEq)]
pub enum Expect {
    Reject,
    Accept,
    Statistic(Op, f64),
    Effect(Op, f64),
    IntervalExcludes(f64),
}

impl Expect {
    pub fn render(&self) -> String {
        match self {
            Expect::Reject => "reject".into(),
            Expect::Accept => "accept".into(),
            Expect::Statistic(op, v) => format!("statistic {} {}", op.text(), v),
            Expect::Effect(op, v) => format!("effect {} {}", op.text(), v),
            Expect::IntervalExcludes(v) => format!("interval excludes {}", v),
        }
    }
}

/// When a claim is judged: on the in-sample window before any run, or on
/// the outcomes after them (Q-5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Before,
    After,
}

/// What a claim's failure does: warns (the default), or blocks the steps
/// that depend on it (Q-5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Warn,
    Block,
}

/// A claim (Definition Q-5.1).
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub name: String,
    pub test: String,
    pub input: Input,
    pub over: Option<Window>,
    pub params: Point,
    pub expect: Expect,
    pub level: f64,
    pub phase: Phase,
    pub policy: Policy,
}

impl Claim {
    /// As written, its name and the steps it reads named by `name`.
    pub fn render(&self, own: &str, name: &dyn Fn(&str) -> String) -> String {
        let mut s = format!("{} : {} on {}", own, self.test, self.input.render(name));
        if let Some(w) = &self.over {
            s.push_str(&format!(" over {}", w.describe()));
        }
        if !self.params.is_empty() {
            s.push_str(&format!(" with {}", render_point(&self.params)));
        }
        s.push_str(&format!(
            " expect {} at {} {} {}",
            self.expect.render(),
            self.level,
            if self.phase == Phase::Before { "before" } else { "after" },
            if self.policy == Policy::Block { "block" } else { "warn" }
        ));
        s
    }
}

/// `{ k = v, ... }` with the names in order.
pub fn render_point(p: &Point) -> String {
    let mut p = p.clone();
    p.sort();
    format!("{{ {} }}", p.iter().map(|(k, v)| format!("{} = {}", k, v)).collect::<Vec<_>>().join(", "))
}

/// The point a run step runs at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At {
    /// A point written out.
    Point(Point),
    /// The best point of a grid by the plan's objective.
    Best(String),
}

/// A step of a plan (Q-2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Run {
        name: String,
        at: Option<At>,
        over: Option<Window>,
        observe: Vec<String>,
    },
    Grid {
        name: String,
    },
    WalkForward {
        name: String,
    },
    Reveal {
        name: String,
        of: Ref,
    },
    Test {
        name: String,
        test: String,
        input: Input,
        params: Point,
        over: Option<Window>,
    },
    Compare {
        name: String,
        runs: Vec<Ref>,
        by: Vec<String>,
    },
    Ledger {
        name: String,
        relation: String,
        of: Ref,
        into: String,
    },
    Feed {
        name: String,
        statistic: String,
        relation: String,
        over: Option<Window>,
    },
    Activation {
        name: String,
        of: Ref,
    },
    Report {
        name: String,
    },
}

/// The kinds of step in the order the normal form breaks ties by (Q-2.4).
pub const KINDS: [&str; 10] = ["run", "grid", "walk_forward", "reveal", "test", "compare", "ledger", "feed", "activation", "report"];

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::Run { name, .. }
            | Step::Grid { name }
            | Step::WalkForward { name }
            | Step::Reveal { name, .. }
            | Step::Test { name, .. }
            | Step::Compare { name, .. }
            | Step::Ledger { name, .. }
            | Step::Feed { name, .. }
            | Step::Activation { name, .. }
            | Step::Report { name } => name,
        }
    }

    /// The step's kind, as written.
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Run { .. } => "run",
            Step::Grid { .. } => "grid",
            Step::WalkForward { .. } => "walk_forward",
            Step::Reveal { .. } => "reveal",
            Step::Test { .. } => "test",
            Step::Compare { .. } => "compare",
            Step::Ledger { .. } => "ledger",
            Step::Feed { .. } => "feed",
            Step::Activation { .. } => "activation",
            Step::Report { .. } => "report",
        }
    }

    /// The steps this one reads (Q-2.2: a step's inputs name earlier steps).
    pub fn inputs(&self) -> Vec<String> {
        let mut out = match self {
            Step::Run { at: Some(At::Best(g)), .. } => vec![g.clone()],
            Step::Run { .. } | Step::Grid { .. } | Step::WalkForward { .. } | Step::Feed { .. } | Step::Report { .. } => vec![],
            Step::Reveal { of, .. } | Step::Ledger { of, .. } | Step::Activation { of, .. } => vec![of.step.clone()],
            Step::Test { input, .. } => input.steps(),
            Step::Compare { runs, .. } => runs.iter().map(|r| r.step.clone()).collect(),
        };
        out.sort();
        out.dedup();
        out
    }

    /// Whether the step asks the kernel for runs (and so counts trials).
    pub fn runs(&self) -> bool {
        matches!(self, Step::Run { .. } | Step::Grid { .. } | Step::WalkForward { .. } | Step::Reveal { .. })
    }

    /// The step as written, its name and the steps it reads named by `name`.
    pub fn render(&self, own: &str, name: &dyn Fn(&str) -> String) -> String {
        let over = |w: &Option<Window>| w.as_ref().map(|w| format!(" over {}", w.describe())).unwrap_or_default();
        match self {
            Step::Run { at, over: w, observe, .. } => {
                let mut s = format!("run {}", own);
                match at {
                    Some(At::Point(p)) => s.push_str(&format!(" at {}", render_point(p))),
                    Some(At::Best(g)) => s.push_str(&format!(" at best({})", name(g))),
                    None => {}
                }
                s.push_str(&over(w));
                if !observe.is_empty() {
                    let mut o = observe.clone();
                    o.sort();
                    o.dedup();
                    s.push_str(&format!(" observe {}", o.join(", ")));
                }
                s
            }
            Step::Grid { .. } => format!("grid {}", own),
            Step::WalkForward { .. } => format!("walk_forward {}", own),
            Step::Reveal { of, .. } => format!("reveal {} of {}", own, of.render(name)),
            Step::Test { test, input, params, over: w, .. } => {
                let mut s = format!("test {}: {} on {}", own, test, input.render(name));
                if !params.is_empty() {
                    s.push_str(&format!(" with {}", render_point(params)));
                }
                s.push_str(&over(w));
                s
            }
            Step::Compare { runs, by, .. } => format!("compare {}: {} by {}", own, runs.iter().map(|r| r.render(name)).collect::<Vec<_>>().join(", "), by.join(", ")),
            Step::Ledger { relation, of, into, .. } => format!("ledger {}: {} of {} into {}", own, relation, of.render(name), into),
            Step::Feed { statistic, relation, over: w, .. } => format!("feed {}: {} on {}{}", own, statistic, relation, over(w)),
            Step::Activation { of, .. } => format!("activation {} of {}", own, of.render(name)),
            Step::Report { .. } => format!("report {}", own),
        }
    }
}

/// The report block (Q-8): the metrics and tables a report shows, and
/// whether it renders text. Not part of the normal form (Q-2.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportSpec {
    pub metrics: Vec<String>,
    pub tables: Vec<String>,
    pub text: bool,
}

/// A budget (D-7.2): what the plan's demands may spend, as written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub money: Option<String>,
    pub bytes: Option<String>,
    pub requests: Option<String>,
}

/// A plan (Q-2.1), its templates substituted.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub name: String,
    pub program: ProgramRef,
    pub params: Point,
    pub grid: Vec<(String, Vec<String>)>,
    pub holdout: Holdout,
    pub walk_forward: Option<WalkForward>,
    pub window: Option<Window>,
    pub budget: Option<Budget>,
    pub objective: String,
    pub claims: Vec<Claim>,
    pub steps: Vec<Step>,
    pub report: ReportSpec,
}

impl Plan {
    /// The steps of a plan with no `steps` block (Q-2.1): the grid when the
    /// plan has one, else one run; the walk-forward when the plan declares
    /// a scheme; then the report. The claims run before and after these by
    /// their phase.
    pub fn default_steps(&self) -> Vec<Step> {
        let main = if self.grid.is_empty() {
            Step::Run {
                name: "main".into(),
                at: None,
                over: None,
                observe: vec![],
            }
        } else {
            Step::Grid { name: "main".into() }
        };
        let mut steps = vec![main];
        if self.walk_forward.is_some() {
            steps.push(Step::WalkForward { name: "folds".into() });
        }
        steps.push(Step::Report { name: "report".into() });
        steps
    }

    /// Every point of the grid: the base parameters overridden by one value
    /// of each axis, axis-major in the order written, each point's names in
    /// order.
    pub fn grid_points(&self) -> Vec<Point> {
        let mut points: Vec<Point> = vec![self.params.clone()];
        for (name, values) in &self.grid {
            let mut next = Vec::new();
            for p in &points {
                for v in values {
                    let mut q: Point = p.iter().filter(|(n, _)| n != name).cloned().collect();
                    q.push((name.clone(), v.clone()));
                    next.push(q);
                }
            }
            points = next;
        }
        for p in points.iter_mut() {
            p.sort();
        }
        points
    }

    /// The step named `name`.
    pub fn step(&self, name: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.name() == name)
    }
}
