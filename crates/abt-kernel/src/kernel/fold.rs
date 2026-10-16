//! The kernel as a fold over the data layer's stream (K-4.2): `state' =
//! step(state, event)`. An event is a stored relation's tuple, received at
//! its `r`, or a barrier, the close of a bucket of one of the program's
//! calendars at an instant after every tuple received by then (D-5.2). A
//! barrier opens its bucket in the calendar's time domain; a barrier of the
//! decision calendar runs the bar: the executor's fill, the bar's open, the
//! strategy's decisions and the executor's take, in that order. Per-bar
//! evaluation goes through the memoised evaluator, which only ever looks
//! back (`prev`, `lag`, windows, resample), so a time domain that grows as
//! buckets open is observationally the whole one. A tuple whose decision
//! bucket was already decided is late (K-4.4): it is stored, what was derived
//! from its key on is recomputed on demand, and decisions taken stand. A
//! backtest is this fold over a replayed stream; live is the same fold on a
//! live stream with another executor attached.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::eval::{WindowKey, WindowRows};
use super::executor::Executor;
use super::time;
use super::{ExecConfig, Kernel, KernelStats, RunError, RunResult, Store, Sym, Tuple};
use crate::check::Program;
use crate::ir::Cal;

/// When the fold takes a checkpoint (data-bundle doc, section 2): at the end
/// of every calendar month of the decision bars, or every n decision bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointEvery {
    Month,
    Bars(usize),
}

/// The fold's state after a decision bar (K-4.7): the facts, the time
/// domains, the windowed groups' rows, the executor's book, the run so far,
/// and the stream's cursor, the seq of the first event still to come.
/// Derived values are not kept: they are recomputed from the facts on
/// demand.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Of the program, its parameters and the configuration; a restore into
    /// another is refused.
    pub fingerprint: u64,
    pub cursor: u64,
    pub last_bar: i64,
    pub symbols: Vec<String>,
    pub labels: Vec<String>,
    pub stores: Vec<Store>,
    pub domains: Vec<(Cal, Vec<i64>)>,
    pub last_price: Vec<(Sym, f64)>,
    pub windows: Vec<(WindowKey, Vec<(i64, WindowRows)>)>,
    pub stats: KernelStats,
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

/// One event of the stream, as the fold takes it (D-5.1).
#[derive(Clone, Debug)]
pub enum Event {
    /// A stored relation's tuple, in the kernel's symbol order, keyed at
    /// `key` and received at `r`; `seq` is its position in the stream.
    Tuple { seq: u64, rel: usize, key: i64, r: i64, tuple: Tuple },
    /// The close of bucket `label` of a calendar.
    Barrier { seq: u64, cal: Cal, label: i64 },
}

impl From<super::bind::StreamEvent> for Event {
    fn from(e: super::bind::StreamEvent) -> Event {
        match e {
            super::bind::StreamEvent::Tuple { seq, rel, key, r, tuple } => Event::Tuple { seq, rel, key, r, tuple },
            super::bind::StreamEvent::Barrier { seq, cal, label } => Event::Barrier { seq, cal, label },
        }
    }
}

/// The fold: a kernel, an executor and the run so far.
pub struct Fold<'p, 'e> {
    pub kernel: Kernel<'p>,
    exec: &'e mut dyn Executor,
    result: RunResult,
    decided_once: bool,
    halted: bool,
    last_bar: Option<i64>,
    checkpoint_every: Option<CheckpointEvery>,
    bars_since_checkpoint: usize,
    pending_checkpoint: Option<Checkpoint>,
    /// Whether the current event's barriers ran a decision bar.
    ran_decision: bool,
    /// The last decision bars, the memo kept for (see `evict_memo`).
    recent: std::collections::VecDeque<i64>,
    /// What the invariants keep between barriers (K-3.3).
    invariants: crate::contract::check::Invariants,
}

