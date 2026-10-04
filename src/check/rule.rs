//! Per-rule analysis: one left-to-right pass over the body in the order
//! written (section 1, "literal order"), judging U, E, B, M, T, F, D, X and
//! the per-rule part of C, and recording what the program-level judgments
//! (R, N, S, W1, W2) need.

use std::collections::{HashMap, HashSet};

use super::types::*;
use super::{AtomRef, Checker, Code, Polarity, RuleInfo, TimeProv};
use crate::ir::*;

#[derive(Clone, Debug)]
struct VarState {
    ty: Option<Ty>,
    prov: TimeProv,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtomCtx {
    Positive,
    Negative,
    InAgg,
    Top,
    ResampleInner,
}

struct AtomInfo {
    sig: Signature,
    fresh: Vec<String>,
}

struct Analyzer<'c, 'a> {
    cx: &'c mut Checker<'a>,
    rule: &'c Rule,
    label: String,
    res: Resolution,
    head_time: Option<String>,
    vars: HashMap<String, VarState>,
    refs: Vec<AtomRef>,
    params_used: Vec<String>,
    key_vars_positive: HashSet<String>,
    /// Inside an aggregation: window variables declared later in the same
    /// conjunction, with the provenance they will have. An atom may bind such
    /// a variable in its key position before the window literal is reached;
    /// the window then acts as the constraint (the conjunction is commutative).
    pending_windows: HashMap<String, TimeProv>,
}

/// For a temporal builtin written where a relation is expected: how to get a
/// relation that holds exactly when the builtin does.
fn builtin_as_relation_hint(name: &str) -> Option<String> {
    Some(match name {
        "month_start" | "day_start" => {
            let rel = if name == "month_start" { "mstart" } else { "dstart" };
            format!("define `rel {rel}(@T: Timestamp)` with `{rel}(T) :- bar(T), {name}(T).` (a derived relation over the builtin is complete) and write `not {rel}(T)`")
        }
        "prev" | "lag" => {
            let args = if name == "prev" { "T, T0" } else { "T, N, T0" };
            format!("`{name}` binds its last argument; define `rel has_{name}(@T: Timestamp)` with `has_{name}(T) :- bar(T), {name}({args}).` and write `not has_{name}(T)`")
        }
        _ => return None,
    })
}

pub(crate) fn analyze(cx: &mut Checker, idx: usize, rule: &Rule) -> Option<RuleInfo> {
    let label = super::rule_label(&cx.rules, idx);
    let unit_res = cx.unit_res.get(&rule.unit).copied().unwrap_or(cx.resolution);
    let mut an = Analyzer {
        cx,
        rule,
        label,
        res: unit_res,
        head_time: None,
        vars: HashMap::new(),
        refs: vec![],
        params_used: vec![],
        key_vars_positive: HashSet::new(),
        pending_windows: HashMap::new(),
    };
    let head_sig = an.cx.relations.get(&rule.head.name).cloned();
    if let Some(sig) = &head_sig {
        an.res = sig.res.unwrap_or(unit_res);
        // WF-2: the caller binds every `+` argument, so a head input is bound
        // on entry to the body; the key and the outputs must be bound by the body.
        for (term, arg) in rule.head.terms.iter().zip(sig.args.iter()) {
            if arg.mode == Mode::In {
                if let Term::Var(v, _) = term {
                    an.bind(v, Some(arg.ty.clone()), TimeProv::Other);
                }
            }
        }
        if let Some(k) = sig.key_pos() {
            match rule.head.terms.get(k) {
                Some(Term::Var(v, _)) => an.head_time = Some(v.clone()),
                Some(t) => an.err(Code::F, t.span(), format!("the head's temporal key must be a variable, found `{}`", t)),
                None => {}
            }
        }
    }
    for lit in &rule.body {
        an.literal(lit, false);
    }
    an.head();
    head_sig.map(|_| RuleInfo {
        head: rule.head.name.clone(),
        refs: an.refs,
        params_used: an.params_used,
        resolution: an.res,
    })
}

impl<'c, 'a> Analyzer<'c, 'a> {
    fn err(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        let unit = self.rule.unit.clone();
        let label = self.label.clone();
        self.cx.diag(code, &unit, Some(label), span, msg);
    }

    fn is_bound(&self, v: &str) -> bool {
        self.vars.contains_key(v)
    }

    fn bind(&mut self, v: &str, ty: Option<Ty>, prov: TimeProv) {
        let ty = match ty {
            Some(Ty::IntLit) => Some(Ty::scalar()),
            t => t,
        };
        self.vars.insert(v.to_string(), VarState { ty, prov });
    }

    fn prov_of(&self, v: &str) -> TimeProv {
        self.vars.get(v).map(|s| s.prov).unwrap_or(TimeProv::Other)
    }

