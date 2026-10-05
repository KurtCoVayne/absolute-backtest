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

use std::collections::HashMap;

use super::executor::{Executor, SimExecutor};
use super::time;
use super::{Dataset, ExecConfig, Kernel, RunError, RunResult, Tuple};
use crate::check::Program;
use crate::ir::Resolution;

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
            for (i, tu) in tuples.iter().enumerate() {
                let tuple = k.remap_tuple(tu);
                let key = tuple[k.rels[rel].key_pos].as_time().ok_or_else(|| RunError::Internal(format!("non-timestamp key in `{}`", name)))?;
                events.push((key, rel, i, Event::Tuple { rel, key, avail: key, tuple }));
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

    /// A tuple keyed `key` at native resolution `native` arrived: the
    /// barriers it closes (finer first), after opening the buckets it falls
    /// in at every resolution at or above its own.
    fn advance(&mut self, k: &mut Kernel, key: i64, native: Resolution) -> Vec<(Resolution, i64)> {
        let mut closed = Vec::new();
        for &r in &self.needed {
            if r < native {
                continue;
            }
            let label = time::bucket(r, key);
            match self.open.get(&r).copied() {
                Some(l) if l == label => {}
                Some(l) if l < label => {
                    closed.push((r, l));
                    self.open.insert(r, label);
                    k.open_bucket(r, label);
                }
                Some(_) => {
                    // Keyed before the open bucket: the domain gets it, no barrier moves.
                    k.open_bucket(r, label);
                }
                None => {
                    self.open.insert(r, label);
                    k.open_bucket(r, label);
                }
            }
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
}

impl<'p, 'e> Fold<'p, 'e> {
    pub fn new(kernel: Kernel<'p>, exec: &'e mut dyn Executor) -> Fold<'p, 'e> {
        let sched = Scheduler::new(&kernel);
        let result = RunResult {
            symbols: kernel.symbols.names().to_vec(),
            warnings: kernel.cfg.warnings(),
            ..Default::default()
        };
        Fold {
            kernel,
            exec,
            result,
            sched,
            decided_once: false,
            halted: false,
        }
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
            Event::Tuple { rel, key, tuple, .. } => {
                let native = self.kernel.relation_resolution(rel);
                for (res, label) in self.sched.advance(&mut self.kernel, key, native) {
                    self.barrier(res, label)?;
                }
                self.kernel.insert_fact(rel, tuple)?;
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
