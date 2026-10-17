//! Live streams (D-5.2 live, D-5.4, D-5.5): the clock, the recording of a
//! live feed, and what its subscriptions share.
//!
//! A **session** records a feed (`adapters::Feed`) into the store under the
//! session's name as its source, write-ahead: a batch is in the store before
//! any subscriber is sent it. Every row is received at its acceptance: the
//! session stamps `r` with the clock, never below the last row's `r` and
//! always past the last barrier emitted, so the acceptance order is the
//! canonical order of Definition D-5.3.
//!
//! A **live subscription** emits the barrier of a label `ℓ` of calendar `C`
//! when the clock passes `ℓ + grace(C)` (D-5.1: 0 for sessions, 250 ms for
//! bars), at the instant it emits it, after every row accepted before. The
//! barriers it emits are recorded as `kernel.<run>.clock` (the clock's
//! schema), so the session replays: the merge of the session's versions with
//! the recorded clock gives the same events with the same `seq`
//! (Proposition D-5.4), at full speed or, `paced`, at the clock's pace
//! (D-5.5).
//!
//! The clock is the wall clock, or one a test or a rehearsal runs faster
//! (`ScaledClock`).

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use arrow::array::{ArrayRef, Float64Array, RecordBatch, StringArray, TimestampNanosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

use crate::adapter::Security;
use crate::adapters::{Feed, Fetched, Keys};
use crate::error::DataError;

/// An instant source.
pub trait Clock: Send + Sync {
    /// Nanoseconds since 1970-01-01.
    fn now(&self) -> i64;
    /// Returns once `now() >= t`.
    fn sleep_until(&self, t: i64);
}

/// The wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
    }

    fn sleep_until(&self, t: i64) {
        loop {
            let d = t - self.now();
            if d <= 0 {
                return;
            }
            std::thread::sleep(Duration::from_nanos(d as u64));
        }
    }
}

/// A clock that starts at `origin` and runs `rate` times the wall clock.
#[derive(Clone, Copy, Debug)]
pub struct ScaledClock {
    start: Instant,
    origin: i64,
    rate: f64,
}

impl ScaledClock {
    pub fn new(origin: i64, rate: f64) -> ScaledClock {
        ScaledClock {
            start: Instant::now(),
            origin,
            rate: rate.max(1e-9),
        }
    }
}

impl Clock for ScaledClock {
    fn now(&self) -> i64 {
        self.origin + (self.start.elapsed().as_nanos() as f64 * self.rate) as i64
    }

    fn sleep_until(&self, t: i64) {
        loop {
            let d = t - self.now();
            if d <= 0 {
                return;
            }
            std::thread::sleep(Duration::from_nanos(((d as f64 / self.rate) as u64).max(1)));
        }
    }
}

/// The grace of a calendar (Definition D-5.1): none for sessions, 250 ms
/// for bars.
pub fn default_grace(calendar: &str) -> i64 {
    if calendar.starts_with("sessions") {
        0
    } else {
        250_000_000
    }
}

/// What a session's recorder and its subscriptions share.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionState {
    /// The greatest `r` stamped.
    pub last_r: i64,
    /// The greatest instant a barrier was emitted at.
    pub last_at: i64,
    /// The feed ended (or failed, with its error).
    pub ended: bool,
    pub error: Option<String>,
    /// The batches and rows recorded.
    pub batches: u64,
    pub rows: u64,
    /// The relations the session records.
    pub relations: Vec<String>,
}

/// A session: its state under a lock, and the signal of its changes.
#[derive(Debug, Default)]
pub struct Session {
    pub state: Mutex<SessionState>,
    pub changed: Condvar,
}

impl Session {
    /// The state, waiting while `wait` holds and the session has not ended,
    /// at most `timeout`.
    pub fn wait_while(&self, timeout: Duration, wait: impl Fn(&SessionState) -> bool) -> SessionState {
        let g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (g, _) = self.changed.wait_timeout_while(g, timeout, |s| !s.ended && wait(s)).unwrap_or_else(|p| p.into_inner());
        g.clone()
    }
}

/// A recording on its thread: the session's state when its feed ends.
pub type Recording = std::thread::JoinHandle<Result<SessionState, DataError>>;

/// The live sessions of a service, by name.
#[derive(Debug, Default)]
pub struct Sessions {
    map: Mutex<BTreeMap<String, Arc<Session>>>,
}

impl Sessions {
    /// The session of a name, made when it is new.
    pub fn open(&self, name: &str) -> Arc<Session> {
        let mut m = self.map.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(name.to_string()).or_default().clone()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Session>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner()).get(name).cloned()
    }
}

/// A clock relation's schema (the registry's `calendar_label`): a
/// calendar's labels and the instants their barriers are emitted at.
pub fn clock_schema(name: &str, class: &str) -> RelationSchema {
    let col = |name: &str, ty: ColumnType, role: Role| ColumnSpec {
        name: name.to_string(),
        ty,
        role,
        dimension: None,
    };
    RelationSchema {
        name: name.to_string(),
        class: class.to_string(),
        columns: vec![
            col("calendar", ColumnType::Label, Role::Entity),
            col("label", ColumnType::Time, Role::EventTime),
            col("at", ColumnType::Time, Role::Value),
            col("r", ColumnType::Time, Role::ReceiptTime),
        ],
        calendar: None,
        constraints: None,
        attributes: None,
        schema_version: Some("1.0".to_string()),
    }
}

