//! The plan grammar (Q-2.1): `study NAME { ... }`, `template NAME(P, ...)
//! { ... }` and `study NAME = TEMPLATE(P = value, ...)`. A template is its
//! body's text with each parameter standing for a value; an instance is the
//! body with every parameter, as a whole word, replaced by its value, then
//! read as a study's (Q-2.1: templates are instantiated by substitution
//! before hashing). Parameter values are literal texts the kernel parses.

use crate::time::{format_timestamp, parse_timestamp};

use super::{At, Budget, Claim, Expect, Holdout, Input, Op, Phase, Plan, Point, Policy, ProgramRef, Ref, ReportSpec, Step, WalkForward, Window};

/// A unit of a plan file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    Study { name: String, body: String },
    Template { name: String, params: Vec<String>, body: String },
    Instance { name: String, template: String, args: Vec<(String, String)> },
}

impl Unit {
    pub fn name(&self) -> &str {
        match self {
            Unit::Study { name, .. } | Unit::Template { name, .. } | Unit::Instance { name, .. } => name,
        }
    }
}

/// The text without its comments: `#` to the end of the line, outside a
/// quoted string.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut quoted = false;
        let mut cut = line.len();
        for (i, c) in line.char_indices() {
            match c {
                '"' => quoted = !quoted,
                '#' if !quoted => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The index just after the brace matching the one at `open`.
fn matching_brace(text: &str, open: usize) -> Result<usize, String> {
    let mut depth = 0usize;
    let mut quoted = false;
    for (i, c) in text[open..].char_indices() {
        match c {
            '"' => quoted = !quoted,
            '{' if !quoted => depth += 1,
            '}' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return Ok(open + i + 1);
                }
            }
            _ => {}
        }
    }
    Err(format!("the brace at offset {} is not closed", open))
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_') && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// The units of a plan file.
pub fn parse(text: &str) -> Result<Vec<Unit>, String> {
    let text = strip_comments(text);
    let mut units = Vec::new();
    let mut rest = text.as_str();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Ok(units);
        }
        let (kw, after) = rest.split_once(char::is_whitespace).ok_or_else(|| format!("`{}` is not a unit: study or template", rest.trim()))?;
        let after = after.trim_start();
        match kw {
            "study" => {
                let name_end = after.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(after.len());
                let name = &after[..name_end];
                if !is_name(name) {
                    return Err(format!("`study {}`: a study is named by a word", after.lines().next().unwrap_or("")));
                }
                let tail = after[name_end..].trim_start();
                if let Some(inst) = tail.strip_prefix('=') {
                    // study NAME = TEMPLATE(P = v, ...)
                    let inst = inst.trim_start();
                    let open = inst.find('(').ok_or_else(|| format!("study {} = ...: a template's arguments are in parentheses", name))?;
                    let close = inst.find(')').ok_or_else(|| format!("study {} = ...: the arguments are not closed", name))?;
                    let template = inst[..open].trim().to_string();
                    let mut args = Vec::new();
                    for a in inst[open + 1..close].split(',').map(str::trim).filter(|a| !a.is_empty()) {
                        let (k, v) = a.split_once('=').ok_or_else(|| format!("study {}: `{}` is not PARAM = value", name, a))?;
                        args.push((k.trim().to_string(), v.trim().to_string()));
                    }
                    units.push(Unit::Instance {
                        name: name.to_string(),
                        template,
                        args,
                    });
                    rest = &inst[close + 1..];
                } else if tail.starts_with('{') {
                    let offset = text.len() - tail.len();
                    let end = matching_brace(&text, offset)?;
                    units.push(Unit::Study {
                        name: name.to_string(),
                        body: text[offset + 1..end - 1].to_string(),
                    });
                    rest = &text[end..];
                } else {
                    return Err(format!("study {}: a body in braces or `= TEMPLATE(...)` follows the name", name));
                }
            }
            "template" => {
                let open = after.find('(').ok_or_else(|| "a template declares its parameters in parentheses".to_string())?;
                let close = after.find(')').ok_or_else(|| "a template's parameters are not closed".to_string())?;
                let name = after[..open].trim().to_string();
                if !is_name(&name) {
                    return Err(format!("`template {}`: a template is named by a word", name));
                }
                let params: Vec<String> = after[open + 1..close].split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                let tail = after[close + 1..].trim_start();
                if !tail.starts_with('{') {
                    return Err(format!("template {}: its body follows in braces", name));
                }
                let offset = text.len() - tail.len();
                let end = matching_brace(&text, offset)?;
                units.push(Unit::Template {
                    name,
                    params,
                    body: text[offset + 1..end - 1].to_string(),
                });
                rest = &text[end..];
            }
            other => return Err(format!("`{}` begins no unit: a plan file holds studies and templates", other)),
        }
    }
}

