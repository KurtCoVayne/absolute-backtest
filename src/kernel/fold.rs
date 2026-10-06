//! The kernel as a fold over an availability-ordered stream (data-bundle
//! doc, section 2): `state' = step(state, event)`. An event is a primitive
//! tuple with its availability time, or a barrier, the close of a bar at
//! some resolution, after which no tuple with availability at or before
//! that instant is processed as belonging to the bar. Per-bar evaluation
//! still goes through the memoised evaluator, which only ever looks back
//! (`prev`, `lag`, windows, resample), so a time domain that grows as
//! buckets open is observationally the up-front one; the decision barrier
//! runs the executor's fill, the bar's open, the strategy's decisions and
//! the executor's take, in that order, exactly as the batch driver does. A
//! backtest is this fold replayed over the bundle's log; live is the same
//! fold on a feed with another executor attached.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::eval::{WindowKey, WindowRows};
use super::executor::{Executor, SimExecutor};
use super::time;
use super::{Dataset, ExecConfig, Kernel, KernelStats, RunError, RunResult, Store, Sym, Tuple};
use crate::check::Program;
use crate::ir::Resolution;

/// When the fold takes a checkpoint (data-bundle doc, section 2): at the end
/// of every calendar month of the decision bars, or every n decision bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointEvery {
    Month,
    Bars(usize),
}

/// The fold's state at a decision barrier (data-bundle doc, section 2,
/// "Checkpoints"): the facts, the time domains, the windowed groups' rows,
/// the executor's book, the run so far, and the cursor the replay resumes
/// from (every event with availability at or after it is still to come).
/// Derived values are not kept: they are recomputed from the facts on
/// demand.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Of the program, its parameters and the configuration; a restore into
    /// another is refused.
    pub fingerprint: u64,
    pub cursor: i64,
    pub last_bar: i64,
    pub symbols: Vec<String>,
    pub labels: Vec<String>,
    pub stores: Vec<Store>,
    pub domains: Vec<(Resolution, Vec<i64>)>,
    pub last_price: Vec<(Sym, f64)>,
    pub windows: Vec<(WindowKey, Vec<(i64, WindowRows)>)>,
    pub stats: KernelStats,
    pub open_buckets: Vec<(Resolution, i64)>,
    pub decided_once: bool,
    pub result: RunResult,
    pub executor: serde_json::Value,
}

