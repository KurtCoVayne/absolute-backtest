//! A hot version in memory (ST-4.8, ST-6.3): its manifest, its presence,
//! its segments mapped read-only, the directory (per level, column and
//! instrument, the block table built from the segments' runs) with a
//! sparse table per instrument, a decoded-block cache, and the reads the
//! store answers from them: a row filter's rows in canonical order, the
//! newest bucket, a bucket's rows for the stream, and the rows of a
//! level of the calendar ladder (ST-5.3).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use abt_codec::block::{patch, unpack};
use abt_codec::summary::{BlockSummary, Column, Cumulative, SparseTable};
use abt_codec::tick::{decode_value, Exception};
use abt_codec::BLOCK;
use abt_wire::generated::abt::wire::{ColumnSpec, ColumnType, RelationSchema, Role};
use arrow::array::{ArrayRef, BooleanArray, Decimal128Array, Float64Array, Int64Array, RecordBatch, TimestampNanosecondArray, UInt32Array, UInt64Array};
use arrow::datatypes::SchemaRef;

use super::append::Overlay;
use super::ladder::Level;
use super::manifest::{ColumnEntry, HotManifest, Kind, Revision};
use super::presence::{Instrument, Presence};
use super::segment::{Run, Segment};
use crate::error::DataError;
use crate::rows::Layout;
use crate::store::RowFilter;

/// The default budget of the decoded-block cache (ST-5.8).
pub const CACHE_BYTES: usize = 256 << 20;

/// A mapped segment and its runs.
pub struct Mapped {
    pub hash: String,
    mmap: memmap2::Mmap,
    pub runs: Vec<Run>,
}

impl Mapped {
    pub fn map(&self) -> &memmap2::Mmap {
        &self.mmap
    }

    pub fn segment(&self) -> Segment<'_> {
        Segment {
            bytes: &self.mmap[..],
            runs: Vec::new(),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.mmap[..]
    }
}

/// Where a block of an instrument's column lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub seg: u32,
    pub run: u32,
    pub block: u32,
    pub n: u16,
}

/// A column's directory: per instrument, its block table and sparse table.
pub struct ColumnDir {
    pub blocks: Vec<Vec<Loc>>,
    pub sparse: Vec<SparseTable>,
}

/// A level's structures: its presence over its own labels, its columns'
/// directories and what each column holds.
pub struct LevelData {
    /// `None` for the base.
    pub level: Option<Level>,
    pub calendar: String,
    pub presence: Presence,
    pub columns: Vec<ColumnDir>,
    pub entries: Vec<ColumnEntry>,
}

/// A decoded block: integers at the instrument's exponent of the column,
/// and the values as the source wrote them.
pub struct Block {
    pub ints: Vec<i64>,
    pub floats: Vec<f64>,
}

type Key = (u32, u32, u32);

struct Cache {
    map: HashMap<Key, Arc<Block>>,
    order: VecDeque<Key>,
    bytes: usize,
    budget: usize,
}

pub struct HotVersion {
    pub manifest: HotManifest,
    pub segments: Vec<Mapped>,
    /// `levels[0]` is the base; the others the ladder's, in the
    /// manifest's order.
    pub levels: Vec<LevelData>,
    /// What was appended since publication (ST-8): sealed blocks in the
    /// append files and the tails.
    pub overlay: Mutex<Overlay>,
    cache: Mutex<Cache>,
}

/// The rows a read gives, in canonical (t, id, r) order.
#[derive(Clone, Debug, Default)]
pub struct Rows {
    pub id: Vec<u64>,
    pub t: Vec<i64>,
    pub r: Vec<i64>,
    pub seq: Vec<u64>,
    /// Per value column (the residual excluded): integers at the column's
    /// version exponent, and the source's values.
    pub ints: Vec<Vec<i64>>,
    pub floats: Vec<Vec<f64>>,
}

impl Rows {
    fn with_columns(n: usize) -> Rows {
        Rows {
            ints: vec![Vec::new(); n],
            floats: vec![Vec::new(); n],
            ..Rows::default()
        }
    }

    pub fn len(&self) -> usize {
        self.id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.id.is_empty()
    }

    fn push(&mut self, id: u64, t: i64, r: i64, seq: u64, ints: &[i64], floats: &[f64]) {
        self.id.push(id);
        self.t.push(t);
        self.r.push(r);
        self.seq.push(seq);
        for (c, v) in ints.iter().enumerate() {
            self.ints[c].push(*v);
            self.floats[c].push(floats[c]);
        }
    }

    /// Row `k` of an instrument's decoded columns (each its integers and
    /// its values).
    fn push_row(&mut self, id: u64, t: i64, r: i64, seq: u64, cols: &[(Vec<i64>, Vec<f64>)], k: usize) {
        self.id.push(id);
        self.t.push(t);
        self.r.push(r);
        self.seq.push(seq);
        for (c, col) in cols.iter().enumerate() {
            self.ints[c].push(col.0[k]);
            self.floats[c].push(col.1[k]);
        }
    }

    /// The rows reordered by `order` (indices), a column at a time.
    fn permuted(&self, order: &[usize]) -> Rows {
        fn pick<T: Copy>(column: &[T], order: &[usize]) -> Vec<T> {
            order.iter().map(|&i| column[i]).collect()
        }
        Rows {
            id: pick(&self.id, order),
            t: pick(&self.t, order),
            r: pick(&self.r, order),
            seq: pick(&self.seq, order),
            ints: self.ints.iter().map(|c| pick(c, order)).collect(),
            floats: self.floats.iter().map(|c| pick(c, order)).collect(),
        }
    }
}

