//! The abstract interpretation (compiler design, sections 3.6 to 3.10):
//! transfer functions over the reduced product of dimension, sign and
//! interval with a definedness flag, a fixpoint over the strata with
//! widening by thresholds and one narrowing pass, and the warnings A001 to
//! A011. Every verdict is over the reals with outward-rounded bounds
//! (section 6.8); an aggregate's interval is widened by the error bound of
//! the kernel's summation.

use std::collections::{BTreeMap, BTreeSet};

use super::domains::{AbsVal, Interval, Sign};
use super::{Analysis, ColumnFacts, DemandSet, Keyedness, LiteralFacts, NeverTrue, RelationFacts, RuleFacts, Structure, Warning};
use crate::check::Program;
use crate::compiler::print::render_expr;
use crate::ir::*;
use crate::plan::cost::Statistics;

type State = BTreeMap<String, AbsVal>;

/// What the final pass collects per rule.
struct Collect {
    rule: usize,
    label: String,
    warnings: Vec<Warning>,
    literals: Vec<LiteralFacts>,
    never: Vec<NeverTrue>,
    may_undef: bool,
}

struct Interp<'a> {
    prog: &'a Program,
    st: &'a Structure,
    stats: Option<&'a Statistics>,
    thresholds: Vec<f64>,
    columns: BTreeMap<String, Vec<AbsVal>>,
    margin: f64,
    /// The environment is a futures one: prices may be non-positive.
    futures: bool,
    running: bool,
}

fn magnitude(l: &Lit) -> Option<f64> {
    match l {
        Lit::Int(i) => Some(*i as f64),
        Lit::Float(x) | Lit::Quantity(x, _) | Lit::Money(x, _) | Lit::Price(x, _) => Some(*x),
        _ => None,
    }
}

fn numeric_ty(t: &Ty) -> bool {
    matches!(t, Ty::Count | Ty::IntLit | Ty::Quantity(_))
}

/// The machine epsilon of a double.
const EPS: f64 = f64::EPSILON;

fn identity(v: &str) -> String {
    v.to_string()
}

impl<'a> Interp<'a> {
    fn warn(col: &mut Option<&mut Collect>, code: &str, span: Span, message: String, note: bool, bundle_relative: bool) {
        if let Some(c) = col {
            c.warnings.push(Warning {
                code: code.into(),
                rule: Some(c.label.clone()),
                span: span.to_string(),
                message,
                bundle_relative,
                note,
            });
        }
    }

    fn lit_val(&self, l: &Lit) -> AbsVal {
        match magnitude(l) {
            Some(x) => AbsVal::point(Some(&l.ty()), x),
            None => AbsVal::non_numeric(&l.ty()),
        }
    }

    /// A parameter's abstract value (3.8, item 3): its range when declared,
    /// its default as a point otherwise.
    fn param_val(&self, unit: &str, name: &str) -> AbsVal {
        let Some(p) = self.prog.param(unit, name) else { return AbsVal::top(None) };
        if !numeric_ty(&p.ty) {
            return AbsVal::non_numeric(&p.ty);
        }
        match (&p.range, magnitude(&p.value)) {
            (Some((lo, hi)), _) => match (magnitude(lo), magnitude(hi)) {
                (Some(l), Some(h)) => AbsVal::of_interval(Some(&p.ty), Interval::new(l, h)),
                _ => AbsVal::top(Some(&p.ty)),
            },
            (None, Some(x)) => AbsVal::point(Some(&p.ty), x),
            _ => AbsVal::top(Some(&p.ty)),
        }
    }

    /// The initial abstract columns of a stored relation (3.8, items 1 and 4).
    fn stored_columns(&self, sig: &Signature) -> Vec<AbsVal> {
        let rs = self.stats.and_then(|s| s.relations.get(&sig.name));
        sig.args
            .iter()
            .map(|a| {
                let mut v = match &a.ty {
                    Ty::Quantity(d) if d.c2 == 2 && d.s2 == -2 && d.t2 == 0 => {
                        // A price: positive unless the instrument is a future.
                        if self.futures || !matches!(sig.kind, Kind::Primitive { .. }) {
                            AbsVal::top(Some(&a.ty))
                        } else {
                            AbsVal {
                                sign: Sign::Pos,
                                ..AbsVal::top(Some(&a.ty))
                            }
                            .reduce()
                        }
                    }
                    Ty::Quantity(d) if d.c2 == 0 && d.s2 == 2 && d.t2 == 0 && matches!(sig.kind, Kind::Primitive { .. }) => AbsVal {
                        sign: Sign::NonNeg,
                        ..AbsVal::top(Some(&a.ty))
                    }
                    .reduce(),
                    Ty::Count => AbsVal {
                        sign: Sign::NonNeg,
                        ..AbsVal::top(Some(&a.ty))
                    }
                    .reduce(),
                    t if numeric_ty(t) => AbsVal::top(Some(t)),
                    t => AbsVal::non_numeric(t),
                };
                if let Some(c) = rs.and_then(|r| r.columns.get(&a.name)) {
                    if v.numeric && c.min.is_finite() && c.max.is_finite() {
                        v = v.meet(&AbsVal::of_interval(None, Interval::new(c.min, c.max)));
                    }
                }
                v
            })
            .collect()
    }

