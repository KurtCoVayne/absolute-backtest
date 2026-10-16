//! The per-rule judgments on the elaborated rule (language-v2, section 10
//! and the retained judgments of the semantic model): a well-moded order
//! (Proposition 4.2; B, M), types (T), causality (F), deterministic
//! reductions (D), explicit grouping (G), window sanity (W7), and the
//! resolution of string literals and decision constructors.

use std::collections::{HashMap, HashSet};

use super::types::{agg_type, bin_type, call_type, cmp_ok, compat, neg_type};
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
    AsOf(TimeProv),
}

/// What `analyze` returns besides the info: the resolved constructor of a
/// decide head (`hold` as `hold_weight` or `hold_quantity`).
pub(crate) struct Analysis {
    pub info: RuleInfo,
    pub head_ctor: Option<String>,
    /// The provenance of each head term relative to the head time: what a
    /// Timestamp output of the relation carries to the rules that read it.
    pub head_prov: Vec<TimeProv>,
}

pub(crate) fn analyze_full(cx: &mut Checker, idx: usize, rule: &Rule) -> Option<Analysis> {
    let label = super::rule_label(&cx.rules, idx);
    let head_sig = cx.relations.get(&rule.head.name).cloned()?;
    let cal = head_sig.cal.clone();
    let cals = cx.var_cals.get(idx).cloned().unwrap_or_default();
    let key_pos = head_sig.key_pos()?;
    let head_key = match rule.head.terms.get(key_pos) {
        Some(Term::Var(v, _)) => v.clone(),
        _ => return None,
    };
    let mut an = Analyzer {
        cx,
        rule,
        label,
        cal,
        head_key: head_key.clone(),
        cals,
        vars: HashMap::new(),
        refs: vec![],
        params_used: vec![],
        str_types: vec![],
        head_ctor: None,
        errors: false,
    };
    an.bind(&head_key, Some(Ty::Timestamp), TimeProv::Head);
    let order = an.schedule(&rule.body, false);
    an.head(&head_sig);
    if an.errors {
        return None;
    }
    let head_prov: Vec<TimeProv> = rule
        .head
        .terms
        .iter()
        .map(|t| match t {
            Term::Var(v, _) => an.prov_of(v),
            _ => TimeProv::Other,
        })
        .collect();
    Some(Analysis {
        info: RuleInfo {
            head: rule.head.name.clone(),
            refs: an.refs,
            params_used: an.params_used,
            cal: an.cal,
            order,
            str_types: an.str_types,
        },
        head_ctor: an.head_ctor,
        head_prov,
    })
}

struct Analyzer<'c, 'a> {
    cx: &'c mut Checker<'a>,
    rule: &'c Rule,
    label: String,
    cal: Cal,
    head_key: String,
    cals: HashMap<String, Cal>,
    vars: HashMap<String, VarState>,
    refs: Vec<AtomRef>,
    params_used: Vec<String>,
    str_types: Vec<(Span, Ty)>,
    head_ctor: Option<String>,
    errors: bool,
}

/// The variables a literal binds when it is evaluated with every other
/// variable bound: its outputs.
fn outputs(l: &Literal, out: &mut Vec<String>) {
    fn terms(ts: &[Term], out: &mut Vec<String>) {
        for t in ts {
            match t {
                Term::Var(v, _) => out.push(v.clone()),
                Term::Ctor(_, subs, _) => terms(subs, out),
                _ => {}
            }
        }
    }
    match l {
        Literal::Atom(a) | Literal::AsOf { atom: a, .. } => terms(&a.terms, out),
        Literal::Neg(_) | Literal::Cmp { .. } => {}
        Literal::Builtin(b, _) => match b {
            Builtin::Prev { t1, .. } | Builtin::Lag { t1, .. } => terms(std::slice::from_ref(t1), out),
            Builtin::SessionOf { s, .. } => terms(std::slice::from_ref(s), out),
            Builtin::MonthStart { .. } | Builtin::DayStart { .. } => {}
        },
        Literal::Window { var, .. } => terms(std::slice::from_ref(var), out),
        Literal::Assign { var, .. } | Literal::Agg { var, .. } | Literal::Case { var, .. } => out.push(var.clone()),
        Literal::Top { atom, rank, .. } => {
            terms(&atom.terms, out);
            if let Some((k, _, _)) = rank {
                out.push(k.clone());
            }
        }
    }
}

/// Every variable a literal mentions.
fn mentions(l: &Literal, out: &mut Vec<String>) {
    crate::kernel::eval::literal_vars(l, out);
}

impl<'c, 'a> Analyzer<'c, 'a> {
    fn err(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        let unit = self.rule.unit.clone();
        let label = self.label.clone();
        if !code.is_warning() {
            self.errors = true;
        }
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
            // The session's earlier labels lie before the session opened, before T.
            TimeProv::Future => {
                if strict {
                    TimeProv::Strict
                } else {
                    TimeProv::Future
                }
            }
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
        match self.cx.params.get(name) {
            Some(p) => {
                self.params_used.push(name.to_string());
                Some(p.ty.clone())
            }
            None => {
                if ORDER_KINDS.contains(&name) || name == "gtc" || name == "day" {
                    return None;
                }
                let strategy = self.cx.root.name.clone();
                self.err(
                    Code::U,
                    span,
                    format!("`{}` is neither a parameter of `{}` nor a variable (variables start with an uppercase letter)", name, strategy),
                );
                None
            }
        }
    }

    fn check_lit_type(&mut self, expected: &Ty, lit: &Lit, span: Span, what: &str) {
        let actual = lit.ty();
        if !compat(expected, &actual) {
            let actual_text = if actual == Ty::StrLit {
                "a string literal (an entity or a label)".to_string()
            } else {
                actual.to_string()
            };
            let hint = self.typed_hint(expected, lit);
            self.err(Code::T, span, format!("{} expects {} but literal `{}` is {}{}", what, expected, lit, actual_text, hint));
        } else if let Lit::Str(_) = lit {
            self.resolve_str(span, expected);
        }
    }

    /// The typed spelling a dimensioned literal should have (7.3).
    fn typed_hint(&self, expected: &Ty, lit: &Lit) -> String {
        let Ty::Quantity(d) = expected else { return String::new() };
        let Some(x) = lit.magnitude() else { return String::new() };
        let unit = self.cx.unit_word.clone();
        let per = unit.trim_end_matches('s').to_string();
        let cur = d.currency.clone().unwrap_or_else(|| "USD".into());
        let spelling = if d.is_price() {
            format!("{} {}/{}", x, cur, per)
        } else if d.is_notional() {
            format!("{} {}", x, cur)
        } else if d.is_quantity() {
            format!("{} {}", x, unit)
        } else {
            return String::new();
        };
        format!("; write the typed form `{}`", spelling)
    }

    fn resolve_str(&mut self, span: Span, ty: &Ty) {
        if matches!(ty, Ty::Entity | Ty::Label) {
            self.str_types.push((span, ty.clone()));
        }
    }

    fn resolve_cmp_strs(&mut self, lhs: &Expr, tl: &Ty, rhs: &Expr, tr: &Ty) {
        if let Expr::Lit(Lit::Str(_), sp) = lhs {
            self.resolve_str(*sp, tr);
        }
        if let Expr::Lit(Lit::Str(_), sp) = rhs {
            self.resolve_str(*sp, tl);
        }
    }

