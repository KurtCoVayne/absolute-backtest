//! The deployment document (K-6.5): what a live run takes beyond its
//! program, its account, broker and session, routing, kill switch,
//! reconciliation, deadlines and recording. It is outside the run's id,
//! hashed separately (BLAKE3 over the RFC 8785 form of what it parses to),
//! and the hash is logged with the run (`binding.deployment_hash`).
//!
//! ```text
//! deployment [NAME] {
//!   account U1234567   broker ibkr
//!   session { host 127.0.0.1, port 4001, sender ABT, target IBKR, version FIX.4.4 }
//!   routing destination SMART   tif_map { market -> DAY, moc -> MOC, limit -> GTC }
//!   kill_switch { halt_all on SIGTERM | file /run/abt/halt | socket; flatten_all through decisions }
//!   reconcile halt at session_open, reconnect   [cash_tolerance 0.01]
//!   deadline { base 55s; session 5min }
//!   recording stream live-equities
//! }
//! ```
//!
//! Fields come in any order, separated by white space, commas or
//! semicolons; `#` starts a comment; a word with spaces is quoted. `routing`
//! takes its `destination` and `tif_map` inline or in braces. `account` and
//! `broker` are required; reconciliation defaults to `halt` at both
//! events, the kill switch to `halt_all socket` (ERRATA E-82).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::reconcile::Policy;
use crate::kernel::time;
use crate::kernel::value::{Order, OrderKind};

/// What halts a live run after its current barrier (`halt_all`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HaltSource {
    /// A signal to the process, by name (`SIGTERM`).
    Signal(String),
    /// A file whose existence halts the run.
    File(String),
    /// A `control halt` request to the kernel service.
    Socket,
}

/// A parsed deployment document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Deployment {
    pub name: Option<String>,
    pub account: String,
    /// The adapter: `mock`, or a broker this build has no adapter for.
    pub broker: String,
    /// The broker session's parameters (host, port, sender, target,
    /// version), for the adapter.
    pub session: BTreeMap<String, String>,
    pub destination: Option<String>,
    /// The broker's time in force by order kind (`market`, `moo`, `moc`,
    /// `moo_moc`, `limit`, `stop`).
    pub tif_map: BTreeMap<String, String>,
    pub halt_all: Vec<HaltSource>,
    pub reconcile: Policy,
    /// When reconciliation runs: `session_open`, `reconnect`.
    pub reconcile_at: Vec<String>,
    pub cash_tolerance: f64,
    /// A decision bar's deadline (K-6.3), in nanoseconds; none for one bar
    /// of the decision calendar.
    pub deadline_base: Option<i64>,
    /// How long the broker's session may stay down before the run halts,
    /// in nanoseconds of the stream's time.
    pub deadline_session: Option<i64>,
    /// The data-layer stream the live session records into: the session a
    /// live run reads.
    pub recording: Option<String>,
}

impl Deployment {
    /// BLAKE3 over the document's canonical form (RFC 8785): its fields as
    /// parsed, so neither layout nor comments change it.
    pub fn hash(&self) -> String {
        let v = serde_json::to_value(self).expect("a deployment is JSON");
        abt_wire::Hash::of(abt_wire::to_canonical_json(&v).expect("a deployment is canonical JSON").as_bytes()).hex()
    }

    /// Whether reconciliation runs at `event` (`session_open`, `reconnect`).
    pub fn reconciles_at(&self, event: &str) -> bool {
        self.reconcile_at.iter().any(|e| e == event)
    }

    /// The broker's time in force for an order, from `tif_map`.
    pub fn tif_for(&self, order: &Order) -> Option<String> {
        let kind = match order.kind {
            OrderKind::Market => "market",
            OrderKind::Moo => "moo",
            OrderKind::Moc => "moc",
            OrderKind::MooMoc => "moo_moc",
            OrderKind::Limit(_) => "limit",
            OrderKind::Stop(_) => "stop",
        };
        self.tif_map.get(kind).cloned()
    }

    /// The signals the kill switch halts on.
    pub fn signals(&self) -> impl Iterator<Item = &str> {
        self.halt_all.iter().filter_map(|s| match s {
            HaltSource::Signal(n) => Some(n.as_str()),
            _ => None,
        })
    }

    /// The file the kill switch halts on, if any.
    pub fn halt_file(&self) -> Option<&str> {
        self.halt_all.iter().find_map(|s| match s {
            HaltSource::File(p) => Some(p.as_str()),
            _ => None,
        })
    }

