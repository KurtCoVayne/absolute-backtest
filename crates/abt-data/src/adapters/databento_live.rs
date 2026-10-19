//! Databento's live gateway (D-7.7, `databento_live`): a session over TCP
//! to `<dataset>.lsg.databento.com:13000` (`GLBX.MDP3` becomes
//! `glbx-mdp3`), in the gateway's line protocol, then DBN.
//!
//! 1. The gateway greets (`lsg_version=…`) and challenges (`cram=C`).
//! 2. The client answers `auth=H-B|dataset=D|encoding=dbn|compression=none|
//!    ts_out=0|client=abt`, with `H` the hex SHA-256 of `C|key` and `B` the
//!    key's last five characters (its bucket); the gateway accepts with
//!    `success=1|session_id=…` or refuses with `success=0|error=…`.
//! 3. The client subscribes (`schema=…|stype_in=…|symbols=…|snapshot=0|
//!    is_last=1`, with `start=` nanoseconds to replay from) and starts the
//!    session (`start_session`).
//! 4. The gateway sends a DBN stream: its metadata, then records, each
//!    followed by its send time when `ts_out` was asked for.
//!
//! Every record is stamped at acceptance, the instant the session read it:
//! its rows' `r` (D-7.7). A continuous symbol (`ES.v.0`) is subscribed with
//! `stype_in = continuous`; the gateway maps it to the contract it holds,
//! and the rows are keyed by the continuous symbol, whose current contract
//! is unadjusted (`continuous(root, t, contract, 0)`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use arrow::array::{ArrayRef, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use super::dbn::{self, Decoded, Record};
use super::{Adapter, Feed, FetchRequest, Fetched, Keys};
use crate::error::DataError;
use crate::resolver::catalog::Offer;

/// A session's request: where, what and with whose key.
#[derive(Clone, Debug, PartialEq)]
pub struct Gateway {
    /// `host:port`; the dataset's gateway when empty.
    pub addr: String,
    pub dataset: String,
    /// The schema subscribed (`ohlcv-1m`).
    pub schema: String,
    /// `raw_symbol`, `parent` or `continuous`.
    pub stype_in: String,
    pub symbols: Vec<String>,
    /// Replay from this instant (nanoseconds), within the gateway's day.
    pub start: Option<i64>,
    /// Ask for every record's send time.
    pub ts_out: bool,
    /// The environment variable holding the key; `DATABENTO_API_KEY` when
    /// empty.
    pub key_env: String,
    /// How long a read waits before the session is taken as lost.
    pub timeout: Duration,
}

impl Default for Gateway {
    fn default() -> Self {
        Gateway {
            addr: String::new(),
            dataset: String::new(),
            schema: "ohlcv-1m".into(),
            stype_in: "raw_symbol".into(),
            symbols: vec![],
            start: None,
            ts_out: false,
            key_env: String::new(),
            timeout: Duration::from_secs(60),
        }
    }
}

/// The gateway of a dataset (`GLBX.MDP3` → `glbx-mdp3.lsg.databento.com:13000`).
pub fn gateway_of(dataset: &str) -> String {
    format!("{}.lsg.databento.com:13000", dataset.replace('.', "-").to_ascii_lowercase())
}

/// The answer to a challenge: the hex SHA-256 of `challenge|key`, then the
/// key's bucket.
pub fn cram(challenge: &str, key: &str) -> String {
    let digest = Sha256::digest(format!("{}|{}", challenge, key).as_bytes());
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
    format!("{}-{}", hex, &key[key.len().saturating_sub(5)..])
}

/// A session started: the records as they arrive.
pub struct Session {
    pub session_id: String,
    decoder: dbn::Decoder<BufReader<TcpStream>>,
    /// The symbol each instrument is subscribed as, from the gateway's
    /// symbol mappings.
    symbols: BTreeMap<u32, String>,
}

