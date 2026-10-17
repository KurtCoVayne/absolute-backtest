//! Databento's historical API (`timeseries.get_range`), the one HTTP call
//! the adapters make: a form POST with the key as the basic-auth user,
//! answered with DBN. A plain `http://` URL (a test's server) is spoken to
//! directly over a socket; an `https://` one through `curl`, given the key
//! and the form on its standard input so that neither appears in a process
//! listing (abt carries no TLS implementation, ERRATA E-68).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::DataError;

/// A historical request's fixed part: where, what and with whose key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Historical {
    /// The API's base URL (`https://hist.databento.com`).
    pub base: String,
    pub dataset: String,
    /// The bars' schema (`ohlcv-1m`).
    pub schema: String,
    pub symbols: Vec<String>,
    /// `raw_symbol`, `parent` or `continuous`; `raw_symbol` when empty.
    pub stype_in: String,
    /// The environment variable holding the key; `DATABENTO_API_KEY` when
    /// empty.
    pub key_env: String,
}

impl Historical {
    /// The DBN of `schema` over `[from, to)` (nanoseconds), decompressed.
    pub fn get_range(&self, schema: &str, from: i64, to: i64) -> Result<Vec<u8>, DataError> {
        let env = if self.key_env.is_empty() { "DATABENTO_API_KEY" } else { self.key_env.as_str() };
        let key = std::env::var(env).map_err(|_| DataError::Unresolved(format!("the Databento key is read from ${}, which is not set", env)))?;
        let form = [
            ("dataset", self.dataset.clone()),
            ("schema", schema.to_string()),
            ("symbols", self.symbols.join(",")),
            ("stype_in", if self.stype_in.is_empty() { "raw_symbol".to_string() } else { self.stype_in.clone() }),
            ("stype_out", "instrument_id".to_string()),
            ("start", from.to_string()),
            ("end", to.to_string()),
            ("encoding", "dbn".to_string()),
            ("compression", "zstd".to_string()),
        ];
        let body = form.iter().map(|(k, v)| format!("{}={}", k, urlencode(v))).collect::<Vec<_>>().join("&");
        let url = format!("{}/v0/timeseries.get_range", self.base.trim_end_matches('/'));
        let bytes = post(&url, &key, &body)?;
        super::databento::unpack(bytes)
    }
}

/// A form value, percent-encoded.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Base64 (RFC 4648, with padding).
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
}

/// POSTs a form with `user` as the basic-auth user and an empty password;
/// the response's body, or an error naming the status and the body's start.
pub fn post(url: &str, user: &str, body: &str) -> Result<Vec<u8>, DataError> {
    if let Some(rest) = url.strip_prefix("http://") {
        return post_plain(rest, user, body);
    }
    if !url.starts_with("https://") {
        return Err(DataError::Malformed(format!("`{}` is not an http:// or https:// URL", url)));
    }
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let config = format!(
        "url = \"{}\"\nuser = \"{}:\"\ndata = \"{}\"\nheader = \"Content-Type: application/x-www-form-urlencoded\"\nsilent\nshow-error\nfail-with-body\n",
        quote(url),
        quote(user),
        quote(body)
    );
    let mut child = Command::new("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DataError::Unresolved(format!("an https request needs curl on the PATH: {}", e)))?;
    child.stdin.take().expect("piped").write_all(config.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(DataError::Unresolved(format!(
            "{}: {} {}",
            url,
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(300)]).trim()
        )));
    }
    Ok(out.stdout)
}

/// A POST over a plain socket (`host:port/path`).
fn post_plain(rest: &str, user: &str, body: &str) -> Result<Vec<u8>, DataError> {
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if host.contains(':') { host.to_string() } else { format!("{}:80", host) };
    let mut s = TcpStream::connect(&addr).map_err(|e| DataError::Unresolved(format!("http://{}: {}", host, e)))?;
    s.set_read_timeout(Some(Duration::from_secs(120)))?;
    let auth = base64(format!("{}:", user).as_bytes());
    write!(
        s,
        "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Basic {}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path,
        host,
        auth,
        body.len(),
        body
    )?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| DataError::Malformed(format!("http://{}{}: a response without its headers", host, path)))?;
    let head = String::from_utf8_lossy(&raw[..end]).to_string();
    let mut body = raw[end + 4..].to_vec();
    let status: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    if head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"))
    {
        body = dechunk(&body)?;
    }
    if !(200..300).contains(&status) {
        return Err(DataError::Unresolved(format!(
            "http://{}{}: status {}: {}",
            host,
            path,
            status,
            String::from_utf8_lossy(&body[..body.len().min(300)]).trim()
        )));
    }
    Ok(body)
}

/// A chunked body joined.
fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, DataError> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n").ok_or_else(|| DataError::Malformed("a chunked body cut short".into()))?;
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..eol]).split(';').next().unwrap_or("").trim(), 16).map_err(|_| DataError::Malformed("a chunk without its size".into()))?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err(DataError::Malformed("a chunked body cut short".into()));
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}