    /// A document read from a file.
    pub fn read(path: &std::path::Path) -> Result<Deployment, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        parse(&text).map_err(|e| format!("{}: {}", path.display(), e))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Open,
    Close,
    /// `,` or `;`: a separator, never required.
    Sep,
    Arrow,
    Pipe,
}

fn lex(text: &str) -> Result<Vec<(Tok, usize)>, String> {
    let mut out = Vec::new();
    let mut line = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            c if c.is_whitespace() => {}
            '#' => {
                while chars.peek().is_some_and(|c| *c != '\n') {
                    chars.next();
                }
            }
            '{' => out.push((Tok::Open, line)),
            '}' => out.push((Tok::Close, line)),
            ',' | ';' => out.push((Tok::Sep, line)),
            '|' => out.push((Tok::Pipe, line)),
            '"' => {
                let mut w = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\n') | None => return Err(format!("line {}: a quoted word does not end", line)),
                        Some(c) => w.push(c),
                    }
                }
                out.push((Tok::Word(w), line));
            }
            '-' if chars.peek() == Some(&'>') => {
                chars.next();
                out.push((Tok::Arrow, line));
            }
            c => {
                let mut w = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || "{},;|#\"".contains(n) {
                        break;
                    }
                    // `->` ends the word before it (`market->DAY`).
                    if n == '-' {
                        let mut ahead = chars.clone();
                        ahead.next();
                        if ahead.peek() == Some(&'>') {
                            break;
                        }
                    }
                    w.push(n);
                    chars.next();
                }
                out.push((Tok::Word(w), line));
            }
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at).map(|t| &t.0)
    }

    fn line(&self) -> usize {
        self.toks.get(self.at).or(self.toks.last()).map(|t| t.1).unwrap_or(1)
    }

    fn fail<T>(&self, m: impl std::fmt::Display) -> Result<T, String> {
        Err(format!("line {}: {}", self.line(), m))
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.at).map(|t| t.0.clone());
        self.at += 1;
        t
    }

    fn skip_seps(&mut self) {
        while self.peek() == Some(&Tok::Sep) {
            self.at += 1;
        }
    }

    fn word(&mut self, what: &str) -> Result<String, String> {
        match self.next() {
            Some(Tok::Word(w)) => Ok(w),
            _ => {
                self.at -= 1;
                self.fail(format!("expected {}", what))
            }
        }
    }

    fn expect(&mut self, t: Tok, what: &str) -> Result<(), String> {
        if self.peek() == Some(&t) {
            self.at += 1;
            Ok(())
        } else {
            self.fail(format!("expected {}", what))
        }
    }

    fn keyword(&mut self, w: &str) -> bool {
        if self.peek() == Some(&Tok::Word(w.to_string())) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// `{ KEY VALUE ... }`: the pairs, keys as `keys` admits them.
    fn pairs(&mut self, what: &str, keys: &[&str]) -> Result<BTreeMap<String, String>, String> {
        self.expect(Tok::Open, &format!("`{{` opening {}", what))?;
        let mut out = BTreeMap::new();
        loop {
            self.skip_seps();
            if self.peek() == Some(&Tok::Close) {
                self.at += 1;
                return Ok(out);
            }
            let key = self.word(&format!("a field of {} or `}}`", what))?;
            if !keys.is_empty() && !keys.contains(&key.as_str()) {
                return self.fail(format!("`{}` is not a field of {} ({})", key, what, keys.join(", ")));
            }
            let value = self.word(&format!("the value of {}'s `{}`", what, key))?;
            if out.insert(key.clone(), value).is_some() {
                return self.fail(format!("{} gives `{}` twice", what, key));
            }
        }
    }

    /// `{ KIND -> TIF ... }`.
    fn tif_map(&mut self) -> Result<BTreeMap<String, String>, String> {
        const KINDS: [&str; 6] = ["market", "moo", "moc", "moo_moc", "limit", "stop"];
        self.expect(Tok::Open, "`{` opening tif_map")?;
        let mut out = BTreeMap::new();
        loop {
            self.skip_seps();
            if self.peek() == Some(&Tok::Close) {
                self.at += 1;
                return Ok(out);
            }
            let kind = self.word("an order kind or `}`")?;
            if !KINDS.contains(&kind.as_str()) {
                return self.fail(format!("tif_map: `{}` is not an order kind ({})", kind, KINDS.join(", ")));
            }
            self.expect(Tok::Arrow, "`->`")?;
            let tif = self.word("the broker's time in force")?;
            out.insert(kind, tif);
        }
    }
}