/// The body with every parameter, as a whole word, replaced by its value.
fn substitute(body: &str, args: &[(String, String)]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match args.iter().find(|(k, _)| k == word) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in body.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The plan a file names: a study, or a template's instance.
pub fn plan(units: &[Unit], name: &str) -> Result<Plan, String> {
    let unit = units.iter().find(|u| u.name() == name && !matches!(u, Unit::Template { .. })).ok_or_else(|| {
        let studies: Vec<&str> = units.iter().filter(|u| !matches!(u, Unit::Template { .. })).map(Unit::name).collect();
        format!("no study `{}` (the file has {})", name, if studies.is_empty() { "none".to_string() } else { studies.join(", ") })
    })?;
    match unit {
        Unit::Study { name, body } => study(name, body),
        Unit::Instance { name, template, args } => {
            let Some(Unit::Template { params, body, .. }) = units.iter().find(|u| matches!(u, Unit::Template { name: n, .. } if n == template)) else {
                return Err(format!("study {} instantiates no template `{}`", name, template));
            };
            for (k, _) in args {
                if !params.contains(k) {
                    return Err(format!("study {}: `{}` is not a parameter of template {} ({})", name, k, template, params.join(", ")));
                }
            }
            if let Some(missing) = params.iter().find(|p| !args.iter().any(|(k, _)| k == *p)) {
                return Err(format!("study {}: template {}'s parameter `{}` has no value", name, template, missing));
            }
            study(name, &substitute(body, args))
        }
        Unit::Template { .. } => unreachable!("templates are not studies"),
    }
}

/// `text` split at every `sep` outside parentheses, braces, brackets and
/// quotes.
pub(crate) fn split_top(text: &str, seps: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' | '{' | '[' if !quoted => depth += 1,
            ')' | '}' | ']' if !quoted => depth -= 1,
            _ => {}
        }
        if depth == 0 && !quoted && seps.contains(&c) {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// The entries of a block: separated by `;` or new lines outside brackets,
/// blank ones dropped, whitespace collapsed.
fn entries(block: &str) -> Vec<String> {
    split_top(block, &['\n', ';'])
        .iter()
        .map(|e| e.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|e| !e.is_empty())
        .collect()
}

/// `k = v` entries, separated by `;`, `,` or new lines outside brackets.
fn pairs(block: &str, what: &str) -> Result<Point, String> {
    pairs_by(block, &['\n', ';', ','], what)
}

/// `k = v` entries, separated by `seps` outside brackets.
fn pairs_by(block: &str, seps: &[char], what: &str) -> Result<Point, String> {
    split_top(block, seps)
        .iter()
        .map(|e| e.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|e| !e.is_empty())
        .map(|e| {
            e.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .filter(|(k, v)| is_name(k) && !v.is_empty())
                .ok_or_else(|| format!("{}: `{}` is not NAME = value", what, e))
        })
        .collect()
}

/// `{ k = v, ... }`.
fn braced_pairs(s: &str, what: &str) -> Result<Point, String> {
    let inner = s
        .trim()
        .strip_prefix('{')
        .and_then(|r| r.strip_suffix('}'))
        .ok_or_else(|| format!("{}: `{}` is not {{ k = v, ... }}", what, s.trim()))?;
    pairs(inner, what)
}

/// The text before the first of `words` (each surrounded by spaces) outside
/// brackets, and the rest from that word on.
fn cut_at<'a>(s: &'a str, words: &[&str]) -> (&'a str, &'a str) {
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            ' ' if depth == 0 => {
                for w in words {
                    let end = i + 1 + w.len();
                    if s[i + 1..].starts_with(w) && (end == s.len() || bytes[end] == b' ') {
                        return (s[..i].trim(), s[i + 1..].trim());
                    }
                }
            }
            _ => {}
        }
    }
    (s.trim(), "")
}

/// A claim's line (Q-5.1): `NAME : TEST on INPUT [over WINDOW] [with {
/// ... }] expect OUTCOME [at LEVEL] [before | after] [warn | block]`.
pub fn claim(line: &str) -> Result<Claim, String> {
    let (name, rest) = line.split_once(':').ok_or_else(|| format!("`{}`: a claim is `NAME : TEST on INPUT ... expect OUTCOME`", line))?;
    let name = name.trim().to_string();
    if !is_name(&name) {
        return Err(format!("`{}`: a claim is named by a word", line));
    }
    let (test, rest) = rest.trim().split_once(" on ").ok_or_else(|| format!("claim {}: `TEST on INPUT`", name))?;
    let (input, mut rest) = cut_at(rest, &["over", "with", "expect"]);
    let input = Input::parse(input).map_err(|e| format!("claim {}: {}", name, e))?;
    let mut over = None;
    let mut params = vec![];
    if let Some(r) = rest.strip_prefix("over ") {
        let (w, r2) = cut_at(r, &["with", "expect"]);
        over = Some(window(w)?);
        rest = r2;
    }
    if let Some(r) = rest.strip_prefix("with ") {
        let (p, r2) = cut_at(r, &["expect"]);
        params = braced_pairs(p, &format!("claim {}'s parameters", name))?;
        rest = r2;
    }
    let rest = rest.strip_prefix("expect ").ok_or_else(|| format!("claim {}: `expect OUTCOME` follows the input", name))?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let num = |w: Option<&&str>| -> Result<f64, String> {
        w.and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("claim {}: `{}` needs a number", name, rest))
    };
    let op = |w: Option<&&str>| -> Result<Op, String> {
        match w.copied() {
            Some("<") => Ok(Op::Lt),
            Some("<=") => Ok(Op::Le),
            Some(">") => Ok(Op::Gt),
            Some(">=") => Ok(Op::Ge),
            _ => Err(format!("claim {}: `{}`: a comparison is <, <=, > or >=", name, rest)),
        }
    };
    let (expect, used) = match words.first().copied() {
        Some("reject") => (Expect::Reject, 1),
        Some("accept") => (Expect::Accept, 1),
        Some("statistic") => (Expect::Statistic(op(words.get(1))?, num(words.get(2))?), 3),
        Some("effect") => (Expect::Effect(op(words.get(1))?, num(words.get(2))?), 3),
        Some("interval") if words.get(1) == Some(&"excludes") => (Expect::IntervalExcludes(num(words.get(2))?), 3),
        Some("interval") => (Expect::Interval(op(words.get(1))?, num(words.get(2))?), 3),
        _ => {
            return Err(format!(
                "claim {}: `{}` is not an outcome: reject, accept, statistic OP V, effect OP V, interval OP V or interval excludes V",
                name, rest
            ))
        }
    };
    let mut level = None;
    let mut phase = Phase::After;
    let mut policy = Policy::Warn;
    let mut i = used;
    while i < words.len() {
        match words[i] {
            "at" => {
                let l = num(words.get(i + 1))?;
                if !(l > 0.0 && l < 1.0) {
                    return Err(format!("claim {}: the level {} is not in (0, 1)", name, l));
                }
                level = Some(l);
                i += 2;
                continue;
            }
            "before" => phase = Phase::Before,
            "after" => phase = Phase::After,
            "warn" => policy = Policy::Warn,
            "block" => policy = Policy::Block,
            other => return Err(format!("claim {}: `{}` is not at LEVEL, before, after, warn or block", name, other)),
        }
        i += 1;
    }
    Ok(Claim {
        name,
        test: test.trim().to_string(),
        input,
        over,
        params,
        expect,
        level,
        phase,
        policy,
    })
}