    fn eval(&self, e: &Expr, unit: &str, state: &State, col: &mut Option<&mut Collect>) -> AbsVal {
        match e {
            Expr::Var(v, _) => state.get(v).cloned().unwrap_or_else(|| AbsVal::top(None)),
            Expr::Param(p, _) => self.param_val(unit, p),
            Expr::Lit(l, _) => self.lit_val(l),
            Expr::Neg(x, _) => {
                let v = self.eval(x, unit, state, col);
                if !v.numeric {
                    return v;
                }
                AbsVal {
                    interval: v.interval.neg(),
                    sign: v.sign.neg(),
                    ..v
                }
                .reduce()
            }
            Expr::Bin(op, a, b, span) => {
                let va = self.eval(a, unit, state, col);
                let vb = self.eval(b, unit, state, col);
                if !va.numeric || !vb.numeric {
                    return AbsVal::top(None);
                }
                if *op == BinOp::Div && vb.interval.contains(0.0) && vb.sign.may_be_zero() {
                    Self::warn(
                        col,
                        "A001",
                        *span,
                        format!("divisor `{}` may be zero: interval {}", render_expr(b, &identity), vb.interval.text()),
                        false,
                        self.stats.is_some(),
                    );
                    if let Some(c) = col {
                        c.may_undef = true;
                    }
                }
                va.binop(*op, &vb)
            }
            Expr::Call(f, args, span) => {
                let vals: Vec<AbsVal> = args.iter().map(|x| self.eval(x, unit, state, col)).collect();
                if vals.iter().any(|v| !v.numeric) {
                    return AbsVal::top(None);
                }
                let x = &vals[0];
                match f.as_str() {
                    "log" => {
                        if x.interval.lo <= 0.0 && x.sign.may_be_zero() | x.sign.may_be_negative() {
                            Self::warn(
                                col,
                                "A002",
                                *span,
                                format!("log of `{}` may be undefined: interval {}", render_expr(&args[0], &identity), x.interval.text()),
                                false,
                                self.stats.is_some(),
                            );
                            if let Some(c) = col {
                                c.may_undef = true;
                            }
                        }
                        let i = x.interval.log();
                        AbsVal {
                            may_undef: x.may_undef || x.interval.lo <= 0.0,
                            ..AbsVal::of_interval(Some(&Ty::scalar()), i)
                        }
                    }
                    "exp" => AbsVal {
                        sign: Sign::Pos,
                        may_undef: x.may_undef,
                        ..AbsVal::of_interval(Some(&Ty::scalar()), x.interval.exp())
                    }
                    .reduce(),
                    "sqrt" => {
                        if x.interval.lo < 0.0 && x.sign.may_be_negative() {
                            Self::warn(
                                col,
                                "A003",
                                *span,
                                format!("sqrt of `{}` may be undefined: interval {}", render_expr(&args[0], &identity), x.interval.text()),
                                false,
                                self.stats.is_some(),
                            );
                            if let Some(c) = col {
                                c.may_undef = true;
                            }
                        }
                        let ty = x.ty.as_deref().and_then(Ty::parse_text).and_then(|t| crate::check::types::call_type("sqrt", &[t]).ok());
                        AbsVal {
                            may_undef: x.may_undef || x.interval.lo < 0.0,
                            ..AbsVal::of_interval(ty.as_ref(), x.interval.sqrt())
                        }
                    }
                    "abs" => AbsVal {
                        may_undef: x.may_undef,
                        ..AbsVal::of_interval(x.ty.as_deref().and_then(Ty::parse_text).as_ref(), x.interval.abs())
                    },
                    "least" | "greatest" => {
                        let mut acc = x.clone();
                        for v in &vals[1..] {
                            let i = if f == "least" { acc.interval.least(v.interval) } else { acc.interval.greatest(v.interval) };
                            acc = AbsVal {
                                may_undef: acc.may_undef || v.may_undef,
                                ..AbsVal::of_interval(acc.ty.as_deref().or(v.ty.as_deref()).and_then(Ty::parse_text).as_ref(), i)
                            };
                        }
                        acc
                    }
                    _ => AbsVal::top(None),
                }
            }
        }
    }