    fn str_without_context(&mut self, e: &Expr) -> bool {
        fn find(e: &Expr) -> Option<Span> {
            match e {
                Expr::Lit(Lit::Str(_), sp) => Some(*sp),
                Expr::Neg(a, _) => find(a),
                Expr::Bin(_, a, b, _) => find(a).or_else(|| find(b)),
                Expr::Call(_, args, _) => args.iter().find_map(find),
                _ => None,
            }
        }
        if let Some(sp) = find(e) {
            self.err(
                Code::T,
                sp,
                "a string literal is an entity or a label from context: compare it with a typed variable, pass it to a typed argument, or declare a param",
            );
            true
        } else {
            false
        }
    }

    // ---- the schedule ----

    /// A well-moded order of `body` (Proposition 4.2): the first literal in
    /// written order whose requirements are met, repeatedly; each literal is
    /// judged as it is scheduled. Literals that can never be scheduled are
    /// an M error naming the variable no literal binds.
    fn schedule(&mut self, body: &[Literal], in_agg: bool) -> Vec<usize> {
        let mut remaining: Vec<usize> = (0..body.len()).collect();
        let mut order: Vec<usize> = Vec::new();
        let producers: Vec<Vec<String>> = body
            .iter()
            .map(|l| {
                let mut o = Vec::new();
                outputs(l, &mut o);
                o
            })
            .collect();
        let producible = |v: &str, except: usize| -> bool { producers.iter().enumerate().any(|(i, p)| i != except && p.iter().any(|x| x == v)) };
        while !remaining.is_empty() {
            let mut picked = None;
            for &i in &remaining {
                if self.ready(&body[i], i, &producible) {
                    picked = Some(i);
                    break;
                }
            }
            let Some(i) = picked else {
                // Nothing is ready: name the first blocked literal and the variable it needs.
                let i = remaining[0];
                let mut needed: Vec<String> = Vec::new();
                mentions(&body[i], &mut needed);
                let unbound = needed.iter().find(|v| !self.is_bound(v) && !producers[i].contains(v)).cloned().unwrap_or_default();
                let span = body[i].span();
                self.err(
                    Code::M,
                    span,
                    format!(
                        "no well-moded order: `{}` needs `{}` bound, and no literal of the rule binds it (bind it with an atom, an assignment or a temporal builtin, or lift it into `params`)",
                        body[i].describe(),
                        unbound
                    ),
                );
                // Judge the rest in written order so later errors still show.
                for &j in &remaining {
                    self.judge(&body[j], in_agg);
                    order.push(j);
                }
                return order;
            };
            self.judge(&body[i], in_agg);
            order.push(i);
            remaining.retain(|&j| j != i);
        }
        order
    }