/// `DATE .. DATE`, both ends written back as the kernel prints a time (so
/// that one window has one text in the normal form).
fn window(s: &str) -> Result<Window, String> {
    let (from, to) = s.split_once("..").ok_or_else(|| format!("`{}` is not a window DATE .. DATE", s))?;
    let end = |e: &str| parse_timestamp(e).ok_or_else(|| format!("`{}` is not a time (YYYY-MM-DD[THH:MM[:SS]])", e.trim()));
    let (a, b) = (end(from)?, end(to)?);
    if a > b {
        return Err(format!("the window `{}` ends before it starts", s.trim()));
    }
    Ok(Window {
        from: format_timestamp(a),
        to: format_timestamp(b),
    })
}

fn months(s: &str) -> Result<u32, String> {
    let s = s.trim();
    if let Some(y) = s.strip_suffix('y') {
        return y.parse::<u32>().map(|y| 12 * y).map_err(|_| format!("`{}` is not a number of years", s));
    }
    s.strip_suffix("mo")
        .unwrap_or(s)
        .parse::<u32>()
        .map_err(|_| format!("`{}` is not a number of months (6mo) or years (2y)", s))
}

/// `none`, `trailing:Ny`, `trailing:Nmo`, `trailing:N%`, `after TIME` or
/// `blocks:K:Nmo[:seed]` (Q-2.1, E-97).
pub fn holdout(s: &str) -> Result<Holdout, String> {
    let s = s.trim();
    if s == "none" {
        return Ok(Holdout::None);
    }
    if let Some(r) = s.strip_prefix("trailing:") {
        let r = r.trim();
        if let Some(p) = r.strip_suffix('%') {
            let percent: u32 = p.parse().map_err(|_| format!("`{}` is not a percentage of the window (trailing:20%)", r))?;
            if percent == 0 || percent >= 100 {
                return Err(format!("`{}`: a trailing hold-out takes between 1 % and 99 % of the window", r));
            }
            return Ok(Holdout::TrailingShare { percent });
        }
        let months = months(r).map_err(|_| format!("`{}` is not a number of years (trailing:2y), months (trailing:6mo) or a percentage (trailing:20%)", r))?;
        if months == 0 {
            return Err("a trailing hold-out needs at least one month".into());
        }
        return Ok(Holdout::Trailing { months });
    }
    if let Some(r) = s.strip_prefix("after ").or_else(|| s.strip_prefix("after\t")) {
        let r = r.trim();
        let time = parse_timestamp(r).ok_or_else(|| format!("`{}` is not a time (after YYYY-MM-DD[THH:MM[:SS[.NNNNNNNNN]]], UTC)", r))?;
        return Ok(Holdout::After { time });
    }
    if let Some(r) = s.strip_prefix("blocks:") {
        let parts: Vec<&str> = r.split(':').collect();
        let (count, months_s, seed) = match parts.as_slice() {
            [c, m] => (*c, *m, "1"),
            [c, m, s] => (*c, *m, *s),
            _ => return Err(format!("`{}` is not blocks:K:Nmo[:seed]", s)),
        };
        let count: u32 = count.parse().map_err(|_| format!("`{}` is not a number of blocks", count))?;
        let months = months(months_s)?;
        let seed: u64 = seed.parse().map_err(|_| format!("`{}` is not a seed", seed))?;
        if count == 0 || months == 0 {
            return Err("a block hold-out needs at least one block of one month".into());
        }
        return Ok(Holdout::Blocks { count, months, seed });
    }
    Err(format!("`{}` is not a hold-out: none, trailing:Ny, trailing:Nmo, trailing:N%, after TIME or blocks:K:Nmo[:seed]", s))
}