    /// Backward refinement (3.7, comparisons): the values of the
    /// expression's variables narrowed to those that can make it lie in
    /// `target`, through the inverse interval operations.
    fn backward(&self, e: &Expr, target: Interval, unit: &str, state: &mut State) {
        if target.is_top() {
            return;
        }
        let mut none: Option<&mut Collect> = None;
        match e {
            Expr::Var(v, _) => {
                if let Some(cur) = state.get(v).cloned() {
                    if cur.numeric {
                        state.insert(v.clone(), cur.with_interval(cur.interval.meet(target)));
                    }
                }
            }
            Expr::Param(..) | Expr::Lit(..) => {}
            Expr::Neg(x, _) => self.backward(x, target.neg(), unit, state),
            Expr::Bin(op, a, b, _) => {
                let va = self.eval(a, unit, state, &mut none).interval;
                let vb = self.eval(b, unit, state, &mut none).interval;
                match op {
                    BinOp::Add => {
                        self.backward(a, target.sub(vb), unit, state);
                        self.backward(b, target.sub(va), unit, state);
                    }
                    BinOp::Sub => {
                        self.backward(a, target.add(vb), unit, state);
                        self.backward(b, va.sub(target), unit, state);
                    }
                    BinOp::Mul => {
                        if !vb.contains(0.0) {
                            self.backward(a, target.div(vb), unit, state);
                        }
                        if !va.contains(0.0) {
                            self.backward(b, target.div(va), unit, state);
                        }
                    }
                    BinOp::Div => {
                        self.backward(a, target.mul(vb), unit, state);
                        if !target.contains(0.0) {
                            self.backward(b, va.div(target), unit, state);
                        }
                    }
                }
            }
            Expr::Call(f, args, _) => match f.as_str() {
                "log" => self.backward(&args[0], target.exp(), unit, state),
                "exp" => self.backward(&args[0], target.meet(Interval::new(0.0, f64::INFINITY)).log(), unit, state),
                "sqrt" => {
                    let t = target.meet(Interval::new(0.0, f64::INFINITY));
                    self.backward(&args[0], t.mul(t), unit, state);
                }
                "abs" => {
                    let m = target.hi.abs().max(target.lo.abs());
                    self.backward(&args[0], Interval::new(-m, m), unit, state);
                }
                "least" => {
                    for x in args {
                        self.backward(x, Interval::new(target.lo, f64::INFINITY), unit, state);
                    }
                }
                "greatest" => {
                    for x in args {
                        self.backward(x, Interval::new(f64::NEG_INFINITY, target.hi), unit, state);
                    }
                }
                _ => {}
            },
        }
    }

