//! Recursive-descent parser from the surface syntax to the IR. The parser
//! never reorders literals (section 1, "literal order").

use crate::ir::*;
use crate::lexer::{lex, LexError, Tok, Token};

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

const TEMPORAL_BUILTINS: &[&str] = &["prev", "lag", "month_start", "day_start"];

pub fn parse_units(src: &str) -> PResult<Vec<Unit>> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0, unit: String::new() };
    let mut units = Vec::new();
    while !p.at(&Tok::Eof) {
        units.push(p.unit()?);
    }
    Ok(units)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    unit: String,
}

fn is_var(name: &str) -> bool {
    name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
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
    fn ident(&mut self) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                let sp = self.bump().span;
                Ok((s, sp))
            }
            t => self.err(format!("expected identifier, found {}", t)),
        }
    }
    fn var(&mut self) -> PResult<(String, Span)> {
        let (s, sp) = self.ident()?;
        if !is_var(&s) {
            return Err(ParseError {
                span: sp,
                message: format!("expected a variable (uppercase initial), found `{}`", s),
            });
        }
        Ok((s, sp))
    }
    fn comma(&mut self) -> bool {
        if self.at(&Tok::Comma) {
            self.bump();
            true
        } else {
            false
        }
    }

    // ---- units -------------------------------------------------------

    fn unit(&mut self) -> PResult<Unit> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::Ident(s) if s == "environment" => UnitKind::Environment,
            Tok::Ident(s) if s == "library" => UnitKind::Library,
            Tok::Ident(s) if s == "strategy" => UnitKind::Strategy,
            t => return self.err(format!("expected `environment`, `library` or `strategy`, found {}", t)),
        };
        self.bump();
        let (name, _) = self.ident()?;
        self.unit = name.clone();
        let mut unit = Unit {
            kind,
            name,
            span,
            env: None,
            uses: vec![],
            resolution: None,
            mode: None,
            redeclared: vec![],
            params: vec![],
            rels: vec![],
            rules: vec![],
        };
        self.expect(Tok::LBrace)?;
        while !self.at(&Tok::RBrace) {
            if self.at(&Tok::Eof) {
                return self.err("unexpected end of input inside unit; missing `}`");
            }
            if kind == UnitKind::Environment {
                let mut sig = self.signature(Kind::Primitive { env: unit.name.clone() })?;
                if self.at_kw("complete") {
                    self.bump();
                    sig.complete = true;
                }
                if sig.res.is_none() {
                    return Err(ParseError {
                        span: sig.span,
                        message: format!("primitive `{}` must declare its native resolution (e.g. @1d)", sig.name),
                    });
                }
                unit.rels.push(sig);
                continue;
            }
            match self.peek().clone() {
                Tok::Ident(s) if s == "env" => {
                    self.bump();
                    let (e, sp) = self.ident()?;
                    if unit.env.is_some() {
                        unit.redeclared.push(("env".to_string(), sp));
                    } else {
                        unit.env = Some((e, sp));
                    }
                }
                Tok::Ident(s) if s == "uses" => {
                    self.bump();
                    loop {
                        let (l, sp) = self.ident()?;
                        unit.uses.push((l, sp));
                        if !self.comma() {
                            break;
                        }
                    }
                }
                Tok::Ident(s) if s == "resolution" => {
                    self.bump();
                    let sp = self.span();
                    match self.bump().tok {
                        Tok::Res(r) => {
                            if unit.resolution.is_some() {
                                unit.redeclared.push(("resolution".to_string(), sp));
                            } else {
                                unit.resolution = Some((r, sp));
                            }
                        }
                        t => {
                            return Err(ParseError {
                                span: sp,
                                message: format!("expected a resolution such as @1d, found {}", t),
                            })
                        }
                    }
                }
                Tok::Ident(s) if s == "mode" => {
                    self.bump();
                    let (m, sp) = self.ident()?;
                    let mode = match m.as_str() {
                        "delta" => DecisionMode::Delta,
                        "target" => DecisionMode::Target,
                        _ => {
                            return Err(ParseError {
                                span: sp,
                                message: format!("unknown decision mode `{}` (delta or target)", m),
                            })
                        }
                    };
                    if unit.mode.is_some() {
                        unit.redeclared.push(("mode".to_string(), sp));
                    } else {
                        unit.mode = Some((mode, sp));
                    }
                }
                Tok::Ident(s) if s == "param" => {
                    self.bump();
                    unit.params.push(self.param()?);
                }
                Tok::Ident(s) if s == "rel" => {
                    self.bump();
                    let sig = self.signature(Kind::Derived { unit: unit.name.clone() })?;
                    unit.rels.push(sig);
                }
                _ => {
                    let rule = self.rule()?;
                    unit.rules.push(rule);
                }
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(unit)
    }

    fn ty(&mut self) -> PResult<Ty> {
        let (name, sp) = self.ident()?;
        let arg = if self.at(&Tok::Lt) {
            self.bump();
            let (a, _) = self.ident()?;
            self.expect(Tok::Gt)?;
            Some(a)
        } else {
            None
        };
        Ty::parse(&name, arg.as_deref()).ok_or_else(|| ParseError {
            span: sp,
            message: format!("unknown type `{}{}`", name, arg.map(|a| format!("<{}>", a)).unwrap_or_default()),
        })
    }

    fn signature(&mut self, kind: Kind) -> PResult<Signature> {
        let (name, span) = self.ident()?;
        self.expect(Tok::LParen)?;
        let mut args = Vec::new();
        if !self.at(&Tok::RParen) {
            loop {
                let mode = match self.bump().tok {
                    Tok::Plus => Mode::In,
                    Tok::Minus => Mode::Out,
                    Tok::At => Mode::Key,
                    t => return self.err(format!("expected argument mode `+`, `-` or `@`, found {}", t)),
                };
                let (aname, _) = self.var()?;
                self.expect(Tok::Colon)?;
                let ty = self.ty()?;
                args.push(Arg { name: aname, mode, ty });
                if !self.comma() {
                    break;
                }
            }
        }
        self.expect(Tok::RParen)?;
        let res = if let Tok::Res(r) = self.peek().clone() {
            self.bump();
            Some(r)
        } else {
            None
        };
        let keys = args.iter().filter(|a| a.mode == Mode::Key).count();
        if keys != 1 {
            return Err(ParseError {
                span,
                message: format!("relation `{}` must mark exactly one argument as the temporal key `@`", name),
            });
        }
        if let Some(k) = args.iter().find(|a| a.mode == Mode::Key) {
            if k.ty != Ty::Timestamp {
                return Err(ParseError {
                    span,
                    message: format!("temporal key `{}` of `{}` must have type Timestamp", k.name, name),
                });
            }
        }
        Ok(Signature {
            name,
            args,
            res,
            complete: false,
            kind,
            span,
        })
    }

    fn param(&mut self) -> PResult<Param> {
        let (name, span) = self.ident()?;
        if is_var(&name) {
            return Err(ParseError {
                span,
                message: "parameter names are lowercase".into(),
            });
        }
        self.expect(Tok::Colon)?;
        let ty = self.ty()?;
        self.expect(Tok::Eq)?;
        let value = self.lit()?;
        let range = if self.at_kw("in") {
            self.bump();
            let lo = self.lit()?;
            self.expect(Tok::DotDot)?;
            let hi = self.lit()?;
            Some((lo, hi))
        } else {
            None
        };
        Ok(Param { name, ty, value, range, span })
    }

    /// A literal value: number with optional unit, duration, or equity string.
    fn lit(&mut self) -> PResult<Lit> {
        let neg = if self.at(&Tok::Minus) {
            self.bump();
            true
        } else {
            false
        };
        let sp = self.span();
        let t = self.bump().tok;
        let sign = if neg { -1.0 } else { 1.0 };
        Ok(match t {
            Tok::Int(i) => {
                let v = if neg { -i } else { i };
                self.unit_suffix(v as f64, Lit::Int(v))?
            }
            Tok::Float(x) => self.unit_suffix(sign * x, Lit::Float(sign * x))?,
            Tok::Duration(d) => {
                if neg {
                    return Err(ParseError {
                        span: sp,
                        message: "durations are non-negative".into(),
                    });
                }
                Lit::Duration(d)
            }
            Tok::Str(s) => Lit::Equity(s),
            t => {
                return Err(ParseError {
                    span: sp,
                    message: format!("expected a literal, found {}", t),
                })
            }
        })
    }

    fn unit_suffix(&mut self, x: f64, plain: Lit) -> PResult<Lit> {
        if let Tok::Ident(s) = self.peek().clone() {
            if s == "shares" {
                self.bump();
                return Ok(Lit::Shares(x));
            }
            if s.len() == 3 && s.chars().all(|c| c.is_ascii_uppercase()) {
                self.bump();
                // `60 USD/share` (or `/shares`) is a price: currency per share
                // (section 2). A `/` followed by anything else is division.
                if self.at(&Tok::Slash) && matches!(self.peek_at(1), Tok::Ident(u) if u == "share" || u == "shares") {
                    self.bump();
                    self.bump();
                    return Ok(Lit::Price(x, s));
                }
                return Ok(Lit::Money(x, s));
            }
        }
        Ok(plain)
    }

    // ---- rules -------------------------------------------------------

    fn rule(&mut self) -> PResult<Rule> {
        let span = self.span();
        let head = self.atom()?;
        self.expect(Tok::Implies)?;
        let mut body = Vec::new();
        loop {
            body.push(self.literal()?);
            if !self.comma() {
                break;
            }
        }
        self.expect(Tok::Dot)?;
        Ok(Rule {
            head,
            body,
            span,
            unit: self.unit.clone(),
        })
    }

    fn atom(&mut self) -> PResult<Atom> {
        let (name, span) = self.ident()?;
        if is_var(&name) {
            return Err(ParseError {
                span,
                message: format!("expected a relation name, found variable `{}`", name),
            });
        }
        self.expect(Tok::LParen)?;
        let mut terms = Vec::new();
        if !self.at(&Tok::RParen) {
            loop {
                terms.push(self.term()?);
                if !self.comma() {
                    break;
                }
            }
        }
        self.expect(Tok::RParen)?;
        Ok(Atom { name, terms, span })
    }

    fn term(&mut self) -> PResult<Term> {
        let sp = self.span();
        match self.peek().clone() {
            Tok::Under => {
                self.bump();
                Ok(Term::Wild(sp))
            }
            Tok::Ident(s) => {
                if is_var(&s) {
                    self.bump();
                    Ok(Term::Var(s, sp))
                } else if *self.peek_at(1) == Tok::LParen {
                    self.bump();
                    self.bump();
                    let mut args = Vec::new();
                    if !self.at(&Tok::RParen) {
                        loop {
                            args.push(self.term()?);
                            if !self.comma() {
                                break;
                            }
                        }
                    }
                    self.expect(Tok::RParen)?;
                    Ok(Term::Ctor(s, args, sp))
                } else {
                    self.bump();
                    Ok(Term::Param(s, sp))
                }
            }
            Tok::Int(_) | Tok::Float(_) | Tok::Duration(_) | Tok::Str(_) | Tok::Minus => Ok(Term::Lit(self.lit()?, sp)),
            t => Err(ParseError {
                span: sp,
                message: format!("expected a term, found {}", t),
            }),
        }
    }

    fn literal(&mut self) -> PResult<Literal> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Ident(s) if s == "not" => {
                self.bump();
                let a = self.atom()?;
                Ok(Literal::Neg(a))
            }
            Tok::Ident(s) if s == "top" && *self.peek_at(1) == Tok::LParen => self.top(),
            Tok::Ident(s) if s == "resample" && *self.peek_at(1) == Tok::LParen => self.resample(),
            Tok::Ident(s) if TEMPORAL_BUILTINS.contains(&s.as_str()) && *self.peek_at(1) == Tok::LParen => self.builtin(),
            Tok::Ident(s) if is_var(&s) && matches!(self.peek_at(1), Tok::Ident(k) if k == "in") => self.window(),
            Tok::Ident(s) if is_var(&s) && *self.peek_at(1) == Tok::Eq => {
                let (var, _) = self.var()?;
                self.expect(Tok::Eq)?;
                if let Tok::Ident(agg) = self.peek().clone() {
                    if AGGREGATES.contains(&agg.as_str()) && *self.peek_at(1) == Tok::LParen {
                        self.bump();
                        self.bump();
                        let mut args = Vec::new();
                        loop {
                            args.push(self.expr()?);
                            if !self.comma() {
                                break;
                            }
                        }
                        self.expect(Tok::RParen)?;
                        self.expect_kw("over")?;
                        self.expect(Tok::LParen)?;
                        let mut conj = Vec::new();
                        loop {
                            conj.push(self.literal()?);
                            if !self.comma() {
                                break;
                            }
                        }
                        self.expect(Tok::RParen)?;
                        return Ok(Literal::Agg { var, agg, args, conj, span });
                    }
                }
                let expr = self.expr()?;
                Ok(Literal::Assign { var, expr, span })
            }
            Tok::Ident(s) if !is_var(&s) && *self.peek_at(1) == Tok::LParen && !SCALAR_FUNCTIONS.contains(&s.as_str()) => Ok(Literal::Atom(self.atom()?)),
            _ => {
                let lhs = self.expr()?;
                let op = match self.bump().tok {
                    Tok::Lt => CmpOp::Lt,
                    Tok::Le => CmpOp::Le,
                    Tok::Eq => CmpOp::Eq,
                    Tok::Gt => CmpOp::Gt,
                    Tok::Ge => CmpOp::Ge,
                    t => {
                        return Err(ParseError {
                            span,
                            message: format!("expected a comparison operator after expression, found {}", t),
                        })
                    }
                };
                let rhs = self.expr()?;
                Ok(Literal::Cmp { op, lhs, rhs, span })
            }
        }
    }

    fn builtin(&mut self) -> PResult<Literal> {
        let (name, span) = self.ident()?;
        self.expect(Tok::LParen)?;
        let b = match name.as_str() {
            "prev" => {
                let t = self.term()?;
                self.expect(Tok::Comma)?;
                let t1 = self.term()?;
                Builtin::Prev { t, t1 }
            }
            "lag" => {
                let t = self.term()?;
                self.expect(Tok::Comma)?;
                let n = self.expr()?;
                self.expect(Tok::Comma)?;
                let t1 = self.term()?;
                Builtin::Lag { t, n, t1 }
            }
            "month_start" => Builtin::MonthStart { t: self.term()? },
            "day_start" => Builtin::DayStart { t: self.term()? },
            _ => unreachable!(),
        };
        self.expect(Tok::RParen)?;
        Ok(Literal::Builtin(b, span))
    }

    fn window(&mut self) -> PResult<Literal> {
        let span = self.span();
        let (v, vs) = self.var()?;
        self.expect_kw("in")?;
        let (k, ks) = self.ident()?;
        let kind = match k.as_str() {
            "window" => WindowKind::Window,
            "prior_window" => WindowKind::Prior,
            _ => {
                return Err(ParseError {
                    span: ks,
                    message: format!("expected `window` or `prior_window`, found `{}`", k),
                })
            }
        };
        self.expect(Tok::LParen)?;
        let base = self.term()?;
        self.expect(Tok::Comma)?;
        let dur = self.expr()?;
        self.expect(Tok::Comma)?;
        self.expect_kw("min")?;
        let min = self.expr()?;
        self.expect(Tok::RParen)?;
        Ok(Literal::Window {
            var: Term::Var(v, vs),
            kind,
            base,
            dur,
            min,
            span,
        })
    }

    fn top(&mut self) -> PResult<Literal> {
        let span = self.span();
        self.expect_kw("top")?;
        self.expect(Tok::LParen)?;
        let n = self.expr()?;
        self.expect(Tok::Comma)?;
        let atom = self.atom()?;
        let by = if self.comma() {
            self.expect_kw("by")?;
            self.expect(Tok::LParen)?;
            let mut keys = Vec::new();
            loop {
                let (v, vs) = self.var()?;
                let (d, ds) = self.ident()?;
                let dir = match d.as_str() {
                    "asc" => Dir::Asc,
                    "desc" => Dir::Desc,
                    _ => {
                        return Err(ParseError {
                            span: ds,
                            message: format!("expected `asc` or `desc`, found `{}`", d),
                        })
                    }
                };
                keys.push((v, dir, vs));
                if !self.comma() {
                    break;
                }
            }
            self.expect(Tok::RParen)?;
            Some(keys)
        } else {
            None
        };
        self.expect(Tok::RParen)?;
        Ok(Literal::Top { n, atom, by, span })
    }

    fn resample(&mut self) -> PResult<Literal> {
        let span = self.span();
        self.expect_kw("resample")?;
        self.expect(Tok::LParen)?;
        let inner = self.atom()?;
        self.expect_kw("to")?;
        let to = match self.bump().tok {
            Tok::Res(r) => r,
            t => return self.err(format!("expected a resolution after `to`, found {}", t)),
        };
        self.expect_kw("as")?;
        let (as_var, _) = self.var()?;
        self.expect(Tok::Comma)?;
        self.expect_kw("min")?;
        let min = self.expr()?;
        let mut aggs = Vec::new();
        while self.comma() {
            let (v, _) = self.var()?;
            self.expect(Tok::Eq)?;
            let (agg, _) = self.ident()?;
            self.expect(Tok::LParen)?;
            let e = self.expr()?;
            self.expect(Tok::RParen)?;
            aggs.push((v, agg, e));
        }
        self.expect(Tok::RParen)?;
        if aggs.is_empty() {
            return Err(ParseError {
                span,
                message: "resample needs at least one aggregate (e.g. `C = last(P)`)".into(),
            });
        }
        Ok(Literal::Resample { inner, to, as_var, min, aggs, span })
    }

    // ---- expressions -------------------------------------------------

    fn expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.mul()?;
        loop {
            let span = self.span();
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.mul()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs), span);
        }
        Ok(lhs)
    }

    fn mul(&mut self) -> PResult<Expr> {
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
        if self.at(&Tok::Minus) {
            self.bump();
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
            Tok::Int(_) | Tok::Float(_) | Tok::Duration(_) | Tok::Str(_) => Ok(Expr::Lit(self.lit()?, span)),
            Tok::Ident(s) => {
                if is_var(&s) {
                    self.bump();
                    Ok(Expr::Var(s, span))
                } else if *self.peek_at(1) == Tok::LParen {
                    self.bump();
                    self.bump();
                    let mut args = Vec::new();
                    if !self.at(&Tok::RParen) {
                        loop {
                            args.push(self.expr()?);
                            if !self.comma() {
                                break;
                            }
                        }
                    }
                    self.expect(Tok::RParen)?;
                    if !SCALAR_FUNCTIONS.contains(&s.as_str()) {
                        return Err(ParseError {
                            span,
                            message: format!("`{}` is not a scalar function (log, exp, sqrt, abs, least, greatest); aggregates need `over (...)`", s),
                        });
                    }
                    Ok(Expr::Call(s, args, span))
                } else {
                    self.bump();
                    Ok(Expr::Param(s, span))
                }
            }
            t => Err(ParseError {
                span,
                message: format!("expected an expression, found {}", t),
            }),
        }
    }
}
