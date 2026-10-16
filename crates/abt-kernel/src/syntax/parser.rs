//! Recursive-descent parser from the surface syntax of the second
//! formulation to the AST. The parser never reorders literals; the written
//! order is semantically void (Proposition 5.5) and kept for the reader.

use super::ast::*;
use super::lexer::{lex, LexError, Tok, Token};
use crate::ir::{BinOp, Cal, CmpOp, Dir, Duration, Expr, Lit, Resolution, Span, Term, Ties, AGGREGATES, SCALAR_FUNCTIONS};

#[derive(Clone, Debug)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.span, self.message)
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError { span: e.span, message: e.message }
    }
}

type PResult<T> = Result<T, ParseError>;

const QUANTITY_WORDS: &[&str] = &["shares", "contracts", "units"];
const PER_UNIT_WORDS: &[&str] = &["share", "contract", "unit"];
const EXEC_FIELDS: &[&str] = &[
    "price",
    "orders",
    "lot",
    "capital",
    "commission",
    "slippage",
    "impact",
    "participation",
    "financing",
    "leverage",
    "actions",
    "delisting",
    "oversize",
    "ruin",
    "numerics",
];

pub fn parse_units(src: &str) -> PResult<Vec<Unit>> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let mut units = Vec::new();
    while !p.at(&Tok::Eof) {
        units.push(p.unit()?);
    }
    Ok(units)
}

/// Parse one literal value on its own (`100 shares`, `20d`, `0.02`, `"SPY"`,
/// `2000-01-01`), as the command line takes parameter overrides.
pub fn parse_lit(src: &str) -> PResult<Lit> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let lit = p.lit_expr()?;
    if !p.at(&Tok::Eof) {
        return Err(ParseError {
            span: p.span(),
            message: format!("unexpected {} after the literal", p.peek()),
        });
    }
    Ok(lit)
}

/// Parse a rule body on its own (the query facility's `? R(...)` forms).
pub fn parse_body(src: &str) -> PResult<Vec<Literal>> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let body = p.body_until(&[Tok::Eof, Tok::Dot])?;
    Ok(body)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

