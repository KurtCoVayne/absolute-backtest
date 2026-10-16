//! The query forms of V-8 and K-7.2, parsed. A `?` query's body is kept as
//! text: it is a rule body of the language, parsed and checked with the
//! program when the query is prepared (Definition V-8.1).

use crate::kernel::time::{self, parse_timestamp};

/// A query of the language (V-8's forms).
#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    /// `? BODY [at [VAR =] TIME | at VAR] [for VAR = KEY] [over TIME .. TIME]`:
    /// the solutions of the body at a bar, at every bar of a window, or at
    /// every bar of the run (`at VAR`).
    Ask(Ask),
    /// `why R(c, ...)`: the derivation of a tuple, down to supplied tuples.
    Why {
        relation: String,
        args: Vec<String>,
    },
    /// `why not R(c, ...) [in written order]`: for each rule of R, the first
    /// literal that excluded the instance, and recursively the atom that
    /// failed.
    WhyNot {
        relation: String,
        args: Vec<String>,
        written: bool,
    },
    /// `trace R at TIME`: every instance of R's rules attempted at the bar,
    /// fired or excluded, with the literal that excluded it.
    Trace {
        relation: String,
        at: i64,
    },
    /// `coverage [over TIME .. TIME]`: the relations on a path to `decide`
    /// that are empty over the window, with the first of each path.
    Coverage {
        over: Option<(i64, i64)>,
    },
    Show(Show),
    /// `ledger R into NAME`: R's extension over the run, written through
    /// the data layer as `kernel.<run>.NAME`.
    Ledger {
        relation: String,
        into: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub body: String,
    pub time_var: Option<String>,
    pub at: Option<i64>,
    pub every_bar: bool,
    pub bind: Option<(String, String)>,
    pub over: Option<(i64, i64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Show {
    Program,
    Plan(Option<String>),
    Schedule,
    Calendars,
    Contract,
}

/// A time of a query: `YYYY-MM-DD[THH:MM[:SS]]`, or a month `YYYY-MM`,
/// which as the end of a range is the month's last instant.
fn time_of(s: &str, end: bool) -> Result<i64, String> {
    let s = s.trim().trim_matches('"');
    if let Some(t) = parse_timestamp(s) {
        return Ok(t);
    }
    if let Some(first) = parse_timestamp(&format!("{}-01", s)) {
        if !end {
            return Ok(first);
        }
        let (y, m) = time::month_key(first);
        let next = if m == 12 { format!("{}-01-01", y + 1) } else { format!("{}-{:02}-01", y, m + 1) };
        return parse_timestamp(&next).map(|t| t - 1).ok_or_else(|| format!("`{}`: no month after it", s));
    }
    Err(format!("`{}` is not a time (YYYY-MM-DD, YYYY-MM-DDTHH:MM or a month YYYY-MM)", s))
}

/// `T0 .. T1` (spaces optional).
fn range(s: &str) -> Result<(i64, i64), String> {
    let (a, b) = s.split_once("..").ok_or_else(|| format!("`over` takes TIME .. TIME, not `{}`", s))?;
    let (a, b) = (time_of(a, false)?, time_of(b, true)?);
    if b < a {
        return Err(format!("the window `{}` ends before it starts", s.trim()));
    }
    Ok((a, b))
}

/// The words of `s` outside parentheses and quotes, with their offsets.
fn top_words(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, None);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' | '[' if !quoted => depth += 1,
            ')' | ']' if !quoted => depth -= 1,
            _ => {}
        }
        let blank = c.is_whitespace() && !quoted && depth == 0;
        match (blank, start) {
            (false, None) => start = Some(i),
            (true, Some(b)) => {
                out.push((b, &s[b..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(b) = start {
        out.push((b, &s[b..]));
    }
    out
}

/// `R(a, b(c, d), "e")`: the name and the argument texts, quotes removed.
fn atom(s: &str) -> Result<(String, Vec<String>), String> {
    let s = s.trim();
    let open = s.find('(').ok_or_else(|| format!("`{}` is not an atom R(...)", s))?;
    if !s.ends_with(')') {
        return Err(format!("`{}`: the argument list does not close", s));
    }
    let name = s[..open].trim().to_string();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '@') {
        return Err(format!("`{}` is not a relation name", name));
    }
    let inner = &s[open + 1..s.len() - 1];
    let mut args = Vec::new();
    let (mut depth, mut quoted, mut cur) = (0i32, false, String::new());
    for c in inner.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' if !quoted => {
                depth += 1;
                cur.push(c)
            }
            ')' if !quoted => {
                depth -= 1;
                cur.push(c)
            }
            ',' if depth == 0 && !quoted => {
                args.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        args.push(cur.trim().to_string());
    }
    Ok((name, args))
}

fn is_var(w: &str) -> bool {
    w.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && w.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Parses a query of the language.
pub fn parse(text: &str) -> Result<Query, String> {
    let text = text.trim().trim_end_matches('.');
    let words = top_words(text);
    let first = words.first().map(|w| w.1).unwrap_or("");
    let rest = |i: usize| words.get(i).map(|w| text[w.0..].trim()).unwrap_or("");
    match first {
        _ if first.starts_with('?') => {
            // The body ends at the first clause keyword outside parentheses.
            let body_start = if first == "?" { words.get(1).map(|w| w.0).unwrap_or(text.len()) } else { 1 };
            let cut = words.iter().position(|w| w.0 >= body_start && matches!(w.1, "at" | "for" | "over"));
            let body_end = cut.map(|i| words[i].0).unwrap_or(text.len());
            let body = text[body_start..body_end].trim().to_string();
            if body.is_empty() {
                return Err("`?` takes a rule body: `? R(A, T, X) at T = 2020-03-06`".into());
            }
            let mut ask = Ask {
                body,
                time_var: None,
                at: None,
                every_bar: false,
                bind: None,
                over: None,
            };
            let clauses: Vec<&str> = cut.map(|i| words[i..].iter().map(|w| w.1).collect()).unwrap_or_default();
            let mut i = 0;
            while i < clauses.len() {
                match clauses[i] {
                    "at" => {
                        let a = clauses.get(i + 1).ok_or("`at` takes a time, VAR = TIME, or a variable")?;
                        if clauses.get(i + 2) == Some(&"=") {
                            let t = clauses.get(i + 3).ok_or("`at VAR =` takes a time")?;
                            ask.time_var = Some(a.to_string());
                            ask.at = Some(time_of(t, false)?);
                            i += 4;
                        } else if let Some((v, t)) = a.split_once('=') {
                            ask.time_var = Some(v.to_string());
                            ask.at = Some(time_of(t, false)?);
                            i += 2;
                        } else if is_var(a) {
                            ask.time_var = Some(a.to_string());
                            ask.every_bar = true;
                            i += 2;
                        } else {
                            ask.at = Some(time_of(a, false)?);
                            i += 2;
                        }
                    }
                    "for" => {
                        let v = clauses.get(i + 1).ok_or("`for` takes VAR = KEY")?;
                        let (var, key, used) = match (v.split_once('='), clauses.get(i + 2)) {
                            (Some((var, key)), _) if !key.is_empty() => (var.to_string(), key.to_string(), 2),
                            (_, Some(&"=")) => (v.to_string(), clauses.get(i + 3).ok_or("`for VAR =` takes a key")?.to_string(), 4),
                            _ => return Err("`for` takes VAR = KEY, as `for A = \"ES\"`".into()),
                        };
                        if !is_var(&var) {
                            return Err(format!("`for {} = ...`: `{}` is not a variable", var, var));
                        }
                        ask.bind = Some((var, key.trim_matches('"').to_string()));
                        i += used;
                    }
                    "over" => {
                        let mut span = String::new();
                        let mut j = i + 1;
                        while j < clauses.len() && !matches!(clauses[j], "at" | "for" | "over") {
                            span.push_str(clauses[j]);
                            j += 1;
                        }
                        ask.over = Some(range(&span)?);
                        i = j;
                    }
                    other => return Err(format!("`{}` is not a clause of `?` (at, for, over)", other)),
                }
            }
            if ask.at.is_some() && ask.over.is_some() {
                return Err("`at` and `over` are two windows; give one".into());
            }
            Ok(Query::Ask(ask))
        }
        "why" if words.get(1).map(|w| w.1) == Some("not") => {
            let tail = rest(2);
            let (atom_text, written) = match tail.strip_suffix("in written order") {
                Some(a) => (a.trim(), true),
                None => (tail, false),
            };
            let (relation, args) = atom(atom_text)?;
            Ok(Query::WhyNot { relation, args, written })
        }
        "why" => {
            let (relation, args) = atom(rest(1))?;
            Ok(Query::Why { relation, args })
        }
        "trace" => {
            let relation = words.get(1).map(|w| w.1.to_string()).ok_or("`trace` takes a relation and a bar: `trace decide at 2020-03-06`")?;
            if words.get(2).map(|w| w.1) != Some("at") {
                return Err("`trace R` takes `at TIME`".into());
            }
            let at = time_of(rest(3), false)?;
            Ok(Query::Trace { relation, at })
        }
        "coverage" => match words.get(1).map(|w| w.1) {
            None => Ok(Query::Coverage { over: None }),
            Some("over") => Ok(Query::Coverage { over: Some(range(rest(2))?) }),
            Some(other) => Err(format!("`coverage` takes `over TIME .. TIME`, not `{}`", other)),
        },
        "show" => {
            let what = words.get(1).map(|w| w.1).unwrap_or("");
            let arg = words.get(2).map(|w| w.1.to_string());
            Ok(Query::Show(match what {
                "program" => Show::Program,
                "plan" => Show::Plan(arg),
                "schedule" => Show::Schedule,
                "calendars" => Show::Calendars,
                "contract" => Show::Contract,
                other => return Err(format!("`show {}`: the forms are show program, show plan [R], show schedule, show calendars, show contract", other)),
            }))
        }
        "ledger" => {
            let relation = words.get(1).map(|w| w.1.to_string()).ok_or("`ledger` takes R into NAME")?;
            if words.get(2).map(|w| w.1) != Some("into") {
                return Err("`ledger R` takes `into NAME`".into());
            }
            let into = words.get(3).map(|w| w.1.to_string()).ok_or("`ledger R into` takes a name")?;
            if !into.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                return Err(format!("`{}` is not a relation name", into));
            }
            Ok(Query::Ledger { relation, into })
        }
        _ => Err(format!(
            "`{}` begins no query; the forms are `? BODY at|for|over ...`, `why R(...)`, `why not R(...)`, `trace R at T`, `coverage over W`, `show ...` and `ledger R into NAME`",
            text
        )),
    }
}
