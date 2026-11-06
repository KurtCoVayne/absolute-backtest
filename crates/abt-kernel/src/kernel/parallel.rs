//! Entity-parallel evaluation between barriers (F-5.3; docs/spec/06-performance.md
//! 3.2, M-8). A rule's body is solved over a block of bindings
//! (`Kernel::solve_kleene_block`); when the rule is entity-local (no literal
//! reads the cross-section, `plan::planner::cross_sectional`), its rows after
//! the first literal are independent of one another, so the block is cut by
//! entity across worker kernels that share the main kernel's stores and memo
//! for the length of the step, and the rows come back in the order they left.
//!
//! What a worker shares and what it owns: the stores and the memo are `Shared`
//! (an `Arc` the main kernel writes only while no worker holds it); a worker
//! derives what it misses into an `overlay` memo the main kernel absorbs after
//! the step; its window groups (`rows_state`, keyed by the entity) stay on the
//! worker whose entity it is, so they stay warm across bars.
//!
//! Proposition 3.1: the rows produced do not depend on the thread count. An
//! entity-local rule computes each row from its own entity's inputs in the
//! literal order of the plan; the first literal runs on the main thread, the
//! rest on the worker the entity hashes to; every row carries the index of the
//! binding it came from, and the merge orders the rows by that index, so the
//! block equals the one the single thread produces. `tests/eval_set.rs` holds
//! the two equal on every corpus program and both books.

use super::eval::Stuck;
use super::eval_set::Block;
use super::{CompiledRule, Derived, Kernel, Memo, MemoKey, RunError, Shared, Value};
use crate::ir::Literal;
use std::sync::Arc;

/// A worker's stack: the evaluator recurses through `call` (the main
/// kernel's thread has 512 MB, `on_kernel_thread`).
const WORKER_STACK: usize = 256 << 20;