fn walk_forward(s: &str) -> Result<WalkForward, String> {
    WalkForward::parse(s.trim())
}

/// A point written `{ k = v, ... }` or `best(GRID)`.
fn at(s: &str) -> Result<At, String> {
    let s = s.trim();
    if let Some(g) = s.strip_prefix("best(").and_then(|r| r.strip_suffix(')')) {
        return Ok(At::Best(g.trim().to_string()));
    }
    if !s.starts_with('{') {
        return Err(format!("`{}` is not a point: {{ k = v, ... }} or best(GRID)", s));
    }
    Ok(At::Point(braced_pairs(s, "a point")?))
}

/// One step's line.
fn step(line: &str) -> Result<Step, String> {
    let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    // `NAME: ...` for the steps that take a body after a colon.
    let colon = |what: &str| -> Result<(String, String), String> {
        let (n, b) = rest.split_once(':').ok_or_else(|| format!("`{}`: a {} step is `{} NAME: ...`", line, what, what))?;
        Ok((n.trim().to_string(), b.trim().to_string()))
    };
    let list = |s: &str| -> Vec<String> { split_top(s, &[',']).iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect() };
    let s = match kind {
        "run" => {
            let (name, mut tail) = cut_at(rest, &["at", "over", "observe"]);
            let mut at_v = None;
            let mut over = None;
            let mut observe = Vec::new();
            if let Some(r) = tail.strip_prefix("at ") {
                let (p, r2) = cut_at(r, &["over", "observe"]);
                at_v = Some(at(p)?);
                tail = r2;
            }
            if let Some(r) = tail.strip_prefix("over ") {
                let (w, r2) = cut_at(r, &["observe"]);
                over = Some(window(w)?);
                tail = r2;
            }
            if let Some(r) = tail.strip_prefix("observe ") {
                observe = list(r);
                tail = "";
            }
            if !tail.is_empty() {
                return Err(format!("`{}`: run NAME [at POINT] [over WINDOW] [observe R, ...]", line));
            }
            Step::Run {
                name: name.to_string(),
                at: at_v,
                over,
                observe,
            }
        }
        "grid" => Step::Grid { name: rest.to_string() },
        "walk_forward" => Step::WalkForward { name: rest.to_string() },
        "reveal" => {
            let (n, of) = rest.split_once(" of ").ok_or_else(|| format!("`{}`: reveal NAME of RUN|best(GRID)", line))?;
            Step::Reveal {
                name: n.trim().to_string(),
                of: Ref::parse(of),
            }
        }
        "test" => {
            let (name, body) = colon("test")?;
            let (test, r) = body.split_once(" on ").ok_or_else(|| format!("`{}`: test NAME: TEST on INPUT [with {{...}}] [over WINDOW]", line))?;
            let (input, mut tail) = cut_at(r, &["with", "over"]);
            let input = Input::parse(input).map_err(|e| format!("test {}: {}", name, e))?;
            let mut params = vec![];
            let mut over = None;
            if let Some(r) = tail.strip_prefix("with ") {
                let (p, r2) = cut_at(r, &["over"]);
                params = braced_pairs(p, &format!("test {}'s parameters", name))?;
                tail = r2;
            }
            if let Some(r) = tail.strip_prefix("over ") {
                over = Some(window(r)?);
                tail = "";
            }
            if !tail.is_empty() {
                return Err(format!("`{}`: test NAME: TEST on INPUT [with {{...}}] [over WINDOW]", line));
            }
            Step::Test {
                name,
                test: test.trim().to_string(),
                input,
                params,
                over,
            }
        }
        "compare" => {
            let (name, body) = colon("compare")?;
            let (runs, by) = body.split_once(" by ").ok_or_else(|| format!("`{}`: compare NAME: RUNS by METRICS", line))?;
            // The metrics as a list or in brackets, as the house template
            // writes them (`by [return, sharpe]`, E-118).
            let by = by.trim();
            let by = by.strip_prefix('[').and_then(|b| b.strip_suffix(']')).unwrap_or(by);
            Step::Compare {
                name,
                runs: list(runs).iter().map(|r| Ref::parse(r)).collect(),
                by: list(by),
            }
        }
        "ledger" => {
            let (name, body) = colon("ledger")?;
            let (rel, rest2) = body.split_once(" of ").ok_or_else(|| format!("`{}`: ledger NAME: R of RUN into TABLE", line))?;
            let (of, into) = rest2.split_once(" into ").ok_or_else(|| format!("`{}`: ledger NAME: R of RUN into TABLE", line))?;
            Step::Ledger {
                name,
                relation: rel.trim().to_string(),
                of: Ref::parse(of),
                into: into.trim().to_string(),
            }
        }
        "feed" => {
            let (name, body) = colon("feed")?;
            let (body, over) = match body.split_once(" over ") {
                Some((head, w)) => (head.trim().to_string(), Some(window(w)?)),
                None => (body, None),
            };
            let (stat, rel) = body.split_once(" on ").ok_or_else(|| format!("`{}`: feed NAME: STATISTIC on RELATION", line))?;
            Step::Feed {
                name,
                statistic: stat.trim().to_string(),
                relation: rel.trim().to_string(),
                over,
            }
        }
        "activation" => {
            let (n, of) = rest.split_once(" of ").ok_or_else(|| format!("`{}`: activation NAME of RUN", line))?;
            Step::Activation {
                name: n.trim().to_string(),
                of: Ref::parse(of),
            }
        }
        "report" => Step::Report { name: rest.to_string() },
        other => return Err(format!("`{}` is not a step: run, grid, walk_forward, reveal, test, compare, ledger, feed, activation or report", other)),
    };
    if !is_name(s.name()) {
        return Err(format!("`{}`: a step is named by a word", line));
    }
    Ok(s)
}