    fn derived_prov(base: TimeProv, strict: bool) -> TimeProv {
        match base {
            TimeProv::Other => TimeProv::Other,
            TimeProv::Unknown => TimeProv::Unknown,
            TimeProv::Strict => TimeProv::Strict,
            TimeProv::Head | TimeProv::Causal => {
                if strict {
                    TimeProv::Strict
                } else {
                    TimeProv::Causal
                }
            }
        }
    }

    fn param_ty(&mut self, name: &str, span: Span) -> Option<Ty> {
        let unit = self.rule.unit.clone();
        match self.cx.params.get(&unit).and_then(|m| m.get(name)) {
            Some(p) => {
                self.params_used.push(name.to_string());
                Some(p.ty.clone())
            }
            None => {
                self.err(
                    Code::U,
                    span,
                    format!("`{}` is neither a parameter of `{}` nor a variable (variables start with an uppercase letter)", name, unit),
                );
                None
            }
        }
    }

    /// Name resolution (U) and environment membership (E).
    ///
    /// Returns `None` without a diagnostic when the root cause was already
    /// reported at unit level: the environment or a used library is missing
    /// from the workspace (the name may live there), or the name is a
    /// primitive of a used library's own, mismatched environment.
    fn resolve_rel(&mut self, name: &str, span: Span) -> Option<Signature> {
        if let Some(sig) = self.cx.relations.get(name) {
            return Some(sig.clone());
        }
        // A temporal builtin (section 4) is a constraint or a binder, not a
        // relation: it has no tuples to negate or reduce. The parser accepts
        // it as an atom only after `not` or inside `top`, so the idiom shown
        // is a derived relation over the builtin, which is complete (WF-5).
        if let Some(hint) = builtin_as_relation_hint(name) {
            self.err(Code::U, span, format!("`{}` is a temporal builtin, not a relation, so it cannot be negated; {}", name, hint));
            return None;
        }
        if self.cx.scope_incomplete {
            return None;
        }
        let declared_env = self.cx.env_name.clone();
        for env in self.cx.ws.units.iter().filter(|u| u.kind == UnitKind::Environment) {
            if env.rels.iter().any(|s| s.name == name) {
                if self.cx.foreign_envs.contains(&env.name) {
                    return None;
                }
                self.err(
                    Code::E,
                    span,
                    format!("`{}` is a primitive of environment `{}`, which is not the declared environment `{}`", name, env.name, declared_env),
                );
                return None;
            }
        }
        for lib in self.cx.ws.units.iter().filter(|u| u.kind == UnitKind::Library) {
            if lib.rels.iter().any(|s| s.name == name) && !self.cx.units.iter().any(|u| u.name == lib.name) {
                self.err(
                    Code::U,
                    span,
                    format!("`{}` is declared in library `{}`, which `{}` does not use; add `uses {}`", name, lib.name, self.cx.root.name, lib.name),
                );
                return None;
            }
        }
        let libs: Vec<String> = self.cx.units.iter().filter(|u| u.kind == UnitKind::Library).map(|u| u.name.clone()).collect();
        let where_ = if libs.is_empty() { String::new() } else { format!(", its libraries ({})", libs.join(", ")) };
        self.err(
            Code::U,
            span,
            format!("relation `{}` is not declared in `{}`{} or environment `{}`", name, self.cx.root.name, where_, declared_env),
        );
        None
    }

    fn check_lit_type(&mut self, expected: &Ty, lit: &Lit, span: Span, what: &str) {
        let actual = lit.ty();
        if !compat(expected, &actual) {
            self.err(Code::T, span, format!("{} expects {} but literal `{}` is {}", what, expected, lit, actual));
        }
    }