/// Clock rows: `(calendar, label, at)`, each received at its `at`.
pub fn clock_rows(rows: &[(&str, i64, i64)]) -> Result<RecordBatch, DataError> {
    let ts = DataType::Timestamp(TimeUnit::Nanosecond, None);
    Ok(RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("calendar", DataType::Utf8, false),
            Field::new("label", ts.clone(), false),
            Field::new("at", ts.clone(), false),
            Field::new("r", ts, false),
        ])),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())) as ArrayRef,
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )?)
}

/// The relation a live stream records its barriers in, beside the clock
/// relation it planned them from (`kernel.<run>.calendar_label` →
/// `kernel.<run>.clock`).
pub fn recorded_clock(planned: &str) -> String {
    match planned.strip_suffix("calendar_label") {
        Some(prefix) => format!("{}clock", prefix),
        None => format!("{}.clock", planned),
    }
}

/// The relation a session's planned bars are in (`live.<session>.plan`).
pub fn plan_relation(session: &str) -> String {
    format!("live.{}.plan", session)
}

/// A fetch's rows all received at `r`: every relation's `r` column set (or
/// added).
pub fn stamp(f: &mut Fetched, r: i64) -> Result<(), DataError> {
    for (_, b) in f.relations.iter_mut() {
        let rs: ArrayRef = Arc::new(TimestampNanosecondArray::from(vec![r; b.num_rows()]));
        let mut fields: Vec<Field> = Vec::with_capacity(b.num_columns() + 1);
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(b.num_columns() + 1);
        for (j, field) in b.schema().fields().iter().enumerate() {
            if field.name() != "r" {
                fields.push(field.as_ref().clone());
                cols.push(b.column(j).clone());
            }
        }
        fields.push(Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false));
        cols.push(rs);
        *b = RecordBatch::try_new(Arc::new(Schema::new(fields)), cols)?;
    }
    Ok(())
}

/// A synthetic live feed (rehearsals, G4): the equity bars of `symbols` at
/// each label of a plan, a deterministic random walk per name, each bar
/// delivered once the clock passes its close plus `delay`.
pub struct SyntheticFeed {
    clock: Arc<dyn Clock>,
    symbols: Vec<String>,
    labels: Vec<i64>,
    delay: i64,
    next: usize,
    state: u64,
    prices: Vec<f64>,
}

impl SyntheticFeed {
    pub fn new(clock: Arc<dyn Clock>, symbols: &[&str], labels: Vec<i64>, delay: i64, seed: u64) -> SyntheticFeed {
        SyntheticFeed {
            clock,
            prices: (0..symbols.len()).map(|i| 100.0 + 10.0 * i as f64).collect(),
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
            labels,
            delay,
            next: 0,
            state: seed.max(1),
        }
    }

    /// What the session knows before the first bar: the names, and every
    /// relation it records, empty (so that a demand binds the session's
    /// versions before they hold a row).
    pub fn prelude(&self) -> Result<Fetched, DataError> {
        let from = self.labels.first().copied().unwrap_or(0);
        let mut f = self.rows(&[])?;
        f.securities = self
            .symbols
            .iter()
            .map(|s| Security {
                key: s.clone(),
                ticker: s.clone(),
                from: from - 86_400_000_000_000,
                to: None,
            })
            .collect();
        Ok(f)
    }

    /// A uniform draw in [0, 1) (xorshift*).
    fn uniform(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        (self.state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The rows of one bar of every name, or of none.
    fn rows(&self, bar: &[(f64, f64, f64, f64)]) -> Result<Fetched, DataError> {
        let t = self.labels.get(self.next).copied().unwrap_or(0);
        let n = bar.len();
        let keys: ArrayRef = Arc::new(StringArray::from(self.symbols.iter().take(n).cloned().collect::<Vec<_>>()));
        let ts: ArrayRef = Arc::new(TimestampNanosecondArray::from(vec![t; n]));
        let mut relations = Vec::new();
        for (k, (rel, col)) in [("equity.open", "O"), ("equity.high", "H"), ("equity.low", "L"), ("equity.close", "P"), ("equity.volume", "V")]
            .into_iter()
            .enumerate()
        {
            let v: Vec<f64> = bar
                .iter()
                .map(|b| match k {
                    0 => b.0,
                    1 => b.1,
                    2 => b.2,
                    3 => b.3,
                    _ => 1000.0,
                })
                .collect();
            let schema = Arc::new(Schema::new(vec![
                Field::new("A", DataType::Utf8, false),
                Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
                Field::new(col, DataType::Float64, true),
            ]));
            relations.push((rel.to_string(), RecordBatch::try_new(schema, vec![keys.clone(), ts.clone(), Arc::new(Float64Array::from(v))])?));
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("calendar".to_string(), "1m".to_string());
        attributes.insert("receipt".to_string(), "recorded".to_string());
        Ok(Fetched {
            relations,
            attributes,
            keys: Keys::Vendor,
            ..Fetched::default()
        })
    }
}

impl Feed for SyntheticFeed {
    fn next(&mut self) -> Result<Option<Fetched>, DataError> {
        let Some(&close) = self.labels.get(self.next) else { return Ok(None) };
        self.clock.sleep_until(close + self.delay);
        let mut bar = Vec::with_capacity(self.symbols.len());
        for i in 0..self.symbols.len() {
            let open = self.prices[i];
            let step = 1.0 + 0.004 * (self.uniform() - 0.5);
            let close_px = (open * step * 100.0).round() / 100.0;
            let high = open.max(close_px) + 0.01;
            let low = open.min(close_px) - 0.01;
            self.prices[i] = close_px;
            bar.push((open, high, low, close_px));
        }
        let f = self.rows(&bar)?;
        self.next += 1;
        Ok(Some(f))
    }
}