/// The order of gathered rows by time, then identity, receipt and sequence
/// (D-3.1), a row before one of the same key gathered after it: what a
/// stable sort by that key gives, made in two counting passes (E-139). A
/// whole read of a relation with appended rows sorted the indices of
/// fifteen million rows by a key read through them. `keys` holds per row
/// its instrument's ordinal and its receipt. The rows are placed by the
/// rank of their instrument's id, then by the rank of their time; rows of
/// one instrument at one time (revisions, when every one is read) are then
/// neighbours in the order they were gathered, and are sorted by receipt
/// and sequence. Where an ordinal does not stand for one id, the sort.
fn canonical_order(rows: &Rows, keys: &[(u32, u32, i64)]) -> Vec<usize> {
    let n = rows.len();
    let sorted = || {
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (rows.t[i], rows.id[i], keys[i].2, rows.seq[i]));
        order
    };
    if n == 0 || n > u32::MAX as usize {
        return sorted();
    }
    // The rank of each ordinal's id among the ids: an ordinal names one
    // instrument, and an instrument has one ordinal.
    let ordinals = keys.iter().map(|k| k.1 as usize + 1).max().unwrap_or(0);
    let mut id_of: Vec<Option<u64>> = vec![None; ordinals];
    for (k, &id) in keys.iter().zip(&rows.id) {
        match id_of[k.1 as usize] {
            None => id_of[k.1 as usize] = Some(id),
            Some(known) if known == id => {}
            Some(_) => return sorted(),
        }
    }
    let mut by_id: Vec<usize> = (0..ordinals).filter(|&o| id_of[o].is_some()).collect();
    by_id.sort_by_key(|&o| id_of[o]);
    if by_id.windows(2).any(|w| id_of[w[0]] == id_of[w[1]]) {
        return sorted();
    }
    let mut id_rank = vec![0u32; ordinals];
    for (rank, &o) in by_id.iter().enumerate() {
        id_rank[o] = rank as u32;
    }
    // The rank of each row's time among the times: an instrument's rows
    // come in time order, so a row's time is beside the one before it.
    let mut times = Times {
        known: Vec::new(),
        at: 0,
        fresh: Vec::new(),
    };
    for &t in &rows.t {
        times.note(t);
    }
    times.merge();
    let times = times.known;
    let mut at = 0usize;
    let t_rank: Vec<u32> = rows
        .t
        .iter()
        .map(|&t| {
            if times[at] != t {
                at = if times.get(at + 1) == Some(&t) { at + 1 } else { times.partition_point(|&x| x < t) };
            }
            at as u32
        })
        .collect();
    // Where each rank's rows start, from how many rows each rank has.
    let starts = |ranks: usize, of: &dyn Fn(usize) -> usize| {
        let mut start = vec![0usize; ranks + 1];
        for i in 0..n {
            start[of(i) + 1] += 1;
        }
        let mut sum = 0;
        for s in start.iter_mut() {
            sum += *s;
            *s = sum;
        }
        start
    };
    let instrument = |i: usize| id_rank[keys[i].1 as usize] as usize;
    let mut start = starts(by_id.len(), &instrument);
    let mut by_instrument = vec![0u32; n];
    for i in 0..n {
        let slot = &mut start[instrument(i)];
        by_instrument[*slot] = i as u32;
        *slot += 1;
    }
    let mut start = starts(times.len(), &|i| t_rank[i] as usize);
    let mut order = vec![0usize; n];
    // The instrument last placed at each time: the next row placed there
    // is of the same instrument only when two rows share a key's time and
    // identity.
    let mut last = vec![usize::MAX; times.len()];
    let mut shared = false;
    for &i in &by_instrument {
        let (i, at) = (i as usize, t_rank[i as usize] as usize);
        shared |= last[at] == instrument(i);
        last[at] = instrument(i);
        order[start[at]] = i;
        start[at] += 1;
    }
    if shared {
        let same = |a: usize, b: usize| t_rank[a] == t_rank[b] && keys[a].1 == keys[b].1;
        let mut a = 0;
        while a < n {
            let mut b = a + 1;
            while b < n && same(order[a], order[b]) {
                b += 1;
            }
            order[a..b].sort_by_key(|&i| (keys[i].2, rows.seq[i]));
            a = b;
        }
    }
    order
}

/// The distinct times met so far, ascending, with those met since the last
/// merge beside them. Times arrive in ascending runs (an instrument's rows
/// in label order), nearly all of them known already: `at` is the place of
/// the last one among the known, and the next is looked for from there.
struct Times {
    known: Vec<i64>,
    at: usize,
    fresh: Vec<i64>,
}

impl Times {
    fn note(&mut self, x: i64) {
        // `at` is brought to the first known time not before `x`: one step
        // on for the next time of a run, a search otherwise.
        if self.known.get(self.at).is_some_and(|&k| k < x) {
            self.at += 1;
            if self.known.get(self.at).is_some_and(|&k| k < x) {
                self.at += self.known[self.at..].partition_point(|&k| k < x);
            }
        } else if self.at > 0 && self.known[self.at - 1] >= x {
            self.at = self.known[..self.at].partition_point(|&k| k < x);
        }
        if self.known.get(self.at) != Some(&x) {
            self.fresh.push(x);
            if self.fresh.len() >= (self.known.len() / 4).max(1024) {
                self.merge();
            }
        }
    }

    /// The times met since the last merge join the known ones.
    fn merge(&mut self) {
        if self.fresh.is_empty() {
            return;
        }
        self.known.append(&mut self.fresh);
        self.known.sort_unstable();
        self.known.dedup();
        self.at = 0;
    }
}

/// The set bits of `words` in `[from, to)`.
pub(crate) fn ones(words: &[u64], from: usize, to: usize) -> impl Iterator<Item = usize> + '_ {
    let mut i = from;
    std::iter::from_fn(move || {
        while i < to {
            let w = words[i / 64] >> (i % 64);
            if w == 0 {
                i = (i / 64 + 1) * 64;
                continue;
            }
            let tz = w.trailing_zeros() as usize;
            i += tz;
            if i >= to {
                return None;
            }
            i += 1;
            return Some(i - 1);
        }
        None
    })
}

/// The cache key of a block: the level in the column's high byte.
fn key(level: usize, c: usize, inst: usize, b: usize) -> Key {
    (((level as u32) << 24) | c as u32, inst as u32, b as u32)
}

impl HotVersion {
    /// Opens a version under the store's root, mapping its segments.
    /// `verify` checks every segment's hash (recovery, S4).
    pub fn open(root: &Path, relation: &str, version: &str, verify: bool) -> Result<HotVersion, DataError> {
        let dir = root.join(HotManifest::dir(relation, version));
        let manifest: HotManifest = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
        Self::open_manifest(root, manifest, verify)
    }

    pub fn open_manifest(root: &Path, manifest: HotManifest, verify: bool) -> Result<HotVersion, DataError> {
        let dir = root.join(HotManifest::dir(&manifest.relation, &manifest.version));
        Self::open_manifest_at(root, &dir, manifest, verify)
    }

