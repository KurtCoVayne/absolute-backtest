//! Tokeniser for the surface syntax of the second formulation.

use crate::ir::{Duration, Resolution, Span};
use crate::kernel::time::{days_from_civil, days_in_month, DAY};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// `@1d`, `@1m`, `@1w`.
    Res(Resolution),
    Duration(Duration),
    /// `2000-01-01`, as a timestamp.
    Date(i64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Dot,
    DotDot,
    Colon,
    Semi,
    Implies,
    Arrow,
    Pipe,
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
    Question,
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
            Tok::Date(t) => write!(f, "`{}`", crate::kernel::time::format_timestamp(*t)),
            Tok::LParen => write!(f, "`(`"),
            Tok::RParen => write!(f, "`)`"),
            Tok::LBrace => write!(f, "`{{`"),
            Tok::RBrace => write!(f, "`}}`"),
            Tok::LBracket => write!(f, "`[`"),
            Tok::RBracket => write!(f, "`]`"),
            Tok::Comma => write!(f, "`,`"),
            Tok::Dot => write!(f, "`.`"),
            Tok::DotDot => write!(f, "`..`"),
            Tok::Colon => write!(f, "`:`"),
            Tok::Semi => write!(f, "`;`"),
            Tok::Implies => write!(f, "`:-`"),
            Tok::Arrow => write!(f, "`->`"),
            Tok::Pipe => write!(f, "`|`"),
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
            Tok::Question => write!(f, "`?`"),
            Tok::Eof => write!(f, "end of input"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// A number's lexeme as written (`2026.10`), for a version.
    pub raw: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LexError {
    pub span: Span,
    pub message: String,
}

/// A duration suffix attached to a number: `20d`, `2w`, `3mo`, `1y`,
/// `30min`, `2h`, `15s`.
fn duration_suffix(amount: i64, suffix: &str) -> Option<Duration> {
    Some(match suffix {
        "d" => Duration::days(amount),
        "w" => Duration::days(amount * 7),
        "mo" => Duration::months(amount),
        "y" => Duration::months(amount * 12),
        "min" => Duration::secs(amount * 60),
        "h" => Duration::secs(amount * 3600),
        "s" => Duration::secs(amount),
        _ => return None,
    })
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
            toks.push(Token { tok: Tok::Str(s), span, raw: None });
            continue;
        }
        if c.is_ascii_digit() {
            // A date: YYYY-MM-DD, optionally THH:MM[:SS].
            if i + 10 <= n
                && chars[i..i + 4].iter().all(|c| c.is_ascii_digit())
                && chars[i + 4] == '-'
                && chars[i + 5..i + 7].iter().all(|c| c.is_ascii_digit())
                && chars[i + 7] == '-'
                && chars[i + 8..i + 10].iter().all(|c| c.is_ascii_digit())
            {
                let text: String = chars[i..i + 10].iter().collect();
                let mut j = i + 10;
                let mut extra = String::new();
                if j < n && chars[j] == 'T' && j + 5 < n && chars[j + 1].is_ascii_digit() {
                    while j < n && (chars[j].is_ascii_digit() || chars[j] == ':' || chars[j] == 'T') {
                        extra.push(chars[j]);
                        j += 1;
                    }
                }
                let full = format!("{}{}", text, extra);
                let t = crate::kernel::time::parse_timestamp(&full).ok_or_else(|| LexError {
                    span,
                    message: format!("`{}` is not a date (YYYY-MM-DD)", full),
                })?;
                let k = j - i;
                advance(&mut i, &mut line, &mut col, k);
                toks.push(Token { tok: Tok::Date(t), span, raw: None });
                continue;
            }
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
            if j < n && (chars[j] == 'e' || chars[j] == 'E') && j + 1 < n && (chars[j + 1].is_ascii_digit() || chars[j + 1] == '-' || chars[j + 1] == '+') {
                is_float = true;
                text.push('e');
                j += 1;
                if chars[j] == '-' || chars[j] == '+' {
                    text.push(chars[j]);
                    j += 1;
                }
                while j < n && chars[j].is_ascii_digit() {
                    text.push(chars[j]);
                    j += 1;
                }
            }
            // A suffix directly attached: a duration (20d, 30min), or an
            // identifier that starts with digits (the root `6J`).
            let mut suffix = String::new();
            let mut k = j;
            while k < n && (chars[k].is_alphanumeric() || chars[k] == '_') {
                suffix.push(chars[k]);
                k += 1;
            }
            let tok = if !suffix.is_empty() {
                match (is_float, text.parse::<i64>().ok().and_then(|a| duration_suffix(a, &suffix))) {
                    (false, Some(d)) => {
                        j = k;
                        Tok::Duration(d)
                    }
                    _ => {
                        let ident: String = chars[i..k].iter().collect();
                        j = k;
                        Tok::Ident(ident)
                    }
                }
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
            toks.push(Token {
                tok,
                span,
                raw: Some(chars[i - k..i].iter().collect()),
            });
            continue;
        }
        if c == '@' {
            // `@1d` is a resolution; `@T` is the temporal-key marker and
            // `R@C` a calendar instance.
            let mut j = i + 1;
            let mut text = String::new();
            while j < n && chars[j].is_ascii_alphanumeric() {
                text.push(chars[j]);
                j += 1;
            }
            if text.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                let r = Resolution::parse(&text).ok_or_else(|| LexError {
                    span,
                    message: format!("unknown resolution `@{}` (use @1m, @5m, @15m, @30m, @1h, @1d, @1w)", text),
                })?;
                let k = j - i;
                advance(&mut i, &mut line, &mut col, k);
                toks.push(Token { tok: Tok::Res(r), span, raw: None });
            } else {
                advance(&mut i, &mut line, &mut col, 1);
                toks.push(Token { tok: Tok::At, span, raw: None });
            }
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
                toks.push(Token { tok: Tok::Under, span, raw: None });
            } else {
                toks.push(Token {
                    tok: Tok::Ident(text),
                    span,
                    raw: None,
                });
            }
            continue;
        }
        let two: String = chars[i..(i + 2).min(n)].iter().collect();
        let (tok, len) = match two.as_str() {
            ":-" => (Tok::Implies, 2),
            "->" => (Tok::Arrow, 2),
            ".." => (Tok::DotDot, 2),
            "<=" => (Tok::Le, 2),
            ">=" => (Tok::Ge, 2),
            _ => match c {
                '(' => (Tok::LParen, 1),
                ')' => (Tok::RParen, 1),
                '{' => (Tok::LBrace, 1),
                '}' => (Tok::RBrace, 1),
                '[' => (Tok::LBracket, 1),
                ']' => (Tok::RBracket, 1),
                ',' => (Tok::Comma, 1),
                '.' => (Tok::Dot, 1),
                ':' => (Tok::Colon, 1),
                ';' => (Tok::Semi, 1),
                '|' => (Tok::Pipe, 1),
                '=' => (Tok::Eq, 1),
                '<' => (Tok::Lt, 1),
                '>' => (Tok::Gt, 1),
                '+' => (Tok::Plus, 1),
                '-' => (Tok::Minus, 1),
                '*' => (Tok::Star, 1),
                '/' => (Tok::Slash, 1),
                '?' => (Tok::Question, 1),
                _ => {
                    return Err(LexError {
                        span,
                        message: format!("unexpected character `{}`", c),
                    })
                }
            },
        };
        advance(&mut i, &mut line, &mut col, len);
        toks.push(Token { tok, span, raw: None });
    }
    toks.push(Token {
        tok: Tok::Eof,
        span: Span { line, col },
        raw: None,
    });
    Ok(toks)
}