    fn atom(&mut self, atom: &Atom, ctx: AtomCtx) -> Option<AtomInfo> {
        let Some(sig) = self.resolve_rel(&atom.name, atom.span) else {
            // Bind the atom's fresh variables with unknown types and unknown
            // time provenance, so that neither WF-3 nor WF-6 is judged on
            // what this atom would have bound; parameters passed to it still
            // count as used (W2).
            for t in &atom.terms {
                match t {
                    Term::Var(v, _) if !self.is_bound(v) => self.bind(v, None, TimeProv::Unknown),
                    Term::Param(p, sp) => {
                        self.param_ty(p, *sp);
                    }
                    _ => {}
                }
            }
            return None;
        };
        if atom.terms.len() != sig.args.len() {
            self.err(Code::T, atom.span, format!("`{}` takes {} arguments, {} given", atom.name, sig.args.len(), atom.terms.len()));
            return None;
        }
        let mut fresh = Vec::new();
        let mut key_prov = TimeProv::Other;
        let head_time = self.head_time.clone();
        for (term, arg) in atom.terms.iter().zip(sig.args.iter()) {
            let is_key = arg.mode == Mode::Key;
            match term {
                Term::Var(v, sp) => {
                    if let Some(st) = self.vars.get(v).cloned() {
                        if let Some(t) = &st.ty {
                            if !compat(&arg.ty, t) {
                                self.err(Code::T, *sp, format!("argument `{}` of `{}` is {} but `{}` is {}", arg.name, atom.name, arg.ty, v, t));
                            }
                        }
                        if is_key {
                            key_prov = st.prov;
                        }
                    } else {
                        if ctx == AtomCtx::Negative {
                            self.err(
                                Code::B,
                                *sp,
                                format!(
                                    "variable `{}` is unbound in negated atom `not {}`; every variable of a negated atom must be bound before it",
                                    v, atom.name
                                ),
                            );
                        } else if arg.mode == Mode::In {
                            self.err(
                                Code::M,
                                *sp,
                                format!("`+{}` of `{}` is an input and must be bound before the call, but `{}` is unbound here", arg.name, atom.name, v),
                            );
                        }
                        let prov = if is_key {
                            if head_time.as_deref() == Some(v.as_str()) && ctx != AtomCtx::ResampleInner {
                                TimeProv::Head
                            } else if let Some(p) = self.pending_windows.get(v) {
                                *p
                            } else {
                                TimeProv::Other
                            }
                        } else {
                            TimeProv::Other
                        };
                        self.bind(v, Some(arg.ty.clone()), prov);
                        fresh.push(v.clone());
                        if is_key {
                            key_prov = prov;
                        }
                    }
                }
                Term::Wild(sp) => {
                    if arg.mode == Mode::In {
                        self.err(Code::M, *sp, format!("`_` is permitted only in `-` positions; `+{}` of `{}` is an input", arg.name, atom.name));
                    } else if is_key && ctx != AtomCtx::ResampleInner {
                        self.err(
                            Code::F,
                            *sp,
                            format!("the temporal key of `{}` may not be a wildcard; bind it to the head time or a causal builtin of it", atom.name),
                        );
                    }
                    if ctx == AtomCtx::ResampleInner && is_key {
                        self.err(Code::X, *sp, "the inner temporal key of a resample must be a fresh variable, not `_`");
                    }
                }
                Term::Param(p, sp) => {
                    if let Some(pt) = self.param_ty(p, *sp) {
                        if !compat(&arg.ty, &pt) {
                            self.err(Code::T, *sp, format!("argument `{}` of `{}` is {} but parameter `{}` is {}", arg.name, atom.name, arg.ty, p, pt));
                        }
                    }
                    if is_key {
                        self.err(Code::F, *sp, format!("the temporal key of `{}` must be a variable derived from the head time", atom.name));
                    }
                }
                Term::Lit(l, sp) => {
                    self.check_lit_type(&arg.ty, l, *sp, &format!("argument `{}` of `{}`", arg.name, atom.name));
                    if is_key {
                        self.err(Code::F, *sp, format!("the temporal key of `{}` must be a variable derived from the head time", atom.name));
                    }
                }
                Term::Ctor(c, subs, sp) => {
                    if arg.ty != Ty::Decision {
                        self.err(Code::T, *sp, format!("argument `{}` of `{}` is {}, not a decision", arg.name, atom.name, arg.ty));
                        continue;
                    }
                    self.ctor(c, subs, *sp, ctx == AtomCtx::Negative || arg.mode == Mode::In, arg.mode == Mode::In);
                }
            }
        }
        if ctx != AtomCtx::ResampleInner {
            if let Some(k) = sig.key_pos() {
                let kterm = &atom.terms[k];
                if let Term::Var(v, sp) = kterm {
                    let head_t = self.head_time.clone().unwrap_or_default();
                    match key_prov {
                        // The head time itself, bound somewhere other than a
                        // key position: `head()` reports that once, so no
                        // "`T` is not derived from `T`" here.
                        TimeProv::Other if *v == head_t => {}
                        TimeProv::Unknown => {}
                        TimeProv::Other => self.err(
                            Code::F,
                            *sp,
                            format!(
                                "temporal key `{}` of `{}` is not derived from the head time `{}`; bind it first with prev, lag, window or prior_window of `{}`",
                                v, atom.name, head_t, head_t
                            ),
                        ),
                        TimeProv::Head | TimeProv::Causal if atom.name == "decided" => self.err(
                            Code::F,
                            *sp,
                            format!(
                                "`decided` must be read strictly before the head time; `decided({}, ...)` at the rule's own `{}` would let a decision see itself (use prev/lag/prior_window)",
                                v, head_t
                            ),
                        ),
                        _ => {}
                    }
                    if matches!(ctx, AtomCtx::Positive | AtomCtx::InAgg) {
                        self.key_vars_positive.insert(v.clone());
                    }
                }
            }
            if let Some(r) = sig.res {
                if r != self.res {
                    let head_name = self.rule.head.name.clone();
                    let kernel_supplied = matches!(sig.kind, Kind::Executor | Kind::KernelState | Kind::Output);
                    let root = self.cx.root;
                    let msg = if kernel_supplied && self.rule.unit != root.name {
                        // An executor relation sits at the decision resolution of
                        // the strategy being checked (section 6), which a library
                        // rule cannot see from its own text.
                        format!(
                            "`{}` is at {}, the decision resolution of {} `{}`, but this rule (head `{}`) is at {}; library `{}` ({}) can only read executor relations from a {} that decides at {}",
                            atom.name, r, root.kind, root.name, head_name, self.res, self.rule.unit, self.res, root.kind, self.res
                        )
                    } else if kernel_supplied {
                        format!(
                            "`{}` is at {}, the decision resolution of {} `{}`, but this rule (head `{}`) is at {}; executor relations are read at the decision resolution only",
                            atom.name, r, root.kind, root.name, head_name, self.res
                        )
                    } else {
                        format!(
                            "`{}` is at {} but this rule (head `{}`) is at {}; two resolutions meet only through resample",
                            atom.name, r, head_name, self.res
                        )
                    };
                    self.err(Code::X, atom.span, msg);
                }
            }
        }
        let polarity = match ctx {
            AtomCtx::Positive => Polarity::Positive,
            AtomCtx::Negative => Polarity::Negative,
            AtomCtx::InAgg | AtomCtx::Top | AtomCtx::ResampleInner => Polarity::Aggregate,
        };
        self.refs.push(AtomRef {
            name: atom.name.clone(),
            polarity,
            key_prov,
            span: atom.span,
            is_top: ctx == AtomCtx::Top,
        });
        Some(AtomInfo { sig, fresh })
    }