    /// Whether a literal's requirements are met: its inputs are bound, or
    /// could never be bound by another literal (then it binds them itself,
    /// and the judgments below say what that means).
    fn ready(&self, l: &Literal, i: usize, producible: &dyn Fn(&str, usize) -> bool) -> bool {
        let bound_or_unproducible = |v: &str| self.is_bound(v) || !producible(v, i);
        let term_ok = |t: &Term| match t {
            Term::Var(v, _) => bound_or_unproducible(v),
            _ => true,
        };
        let expr_ok = |e: &Expr| {
            let mut vs = Vec::new();
            e.vars(&mut vs);
            vs.iter().all(|(v, _)| bound_or_unproducible(v))
        };
        match l {
            Literal::Atom(a) => {
                let key = self.cx.relations.get(&a.name).and_then(|s| s.key_pos()).and_then(|k| a.terms.get(k));
                key.map(term_ok).unwrap_or(true)
            }
            Literal::Neg(a) => {
                let mut vs = Vec::new();
                for t in &a.terms {
                    if let Term::Var(v, _) = t {
                        vs.push(v.clone());
                    }
                    if let Term::Ctor(_, subs, _) = t {
                        for s in subs {
                            if let Term::Var(v, _) = s {
                                vs.push(v.clone());
                            }
                        }
                    }
                }
                vs.iter().all(|v| bound_or_unproducible(v))
            }
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, .. } | Builtin::MonthStart { t } | Builtin::DayStart { t } => term_ok(t),
                Builtin::Lag { t, n, .. } => term_ok(t) && expr_ok(n),
                Builtin::SessionOf { a, t, .. } => term_ok(a) && term_ok(t),
            },
            Literal::Window { base, dur, min, lo, .. } => term_ok(base) && expr_ok(dur) && expr_ok(min) && lo.as_ref().map(expr_ok).unwrap_or(true),
            Literal::Cmp { lhs, rhs, .. } => expr_ok(lhs) && expr_ok(rhs),
            Literal::Assign { expr, .. } => expr_ok(expr),
            Literal::Agg { args, conj, within, .. } => {
                let mut vs: Vec<String> = Vec::new();
                for a in args {
                    let mut t = Vec::new();
                    a.vars(&mut t);
                    vs.extend(t.into_iter().map(|(v, _)| v));
                }
                for c in conj {
                    mentions(c, &mut vs);
                }
                let mut local: Vec<String> = Vec::new();
                for c in conj {
                    outputs(c, &mut local);
                }
                let within_ok = within.as_ref().map(|w| w.iter().all(|v| self.is_bound(v))).unwrap_or(true);
                within_ok && vs.iter().all(|v| self.is_bound(v) || local.contains(v) || !producible(v, i))
            }
            Literal::Top { n, within, .. } => {
                let within_ok = within.as_ref().map(|w| w.iter().all(|v| self.is_bound(v))).unwrap_or(true);
                within_ok && n.as_ref().map(expr_ok).unwrap_or(true)
            }
            Literal::AsOf { at, .. } => term_ok(at),
            Literal::Case { branches, otherwise, .. } => {
                let mut ok = otherwise.as_ref().map(expr_ok).unwrap_or(true);
                for b in branches {
                    let mut local: Vec<String> = Vec::new();
                    for g in &b.guard {
                        outputs(g, &mut local);
                    }
                    let mut vs: Vec<String> = Vec::new();
                    for g in &b.guard {
                        mentions(g, &mut vs);
                    }
                    let mut ev = Vec::new();
                    b.value.vars(&mut ev);
                    vs.extend(ev.into_iter().map(|(v, _)| v));
                    ok &= vs.iter().all(|v| self.is_bound(v) || local.contains(v) || !producible(v, i));
                }
                ok
            }
        }
    }

    // ---- literals ----

    /// Judge a literal; when it fails, the variables it mentions are bound
    /// as unknown, so that the failure is reported once and not again as
    /// unbound variables or underived keys downstream.
    fn judge(&mut self, lit: &Literal, in_agg: bool) {
        let before = self.errors;
        self.literal(lit, in_agg);
        if self.errors && !before {
            let mut vs = Vec::new();
            mentions(lit, &mut vs);
            for v in vs {
                if !self.is_bound(&v) {
                    self.bind(&v, None, TimeProv::Unknown);
                }
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
            Literal::AsOf { atom, at, within_bucket, .. } => {
                let head_t = self.head_key.clone();
                let base = self.time_var_bound(at, "the time of an as-of read");
                let prov = match base {
                    None => TimeProv::Unknown,
                    Some(b) => match self.prov_of(&b) {
                        TimeProv::Other if b == head_t => TimeProv::Causal,
                        TimeProv::Other => {
                            self.err(
                                Code::F,
                                at.span(),
                                format!(
                                    "the as-of time `{}` of `{} asof {}` is not derived from the head time `{}`; bind it first with prev, lag, a window or a bucket of `{}`",
                                    b, atom.name, b, head_t, head_t
                                ),
                            );
                            TimeProv::Unknown
                        }
                        TimeProv::Future => {
                            self.err(
                                Code::F,
                                at.span(),
                                format!("`{}` is the label of the bar's own session, which closes after the bar; a relation cannot be read as of it", b),
                            );
                            TimeProv::Unknown
                        }
                        p => Self::derived_prov(p, false),
                    },
                };
                let _ = within_bucket;
                self.atom(atom, AtomCtx::AsOf(prov));
            }
            Literal::Builtin(b, _) => match b {
                Builtin::Prev { t, t1 } => {
                    let base = self.time_var_bound(t, "the first argument of prev");
                    if matches!(t1, Term::Wild(_)) {
                        return;
                    }
                    if let Some(v) = self.time_var_fresh(t1, "the second argument of prev") {
                        let prov = base.map(|b| Self::derived_prov(self.prov_of(&b), true)).unwrap_or(TimeProv::Other);
                        self.bind(&v, Some(Ty::Timestamp), prov);
                    }
                }
                Builtin::Lag { t, n, t1 } => {
                    let base = self.time_var_bound(t, "the first argument of lag");
                    self.expect_expr(n, |t| *t == Ty::Duration, "a Duration");
                    let zero = match n {
                        Expr::Lit(Lit::Duration(d), _) => d.is_zero(),
                        Expr::Param(p, _) => matches!(self.cx.params.get(p).map(|p| &p.value), Some(Lit::Duration(d)) if d.is_zero()),
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
                Builtin::SessionOf { a, t, s } => {
                    self.entity_term(a, "the first argument of session_of");
                    let base = self.time_var_bound(t, "the second argument of session_of");
                    if let Some(b) = &base {
                        if self.cals.get(b).map(|c| *c != Cal::Base).unwrap_or(false) {
                            self.err(
                                Code::X,
                                t.span(),
                                format!("`session_of` takes a base bar, and `{}` is on `{}`", b, self.cx.cal_name(self.cals.get(b).unwrap())),
                            );
                        }
                    }
                    if matches!(s, Term::Wild(_)) {
                        return;
                    }
                    if let Some(v) = self.time_var_fresh(s, "the third argument of session_of") {
                        let prov = match base.map(|b| self.prov_of(&b)) {
                            Some(TimeProv::Head) => TimeProv::Future,
                            Some(TimeProv::Unknown) => TimeProv::Unknown,
                            _ => TimeProv::Other,
                        };
                        self.bind(&v, Some(Ty::Timestamp), prov);
                    }
                }
            },
            Literal::Window { var, kind, base, dur, min, lo, span } => {
                if !in_agg {
                    self.err(
                        Code::B,
                        *span,
                        "a temporal constraint binds its variable only inside an aggregation (`X = agg(e) over (...)`); outside one use prev or lag",
                    );
                }
                let b = self.time_var_bound(base, "the base of the constraint");
                match kind {
                    WindowKind::Rows | WindowKind::PriorRows => {
                        self.expect_expr(dur, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count (the number of rows)");
                        self.expect_expr(min, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count (the minimum count)");
                    }
                    WindowKind::Window | WindowKind::Prior => {
                        self.expect_expr(dur, |t| *t == Ty::Duration, "a Duration");
                        self.expect_expr(min, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count (the minimum count)");
                        self.window_sanity(dur, min, *span);
                    }
                    WindowKind::Bucket(_) => {}
                    WindowKind::Session => {
                        self.expect_expr(dur, |t| *t == Ty::Duration, "a Duration (the upper offset)");
                        if let Some(lo) = lo {
                            self.expect_expr(lo, |t| *t == Ty::Duration, "a Duration (the lower offset)");
                        }
                    }
                }
                let strict = matches!(kind, WindowKind::Prior | WindowKind::PriorRows);
                let prov = b.map(|b| Self::derived_prov(self.prov_of(&b), strict)).unwrap_or(TimeProv::Other);
                // A session label as the base of a session constraint: its bars are at or before it.
                let prov = if matches!(kind, WindowKind::Session | WindowKind::Bucket(_)) && prov == TimeProv::Future {
                    TimeProv::Causal
                } else {
                    prov
                };
                if let Some(v) = self.time_var_fresh(var, "a temporal constraint") {
                    self.bind(&v, Some(Ty::Timestamp), prov);
                }
            }
            Literal::Cmp { op, lhs, rhs, span } => {
                let tl = self.expr_ty(lhs);
                let tr = self.expr_ty(rhs);
                if let (Some(tl), Some(tr)) = (tl, tr) {
                    if let Err(m) = cmp_ok(*op, &tl, &tr) {
                        let zero = [lhs, rhs]
                            .iter()
                            .any(|e| matches!(e, Expr::Lit(l, _) if l.magnitude() == Some(0.0) && matches!(l, Lit::Int(_) | Lit::Float(_))));
                        let hint = if zero {
                            let dimensioned = if matches!(tl, Ty::Quantity(ref d) if !d.is_scalar()) {
                                Some(tl.clone())
                            } else if matches!(tr, Ty::Quantity(ref d) if !d.is_scalar()) {
                                Some(tr.clone())
                            } else {
                                None
                            };
                            dimensioned.map(|t| self.typed_hint(&t, &Lit::Int(0))).unwrap_or_default()
                        } else {
                            String::new()
                        };
                        self.err(Code::T, *span, format!("in `{} {} {}`: {}{}", lhs, op, rhs, m, hint));
                    } else {
                        self.resolve_cmp_strs(lhs, &tl, rhs, &tr);
                    }
                }
            }
            Literal::Assign { var, expr, span } => {
                let t = self.expr_ty(expr);
                match self.vars.get(var).cloned() {
                    Some(st) => {
                        if let (Some(vt), Some(et)) = (st.ty, t) {
                            if let Err(m) = cmp_ok(CmpOp::Eq, &vt, &et) {
                                self.err(Code::T, *span, format!("in `{} = {}`: {}", var, expr, m));
                            } else if let Expr::Lit(Lit::Str(_), sp) = expr {
                                self.resolve_str(*sp, &vt);
                            }
                        }
                    }
                    None => {
                        if self.str_without_context(expr) {
                            return;
                        }
                        self.bind(var, t, TimeProv::Other);
                    }
                }
            }
            Literal::Agg { var, agg, args, conj, within, span } => {
                if in_agg {
                    self.err(Code::B, *span, "aggregations do not nest; compute the inner aggregate in its own relation");
                    return;
                }
                if self.is_bound(var) {
                    self.err(Code::B, *span, format!("aggregate result `{}` is already bound", var));
                    return;
                }
                if let Some(w) = within {
                    for v in w {
                        if !self.is_bound(v) {
                            self.err(Code::G, *span, format!("`{}` in `within (...)` is unbound; the group names bound variables", v));
                        }
                    }
                }
                // The conjunction in its own scope.
                let saved = self.vars.clone();
                self.schedule(conj, true);
                // The group of an aggregation with no `within` is its free
                // variables (4.2): what the conjunction binds that the rest
                // of the rule shares (the head or another literal), neither a
                // constraint's time nor aggregated over, is a group key,
                // bound by the aggregation with one result per group
                // (`close@C(A, T, P) :- P = last(P1) over (...)`); a
                // variable of the conjunction alone is local to it.
                let outer = crate::kernel::eval::outer_vars(self.rule, lit);
                let mut aggregated: Vec<String> = Vec::new();
                for c in conj {
                    if let Literal::Window { var: Term::Var(v, _), .. } = c {
                        aggregated.push(v.clone());
                    }
                }
                for a in args {
                    let mut vs = Vec::new();
                    a.vars(&mut vs);
                    aggregated.extend(vs.into_iter().map(|(v, _)| v));
                }
                let group_keys: Vec<(String, VarState)> = if within.is_none() {
                    self.vars
                        .iter()
                        .filter(|(v, _)| !saved.contains_key(*v) && !aggregated.contains(v) && outer.contains(v))
                        .map(|(v, st)| (v.clone(), st.clone()))
                        .collect()
                } else {
                    vec![]
                };
                // Every temporal key inside the group is the head's bar or bound by a constraint of it (WF-6).
                let has_window = conj.iter().any(|l| matches!(l, Literal::Window { .. }));
                if matches!(agg.as_str(), "first" | "last") && !has_window {
                    self.err(
                        Code::D,
                        *span,
                        format!(
                            "`{}` is ordered by the temporal key of a rows, window or session constraint; over the entities of one bar the group has no order, so which value is {} is unspecified",
                            agg, agg
                        ),
                    );
                }
                let mut tys = Vec::new();
                for a in args {
                    if self.str_without_context(a) {
                        self.vars = saved;
                        return;
                    }
                    match self.expr_ty(a) {
                        Some(t) => tys.push(t),
                        None => {
                            self.vars = saved;
                            return;
                        }
                    }
                }
                self.vars = saved;
                for (v, st) in group_keys {
                    self.vars.insert(v, st);
                }
                match agg_type(agg, &tys) {
                    Ok(t) => self.bind(var, Some(t), TimeProv::Other),
                    Err(m) => self.err(Code::T, *span, format!("in `{} = {}(...)`: {}", var, agg, m)),
                }
            }
            Literal::Top { n, atom, by, rank, within, span } => {
                if in_agg {
                    self.err(Code::B, *span, "a reduction is not permitted inside an aggregation");
                    return;
                }
                let what = if n.is_some() { "top" } else { "rank" };
                if let Some(n) = n {
                    self.expect_expr(n, |t| matches!(t, Ty::Count | Ty::IntLit), "a Count");
                }
                let Some(w) = within else {
                    self.err(
                        Code::G,
                        *span,
                        format!(
                            "`{}` names its group in `within (...)`: write `within (T)` to reduce the entities of one bar, or `within (A, T)` to rank one tuple",
                            what
                        ),
                    );
                    return;
                };
                for v in w {
                    if !self.is_bound(v) {
                        self.err(Code::G, *span, format!("`{}` in `within (...)` is unbound; the group names bound variables", v));
                        return;
                    }
                }
                let Some(sig) = self.cx.relations.get(&atom.name).cloned() else { return };
                // Every variable of the reduced atom outside the group is fresh.
                for (i, t) in atom.terms.iter().enumerate() {
                    if let Term::Var(v, sp) = t {
                        if !w.contains(v) && self.is_bound(v) {
                            let arg = sig.args.get(i).map(|a| a.name.clone()).unwrap_or_default();
                            self.err(
                                Code::G,
                                *sp,
                                format!(
                                    "`{}` is bound before the {} and is not in `within ({})`: a {} that varies over `{}` must not bind it; write `within ({}, {})` to rank one tuple, or remove the literal that binds `{}` and let the reduction bind it",
                                    v,
                                    what,
                                    w.join(", "),
                                    what,
                                    v,
                                    v,
                                    w.join(", "),
                                    v
                                ),
                            );
                            let _ = arg;
                            return;
                        }
                    }
                }
                // The atom's key must be in the group (one bar).
                if let Some(k) = sig.key_pos() {
                    match atom.terms.get(k) {
                        Some(Term::Var(v, sp)) if !w.contains(v) => {
                            self.err(
                                Code::G,
                                *sp,
                                format!("the temporal key `{}` of `{}` is not in `within (...)`: a reduction ranges over one bar's tuples", v, atom.name),
                            );
                            return;
                        }
                        _ => {}
                    }
                }
                if self.atom(atom, AtomCtx::InAgg).is_none() {
                    return;
                }
                if let Some(r) = self.refs.last_mut() {
                    r.is_top = true;
                    r.polarity = Polarity::Aggregate;
                }
                // The order: required, over the atom's variables, total for
                // `top` and ordinal `rank` (the identity columns outside
                // the group are among the keys).
                let ties = rank.as_ref().map(|(_, t, _)| *t).unwrap_or(Ties::Ordinal);
                match by {
                    None => {
                        self.err(
                            Code::D,
                            *span,
                            format!(
                                "`{}` has no `by (...)`: without a total order, which tuples survive is unspecified; order by a value and the identity, `by (M desc, A asc)`",
                                what
                            ),
                        );
                        return;
                    }
                    Some(keys) => {
                        let atom_vars: Vec<String> = atom.terms.iter().filter_map(|t| if let Term::Var(v, _) = t { Some(v.clone()) } else { None }).collect();
                        for (k, _, ksp) in keys {
                            if !atom_vars.contains(k) {
                                self.err(Code::D, *ksp, format!("order key `{}` is not a variable of `{}`", k, atom.name));
                                return;
                            }
                        }
                        if ties == Ties::Ordinal {
                            for i in sig.identity_positions() {
                                if let Some(Term::Var(v, _)) = atom.terms.get(i) {
                                    if !w.contains(v) && !keys.iter().any(|(k, _, _)| k == v) {
                                        self.err(
                                            Code::D,
                                            *span,
                                            format!(
                                                "the order keys omit the identity column `{}` of `{}`: two tuples with equal values would tie, and the order must be total; add `{} asc` as the last key",
                                                v, atom.name, v
                                            ),
                                        );
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                // W4: the group holds every identity of the atom.
                let ids: Vec<String> = sig
                    .identity_positions()
                    .iter()
                    .filter_map(|&i| if let Some(Term::Var(v, _)) = atom.terms.get(i) { Some(v.clone()) } else { None })
                    .collect();
                if !ids.is_empty() && ids.iter().all(|v| w.contains(v)) {
                    self.err(
                        Code::W4,
                        *span,
                        format!(
                            "`within ({})` holds every identity of `{}`, so each group has one tuple and the {} ranks it first",
                            w.join(", "),
                            atom.name,
                            what
                        ),
                    );
                }
                if let Some((k, ties, ksp)) = rank {
                    if self.is_bound(k) {
                        self.err(Code::B, *ksp, format!("the rank `{}` must be a fresh variable", k));
                    } else {
                        // An ordinal rank is a position, a Count; an averaged
                        // one is the mean of the positions a run of equal keys
                        // spans (2.5), a Scalar.
                        let ty = if *ties == Ties::Average { Ty::scalar() } else { Ty::Count };
                        self.bind(k, Some(ty), TimeProv::Other);
                    }
                }
            }
            Literal::Case { var, branches, otherwise, span } => {
                if self.is_bound(var) {
                    self.err(Code::B, *span, format!("case result `{}` is already bound", var));
                    return;
                }
                let mut ty: Option<Ty> = None;
                let saved = self.vars.clone();
                let guarded = branches.len() > 1 || otherwise.is_some();
                for b in branches {
                    self.vars = saved.clone();
                    for g in &b.guard {
                        if matches!(g, Literal::Agg { .. } | Literal::Top { .. } | Literal::Case { .. }) {
                            self.err(Code::B, g.span(), "a case guard is a conjunction of atoms, negations, comparisons and temporal builtins");
                        }
                    }
                    let n0 = self.refs.len();
                    self.schedule(&b.guard, in_agg);
                    // Later branches negate this guard: its atoms must be closed.
                    if guarded {
                        for r in self.refs[n0..].iter_mut() {
                            if r.polarity == Polarity::Positive {
                                r.in_case_guard = true;
                            }
                        }
                    }
                    if let Some(t) = self.expr_ty(&b.value) {
                        let t = if t == Ty::IntLit { Ty::scalar() } else { t };
                        match &ty {
                            None => ty = Some(t),
                            Some(prev) => {
                                if !compat(prev, &t) {
                                    self.err(Code::T, b.span, format!("the branches of the case form give `{}` different types: {} and {}", var, prev, t));
                                }
                            }
                        }
                    }
                }
                self.vars = saved;
                if let Some(e) = otherwise {
                    if let Some(t) = self.expr_ty(e) {
                        let t = if t == Ty::IntLit { Ty::scalar() } else { t };
                        match &ty {
                            None => ty = Some(t),
                            Some(prev) => {
                                if !compat(prev, &t) {
                                    self.err(Code::T, e.span(), format!("the `else` branch gives `{}` type {}, and the branches give {}", var, t, prev));
                                }
                            }
                        }
                    }
                }
                self.bind(var, ty, TimeProv::Other);
            }
        }
    }

    /// W7: a calendar window's `min K` beyond the bars its duration can
    /// hold on the rule's calendar.
    fn window_sanity(&mut self, dur: &Expr, min: &Expr, span: Span) {
        let d = match dur {
            Expr::Lit(Lit::Duration(d), _) => Some(*d),
            Expr::Param(p, _) => match self.cx.params.get(p).map(|p| &p.value) {
                Some(Lit::Duration(d)) => Some(*d),
                _ => None,
            },
            _ => None,
        };
        let k = match min {
            Expr::Lit(Lit::Int(k), _) => Some(*k),
            Expr::Param(p, _) => match self.cx.params.get(p).map(|p| &p.value) {
                Some(Lit::Int(k)) => Some(*k),
                _ => None,
            },
            _ => None,
        };
        let (Some(d), Some(k)) = (d, k) else { return };
        let bars = window_capacity(&self.cal, self.cx.base, &d);
        if k > bars {
            self.err(
                Code::W7,
                span,
                format!(
                    "`min {}` exceeds the {} bars a window of {} can hold on calendar `{}`; the aggregate never has enough rows (use `rows[{}]` for a count of the entity's rows)",
                    k,
                    bars,
                    d,
                    self.cx.cal_name(&self.cal),
                    k
                ),
            );
        }
    }

    // ---- atoms ----

    fn atom(&mut self, atom: &Atom, ctx: AtomCtx) -> Option<()> {
        let Some(sig) = self.cx.relations.get(&atom.name).cloned() else {
            for t in &atom.terms {
                if let Term::Var(v, _) = t {
                    if !self.is_bound(v) {
                        self.bind(v, None, TimeProv::Unknown);
                    }
                }
            }
            return None;
        };
        if atom.terms.len() != sig.args.len() {
            self.err(Code::T, atom.span, format!("`{}` takes {} arguments, {} given", atom.name, sig.args.len(), atom.terms.len()));
            return None;
        }
        let key_pos = sig.key_pos();
        let mut key_prov = TimeProv::Unknown;
        let negative = ctx == AtomCtx::Negative;
        // The key's provenance first: the Timestamp outputs' derives from it.
        let pre_key_prov: TimeProv = match key_pos.and_then(|k| atom.terms.get(k)) {
            Some(Term::Var(v, _)) => match self.vars.get(v) {
                Some(st) => st.prov,
                None => match ctx {
                    AtomCtx::AsOf(p) => p,
                    _ => TimeProv::Other,
                },
            },
            Some(Term::Wild(_)) => match ctx {
                AtomCtx::AsOf(p) => p,
                _ => TimeProv::Unknown,
            },
            _ => TimeProv::Unknown,
        };
        let out_prov: Vec<Option<TimeProv>> = self.cx.out_prov.get(&atom.name).cloned().unwrap_or_default();
        for (i, (arg, term)) in sig.args.iter().zip(&atom.terms).enumerate() {
            let is_key = Some(i) == key_pos;
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
                            // The calendar of the key against the relation's.
                            if let Some(c) = self.cals.get(v) {
                                if *c != sig.cal && !(c.is_sessions() && sig.cal.is_sessions()) && !matches!(ctx, AtomCtx::AsOf(_)) {
                                    self.err(
                                        Code::X,
                                        *sp,
                                        format!(
                                            "`{}` is on calendar `{}`, and its key `{}` here is on `{}`",
                                            atom.name,
                                            self.cx.cal_name(&sig.cal),
                                            v,
                                            self.cx.cal_name(c)
                                        ),
                                    );
                                }
                            }
                        }
                    } else if negative {
                        self.err(
                            Code::B,
                            *sp,
                            format!("variable `{}` in `not {}(...)` is unbound; a negated atom's variables are bound by positive literals", v, atom.name),
                        );
                        return None;
                    } else {
                        let prov = if is_key {
                            match ctx {
                                AtomCtx::AsOf(p) => p,
                                _ => TimeProv::Other,
                            }
                        } else if arg.ty == Ty::Timestamp {
                            Self::output_prov(pre_key_prov, out_prov.get(i).copied().flatten())
                        } else {
                            TimeProv::Other
                        };
                        self.bind(v, Some(arg.ty.clone()), prov);
                        if is_key {
                            key_prov = prov;
                        }
                    }
                }
                Term::Wild(sp) => {
                    if is_key && !matches!(ctx, AtomCtx::AsOf(_)) {
                        self.err(Code::F, *sp, format!("the temporal key of `{}` must be a variable derived from the head time, not `_`", atom.name));
                    }
                    if is_key {
                        if let AtomCtx::AsOf(p) = ctx {
                            key_prov = p;
                        }
                    }
                }
                Term::Lit(l, sp) => {
                    if is_key {
                        self.err(
                            Code::F,
                            *sp,
                            format!("the temporal key of `{}` must be a variable derived from the head time, not a literal", atom.name),
                        );
                    } else {
                        self.check_lit_type(&arg.ty, l, *sp, &format!("argument `{}` of `{}`", arg.name, atom.name));
                    }
                }
                Term::Param(p, sp) => {
                    if let Some(pt) = self.param_ty(p, *sp) {
                        if !compat(&arg.ty, &pt) {
                            self.err(Code::T, *sp, format!("argument `{}` of `{}` is {} but parameter `{}` is {}", arg.name, atom.name, arg.ty, p, pt));
                        }
                    }
                }
                Term::Ctor(c, subs, sp) => {
                    if arg.ty != Ty::Decision {
                        self.err(Code::T, *sp, format!("argument `{}` of `{}` is {}, not a decision", arg.name, atom.name, arg.ty));
                        continue;
                    }
                    self.ctor_pattern(c, subs, *sp, negative);
                }
            }
        }
        // WF-6: the key is derived from the head time.
        match key_prov {
            TimeProv::Other => {
                let key_var = key_pos.and_then(|k| atom.terms.get(k)).map(|t| t.to_string()).unwrap_or_default();
                let msg = if ctx == AtomCtx::InAgg {
                    format!(
                        "the group's temporal key `{}` of `{}` is unconstrained: the aggregate would run over the whole sample, including bars after the head time `{}`; every temporal key inside a group is the head's bar or bound by rows, window, prior_window, bucket or session of it",
                        key_var, atom.name, self.head_key
                    )
                } else {
                    format!(
                        "the temporal key `{}` of `{}` is not derived from the head time `{}`; a rule reads the past through prev, lag, rows, window, bucket or session of `{}`",
                        key_var, atom.name, self.head_key, self.head_key
                    )
                };
                self.err(Code::F, atom.span, msg);
            }
            TimeProv::Future => {
                let key_var = key_pos.and_then(|k| atom.terms.get(k)).map(|t| t.to_string()).unwrap_or_default();
                self.err(
                    Code::F,
                    atom.span,
                    format!(
                        "`{}` is the label of the bar's own session, which closes after the bar: it is a value, not a causal key, so `{}` cannot be read at it; read the previous session through `prev({}, S1)`",
                        key_var, atom.name, key_var
                    ),
                );
            }
            _ => {}
        }
        if atom.name == "decided" && !matches!(key_prov, TimeProv::Strict | TimeProv::Unknown | TimeProv::Other | TimeProv::Future) {
            self.err(
                Code::F,
                atom.span,
                format!(
                    "`decided` must be read strictly before the head time `{}`: at the rule's own bar the decision would see itself; bind its time through prev, lag or a prior window",
                    self.head_key
                ),
            );
        }
        self.refs.push(AtomRef {
            name: atom.name.clone(),
            polarity: match ctx {
                AtomCtx::Negative => Polarity::Negative,
                AtomCtx::InAgg => Polarity::Aggregate,
                _ => Polarity::Positive,
            },
            key_prov,
            span: atom.span,
            is_top: false,
            in_case_guard: false,
        });
        Some(())
    }

    /// The provenance of a Timestamp output bound by an atom read at a key
    /// of the given provenance: the relation's rules bind it at or before
    /// (`out` Causal) or before (Strict) the key, so it is derived from
    /// the head time through the key.
    fn output_prov(key: TimeProv, out: Option<TimeProv>) -> TimeProv {
        match (key, out) {
            (TimeProv::Unknown, _) => TimeProv::Unknown,
            (_, None) => TimeProv::Other,
            (TimeProv::Strict, Some(_)) => TimeProv::Strict,
            (TimeProv::Head | TimeProv::Causal, Some(TimeProv::Strict)) => TimeProv::Strict,
            (TimeProv::Head | TimeProv::Causal, Some(_)) => TimeProv::Causal,
            _ => TimeProv::Other,
        }
    }

    /// A decision pattern inside `decided(T0, hold(A, Q))`: binds the
    /// entity and the amount.
    fn ctor_pattern(&mut self, name: &str, subs: &[Term], span: Span, negative: bool) {
        if name != "hold" && name != "trade" {
            self.err(Code::T, span, format!("`{}` is not a decision constructor (hold, trade)", name));
            return;
        }
        if subs.len() < 2 || subs.len() > 3 {
            self.err(Code::T, span, format!("`{}` takes an instrument and an amount, with an optional order", name));
            return;
        }
        for (i, s) in subs.iter().enumerate().take(2) {
            let ty = if i == 0 {
                Some(Ty::Entity)
            } else if name == "trade" {
                Some(Ty::Quantity(Dim::shares()))
            } else {
                None
            };
            match s {
                Term::Var(v, sp) => {
                    if let Some(st) = self.vars.get(v).cloned() {
                        if let (Some(exp), Some(t)) = (&ty, &st.ty) {
                            if !compat(exp, t) {
                                self.err(Code::T, *sp, format!("field of `{}` is {} but `{}` is {}", name, exp, v, t));
                            }
                        }
                    } else if negative {
                        self.err(Code::B, *sp, format!("variable `{}` is unbound in `{}(...)` under a negation", v, name));
                    } else {
                        self.bind(v, ty.clone(), TimeProv::Other);
                    }
                }
                Term::Wild(_) => {}
                Term::Lit(l, sp) => {
                    if let Some(exp) = &ty {
                        self.check_lit_type(exp, l, *sp, &format!("field of `{}`", name));
                    }
                }
                Term::Param(p, sp) => {
                    if let (Some(pt), Some(exp)) = (self.param_ty(p, *sp), &ty) {
                        if !compat(exp, &pt) {
                            self.err(Code::T, *sp, format!("field of `{}` is {} but parameter `{}` is {}", name, exp, p, pt));
                        }
                    }
                }
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
                        format!("variable `{}` is unbound here; bind it with an atom, an assignment or a temporal builtin first", v),
                    );
                    None
                }
            },
            Expr::Param(p, sp) => self.param_ty(p, *sp),
            Expr::Lit(l, _) => Some(l.ty()),
            Expr::Neg(a, sp) => {
                let t = self.expr_ty(a)?;
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
                if !SCALAR_FUNCTIONS.contains(&f.as_str()) {
                    self.err(Code::U, *sp, format!("`{}` is not a function ({})", f, SCALAR_FUNCTIONS.join(", ")));
                    return None;
                }
                let mut tys = Vec::new();
                for a in args {
                    tys.push(self.expr_ty(a)?);
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

    fn entity_term(&mut self, t: &Term, what: &str) {
        match t {
            Term::Var(v, sp) => match self.vars.get(v).cloned() {
                Some(st) => {
                    if let Some(ty) = st.ty {
                        if !ty.is_entity() {
                            self.err(Code::T, *sp, format!("{} must be an instrument, but `{}` is {}", what, v, ty));
                        }
                    }
                }
                None => self.err(Code::B, *sp, format!("variable `{}` is unbound; {} must be bound", v, what)),
            },
            Term::Param(p, sp) => {
                if let Some(pt) = self.param_ty(p, *sp) {
                    if !pt.is_entity() {
                        self.err(Code::T, *sp, format!("{} must be an instrument, but parameter `{}` is {}", what, p, pt));
                    }
                }
            }
            Term::Lit(Lit::Str(_), sp) => self.resolve_str(*sp, &Ty::Entity),
            other => self.err(Code::T, other.span(), format!("{} must be an instrument", what)),
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
            _ => {
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
            _ => {
                self.err(Code::T, t.span(), format!("{} is bound by the builtin and must be a fresh variable, found `{}`", what, t));
                None
            }
        }
    }

    // ---- the head ----

    fn head(&mut self, sig: &Signature) {
        let head = &self.rule.head;
        if head.name == "decide" {
            self.decide_head(head);
            return;
        }
        // A body that failed a judgment bound its variables as unknown: an
        // unbound head variable then is that failure, reported once.
        let judged = !self.errors;
        for (arg, term) in sig.args.iter().zip(&head.terms) {
            match term {
                Term::Var(v, sp) => match self.vars.get(v).cloned() {
                    Some(st) => {
                        if let Some(t) = &st.ty {
                            if !compat(&arg.ty, t) {
                                self.err(Code::T, *sp, format!("head argument `{}` of `{}` is {} but `{}` is {}", arg.name, head.name, arg.ty, v, t));
                            }
                        }
                    }
                    None => {
                        if judged {
                            self.err(Code::B, *sp, format!("head variable `{}` is unbound: every head variable must be bound by the body", v));
                        }
                    }
                },
                Term::Wild(sp) => self.err(Code::B, *sp, "`_` is not permitted in a head"),
                Term::Lit(l, sp) => self.check_lit_type(&arg.ty, l, *sp, &format!("head argument `{}` of `{}`", arg.name, head.name)),
                Term::Param(p, sp) => {
                    if let Some(pt) = self.param_ty(p, *sp) {
                        if !compat(&arg.ty, &pt) {
                            self.err(Code::T, *sp, format!("head argument `{}` of `{}` is {} but parameter `{}` is {}", arg.name, head.name, arg.ty, p, pt));
                        }
                    }
                }
                Term::Ctor(_, _, sp) => self.err(Code::T, *sp, format!("head argument `{}` of `{}` is {}, not a decision", arg.name, head.name, arg.ty)),
            }
        }
    }

    /// `decide(T, hold(A, W))`: the constructor resolved by the amount's
    /// type, the order term admissible.
    fn decide_head(&mut self, head: &Atom) {
        let Some(Term::Ctor(c, subs, sp)) = head.terms.get(1) else {
            self.err(Code::T, head.span, "decide's second argument is a decision, `hold(A, W)` or `trade(A, Q)`");
            return;
        };
        match c.as_str() {
            "combo" => return self.combo_head(subs, *sp),
            "exercise" => return self.exercise_head(subs, *sp),
            "hold" | "trade" => {}
            _ => {
                self.err(
                    Code::T,
                    *sp,
                    format!(
                        "`{}` is not a decision constructor: write `hold(A, W)` (a target weight), `hold(A, Q)` (a target quantity), `trade(A, Q)` (a signed delta), `combo([leg(A1, Q1), leg(A2, Q2), ...])` (an all-or-none multi-leg delta) or `exercise(A, Q)` (an option exercised)",
                        c
                    ),
                );
                return;
            }
        }
        if subs.len() < 2 {
            self.err(Code::T, *sp, format!("`{}` takes an instrument and an amount", c));
            return;
        }
        self.entity_term(&subs[0], &format!("the instrument of `{}`", c));
        let amount_ty: Option<Ty> = match &subs[1] {
            Term::Var(v, vsp) => match self.vars.get(v).cloned() {
                Some(st) => st.ty,
                None => {
                    self.err(Code::B, *vsp, format!("variable `{}` is unbound in `{}(...)`", v, c));
                    return;
                }
            },
            Term::Param(p, psp) => self.param_ty(p, *psp),
            Term::Lit(l, _) => Some(l.ty()),
            other => {
                self.err(Code::T, other.span(), format!("the amount of `{}` must be a variable, a parameter or a literal", c));
                return;
            }
        };
        let Some(amount_ty) = amount_ty else { return };
        let resolved = match c.as_str() {
            "trade" => {
                if !compat(&Ty::Quantity(Dim::shares()), &amount_ty) {
                    let hint = self.typed_hint(&Ty::Quantity(Dim::shares()), &Lit::Int(100));
                    self.err(
                        Code::T,
                        subs[1].span(),
                        format!("the delta of `trade` is a quantity in the class's unit, and `{}` is {}{}", subs[1], amount_ty, hint),
                    );
                    return;
                }
                "trade"
            }
            _ => match &amount_ty {
                Ty::Quantity(d) if d.is_quantity() => "hold_quantity",
                Ty::Quantity(d) if d.is_scalar() => "hold_weight",
                Ty::IntLit | Ty::Count => "hold_weight",
                other => {
                    self.err(
                        Code::T,
                        subs[1].span(),
                        format!("the amount of `hold` is a Scalar weight of capital or a quantity in the class's unit, and `{}` is {}", subs[1], other),
                    );
                    return;
                }
            },
        };
        if let Some(order) = subs.get(2) {
            self.order_term(c, order);
        }
        if subs.len() > 3 {
            self.err(Code::T, *sp, format!("`{}` takes an instrument, an amount and an optional order", c));
        }
        self.head_ctor = Some(resolved.to_string());
    }

    /// The amount of a delta: a quantity in the class's unit.
    fn delta_amount(&mut self, ctor: &str, amount: &Term) {
        let ty: Option<Ty> = match amount {
            Term::Var(v, vsp) => match self.vars.get(v).cloned() {
                Some(st) => st.ty,
                None => {
                    self.err(Code::B, *vsp, format!("variable `{}` is unbound in `{}(...)`", v, ctor));
                    return;
                }
            },
            Term::Param(p, psp) => self.param_ty(p, *psp),
            Term::Lit(l, _) => Some(l.ty()),
            other => {
                self.err(Code::T, other.span(), format!("the amount of `{}` must be a variable, a parameter or a literal", ctor));
                return;
            }
        };
        if let Some(ty) = ty {
            if !compat(&Ty::Quantity(Dim::shares()), &ty) {
                let hint = self.typed_hint(&Ty::Quantity(Dim::shares()), &Lit::Int(100));
                self.err(
                    Code::T,
                    amount.span(),
                    format!("the delta of `{}` is a quantity in the class's unit, and `{}` is {}{}", ctor, amount, ty, hint),
                );
            }
        }
    }

    /// `combo([leg(A1, Q1), ..., leg(An, Qn)][, ORDER])` (K-2.3): at least two
    /// legs on distinct instruments, each a signed delta; the order's limit
    /// is the combo's net price.
    fn combo_head(&mut self, subs: &[Term], sp: Span) {
        let shape = "a combo is `combo([leg(A1, Q1), leg(A2, Q2), ...])` with an optional order";
        let legs = match subs.first() {
            Some(Term::Ctor(l, legs, _)) if l == crate::ir::LIST => legs.clone(),
            _ => {
                self.err(Code::T, sp, shape);
                return;
            }
        };
        if legs.len() < 2 {
            self.err(Code::T, sp, format!("a combo has at least two legs ({})", shape));
        }
        let mut seen: Vec<String> = Vec::new();
        for leg in &legs {
            match leg {
                Term::Ctor(l, args, lsp) if l == "leg" && args.len() == 2 => {
                    self.entity_term(&args[0], "the instrument of a leg");
                    let key = args[0].to_string();
                    if seen.contains(&key) {
                        self.err(
                            Code::T,
                            *lsp,
                            format!("two legs of a combo trade `{}`: the legs are distinct instruments (one decision per instrument per bar, F-5)", key),
                        );
                    }
                    seen.push(key);
                    self.delta_amount("leg", &args[1]);
                }
                other => self.err(Code::T, other.span(), format!("`{}` is not a leg: {}", other, shape)),
            }
        }
        if let Some(order) = subs.get(1) {
            self.order_term("combo", order);
        }
        if subs.len() > 2 {
            self.err(Code::T, sp, format!("{}: the legs and an optional order", shape));
        }
        self.head_ctor = Some("combo".to_string());
    }

    /// `exercise(A, Q)` (K-2.3, K-5.5.4): Q long contracts of an option
    /// exercised now; admitted only in the option class (T).
    fn exercise_head(&mut self, subs: &[Term], sp: Span) {
        let class = self.cx.class_name.clone();
        if class != "option" {
            self.err(
                Code::T,
                sp,
                format!("`exercise` takes an option, and this program's class is `{}`: an exercise is admitted only in the option class", class),
            );
            return;
        }
        if subs.len() != 2 {
            self.err(Code::T, sp, "`exercise` takes an option and a number of contracts, `exercise(A, Q)`");
            return;
        }
        self.entity_term(&subs[0], "the option of `exercise`");
        self.delta_amount("exercise", &subs[1]);
        self.head_ctor = Some("exercise".to_string());
    }

    fn order_term(&mut self, ctor: &str, term: &Term) {
        let shape = "an order is `market`, `moo`, `moc`, `moo_moc`, `limit(P[, TIF])` or `stop(P[, TIF])`";
        match term {
            Term::Param(p, sp) => {
                if !ORDER_KINDS.contains(&p.as_str()) {
                    self.err(Code::T, *sp, format!("`{}` in `{}(...)` is not an order; {}", p, ctor, shape));
                }
            }
            Term::Ctor(c, args, sp) if c == "limit" || c == "stop" => {
                if args.is_empty() || args.len() > 2 {
                    self.err(Code::T, *sp, format!("`{}` takes a price and an optional time in force; {}", c, shape));
                    return;
                }
                let price = Ty::Quantity(Dim::price("USD"));
                match &args[0] {
                    Term::Var(v, vsp) => match self.vars.get(v).cloned() {
                        Some(st) => {
                            if let Some(t) = st.ty {
                                if !matches!(&t, Ty::Quantity(d) if d.is_price()) {
                                    self.err(Code::T, *vsp, format!("the price of `{}` in `{}(...)` is {}, not a Price", c, ctor, t));
                                }
                            }
                        }
                        None => self.err(Code::B, *vsp, format!("variable `{}` is unbound in `{}(...)`", v, c)),
                    },
                    Term::Param(p, psp) => {
                        if let Some(pt) = self.param_ty(p, *psp) {
                            if !compat(&price, &pt) {
                                self.err(Code::T, *psp, format!("the price of `{}` is {} but parameter `{}` is {}", c, price, p, pt));
                            }
                        }
                    }
                    Term::Lit(l, lsp) => self.check_lit_type(&price, l, *lsp, &format!("the price of `{}`", c)),
                    other => self.err(Code::T, other.span(), format!("the price of `{}` must be a variable, a parameter or a literal", c)),
                }
                match args.get(1) {
                    None => {}
                    Some(Term::Param(p, _)) if p == "day" || p == "gtc" => {}
                    Some(Term::Ctor(b, bargs, bsp)) if b == "bars" => match bargs.first() {
                        Some(Term::Lit(Lit::Int(_), _)) => {}
                        Some(Term::Param(p, psp)) => {
                            if let Some(pt) = self.param_ty(p, *psp) {
                                if pt != Ty::Count {
                                    self.err(Code::T, *psp, format!("`bars({})` takes a Count, and `{}` is {}", p, p, pt));
                                }
                            }
                        }
                        _ => self.err(Code::T, *bsp, "`bars(N)` takes a Count literal or parameter"),
                    },
                    Some(other) => self.err(Code::T, other.span(), format!("`{}` is not a time in force (`day`, `gtc`, `bars(N)`)", other)),
                }
            }
            other => self.err(Code::T, other.span(), format!("`{}` in `{}(...)`: {}", other, ctor, shape)),
        }
    }
}

/// Two rules of one relation are complementary (WF-11) when one holds a
/// literal the other negates: an atom against `not` of it on the same
/// identity, a comparison against its negation, an atom of a relation whose
/// single rule negates an atom the other rule holds, or the recursion's
/// common guard against its negation at the recursive bar.
/// A rule's positive atoms, negated atoms and comparisons over variables.
type RuleAtoms = (Vec<Atom>, Vec<Atom>, Vec<(CmpOp, String, String)>);

pub(crate) fn complementary(cx: &Checker, a: &Rule, b: &Rule) -> bool {
    let atoms = |r: &Rule| -> RuleAtoms {
        let mut pos = Vec::new();
        let mut neg = Vec::new();
        let mut cmps = Vec::new();
        for l in &r.body {
            match l {
                Literal::Atom(x) | Literal::AsOf { atom: x, .. } => pos.push(x.clone()),
                Literal::Neg(x) => neg.push(x.clone()),
                Literal::Cmp { op, lhs, rhs, .. } => cmps.push((*op, lhs.to_string(), rhs.to_string())),
                _ => {}
            }
        }
        (pos, neg, cmps)
    };
    let same_identity = |x: &Atom, y: &Atom| -> bool {
        if x.name != y.name || x.terms.len() != y.terms.len() {
            return false;
        }
        let Some(sig) = cx.relations.get(&x.name) else { return false };
        sig.identity_positions().iter().all(|&i| match (&x.terms[i], &y.terms[i]) {
            (Term::Var(p, _), Term::Var(q, _)) => p == q,
            (Term::Lit(p, _), Term::Lit(q, _)) => p == q,
            (Term::Param(p, _), Term::Param(q, _)) => p == q,
            _ => false,
        })
    };
    let (pa, na, ca) = atoms(a);
    let (pb, nb, cb) = atoms(b);
    // (a) an atom against its negation.
    if pa.iter().any(|x| nb.iter().any(|y| same_identity(x, y))) || pb.iter().any(|x| na.iter().any(|y| same_identity(x, y))) {
        return true;
    }
    // (b) a comparison against its negation.
    for (op, l, r) in &ca {
        if cb.iter().any(|(op2, l2, r2)| l2 == l && r2 == r && *op2 == op.negate() && *op != CmpOp::Eq) {
            return true;
        }
    }
    // (c) an atom of a single-rule relation whose body negates an atom the other holds.
    let unfold = |pos: &[Atom], other_pos: &[Atom]| -> bool {
        for x in pos {
            let rules: Vec<&Rule> = cx.rules.iter().filter(|r| r.head.name == x.name).collect();
            if rules.len() != 1 {
                continue;
            }
            let r = rules[0];
            // Map the rule's head variables to the atom's terms.
            let map: HashMap<String, Term> = r
                .head
                .terms
                .iter()
                .zip(&x.terms)
                .filter_map(|(h, t)| if let Term::Var(v, _) = h { Some((v.clone(), t.clone())) } else { None })
                .collect();
            for l in &r.body {
                if let Literal::Neg(n) = l {
                    let renamed = Atom {
                        name: n.name.clone(),
                        terms: n
                            .terms
                            .iter()
                            .map(|t| if let Term::Var(v, _) = t { map.get(v).cloned().unwrap_or(t.clone()) } else { t.clone() })
                            .collect(),
                        span: n.span,
                    };
                    if other_pos.iter().any(|y| same_identity(&renamed, y)) {
                        return true;
                    }
                }
            }
        }
        false
    };
    if unfold(&pa, &pb) || unfold(&pb, &pa) {
        return true;
    }
    // (d) the recursion's guard: every rule of the head holds G at the head
    // time; one rule reads the head at T0 and the other `not G` at T0.
    let head = &a.head.name;
    let all_rules: Vec<&Rule> = cx.rules.iter().filter(|r| r.head.name == *head).collect();
    let key_pos = cx.relations.get(head).and_then(|s| s.key_pos()).unwrap_or(1);
    let head_key = |r: &Rule| match r.head.terms.get(key_pos) {
        Some(Term::Var(v, _)) => v.clone(),
        _ => String::new(),
    };
    let common: Vec<Atom> = atoms(all_rules[0])
        .0
        .into_iter()
        .filter(|g| {
            let hk = head_key(all_rules[0]);
            let gsig = cx.relations.get(&g.name);
            let at_head = gsig
                .and_then(|s| s.key_pos())
                .and_then(|k| g.terms.get(k))
                .map(|t| matches!(t, Term::Var(v, _) if *v == hk))
                .unwrap_or(false);
            at_head && all_rules.iter().all(|r| atoms(r).0.iter().any(|y| y.name == g.name))
        })
        .collect();
    let guard_pair = |rec: &Rule, neg_rule: &Rule| -> bool {
        let (rp, _, _) = atoms(rec);
        let (_, nn, _) = atoms(neg_rule);
        for x in rp.iter().filter(|x| x.name == *head) {
            let Some(Term::Var(t0, _)) = x.terms.get(key_pos) else { continue };
            for g in &common {
                let Some(gk) = cx.relations.get(&g.name).and_then(|s| s.key_pos()) else { continue };
                if nn.iter().any(|n| n.name == g.name && matches!(n.terms.get(gk), Some(Term::Var(v, _)) if v == t0)) {
                    return true;
                }
            }
        }
        false
    };
    if guard_pair(a, b) || guard_pair(b, a) {
        return true;
    }
    // A single case form on one side is keyed by construction against an
    // otherwise disjoint base case.
    false
}

#[allow(dead_code)]
fn _unused(_: &HashSet<String>) {}

/// The bars a calendar window of duration `d` can hold on calendar `cal`
/// over a base of resolution `base`: what W7 judges a window's `min K`
/// against, on the defaults here and on a run's parameter overrides in the
/// kernel.
pub fn window_capacity(cal: &Cal, base: Resolution, d: &Duration) -> i64 {
    use crate::kernel::time::DAY;
    let bar: i64 = match cal {
        Cal::Base => base.nanos().unwrap_or(DAY),
        Cal::Days | Cal::Sessions | Cal::SessionsOf(_) => DAY,
        Cal::Weeks(n) => *n as i64 * 7 * DAY,
        Cal::Months(n) => *n as i64 * 28 * DAY,
        Cal::Years(n) => *n as i64 * 365 * DAY,
    };
    d.max_nanos() / bar.max(1)
}
