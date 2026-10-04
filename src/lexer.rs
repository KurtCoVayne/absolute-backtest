//! Tokeniser for the surface syntax.

use crate::ir::{Duration, Resolution, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    Res(Resolution),
    Duration(Duration),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,
    DotDot,
    Colon,
    Implies,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Under,
    At,
    Eof,
}

impl std::fmt::Display for Tok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "`{}`", s),
            Tok::Int(i) => write!(f, "`{}`", i),
            Tok::Float(x) => write!(f, "`{}`", x),
            Tok::Str(s) => write!(f, "\"{}\"", s),
            Tok::Res(r) => write!(f, "`{}`", r),
            Tok::Duration(d) => write!(f, "`{}`", d),
            Tok::LParen => write!(f, "`(`"),
            Tok::RParen => write!(f, "`)`"),
            Tok::LBrace => write!(f, "`{{`"),
            Tok::RBrace => write!(f, "`}}`"),
            Tok::Comma => write!(f, "`,`"),
            Tok::Dot => write!(f, "`.`"),
            Tok::DotDot => write!(f, "`..`"),
            Tok::Colon => write!(f, "`:`"),
            Tok::Implies => write!(f, "`:-`"),
            Tok::Eq => write!(f, "`=`"),
            Tok::Lt => write!(f, "`<`"),
            Tok::Le => write!(f, "`<=`"),
            Tok::Gt => write!(f, "`>`"),
            Tok::Ge => write!(f, "`>=`"),
            Tok::Plus => write!(f, "`+`"),
            Tok::Minus => write!(f, "`-`"),
            Tok::Star => write!(f, "`*`"),
            Tok::Slash => write!(f, "`/`"),
            Tok::Under => write!(f, "`_`"),
            Tok::At => write!(f, "`@`"),
            Tok::Eof => write!(f, "end of input"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct LexError {
    pub span: Span,
    pub message: String,
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    let mut col = 1u32;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        let span = Span { line, col };
        let advance = |i: &mut usize, line: &mut u32, col: &mut u32, k: usize| {
            for _ in 0..k {
                if chars[*i] == '\n' {
                    *line += 1;
                    *col = 1;
                } else {
                    *col += 1;
                }
                *i += 1;
            }
        };
        if c.is_whitespace() {
            advance(&mut i, &mut line, &mut col, 1);
            continue;
        }
        if c == '#' || (c == '/' && i + 1 < n && chars[i + 1] == '/') {
            while i < n && chars[i] != '\n' {
                advance(&mut i, &mut line, &mut col, 1);
            }
            continue;
        }
        if c == '"' {
            let mut j = i + 1;
            while j < n && chars[j] != '"' {
                if chars[j] == '\n' {
                    return Err(LexError {
                        span,
                        message: "unterminated string literal".into(),
                    });
                }
                j += 1;
            }
            if j >= n {
                return Err(LexError {
                    span,
                    message: "unterminated string literal".into(),
                });
            }
            let s: String = chars[i + 1..j].iter().collect();
            let k = j + 1 - i;
            advance(&mut i, &mut line, &mut col, k);
            toks.push(Token { tok: Tok::Str(s), span });
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            let mut text = String::new();
            let mut is_float = false;
            while j < n && (chars[j].is_ascii_digit() || chars[j] == '_') {
                if chars[j] != '_' {
                    text.push(chars[j]);
                }
                j += 1;
            }
            if j + 1 < n && chars[j] == '.' && chars[j + 1].is_ascii_digit() {
                is_float = true;
                text.push('.');
                j += 1;
                while j < n && (chars[j].is_ascii_digit() || chars[j] == '_') {
                    if chars[j] != '_' {
                        text.push(chars[j]);
                    }
                    j += 1;
                }
            }
            if j < n && (chars[j] == 'e' || chars[j] == 'E') && j + 1 < n && (chars[j + 1].is_ascii_digit() || chars[j + 1] == '-') {
                is_float = true;
                text.push('e');
                j += 1;
                if chars[j] == '-' {
                    text.push('-');
                    j += 1;
                }
                while j < n && chars[j].is_ascii_digit() {
                    text.push(chars[j]);
                    j += 1;
                }
            }
            // Duration suffix directly attached: 20d, 2w, 3mo, 1y.
            let mut suffix = String::new();
            let mut k = j;
            while k < n && chars[k].is_ascii_alphabetic() {
                suffix.push(chars[k]);
                k += 1;
            }
            let tok = if !suffix.is_empty() {
                let amount: i64 = text.parse().map_err(|_| LexError {
                    span,
                    message: format!("bad duration amount `{}`", text),
                })?;
                if is_float {
                    return Err(LexError {
                        span,
                        message: "durations are whole numbers of d, w, mo or y".into(),
                    });
                }
                let d = match suffix.as_str() {
                    "d" => Duration { months: 0, days: amount },
                    "w" => Duration { months: 0, days: amount * 7 },
                    "mo" => Duration { months: amount, days: 0 },
                    "y" => Duration { months: amount * 12, days: 0 },
                    _ => {
                        return Err(LexError {
                            span,
                            message: format!("unknown unit suffix `{}` (use d, w, mo, y)", suffix),
                        })
                    }
                };
                j = k;
                Tok::Duration(d)
            } else if is_float {
                Tok::Float(text.parse().map_err(|_| LexError {
                    span,
                    message: format!("bad number `{}`", text),
                })?)
            } else {
                Tok::Int(text.parse().map_err(|_| LexError {
                    span,
                    message: format!("bad integer `{}`", text),
                })?)
            };
            let k = j - i;
            advance(&mut i, &mut line, &mut col, k);
            toks.push(Token { tok, span });
            continue;
        }
        if c == '@' {
            let mut j = i + 1;
            let mut text = String::new();
            while j < n && chars[j].is_ascii_alphanumeric() {
                text.push(chars[j]);
                j += 1;
            }
            // `@1d` is a resolution; `@T` is the temporal-key mode marker.
            if !text.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                advance(&mut i, &mut line, &mut col, 1);
                toks.push(Token { tok: Tok::At, span });
                continue;
            }
            let r = Resolution::parse(&text).ok_or_else(|| LexError {
                span,
                message: format!("unknown resolution `@{}` (use @1m, @5m, @15m, @30m, @1h, @1d)", text),
            })?;
            let k = j - i;
            advance(&mut i, &mut line, &mut col, k);
            toks.push(Token { tok: Tok::Res(r), span });
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            let mut text = String::new();
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                text.push(chars[j]);
                j += 1;
            }
            let k = j - i;
            advance(&mut i, &mut line, &mut col, k);
            if text == "_" {
                toks.push(Token { tok: Tok::Under, span });
            } else {
                toks.push(Token { tok: Tok::Ident(text), span });
            }
            continue;
        }
        let two: String = chars[i..(i + 2).min(n)].iter().collect();
        let (tok, len) = match two.as_str() {
            ":-" => (Tok::Implies, 2),
            ".." => (Tok::DotDot, 2),
            "<=" => (Tok::Le, 2),
            ">=" => (Tok::Ge, 2),
            _ => match c {
                '(' => (Tok::LParen, 1),
                ')' => (Tok::RParen, 1),
                '{' => (Tok::LBrace, 1),
                '}' => (Tok::RBrace, 1),
                ',' => (Tok::Comma, 1),
                '.' => (Tok::Dot, 1),
                ':' => (Tok::Colon, 1),
                '=' => (Tok::Eq, 1),
                '<' => (Tok::Lt, 1),
                '>' => (Tok::Gt, 1),
                '+' => (Tok::Plus, 1),
                '-' => (Tok::Minus, 1),
                '*' => (Tok::Star, 1),
                '/' => (Tok::Slash, 1),
                _ => {
                    return Err(LexError {
                        span,
                        message: format!("unexpected character `{}`", c),
                    })
                }
            },
        };
        advance(&mut i, &mut line, &mut col, len);
        toks.push(Token { tok, span });
    }
    toks.push(Token {
        tok: Tok::Eof,
        span: Span { line, col },
    });
    Ok(toks)
}