    /// A decision constructor term, in a head, a `decided` pattern or an atom.
    /// `must_be_bound`: variables may not be fresh (head, input position,
    /// negation). `no_wild`: `_` is not a pattern here (head, input position).
    fn ctor(&mut self, name: &str, subs: &[Term], span: Span, must_be_bound: bool, no_wild: bool) {
        let Some(fields) = ctor_fields(name) else {
            self.err(
                Code::C,
                span,
                format!("`{}` is not a decision constructor (buy, sell, short, cover, target_weight, target_quantity)", name),
            );
            return;
        };
        if subs.len() != fields.len() {
            self.err(Code::T, span, format!("`{}` takes {} fields, {} given", name, fields.len(), subs.len()));
            return;
        }
        for (sub, fty) in subs.iter().zip(fields.iter()) {
            match sub {
                Term::Var(v, sp) => {
                    if let Some(st) = self.vars.get(v).cloned() {
                        if let Some(t) = &st.ty {
                            if !compat(fty, t) {
                                self.err(Code::T, *sp, format!("field of `{}` is {} but `{}` is {}", name, fty, v, t));
                            }
                        }
                    } else if must_be_bound {
                        self.err(Code::B, *sp, format!("variable `{}` is unbound in `{}(...)`", v, name));
                        self.bind(v, Some(fty.clone()), TimeProv::Other);
                    } else {
                        self.bind(v, Some(fty.clone()), TimeProv::Other);
                    }
                }
                Term::Wild(sp) => {
                    if no_wild {
                        self.err(Code::B, *sp, format!("`_` is not permitted in a decision value here; every field of `{}` must be bound", name));
                    }
                }
                Term::Param(p, sp) => {
                    if let Some(pt) = self.param_ty(p, *sp) {
                        if !compat(fty, &pt) {
                            self.err(Code::T, *sp, format!("field of `{}` is {} but parameter `{}` is {}", name, fty, p, pt));
                        }
                    }
                }
                Term::Lit(l, sp) => self.check_lit_type(fty, l, *sp, &format!("field of `{}`", name)),
                Term::Ctor(_, _, sp) => self.err(Code::T, *sp, "decision constructors do not nest"),
            }
        }
    }

    fn expr_ty(&mut self, e: &Expr) -> Option<Ty> {
        match e {
            Expr::Var(v, sp) => match self.vars.get(v) {
                Some(st) => st.ty.clone(),
                None => {
                    self.err(
                        Code::B,
                        *sp,
                        format!(
                            "variable `{}` is unbound at this point; body literals are read left to right, so bind it with a positive atom, an assignment or a temporal builtin before use",
                            v
                        ),
                    );
                    None
                }
            },
            Expr::Param(p, sp) => self.param_ty(p, *sp),
            Expr::Lit(l, _) => Some(l.ty()),
            Expr::Neg(inner, sp) => {
                let t = self.expr_ty(inner)?;
                match neg_type(&t) {
                    Ok(t) => Some(t),
                    Err(m) => {
                        self.err(Code::T, *sp, m);
                        None
                    }
                }
            }
            Expr::Bin(op, a, b, sp) => {
                let ta = self.expr_ty(a);
                let tb = self.expr_ty(b);
                let (ta, tb) = (ta?, tb?);
                match bin_type(*op, &ta, &tb) {
                    Ok(t) => Some(t),
                    Err(m) => {
                        self.err(Code::T, *sp, format!("in `{}`: {}", e, m));
                        None
                    }
                }
            }
            Expr::Call(f, args, sp) => {
                let mut tys = Vec::new();
                let mut ok = true;
                for a in args {
                    match self.expr_ty(a) {
                        Some(t) => tys.push(t),
                        None => ok = false,
                    }
                }
                if !ok {
                    return None;
                }
                match call_type(f, &tys) {
                    Ok(t) => Some(t),
                    Err(m) => {
                        self.err(Code::T, *sp, format!("in `{}`: {}", e, m));
                        None
                    }
                }
            }
        }
    }