/// One record read, with the instant it was accepted.
#[derive(Clone, Debug, PartialEq)]
pub struct Accepted {
    pub decoded: Decoded,
    pub at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

fn line(r: &mut BufReader<TcpStream>) -> Result<String, DataError> {
    let mut s = String::new();
    let n = r.read_line(&mut s)?;
    if n == 0 {
        return Err(DataError::Unresolved("the gateway closed the connection".into()));
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

impl Gateway {
    /// Connects, authenticates with the key from the environment, subscribes
    /// and starts the session.
    pub fn connect(&self) -> Result<Session, DataError> {
        let env = if self.key_env.is_empty() { "DATABENTO_API_KEY" } else { self.key_env.as_str() };
        let key = std::env::var(env).map_err(|_| DataError::Unresolved(format!("the Databento key is read from ${}, which is not set", env)))?;
        self.connect_with(&key)
    }

    /// [`Gateway::connect`] with the key given.
    pub fn connect_with(&self, key: &str) -> Result<Session, DataError> {
        let addr = if self.addr.is_empty() { gateway_of(&self.dataset) } else { self.addr.clone() };
        let stream = TcpStream::connect(&addr).map_err(|e| DataError::Unresolved(format!("the gateway {}: {}", addr, e)))?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_nodelay(true)?;
        let mut w = stream.try_clone()?;
        let mut r = BufReader::new(stream);
        let _greeting = line(&mut r)?;
        let challenge = line(&mut r)?;
        let challenge = challenge
            .strip_prefix("cram=")
            .ok_or_else(|| DataError::Malformed(format!("the gateway {} sent no challenge: `{}`", addr, challenge)))?;
        writeln!(
            w,
            "auth={}|dataset={}|encoding=dbn|compression=none|ts_out={}|client=abt {}",
            cram(challenge, key),
            self.dataset,
            u8::from(self.ts_out),
            env!("CARGO_PKG_VERSION")
        )?;
        let reply = line(&mut r)?;
        let fields: BTreeMap<&str, &str> = reply.split('|').filter_map(|kv| kv.split_once('=')).collect();
        if fields.get("success") != Some(&"1") {
            return Err(DataError::Unresolved(format!(
                "the gateway {} refused the session: {}",
                addr,
                fields.get("error").copied().unwrap_or(reply.as_str())
            )));
        }
        let session_id = fields.get("session_id").copied().unwrap_or("").to_string();
        let mut sub = format!("schema={}|stype_in={}|symbols={}|snapshot=0|is_last=1", self.schema, self.stype_in, self.symbols.join(","));
        if let Some(s) = self.start {
            sub.push_str(&format!("|start={}", s));
        }
        sub.push('\n');
        w.write_all(sub.as_bytes())?;
        w.write_all(b"start_session\n")?;
        w.flush()?;
        let decoder = dbn::Decoder::new(r).map_err(|e| DataError::Malformed(format!("the gateway {}: {}", addr, e)))?;
        Ok(Session {
            session_id,
            decoder,
            symbols: BTreeMap::new(),
        })
    }
}

impl Session {
    pub fn metadata(&self) -> &dbn::Metadata {
        self.decoder.metadata()
    }

    /// The next record, stamped at acceptance; `None` when the gateway ends
    /// the session. A symbol mapping is kept (the subscribed symbol of its
    /// instrument) and returned like any record; an error record ends the
    /// session with its message.
    #[allow(clippy::should_implement_trait)] // an iterator could not tell the session's end from an error
    pub fn next(&mut self) -> Result<Option<Accepted>, DataError> {
        let Some(d) = self.decoder.next().map_err(|e| DataError::Malformed(format!("the live stream: {}", e)))? else {
            return Ok(None);
        };
        let at = now();
        match &d.record {
            Record::SymbolMapping { hd, stype_in_symbol, .. } => {
                self.symbols.insert(hd.instrument_id, stype_in_symbol.clone());
            }
            Record::Error { err, .. } => return Err(DataError::Unresolved(format!("the gateway: {}", err))),
            _ => {}
        }
        Ok(Some(Accepted { decoded: d, at }))
    }

    /// The symbol an instrument was subscribed as.
    pub fn symbol(&self, instrument: u32) -> Option<&str> {
        self.symbols.get(&instrument).map(String::as_str)
    }

    /// The rows of accepted bars of `class` (`equity`: keyed by the
    /// subscribed symbol and joined to the store's tickers; `future`: keyed
    /// by the subscribed continuous symbol, with `continuous` naming each
    /// bar's contract), each received at its acceptance plus `delay`.
    pub fn rows(&self, class: &str, accepted: &[Accepted], delay: i64) -> Result<Fetched, DataError> {
        let mut keys = Vec::new();
        let mut contracts = Vec::new();
        let mut t = Vec::new();
        let mut r = Vec::new();
        let mut px: [Vec<f64>; 5] = Default::default();
        for a in accepted {
            let Record::Ohlcv { hd, open, high, low, close, volume } = &a.decoded.record else { continue };
            let step = match hd.rtype {
                dbn::rtype::OHLCV_1S => 1_000_000_000,
                dbn::rtype::OHLCV_1M => 60_000_000_000,
                dbn::rtype::OHLCV_1H => 3_600_000_000_000,
                dbn::rtype::OHLCV_1D => 86_400_000_000_000,
                _ => 0,
            };
            let symbol = self
                .symbol(hd.instrument_id)
                .ok_or_else(|| DataError::Identity(format!("a bar of instrument {} came before its symbol mapping", hd.instrument_id)))?;
            let close_t = hd.ts_event as i64 + step;
            keys.push(symbol.to_string());
            contracts.push(hd.instrument_id.to_string());
            t.push(close_t);
            r.push(a.at.max(close_t) + delay);
            for (k, v) in [*open, *high, *low, *close].into_iter().enumerate() {
                px[k].push(dbn::price(v).unwrap_or(f64::NAN));
            }
            px[4].push(*volume as f64);
        }
        let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
        let batch = |name: &str, col: &str, v: &[f64]| -> Result<(String, RecordBatch), DataError> {
            Ok((
                format!("{}.{}", class, name),
                RecordBatch::try_new(
                    Arc::new(Schema::new(vec![
                        Field::new("A", DataType::Utf8, false),
                        Field::new("T", ts.clone(), false),
                        Field::new(col, DataType::Float64, true),
                        Field::new("r", ts.clone(), false),
                    ])),
                    vec![
                        Arc::new(StringArray::from(keys.clone())) as ArrayRef,
                        Arc::new(TimestampNanosecondArray::from(t.clone())),
                        Arc::new(Float64Array::from(v.to_vec())),
                        Arc::new(TimestampNanosecondArray::from(r.clone())),
                    ],
                )?,
            ))
        };
        let mut relations = vec![
            batch("open", "O", &px[0])?,
            batch("high", "H", &px[1])?,
            batch("low", "L", &px[2])?,
            batch("close", "P", &px[3])?,
            batch("volume", "V", &px[4])?,
        ];
        let keys_rule = if class == "equity" { Keys::Ticker } else { Keys::Vendor };
        if class == "future" {
            relations.push((
                "continuous".into(),
                RecordBatch::try_new(
                    Arc::new(Schema::new(vec![
                        Field::new("root_id", DataType::Utf8, false),
                        Field::new("t", ts.clone(), false),
                        Field::new("contract_id", DataType::Utf8, true),
                        Field::new("adjustment", DataType::Float64, true),
                        Field::new("r", ts.clone(), false),
                    ])),
                    vec![
                        Arc::new(StringArray::from(keys.clone())) as ArrayRef,
                        Arc::new(TimestampNanosecondArray::from(t.clone())),
                        Arc::new(StringArray::from(contracts)),
                        Arc::new(Float64Array::from(vec![0.0; keys.len()])),
                        Arc::new(TimestampNanosecondArray::from(r.clone())),
                    ],
                )?,
            ));
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("receipt".to_string(), "recorded".to_string());
        attributes.insert("dataset".to_string(), self.metadata().dataset.clone());
        Ok(Fetched {
            relations,
            attributes,
            keys: keys_rule,
            ..Fetched::default()
        })
    }
}

/// The live gateway as a source: no history (the `databento` adapter's),
/// a subscription per request.
pub struct DatabentoLive {
    gateway: Gateway,
    /// `future` or `equity`.
    class: String,
    /// Added to every row's acceptance stamp, in nanoseconds.
    delay: i64,
}

impl DatabentoLive {
    pub fn new(gateway: Gateway, class: &str, delay: i64) -> DatabentoLive {
        DatabentoLive {
            gateway,
            class: class.to_string(),
            delay,
        }
    }

    /// The adapter a catalog's source configures: `dataset`, `schema`
    /// (`ohlcv-1m`), `stype_in`, `symbols` (`ES.v.0,NQ.v.0`), and optionally
    /// `addr` (`host:port`, the dataset's gateway by default), `key_env`,
    /// `class` (`future` or `equity`) and `processing_delay` (seconds).
    pub fn from_settings(settings: &BTreeMap<String, String>) -> Result<DatabentoLive, DataError> {
        let bad = |k: &str, v: &str, want: &str| DataError::Malformed(format!("the databento_live setting `{}` is {}, not `{}`", k, want, v));
        let mut g = Gateway::default();
        let mut class = "future".to_string();
        let mut delay = 0;
        for (k, v) in settings {
            match k.as_str() {
                "addr" => g.addr = v.clone(),
                "dataset" => g.dataset = v.clone(),
                "schema" => g.schema = v.clone(),
                "stype_in" => g.stype_in = v.clone(),
                "symbols" => g.symbols = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                "key_env" => g.key_env = v.clone(),
                "class" if v == "future" || v == "equity" => class = v.clone(),
                "class" => return Err(bad(k, v, "future or equity")),
                "processing_delay" => delay = (v.parse::<f64>().map_err(|_| bad(k, v, "a number of seconds"))? * 1e9).round() as i64,
                _ => return Err(DataError::Malformed(format!("the databento_live adapter has no setting `{}`", k))),
            }
        }
        if g.dataset.is_empty() || g.symbols.is_empty() {
            return Err(DataError::Malformed("a databento_live source names its `dataset` and `symbols`".into()));
        }
        Ok(DatabentoLive::new(g, &class, delay))
    }
}

impl Adapter for DatabentoLive {
    fn vendor(&self) -> &str {
        "databento"
    }

    fn offers(&self) -> Vec<Offer> {
        vec![Offer {
            class: self.class.clone(),
            relations: ["open", "high", "low", "close", "volume"].iter().map(|r| r.to_string()).collect(),
            calendar: self.gateway.schema.trim_start_matches("ohlcv-").to_string(),
            from: None,
            to: None,
            scope: "by_request".into(),
            has_receipt: true,
            supplies: vec![],
            attributes: BTreeMap::new(),
            instruments: Some(self.gateway.symbols.len() as u64),
        }]
    }

    fn fetch(&self, _req: &FetchRequest) -> Result<Fetched, DataError> {
        Err(DataError::Unresolved("the live gateway sells no history: the `databento` adapter's historical source does".into()))
    }

    fn subscribe(&self, _req: &FetchRequest) -> Result<Box<dyn Feed>, DataError> {
        let session = self.gateway.connect()?;
        Ok(Box::new(LiveFeed {
            session,
            class: self.class.clone(),
            delay: self.delay,
        }))
    }
}

/// A session as a feed: one bar's rows at a time.
pub struct LiveFeed {
    session: Session,
    class: String,
    delay: i64,
}

impl LiveFeed {
    pub fn new(session: Session, class: &str, delay: i64) -> LiveFeed {
        LiveFeed {
            session,
            class: class.to_string(),
            delay,
        }
    }
}

impl Feed for LiveFeed {
    fn next(&mut self) -> Result<Option<Fetched>, DataError> {
        while let Some(a) = self.session.next()? {
            if matches!(a.decoded.record, Record::Ohlcv { .. }) {
                return self.session.rows(&self.class, &[a], self.delay).map(Some);
            }
        }
        Ok(None)
    }
}