fn is_var(name: &str) -> bool {
    name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn is_currency(name: &str) -> bool {
    name.len() == 3 && name.chars().all(|c| c.is_ascii_uppercase())
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, k: usize) -> &Tok {
        let i = (self.pos + k).min(self.toks.len() - 1);
        &self.toks[i].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn at_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s == kw)
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            span: self.span(),
            message: msg.into(),
        })
    }
    fn expect(&mut self, t: Tok) -> PResult<Span> {
        if self.at(&t) {
            Ok(self.bump().span)
        } else {
            self.err(format!("expected {}, found {}", t, self.peek()))
        }
    }
    fn expect_kw(&mut self, kw: &str) -> PResult<Span> {
        if self.at_kw(kw) {
            Ok(self.bump().span)
        } else {
            self.err(format!("expected `{}`, found {}", kw, self.peek()))
        }
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.at(t) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                let sp = self.bump().span;
                Ok((s, sp))
            }
            t => self.err(format!("expected a name, found {}", t)),
        }
    }
    fn var(&mut self) -> PResult<(String, Span)> {
        let (s, sp) = self.ident()?;
        if !is_var(&s) {
            return Err(ParseError {
                span: sp,
                message: format!("expected a variable (an uppercase name), found `{}`", s),
            });
        }
        Ok((s, sp))
    }
    fn string(&mut self) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Str(s) => {
                let sp = self.bump().span;
                Ok((s, sp))
            }
            t => self.err(format!("expected a string literal, found {}", t)),
        }
    }
    fn number(&mut self) -> PResult<(f64, Span)> {
        let neg = self.eat(&Tok::Minus);
        let (x, sp) = match self.peek().clone() {
            Tok::Int(i) => (i as f64, self.bump().span),
            Tok::Float(x) => (x, self.bump().span),
            t => return self.err(format!("expected a number, found {}", t)),
        };
        Ok((if neg { -x } else { x }, sp))
    }
    fn uint(&mut self) -> PResult<(u32, Span)> {
        match self.peek().clone() {
            Tok::Int(i) if i >= 0 => Ok((i as u32, self.bump().span)),
            t => self.err(format!("expected a whole number, found {}", t)),
        }
    }
    fn date(&mut self) -> PResult<(i64, Span)> {
        match self.peek().clone() {
            Tok::Date(t) => Ok((t, self.bump().span)),
            t => self.err(format!("expected a date (YYYY-MM-DD), found {}", t)),
        }
    }

    // ---- units ----

    fn unit(&mut self) -> PResult<Unit> {
        let span = self.span();
        Ok(if self.eat_kw("strategy") {
            Unit::Strategy(self.strategy(span)?)
        } else if self.eat_kw("library") {
            Unit::Library(self.library(span)?)
        } else if self.eat_kw("class") {
            Unit::Class(self.class(span)?)
        } else if self.eat_kw("execution") {
            let (name, _) = self.ident()?;
            self.expect(Tok::LBrace)?;
            let fields = self.exec_fields()?;
            self.expect(Tok::RBrace)?;
            Unit::Execution(ExecutionPreset { name, span, fields })
        } else {
            return self.err(format!("expected `strategy`, `library`, `class` or `execution`, found {}", self.peek()));
        })
    }

    fn strategy(&mut self, span: Span) -> PResult<Strategy> {
        let (name, _) = self.ident()?;
        let mut s = Strategy { name, span, ..Default::default() };
        self.expect(Tok::LBrace)?;
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            let (kw, _) = match self.peek().clone() {
                Tok::Ident(k) => (k, sp),
                t => return self.err(format!("expected a block name, found {}", t)),
            };
            self.bump();
            s.blocks.push((kw.clone(), sp));
            match kw.as_str() {
                "data" => s.data = Some(self.data_block(sp)?),
                "uses" => s.uses.extend(self.uses_list()?),
                "calendars" => s.calendars = Some(self.calendars_block(sp)?),
                "clock" => s.clock = Some(self.clock_block(sp)?),
                "universe" => s.universe = Some(self.universe_block(sp)?),
                "params" => s.params = Some(self.params_block(sp)?),
                "rules" => s.rules = Some(self.rules_block(sp)?),
                "decisions" => s.decisions = Some(self.decisions_block(sp)?),
                "execution" => s.execution = Some(self.execution_block(sp)?),
                "report" => s.report = Some(self.report_block(sp)?),
                _ => {
                    return Err(ParseError {
                        span: sp,
                        message: format!(
                            "`{}` is not a block of a strategy (data, uses, calendars, clock, universe, params, rules, decisions, execution, report)",
                            kw
                        ),
                    })
                }
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(s)
    }

    fn library(&mut self, span: Span) -> PResult<Library> {
        let (name, _) = self.ident()?;
        self.expect(Tok::LBrace)?;
        let mut data = None;
        let mut uses = Vec::new();
        let mut rules = None;
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            if self.eat_kw("data") {
                data = Some(self.data_block(sp)?);
            } else if self.eat_kw("uses") {
                uses.extend(self.uses_list()?);
            } else if self.eat_kw("rules") {
                rules = Some(self.rules_block(sp)?);
            } else {
                return self.err(format!("expected `data`, `uses` or `rules` in a library, found {}", self.peek()));
            }
        }
        self.expect(Tok::RBrace)?;
        let rules = rules.unwrap_or(RulesBlock {
            span,
            rels: vec![],
            rules: vec![],
            observes: vec![],
        });
        Ok(Library { name, span, data, uses, rules })
    }

    fn class(&mut self, span: Span) -> PResult<Class> {
        let (name, _) = self.ident()?;
        self.expect(Tok::LBrace)?;
        let mut c = Class {
            name,
            span,
            base: None,
            unit: None,
            rels: vec![],
            vocab: vec![],
        };
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            if self.eat_kw("base") {
                match self.peek().clone() {
                    Tok::Res(r) => {
                        self.bump();
                        c.base = Some(r);
                    }
                    t => return self.err(format!("expected a resolution after `base`, found {}", t)),
                }
            } else if self.eat_kw("unit") {
                c.unit = Some(self.ident()?.0);
            } else if self.at_kw("lists") || self.at_kw("reasons") || self.at_kw("schemes") || self.at_kw("names") {
                let (kind, _) = self.ident()?;
                let mut items = Vec::new();
                loop {
                    let (s, _) = self.string()?;
                    let res = match self.peek().clone() {
                        Tok::Res(r) => {
                            self.bump();
                            Some(r)
                        }
                        _ => None,
                    };
                    items.push((s, res));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                c.vocab.push((kind, items, sp));
            } else {
                c.rels.push(self.rel_signature(false)?);
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(c)
    }

    fn uses_list(&mut self) -> PResult<Vec<(String, Span)>> {
        let mut out = vec![self.ident()?];
        while self.eat(&Tok::Comma) {
            out.push(self.ident()?);
        }
        Ok(out)
    }

    // ---- data ----

    fn data_block(&mut self, span: Span) -> PResult<DataBlock> {
        self.expect(Tok::LBrace)?;
        let mut d = DataBlock {
            span,
            class: None,
            needs: vec![],
            require: None,
            prefer: None,
            budget: None,
        };
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            if self.eat_kw("class") {
                d.class = Some(self.ident()?);
            } else if self.eat_kw("need") {
                let mut items: Vec<(NeedKind, Span)> = Vec::new();
                loop {
                    let isp = self.span();
                    let kind = if self.eat_kw("bars") {
                        let res = match self.peek().clone() {
                            Tok::Res(r) => {
                                self.bump();
                                Some(r)
                            }
                            _ => None,
                        };
                        let from = if self.eat_kw("from") { Some(self.date()?.0) } else { None };
                        NeedKind::Bars { res, from }
                    } else if self.eat_kw("actions") {
                        NeedKind::Actions
                    } else if self.eat_kw("listing") {
                        NeedKind::Listing
                    } else if self.eat_kw("sessions") {
                        NeedKind::Sessions
                    } else if self.eat_kw("point_value") {
                        NeedKind::PointValue
                    } else if self.eat_kw("commission") {
                        NeedKind::Commission
                    } else if self.eat_kw("lists") {
                        let mut names = vec![self.string()?.0];
                        while self.at(&Tok::Comma) && matches!(self.peek_at(1), Tok::Str(_)) {
                            self.bump();
                            names.push(self.string()?.0);
                        }
                        NeedKind::Lists(names)
                    } else if self.eat_kw("series") {
                        let mut items = Vec::new();
                        loop {
                            let (s, _) = self.string()?;
                            let res = match self.peek().clone() {
                                Tok::Res(r) => {
                                    self.bump();
                                    Some(r)
                                }
                                _ => None,
                            };
                            items.push((s, res));
                            if self.at(&Tok::Comma) && matches!(self.peek_at(1), Tok::Str(_)) {
                                self.bump();
                                continue;
                            }
                            break;
                        }
                        NeedKind::Series(items)
                    } else if self.eat_kw("chain") {
                        NeedKind::Chain(self.chain_need()?)
                    } else if self.eat_kw("instruments") {
                        self.expect_kw("where")?;
                        let (attr, _) = self.ident()?;
                        self.expect(Tok::Eq)?;
                        let value = match self.peek().clone() {
                            Tok::Ident(s) | Tok::Str(s) => {
                                self.bump();
                                s
                            }
                            t => return self.err(format!("expected an attribute value, found {}", t)),
                        };
                        NeedKind::Instruments { attr, value }
                    } else {
                        return self.err(format!(
                            "expected a need (bars, actions, listing, lists, series, sessions, point_value, commission, instruments, chain), found {}",
                            self.peek()
                        ));
                    };
                    items.push((kind, isp));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                let complete = self.eat_kw("complete");
                for (kind, isp) in items {
                    d.needs.push(Need { kind, complete, span: isp });
                }
            } else if self.eat_kw("require") {
                if self.at_kw("tested") {
                    return self.err("`require reviewed` is now `require reviewed`: bundle tests became the `reviewed` attribute of a version (docs/spec/02-kernel.md, 2.1)");
                }
                if !self.eat_kw("reviewed") {
                    return self.err("expected `reviewed` after `require`");
                }
                d.require = Some((Require::Reviewed, sp));
            } else if self.eat_kw("allow") {
                if self.at_kw("untested") {
                    return self.err("`allow unreviewed` is now `allow unreviewed`: bundle tests became the `reviewed` attribute of a version (docs/spec/02-kernel.md, 2.1)");
                }
                self.expect_kw("unreviewed")?;
                d.require = Some((Require::AllowUnreviewed, sp));
            } else if self.eat_kw("budget") {
                if d.budget.is_some() {
                    return self.err("the data block has one `budget`");
                }
                d.budget = Some((self.budget()?, sp));
            } else if self.eat_kw("prefer") {
                let p = if self.eat_kw("latest") {
                    Prefer::Latest
                } else if self.eat_kw("version") {
                    let v = match self.peek().clone() {
                        // A bare version keeps its spelling: `2026.10` is not 2026.1.
                        Tok::Float(x) => {
                            let raw = self.toks[self.pos].raw.clone();
                            self.bump();
                            raw.unwrap_or_else(|| format!("{}", x))
                        }
                        Tok::Int(i) => {
                            let raw = self.toks[self.pos].raw.clone();
                            self.bump();
                            raw.unwrap_or_else(|| format!("{}", i))
                        }
                        Tok::Ident(s) | Tok::Str(s) => {
                            self.bump();
                            s
                        }
                        Tok::Date(t) => {
                            self.bump();
                            crate::kernel::time::format_timestamp(t)
                        }
                        t => return self.err(format!("expected a version, found {}", t)),
                    };
                    Prefer::Version(v)
                } else if self.eat_kw("base") {
                    self.expect_kw("finest")?;
                    Prefer::BaseFinest
                } else {
                    return self.err("expected `latest`, `version V` or `base finest` after `prefer`");
                };
                d.prefer = Some((p, sp));
            } else {
                return self.err(format!("expected `class`, `need`, `require`, `allow`, `prefer` or `budget` in a data block, found {}", self.peek()));
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(d)
    }

    /// `of "U" within D1..D2 and M1..M2 [right call|put] [spot RELATION]`.
    fn chain_need(&mut self) -> PResult<ChainNeed> {
        self.expect_kw("of")?;
        let (underlying, _) = self.string()?;
        self.expect_kw("within")?;
        let duration = |p: &mut Parser| -> PResult<Duration> {
            match p.peek().clone() {
                Tok::Duration(d) => {
                    p.bump();
                    Ok(d)
                }
                t => p.err(format!("expected a duration from the bar to the expiry (`20d`, `3mo`), found {}", t)),
            }
        };
        let lo = duration(self)?;
        self.expect(Tok::DotDot)?;
        let hi = duration(self)?;
        self.expect_kw("and")?;
        let (m_lo, _) = self.number()?;
        self.expect(Tok::DotDot)?;
        let (m_hi, _) = self.number()?;
        let right = if self.eat_kw("right") {
            let (r, rsp) = self.ident()?;
            if r != "call" && r != "put" {
                return Err(ParseError {
                    span: rsp,
                    message: format!("a chain's right is `call` or `put`, not `{}`", r),
                });
            }
            Some(r)
        } else {
            None
        };
        let spot = if self.eat_kw("spot") { Some(self.ident()?.0) } else { None };
        Ok(ChainNeed {
            underlying,
            expiry: (lo, hi),
            moneyness: (m_lo, m_hi),
            right,
            spot,
        })
    }

    /// `{ money X [CUR]; bytes N [B|KB|MB|GB|TB]; requests N; seconds S }`.
    fn budget(&mut self) -> PResult<Budget> {
        self.expect(Tok::LBrace)?;
        let mut b = Budget::default();
        while !self.at(&Tok::RBrace) {
            let (field, fsp) = self.ident()?;
            let twice = |set: bool| -> PResult<()> {
                if set {
                    Err(ParseError {
                        span: fsp,
                        message: format!("`{}` is written twice in the budget", field),
                    })
                } else {
                    Ok(())
                }
            };
            match field.as_str() {
                "money" => {
                    twice(b.money.is_some())?;
                    let (x, _) = self.number()?;
                    // A currency word may follow the amount.
                    if let Tok::Ident(c) = self.peek().clone() {
                        if c.len() == 3 && c.chars().all(|ch| ch.is_ascii_uppercase()) {
                            self.bump();
                        }
                    }
                    b.money = Some(x);
                }
                "bytes" => {
                    twice(b.bytes.is_some())?;
                    let (x, xsp) = self.number()?;
                    let scale = match self.peek().clone() {
                        Tok::Ident(u) => {
                            let s = match u.as_str() {
                                "B" => 1.0,
                                "KB" => 1e3,
                                "MB" => 1e6,
                                "GB" => 1e9,
                                "TB" => 1e12,
                                _ => 0.0,
                            };
                            if s > 0.0 {
                                self.bump();
                                s
                            } else {
                                1.0
                            }
                        }
                        _ => 1.0,
                    };
                    if x < 0.0 {
                        return Err(ParseError {
                            span: xsp,
                            message: "a budget of bytes is not negative".into(),
                        });
                    }
                    b.bytes = Some((x * scale).round() as u64);
                }
                "requests" => {
                    twice(b.requests.is_some())?;
                    b.requests = Some(self.uint()?.0 as u64);
                }
                "seconds" => {
                    twice(b.seconds.is_some())?;
                    b.seconds = Some(self.number()?.0);
                }
                other => {
                    return Err(ParseError {
                        span: fsp,
                        message: format!("`{}` is not a budget (money, bytes, requests, seconds)", other),
                    })
                }
            }
            if b.money.is_some_and(|m| m < 0.0) || b.seconds.is_some_and(|x| x < 0.0) {
                return Err(ParseError {
                    span: fsp,
                    message: format!("a budget of {} is not negative", field),
                });
            }
            self.eat(&Tok::Semi);
        }
        self.expect(Tok::RBrace)?;
        Ok(b)
    }

    // ---- calendars, clock ----

    fn cal_expr(&mut self) -> PResult<Cal> {
        let (name, sp) = self.ident()?;
        let count = |p: &mut Parser| -> PResult<u32> {
            p.expect(Tok::LParen)?;
            let (n, _) = p.uint()?;
            p.expect(Tok::RParen)?;
            Ok(n)
        };
        Ok(match name.as_str() {
            "base" => Cal::Base,
            "days" => Cal::Days,
            "weeks" => Cal::Weeks(count(self)?),
            "months" => Cal::Months(count(self)?),
            "years" => Cal::Years(count(self)?),
            "sessions" => {
                if self.eat_kw("of") {
                    Cal::SessionsOf(self.string()?.0)
                } else {
                    Cal::Sessions
                }
            }
            _ => {
                return Err(ParseError {
                    span: sp,
                    message: format!("`{}` is not a calendar constructor (base, days, weeks(n), months(n), years(n), sessions, sessions of \"ROOT\")", name),
                })
            }
        })
    }

    fn calendars_block(&mut self, span: Span) -> PResult<CalendarsBlock> {
        self.expect(Tok::LBrace)?;
        let mut decls = Vec::new();
        while !self.at(&Tok::RBrace) {
            let (name, sp) = self.ident()?;
            self.expect(Tok::Eq)?;
            let cal = self.cal_expr()?;
            decls.push((name, cal, sp));
        }
        self.expect(Tok::RBrace)?;
        Ok(CalendarsBlock { span, decls })
    }

    fn clock_block(&mut self, span: Span) -> PResult<ClockBlock> {
        self.expect(Tok::LBrace)?;
        self.expect_kw("decide")?;
        self.expect_kw("on")?;
        let on = self.ident()?;
        let mut offset = None;
        if self.eat_kw("at") {
            let sp = self.span();
            self.expect_kw("session")?;
            self.expect(Tok::Plus)?;
            let d = match self.peek().clone() {
                Tok::Duration(d) => {
                    self.bump();
                    d
                }
                t => return self.err(format!("expected a duration after `session +`, found {}", t)),
            };
            offset = Some((Lit::Duration(d), sp));
        }
        self.expect(Tok::RBrace)?;
        Ok(ClockBlock { span, on, offset })
    }

    // ---- universe, params ----

    fn universe_block(&mut self, span: Span) -> PResult<UniverseBlock> {
        self.expect(Tok::LBrace)?;
        let mut instruments = None;
        let mut rules = Vec::new();
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            if self.at_kw("instruments") && self.peek_at(1) == &Tok::LBrace {
                self.bump();
                self.bump();
                let mut rows = Vec::new();
                while !self.at(&Tok::RBrace) {
                    let (name, isp) = self.ident()?;
                    let from = if self.eat_kw("from") { Some(self.date()?.0) } else { None };
                    rows.push((name, from, isp));
                    self.eat(&Tok::Semi);
                }
                self.expect(Tok::RBrace)?;
                instruments = Some((rows, sp));
            } else {
                rules.push(self.rule()?);
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(UniverseBlock { span, instruments, rules })
    }

    fn params_block(&mut self, span: Span) -> PResult<ParamsBlock> {
        self.expect(Tok::LBrace)?;
        let mut params = Vec::new();
        while !self.at(&Tok::RBrace) {
            let (name, sp) = self.ident()?;
            self.expect(Tok::Colon)?;
            let ty = self.ty_name()?;
            self.expect(Tok::Eq)?;
            let value = self.lit_expr()?;
            let range = if self.eat_kw("in") {
                let lo = self.lit_expr()?;
                self.expect(Tok::DotDot)?;
                let hi = self.lit_expr()?;
                Some((lo, hi))
            } else {
                None
            };
            params.push(ParamDecl { name, ty, value, range, span: sp });
        }
        self.expect(Tok::RBrace)?;
        Ok(ParamsBlock { span, params })
    }

    fn ty_name(&mut self) -> PResult<TyName> {
        let (name, span) = self.ident()?;
        let arg = if self.eat(&Tok::Lt) {
            let (a, _) = self.ident()?;
            self.expect(Tok::Gt)?;
            Some(a)
        } else {
            None
        };
        Ok(TyName { name, arg, span })
    }

    /// A literal, possibly a quotient or product of two numbers (`1/3`), or
    /// negated.
    fn lit_expr(&mut self) -> PResult<Lit> {
        let neg = self.eat(&Tok::Minus);
        let mut lit = self.lit()?;
        if neg {
            lit = match lit {
                Lit::Int(i) => Lit::Int(-i),
                Lit::Float(x) => Lit::Float(-x),
                Lit::Quantity(x, u) => Lit::Quantity(-x, u),
                Lit::Money(x, c) => Lit::Money(-x, c),
                Lit::Price(x, c) => Lit::Price(-x, c),
                other => return self.err(format!("`-` applies to a number, not to {}", other)),
            };
        }
        while self.at(&Tok::Slash) || self.at(&Tok::Star) {
            let div = self.at(&Tok::Slash);
            self.bump();
            let (y, _) = self.number()?;
            let x = lit.magnitude().ok_or_else(|| ParseError {
                span: self.span(),
                message: "a literal expression combines numbers only".into(),
            })?;
            let z = if div { x / y } else { x * y };
            lit = match lit {
                Lit::Quantity(_, u) => Lit::Quantity(z, u),
                Lit::Money(_, c) => Lit::Money(z, c),
                Lit::Price(_, c) => Lit::Price(z, c),
                _ => Lit::Float(z),
            };
        }
        Ok(lit)
    }

    /// A literal with its unit: `100 shares`, `2 contracts`, `60 USD/share`,
    /// `1_000_000 USD`, `20d`, `0.02`, `"SPY"`, `2000-01-01`.
    fn lit(&mut self) -> PResult<Lit> {
        match self.peek().clone() {
            Tok::Int(i) => {
                self.bump();
                self.unit_suffix(i as f64, Lit::Int(i))
            }
            Tok::Float(x) => {
                self.bump();
                self.unit_suffix(x, Lit::Float(x))
            }
            Tok::Duration(d) => {
                self.bump();
                Ok(Lit::Duration(d))
            }
            Tok::Date(t) => {
                self.bump();
                Ok(Lit::Time(t))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Lit::Str(s))
            }
            t => self.err(format!("expected a literal, found {}", t)),
        }
    }

    fn unit_suffix(&mut self, x: f64, plain: Lit) -> PResult<Lit> {
        if let Tok::Ident(w) = self.peek().clone() {
            if QUANTITY_WORDS.contains(&w.as_str()) {
                self.bump();
                return Ok(Lit::Quantity(x, w));
            }
            if is_currency(&w) {
                self.bump();
                if self.at(&Tok::Slash) {
                    if let Tok::Ident(u) = self.peek_at(1).clone() {
                        if PER_UNIT_WORDS.contains(&u.as_str()) {
                            self.bump();
                            self.bump();
                            return Ok(Lit::Price(x, w));
                        }
                    }
                }
                return Ok(Lit::Money(x, w));
            }
        }
        Ok(plain)
    }

    // ---- rules ----

    fn rules_block(&mut self, span: Span) -> PResult<RulesBlock> {
        self.expect(Tok::LBrace)?;
        let mut b = RulesBlock {
            span,
            rels: vec![],
            rules: vec![],
            observes: vec![],
        };
        while !self.at(&Tok::RBrace) {
            if self.at_kw("rel") {
                self.bump();
                b.rels.push(self.rel_signature(true)?);
            } else if self.at_kw("observe") {
                let sp = self.bump().span;
                let (rel, _) = self.ident()?;
                let mut wheres = Vec::new();
                if self.eat_kw("where") {
                    loop {
                        let (v, vsp) = self.var()?;
                        self.expect(Tok::Eq)?;
                        let l = self.lit_expr()?;
                        wheres.push((v, l, vsp));
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                }
                let trace = if self.eat_kw("trace") { Some(self.ident()?.0) } else { None };
                b.observes.push(Observe { rel, wheres, trace, span: sp });
            } else {
                b.rules.push(self.rule()?);
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(b)
    }

    /// `name[IN: TY, ...](A: Equity, @T[: CAL], X: TY, ...) [complete]`; in a
    /// class the `rel` keyword is absent.
    fn rel_signature(&mut self, _in_rules: bool) -> PResult<RelDecl> {
        let (name, span) = self.ident()?;
        let mut inputs = Vec::new();
        if self.eat(&Tok::LBracket) {
            while !self.at(&Tok::RBracket) {
                let (iname, isp) = self.ident()?;
                self.expect(Tok::Colon)?;
                let ty = if self.at_kw("rel") && self.peek_at(1) == &Tok::LParen {
                    self.bump();
                    self.expect(Tok::LParen)?;
                    let mut shape = Vec::new();
                    while !self.at(&Tok::RParen) {
                        shape.push(self.arg_decl(true)?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RParen)?;
                    InputTy::Rel(shape)
                } else {
                    InputTy::Value(self.ty_name()?)
                };
                inputs.push(InputDecl { name: iname, ty, span: isp });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(Tok::RBracket)?;
        }
        self.expect(Tok::LParen)?;
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.arg_decl(false)?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        let complete = self.eat_kw("complete");
        Ok(RelDecl { name, inputs, args, complete, span })
    }

    /// `A: Equity`, `@T`, `@T: weekly`, `@X: base | session`, `M: Price<USD>`;
    /// in a relation input's shape a bare type (`Entity`, `D`) stands for an
    /// unnamed argument.
    fn arg_decl(&mut self, shape: bool) -> PResult<ArgDecl> {
        let span = self.span();
        if self.eat(&Tok::At) {
            let (name, _) = self.ident()?;
            let mut cals = Vec::new();
            if self.eat(&Tok::Colon) {
                cals.push(self.ident()?);
                while self.eat(&Tok::Pipe) {
                    cals.push(self.ident()?);
                }
            }
            return Ok(ArgDecl {
                name,
                ty: None,
                key: true,
                cals,
                span,
            });
        }
        let (first, fsp) = self.ident()?;
        if self.eat(&Tok::Colon) {
            let ty = self.ty_name()?;
            Ok(ArgDecl {
                name: first,
                ty: Some(ty),
                key: false,
                cals: vec![],
                span,
            })
        } else if shape {
            let arg = if self.eat(&Tok::Lt) {
                let (a, _) = self.ident()?;
                self.expect(Tok::Gt)?;
                Some(a)
            } else {
                None
            };
            Ok(ArgDecl {
                name: String::new(),
                ty: Some(TyName { name: first, arg, span: fsp }),
                key: false,
                cals: vec![],
                span,
            })
        } else {
            self.err(format!("expected `: TYPE` after argument `{}`", first))
        }
    }

    fn rule(&mut self) -> PResult<Rule> {
        let span = self.span();
        let head = self.atom()?;
        let body = if self.eat(&Tok::Implies) { self.body_until(&[Tok::Dot])? } else { vec![] };
        self.expect(Tok::Dot)?;
        Ok(Rule { head, body, span })
    }

    fn body_until(&mut self, stops: &[Tok]) -> PResult<Vec<Literal>> {
        let mut body = Vec::new();
        if stops.iter().any(|s| self.at(s)) {
            return Ok(body);
        }
        loop {
            body.push(self.literal()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(body)
    }

    /// `name[args]@cal(terms)`.
    fn atom(&mut self) -> PResult<Atom> {
        let (name, span) = self.ident()?;
        let mut brackets = Vec::new();
        if self.eat(&Tok::LBracket) {
            while !self.at(&Tok::RBracket) {
                brackets.push(self.bracket_arg()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(Tok::RBracket)?;
        }
        let cal = if self.eat(&Tok::At) { Some(self.ident()?) } else { None };
        self.expect(Tok::LParen)?;
        let mut terms = Vec::new();
        while !self.at(&Tok::RParen) {
            terms.push(self.term()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(Atom { name, cal, brackets, terms, span })
    }

    fn bracket_arg(&mut self) -> PResult<BracketArg> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Ident(n) => {
                if self.peek_at(1) == &Tok::LBracket || self.peek_at(1) == &Tok::At {
                    // A nested relation call, `mean_rows[20, close]`, without terms.
                    self.bump();
                    let mut brackets = Vec::new();
                    if self.eat(&Tok::LBracket) {
                        while !self.at(&Tok::RBracket) {
                            brackets.push(self.bracket_arg()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        self.expect(Tok::RBracket)?;
                    }
                    let cal = if self.eat(&Tok::At) { Some(self.ident()?) } else { None };
                    Ok(BracketArg::Nested(Box::new(Atom {
                        name: n,
                        cal,
                        brackets,
                        terms: vec![],
                        span,
                    })))
                } else {
                    self.bump();
                    Ok(BracketArg::Name(n, span))
                }
            }
            _ => Ok(BracketArg::Lit(self.lit_expr()?, span)),
        }
    }

    fn term(&mut self) -> PResult<Term> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Under => {
                self.bump();
                Ok(Term::Wild(span))
            }
            // `[t, ...]`: the legs of a combo (K-2.3).
            Tok::LBracket => {
                self.bump();
                let mut items = Vec::new();
                while !self.at(&Tok::RBracket) {
                    items.push(self.term()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RBracket)?;
                Ok(Term::Ctor(crate::ir::LIST.to_string(), items, span))
            }
            Tok::Ident(name) => {
                self.bump();
                if self.at(&Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(&Tok::RParen) {
                        args.push(self.term()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RParen)?;
                    Ok(Term::Ctor(name, args, span))
                } else if is_var(&name) {
                    Ok(Term::Var(name, span))
                } else {
                    Ok(Term::Param(name, span))
                }
            }
            _ => Ok(Term::Lit(self.lit_expr()?, span)),
        }
    }

    fn literal(&mut self) -> PResult<Literal> {
        let span = self.span();
        // not R(...)
        if self.at_kw("not") {
            self.bump();
            let atom = self.atom()?;
            return Ok(Literal::Neg(atom));
        }
        // top[n](R(...) by (...)) within (...)
        if self.at_kw("top") && self.peek_at(1) == &Tok::LBracket {
            self.bump();
            self.expect(Tok::LBracket)?;
            let n = self.expr()?;
            self.expect(Tok::RBracket)?;
            self.expect(Tok::LParen)?;
            let atom = self.atom()?;
            let by = self.by_clause()?;
            self.expect(Tok::RParen)?;
            let within = self.within_clause()?;
            return Ok(Literal::Top {
                n: Some(n),
                atom,
                by,
                rank: None,
                within,
                span,
            });
        }
        if let Tok::Ident(name) = self.peek().clone() {
            let next = self.peek_at(1).clone();
            // Temporal builtins.
            if next == Tok::LParen {
                match name.as_str() {
                    "prev" => {
                        self.bump();
                        self.bump();
                        let t = self.term()?;
                        self.expect(Tok::Comma)?;
                        let t1 = self.term()?;
                        self.expect(Tok::RParen)?;
                        return Ok(Literal::Prev { t, t1, span });
                    }
                    "lag" => {
                        self.bump();
                        self.bump();
                        let t = self.term()?;
                        self.expect(Tok::Comma)?;
                        let n = self.expr()?;
                        self.expect(Tok::Comma)?;
                        let t1 = self.term()?;
                        self.expect(Tok::RParen)?;
                        return Ok(Literal::Lag { t, n, t1, span });
                    }
                    "month_start" | "day_start" => {
                        self.bump();
                        self.bump();
                        let t = self.term()?;
                        self.expect(Tok::RParen)?;
                        return Ok(if name == "month_start" { Literal::MonthStart { t, span } } else { Literal::DayStart { t, span } });
                    }
                    "session_of" => {
                        self.bump();
                        self.bump();
                        let a = self.term()?;
                        self.expect(Tok::Comma)?;
                        let t = self.term()?;
                        self.expect(Tok::Comma)?;
                        let s = self.term()?;
                        self.expect(Tok::RParen)?;
                        return Ok(Literal::SessionOf { a, t, s, span });
                    }
                    _ => {}
                }
            }
            // VAR in rows[N] ... (a temporal constraint of a group)
            if is_var(&name) && matches!(&next, Tok::Ident(k) if k == "in") {
                self.bump();
                self.bump();
                let var = Term::Var(name, span);
                let spec = self.window_spec()?;
                return Ok(Literal::Window { var, spec, span });
            }
            // VAR = agg(...) over (...) | rank(...) | case {...} | expr
            if is_var(&name) && next == Tok::Eq {
                self.bump();
                self.bump();
                if let Tok::Ident(f) = self.peek().clone() {
                    if self.peek_at(1) == &Tok::LParen && AGGREGATES.contains(&f.as_str()) && self.closes_then_over() {
                        self.bump();
                        self.expect(Tok::LParen)?;
                        let mut args = Vec::new();
                        while !self.at(&Tok::RParen) {
                            args.push(self.expr()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        self.expect(Tok::RParen)?;
                        self.expect_kw("over")?;
                        self.expect(Tok::LParen)?;
                        let conj = self.body_until(&[Tok::RParen])?;
                        self.expect(Tok::RParen)?;
                        let within = self.within_clause()?;
                        return Ok(Literal::Agg {
                            var: name,
                            agg: f,
                            args,
                            conj,
                            within,
                            span,
                        });
                    }
                    if self.peek_at(1) == &Tok::LParen && (f == "rank" || f == "rank_average") {
                        let fsp = self.bump().span;
                        self.expect(Tok::LParen)?;
                        let atom = self.atom()?;
                        let by = self.by_clause()?;
                        self.expect(Tok::RParen)?;
                        let within = self.within_clause()?;
                        let ties = if f == "rank_average" { Ties::Average } else { Ties::Ordinal };
                        return Ok(Literal::Top {
                            n: None,
                            atom,
                            by,
                            rank: Some((name, ties, fsp)),
                            within,
                            span,
                        });
                    }
                    if f == "case" && self.peek_at(1) == &Tok::LBrace {
                        self.bump();
                        self.expect(Tok::LBrace)?;
                        let mut branches = Vec::new();
                        let mut otherwise = None;
                        loop {
                            let bsp = self.span();
                            if self.eat_kw("else") {
                                self.expect(Tok::Arrow)?;
                                otherwise = Some(self.expr()?);
                                self.eat(&Tok::Semi);
                                self.expect(Tok::RBrace)?;
                                break;
                            }
                            let guard = self.body_until(&[Tok::Arrow])?;
                            self.expect(Tok::Arrow)?;
                            let value = self.expr()?;
                            branches.push(Branch { guard, value, span: bsp });
                            if self.eat(&Tok::Semi) {
                                if self.eat(&Tok::RBrace) {
                                    break;
                                }
                                continue;
                            }
                            self.expect(Tok::RBrace)?;
                            break;
                        }
                        return Ok(Literal::Case { var: name, branches, otherwise, span });
                    }
                }
                let expr = self.expr()?;
                return Ok(Literal::Assign { var: name, expr, span });
            }
            // An atom: a name followed by `[` or `@` (a function takes neither,
            // so the xsec library's `decile[R]` is a relation although
            // `decile` is also a function), or by `(` when it is not a
            // function.
            let starts_atom = matches!(next, Tok::LBracket | Tok::At) || (next == Tok::LParen && !SCALAR_FUNCTIONS.contains(&name.as_str()));
            if starts_atom {
                let atom = self.atom()?;
                if self.eat_kw("asof") {
                    let at = self.term()?;
                    return Ok(Literal::AsOf { atom, at, span });
                }
                return Ok(Literal::Atom(atom));
            }
        }
        // A comparison between expressions.
        let lhs = self.expr()?;
        let op = match self.peek() {
            Tok::Lt => CmpOp::Lt,
            Tok::Le => CmpOp::Le,
            Tok::Gt => CmpOp::Gt,
            Tok::Ge => CmpOp::Ge,
            Tok::Eq => CmpOp::Eq,
            t => return self.err(format!("expected a comparison operator, found {}", t)),
        };
        self.bump();
        let rhs = self.expr()?;
        Ok(Literal::Cmp { op, lhs, rhs, span })
    }

    /// Whether the parenthesised arguments at the cursor's next token are
    /// followed by `over`: an aggregation rather than a function call.
    fn closes_then_over(&self) -> bool {
        // cursor at the aggregate name; next is `(`.
        let mut depth = 0usize;
        let mut i = self.pos + 1;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(&self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::Ident(k)) if k == "over");
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn by_clause(&mut self) -> PResult<Option<Vec<(String, Dir, Span)>>> {
        if !self.eat_kw("by") {
            return Ok(None);
        }
        self.expect(Tok::LParen)?;
        let mut keys = Vec::new();
        while !self.at(&Tok::RParen) {
            let (v, sp) = self.var()?;
            let dir = if self.eat_kw("desc") {
                Dir::Desc
            } else {
                self.eat_kw("asc");
                Dir::Asc
            };
            keys.push((v, dir, sp));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(Some(keys))
    }

    fn within_clause(&mut self) -> PResult<Option<Vec<(String, Span)>>> {
        if !self.eat_kw("within") {
            return Ok(None);
        }
        self.expect(Tok::LParen)?;
        let mut vars = Vec::new();
        while !self.at(&Tok::RParen) {
            vars.push(self.var()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(Some(vars))
    }

    fn window_spec(&mut self) -> PResult<WindowSpec> {
        let (kind, sp) = self.ident()?;
        let min_clause = |p: &mut Parser| -> PResult<Option<Expr>> {
            if p.eat_kw("min") {
                Ok(Some(p.expr()?))
            } else {
                Ok(None)
            }
        };
        Ok(match kind.as_str() {
            "rows" | "prior_rows" | "window" | "prior_window" => {
                self.expect(Tok::LBracket)?;
                let e = self.expr()?;
                self.expect(Tok::RBracket)?;
                let min = min_clause(self)?;
                match kind.as_str() {
                    "rows" => WindowSpec::Rows { n: e, min },
                    "prior_rows" => WindowSpec::PriorRows { n: e, min },
                    "window" => WindowSpec::Window { dur: e, min },
                    _ => WindowSpec::PriorWindow { dur: e, min },
                }
            }
            "bucket" => {
                self.expect(Tok::LParen)?;
                let cal = self.ident()?;
                self.expect(Tok::Comma)?;
                let base = self.term()?;
                self.expect(Tok::RParen)?;
                WindowSpec::Bucket { cal, base }
            }
            "session" => {
                self.expect(Tok::LParen)?;
                let base = self.term()?;
                self.expect(Tok::Comma)?;
                self.expect(Tok::LBracket)?;
                let lo = self.expr()?;
                self.expect(Tok::Comma)?;
                let hi = self.expr()?;
                self.expect(Tok::RParen)?;
                self.expect(Tok::RParen)?;
                WindowSpec::Session { base, lo, hi }
            }
            _ => {
                return Err(ParseError {
                    span: sp,
                    message: format!("`{}` is not a temporal constraint (rows, prior_rows, window, prior_window, bucket, session)", kind),
                })
            }
        })
    }

    // ---- expressions ----

    fn expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.term_expr()?;
        loop {
            let span = self.span();
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.term_expr()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn term_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let span = self.span();
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => break,
            };
            self.bump();
            let rhs = self.unary()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let span = self.span();
        if self.eat(&Tok::Minus) {
            let e = self.unary()?;
            return Ok(Expr::Neg(Box::new(e), span));
        }
        self.primary()
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        match self.peek().clone() {
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(name) => {
                self.bump();
                if self.at(&Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(&Tok::RParen) {
                        args.push(self.expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RParen)?;
                    Ok(Expr::Call(name, args, span))
                } else if is_var(&name) {
                    Ok(Expr::Var(name, span))
                } else {
                    Ok(Expr::Param(name, span))
                }
            }
            Tok::Int(_) | Tok::Float(_) | Tok::Duration(_) | Tok::Date(_) | Tok::Str(_) => Ok(Expr::Lit(self.lit()?, span)),
            t => self.err(format!("expected an expression, found {}", t)),
        }
    }

    // ---- decisions, execution, report ----

    fn decisions_block(&mut self, span: Span) -> PResult<DecisionsBlock> {
        self.expect(Tok::LBrace)?;
        let mut b = DecisionsBlock {
            span,
            silence: None,
            orders: None,
            rules: vec![],
        };
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            if self.at_kw("silence") && self.peek_at(1) != &Tok::LParen {
                self.bump();
                b.silence = Some((self.ident()?.0, sp));
            } else if self.at_kw("orders") && self.peek_at(1) != &Tok::LParen {
                self.bump();
                b.orders = Some((self.ident()?.0, sp));
            } else {
                b.rules.push(self.rule()?);
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(b)
    }

    fn execution_block(&mut self, span: Span) -> PResult<ExecutionBlock> {
        if self.eat(&Tok::LBrace) {
            let fields = self.exec_fields()?;
            self.expect(Tok::RBrace)?;
            return Ok(ExecutionBlock::Fields { span, fields });
        }
        let (name, _) = self.ident()?;
        let mut with = Vec::new();
        if self.eat_kw("with") {
            self.expect(Tok::LBrace)?;
            with = self.exec_fields()?;
            self.expect(Tok::RBrace)?;
        }
        Ok(ExecutionBlock::Preset { name, span, with })
    }

    fn exec_fields(&mut self) -> PResult<Vec<ExecField>> {
        let mut fields = Vec::new();
        while !self.at(&Tok::RBrace) {
            let (name, span) = self.ident()?;
            if !EXEC_FIELDS.contains(&name.as_str()) {
                return Err(ParseError {
                    span,
                    message: format!("`{}` is not a field of the execution block ({})", name, EXEC_FIELDS.join(", ")),
                });
            }
            let value = self.exec_value(&name)?;
            fields.push(ExecField { name, value, span });
        }
        Ok(fields)
    }

    fn exec_value(&mut self, field: &str) -> PResult<ExecValue> {
        let word = |p: &mut Parser, admitted: &[&str]| -> PResult<ExecValue> {
            let (w, sp) = p.ident()?;
            if !admitted.contains(&w.as_str()) {
                return Err(ParseError {
                    span: sp,
                    message: format!("`{}` is not an admissible value of `{}` ({})", w, field, admitted.join(" | ")),
                });
            }
            Ok(ExecValue::Word(w))
        };
        Ok(match field {
            "price" => {
                let (rel, _) = self.ident()?;
                let cal = if self.eat(&Tok::At) { Some(self.ident()?.0) } else { None };
                ExecValue::Price { rel, cal }
            }
            "orders" => word(self, &["market", "moo", "moc", "moo_moc"])?,
            "lot" => word(self, &["whole", "fractional"])?,
            "actions" => word(self, &["apply", "in_price"])?,
            "oversize" => word(self, &["halt", "clamp", "allow"])?,
            "ruin" => word(self, &["halt", "continue"])?,
            "numerics" => word(self, &["two_pass", "running"])?,
            "capital" => {
                let (amount, _) = self.number()?;
                let (cur, csp) = self.ident()?;
                if !is_currency(&cur) {
                    return Err(ParseError {
                        span: csp,
                        message: format!("expected a currency after the capital, found `{}`", cur),
                    });
                }
                let compounding = if self.eat_kw("fixed") {
                    false
                } else if self.eat_kw("compounding") {
                    true
                } else {
                    return self.err("expected `fixed` or `compounding` after the capital");
                };
                ExecValue::Capital { amount, compounding }
            }
            "commission" => {
                if self.eat_kw("none") {
                    ExecValue::None
                } else if self.eat_kw("per_share") {
                    let (per_share, _) = self.number()?;
                    self.ident()?;
                    self.expect_kw("min")?;
                    let (min, _) = self.number()?;
                    self.ident()?;
                    ExecValue::CommissionPerShare { per_share, min }
                } else if self.eat_kw("per_contract") {
                    self.expect_kw("from")?;
                    self.expect_kw("catalog")?;
                    ExecValue::CommissionPerContract
                } else if self.eat_kw("bps") {
                    let (x, _) = self.number()?;
                    ExecValue::CommissionBps(x)
                } else {
                    let (x, _) = self.number()?;
                    self.expect_kw("bps")?;
                    self.expect_kw("per")?;
                    self.expect_kw("side")?;
                    ExecValue::CommissionBps(x)
                }
            }
            "slippage" => {
                if self.eat_kw("none") {
                    ExecValue::None
                } else if self.eat_kw("fixed") {
                    let (x, _) = self.number()?;
                    self.expect_kw("bps")?;
                    ExecValue::SlippageFixed(x)
                } else if self.eat_kw("vol") {
                    let (mult, _) = self.number()?;
                    self.expect_kw("over")?;
                    let (bars, _) = self.uint()?;
                    self.expect_kw("bars")?;
                    ExecValue::SlippageVol { mult, bars }
                } else {
                    return self.err("expected `fixed X bps`, `vol X over N bars` or `none` after `slippage`");
                }
            }
            "impact" => {
                if self.eat_kw("none") {
                    ExecValue::None
                } else if self.eat_kw("sqrt") {
                    let (coef, _) = self.number()?;
                    self.expect_kw("over")?;
                    let (days, _) = self.uint()?;
                    self.expect_kw("days")?;
                    self.expect_kw("adv")?;
                    ExecValue::Impact { coef, days }
                } else {
                    return self.err("expected `sqrt X over N days adv` or `none` after `impact`");
                }
            }
            "participation" => {
                if self.eat_kw("none") {
                    ExecValue::None
                } else {
                    let (x, _) = self.number()?;
                    self.expect_kw("of")?;
                    self.expect_kw("bar")?;
                    self.expect_kw("volume")?;
                    ExecValue::Participation(x)
                }
            }
            "financing" => {
                if self.eat_kw("none") {
                    ExecValue::None
                } else {
                    self.expect_kw("cash")?;
                    let (cash, _) = self.number()?;
                    self.expect_kw("debit")?;
                    let (debit, _) = self.number()?;
                    self.expect_kw("short")?;
                    let (short, _) = self.number()?;
                    ExecValue::Financing { cash, debit, short }
                }
            }
            "leverage" => {
                if self.eat_kw("unconstrained") {
                    ExecValue::Word("unconstrained".into())
                } else if self.eat_kw("reg_t") {
                    ExecValue::Word("reg_t".into())
                } else if self.eat_kw("gross") {
                    self.expect(Tok::Le)?;
                    let (max, _) = self.number()?;
                    let (policy, psp) = self.ident()?;
                    if policy != "halt" && policy != "reject" {
                        return Err(ParseError {
                            span: psp,
                            message: format!("expected `halt` or `reject` after the gross limit, found `{}`", policy),
                        });
                    }
                    ExecValue::LeverageGross { max, policy }
                } else {
                    return self.err("expected `gross <= X halt|reject`, `reg_t` or `unconstrained` after `leverage`");
                }
            }
            "delisting" => {
                if self.eat_kw("last_price") {
                    ExecValue::Word("last_price".into())
                } else if self.eat_kw("haircut") {
                    self.expect(Tok::LBrace)?;
                    let mut hs = Vec::new();
                    while !self.at(&Tok::RBrace) {
                        let (reason, _) = self.ident()?;
                        let (x, _) = self.number()?;
                        hs.push((reason, x));
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RBrace)?;
                    ExecValue::Haircuts(hs)
                } else {
                    return self.err("expected `last_price` or `haircut { reason X, ... }` after `delisting`");
                }
            }
            _ => unreachable!(),
        })
    }

    fn report_block(&mut self, span: Span) -> PResult<ReportBlock> {
        self.expect(Tok::LBrace)?;
        let mut r = ReportBlock {
            span,
            calendar: None,
            periods: None,
            window: None,
        };
        while !self.at(&Tok::RBrace) {
            let sp = self.span();
            if self.eat_kw("calendar") {
                let (c, _) = self.ident()?;
                r.calendar = Some((c, sp));
            } else if self.eat_kw("periods") {
                let (n, _) = self.number()?;
                self.expect_kw("per")?;
                self.expect_kw("year")?;
                r.periods = Some((n, sp));
            } else if self.eat_kw("window") {
                let (lo, _) = self.date()?;
                self.expect(Tok::DotDot)?;
                let (hi, _) = self.date()?;
                r.window = Some((lo, hi, sp));
            } else {
                return self.err(format!("expected `calendar`, `periods` or `window` in the report block, found {}", self.peek()));
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(r)
    }
}

/// A duration literal's text (`20d`) for messages.
pub fn duration_text(d: &Duration) -> String {
    d.to_string()
}

/// A resolution's text (`@1d`) for messages.
pub fn resolution_text(r: Resolution) -> String {
    r.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_strategy_with_every_block() {
        let src = r#"
strategy s {
  data { class equity   need bars @1d from 1990-01-01 complete   need actions, listing   need lists "SP500", "SP400"   need series "SPXTR" @1d, "VIX" @1w complete   require reviewed   prefer latest }
  uses calendar, stat
  calendars { weekly = weeks(1)   es = sessions of "ES" }
  clock { decide on weekly }
  universe { instruments { ES from 2000-01-01; 6J from 2001-01-01; ETH }   universe(A, T) :- listed(A, T), traded@base(A, T). }
  params { n : Count = 7   late : Scalar = 1/3   floor : Price<USD> = 3 USD/share in 1 USD/share..10 USD/share   entry : Scalar = -2.0 in -4.0..-1.0 }
  rules {
    rel sma[N: Count, R: rel(Entity, @T, D)](A: Equity, @T, M: D)
    sma[N, R](A, T, M) :- universe(A, T), M = mean(X) over (T1 in rows[N], R(A, T1, X)).
    rel open0(A: Future, @X: base | session, O: Price<USD>)
    open0(A, X, O) :- O = first(P) over (T1 in session(X, [0min, 1min)), open(A, T1, P)).
    rel ranked(A: Equity, @T, K: Count)
    ranked(A, T, K) :- K = rank(scored(A, T, S) by (S desc, A asc)) within (T).
    rel scale(@T, S: Scalar)
    scale(T, S) :- bar(T), S = case { not gate(T), not calm(T) -> 0 ; crowded(T) -> crowd_dial ; else -> 1 }.
    rel sel(A: Equity, @T)
    sel(A, T) :- bar(T), top[n](cand(A, T, M) by (M desc, A asc)) within (T), dividend(A, _, Ex, _, _) asof T, Ex > T.
    observe scale
    observe latched where A = "ES" trace instances
  }
  decisions { silence hold   orders moc   decide(T, hold(A, W)) :- target(A, T, W), abs(W - C) >= band - band_tol. }
  execution book_fixed_base with { commission 10 bps per side }
  report { calendar weekly   periods 52 per year   window 1991-01-01 .. 2026-07-02 }
}
"#;
        let units = parse_units(src).unwrap_or_else(|e| panic!("{}", e));
        let Unit::Strategy(s) = &units[0] else { panic!() };
        assert_eq!(s.data.as_ref().unwrap().needs.len(), 5);
        assert!(s.data.as_ref().unwrap().needs[0].complete);
        assert!(!s.data.as_ref().unwrap().needs[1].complete);
        assert_eq!(s.calendars.as_ref().unwrap().decls[1].1, Cal::SessionsOf("ES".into()));
        assert_eq!(s.universe.as_ref().unwrap().instruments.as_ref().unwrap().0.len(), 3);
        assert_eq!(s.params.as_ref().unwrap().params[1].value, Lit::Float(1.0 / 3.0));
        let rules = s.rules.as_ref().unwrap();
        assert_eq!(rules.rels.len(), 5);
        assert_eq!(rules.rels[0].inputs.len(), 2);
        assert_eq!(rules.rels[1].args[1].cals.len(), 2);
        assert_eq!(rules.rules.len(), 5);
        assert_eq!(rules.observes.len(), 2);
        assert!(matches!(&rules.rules[3].body[1], Literal::Case { branches, otherwise: Some(_), .. } if branches.len() == 2));
        assert!(matches!(&rules.rules[4].body[1], Literal::Top { n: Some(_), within: Some(w), .. } if w.len() == 1));
        assert!(matches!(&rules.rules[4].body[2], Literal::AsOf { .. }));
        assert!(matches!(&s.execution, Some(ExecutionBlock::Preset { with, .. }) if with.len() == 1));
        assert_eq!(s.report.as_ref().unwrap().periods.unwrap().0, 52.0);
    }

    #[test]
    fn parses_the_class_and_the_presets() {
        let src = r#"
class equity {
  base @1d
  unit shares
  close(A: Equity, @T, P: Price<USD>)
  member(A: Equity, @T, List: Label) complete
  lists "SPX", "SP500"
}
class series { value(Name: Label, @T, V: Scalar)   names "SPXTR" @1d, "VIX" @1w }
execution equity_default {
  price close
  orders market
  lot whole
  capital 1_000_000 USD fixed
  commission per_share 0.005 USD min 1 USD
  slippage vol 0.1 over 20 bars
  impact sqrt 0.1 over 20 days adv
  participation 0.1 of bar volume
  financing cash 0 debit 0.05 short 0
  leverage gross <= 1 halt
  actions apply
  delisting haircut { bankruptcy 1, regulatory 1, acquisition 0, voluntary 0, other 1 }
  oversize halt
  ruin halt
  numerics running
}
"#;
        let units = parse_units(src).unwrap_or_else(|e| panic!("{}", e));
        assert_eq!(units.len(), 3);
        let Unit::Class(c) = &units[0] else { panic!() };
        assert_eq!(c.base, Some(Resolution::D1));
        assert_eq!(c.rels.len(), 2);
        assert!(c.rels[1].complete);
        let Unit::Execution(e) = &units[2] else { panic!() };
        assert_eq!(e.fields.len(), 15);
    }
}