    /// Opens a version whose files sit in `dir` (a staging directory before
    /// publication); its segments are under the store's root.
    pub fn open_manifest_at(root: &Path, dir: &Path, manifest: HotManifest, verify: bool) -> Result<HotVersion, DataError> {
        if manifest.levels.is_empty() {
            return Err(DataError::Schema(format!("`{}`: a hot version without a base level", manifest.relation)));
        }
        let mut segments: Vec<Mapped> = Vec::new();
        let mut index: HashMap<String, u32> = HashMap::new();
        for s in manifest.segments() {
            if index.contains_key(&s.hash) {
                continue;
            }
            let file = fs::File::open(root.join(&s.file))?;
            // SAFETY: a segment is written once under the name of its hash
            // and never written again (ST-7.1); the map is read only.
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            let runs = Segment::parse(&mmap[..], verify).map_err(|e| DataError::Schema(format!("{}: {}", s.file, e)))?.runs;
            index.insert(s.hash.clone(), segments.len() as u32);
            segments.push(Mapped { hash: s.hash.clone(), mmap, runs });
        }
        let mut levels = Vec::with_capacity(manifest.levels.len());
        for (li, entry) in manifest.levels.iter().enumerate() {
            let entries: Vec<ColumnEntry> = if li == 0 || entry.columns.is_empty() { manifest.columns.clone() } else { entry.columns.clone() };
            let presence = Presence::from_bytes(&fs::read(dir.join(&entry.presence))?, entries.len())?;
            let level = if li == 0 {
                None
            } else {
                Some(Level::parse(&entry.calendar).ok_or_else(|| DataError::Schema(format!("`{}`: a level named `{}`", manifest.relation, entry.calendar)))?)
            };
            let mut columns = Vec::with_capacity(entries.len());
            for (c, col) in entries.iter().enumerate() {
                let mut blocks: Vec<Vec<Loc>> = vec![Vec::new(); presence.instruments.len()];
                for sref in entry.segments.get(c).map(|v| v.as_slice()).unwrap_or(&[]) {
                    let si = index[&sref.hash];
                    let seg = &segments[si as usize];
                    for (ri, run) in seg.runs.iter().enumerate() {
                        let inst = run.instrument as usize;
                        if inst >= blocks.len() {
                            return Err(DataError::Schema(format!(
                                "`{}`: segment {} names instrument {} of {}",
                                manifest.relation,
                                sref.hash,
                                inst,
                                blocks.len()
                            )));
                        }
                        let table = &mut blocks[inst];
                        table.truncate(run.first_block as usize);
                        if table.len() != run.first_block as usize {
                            return Err(DataError::Schema(format!(
                                "`{}`: a run of instrument {} starts at block {} of {}",
                                manifest.relation,
                                inst,
                                run.first_block,
                                table.len()
                            )));
                        }
                        let s = seg.segment();
                        for b in 0..run.blocks as usize {
                            let h = s.header(run, b);
                            table.push(Loc {
                                seg: si,
                                run: ri as u32,
                                block: b as u32,
                                n: h.n,
                            });
                        }
                    }
                }
                // The blocks a column keeps are at the instrument's exponent.
                // A run a later one superseded may not be: a rebuilt
                // instrument's earlier run, at the exponent before, in a
                // segment the version keeps for its other instruments
                // (E-111).
                for (i, table) in blocks.iter().enumerate() {
                    if let Some(run) = table
                        .iter()
                        .map(|l| &segments[l.seg as usize].runs[l.run as usize])
                        .find(|run| run.exponent != presence.instruments[i].exponents[c])
                    {
                        return Err(DataError::Schema(format!(
                            "`{}`: instrument {}'s `{}` run is at exponent {}, its presence says {}",
                            manifest.relation, i, col.name, run.exponent, presence.instruments[i].exponents[c]
                        )));
                    }
                }
                let mut sparse = Vec::with_capacity(blocks.len());
                for (i, table) in blocks.iter().enumerate() {
                    let rows: u64 = table.iter().map(|l| l.n as u64).sum();
                    if rows != presence.instruments[i].rows {
                        return Err(DataError::Schema(format!(
                            "`{}` level `{}`: instrument {} has {} rows of `{}` and {} in its presence",
                            manifest.relation, entry.calendar, presence.instruments[i].id, rows, col.name, presence.instruments[i].rows
                        )));
                    }
                    let summaries: Vec<BlockSummary> = table
                        .iter()
                        .map(|l| segments[l.seg as usize].segment().summary(&segments[l.seg as usize].runs[l.run as usize], l.block as usize))
                        .collect();
                    sparse.push(SparseTable::of(&summaries));
                }
                columns.push(ColumnDir { blocks, sparse });
            }
            levels.push(LevelData {
                level,
                calendar: entry.calendar.clone(),
                presence,
                columns,
                entries,
            });
        }
        let budget = std::env::var("ABT_BLOCK_CACHE").ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(CACHE_BYTES);
        // The overlay: the journaled blocks since publication; the tails
        // come from the log (the store replays it, ST-8.8).
        let base = &levels[0];
        let ncols = manifest.columns.len() - 1;
        let published: Vec<(u64, u64, Option<i64>, Vec<u8>)> = base
            .presence
            .instruments
            .iter()
            .map(|i| (i.id, i.rows, Some(base.presence.labels[i.last as usize]), i.exponents[..ncols].to_vec()))
            .collect();
        let value_names: Vec<String> = manifest.columns.iter().take(ncols).map(|c| c.name.clone()).collect();
        let kinds: Vec<Kind> = manifest.columns.iter().take(ncols).map(|c| c.kind).collect();
        let named: Vec<&str> = manifest.levels.iter().map(|l| l.presence.as_str()).collect();
        Overlay::remove_earlier_generations(dir, manifest.generation, &named)?;
        let mut overlay = Overlay::new(dir, &value_names, &kinds, published, manifest.schema.calendar.clone(), manifest.generation);
        overlay.replay_journal()?;
        Ok(HotVersion {
            manifest,
            segments,
            levels,
            overlay: Mutex::new(overlay),
            cache: Mutex::new(Cache {
                map: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
                budget,
            }),
        })
    }

    /// The base level's presence.
    pub fn presence(&self) -> &Presence {
        &self.levels[0].presence
    }

    /// The base level's column directories.
    pub fn columns(&self) -> &[ColumnDir] {
        &self.levels[0].columns
    }

    /// The index of a ladder level by name (`levels[0]` is the base).
    pub fn level_index(&self, name: &str) -> Option<usize> {
        self.levels.iter().position(|l| l.level.is_some() && l.calendar == name)
    }

    /// The value columns but the receipt residual, of a level: a ladder
    /// level's are its value and its base-row count; its sequence column
    /// and residual are read apart.
    pub fn value_columns_of(&self, level: usize) -> usize {
        if level == 0 {
            self.levels[level].entries.len() - 1
        } else {
            self.levels[level].entries.len() - 2
        }
    }

    /// The index of a ladder level's sequence column.
    fn seq_column(&self, level: usize) -> usize {
        self.levels[level].entries.len() - 2
    }

    /// The base's value columns but the receipt residual.
    pub fn value_columns(&self) -> usize {
        self.value_columns_of(0)
    }