/// FNV-1a of the program's rules and parameters and the configuration.
pub fn fingerprint(prog: &Program, cfg: &ExecConfig) -> u64 {
    let text = format!("{:?}|{:?}|{:?}|{:?}", prog.strategy, prog.rules, prog.params, cfg);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// One event of the stream.
#[derive(Clone, Debug)]
pub enum Event {
    /// A primitive tuple, in the kernel's symbol order, available at `avail`.
    Tuple { rel: usize, key: i64, avail: i64, tuple: Tuple },
    /// The close of bucket `label` at `res`.
    Barrier { res: Resolution, label: i64 },
}

/// The ordered stream of a dataset: every primitive tuple available at its
/// own bar's close (the v1 convention), ordered by availability, then by
/// relation, then by the order the dataset holds them in.
pub struct EventLog {
    pub events: Vec<Event>,
}

impl EventLog {
    pub fn from_dataset(k: &Kernel, ds: &Dataset) -> Result<EventLog, RunError> {
        let mut events: Vec<(i64, usize, usize, Event)> = Vec::new();
        for (name, tuples) in &ds.facts {
            let Some(rel) = k.relation_id(name) else { continue };
            if !k.is_primitive(rel) {
                continue;
            }
            let res = k.relation_resolution(rel);
            let records = ds.availability_of(name).is_some();
            for (i, tu) in tuples.iter().enumerate() {
                let tuple = k.remap_tuple(tu);
                let key = tuple[k.rels[rel].key_pos].as_time().ok_or_else(|| RunError::Internal(format!("non-timestamp key in `{}`", name)))?;
                // Available at its bar's close by convention, or when the
                // bundle recorded it (never before its own bar).
                let avail = if records { ds.available(name, i, key, res).max(key) } else { key };
                events.push((avail, rel, i, Event::Tuple { rel, key, avail, tuple }));
            }
        }
        events.sort_by_key(|(avail, rel, i, _)| (*avail, *rel, *i));
        Ok(EventLog {
            events: events.into_iter().map(|(_, _, _, e)| e).collect(),
        })
    }
}

/// The open bucket per resolution the program needs, and the barriers a
/// tuple's arrival closes.
struct Scheduler {
    needed: Vec<Resolution>,
    open: HashMap<Resolution, i64>,
}

impl Scheduler {
    fn new(k: &Kernel) -> Scheduler {
        let mut needed: Vec<Resolution> = k.rels.iter().map(|r| r.res).collect();
        needed.push(k.prog.resolution);
        needed.sort();
        needed.dedup();
        Scheduler { needed, open: HashMap::new() }
    }

    /// A tuple keyed `key` at native resolution `native`, available at
    /// `avail`, arrived: the barriers its availability closes (finer first),
    /// after opening the bucket the availability reaches and the bucket the
    /// tuple falls in at every resolution at or above its own. A bucket
    /// closes when the stream's availability passes its end, never because
    /// of a key: a late tuple falls into a closed bucket.
    fn advance(&mut self, k: &mut Kernel, key: i64, avail: i64, native: Resolution) -> Vec<(Resolution, i64)> {
        let mut closed = Vec::new();
        for &r in &self.needed {
            if r < native {
                continue;
            }
            let front = time::bucket(r, avail);
            match self.open.get(&r).copied() {
                Some(l) if l == front => {}
                Some(l) if l < front => {
                    closed.push((r, l));
                    self.open.insert(r, front);
                    k.open_bucket(r, front);
                }
                Some(_) => {}
                None => {
                    self.open.insert(r, front);
                    k.open_bucket(r, front);
                }
            }
            // The tuple's own bucket exists whatever its availability.
            k.open_bucket(r, time::bucket(r, key));
        }
        closed
    }

    fn close_all(&mut self) -> Vec<(Resolution, i64)> {
        let mut out: Vec<(Resolution, i64)> = self.open.drain().collect();
        out.sort();
        out
    }
}

/// The fold: a kernel, an executor, the scheduler and the run so far.
pub struct Fold<'p, 'e> {
    pub kernel: Kernel<'p>,
    exec: &'e mut dyn Executor,
    result: RunResult,
    sched: Scheduler,
    decided_once: bool,
    halted: bool,
    last_bar: Option<i64>,
    checkpoint_every: Option<CheckpointEvery>,
    bars_since_checkpoint: usize,
    pending_checkpoint: Option<Checkpoint>,
    /// Whether the current event's barriers ran a decision bar.
    ran_decision: bool,
}

impl<'p, 'e> Fold<'p, 'e> {
    pub fn new(kernel: Kernel<'p>, exec: &'e mut dyn Executor) -> Fold<'p, 'e> {
        let sched = Scheduler::new(&kernel);
        let result = RunResult {
            symbols: kernel.symbols.names().to_vec(),
            warnings: kernel.cfg.warnings(),
            price_relation: kernel.price_rel.map(|id| kernel.rels[id].name.clone()),
            volume_relation: kernel.volume_rel.map(|id| kernel.rels[id].name.clone()),
            base_capital: (!kernel.cfg.compounding).then_some(kernel.cfg.initial_cash),
            ..Default::default()
        };
        Fold {
            kernel,
            exec,
            result,
            sched,
            decided_once: false,
            halted: false,
            last_bar: None,
            checkpoint_every: None,
            bars_since_checkpoint: 0,
            pending_checkpoint: None,
            ran_decision: false,
        }
    }

    /// Take checkpoints on this schedule; `take_checkpoint` hands them out.
    pub fn with_checkpoints(mut self, every: CheckpointEvery) -> Self {
        self.checkpoint_every = Some(every);
        self
    }

    /// The checkpoint the last `step` produced, if any.
    pub fn take_checkpoint(&mut self) -> Option<Checkpoint> {
        self.pending_checkpoint.take()
    }

    /// The fold's state now, for a replay resuming at `cursor`.
    pub fn checkpoint(&self, cursor: i64) -> Result<Checkpoint, RunError> {
        let k = &self.kernel;
        Ok(Checkpoint {
            fingerprint: fingerprint(k.prog, &k.cfg),
            cursor,
            last_bar: self.last_bar.unwrap_or(i64::MIN),
            symbols: k.symbols.names().to_vec(),
            labels: k.labels.names().to_vec(),
            stores: k.stores.clone(),
            domains: {
                let mut d: Vec<(Resolution, Vec<i64>)> = k.domains.iter().map(|(r, s)| (*r, s.iter().copied().collect())).collect();
                d.sort_by_key(|(r, _)| *r);
                d
            },
            last_price: {
                let mut v: Vec<(Sym, f64)> = k.last_price.iter().map(|(s, p)| (*s, *p)).collect();
                v.sort_by_key(|(s, _)| *s);
                v
            },
            windows: k.windows.iter().map(|(key, cache)| (key.clone(), cache.iter().map(|(t, rows)| (*t, rows.clone())).collect())).collect(),
            stats: k.stats(),
            open_buckets: {
                let mut v: Vec<(Resolution, i64)> = self.sched.open.iter().map(|(r, l)| (*r, *l)).collect();
                v.sort();
                v
            },
            decided_once: self.decided_once,
            result: self.result.clone(),
            executor: self.exec.checkpoint().map_err(RunError::Internal)?,
        })
    }

    /// A fold continuing from `cp`: `kernel` is a streaming kernel over the
    /// same program, dataset and configuration (checked by fingerprint and
    /// symbols), `exec` the same kind of executor. Replay the events with
    /// availability at or after `cp.cursor`.
    pub fn restore(mut kernel: Kernel<'p>, exec: &'e mut dyn Executor, cp: Checkpoint) -> Result<Fold<'p, 'e>, RunError> {
        if cp.fingerprint != fingerprint(kernel.prog, &kernel.cfg) {
            return Err(RunError::Config("the checkpoint was taken by another program, parameters or configuration".into()));
        }
        if cp.symbols != kernel.symbols.names() || cp.labels != kernel.labels.names() {
            return Err(RunError::Config("the checkpoint was taken over another dataset (its symbols differ)".into()));
        }
        if cp.stores.len() != kernel.stores.len() {
            return Err(RunError::Config("the checkpoint's relations do not match the program's".into()));
        }
        kernel.stores = cp.stores;
        kernel.domains = cp.domains.into_iter().map(|(r, v)| (r, v.into_iter().collect::<BTreeSet<i64>>())).collect();
        kernel.last_price = cp.last_price.into_iter().collect();
        kernel.windows = cp.windows.into_iter().map(|(key, rows)| (key, rows.into_iter().collect())).collect();
        kernel.stats = cp.stats;
        kernel.memo.clear();
        kernel.asof_memo.clear();
        exec.restore(cp.executor).map_err(RunError::Config)?;
        let mut sched = Scheduler::new(&kernel);
        sched.open = cp.open_buckets.into_iter().collect();
        Ok(Fold {
            kernel,
            exec,
            result: cp.result,
            sched,
            decided_once: cp.decided_once,
            halted: false,
            last_bar: if cp.last_bar == i64::MIN { None } else { Some(cp.last_bar) },
            checkpoint_every: None,
            bars_since_checkpoint: 0,
            pending_checkpoint: None,
            ran_decision: false,
        })
    }

    /// Process one event: a tuple closes the buckets before it (their
    /// barriers run first) and is stored; a decision barrier runs the bar.
    pub fn step(&mut self, ev: Event) -> Result<(), RunError> {
        if self.halted {
            return Err(RunError::Internal("the fold halted on an earlier event".into()));
        }
        let r = self.step_inner(ev);
        if r.is_err() {
            self.halted = true;
        }
        r
    }

    fn step_inner(&mut self, ev: Event) -> Result<(), RunError> {
        match ev {
            Event::Tuple { rel, key, avail, tuple } => {
                let native = self.kernel.relation_resolution(rel);
                self.ran_decision = false;
                for (res, label) in self.sched.advance(&mut self.kernel, key, avail, native) {
                    self.barrier(res, label)?;
                }
                // A checkpoint at the end of a decision bar, before the tuple
                // that closed it is stored: the replay resumes at this tuple.
                if self.ran_decision {
                    let due = match (self.checkpoint_every, self.last_bar) {
                        (Some(CheckpointEvery::Month), Some(last)) => time::month_key(last) != time::month_key(key),
                        (Some(CheckpointEvery::Bars(n)), _) => self.bars_since_checkpoint >= n,
                        _ => false,
                    };
                    if due {
                        self.pending_checkpoint = Some(self.checkpoint(avail)?);
                        self.bars_since_checkpoint = 0;
                    }
                }
                // A tuple keyed before the open bucket of its own resolution
                // arrived after its bar closed: it is available from now on,
                // and what was derived meanwhile is recomputed on demand.
                let late = self.sched.open.get(&native).map(|&open| time::bucket(native, key) < open).unwrap_or(false);
                self.kernel.insert_fact(rel, tuple)?;
                if late {
                    self.kernel.invalidate_from(key);
                }
                Ok(())
            }
            Event::Barrier { res, label } => {
                self.kernel.open_bucket(res, label);
                self.barrier(res, label)
            }
        }
    }

    fn barrier(&mut self, res: Resolution, label: i64) -> Result<(), RunError> {
        if res != self.kernel.prog.resolution {
            return Ok(());
        }
        if self.decided_once {
            self.exec.fill(&mut self.kernel, label, &mut self.result)?;
        }
        self.exec.open_bar(&mut self.kernel, label, &mut self.result)?;
        let by_equity = self.kernel.decide_at(label, &mut self.result)?;
        self.exec.on_decisions(&mut self.kernel, label, &by_equity, &mut self.result)?;
        self.decided_once = true;
        self.last_bar = Some(label);
        self.ran_decision = true;
        self.bars_since_checkpoint += 1;
        Ok(())
    }

    /// The stream ended: close every open bucket (finer first) and let the
    /// executor finish. Returns the run and the kernel (for `explain`).
    pub fn finish(mut self) -> Result<(RunResult, Kernel<'p>), RunError> {
        if self.halted {
            return Err(RunError::Internal("the fold halted on an earlier event".into()));
        }
        for (res, label) in self.sched.close_all() {
            self.barrier(res, label)?;
        }
        if !self.decided_once {
            return Err(RunError::NoBars);
        }
        self.exec.finish(&self.kernel, &mut self.result);
        self.result.stats = self.kernel.stats();
        Ok((self.result, self.kernel))
    }
}

/// Run `prog` on `dataset` as a fold over its event log with the simulated
/// executor: the same decisions, fills and book as `run`.
pub fn run_fold(prog: &Program, dataset: &Dataset, cfg: ExecConfig) -> Result<RunResult, RunError> {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 << 20)
            .spawn_scoped(s, || {
                let kernel = Kernel::new_streaming(prog, dataset, cfg.clone())?;
                let log = EventLog::from_dataset(&kernel, dataset)?;
                let mut exec = SimExecutor::new(cfg);
                let mut fold = Fold::new(kernel, &mut exec);
                for ev in log.events {
                    fold.step(ev)?;
                }
                fold.finish().map(|(r, _)| r)
            })
            .map_err(|e| RunError::Internal(format!("cannot spawn kernel thread: {}", e)))?
            .join()
            .map_err(|_| RunError::Internal("kernel thread panicked".into()))?
    })
}