/// The timestamp of a civil date, for the parser's own literals.
pub fn date(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_dates_and_roots() {
        let toks = lex("30min 20d 2w 6mo 1y 2000-01-01 6J ETH @1d @1w close@weekly 1e-12 1_000_000").unwrap();
        let kinds: Vec<String> = toks.iter().map(|t| format!("{}", t.tok)).collect();
        assert_eq!(
            kinds,
            vec![
                "`30min`",
                "`20d`",
                "`14d`",
                "`6mo`",
                "`1y`",
                "`2000-01-01`",
                "`6J`",
                "`ETH`",
                "`@1d`",
                "`@1w`",
                "`close`",
                "`@`",
                "`weekly`",
                "`0.000000000001`",
                "`1000000`",
                "end of input"
            ]
        );
    }

    #[test]
    fn operators() {
        let toks = lex(":- -> .. <= >= [ ] | ; ?").unwrap();
        let kinds: Vec<Tok> = toks.into_iter().map(|t| t.tok).collect();
        assert_eq!(
            kinds,
            vec![
                Tok::Implies,
                Tok::Arrow,
                Tok::DotDot,
                Tok::Le,
                Tok::Ge,
                Tok::LBracket,
                Tok::RBracket,
                Tok::Pipe,
                Tok::Semi,
                Tok::Question,
                Tok::Eof
            ]
        );
    }
}