    fn residual_column(&self, level: usize) -> usize {
        self.levels[level].entries.len() - 1
    }

    /// Block `b` of instrument `inst`'s column `c` of the base, decoded
    /// (through the cache).
    pub fn block(&self, c: usize, inst: usize, b: usize) -> Arc<Block> {
        self.block_at(0, c, inst, b)
    }

    pub fn block_at(&self, level: usize, c: usize, inst: usize, b: usize) -> Arc<Block> {
        let k = key(level, c, inst, b);
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(blk) = cache.map.get(&k) {
                return blk.clone();
            }
            let blk = Arc::new(self.decode(level, c, inst, b));
            let bytes = blk.ints.len() * 16;
            while cache.bytes + bytes > cache.budget {
                let Some(old) = cache.order.pop_front() else { break };
                if let Some(gone) = cache.map.remove(&old) {
                    cache.bytes -= gone.ints.len() * 16;
                }
            }
            cache.map.insert(k, blk.clone());
            cache.order.push_back(k);
            cache.bytes += bytes;
            return blk;
        }
        Arc::new(self.decode(level, c, inst, b))
    }

    fn decode(&self, level: usize, c: usize, inst: usize, b: usize) -> Block {
        let lv = &self.levels[level];
        let loc = lv.columns[c].blocks[inst][b];
        let seg = &self.segments[loc.seg as usize];
        let run = &seg.runs[loc.run as usize];
        let s = seg.segment();
        let (h, packed, exceptions) = s.packed(run, loc.block as usize);
        let mut out = [0u64; BLOCK];
        unpack(&packed, &mut out);
        patch(&exceptions, 0, BLOCK, &mut out);
        let n = h.n as usize;
        let scale = abt_agg::exact::pow10((run.exponent as i32 - h.e as i32).max(0) as u32) as i64;
        let mut ints: Vec<i64> = (0..n).map(|j| h.base.wrapping_add(h.g.wrapping_mul(out[j]) as i64).wrapping_mul(scale)).collect();
        // A value off any grid counts as 0, as the summaries count it (A-D54).
        abt_codec::block::zero_raw(&exceptions, 0, n, &mut ints);
        let floats: Vec<f64> = match lv.entries[c].kind {
            Kind::Tick | Kind::TickF32 => {
                let mut raw = exceptions
                    .iter()
                    .filter_map(|x| if let Exception::Raw { position, bits } = x { Some((*position as usize, *bits)) } else { None })
                    .peekable();
                (0..n)
                    .map(|j| {
                        if raw.peek().is_some_and(|(p, _)| *p == j) {
                            let (_, bits) = raw.next().unwrap();
                            return f64::from_bits(bits);
                        }
                        decode_value(h.base, h.g, h.e as u8, h.recip_exact(), h.f32(), out[j])
                    })
                    .collect()
            }
            Kind::Int | Kind::Residual => ints.iter().map(|&v| v as f64).collect(),
        };
        Block { ints, floats }
    }

    /// The block's hash (Proposition 7.1): BLAKE3 over its header, words
    /// and exceptions.
    pub fn block_hash(&self, c: usize, inst: usize, b: usize) -> blake3::Hash {
        self.block_hash_at(0, c, inst, b)
    }

    pub fn block_hash_at(&self, level: usize, c: usize, inst: usize, b: usize) -> blake3::Hash {
        let loc = self.levels[level].columns[c].blocks[inst][b];
        let seg = &self.segments[loc.seg as usize];
        seg.segment().block_hash(&seg.runs[loc.run as usize], loc.block as usize)
    }

    /// The block as its encoder produced it (for a byte-for-byte copy).
    pub fn encoded(&self, c: usize, inst: usize, b: usize) -> abt_codec::tick::Encoded {
        self.encoded_at(0, c, inst, b)
    }

    pub fn encoded_at(&self, level: usize, c: usize, inst: usize, b: usize) -> abt_codec::tick::Encoded {
        let loc = self.levels[level].columns[c].blocks[inst][b];
        let seg = &self.segments[loc.seg as usize];
        let s = seg.segment();
        let (h, packed, exceptions) = s.packed(&seg.runs[loc.run as usize], loc.block as usize);
        let mut out = [0u64; BLOCK];
        unpack(&packed, &mut out);
        abt_codec::tick::Encoded {
            base: h.base,
            g: h.g,
            e: h.e as u8,
            width: h.width,
            n: h.n,
            recip_exact: h.recip_exact(),
            f32: h.f32(),
            offsets: out[..h.n as usize].to_vec(),
            exceptions,
        }
    }

    /// The values of rows `[a, b)` of an instrument's column: integers and
    /// the source's values.
    fn values(&self, level: usize, c: usize, inst: usize, a: u64, b: u64) -> (Vec<i64>, Vec<f64>) {
        let mut ints = Vec::with_capacity((b - a) as usize);
        let mut floats = Vec::with_capacity((b - a) as usize);
        let mut k = a;
        while k < b {
            let blk = (k / BLOCK as u64) as usize;
            let lo = (k % BLOCK as u64) as usize;
            let block = self.block_at(level, c, inst, blk);
            let hi = (block.ints.len() as u64).min(lo as u64 + (b - k)) as usize;
            assert!(hi > lo, "a block of {} rows inside a run (every block but the last is full, ST-4.1)", block.ints.len());
            ints.extend_from_slice(&block.ints[lo..hi]);
            floats.extend_from_slice(&block.floats[lo..hi]);
            k += (hi - lo) as u64;
        }
        (ints, floats)
    }

    /// A column of an instrument of the base as the range rules read it.
    pub fn column(&self, c: usize, inst: usize) -> ColumnView<'_> {
        self.column_at(0, c, inst)
    }

    pub fn column_at(&self, level: usize, c: usize, inst: usize) -> ColumnView<'_> {
        ColumnView { v: self, level, c, inst }
    }

    /// The instruments a filter names, as ordinals in order; every
    /// instrument when it names none.
    fn ordinals(&self, level: usize, f: &RowFilter) -> Vec<usize> {
        let p = &self.levels[level].presence;
        match &f.identity {
            Some(ids) => {
                let mut o: Vec<usize> = ids.iter().filter_map(|&id| p.ordinal(id).map(|o| o as usize)).collect();
                o.sort_unstable();
                o.dedup();
                o
            }
            None => (0..p.instruments.len()).collect(),
        }
    }

    /// The revisions of an instrument at a label, by receipt.
    fn revisions_at(&self, id: u64, label: i64) -> &[Revision] {
        let rev = &self.manifest.revisions;
        let lo = rev.partition_point(|x| (x.id, x.label) < (id, label));
        let hi = rev.partition_point(|x| (x.id, x.label) <= (id, label));
        &rev[lo..hi]
    }

    /// The filter's rows of the base with labels in `[lo, hi)` (label
    /// indices), in canonical (t, id, r) order (D-3.1).
    pub fn read(&self, f: &RowFilter, lo: u32, hi: u32) -> Result<Rows, DataError> {
        self.read_at(0, f, lo, hi)
    }

    /// The filter's rows of a level: a ladder level's rows are its bars,
    /// each at its bucket's label with the bucket's receipt and the
    /// sequence of its last base row; revisions are the base's.
    pub fn read_at(&self, level: usize, f: &RowFilter, lo: u32, hi: u32) -> Result<Rows, DataError> {
        let lv = &self.levels[level];
        let presence = &lv.presence;
        let (flo, fhi) = presence.label_range(f.t0, f.t1);
        // Label indices the caller bounds the read by, beyond the filter's.
        let explicit = lo != 0 || hi != u32::MAX;
        let (lo, hi) = (lo.max(flo), hi.min(fhi));
        let ncols = self.value_columns_of(level);
        let mut out = Rows::with_columns(ncols);
        let overlay_rows = level == 0 && !self.overlay.lock().map(|ov| ov.is_empty()).unwrap_or(true);
        if lo >= hi && !overlay_rows {
            return Ok(out);
        }
        let residual = self.residual_column(level);
        let has_revisions = level == 0 && !self.manifest.revisions.is_empty();
        // Per label, the rows: counted first so each instrument's rows go
        // to their places in (label, instrument) order without a sort.
        let labels = hi.saturating_sub(lo) as usize;
        let ordinals = self.ordinals(level, f);
        let mut per_inst: Vec<(usize, u64, u64)> = Vec::new();
        if lo < hi {
            for &o in &ordinals {
                let inst = &presence.instruments[o];
                let (a, b) = inst.rows_in(lo, hi - 1);
                if a < b {
                    per_inst.push((o, a, b));
                }
            }
        }
        let total: u64 = per_inst.iter().map(|(_, a, b)| b - a).sum();
        let mut gathered = Rows::with_columns(ncols);
        let mut keys: Vec<(u32, u32, i64)> = Vec::with_capacity(total as usize);
        let mut extra = false;
        // The overlay's rows (ST-8): the base level's appended rows, whose
        // integers may sit at an exponent the appends raised, admitted by
        // receipt and sequence. They sit beyond the published labels, so
        // the rows are sorted after (`extra`).
        let mut raised: Vec<Vec<i64>> = Vec::new();
        if level == 0 {
            let mut ov = self.overlay.lock().unwrap_or_else(|p| p.into_inner());
            if !ov.is_empty() {
                let (t0, t1) = if explicit { self.time_bounds(f, lo, hi) } else { (f.t0, f.t1) };
                let mut wanted: Vec<usize> = match &f.identity {
                    Some(ids) => ids.iter().filter_map(|id| ov.by_id.get(id).copied()).collect(),
                    None => (0..ov.instruments.len()).collect(),
                };
                wanted.sort_unstable();
                wanted.dedup();
                for o in wanted {
                    let id = ov.instruments[o].id;
                    ov.for_each_row(o, t0, t1, &mut |label, r, seq, ints, floats| {
                        if f.r_max.is_none_or(|m| r <= m) && f.as_of_seq.is_none_or(|n| seq <= n) {
                            gathered.push(id, label, r, seq, ints, floats);
                            keys.push((u32::MAX, o as u32, r));
                            extra = true;
                        }
                    })?;
                }
                raised = (0..presence.instruments.len())
                    .map(|o| {
                        (0..ncols)
                            .map(|c| {
                                let e = presence.instruments[o].exponents[c];
                                abt_agg::exact::pow10(ov.instruments[o].exponents[c].saturating_sub(e) as u32) as i64
                            })
                            .collect()
                    })
                    .collect();
            }
        }
        for &(o, a, b) in &per_inst {
            let inst = &presence.instruments[o];
            let (res, _) = self.values(level, residual, o, a, b);
            let seqs: Option<Vec<i64>> = lv.level.map(|_| self.values(level, self.seq_column(level), o, a, b).0);
            let mut cols: Vec<(Vec<i64>, Vec<f64>)> = (0..ncols).map(|c| self.values(level, c, o, a, b)).collect();
            if let Some(up) = raised.get(o) {
                for (c, col) in cols.iter_mut().enumerate() {
                    if up[c] != 1 {
                        for v in col.0.iter_mut() {
                            *v = v.wrapping_mul(up[c]);
                        }
                    }
                }
            }
            let i0 = (lo.max(inst.first) - inst.first) as usize;
            let i1 = (hi.min(inst.last + 1) - inst.first) as usize;
            for (k, bit) in ones(inst.presence.words(), i0, i1).enumerate() {
                let row = a + k as u64;
                let label_ix = inst.first + bit as u32;
                let t = presence.labels[label_ix as usize];
                // A bar's residual is from its bucket's start (A-D50).
                let r = match lv.level {
                    Some(l) => l.bucket_start(t) + res[k],
                    None => t + res[k],
                };
                let seq = match &seqs {
                    Some(s) => s[k] as u64,
                    None => inst.seq_of(row),
                };
                let admitted = f.r_max.is_none_or(|m| r <= m) && f.as_of_seq.is_none_or(|n| seq <= n);
                let revs = if has_revisions { self.revisions_at(inst.id, t) } else { &[] };
                if revs.is_empty() {
                    if admitted {
                        gathered.push_row(inst.id, t, r, seq, &cols, k);
                        keys.push((label_ix, o as u32, r));
                    }
                } else {
                    // The superseded rows admitted, then the current one.
                    let ints: Vec<i64> = cols.iter().map(|c| c.0[k]).collect();
                    let floats: Vec<f64> = cols.iter().map(|c| c.1[k]).collect();
                    let mut candidates: Vec<(i64, u64, Vec<i64>, Vec<f64>)> = Vec::new();
                    for x in revs {
                        if f.r_max.is_none_or(|m| x.r <= m) && f.as_of_seq.is_none_or(|n| x.seq <= n) {
                            let (xi, xf) = self.revision_values(x);
                            candidates.push((x.r, x.seq, xi, xf));
                        }
                    }
                    if admitted {
                        candidates.push((r, seq, ints, floats));
                    }
                    if f.latest_only {
                        // The latest admitted by receipt (then by sequence).
                        if let Some(best) = candidates.iter().max_by_key(|c| (c.0, c.1)) {
                            gathered.push(inst.id, t, best.0, best.1, &best.2, &best.3);
                            keys.push((label_ix, o as u32, best.0));
                        }
                    } else {
                        extra = extra || candidates.len() > 1;
                        for c in &candidates {
                            gathered.push(inst.id, t, c.0, c.1, &c.2, &c.3);
                            keys.push((label_ix, o as u32, c.0));
                        }
                    }
                }
            }
        }
        // Into (label, instrument, r) order.
        if gathered.is_empty() {
            return Ok(out);
        }
        if extra {
            // By time, then identity, receipt and sequence (D-3.1).
            return Ok(gathered.permuted(&canonical_order(&gathered, &keys)));
        }
        let mut count = vec![0usize; labels + 1];
        for k in &keys {
            count[(k.0 - lo) as usize + 1] += 1;
        }
        for i in 1..count.len() {
            count[i] += count[i - 1];
        }
        let mut place = vec![0usize; gathered.len()];
        for (i, k) in keys.iter().enumerate() {
            let slot = &mut count[(k.0 - lo) as usize];
            place[*slot] = i;
            *slot += 1;
        }
        out = gathered.permuted(&place);
        Ok(out)
    }

    /// The distinct times of the base's rows in `[t0, t1]`, ascending, with
    /// no row read: the published labels an instrument's presence marks,
    /// and the labels of the appended rows. It is the set of times `read`
    /// gives a filter that cuts by neither receipt, sequence nor identity:
    /// such a read gives one row or more for every marked label of every
    /// instrument (a revised label still has its current row) and every
    /// appended row. A label no instrument marks is left out, as no row is
    /// read at it.
    pub fn distinct_labels(&self, t0: Option<i64>, t1: Option<i64>) -> Result<Vec<i64>, DataError> {
        let presence = &self.levels[0].presence;
        let (lo, hi) = presence.label_range(t0, t1);
        let span = hi.saturating_sub(lo) as usize;
        let mut marked = vec![false; span];
        let mut left = span;
        for inst in &presence.instruments {
            if left == 0 {
                break;
            }
            if inst.rows == 0 || inst.last < lo || inst.first >= hi {
                continue;
            }
            let i0 = (lo.max(inst.first) - inst.first) as usize;
            let i1 = (hi.min(inst.last + 1) - inst.first) as usize;
            for bit in ones(inst.presence.words(), i0, i1) {
                let at = (inst.first + bit as u32 - lo) as usize;
                if !marked[at] {
                    marked[at] = true;
                    left -= 1;
                }
            }
        }
        let mut times = Times {
            known: marked.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| presence.labels[lo as usize + i]).collect(),
            at: 0,
            fresh: Vec::new(),
        };
        let mut ov = self.overlay.lock().unwrap_or_else(|p| p.into_inner());
        if !ov.is_empty() {
            ov.for_each_label(t0, t1, &mut |label| times.note(label))?;
        }
        times.merge();
        Ok(times.known)
    }

    /// A number no value of the base's value column `c` is below, over
    /// every row the version holds (published, superseded and appended),
    /// with no block decoded: the least of the published blocks' floors
    /// (`append::block_floor`), of the superseded rows' values and of the
    /// appended ones (`Overlay::least_value`). Infinity for a version
    /// without a row; `None` for a column that is not a tick column and
    /// where a value is not a number.
    pub fn least_value(&self, c: usize) -> Result<Option<f64>, DataError> {
        let lv = &self.levels[0];
        if c >= self.value_columns() || !matches!(lv.entries[c].kind, Kind::Tick | Kind::TickF32) {
            return Ok(None);
        }
        let mut least = f64::INFINITY;
        for l in lv.columns[c].blocks.iter().flatten() {
            let seg = &self.segments[l.seg as usize];
            let (s, run) = (seg.segment(), &seg.runs[l.run as usize]);
            let h = s.header(run, l.block as usize);
            let exceptions = if h.raw() { s.exceptions(run, l.block as usize) } else { Vec::new() };
            let Some(floor) = super::append::block_floor(&h, &exceptions) else { return Ok(None) };
            least = least.min(floor);
        }
        for x in &self.manifest.revisions {
            let v = f64::from_bits(x.values.get(c).copied().unwrap_or(0));
            if v.is_nan() {
                return Ok(None);
            }
            least = least.min(v);
        }
        let mut ov = self.overlay.lock().unwrap_or_else(|p| p.into_inner());
        let Some(appended) = ov.least_value(c)? else { return Ok(None) };
        Ok(Some(least.min(appended)))
    }

    /// The time bounds a read over label indices `[lo, hi)` of the base
    /// means for the overlay's rows: the filter's, narrowed to the labels'
    /// span when the indices name a span inside the published labels.
    fn time_bounds(&self, f: &RowFilter, lo: u32, hi: u32) -> (Option<i64>, Option<i64>) {
        let labels = &self.levels[0].presence.labels;
        let mut t0 = f.t0;
        let mut t1 = f.t1;
        if lo > 0 && (lo as usize) < labels.len() {
            t0 = Some(t0.map_or(labels[lo as usize], |t| t.max(labels[lo as usize])));
        }
        if (hi as usize) < labels.len() {
            // Up to the label before `hi`: strictly before the label at `hi`.
            let bound = labels[hi as usize] - 1;
            t1 = Some(t1.map_or(bound, |t| t.min(bound)));
        }
        (t0, t1)
    }

    /// The greatest sequence number the version holds, appends included.
    pub fn seq_high(&self) -> u64 {
        let ov = self.overlay.lock().unwrap_or_else(|p| p.into_inner());
        self.manifest.seq_high.max(ov.seq_high)
    }

    /// A superseded row's values from its stored bits.
    fn revision_values(&self, x: &Revision) -> (Vec<i64>, Vec<f64>) {
        let n = self.value_columns();
        let mut ints = Vec::with_capacity(n);
        let mut floats = Vec::with_capacity(n);
        for (c, col) in self.manifest.columns.iter().take(n).enumerate() {
            let bits = x.values.get(c).copied().unwrap_or(0);
            match col.kind {
                Kind::Tick | Kind::TickF32 => {
                    let v = f64::from_bits(bits);
                    floats.push(v);
                    ints.push(abt_codec::tick::scaled_or_zero(v, col.exponent));
                }
                Kind::Int | Kind::Residual => {
                    ints.push(bits as i64);
                    floats.push(bits as i64 as f64);
                }
            }
        }
        (ints, floats)
    }

    /// The latest admitted revisions in the newest bucket at or before
    /// `t1` that holds a row the filter admits (the store's `read_newest`,
    /// E-14): the buckets from the last backwards, each read with the
    /// filter, the first with rows answers.
    pub fn read_newest(&self, f: &RowFilter) -> Result<Rows, DataError> {
        // The buckets, published and opened by appends, newest first; each
        // read over its time span (the appended rows come with it).
        let calendar = self.manifest.schema.calendar.clone();
        let mut buckets: Vec<(String, i64)> = self.manifest.buckets.iter().map(|b| (b.bucket.clone(), b.t_min)).collect();
        {
            let ov = self.overlay.lock().unwrap_or_else(|p| p.into_inner());
            for (name, st) in &ov.buckets {
                if !buckets.iter().any(|(n, _)| n == name) {
                    buckets.push((name.clone(), st.t_min));
                }
            }
        }
        buckets.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, t) in &buckets {
            let (_, from, to) = crate::store::bucket_span(calendar.as_deref(), *t);
            if f.t1.is_some_and(|t1| from > t1) {
                continue;
            }
            let latest = RowFilter {
                latest_only: true,
                t0: Some(f.t0.map_or(from, |t0| t0.max(from))),
                t1: Some(f.t1.map_or(to - 1, |t1| t1.min(to - 1))),
                ..f.clone()
            };
            let rows = self.read(&latest, 0, u32::MAX)?;
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
        Ok(Rows::with_columns(self.value_columns()))
    }

    /// The schema of a ladder level as a relation of its own
    /// (`<relation>@<level>`): the instrument, the bar's label, the
    /// value (the base column's type), the base rows it summarises, and
    /// the receipt.
    pub fn level_schema(&self, level: usize) -> RelationSchema {
        let s = &self.manifest.schema;
        let lay = crate::rows::layout(s);
        let lv = &self.levels[level];
        let value = s
            .columns
            .iter()
            .enumerate()
            .find(|(i, _)| *i != lay.t && Some(*i) != lay.r && !lay.identity.contains(i))
            .map(|(_, c)| c.clone())
            .expect("a bar relation has a value column");
        let col = |name: &str, ty: ColumnType, role: Role| ColumnSpec {
            name: name.to_string(),
            ty,
            role,
            dimension: None,
        };
        RelationSchema {
            name: format!("{}@{}", s.name, lv.calendar),
            class: s.class.clone(),
            columns: vec![
                s.columns[lay.identity[0]].clone(),
                col(&s.columns[lay.t].name, ColumnType::Time, Role::EventTime),
                value,
                col("count", ColumnType::Count, Role::Value),
                col("r", ColumnType::Time, Role::ReceiptTime),
            ],
            calendar: Some(lv.calendar.clone()),
            constraints: None,
            attributes: None,
            schema_version: s.schema_version.clone(),
        }
    }

    /// The rows as a batch in the stored layout of the base; tick columns
    /// as `Decimal128(18, scale)` when `ticks` names a scale, else
    /// `Float64`.
    pub fn to_batch(&self, rows: &Rows, stored: &SchemaRef, lay: &Layout, ticks: Option<u32>) -> Result<RecordBatch, DataError> {
        self.to_batch_at(0, &self.manifest.schema.clone(), rows, stored, lay, ticks)
    }

    /// The rows of a level as a batch in a stored layout (`schema` the
    /// level's, `level_schema`, for a ladder level).
    pub fn to_batch_at(&self, level: usize, s: &RelationSchema, rows: &Rows, stored: &SchemaRef, lay: &Layout, ticks: Option<u32>) -> Result<RecordBatch, DataError> {
        let entries = &self.levels[level].entries;
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(s.columns.len() + 1);
        let mut vc = 0usize;
        for (i, c) in s.columns.iter().enumerate() {
            let col: ArrayRef = if i == lay.t {
                Arc::new(TimestampNanosecondArray::from(rows.t.clone()).with_timezone("UTC"))
            } else if Some(i) == lay.r {
                Arc::new(TimestampNanosecondArray::from(rows.r.clone()).with_timezone("UTC"))
            } else if lay.identity.contains(&i) {
                Arc::new(UInt64Array::from(rows.id.clone()))
            } else {
                let (ints, floats) = (&rows.ints[vc], &rows.floats[vc]);
                let entry = &entries[vc];
                vc += 1;
                match c.ty {
                    ColumnType::Num => match ticks {
                        Some(scale) => {
                            let up = abt_agg::exact::pow10(scale.saturating_sub(entry.exponent as u32)) as i128;
                            Arc::new(Decimal128Array::from(ints.iter().map(|&v| v as i128 * up).collect::<Vec<_>>()).with_precision_and_scale(18, scale as i8)?)
                        }
                        None => Arc::new(Float64Array::from(floats.clone())),
                    },
                    ColumnType::Count => Arc::new(Int64Array::from(ints.clone())),
                    ColumnType::Label => Arc::new(UInt32Array::from(ints.iter().map(|&v| v as u32).collect::<Vec<_>>())),
                    ColumnType::Bool => Arc::new(BooleanArray::from(ints.iter().map(|&v| v != 0).collect::<Vec<_>>())),
                    ColumnType::Time => Arc::new(TimestampNanosecondArray::from(ints.clone()).with_timezone("UTC")),
                    ColumnType::Entity => Arc::new(UInt64Array::from(ints.iter().map(|&v| v as u64).collect::<Vec<_>>())),
                    ColumnType::Tick => Arc::new(Decimal128Array::from(ints.iter().map(|&v| v as i128).collect::<Vec<_>>()).with_precision_and_scale(18, entry.exponent as i8)?),
                    ColumnType::Bytes => return Err(DataError::Schema(format!("`{}`: a bytes column is never hot", s.name))),
                }
            };
            cols.push(col);
        }
        cols.push(Arc::new(UInt64Array::from(rows.seq.clone())));
        // The stored schema's fields, with a tick column's type where asked.
        if ticks.is_some() {
            let fields: Vec<arrow::datatypes::Field> = stored
                .fields()
                .iter()
                .zip(&cols)
                .map(|(f, c)| arrow::datatypes::Field::new(f.name(), c.data_type().clone(), f.is_nullable()))
                .collect();
            return Ok(RecordBatch::try_new(Arc::new(arrow::datatypes::Schema::new(fields)), cols)?);
        }
        Ok(RecordBatch::try_new(stored.clone(), cols)?)
    }
}