impl<'p, 'e> Fold<'p, 'e> {
    pub fn new(kernel: Kernel<'p>, exec: &'e mut dyn Executor) -> Fold<'p, 'e> {
        let result = RunResult {
            symbols: kernel.symbols.names().to_vec(),
            warnings: kernel.cfg.warnings().into_iter().chain(kernel.run_warnings.iter().cloned()).collect(),
            price_relation: kernel.price_rel.map(|id| kernel.rels[id].name.clone()),
            volume_relation: kernel.volume_rel.map(|id| kernel.rels[id].name.clone()),
            base_capital: (!kernel.cfg.compounding).then_some(kernel.cfg.initial_cash),
            multipliers: kernel.contracts.iter().map(|(s, c)| (*s, c.multiplier)).collect(),
            contract: crate::contract::contract(kernel.prog),
            ..Default::default()
        };
        let invariants = crate::contract::check::Invariants::new(kernel.cfg.initial_cash, &result);
        Fold {
            kernel,
            exec,
            result,
            decided_once: false,
            halted: false,
            last_bar: None,
            checkpoint_every: None,
            bars_since_checkpoint: 0,
            pending_checkpoint: None,
            ran_decision: false,
            recent: Default::default(),
            invariants,
        }
    }

    /// The contract its preconditions were checked against at bind (K-3.2);
    /// a resumed run keeps the statuses its checkpoint carries and takes the
    /// bind's.
    pub fn with_contract(mut self, c: crate::contract::Contract) -> Self {
        if self.result.events.is_empty() {
            self.result.contract = c;
        } else {
            for clause in c.clauses.into_iter().filter(|x| x.kind == crate::contract::Kind::Pre) {
                self.result.contract.set(&clause.id, clause.status);
            }
        }
        self
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

    /// The fold's state now, for a replay resuming at the stream's `cursor`.
    pub fn checkpoint(&self, cursor: u64) -> Result<Checkpoint, RunError> {
        let k = &self.kernel;
        Ok(Checkpoint {
            fingerprint: fingerprint(k.prog, &k.cfg),
            cursor,
            last_bar: self.last_bar.unwrap_or(i64::MIN),
            symbols: k.symbols.names().to_vec(),
            labels: k.labels.names().to_vec(),
            stores: k.stores.clone(),
            domains: {
                let mut d: Vec<(Cal, Vec<i64>)> = k.domains.iter().map(|(r, s)| (r.clone(), s.iter().copied().collect())).collect();
                d.sort_by_key(|(r, _)| r.clone());
                d
            },
            last_price: {
                let mut v: Vec<(Sym, f64)> = k.last_price.iter().map(|(s, p)| (*s, *p)).collect();
                v.sort_by_key(|(s, _)| *s);
                v
            },
            windows: k.windows.iter().map(|(key, cache)| (key.clone(), cache.iter().map(|(t, rows)| (*t, rows.clone())).collect())).collect(),
            stats: k.stats(),
            decided_once: self.decided_once,
            result: self.result.clone(),
            executor: self.exec.checkpoint().map_err(RunError::Internal)?,
        })
    }

    /// A fold continuing from `cp`: `kernel` is a bound kernel over the same
    /// program, binding and configuration (checked by fingerprint and
    /// symbols), `exec` the same kind of executor. The stream resumes at the
    /// seq `cp.cursor`.
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
        // A checkpoint's blocks may predate the index: re-sort on first use.
        for (st, info) in kernel.stores.iter_mut().zip(kernel.rels.iter()) {
            st.index_by(info.entity_positions.first().copied());
        }
        kernel.domains = cp.domains.into_iter().map(|(r, v)| (r, v.into_iter().collect::<BTreeSet<i64>>())).collect();
        kernel.last_price = cp.last_price.into_iter().collect();
        kernel.windows = cp.windows.into_iter().map(|(key, rows)| (key, rows.into_iter().collect())).collect();
        kernel.stats = cp.stats;
        kernel.memo.clear();
        kernel.asof_memo.clear();
        kernel.memo_errors.clear();
        kernel.domain_vecs.clear();
        exec.restore(cp.executor).map_err(RunError::Config)?;
        let invariants = crate::contract::check::Invariants::new(kernel.cfg.initial_cash, &cp.result);
        Ok(Fold {
            kernel,
            exec,
            invariants,
            result: cp.result,
            decided_once: cp.decided_once,
            halted: false,
            last_bar: if cp.last_bar == i64::MIN { None } else { Some(cp.last_bar) },
            checkpoint_every: None,
            bars_since_checkpoint: 0,
            pending_checkpoint: None,
            ran_decision: false,
            recent: Default::default(),
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
            Event::Tuple { seq, rel, key, r, tuple } => {
                self.kernel.received_r = self.kernel.received_r.max(r);
                // A checkpoint at the end of a decision bar, before the next
                // tuple is stored: the replay resumes at this tuple.
                if self.ran_decision {
                    let due = match (self.checkpoint_every, self.last_bar) {
                        (Some(CheckpointEvery::Month), Some(last)) => time::month_key(last) != time::month_key(key),
                        (Some(CheckpointEvery::Bars(n)), _) => self.bars_since_checkpoint >= n,
                        _ => false,
                    };
                    if due {
                        self.pending_checkpoint = Some(self.checkpoint(seq)?);
                        self.bars_since_checkpoint = 0;
                    }
                }
                self.ran_decision = false;
                // A tuple whose decision bucket was decided arrived late
                // (K-4.4): it is available from now on, and what was derived
                // meanwhile is recomputed on demand.
                let dcal = self.kernel.prog.decision_cal.clone();
                let late = self.last_bar.is_some_and(|last| self.kernel.label_on(&dcal, key).is_some_and(|l| l <= last));
                self.kernel.insert_fact(rel, tuple)?;
                if late {
                    self.kernel.invalidate_from(key);
                }
                Ok(())
            }
            Event::Barrier { cal, label, .. } => {
                // Every tuple received by the barrier's instant precedes it
                // (D-5.3): the kernel holds what was received by `now`.
                self.kernel.now = super::bind::barrier_at(self.kernel.prog, label);
                self.kernel.open_bucket(&cal, label);
                self.barrier(&cal, label)
            }
        }
    }

    fn barrier(&mut self, res: &Cal, label: i64) -> Result<(), RunError> {
        if *res != self.kernel.prog.decision_cal {
            return Ok(());
        }
        // Outside the run's window a bar is data only.
        if self.kernel.cfg.start.map(|s| label < s).unwrap_or(false) || self.kernel.cfg.end.map(|e| label > e).unwrap_or(false) {
            return Ok(());
        }
        if self.decided_once {
            self.exec.fill(&mut self.kernel, label, &mut self.result)?;
        }
        self.exec.open_bar(&mut self.kernel, label, &mut self.result)?;
        let by_equity = self.kernel.decide_at(label, &mut self.result)?;
        self.exec.on_decisions(&mut self.kernel, label, &by_equity, &mut self.result)?;
        // The invariants after the executor's take (K-3.3, K-4.2).
        let working = self.exec.working();
        let at = self.kernel.now;
        self.invariants.check(&mut self.kernel, self.exec.book(), &working, self.exec.expected(), &mut self.result, label, at)?;
        // Bucketed by time, eviction costs only what it drops: every bar,
        // keeping the last few decision bars' entries.
        self.recent.push_back(label);
        if self.recent.len() > 4 {
            self.recent.pop_front();
        }
        self.kernel.evict_memo(label, self.recent.front().copied().unwrap_or(label));
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
        if !self.decided_once {
            return Err(RunError::NoBars);
        }
        self.exec.finish(&mut self.kernel, &mut self.result);
        self.result.stats = self.kernel.stats();
        self.result.coverage = self.kernel.coverage();
        self.result.literal_counts = self.kernel.literal_counts().to_vec();
        self.result.plan = self.kernel.plan_label().to_string();
        crate::contract::check::postconditions(&mut self.result, self.kernel.prog, self.kernel.cfg.initial_cash, false);
        Ok((self.result, self.kernel))
    }
}