    /// A positive atom (3.7): outputs receive the column values, inputs
    /// and bound outputs are met with them.
    fn atom(&self, a: &Atom, state: &mut State) {
        let Some(sig) = self.prog.relations.get(&a.name) else { return };
        let cols = self.columns.get(&a.name).cloned().unwrap_or_default();
        for (i, (t, arg)) in a.terms.iter().zip(sig.args.iter()).enumerate() {
            let cv = cols.get(i).cloned().unwrap_or_else(|| AbsVal::top(Some(&arg.ty)));
            match t {
                Term::Var(v, _) => {
                    let next = match state.get(v) {
                        Some(cur) if cur.numeric && cv.numeric => cur.meet(&cv),
                        Some(cur) => cur.clone(),
                        None => cv,
                    };
                    state.insert(v.clone(), next);
                }
                Term::Ctor(c, subs, _) => {
                    // A decision pattern: its entity and its amount.
                    for (k, s) in subs.iter().enumerate() {
                        if let Term::Var(v, _) = s {
                            if state.contains_key(v) {
                                continue;
                            }
                            let val = if k == 0 {
                                AbsVal::non_numeric(&Ty::Entity)
                            } else if crate::kernel::value::Ctor::parse(c).is_some_and(|k| !k.is_target()) {
                                AbsVal {
                                    sign: Sign::Pos,
                                    ..AbsVal::top(Some(&Ty::Quantity(Dim::shares())))
                                }
                                .reduce()
                            } else {
                                AbsVal::top(ctor_fields(c).and_then(|f| f.get(1).cloned()).as_ref())
                            };
                            state.insert(v.clone(), val);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The group size of an aggregation: `[K, w_max]` from the window
    /// lengths, with the minimum a count needs.
    fn group_size(&self, rule: usize, literal: usize, agg: &str, conj: &[Literal], unit: &str) -> Interval {
        let w = self.st.windows.iter().find(|w| w.rule == rule && w.literal == literal);
        let has_window = conj.iter().any(|l| matches!(l, Literal::Window { .. }));
        let k = w
            .and_then(|w| w.k)
            .unwrap_or(if agg == "count" || !has_window { 0.0 } else { 1.0 })
            .max(if agg == "count" { 0.0 } else { 1.0 });
        let _ = unit;
        let hi = w.and_then(|w| w.w_max).unwrap_or(f64::INFINITY);
        Interval::new(k.min(hi), hi)
    }

    /// The interval of an aggregate over argument values in `[l, u]` with
    /// `n` rows (3.7), widened by the summation error of the numerics
    /// setting (6.8).
    fn aggregate(&self, agg: &str, vals: &[AbsVal], n: Interval, col: &mut Option<&mut Collect>, span: Span, args: &[Expr]) -> AbsVal {
        let x = vals.first().cloned().unwrap_or_else(|| AbsVal::top(None));
        let xi = x.interval;
        let y = vals.get(1).cloned();
        let scalar = Ty::scalar();
        let span_err = |i: Interval, n: Interval| -> Interval {
            // The error of summing n terms of magnitude at most M: naive
            // n·ε·n·M (two-pass sums), Neumaier 2ε·n·M + n²ε²·M (running).
            if i.is_bottom() || !i.lo.is_finite() || !i.hi.is_finite() || !n.hi.is_finite() {
                return i;
            }
            let m = i.lo.abs().max(i.hi.abs());
            let err = if self.running {
                2.0 * EPS * n.hi * m + n.hi * n.hi * EPS * EPS * m
            } else {
                n.hi * EPS * n.hi * m
            };
            Interval::new(i.lo - err, i.hi + err)
        };
        let ty_of = |t: &Option<String>| t.as_deref().and_then(Ty::parse_text);
        match agg {
            "count" => AbsVal::of_interval(Some(&Ty::Count), Interval::new(n.lo.max(0.0), n.hi)),
            "sum" => AbsVal {
                may_undef: x.may_undef,
                ..AbsVal::of_interval(ty_of(&x.ty).as_ref(), span_err(n.mul(xi), n))
            },
            "mean" | "median" | "quantile" | "max" | "min" | "first" | "last" => {
                let i = if agg == "mean" { span_err(xi, n) } else { xi };
                let ty = match (agg, ty_of(&x.ty)) {
                    ("mean" | "median", Some(Ty::Count)) => Some(scalar.clone()),
                    (_, t) => t,
                };
                AbsVal {
                    may_undef: x.may_undef || (agg == "quantile" && !y.as_ref().map(|q| Interval::new(0.0, 1.0).meet(q.interval) == q.interval).unwrap_or(false)),
                    ..AbsVal::of_interval(ty.as_ref(), i)
                }
            }
            "std" => {
                let width = if xi.is_bottom() { 0.0 } else { (xi.hi - xi.lo) / 2.0 };
                let i = span_err(Interval::new(0.0, width.max(0.0)), n);
                let ty = match ty_of(&x.ty) {
                    Some(Ty::Count) => Some(scalar.clone()),
                    t => t,
                };
                AbsVal {
                    sign: Sign::NonNeg,
                    may_undef: x.may_undef || n.lo < 2.0,
                    ..AbsVal::of_interval(ty.as_ref(), i)
                }
                .reduce()
            }
            "cov" => {
                let wy = y.as_ref().map(|v| v.interval).unwrap_or(Interval::TOP);
                let c = if xi.is_bottom() || wy.is_bottom() { 0.0 } else { (xi.hi - xi.lo) * (wy.hi - wy.lo) / 4.0 };
                let ty = match (ty_of(&x.ty), y.as_ref().and_then(|v| ty_of(&v.ty))) {
                    (Some(a), Some(b)) => crate::check::types::bin_type(BinOp::Mul, &a, &b).ok(),
                    _ => None,
                };
                AbsVal {
                    may_undef: x.may_undef || n.lo < 2.0,
                    ..AbsVal::of_interval(ty.as_ref(), span_err(Interval::new(-c, c), n))
                }
            }
            "corr" => {
                // A point interval is a constant at every row: the aggregate
                // is undefined at every bar, a warning rather than a note.
                let constant = [Some(&x), y.as_ref()]
                    .into_iter()
                    .flatten()
                    .find(|v| v.numeric && v.interval.lo == v.interval.hi && v.interval.lo.is_finite());
                match constant {
                    Some(c) => Self::warn(
                        col,
                        "A008",
                        span,
                        format!(
                            "`corr` is undefined at every bar: an argument of `corr({}, {})` is the constant {} over every window; the run halts at the first full window",
                            render_expr(&args[0], &identity),
                            args.get(1).map(|a| render_expr(a, &identity)).unwrap_or_default(),
                            c.interval.lo
                        ),
                        false,
                        false,
                    ),
                    None => Self::warn(
                        col,
                        "A008",
                        span,
                        format!(
                            "`corr` is undefined when `{}` or `{}` is constant over the window",
                            render_expr(&args[0], &identity),
                            args.get(1).map(|a| render_expr(a, &identity)).unwrap_or_default()
                        ),
                        true,
                        false,
                    ),
                }
                AbsVal {
                    may_undef: true,
                    ..AbsVal::of_interval(Some(&scalar), Interval::new(-1.0, 1.0))
                }
            }
            "ols_slope" => {
                let regressor = y.as_ref().filter(|v| v.numeric && v.interval.lo == v.interval.hi && v.interval.lo.is_finite());
                match regressor {
                    Some(c) => Self::warn(
                        col,
                        "A008",
                        span,
                        format!(
                            "`ols_slope` is undefined at every bar: the regressor `{}` is the constant {} over every window; the run halts at the first full window",
                            args.get(1).map(|a| render_expr(a, &identity)).unwrap_or_default(),
                            c.interval.lo
                        ),
                        false,
                        false,
                    ),
                    None => Self::warn(
                        col,
                        "A008",
                        span,
                        format!(
                            "`ols_slope` is undefined when `{}` is constant over the window",
                            args.get(1).map(|a| render_expr(a, &identity)).unwrap_or_default()
                        ),
                        true,
                        false,
                    ),
                }
                let wy = y.as_ref().map(|v| v.interval).unwrap_or(Interval::TOP);
                let c = if xi.is_bottom() || wy.is_bottom() { 0.0 } else { (xi.hi - xi.lo) * (wy.hi - wy.lo) / 4.0 };
                let vx = if wy.is_bottom() { 0.0 } else { (wy.hi - wy.lo) * (wy.hi - wy.lo) / 4.0 };
                let ty = match (ty_of(&x.ty), y.as_ref().and_then(|v| ty_of(&v.ty))) {
                    (Some(a), Some(b)) => crate::check::types::bin_type(BinOp::Div, &a, &b).ok(),
                    _ => None,
                };
                AbsVal {
                    may_undef: true,
                    ..AbsVal::of_interval(ty.as_ref(), Interval::new(-c, c).div(Interval::new(0.0, vx)))
                }
            }
            _ => AbsVal::top(None),
        }
    }

    fn literal(&self, ri: usize, li: Option<usize>, lit: &Literal, unit: &str, state: &mut State, col: &mut Option<&mut Collect>) -> (Option<String>, Option<f64>) {
        let mut verdict = None;
        let mut selectivity = None;
        match lit {
            Literal::Atom(a) | Literal::AsOf { atom: a, .. } => {
                if let Literal::AsOf { atom, .. } = lit {
                    if let Some(k) = self.prog.relations.get(&atom.name).and_then(|s| s.key_pos()) {
                        if let Some(Term::Var(v, _)) = atom.terms.get(k) {
                            state.entry(v.clone()).or_insert_with(|| AbsVal::non_numeric(&Ty::Timestamp));
                        }
                    }
                }
                self.atom(a, state);
            }
            Literal::Neg(_) => {}
            Literal::Builtin(b, _) => {
                if let Builtin::Prev { t1, .. } | Builtin::Lag { t1, .. } = b {
                    if let Term::Var(v, _) = t1 {
                        state.entry(v.clone()).or_insert_with(|| AbsVal::non_numeric(&Ty::Timestamp));
                    }
                }
            }
            Literal::Window { var, .. } => {
                if let Term::Var(v, _) = var {
                    state.entry(v.clone()).or_insert_with(|| AbsVal::non_numeric(&Ty::Timestamp));
                }
            }
            Literal::Cmp { op, lhs, rhs, span } => {
                let a = self.eval(lhs, unit, state, col);
                let b = self.eval(rhs, unit, state, col);
                if a.numeric && b.numeric {
                    selectivity = Interval::selectivity(*op, a.interval, b.interval);
                    if Interval::never(*op, a.interval, b.interval) {
                        verdict = Some("never true".into());
                        let gap = Interval::gap(*op, a.interval, b.interval);
                        let within = gap.is_finite() && gap <= self.margin;
                        let evidence = format!("{} {} {}", a.interval.text(), op, b.interval.text());
                        if let Some(c) = col {
                            let ranged: Vec<String> = {
                                let mut ps = Vec::new();
                                lhs.params(&mut ps);
                                rhs.params(&mut ps);
                                ps.into_iter().filter(|p| self.prog.param(unit, p).map(|p| p.range.is_some()).unwrap_or(false)).collect()
                            };
                            let text = lit.describe();
                            c.never.push(NeverTrue {
                                rule: ri,
                                literal: li.unwrap_or(usize::MAX),
                                evidence: evidence.clone(),
                                gap,
                                within_margin: within,
                                bundle_relative: self.stats.is_some(),
                            });
                            if let Some(p) = ranged.first() {
                                let pr = self.prog.param(unit, p).unwrap();
                                let (lo, hi) = pr.range.as_ref().map(|(l, h)| (l.to_string(), h.to_string())).unwrap_or_default();
                                c.warnings.push(Warning {
                                    code: "A009".into(),
                                    rule: Some(c.label.clone()),
                                    span: span.to_string(),
                                    message: format!("guard `{}` is infeasible for every value of `{}` in [{}, {}]: {}", text, p, lo, hi, evidence),
                                    bundle_relative: self.stats.is_some(),
                                    note: false,
                                });
                            } else {
                                c.warnings.push(Warning {
                                    code: "A004".into(),
                                    rule: Some(c.label.clone()),
                                    span: span.to_string(),
                                    message: format!(
                                        "`{}` can never hold: {}; rule {} never fires{}",
                                        text,
                                        evidence,
                                        c.label,
                                        if within { " (within the rounding margin: kept)" } else { "" }
                                    ),
                                    bundle_relative: self.stats.is_some(),
                                    note: within,
                                });
                            }
                        }
                    } else if Interval::always(*op, a.interval, b.interval) {
                        verdict = Some("always true".into());
                        Self::warn(
                            col,
                            "A005",
                            *span,
                            format!("`{}` always holds and can be removed: {} {} {}", lit.describe(), a.interval.text(), op, b.interval.text()),
                            true,
                            self.stats.is_some(),
                        );
                    } else {
                        let (ra, rb) = Interval::refine(*op, a.interval, b.interval);
                        self.backward(lhs, ra, unit, state);
                        self.backward(rhs, rb, unit, state);
                    }
                }
            }
            Literal::Assign { var, expr, .. } => {
                let v = self.eval(expr, unit, state, col);
                match state.get(var).cloned() {
                    Some(cur) if cur.numeric && v.numeric => {
                        let m = cur.interval.meet(v.interval);
                        if m.is_bottom() {
                            verdict = Some("never true".into());
                        }
                        self.backward(expr, m, unit, state);
                        state.insert(var.clone(), cur.with_interval(m));
                    }
                    Some(_) => {}
                    None => {
                        state.insert(var.clone(), v);
                    }
                }
            }
            Literal::Agg { var, agg, args, conj, span, .. } => {
                let mut sub = state.clone();
                for c in conj {
                    self.literal(ri, None, c, unit, &mut sub, col);
                }
                let vals: Vec<AbsVal> = args.iter().map(|a| self.eval(a, unit, &sub, col)).collect();
                let n = self.group_size(ri, li.unwrap_or(usize::MAX), agg, conj, unit);
                let v = self.aggregate(agg, &vals, n, col, *span, args);
                if let Some(c) = col {
                    if v.may_undef {
                        c.may_undef = true;
                    }
                }
                state.insert(var.clone(), v);
            }
            Literal::Top { atom, rank, n, .. } => {
                self.atom(atom, state);
                if let Some((k, _, _)) = rank {
                    state.insert(
                        k.clone(),
                        AbsVal {
                            sign: Sign::Pos,
                            ..AbsVal::of_interval(Some(&Ty::scalar()), Interval::new(1.0, f64::INFINITY))
                        }
                        .reduce(),
                    );
                }
                if let Some(n) = n {
                    let _ = self.eval(n, unit, state, col);
                }
            }
            Literal::Case { var, branches, otherwise, .. } => {
                // The value is the join of the branch values (each evaluated
                // under its guards) and the otherwise value.
                let mut acc: Option<AbsVal> = None;
                for b in branches {
                    let mut sub = state.clone();
                    for g in &b.guard {
                        self.literal(ri, None, g, unit, &mut sub, col);
                    }
                    let v = self.eval(&b.value, unit, &sub, col);
                    acc = Some(match acc {
                        Some(a) => a.join(&v),
                        None => v,
                    });
                }
                if let Some(o) = otherwise {
                    let v = self.eval(o, unit, state, col);
                    acc = Some(match acc {
                        Some(a) => a.join(&v),
                        None => v,
                    });
                }
                let mut v = acc.unwrap_or_else(|| AbsVal::top(None));
                if otherwise.is_none() {
                    v.may_undef = true;
                }
                state.insert(var.clone(), v);
            }
        }
        (verdict, selectivity)
    }

    /// The transfer of a rule: the abstract values of its head terms, and
    /// what the final pass collects.
    fn transfer(&self, ri: usize, mut col: Option<&mut Collect>) -> Vec<AbsVal> {
        let rule = &self.prog.rules[ri];
        let unit = rule.unit.clone();
        let sig = self.prog.relations.get(&rule.head.name);
        let mut state: State = State::new();
        // Head inputs are bound by the caller: their values are the column
        // values, narrowed by the demand set when it is static.
        if let Some(sig) = sig {
            let cols = self.columns.get(&rule.head.name).cloned().unwrap_or_default();
            let _ = &cols;
            for (t, arg) in rule.head.terms.iter().zip(sig.args.iter()) {
                if let Term::Var(v, _) = t {
                    match arg.mode {
                        Mode::Key => {
                            state.insert(v.clone(), AbsVal::non_numeric(&Ty::Timestamp));
                        }
                        Mode::Out => {}
                    }
                }
            }
        }
        for (li, lit) in rule.body.iter().enumerate() {
            let (verdict, selectivity) = self.literal(ri, Some(li), lit, &unit, &mut state, &mut col);
            if let Some(c) = col.as_deref_mut() {
                c.literals.push(LiteralFacts {
                    index: li,
                    text: lit.describe(),
                    state: state.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                    verdict,
                    selectivity,
                });
            }
        }
        // The head.
        let mut out = Vec::with_capacity(rule.head.terms.len());
        for (i, t) in rule.head.terms.iter().enumerate() {
            let ty = sig.and_then(|s| s.args.get(i)).map(|a| a.ty.clone());
            let v = match t {
                Term::Var(v, _) => state.get(v).cloned().unwrap_or_else(|| {
                    ty.as_ref()
                        .map(|t| if numeric_ty(t) { AbsVal::top(Some(t)) } else { AbsVal::non_numeric(t) })
                        .unwrap_or_else(|| AbsVal::top(None))
                }),
                Term::Param(p, _) => self.param_val(&unit, p),
                Term::Lit(l, _) => self.lit_val(l),
                Term::Wild(_) => AbsVal::top(ty.as_ref()),
                Term::Ctor(c, subs, span) => {
                    if crate::kernel::value::Ctor::parse(c).is_some_and(|k| !k.is_target()) {
                        if let Some(amount) = subs.get(1) {
                            let q = match amount {
                                Term::Var(v, _) => state.get(v).cloned(),
                                Term::Param(p, _) => Some(self.param_val(&unit, p)),
                                Term::Lit(l, _) => Some(self.lit_val(l)),
                                _ => None,
                            };
                            if let Some(q) = q {
                                if q.numeric && (q.sign.may_be_negative() || q.sign.may_be_zero()) {
                                    Self::warn(
                                        &mut col,
                                        "A011",
                                        *span,
                                        format!(
                                            "`{}` may carry a non-positive quantity: {} (a delta decision needs a positive one; the kernel halts on it)",
                                            t,
                                            q.interval.text()
                                        ),
                                        false,
                                        self.stats.is_some(),
                                    );
                                }
                            }
                        }
                    }
                    AbsVal::non_numeric(&Ty::Decision)
                }
            };
            out.push(v);
        }
        out
    }

    /// Widening by thresholds (3.9): a bound that moved goes to the nearest
    /// threshold beyond it, or to ±∞.
    fn widen(&self, old: &AbsVal, new: &AbsVal) -> AbsVal {
        if !new.numeric || old.sign == Sign::Bot {
            return new.clone();
        }
        let mut i = new.interval;
        if i.lo < old.interval.lo {
            i.lo = self.thresholds.iter().copied().filter(|t| *t <= i.lo).fold(f64::NEG_INFINITY, f64::max);
        }
        if i.hi > old.interval.hi {
            i.hi = self.thresholds.iter().copied().filter(|t| *t >= i.hi).fold(f64::INFINITY, f64::min);
        }
        AbsVal {
            interval: i,
            sign: new.sign.join(old.sign),
            ..new.clone()
        }
        .reduce()
    }
}

/// The thresholds of widening: the program's numeric literals, the
/// parameters' defaults and range bounds, zero and one.
fn thresholds(prog: &Program) -> Vec<f64> {
    let mut ts: Vec<f64> = vec![0.0, 1.0, -1.0];
    for r in &prog.rules {
        for (l, _) in crate::check::dof::literals_in_rule(r) {
            if let Some(x) = magnitude(&l) {
                ts.push(x);
            }
        }
    }
    for ps in prog.params.values() {
        for p in ps.values() {
            if let Some(x) = magnitude(&p.value) {
                ts.push(x);
            }
            if let Some((lo, hi)) = &p.range {
                ts.extend(magnitude(lo));
                ts.extend(magnitude(hi));
            }
        }
    }
    ts.retain(|x| x.is_finite());
    ts.sort_by(|a, b| a.total_cmp(b));
    ts.dedup();
    ts
}

/// Run the abstract interpretation (3.9) and assemble the analysis.
pub fn interpret(prog: &Program, st: Structure, stats: Option<&Statistics>, margin_log2: i32) -> Analysis {
    let mut columns: BTreeMap<String, Vec<AbsVal>> = BTreeMap::new();
    let futures = prog.class == "future";
    let interp_seed = Interp {
        prog,
        st: &st,
        stats,
        thresholds: thresholds(prog),
        columns: BTreeMap::new(),
        margin: 2f64.powi(margin_log2),
        futures,
        running: true,
    };
    for (name, sig) in &prog.relations {
        let v = if matches!(sig.kind, Kind::Derived { .. } | Kind::Output) {
            vec![AbsVal::bottom(); sig.args.len()]
        } else {
            interp_seed.stored_columns(sig)
        };
        columns.insert(name.clone(), v);
    }
    let mut interp = Interp { columns, ..interp_seed };
    let mut widened: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for stratum in &prog.strata {
        let rules: Vec<usize> = stratum.iter().flat_map(|n| prog.rules_for(n)).collect();
        if rules.is_empty() {
            continue;
        }
        let recursive = stratum.iter().any(|n| st.recursive.contains(n));
        let mut iteration = 0;
        loop {
            let before: BTreeMap<String, Vec<AbsVal>> = stratum.iter().filter_map(|n| interp.columns.get(n).map(|c| (n.clone(), c.clone()))).collect();
            let mut joined: BTreeMap<String, Vec<AbsVal>> = before.clone();
            let mut changed = false;
            for &ri in &rules {
                let head = interp.transfer(ri, None);
                let name = &prog.rules[ri].head.name;
                let cols = joined.get_mut(name).unwrap();
                for (i, v) in head.iter().enumerate() {
                    let j = cols[i].join(v);
                    if j != cols[i] {
                        cols[i] = j;
                        changed = true;
                    }
                }
            }
            iteration += 1;
            if recursive && iteration >= 3 {
                for (name, cols) in joined.iter_mut() {
                    let old = &before[name];
                    for (i, c) in cols.iter_mut().enumerate() {
                        let w = interp.widen(&old[i], c);
                        if w != *c {
                            *c = w;
                        }
                    }
                }
            }
            for (name, cols) in joined {
                interp.columns.insert(name, cols);
            }
            if !changed || iteration >= 12 {
                break;
            }
        }
        if recursive {
            // One narrowing pass: the widened state met with its own image.
            let mut image: BTreeMap<String, Vec<AbsVal>> = stratum.iter().filter_map(|n| interp.columns.get(n).map(|c| (n.clone(), vec![AbsVal::bottom(); c.len()]))).collect();
            for &ri in &rules {
                let head = interp.transfer(ri, None);
                let name = &prog.rules[ri].head.name;
                let cols = image.get_mut(name).unwrap();
                for (i, v) in head.iter().enumerate() {
                    cols[i] = cols[i].join(v);
                }
            }
            for (name, img) in image {
                let cur = interp.columns.get_mut(&name).unwrap();
                for (i, v) in img.iter().enumerate() {
                    if v.numeric && cur[i].numeric && v.sign != Sign::Bot {
                        cur[i] = cur[i].meet(v);
                    }
                }
            }
            for n in stratum {
                if let Some(cols) = interp.columns.get(n) {
                    let sig = &prog.relations[n];
                    let names: Vec<String> = cols
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.numeric && (c.interval.lo == f64::NEG_INFINITY || c.interval.hi == f64::INFINITY))
                        .map(|(i, _)| sig.args[i].name.clone())
                        .collect();
                    if !names.is_empty() && matches!(sig.kind, Kind::Derived { .. }) {
                        widened.insert(n.clone(), names);
                    }
                }
            }
        }
    }
    // The final pass: facts and warnings per rule.
    let mut rules_facts = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();
    let mut never = Vec::new();
    for ri in 0..prog.rules.len() {
        let mut c = Collect {
            rule: ri,
            label: prog.rule_label(ri),
            warnings: Vec::new(),
            literals: Vec::new(),
            never: Vec::new(),
            may_undef: false,
        };
        interp.transfer(ri, Some(&mut c));
        warnings.extend(c.warnings);
        never.extend(c.never);
        rules_facts.push(RuleFacts {
            rule: c.rule,
            label: c.label,
            literals: c.literals,
            may_undef: c.may_undef,
        });
    }
    // Window warnings A006 and A007.
    for w in &st.windows {
        let label = prog.rule_label(w.rule);
        let span = prog.rules[w.rule].body.get(w.literal).map(|l| l.span()).unwrap_or_default();
        if let (Some(k), Some(m)) = (w.k, w.w_max) {
            if k > m {
                warnings.push(Warning {
                    code: "A006".into(),
                    rule: Some(label.clone()),
                    span: span.to_string(),
                    message: format!("window {} holds at most {} bars; min {} can never be met", w.kind, m, k),
                    bundle_relative: false,
                    note: false,
                });
            }
        }
        if let (Some(k), Some(warm)) = (w.k, w.warmup) {
            if k > warm {
                warnings.push(Warning {
                    code: "A007".into(),
                    rule: Some(label.clone()),
                    span: span.to_string(),
                    message: format!("window {} cannot fill before the first decision bar: min {} rows, {} bars of warm-up", w.kind, k, warm),
                    bundle_relative: true,
                    note: false,
                });
            }
        }
    }
    // A010: widened recursive columns.
    for (name, cols) in &widened {
        for c in cols {
            warnings.push(Warning {
                code: "A010".into(),
                rule: None,
                span: prog.relations[name].span.to_string(),
                message: format!("no finite bound for {}.{}; the CHC tier may refine it", name, c),
                bundle_relative: false,
                note: true,
            });
        }
    }
    // One warning per distinct text.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    warnings.retain(|w| seen.insert(w.to_string()));
    let stratum_of = |name: &str| prog.strata.iter().position(|s| s.iter().any(|r| r == name));
    let relations: Vec<RelationFacts> = prog
        .relations
        .iter()
        .map(|(name, sig)| {
            let cols = interp.columns.get(name).cloned().unwrap_or_default();
            let (keyedness, note) = match st.keyedness.get(name) {
                Some((k, n)) => (*k, n.clone()),
                None => (Keyedness::NotRequired, String::new()),
            };
            RelationFacts {
                name: name.clone(),
                kind: crate::compiler::print::kind_name(&sig.kind).into(),
                calendar: prog.cal_name(&sig.cal),
                stratum: stratum_of(name),
                component: st.components.get(name).copied().unwrap_or(0),
                recursive: st.recursive.contains(name),
                keyedness,
                keyedness_note: note,
                closed_loop: st.closed_loop.contains(name),
                demanded: st.demanded.contains(name),
                demand: st.demand.get(name).cloned().unwrap_or(DemandSet::Total),
                columns: sig
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| ColumnFacts {
                        name: a.name.clone(),
                        ty: a.ty.to_string(),
                        value: cols.get(i).cloned().unwrap_or_else(|| AbsVal::top(Some(&a.ty))),
                    })
                    .collect(),
                widened: widened.get(name).cloned().unwrap_or_default(),
            }
        })
        .collect();
    Analysis {
        relations,
        rules: rules_facts,
        windows: st.windows.clone(),
        demanded: st.demanded.clone(),
        never_true: never,
        warnings,
        thresholds: interp.thresholds.clone(),
        statistics: stats.map(|s| s.source.clone()).unwrap_or_else(|| "defaults".into()),
        bundle_relative: stats.is_some(),
    }
}