/// A duration: `500ms`, `55s`, `5min` (or `5m`), `1h`.
fn duration(w: &str) -> Option<i64> {
    let split = w.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (n, unit) = w.split_at(split);
    let n: f64 = n.parse().ok()?;
    let ns = match unit {
        "ms" => time::SEC / 1000,
        "s" | "sec" => time::SEC,
        "m" | "min" => time::MIN,
        "h" => time::HOUR,
        _ => return None,
    };
    (n >= 0.0).then(|| (n * ns as f64).round() as i64)
}

/// The signals the kill switch can halt on.
pub const SIGNALS: [&str; 6] = ["SIGHUP", "SIGINT", "SIGQUIT", "SIGTERM", "SIGUSR1", "SIGUSR2"];

/// Parses a deployment document.
pub fn parse(text: &str) -> Result<Deployment, String> {
    let mut p = Parser { toks: lex(text)?, at: 0 };
    if !p.keyword("deployment") {
        return p.fail("a deployment document starts with `deployment`");
    }
    let name = match p.peek() {
        Some(Tok::Word(_)) => Some(p.word("a name")?),
        _ => None,
    };
    p.expect(Tok::Open, "`{`")?;
    let mut account = None;
    let mut broker = None;
    let mut session = None;
    let mut destination = None;
    let mut tif_map = None;
    let mut halt_all: Option<Vec<HaltSource>> = None;
    let mut reconcile: Option<(Policy, Vec<String>, f64)> = None;
    let mut deadline_base = None;
    let mut deadline_session = None;
    let mut recording = None;
    fn once<T>(p: &Parser, slot: &mut Option<T>, field: &str, v: T) -> Result<(), String> {
        if slot.is_some() {
            return p.fail(format!("`{}` is given twice", field));
        }
        *slot = Some(v);
        Ok(())
    }
    loop {
        p.skip_seps();
        let field = match p.next() {
            Some(Tok::Close) => break,
            Some(Tok::Word(w)) => w,
            None => return p.fail("the document does not close its `{`"),
            Some(_) => {
                p.at -= 1;
                return p.fail("expected a field of the deployment");
            }
        };
        match field.as_str() {
            "account" => {
                let v = p.word("the account")?;
                once(&p, &mut account, "account", v)?
            }
            "broker" => {
                let v = p.word("the broker")?;
                once(&p, &mut broker, "broker", v)?
            }
            "session" => {
                let v = p.pairs("session", &["host", "port", "sender", "target", "version"])?;
                once(&p, &mut session, "session", v)?
            }
            "routing" | "destination" | "tif_map" => {
                // `routing` takes its two fields inline or in braces.
                let braced = field == "routing" && p.peek() == Some(&Tok::Open);
                if braced {
                    p.at += 1;
                }
                let mut first = field != "routing";
                let mut current = field.clone();
                let mut given = 0;
                loop {
                    if !first {
                        p.skip_seps();
                        match p.peek() {
                            Some(Tok::Word(w)) if w == "destination" || w == "tif_map" => current = p.word("a routing field")?,
                            Some(Tok::Close) if braced => {
                                p.at += 1;
                                break;
                            }
                            _ if braced => return p.fail("routing takes `destination` and `tif_map`"),
                            _ => break,
                        }
                    }
                    first = false;
                    given += 1;
                    match current.as_str() {
                        "destination" => {
                            let v = p.word("the destination")?;
                            once(&p, &mut destination, "destination", v)?
                        }
                        "tif_map" => {
                            let v = p.tif_map()?;
                            once(&p, &mut tif_map, "tif_map", v)?
                        }
                        _ => {}
                    }
                }
                if given == 0 {
                    return p.fail("routing takes `destination NAME` and `tif_map { KIND -> TIF, ... }`");
                }
            }
            "kill_switch" => {
                p.expect(Tok::Open, "`{` opening kill_switch")?;
                let mut sources = Vec::new();
                loop {
                    p.skip_seps();
                    if p.peek() == Some(&Tok::Close) {
                        p.at += 1;
                        break;
                    }
                    match p.word("`halt_all`, `flatten_all` or `}`")?.as_str() {
                        "halt_all" => loop {
                            match p.word("`on SIGNAL`, `file PATH` or `socket`")?.as_str() {
                                "on" => {
                                    let s = p.word("a signal")?;
                                    let named = if s.starts_with("SIG") { s.clone() } else { format!("SIG{}", s) };
                                    if !SIGNALS.contains(&named.as_str()) {
                                        return p.fail(format!("halt_all on {}: not a signal the kill switch takes ({})", s, SIGNALS.join(", ")));
                                    }
                                    sources.push(HaltSource::Signal(named));
                                }
                                "file" => sources.push(HaltSource::File(p.word("the file's path")?)),
                                "socket" => sources.push(HaltSource::Socket),
                                other => return p.fail(format!("halt_all: `{}` is not `on SIGNAL`, `file PATH` or `socket`", other)),
                            }
                            if p.peek() == Some(&Tok::Pipe) {
                                p.at += 1;
                            } else {
                                break;
                            }
                        },
                        "flatten_all" => {
                            if !(p.keyword("through") && p.keyword("decisions")) {
                                return p.fail("flatten_all goes `through decisions` (K-6.5): its closing decisions are logged and limited like any");
                            }
                        }
                        other => return p.fail(format!("kill_switch: `{}` is not `halt_all` or `flatten_all`", other)),
                    }
                }
                once(&p, &mut halt_all, "kill_switch", sources)?
            }
            "reconcile" => {
                let w = p.word("halt, adopt or warn")?;
                let Some(policy) = Policy::parse(&w) else {
                    return p.fail(format!("reconcile: `{}` is not halt, adopt or warn", w));
                };
                let mut at = Vec::new();
                let mut tolerance = 0.01;
                loop {
                    if p.keyword("at") {
                        loop {
                            let e = p.word("session_open or reconnect")?;
                            if e != "session_open" && e != "reconnect" {
                                return p.fail(format!("reconcile at `{}`: it runs at session_open and reconnect", e));
                            }
                            if !at.contains(&e) {
                                at.push(e);
                            }
                            // `at session_open, reconnect`: a comma joins the list.
                            if p.peek() == Some(&Tok::Sep) && matches!(p.toks.get(p.at + 1).map(|t| &t.0), Some(Tok::Word(w)) if w == "session_open" || w == "reconnect") {
                                p.at += 1;
                            } else {
                                break;
                            }
                        }
                    } else if p.keyword("cash_tolerance") {
                        let w = p.word("a sum of cash")?;
                        tolerance = match w.parse::<f64>() {
                            Ok(x) if x >= 0.0 && x.is_finite() => x,
                            _ => return p.fail(format!("cash_tolerance `{}` is not a sum of cash", w)),
                        };
                    } else {
                        break;
                    }
                }
                if at.is_empty() {
                    at = vec!["session_open".into(), "reconnect".into()];
                }
                once(&p, &mut reconcile, "reconcile", (policy, at, tolerance))?
            }
            "deadline" => {
                let pairs = p.pairs("deadline", &["base", "session"])?;
                for (k, v) in pairs {
                    let Some(ns) = duration(&v) else {
                        return p.fail(format!("deadline {}: `{}` is not a duration (500ms, 55s, 5min, 1h)", k, v));
                    };
                    if k == "base" {
                        deadline_base = Some(ns);
                    } else {
                        deadline_session = Some(ns);
                    }
                }
            }
            "recording" => {
                if !p.keyword("stream") {
                    return p.fail("recording names its `stream NAME`");
                }
                let v = p.word("the stream's name")?;
                once(&p, &mut recording, "recording", v)?
            }
            other => {
                p.at -= 1;
                return p.fail(format!(
                    "`{}` is not a field of a deployment (account, broker, session, routing, kill_switch, reconcile, deadline, recording)",
                    other
                ));
            }
        }
    }
    if p.peek().is_some() {
        return p.fail("text after the deployment's closing `}`");
    }
    let (policy, at, cash_tolerance) = reconcile.unwrap_or((Policy::Halt, vec!["session_open".into(), "reconnect".into()], 0.01));
    Ok(Deployment {
        name,
        account: account.ok_or("a deployment names its `account`")?,
        broker: broker.ok_or("a deployment names its `broker`")?,
        session: session.unwrap_or_default(),
        destination,
        tif_map: tif_map.unwrap_or_default(),
        halt_all: halt_all.unwrap_or_else(|| vec![HaltSource::Socket]),
        reconcile: policy,
        reconcile_at: at,
        cash_tolerance,
        deadline_base,
        deadline_session,
        recording,
    })
}