/// The block that follows `keyword` in `body` (its text inside the braces),
/// and the body without it.
fn take_block(body: &str, at: usize) -> Result<(String, usize), String> {
    let open = body[at..].find('{').map(|i| at + i).ok_or_else(|| "a block follows in braces".to_string())?;
    let end = matching_brace(body, open)?;
    Ok((body[open + 1..end - 1].to_string(), end))
}

/// A study's body.
fn study(name: &str, body: &str) -> Result<Plan, String> {
    let mut plan = Plan {
        name: name.to_string(),
        program: ProgramRef::Name(String::new()),
        params: vec![],
        grid: vec![],
        holdout: Holdout::None,
        walk_forward: None,
        window: None,
        budget: None,
        objective: "sharpe".into(),
        claims: vec![],
        steps: vec![],
        report: ReportSpec::default(),
    };
    let mut program = None;
    let mut steps_given = false;
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        i += rest.len() - trimmed.len();
        let kw_end = trimmed.find(|c: char| c.is_whitespace() || c == '{').unwrap_or(trimmed.len());
        let kw = &trimmed[..kw_end];
        let line_end = trimmed.find('\n').unwrap_or(trimmed.len());
        let value = trimmed[kw_end..line_end].trim().to_string();
        match kw {
            "program" | "holdout" | "walk_forward" | "window" | "objective" => {
                match kw {
                    "program" => {
                        program = Some(if value.contains('/') || value.ends_with(".dsl") {
                            ProgramRef::Path(value)
                        } else {
                            ProgramRef::Name(value)
                        });
                    }
                    "holdout" => plan.holdout = holdout(&value)?,
                    "walk_forward" => plan.walk_forward = Some(walk_forward(&value)?),
                    "window" => plan.window = Some(window(&value)?),
                    _ => {
                        if !is_name(&value) {
                            return Err(format!("study {}: `objective {}` names no metric", name, value));
                        }
                        plan.objective = value;
                    }
                }
                i += line_end;
            }
            "params" | "grid" | "budget" | "claims" | "steps" | "report" => {
                let (block, end) = take_block(body, i)?;
                match kw {
                    "params" => plan.params = pairs(&block, "params")?,
                    "grid" => {
                        for (k, v) in pairs_by(&block, &['\n', ';'], "grid")? {
                            let values: Vec<String> = v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                            if values.is_empty() {
                                return Err(format!("study {}: the grid axis `{}` has no value", name, k));
                            }
                            plan.grid.push((k, values));
                        }
                    }
                    "budget" => {
                        let mut b = Budget::default();
                        for e in entries(&block) {
                            let (k, v) = e.split_once(' ').ok_or_else(|| format!("budget: `{}` is not money|bytes|requests X", e))?;
                            let v = Some(v.trim().to_string());
                            match k {
                                "money" => b.money = v,
                                "bytes" => b.bytes = v,
                                "requests" => b.requests = v,
                                other => return Err(format!("budget: `{}` is not money, bytes or requests", other)),
                            }
                        }
                        plan.budget = Some(b);
                    }
                    "claims" => plan.claims = entries(&block).iter().map(|l| claim(l)).collect::<Result<_, _>>()?,
                    "steps" => {
                        steps_given = true;
                        plan.steps = entries(&block).iter().map(|l| step(l)).collect::<Result<_, _>>()?;
                    }
                    _ => {
                        for e in entries(&block) {
                            let list = |s: &str| {
                                s.trim()
                                    .trim_start_matches('[')
                                    .trim_end_matches(']')
                                    .split(',')
                                    .map(|x| x.trim().to_string())
                                    .filter(|x| !x.is_empty())
                                    .collect()
                            };
                            match e.split_once(' ') {
                                Some(("metrics", v)) => plan.report.metrics = list(v),
                                Some(("tables", v)) => plan.report.tables = list(v),
                                None if e == "text" => plan.report.text = true,
                                _ => return Err(format!("report: `{}` is not metrics [..], tables [..] or text", e)),
                            }
                        }
                    }
                }
                i = end;
            }
            other => return Err(format!("study {}: `{}` is not a field of a study", name, other)),
        }
    }
    plan.program = program.ok_or_else(|| format!("study {} names no program", name))?;
    if !steps_given {
        plan.steps = plan.default_steps();
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &plan.steps {
        if !seen.insert(s.name().to_string()) {
            return Err(format!("study {}: two steps are named `{}`", name, s.name()));
        }
    }
    validate(&plan)?;
    Ok(plan)
}