/// An instrument's column as the range rules read it.
pub struct ColumnView<'a> {
    v: &'a HotVersion,
    level: usize,
    c: usize,
    inst: usize,
}

impl ColumnView<'_> {
    fn loc(&self, b: usize) -> Loc {
        self.v.levels[self.level].columns[self.c].blocks[self.inst][b]
    }

    pub fn rows(&self) -> u64 {
        self.v.levels[self.level].presence.instruments[self.inst].rows
    }

    /// The number of blocks (the trait's `blocks`, reachable without it).
    pub fn blocks_len(&self) -> usize {
        self.v.levels[self.level].columns[self.c].blocks[self.inst].len()
    }

    /// Whether any block holds a value off its grid (no exact view).
    pub fn raw(&self) -> bool {
        (0..self.blocks()).any(|b| {
            let l = self.loc(b);
            let seg = &self.v.segments[l.seg as usize];
            seg.segment().header(&seg.runs[l.run as usize], l.block as usize).raw()
        })
    }
}

impl Column for ColumnView<'_> {
    fn block(&self, b: usize) -> Vec<i64> {
        self.v.block_at(self.level, self.c, self.inst, b).ints.clone()
    }
    fn len(&self, b: usize) -> usize {
        self.loc(b).n as usize
    }
    fn blocks(&self) -> usize {
        self.blocks_len()
    }
    fn cumulative(&self, boundary: usize) -> Cumulative {
        let table = &self.v.levels[self.level].columns[self.c].blocks[self.inst];
        if table.is_empty() {
            return Cumulative::ZERO;
        }
        let l = if boundary < table.len() { table[boundary] } else { table[table.len() - 1] };
        let seg = &self.v.segments[l.seg as usize];
        let run = &seg.runs[l.run as usize];
        let within = if boundary < table.len() { boundary - run.first_block as usize } else { run.blocks as usize };
        seg.segment().cumulative(run, within)
    }
    fn summary(&self, b: usize) -> BlockSummary {
        let l = self.loc(b);
        let seg = &self.v.segments[l.seg as usize];
        seg.segment().summary(&seg.runs[l.run as usize], l.block as usize)
    }
    fn sparse(&self) -> &SparseTable {
        &self.v.levels[self.level].columns[self.c].sparse[self.inst]
    }
}