    fn expect_expr(&mut self, e: &Expr, pred: impl Fn(&Ty) -> bool, what: &str) {
        if let Some(t) = self.expr_ty(e) {
            if !pred(&t) {
                self.err(Code::T, e.span(), format!("{} must be {}, found {}", e, what, t));
            }
        }
    }

    fn time_var_bound(&mut self, t: &Term, what: &str) -> Option<String> {
        match t {
            Term::Var(v, sp) => match self.vars.get(v).cloned() {
                Some(st) => {
                    if let Some(ty) = st.ty {
                        if ty != Ty::Timestamp {
                            self.err(Code::T, *sp, format!("{} must be a Timestamp, but `{}` is {}", what, v, ty));
                            return None;
                        }
                    }
                    Some(v.clone())
                }
                None => {
                    self.err(Code::B, *sp, format!("variable `{}` is unbound at this point; {} must already be bound", v, what));
                    None
                }
            },
            t => {
                self.err(Code::T, t.span(), format!("{} must be a Timestamp variable, found `{}`", what, t));
                None
            }
        }
    }

    fn time_var_fresh(&mut self, t: &Term, what: &str) -> Option<String> {
        match t {
            Term::Var(v, sp) => {
                if self.is_bound(v) {
                    self.err(Code::B, *sp, format!("`{}` is already bound; {} binds a fresh variable", v, what));
                    return None;
                }
                Some(v.clone())
            }
            t => {
                self.err(Code::T, t.span(), format!("{} is bound by the builtin and must be a fresh variable, found `{}`", what, t));
                None
            }
        }
    }