/// What a plan's steps and claims read is what they may read: an existing
/// step, of the kind the reading takes.
fn validate(plan: &Plan) -> Result<(), String> {
    let name = &plan.name;
    let kind_of = |s: &str| plan.step(s).map(Step::kind);
    // `one`: the reader takes one run, so a grid is read by its best point.
    let check_ref = |who: &str, r: &Ref, one: bool| -> Result<(), String> {
        match kind_of(&r.step) {
            None => Err(format!("study {}: {} reads `{}`, which no step is", name, who, r.step)),
            Some("grid") if r.best => Ok(()),
            Some(k) if r.best => Err(format!("study {}: {} reads best({}), but `{}` is a {} step, not a grid", name, who, r.step, r.step, k)),
            Some("grid") if one => Err(format!("study {}: {} reads one run of the grid `{}`: name it best({})", name, who, r.step, r.step)),
            Some("run" | "grid" | "reveal") => Ok(()),
            Some(k) => Err(format!("study {}: {} reads the run of `{}`, a {} step, which makes none", name, who, r.step, k)),
        }
    };
    let check_input = |who: &str, i: &Input| -> Result<(), String> {
        match i {
            Input::Feed { .. } => Ok(()),
            Input::Feature { of, .. } | Input::Returns(of) | Input::Events(of) => check_ref(who, of, false),
            Input::Ledger(s) => match kind_of(s) {
                Some("ledger") => Ok(()),
                _ => Err(format!("study {}: {} reads `ledger {}`, which is no ledger step", name, who, s)),
            },
            Input::Grid(s) => match kind_of(s) {
                Some("grid" | "walk_forward") => Ok(()),
                _ => Err(format!("study {}: {} reads `grid {}`, which is no grid or walk-forward step", name, who, s)),
            },
        }
    };
    for s in &plan.steps {
        let who = format!("step `{}`", s.name());
        match s {
            Step::Run { at: Some(At::Best(g)), .. } => check_ref(&who, &Ref { step: g.clone(), best: true }, true)?,
            Step::Reveal { of, .. } | Step::Ledger { of, .. } | Step::Activation { of, .. } => check_ref(&who, of, true)?,
            Step::Compare { runs, .. } => runs.iter().try_for_each(|r| check_ref(&who, r, true))?,
            Step::Test { input, .. } => check_input(&who, input)?,
            Step::WalkForward { .. } if plan.walk_forward.is_none() => return Err(format!("study {}: step `{}` walks forward, but the study declares no walk_forward scheme", name, s.name())),
            _ => {}
        }
        if matches!(s, Step::Reveal { .. }) && plan.holdout == Holdout::None {
            return Err(format!("study {}: step `{}` reveals, but the study declares no hold-out: there is nothing to reveal", name, s.name()));
        }
    }
    let mut claims = std::collections::BTreeSet::new();
    for c in &plan.claims {
        if !claims.insert(c.name.as_str()) {
            return Err(format!("study {}: two claims are named `{}`", name, c.name));
        }
        if plan.step(&c.name).is_some() {
            return Err(format!("study {}: `{}` names both a claim and a step", name, c.name));
        }
        check_input(&format!("claim `{}`", c.name), &c.input)?;
        // A `before` claim is judged before any grid or walk-forward runs:
        // it reads a feed, or a `run` step, which runs first (Q-5.1).
        if c.phase == Phase::Before {
            if let Some(s) = c.input.steps().into_iter().find(|s| kind_of(s) != Some("run")) {
                return Err(format!(
                    "study {}: claim `{}` is judged before the runs, so it reads a feed or a `run` step, not the {} step `{}`",
                    name,
                    c.name,
                    kind_of(&s).unwrap_or("unknown"),
                    s
                ));
            }
        }
    }
    Ok(())
}