/// Whether rows can be held hot: at least one, and no null in a value
/// column (a block holds every value).
pub fn qualifies_rows(all: &RecordBatch, lay: &Layout) -> bool {
    use arrow::array::Array;
    all.num_rows() > 0
        && (0..all.num_columns() - 1)
            .filter(|&i| i != lay.t && Some(i) != lay.r && !lay.identity.contains(&i))
            .all(|i| all.column(i).null_count() == 0)
}

/// Whether a relation's rows can be held hot: one entity column that is
/// the whole key, an event time, and value columns of a kind the blocks
/// hold (no bytes). Nulls in value columns are checked on the rows.
pub fn qualifies(s: &RelationSchema, lay: &Layout) -> bool {
    lay.identity.len() == 1
        && lay.key.len() == 1
        && s.columns[lay.identity[0]].ty == ColumnType::Entity
        && s.columns
            .iter()
            .enumerate()
            .all(|(i, c)| i == lay.t || Some(i) == lay.r || i == lay.identity[0] || !matches!(c.ty, ColumnType::Bytes | ColumnType::Entity))
}

impl Instrument {
    /// The instrument's rows with labels in `[lo, hi)` as `(row, label
    /// index)` pairs.
    pub fn rows_with_labels(&self, lo: u32, hi: u32) -> Vec<(u64, u32)> {
        let (a, _) = self.rows_in(lo, hi.saturating_sub(1));
        if hi == 0 || hi <= self.first || lo > self.last {
            return Vec::new();
        }
        let i0 = (lo.max(self.first) - self.first) as usize;
        let i1 = (hi.min(self.last + 1) - self.first) as usize;
        ones(self.presence.words(), i0, i1).enumerate().map(|(k, bit)| (a + k as u64, self.first + bit as u32)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stable sort of the rows' indices by (t, id, r, seq): what
    /// `canonical_order` stands in for.
    fn by_sort(rows: &Rows, keys: &[(u32, u32, i64)]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by_key(|&i| (rows.t[i], rows.id[i], keys[i].2, rows.seq[i]));
        order
    }

    #[test]
    fn the_counting_order_is_the_stable_sort_by_time_identity_receipt_and_sequence() {
        let mut state = 11u64;
        let mut below = |n: u64| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) % n
        };
        for round in 0..200 {
            // Instruments whose ids are not in the ordinals' order, each
            // with a run of rows in time order (some times shared, some
            // rows of one time revised, some repeated whole), then a second
            // run of some of them before or inside the first, as appended
            // rows are gathered apart from the published ones.
            let instruments = 1 + below(9) as usize;
            let ids: Vec<u64> = (0..instruments).map(|o| 1_000 - 37 * o as u64 + below(30)).collect();
            let mut rows = Rows::with_columns(0);
            let mut keys: Vec<(u32, u32, i64)> = Vec::new();
            for pass in 0..2 {
                for (o, &id) in ids.iter().enumerate() {
                    if pass == 1 && below(2) == 0 {
                        continue;
                    }
                    let mut t = below(40) as i64;
                    for _ in 0..below(60) {
                        t += below(3) as i64;
                        let copies = if round % 3 == 0 { 1 } else { 1 + below(3) };
                        for _ in 0..copies {
                            let r = t + below(4) as i64;
                            rows.push(id, t, r, below(5), &[], &[]);
                            keys.push((u32::MAX, o as u32, r));
                        }
                    }
                }
            }
            assert_eq!(canonical_order(&rows, &keys), by_sort(&rows, &keys), "round {}", round);
        }
        // No row, one row, and ordinals that do not stand for one id each
        // (the sort answers).
        let none = Rows::with_columns(0);
        assert_eq!(canonical_order(&none, &[]), Vec::<usize>::new());
        let mut rows = Rows::with_columns(0);
        for (id, t, seq) in [(7u64, 5i64, 1u64), (3, 5, 2), (7, 4, 3), (3, 5, 0)] {
            rows.push(id, t, t, seq, &[], &[]);
        }
        let two_ids_of_one_ordinal = [(0u32, 0u32, 5i64), (0, 0, 5), (0, 0, 4), (0, 0, 5)];
        assert_eq!(canonical_order(&rows, &two_ids_of_one_ordinal), vec![2, 3, 1, 0]);
        let two_ordinals_of_one_id = [(0u32, 0u32, 5i64), (0, 1, 5), (0, 2, 4), (0, 3, 5)];
        assert_eq!(canonical_order(&rows, &two_ordinals_of_one_id), vec![2, 3, 1, 0]);
        let sound = [(0u32, 1u32, 5i64), (0, 0, 5), (0, 1, 4), (0, 0, 5)];
        assert_eq!(canonical_order(&rows, &sound), vec![2, 3, 1, 0]);
    }
}