impl<'p> Kernel<'p> {
    /// The thread count the run asked for: `0` is the machine's cores up to
    /// four, as a study's `--threads`.
    pub(crate) fn threads(&self) -> usize {
        match self.cfg.threads {
            0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4),
            n => n,
        }
    }

    /// A memoised result: the worker's overlay first, then the shared memo.
    pub(crate) fn memo_get(&self, key: &MemoKey) -> Option<&Derived> {
        if self.worker.is_some() {
            if let Some(d) = self.overlay.get(key) {
                return Some(d);
            }
        }
        self.memo.get(key)
    }

    /// A derived result: a worker's goes to its overlay, the main kernel's to
    /// the memo (which no worker holds between steps).
    pub(crate) fn memo_insert(&mut self, key: MemoKey, d: Derived) {
        if self.worker.is_some() {
            self.overlay.insert(key, d);
        } else {
            self.memo.insert(key, d);
        }
    }

    /// A stored relation's block at a key: the main kernel's lookup sorts a
    /// dirty block first; a worker reads blocks the main kernel sealed.
    pub(crate) fn store_lookup(&mut self, rel: usize, key: i64, entity: Option<&Value>) -> super::rows::RowSlice<'_> {
        if self.worker.is_some() {
            self.stores[rel].lookup_sealed(key, entity)
        } else {
            self.stores[rel].lookup(key, entity)
        }
    }

    /// Sort every dirty block once, so the workers read without writing.
    pub(crate) fn seal_stores(&mut self) {
        for st in self.stores.iter_mut() {
            st.seal();
        }
    }

    /// The worker kernels, made once: clones of this kernel that own nothing
    /// heavy (the shares are handed to them per step) and start with empty
    /// caches.
    pub(crate) fn prepare_workers(&mut self) {
        let n = self.threads();
        if n <= 1 || !self.workers.is_empty() || self.worker.is_some() {
            return;
        }
        // The open-loop derived relations in stratum order: what a barrier
        // warms before `decide` demands it (closed-loop relations read the
        // executor's state and stay demand-driven).
        let closed = crate::plan::planner::closed_loop(self.prog);
        let mut warm = Vec::new();
        for stratum in &self.prog.strata {
            for name in stratum {
                if closed.contains(name) {
                    continue;
                }
                if let Some(&rel) = self.rel_ids.get(name) {
                    let info = &self.rels[rel];
                    if !info.stored && !info.rules.is_empty() && !info.rules.iter().all(|&ri| self.skip_rule.get(ri).copied().unwrap_or(false)) {
                        warm.push(rel);
                    }
                }
            }
        }
        self.warm_rels = warm;
        let mut workers = Vec::with_capacity(n);
        for i in 0..n {
            let mut w = self.clone();
            w.worker = Some(i);
            w.workers = Vec::new();
            w.stores = Shared::new(Vec::new());
            w.memo = Shared::default();
            w.overlay = Memo::default();
            w.rows_state.clear();
            w.windows.clear();
            w.rows_floor.clear();
            w.rows_anchor.clear();
            w.asof_memo.clear();
            w.memo_stuck.clear();
            w.memo_errors.clear();
            w.in_progress.clear();
            w.stats = Default::default();
            w.coverage = vec![(0, 0, 0); self.coverage.len()];
            for c in &mut w.literal_counts {
                for x in c.iter_mut() {
                    *x = (0, 0, 0);
                }
            }
            workers.push(w);
        }
        self.workers = workers;
    }

    /// Before a bar's decision: derive the open-loop relations the last
    /// decision demanded (every one, at the first) at `t` in stratum order,
    /// each through `call`, so that every entity-local rule is cut across
    /// the workers while its inputs are already memoised, and `decide` finds
    /// them. A halt met here is memoised and met again by the decision that
    /// demands it (Kleene: an instance halts where it is demanded), so none
    /// is raised here.
    pub(crate) fn warm_strata(&mut self, t: i64) {
        if self.threads() <= 1 || self.worker.is_some() {
            return;
        }
        self.prepare_workers();
        self.seal_stores();
        let rels = self.warm_next.clone().unwrap_or_else(|| self.warm_rels.clone());
        for rel in rels {
            let (key_pos, arity) = {
                let info = &self.rels[rel];
                let arity = self.prog.relations.get(&info.name).map(|s| s.args.len()).unwrap_or(0);
                (info.key_pos, arity)
            };
            if arity == 0 {
                continue;
            }
            let mut pattern = vec![None; arity];
            pattern[key_pos] = Some(Value::Time(t));
            let _ = self.call(rel, &pattern);
        }
    }

    /// After a decision: which of the open-loop relations it demanded at
    /// `t` (their result is in the memo), in stratum order -- the next
    /// decision warms those and no other, so a relation a program reads
    /// only at some bars costs nothing at the others.
    pub(crate) fn note_demanded(&mut self, t: i64) {
        if self.threads() <= 1 || self.worker.is_some() {
            return;
        }
        let key_of = |rel: usize| (rel, vec![Value::Time(t)]);
        let demanded: Vec<usize> = self.warm_rels.iter().copied().filter(|&rel| self.memo.get(&key_of(rel)).is_some()).collect();
        self.warm_next = Some(demanded);
    }

    /// `solve_kleene_block` over one starting binding, cut by entity across
    /// the workers when the rule allows it (`parallel_ok`), the row count
    /// warrants it and this is the main kernel.
    pub(crate) fn solve_block_maybe_parallel(&mut self, cr: &Arc<CompiledRule>, ri: usize, lits: &[Literal], order: &[usize], env: super::Env) -> Result<(Block, Vec<Stuck>), RunError> {
        let n = self.threads();
        let es = cr.entity_slot;
        if self.worker.is_some() || n <= 1 || es.is_none() || order.len() < 2 || !self.parallel_ok.get(ri).copied().unwrap_or(false) {
            return self.solve_kleene_block(cr, lits, order, Block::of(cr.nslots, vec![env]), Some(ri));
        }
        let es = es.expect("checked");
        // The first literal on this thread: it binds the entity.
        let mut first = Block::with_capacity(cr.nslots, 16);
        if self.step(cr, &lits[order[0]], &env, &mut first).is_err() {
            // A halt on the first literal: the plain path reports it exactly.
            return self.solve_kleene_block(cr, lits, order, Block::of(cr.nslots, vec![env]), Some(ri));
        }
        self.count_literal(Some(ri), order[0], 1, first.len(), true);
        if first.is_empty() {
            // As the block solver: nothing left to solve, no later literal counted.
            return Ok((first, Vec::new()));
        }
        first.renumber();
        // A rule with a window takes the cut at any count that gives each
        // worker a row, so that an entity's group stays on one worker; one
        // without waits for `parallel_min_rows`.
        let floor = if self.rule_has_window.get(ri).copied().unwrap_or(false) {
            n
        } else {
            self.cfg.parallel_min_rows.max(1)
        };
        if first.len() < floor {
            return self.solve_kleene_block(cr, lits, &order[1..], first, Some(ri));
        }
        self.prepare_workers();
        let n = self.workers.len();
        let mut shards: Vec<Block> = (0..n).map(|_| Block::with_capacity(cr.nslots, first.len() / n + 1)).collect();
        for i in 0..first.len() {
            let row = first.row(i);
            let w = match row[es] {
                Some(Value::Equity(s)) => s as usize % n,
                _ => 0,
            };
            shards[w].push_row(row, first.origin_of(i));
        }
        let rest = &order[1..];
        // A query after the run, or a decision's own demand, cuts rules too:
        // the blocks the executor or the stream dirtied since the barrier's
        // seal are sorted now, so the workers read without writing.
        if self.stores.iter().any(|st| !st.dirty.is_empty()) {
            self.seal_stores();
        }
        let started = std::time::Instant::now();
        let stores = self.stores.share();
        let memo = self.memo.share();
        let mut workers = std::mem::take(&mut self.workers);
        for w in &mut workers {
            self.sync_worker(w);
        }
        let outcomes: Vec<Result<(Block, Vec<Stuck>), RunError>> = std::thread::scope(|s| {
            let handles: Vec<_> = workers
                .iter_mut()
                .zip(shards)
                .map(|(w, shard)| {
                    w.stores = stores.share();
                    w.memo = memo.share();
                    std::thread::Builder::new()
                        .stack_size(WORKER_STACK)
                        .spawn_scoped(s, move || {
                            let r = w.solve_kleene_block(cr, lits, rest, shard, Some(ri));
                            // Give the shares back before the main thread continues.
                            w.stores = Shared::new(Vec::new());
                            w.memo = Shared::default();
                            r
                        })
                        .map_err(|e| RunError::Internal(format!("cannot spawn a worker: {}", e)))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, RunError>(handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(RunError::Internal("a worker panicked".into())))).collect())
        })?;
        drop(stores);
        drop(memo);
        self.workers = workers;
        self.stats.parallel_steps += 1;
        self.stats.parallel_rows += first.len();
        self.stats.parallel_secs += started.elapsed().as_secs_f64();
        self.absorb_workers();
        let mut blocks = Vec::with_capacity(n);
        let mut stucks: Vec<Stuck> = Vec::new();
        for o in outcomes {
            let (b, s) = o?;
            blocks.push(b);
            stucks.extend(s);
        }
        // The single thread lists stuck instances by the literal they stuck
        // at, then by the binding they came from; the workers' lists are
        // merged into that order (the literal positions are those of `rest`,
        // the same for every worker).
        stucks.sort_by_key(|s| (s.at, s.origin));
        Ok((Block::merge_by_origin(cr.nslots, blocks), stucks))
    }

    /// The per-bar state a worker reads, brought up to this kernel's before a
    /// step: the clock and receipt horizon, the calendars' domains (a bucket
    /// opens at every barrier, and a `rows` window walks the domain), and the
    /// symbol tables and pushed answers when they have grown.
    fn sync_worker(&self, w: &mut Kernel<'p>) {
        w.now = self.now;
        w.received_r = self.received_r;
        w.pushed_read_r = self.pushed_read_r;
        w.as_of = self.as_of;
        if w.domains != self.domains {
            w.domains = self.domains.clone();
            w.domain_vecs.clear();
        }
        if w.schedule != self.schedule {
            w.schedule = self.schedule.clone();
        }
        if w.session_domains.len() != self.session_domains.len() {
            w.session_domains = self.session_domains.clone();
        }
        if w.symbols.names().len() != self.symbols.names().len() {
            w.symbols = self.symbols.clone();
        }
        if w.labels.names().len() != self.labels.names().len() {
            w.labels = self.labels.clone();
        }
        if w.pushed.len() != self.pushed.len() {
            w.pushed = self.pushed.clone();
        }
        if w.last_price.len() != self.last_price.len() {
            w.last_price = self.last_price.clone();
        }
    }

    /// What the workers derived and counted, folded into this kernel: the
    /// overlays' results (a result a worker derived is the result the main
    /// kernel would have derived, so the first copy is kept), the stuck sets
    /// and halts, the counters.
    fn absorb_workers(&mut self) {
        let mut workers = std::mem::take(&mut self.workers);
        for w in &mut workers {
            debug_assert_eq!(w.symbols.names().len(), self.symbols.names().len(), "a worker interned a symbol");
            for (k, d) in std::mem::take(&mut w.overlay).into_entries() {
                if self.memo.get(&k).is_none() {
                    self.memo.insert(k, d);
                }
            }
            for (k, s) in w.memo_stuck.drain() {
                self.memo_stuck.entry(k).or_insert(s);
            }
            for (k, e) in w.memo_errors.drain() {
                self.memo_errors.entry(k).or_insert(e);
            }
            self.stats.absorb(&w.stats);
            w.stats = Default::default();
            for (a, b) in self.coverage.iter_mut().zip(w.coverage.iter_mut()) {
                a.0 += b.0;
                a.1 += b.1;
                a.2 += b.2;
                *b = (0, 0, 0);
            }
            for (a, b) in self.literal_counts.iter_mut().zip(w.literal_counts.iter_mut()) {
                for (x, y) in a.iter_mut().zip(b.iter_mut()) {
                    x.0 += y.0;
                    x.1 += y.1;
                    x.2 += y.2;
                    *y = (0, 0, 0);
                }
            }
        }
        self.workers = workers;
    }
}

impl super::KernelStats {
    /// The counters of a worker's step added to this kernel's.
    pub(crate) fn absorb(&mut self, o: &super::KernelStats) {
        self.window_calls += o.window_calls;
        self.window_bars_solved += o.window_bars_solved;
        self.late_tuples += o.late_tuples;
        self.excluded_after_undefined += o.excluded_after_undefined;
        self.window_noncanonical += o.window_noncanonical;
        self.window_backward += o.window_backward;
        self.pushed_answers += o.pushed_answers;
        self.parallel_secs += o.parallel_secs;
    }
}