    fn literal(&mut self, lit: &Literal, in_agg: bool) {
        match lit {
            Literal::Atom(a) => {
                self.atom(a, if in_agg { AtomCtx::InAgg } else { AtomCtx::Positive });
            }
            Literal::Neg(a) => {
                if in_agg {
                    self.err(
                        Code::B,
                        a.span,
                        "an aggregation's group is a conjunction of positive atoms and temporal constraints; negation is not permitted inside it",
                    );
                }
                self.atom(a, AtomCtx::Negative);
            }
            Literal::Builtin(b, span) => match b {
                // The last argument of prev and lag is an output (section 4,
                // "Binds"): a fresh variable, or `_` when only the existence
                // of the earlier bar matters (WF-2 permits `_` in `-` positions).
                Builtin::Prev { t, t1 } => {
                    let base = self.time_var_bound(t, "the first argument of prev");
                    if matches!(t1, Term::Wild(_)) {
                        return;
                    }
                    if let Some(v) = self.time_var_fresh(t1, "the second argument of prev") {
                        let prov = base.map(|b| Self::derived_prov(self.prov_of(&b), true)).unwrap_or(TimeProv::Other);
                        self.bind(&v, Some(Ty::Timestamp), prov);
                    }
                    let _ = span;
                }
                Builtin::Lag { t, n, t1 } => {
                    let base = self.time_var_bound(t, "the first argument of lag");
                    self.expect_expr(n, |t| *t == Ty::Duration, "a Duration");
                    // lag by a zero duration lands on T itself: causal, not strict.
                    let zero = match n {
                        Expr::Lit(Lit::Duration(d), _) => d.is_zero(),
                        Expr::Param(p, _) => {
                            let unit = self.rule.unit.clone();
                            matches!(self.cx.params.get(&unit).and_then(|m| m.get(p)).map(|p| &p.value), Some(Lit::Duration(d)) if d.is_zero())
                        }
                        _ => false,
                    };
                    if matches!(t1, Term::Wild(_)) {
                        return;
                    }
                    if let Some(v) = self.time_var_fresh(t1, "the third argument of lag") {
                        let prov = base.map(|b| Self::derived_prov(self.prov_of(&b), !zero)).unwrap_or(TimeProv::Other);
                        self.bind(&v, Some(Ty::Timestamp), prov);
                    }
                }
                Builtin::MonthStart { t } => {
                    self.time_var_bound(t, "the argument of month_start");
                }
                Builtin::DayStart { t } => {
                    self.time_var_bound(t, "the argument of day_start");
                }
            },
            Literal::Window { var, kind, base, dur, min, span } => {
                if !in_agg {
                    self.err(
                        Code::B,
                        *span,
                        "`T1 in window(...)` binds T1 only inside an aggregation (`X = agg(e) over (...)`); outside one use prev or lag",
                    );
                }
                let b = self.time_var_bound(base, "the base of the window");
                self.expect_expr(dur, |t| *t == Ty::Duration, "a Duration");
                self.expect_expr(min, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count (the minimum observation count)");
                let strict = *kind == WindowKind::Prior;
                let prov = b.map(|b| Self::derived_prov(self.prov_of(&b), strict)).unwrap_or(TimeProv::Other);
                match var {
                    // Already bound by an earlier atom of this conjunction: the window is its constraint.
                    Term::Var(v, _) if self.pending_windows.contains_key(v) && self.is_bound(v) => {
                        if let Some(st) = self.vars.get_mut(v) {
                            st.prov = prov;
                        }
                    }
                    _ => {
                        if let Some(v) = self.time_var_fresh(var, "a window") {
                            self.bind(&v, Some(Ty::Timestamp), prov);
                        }
                    }
                }
            }
            Literal::Cmp { op, lhs, rhs, span } => {
                let tl = self.expr_ty(lhs);
                let tr = self.expr_ty(rhs);
                if let (Some(tl), Some(tr)) = (tl, tr) {
                    if let Err(m) = cmp_ok(*op, &tl, &tr) {
                        self.err(Code::T, *span, format!("in `{} {} {}`: {}", lhs, op, rhs, m));
                    }
                }
            }
            Literal::Assign { var, expr, span } => {
                if let Some(st) = self.vars.get(var).cloned() {
                    // Already bound: a comparison on equality.
                    if let (Some(tv), Some(te)) = (st.ty, self.expr_ty(expr)) {
                        if let Err(m) = cmp_ok(CmpOp::Eq, &tv, &te) {
                            self.err(Code::T, *span, format!("in `{} = {}`: {}", var, expr, m));
                        }
                    }
                } else {
                    let t = self.expr_ty(expr);
                    if t == Some(Ty::Timestamp) {
                        self.err(Code::T, *span, "a Timestamp cannot be assigned; bind times with prev, lag, window or prior_window");
                    }
                    self.bind(var, t, TimeProv::Other);
                }
            }
            Literal::Agg { var, agg, args, conj, span } => {
                if in_agg {
                    self.err(Code::B, *span, "aggregations do not nest; compute the inner aggregate in its own relation");
                }
                if self.is_bound(var) {
                    self.err(Code::B, *span, format!("aggregate result `{}` is already bound", var));
                }
                let saved = self.vars.clone();
                // Register the conjunction's window variables up front, so that
                // an atom written before its window (section 4's bivariate
                // example) is judged by the window's provenance.
                let saved_pending = std::mem::take(&mut self.pending_windows);
                for l in conj {
                    if let Literal::Window {
                        var: Term::Var(v, _),
                        kind,
                        base: Term::Var(b, _),
                        ..
                    } = l
                    {
                        if !self.is_bound(v) {
                            let prov = if self.is_bound(b) {
                                Self::derived_prov(self.prov_of(b), *kind == WindowKind::Prior)
                            } else {
                                TimeProv::Other
                            };
                            self.pending_windows.insert(v.clone(), prov);
                        }
                    }
                }
                for l in conj {
                    self.literal(l, true);
                }
                self.pending_windows = saved_pending;
                let mut tys = Vec::new();
                let mut ok = true;
                for a in args {
                    match self.expr_ty(a) {
                        Some(t) => tys.push(t),
                        None => ok = false,
                    }
                }
                self.vars = saved;
                let rty = if !ok {
                    None
                } else if agg == "first" || agg == "last" {
                    self.err(
                        Code::D,
                        *span,
                        format!(
                            "`{}` is ordered only by the fine temporal key inside a resample; over an arbitrary group it is not deterministic, so name a total order (top with by) or use max/min",
                            agg
                        ),
                    );
                    None
                } else {
                    match agg_type(agg, &tys) {
                        Ok(t) => Some(t),
                        Err(m) => {
                            self.err(Code::T, *span, format!("in `{} = {}(...)`: {}", var, agg, m));
                            None
                        }
                    }
                };
                self.bind(var, rty, TimeProv::Other);
            }
            Literal::Top { n, atom, by, span } => {
                if in_agg {
                    self.err(Code::B, *span, "a reduction is not permitted inside an aggregation");
                }
                self.expect_expr(n, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count");
                if let Some(sig) = self.cx.relations.get(&atom.name).cloned() {
                    if let Some(k) = sig.key_pos() {
                        if let Term::Var(v, sp) = &atom.terms[k] {
                            if !self.is_bound(v) {
                                self.err(Code::D, *sp, format!("the temporal key `{}` of `{}` must be bound before the reduction: top keeps N tuples per group of bound outer variables, and the group is one bar (bind `{}` with a positive atom such as `bar({})` first)", v, atom.name, v, v));
                            }
                        }
                    }
                }
                let info = self.atom(atom, AtomCtx::Top);
                let Some(by) = by else {
                    self.err(
                        Code::D,
                        *span,
                        format!(
                            "`top` over `{}` names no order; every reduction needs `by (k1 asc|desc, ..., tie-break)` so that evaluation is deterministic",
                            atom.name
                        ),
                    );
                    return;
                };
                let Some(info) = info else { return };
                let atom_vars: HashSet<String> = atom.terms.iter().filter_map(|t| if let Term::Var(v, _) = t { Some(v.clone()) } else { None }).collect();
                let key_names: HashSet<String> = by.iter().map(|(k, _, _)| k.clone()).collect();
                for (k, _, ksp) in by {
                    if !atom_vars.contains(k) {
                        self.err(
                            Code::D,
                            *ksp,
                            format!("order key `{}` is not bound by `{}`; every key must be a variable of the reduced atom", k, atom.name),
                        );
                    }
                }
                for pos in info.sig.identity_positions() {
                    let arg = &info.sig.args[pos];
                    match &atom.terms[pos] {
                        Term::Var(v, _) if info.fresh.contains(v) && !key_names.contains(v) => {
                            self.err(
                                Code::D,
                                *span,
                                format!(
                                    "the order is not total: identity column `{}` of `{}` (bound here as `{}`) is not among the keys; add `{} asc` as the tie-break",
                                    arg.name, atom.name, v, v
                                ),
                            );
                        }
                        Term::Wild(sp) => {
                            self.err(
                                Code::D,
                                *sp,
                                format!("identity column `{}` of `{}` may not be `_` in a reduction; name it so the order can be total", arg.name, atom.name),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Literal::Resample { inner, to, as_var, min, aggs, span } => {
                if in_agg {
                    self.err(Code::B, *span, "a resample is not permitted inside an aggregation");
                }
                if *to != self.res {
                    self.err(
                        Code::X,
                        *span,
                        format!("resample targets {} but this rule is at {}; a resample produces tuples at the head's resolution", to, self.res),
                    );
                }
                self.expect_expr(min, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count (the minimum bucket count)");
                let head_t = self.head_time.clone();
                if head_t.as_deref() != Some(as_var.as_str()) {
                    self.err(
                        Code::X,
                        *span,
                        format!(
                            "the bucket label `{}` must be the head's temporal key{}",
                            as_var,
                            head_t.map(|h| format!(" `{}`", h)).unwrap_or_default()
                        ),
                    );
                }
                // An already-bound label (from an earlier atom at the head's
                // resolution) is an equality constraint on the bucket.
                let before: HashSet<String> = self.vars.keys().cloned().collect();
                let info = self.atom(inner, AtomCtx::ResampleInner);
                let mut locals: Vec<String> = Vec::new();
                if let Some(info) = &info {
                    match info.sig.res {
                        Some(r) if r.finer_than(*to) => {}
                        Some(r) => self.err(
                            Code::X,
                            inner.span,
                            format!("`{}` is at {}, which is not strictly finer than {}; resample only goes from fine to coarse", inner.name, r, to),
                        ),
                        None => {}
                    }
                    if let Some(k) = info.sig.key_pos() {
                        match &inner.terms[k] {
                            Term::Var(v, _) if info.fresh.contains(v) => locals.push(v.clone()),
                            Term::Var(v, sp) => self.err(
                                Code::X,
                                *sp,
                                format!("the inner temporal key `{}` of a resample must be a fresh variable; the bucket label `{}` replaces it", v, as_var),
                            ),
                            _ => {}
                        }
                    }
                    for (i, arg) in info.sig.args.iter().enumerate() {
                        if let Term::Var(v, _) = &inner.terms[i] {
                            if info.fresh.contains(v) && !arg.ty.is_entity() && arg.mode != Mode::Key {
                                locals.push(v.clone());
                            }
                        }
                    }
                }
                let mut results: Vec<(String, Option<Ty>)> = Vec::new();
                for (x, agg, e) in aggs {
                    if !RESAMPLE_AGGREGATES.contains(&agg.as_str()) {
                        self.err(Code::X, *span, format!("`{}` is not a resample aggregate (first, last, max, min, sum, mean, count)", agg));
                    }
                    if self.is_bound(x) || before.contains(x) {
                        self.err(Code::B, *span, format!("resample result `{}` is already bound", x));
                    }
                    let t = self.expr_ty(e).and_then(|t| match agg_type(agg, &[t]) {
                        Ok(t) => Some(t),
                        Err(m) => {
                            self.err(Code::T, *span, format!("in `{} = {}(...)`: {}", x, agg, m));
                            None
                        }
                    });
                    results.push((x.clone(), t));
                }
                for l in &locals {
                    self.vars.remove(l);
                }
                self.bind(as_var, Some(Ty::Timestamp), TimeProv::Head);
                for (x, t) in results {
                    self.bind(&x, t, TimeProv::Other);
                }
            }
        }
    }

    fn head(&mut self) {
        let head = self.rule.head.clone();
        let Some(sig) = self.cx.relations.get(&head.name).cloned() else {
            self.err(
                Code::U,
                head.span,
                format!("rule defines `{}`, which is not declared; declare it with `rel {}(...)` in `{}`", head.name, head.name, self.rule.unit),
            );
            return;
        };
        match &sig.kind {
            Kind::Primitive { env } => {
                self.err(Code::U, head.span, format!("`{}` is a primitive of environment `{}` and cannot be defined by a rule", head.name, env));
                return;
            }
            Kind::Executor | Kind::KernelState => {
                self.err(Code::U, head.span, format!("`{}` is kernel-supplied and cannot be defined by a rule", head.name));
                return;
            }
            Kind::Derived { unit } if *unit != self.rule.unit => {
                self.err(Code::U, head.span, format!("`{}` is declared in `{}`; rules for it belong there", head.name, unit));
                return;
            }
            _ => {}
        }
        if head.terms.len() != sig.args.len() {
            self.err(
                Code::T,
                head.span,
                format!("`{}` takes {} arguments, {} given in the head", head.name, sig.args.len(), head.terms.len()),
            );
            return;
        }
        for (term, arg) in head.terms.iter().zip(sig.args.iter()) {
            match term {
                Term::Var(v, sp) => match self.vars.get(v).cloned() {
                    Some(st) => {
                        if let Some(t) = &st.ty {
                            if !compat(&arg.ty, t) {
                                self.err(Code::T, *sp, format!("head argument `{}` of `{}` is {} but `{}` is {}", arg.name, head.name, arg.ty, v, t));
                            }
                        }
                        if arg.mode == Mode::Key && !matches!(st.prov, TimeProv::Head | TimeProv::Unknown) {
                            self.err(Code::F, *sp, format!("head temporal key `{}` is not bound in a temporal-key position of a positive body atom (or as a resample bucket); its value would not be the time the tuple becomes available", v));
                        }
                    }
                    None => self.err(Code::B, *sp, format!("head variable `{}` is unbound: every head variable must be bound by the body", v)),
                },
                Term::Wild(sp) => self.err(Code::B, *sp, "`_` is not permitted in a head"),
                Term::Param(p, sp) => {
                    if let Some(pt) = self.param_ty(p, *sp) {
                        if !compat(&arg.ty, &pt) {
                            self.err(Code::T, *sp, format!("head argument `{}` of `{}` is {} but parameter `{}` is {}", arg.name, head.name, arg.ty, p, pt));
                        }
                    }
                }
                Term::Lit(l, sp) => self.check_lit_type(&arg.ty, l, *sp, &format!("head argument `{}` of `{}`", arg.name, head.name)),
                Term::Ctor(c, subs, sp) => {
                    if arg.ty != Ty::Decision {
                        self.err(Code::T, *sp, format!("head argument `{}` of `{}` is {}, not a decision", arg.name, head.name, arg.ty));
                        continue;
                    }
                    self.ctor(c, subs, *sp, true, true);
                }
            }
        }
        if head.name == "decide" {
            self.decide_head(&head);
        }
    }

    /// WF-9 (C): decide heads use the declared mode's constructors, and T is
    /// the temporal key of at least one positive body atom.
    fn decide_head(&mut self, head: &Atom) {
        let Some(mode) = self.cx.mode else { return };
        if let Some(Term::Ctor(c, _, sp)) = head.terms.get(1) {
            match ctor_mode(c) {
                Some(m) if m != mode => self.err(Code::C, *sp, format!("`{}` is a {} constructor but the strategy declares `mode {}`", c, m, mode)),
                _ => {}
            }
        } else if let Some(t) = head.terms.get(1) {
            self.err(
                Code::C,
                t.span(),
                format!("a decide rule's decision must be a constructor of the declared mode ({}), found `{}`", mode.ctors().join(", "), t),
            );
        }
        if let Some(Term::Var(t, sp)) = head.terms.first() {
            if !self.key_vars_positive.contains(t) && self.prov_of(t) != TimeProv::Unknown {
                self.err(Code::C, *sp, format!("decide's time `{}` must be the temporal key of at least one positive body atom", t));
            }
        }
    }
}
