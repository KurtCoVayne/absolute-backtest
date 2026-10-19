//! The resident cache of decoded partitions (D-3.2, D-3.4; X-2.1): row
//! groups decoded on first touch and kept under `resident_budget` bytes,
//! evicted by the clock (second-chance) algorithm over a slab. A partition
//! file is immutable and named by its content's hash (D-2.6), so a row
//! group's decoded batch is keyed by its file and the group's index, and a
//! read through the cache returns exactly the rows a read from the file
//! does.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arrow::array::RecordBatch;

/// A decoded row group's key: the partition file (its name carries its
/// content's hash, so a name is one content) and the group's index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub file: PathBuf,
    pub group: usize,
}

struct Slot {
    key: GroupKey,
    batch: Arc<RecordBatch>,
    bytes: u64,
    /// The clock's reference bit: set on every hit, cleared as the hand
    /// passes; a slot the hand finds cleared is evicted.
    referenced: bool,
}

/// What the cache has done, for the run report and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResidentStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub bytes: u64,
    pub groups: usize,
}

#[derive(Default)]
struct Clock {
    budget: u64,
    used: u64,
    slots: Vec<Option<Slot>>,
    free: Vec<usize>,
    index: HashMap<GroupKey, usize>,
    hand: usize,
    stats: ResidentStats,
}

impl Clock {
    fn get(&mut self, key: &GroupKey) -> Option<Arc<RecordBatch>> {
        let i = *self.index.get(key)?;
        let slot = self.slots[i].as_mut().expect("an indexed slot is occupied");
        slot.referenced = true;
        self.stats.hits += 1;
        Some(slot.batch.clone())
    }

    /// Evicts until `bytes` more fit, sweeping the hand over the slab:
    /// a referenced slot gets its second chance, an unreferenced one goes.
    fn make_room(&mut self, bytes: u64) {
        let n = self.slots.len();
        // Two sweeps clear every reference bit, so the loop ends.
        let mut steps = 0usize;
        while self.used + bytes > self.budget && !self.index.is_empty() && steps < 2 * n + 1 {
            let i = self.hand % n;
            self.hand = (self.hand + 1) % n;
            steps += 1;
            let Some(slot) = self.slots[i].as_mut() else { continue };
            if slot.referenced {
                slot.referenced = false;
                continue;
            }
            let slot = self.slots[i].take().expect("occupied");
            self.index.remove(&slot.key);
            self.used -= slot.bytes;
            self.free.push(i);
            self.stats.evictions += 1;
        }
    }

    fn insert(&mut self, key: GroupKey, batch: Arc<RecordBatch>) {
        let bytes = batch.get_array_memory_size() as u64;
        if bytes > self.budget || self.index.contains_key(&key) {
            return;
        }
        self.make_room(bytes);
        if self.used + bytes > self.budget {
            return;
        }
        let slot = Slot {
            key: key.clone(),
            batch,
            bytes,
            // A new slot is not referenced until it is hit: a scan's groups
            // read once are the first to go.
            referenced: false,
        };
        let i = match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(slot);
                i
            }
            None => {
                self.slots.push(Some(slot));
                self.slots.len() - 1
            }
        };
        self.index.insert(key, i);
        self.used += bytes;
    }
}

/// The resident cache, shared by every reader of a store.
#[derive(Clone, Default)]
pub struct Resident {
    clock: Arc<Mutex<Clock>>,
}

impl Resident {
    /// A cache holding at most `budget` bytes of decoded row groups; 0
    /// keeps nothing (every read decodes from the file).
    pub fn new(budget: u64) -> Resident {
        Resident {
            clock: Arc::new(Mutex::new(Clock { budget, ..Clock::default() })),
        }
    }

    pub fn budget(&self) -> u64 {
        self.clock.lock().map(|c| c.budget).unwrap_or(0)
    }

    /// The decoded row group `key`, from the cache or by `decode`, kept
    /// when it fits the budget.
    pub fn group(&self, key: GroupKey, decode: impl FnOnce() -> Result<RecordBatch, crate::DataError>) -> Result<Arc<RecordBatch>, crate::DataError> {
        if let Ok(mut c) = self.clock.lock() {
            if c.budget == 0 {
                drop(c);
                return decode().map(Arc::new);
            }
            if let Some(b) = c.get(&key) {
                return Ok(b);
            }
            c.stats.misses += 1;
        }
        let batch = Arc::new(decode()?);
        if let Ok(mut c) = self.clock.lock() {
            c.insert(key, batch.clone());
        }
        Ok(batch)
    }

    pub fn stats(&self) -> ResidentStats {
        self.clock
            .lock()
            .map(|c| ResidentStats {
                bytes: c.used,
                groups: c.index.len(),
                ..c.stats
            })
            .unwrap_or_default()
    }
}

/// The default resident budget (D-3.4): half of physical memory, from
/// `/proc/meminfo` where there is one, else 4 GiB; `ABT_RESIDENT_BUDGET`
/// (bytes) overrides it.
pub fn default_budget() -> u64 {
    if let Some(b) = std::env::var("ABT_RESIDENT_BUDGET").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        return b;
    }
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok();
    let total_kib = meminfo
        .as_deref()
        .and_then(|m| m.lines().find(|l| l.starts_with("MemTotal:")))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u64>().ok());
    match total_kib {
        Some(k) => k * 1024 / 2,
        None => 4 << 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Float64Array;
    use arrow::datatypes::{DataType, Field, Schema};

    fn batch(n: usize) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Float64, false)]));
        RecordBatch::try_new(schema, vec![Arc::new(Float64Array::from(vec![1.0; n]))]).unwrap()
    }

    fn key(g: usize) -> GroupKey {
        GroupKey {
            file: PathBuf::from("p.parquet"),
            group: g,
        }
    }

    #[test]
    fn a_hit_is_counted_and_returns_the_same_batch() {
        let r = Resident::new(1 << 20);
        let a = r.group(key(0), || Ok(batch(10))).unwrap();
        let b = r.group(key(0), || panic!("decoded twice")).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let s = r.stats();
        assert_eq!((s.hits, s.misses, s.groups), (1, 1, 1));
    }

    #[test]
    fn the_cache_stays_within_its_budget_and_the_clock_spares_a_referenced_group() {
        let one = batch(1_000).get_array_memory_size() as u64;
        let r = Resident::new(2 * one + one / 2);
        r.group(key(0), || Ok(batch(1_000))).unwrap();
        r.group(key(1), || Ok(batch(1_000))).unwrap();
        // Group 0 is hit: its second chance spares it when group 2 comes.
        r.group(key(0), || panic!("group 0 was evicted")).unwrap();
        r.group(key(2), || Ok(batch(1_000))).unwrap();
        let s = r.stats();
        assert!(s.bytes <= 2 * one + one / 2, "{:?}", s);
        assert_eq!((s.evictions, s.groups), (1, 2), "{:?}", s);
        r.group(key(0), || panic!("group 0 was evicted")).unwrap();
        let mut decoded = false;
        r.group(key(1), || {
            decoded = true;
            Ok(batch(1_000))
        })
        .unwrap();
        assert!(decoded, "group 1, never hit, is the one the clock took");
    }

    #[test]
    fn a_zero_budget_keeps_nothing() {
        let r = Resident::new(0);
        r.group(key(0), || Ok(batch(10))).unwrap();
        let mut decoded = false;
        r.group(key(0), || {
            decoded = true;
            Ok(batch(10))
        })
        .unwrap();
        assert!(decoded);
        assert_eq!(r.stats().groups, 0);
    }
}
