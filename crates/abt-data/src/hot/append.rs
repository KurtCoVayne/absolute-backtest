//! Appends into a published version (ST-8; A-D39, A-D41): a row whose
//! time is after its instrument's last stored row at the base level joins
//! the instrument's tail once its log batch is durable (ST-8.2); a tail of
//! 1,024 rows is sealed into the current append file of each column
//! (value columns, the receipt residual, the row's time and its sequence
//! number), synced, then recorded in the seal journal and synced, and only
//! then do readers see the sealed block (ST-8.3). Readers at a sequence
//! see exactly the rows with sequence numbers up to theirs (Proposition
//! 8.1): every appended row carries its own.
//!
//! At open, the journal is replayed past the manifest (bytes of the append
//! files past its last record are truncated) and the tails are rebuilt
//! from the log (ST-8.8), so a restart answers what it answered before
//! (Proposition 8.2). Consolidation (ST-8.7) copies the journaled blocks
//! into base segments in instrument order, byte for byte, and starts the
//! journal over.
//!
//! The ladder's levels are built at staging only; their bars of appended
//! rows arrive with the next version (E-90).

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use abt_codec::block::{pack, patch, unpack, Header, Packed, HEADER_BYTES};
use abt_codec::summary::{BlockSummary, Cumulative};
use abt_codec::tick::{decode_value, encode, encode_integers, Encoded, Exception, Source};
use abt_codec::BLOCK;

use super::manifest::Kind;
use super::segment::{cumulative_to_bytes, int_to_le256, summary_to_bytes, SUMMARY_BYTES};
use crate::error::DataError;

/// Bytes an append file is rotated at.
pub const APPEND_FILE_BYTES: u64 = 64 << 20;

/// A row appended since the version's publication.
#[derive(Clone, Debug, PartialEq)]
pub struct TailRow {
    pub label: i64,
    pub r: i64,
    pub seq: u64,
    /// Per value column (the residual excluded): the integer at the
    /// instrument's exponent and the source's value.
    pub ints: Vec<i64>,
    pub floats: Vec<f64>,
}

/// Where a sealed block's bytes lie in an append file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendLoc {
    pub file: u32,
    pub offset: u64,
    pub len: u32,
}

/// A block sealed from a tail (ST-8.3): its rows' span, the location of
/// each column's block (the value columns, then `r`, `t`, `seq`), and the
/// value columns' summaries and totals after it.
#[derive(Clone, Debug)]
pub struct SealedRef {
    pub first_label: i64,
    pub last_label: i64,
    pub rows: u16,
    pub seq_max: u64,
    pub locs: Vec<AppendLoc>,
    pub summaries: Vec<BlockSummary>,
    pub after: Vec<Cumulative>,
}

/// An instrument's appended rows: its sealed blocks and its tail.
#[derive(Clone, Debug)]
pub struct InstrumentOverlay {
    pub id: u64,
    /// The rows the published version holds, which the overlay's follow.
    pub published_rows: u64,
    /// The published instrument's last label (none for a new instrument).
    pub published_last: Option<i64>,
    /// Per value column, the exponent of the overlay's integers (the
    /// published instrument's, or found at the first seal).
    pub exponents: Vec<u8>,
    pub sealed: Vec<SealedRef>,
    /// In label order, at most 1,023 rows.
    pub tail: Vec<TailRow>,
}

impl InstrumentOverlay {
    /// The last sealed label (the tail's span starts after it).
    pub fn last_sealed(&self) -> Option<i64> {
        self.sealed.last().map(|s| s.last_label).or(self.published_last)
    }

    pub fn rows(&self) -> u64 {
        self.sealed.iter().map(|s| s.rows as u64).sum::<u64>() + self.tail.len() as u64
    }
}

/// An append file of one column family.
pub struct AppendFile {
    pub column: usize,
    pub index: u32,
    pub path: PathBuf,
    pub len: u64,
    map: Option<Arc<memmap2::Mmap>>,
}

impl AppendFile {
    /// The file mapped at its current length (remapped after it grows).
    fn map(&mut self) -> Result<Arc<memmap2::Mmap>, DataError> {
        if let Some(m) = &self.map {
            if m.len() as u64 == self.len {
                return Ok(m.clone());
            }
        }
        if self.len == 0 {
            return Err(DataError::Schema("an empty append file".into()));
        }
        let f = fs::File::open(&self.path)?;
        // SAFETY: an append file only grows, and only this process writes
        // it; the bytes a mapping covers are never rewritten (ST-8.3).
        let m = unsafe { memmap2::MmapOptions::new().len(self.len as usize).map(&f)? };
        let m = Arc::new(m);
        self.map = Some(m.clone());
        Ok(m)
    }
}

/// The overlay of a version: what was appended since its publication.
pub struct Overlay {
    pub dir: PathBuf,
    /// The column families: the value columns' names, then `r`, `t`,
    /// `seq`.
    pub families: Vec<String>,
    pub kinds: Vec<Kind>,
    pub instruments: Vec<InstrumentOverlay>,
    pub by_id: HashMap<u64, usize>,
    pub files: Vec<AppendFile>,
    pub seq_high: u64,
    pub journal_records: u64,
    /// The partition calendar, for the buckets the appends open.
    pub calendar: Option<String>,
    /// Per bucket the appends touched: what its rows add to the published
    /// partition (or open beyond them).
    pub buckets: std::collections::BTreeMap<String, BucketStat>,
    /// The journal's generation: consolidation starts a new journal and
    /// append directory under the next one (the manifest's rename is the
    /// commit point; the old files are removed at the next open).
    pub generation: u64,
}

/// What the appended rows of one bucket add to its partition: every push
/// counts (a superseded row is a stored row, as the Parquet store counts
/// them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BucketStat {
    pub t_min: i64,
    pub t_max: i64,
    pub rows: u64,
    pub r_max: i64,
    pub seq_max: u64,
}

const JOURNAL_MAGIC: &[u8; 4] = b"ABTJ";

impl Overlay {
    /// An empty overlay over a version's directory, for `instruments`
    /// published (in ordinal order) with their last labels and exponents.
    pub fn new(dir: &Path, value_names: &[String], kinds: &[Kind], published: Vec<(u64, u64, Option<i64>, Vec<u8>)>, calendar: Option<String>, generation: u64) -> Overlay {
        let mut families: Vec<String> = value_names.to_vec();
        families.extend(["r", "t", "seq"].map(String::from));
        let instruments: Vec<InstrumentOverlay> = published
            .into_iter()
            .map(|(id, rows, last, exponents)| InstrumentOverlay {
                id,
                published_rows: rows,
                published_last: last,
                exponents,
                sealed: Vec::new(),
                tail: Vec::new(),
            })
            .collect();
        let by_id = instruments.iter().enumerate().map(|(i, x)| (x.id, i)).collect();
        Overlay {
            dir: dir.to_path_buf(),
            families,
            kinds: kinds.to_vec(),
            instruments,
            by_id,
            files: Vec::new(),
            seq_high: 0,
            journal_records: 0,
            calendar,
            buckets: std::collections::BTreeMap::new(),
            generation,
        }
    }

    /// Notes a row in its bucket.
    fn note_bucket(&mut self, label: i64, r: i64, seq: u64) {
        let (name, _, _) = crate::store::bucket_span(self.calendar.as_deref(), label);
        let e = self.buckets.entry(name).or_insert(BucketStat {
            t_min: label,
            t_max: label,
            rows: 0,
            r_max: r,
            seq_max: seq,
        });
        e.t_min = e.t_min.min(label);
        e.t_max = e.t_max.max(label);
        e.rows += 1;
        e.r_max = e.r_max.max(r);
        e.seq_max = e.seq_max.max(seq);
    }

    /// The journal file and the append directory of the generation.
    pub fn journal_file(dir: &Path, generation: u64) -> PathBuf {
        dir.join(format!("journal-{}.bin", generation))
    }

    pub fn append_dir(dir: &Path, generation: u64) -> PathBuf {
        dir.join(format!("append-{}", generation))
    }

    /// Removes the journals and append directories of earlier generations
    /// (folded by a consolidation whose manifest is in place, ST-8.7).
    pub fn remove_earlier_generations(dir: &Path, generation: u64) -> Result<(), DataError> {
        if !dir.exists() {
            return Ok(());
        }
        for e in fs::read_dir(dir)? {
            let p = e?.path();
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let gen = name
                .strip_prefix("journal-")
                .and_then(|r| r.strip_suffix(".bin"))
                .or_else(|| name.strip_prefix("append-"))
                .and_then(|g| g.parse::<u64>().ok());
            if gen.is_some_and(|g| g < generation) {
                if p.is_dir() {
                    fs::remove_dir_all(&p)?;
                } else {
                    fs::remove_file(&p)?;
                }
            }
        }
        Ok(())
    }

    /// The least sequence number a tail holds (what a restart replays the
    /// log from once the sealed blocks are folded, ST-8.8), if any.
    pub fn least_tail_seq(&self) -> Option<u64> {
        self.instruments.iter().flat_map(|i| i.tail.iter().map(|r| r.seq)).min()
    }

    /// Bytes of the append files.
    pub fn append_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.len).sum()
    }

    /// The rows of an instrument's tail visible at `seq` (every row when
    /// no sequence is given).
    pub fn tail_visible(&self, o: usize, seq: Option<u64>) -> u64 {
        self.instruments[o].tail.iter().filter(|r| seq.is_none_or(|s| r.seq <= s)).count() as u64
    }

    /// Raises an instrument's exponent of a column to `e` (a finer
    /// quantum arrived): its totals and tail integers rescale exactly.
    pub fn raise(&mut self, o: usize, c: usize, e: u8) {
        let inst = &mut self.instruments[o];
        if e <= inst.exponents[c] {
            return;
        }
        let up = abt_agg::exact::pow10((e - inst.exponents[c]) as u32) as i64;
        for s in &mut inst.sealed {
            s.summaries[c] = scaled_summary(&s.summaries[c], up);
            s.after[c] = scaled_cumulative(&s.after[c], up);
        }
        for r in &mut inst.tail {
            r.ints[c] = r.ints[c].wrapping_mul(up);
        }
        inst.exponents[c] = e;
    }

    /// Whether anything was appended.
    pub fn is_empty(&self) -> bool {
        self.instruments.iter().all(|i| i.sealed.is_empty() && i.tail.is_empty())
    }

    pub fn value_columns(&self) -> usize {
        self.families.len() - 3
    }

    fn journal_path(&self) -> PathBuf {
        Overlay::journal_file(&self.dir, self.generation)
    }

    /// The instrument's overlay, created for a new instrument.
    pub fn instrument(&mut self, id: u64, exponents: Vec<u8>) -> usize {
        if let Some(&o) = self.by_id.get(&id) {
            return o;
        }
        self.instruments.push(InstrumentOverlay {
            id,
            published_rows: 0,
            published_last: None,
            exponents,
            sealed: Vec::new(),
            tail: Vec::new(),
        });
        let o = self.instruments.len() - 1;
        self.by_id.insert(id, o);
        o
    }

    /// Whether a row at `label` is an append for the instrument: after its
    /// last sealed row and at a label its tail does not hold (a row inside
    /// the unsealed tail's span at a new label is inserted, ST-8.5). A row
    /// inside the sealed history, or at a label the tail holds already, is
    /// a revision: the presence holds one row per label, and a superseded
    /// row lives in the revision table a version build maintains (E-90).
    pub fn admits(&self, o: usize, label: i64) -> bool {
        let inst = &self.instruments[o];
        inst.last_sealed().is_none_or(|l| label > l) && inst.tail.binary_search_by_key(&label, |r| r.label).is_err()
    }

    /// A row into its instrument's tail (the log batch holding it being
    /// durable), in label order; a row at a label the tail holds replaces
    /// it (a replay of the log in sequence order keeps the latest).
    pub fn push(&mut self, o: usize, row: TailRow) {
        self.seq_high = self.seq_high.max(row.seq);
        self.note_bucket(row.label, row.r, row.seq);
        let tail = &mut self.instruments[o].tail;
        let at = tail.partition_point(|x| x.label < row.label);
        if at < tail.len() && tail[at].label == row.label {
            tail[at] = row;
        } else {
            tail.insert(at, row);
        }
    }

    /// Whether the instrument's tail is due for sealing.
    pub fn due(&self, o: usize) -> bool {
        self.instruments[o].tail.len() >= BLOCK
    }

    /// The current append file of a column family, rotated at the limit.
    fn file_for(&mut self, column: usize, bytes: u64) -> Result<usize, DataError> {
        let current = self.files.iter().rposition(|f| f.column == column);
        if let Some(i) = current {
            if self.files[i].len + bytes <= APPEND_FILE_BYTES {
                return Ok(i);
            }
        }
        let index = self.files.iter().filter(|f| f.column == column).count() as u32;
        let dir = Overlay::append_dir(&self.dir, self.generation);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}-{}.bin", self.families[column], index));
        fs::File::create(&path)?;
        self.files.push(AppendFile {
            column,
            index,
            path,
            len: 0,
            map: None,
        });
        Ok(self.files.len() - 1)
    }

    /// Seals the instrument's first 1,024 tail rows: a block per column
    /// family written and synced, then the journal record (ST-8.3). The
    /// site `seal-before-record` of the crash injection sits between.
    #[allow(clippy::needless_range_loop)]
    pub fn seal(&mut self, o: usize) -> Result<(), DataError> {
        let n = BLOCK.min(self.instruments[o].tail.len());
        if n == 0 {
            return Ok(());
        }
        let rows: Vec<TailRow> = self.instruments[o].tail[..n].to_vec();
        let ncols = self.value_columns();
        // Every column family's block.
        let mut encoded: Vec<Encoded> = Vec::with_capacity(ncols + 3);
        let mut exponents = self.instruments[o].exponents.clone();
        for c in 0..ncols {
            let enc = match self.kinds[c] {
                Kind::Tick | Kind::TickF32 => {
                    let v: Vec<f64> = rows.iter().map(|r| r.floats[c]).collect();
                    encode(&v, if self.kinds[c] == Kind::TickF32 { Source::F32 } else { Source::F64 })
                }
                Kind::Int | Kind::Residual => encode_integers(&rows.iter().map(|r| r.ints[c]).collect::<Vec<_>>()),
            };
            if enc.e > exponents[c] {
                // A finer quantum than the instrument's: its totals so far
                // are rescaled (exact: integers times a power of ten).
                self.raise(o, c, enc.e);
                exponents[c] = enc.e;
            }
            encoded.push(enc);
        }
        let rows: Vec<TailRow> = self.instruments[o].tail[..n].to_vec();
        encoded.push(encode_integers(&rows.iter().map(|r| r.r - r.label).collect::<Vec<_>>()));
        encoded.push(encode_integers(&rows.iter().map(|r| r.label).collect::<Vec<_>>()));
        encoded.push(encode_integers(&rows.iter().map(|r| r.seq as i64).collect::<Vec<_>>()));
        // The values at the instrument's exponents, for the summaries.
        let mut summaries = Vec::with_capacity(ncols);
        let mut after = Vec::with_capacity(ncols);
        let k0 = self.instruments[o].published_rows + self.instruments[o].sealed.iter().map(|s| s.rows as u64).sum::<u64>();
        for c in 0..ncols {
            let values: Vec<i64> = rows.iter().map(|r| r.ints[c]).collect();
            summaries.push(BlockSummary::of(&values));
            let before = self.instruments[o].sealed.last().map(|s| s.after[c].clone()).unwrap_or(Cumulative::ZERO);
            after.push(before.plus(&values, k0));
        }
        let mut locs = Vec::with_capacity(encoded.len());
        for (c, enc) in encoded.iter().enumerate() {
            let bytes = block_bytes(enc, summaries.get(c), after.get(c));
            let fi = self.file_for(c, bytes.len() as u64)?;
            let f = &mut self.files[fi];
            let mut out = fs::OpenOptions::new().append(true).open(&f.path)?;
            out.write_all(&bytes)?;
            out.sync_all()?;
            locs.push(AppendLoc {
                file: fi as u32,
                offset: f.len,
                len: bytes.len() as u32,
            });
            f.len += bytes.len() as u64;
        }
        crate::fault::point("seal-before-record");
        let sealed = SealedRef {
            first_label: rows[0].label,
            last_label: rows[n - 1].label,
            rows: n as u16,
            seq_max: rows.iter().map(|r| r.seq).max().unwrap_or(0),
            locs,
            summaries,
            after,
        };
        self.record(o, &sealed)?;
        crate::fault::point("seal-after-record");
        let inst = &mut self.instruments[o];
        inst.sealed.push(sealed);
        inst.tail.drain(..n);
        Ok(())
    }

    /// Appends a seal record to the journal and syncs it: the instrument
    /// (its ordinal and id), the exponents, the span, the rows, the
    /// greatest sequence and each column's location, framed by a length
    /// and a BLAKE3 tag.
    fn record(&mut self, o: usize, s: &SealedRef) -> Result<(), DataError> {
        let inst = &self.instruments[o];
        let mut payload = Vec::new();
        payload.extend_from_slice(&(o as u32).to_le_bytes());
        payload.extend_from_slice(&inst.id.to_le_bytes());
        payload.push(inst.exponents.len() as u8);
        payload.extend_from_slice(&inst.exponents);
        payload.extend_from_slice(&s.first_label.to_le_bytes());
        payload.extend_from_slice(&s.last_label.to_le_bytes());
        payload.extend_from_slice(&s.rows.to_le_bytes());
        payload.extend_from_slice(&s.seq_max.to_le_bytes());
        payload.extend_from_slice(&(s.locs.len() as u32).to_le_bytes());
        for l in &s.locs {
            payload.extend_from_slice(&l.file.to_le_bytes());
            payload.extend_from_slice(&self.files[l.file as usize].column.to_le_bytes());
            payload.extend_from_slice(&self.files[l.file as usize].index.to_le_bytes());
            payload.extend_from_slice(&l.offset.to_le_bytes());
            payload.extend_from_slice(&l.len.to_le_bytes());
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(self.journal_path())?;
        let mut frame = Vec::with_capacity(payload.len() + 40);
        frame.extend_from_slice(JOURNAL_MAGIC);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(blake3::hash(&payload).as_bytes());
        f.write_all(&frame)?;
        f.sync_all()?;
        self.journal_records += 1;
        Ok(())
    }

    /// Replays the journal (ST-8.8): every well-formed record whose bytes
    /// the append files hold joins the overlay; the journal and the files
    /// are truncated past the last such record.
    pub fn replay_journal(&mut self) -> Result<u64, DataError> {
        let path = self.journal_path();
        if !path.exists() {
            return Ok(0);
        }
        let mut bytes = Vec::new();
        fs::File::open(&path)?.read_to_end(&mut bytes)?;
        let mut at = 0usize;
        let mut good = 0usize;
        let mut records = 0u64;
        let mut file_ends: HashMap<usize, u64> = HashMap::new();
        while at + 8 <= bytes.len() && &bytes[at..at + 4] == JOURNAL_MAGIC {
            let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            let end = at + 8 + len + 32;
            if end > bytes.len() {
                break;
            }
            let payload = &bytes[at + 8..at + 8 + len];
            if blake3::hash(payload).as_bytes() != &bytes[at + 8 + len..end] {
                break;
            }
            match self.apply_record(payload, &mut file_ends) {
                Ok(()) => {
                    good = end;
                    records += 1;
                    at = end;
                }
                Err(_) => break,
            }
        }
        if good < bytes.len() {
            let f = fs::OpenOptions::new().write(true).open(&path)?;
            f.set_len(good as u64)?;
            f.sync_all()?;
        }
        // The append files end at their last journaled block.
        for (fi, end) in file_ends {
            let f = &mut self.files[fi];
            if f.path.exists() {
                let actual = fs::metadata(&f.path)?.len();
                if actual > end {
                    let h = fs::OpenOptions::new().write(true).open(&f.path)?;
                    h.set_len(end)?;
                    h.sync_all()?;
                }
                f.len = end;
            }
        }
        self.journal_records = records;
        Ok(records)
    }

    #[allow(clippy::needless_range_loop)]
    fn apply_record(&mut self, p: &[u8], file_ends: &mut HashMap<usize, u64>) -> Result<(), DataError> {
        let bad = || DataError::Schema("journal: a short record".into());
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8], DataError> {
            if at + n > p.len() {
                return Err(bad());
            }
            let s = &p[at..at + n];
            at += n;
            Ok(s)
        };
        let o = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let id = u64::from_le_bytes(take(8)?.try_into().unwrap());
        let ne = take(1)?[0] as usize;
        let exponents = take(ne)?.to_vec();
        let first_label = i64::from_le_bytes(take(8)?.try_into().unwrap());
        let last_label = i64::from_le_bytes(take(8)?.try_into().unwrap());
        let rows = u16::from_le_bytes(take(2)?.try_into().unwrap());
        let seq_max = u64::from_le_bytes(take(8)?.try_into().unwrap());
        let nl = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let mut locs = Vec::with_capacity(nl);
        for _ in 0..nl {
            let file = u32::from_le_bytes(take(4)?.try_into().unwrap());
            let column = usize::from_le_bytes(take(8)?.try_into().unwrap());
            let index = u32::from_le_bytes(take(4)?.try_into().unwrap());
            let offset = u64::from_le_bytes(take(8)?.try_into().unwrap());
            let len = u32::from_le_bytes(take(4)?.try_into().unwrap());
            // The file as the record names it, registered on first sight.
            while self.files.len() <= file as usize {
                let fi = self.files.len();
                let (c, ix) = if fi == file as usize { (column, index) } else { (0, 0) };
                self.files.push(AppendFile {
                    column: c,
                    index: ix,
                    path: Overlay::append_dir(&self.dir, self.generation).join(format!("{}-{}.bin", self.families.get(c).cloned().unwrap_or_default(), ix)),
                    len: 0,
                    map: None,
                });
            }
            let f = &mut self.files[file as usize];
            let actual = if f.path.exists() { fs::metadata(&f.path)?.len() } else { 0 };
            if offset + len as u64 > actual {
                return Err(DataError::Schema("journal: a record past its file".into()));
            }
            // The file's journaled extent so far (what a map covers).
            f.len = f.len.max(offset + len as u64);
            locs.push(AppendLoc { file, offset, len });
            let e = file_ends.entry(file as usize).or_insert(0);
            *e = (*e).max(offset + len as u64);
        }
        // The instrument, new or known; exponents as recorded.
        let o2 = self.instrument(id, exponents.clone());
        if o2 != o {
            return Err(DataError::Schema(format!("journal: instrument {} at ordinal {} was {}", id, o, o2)));
        }
        let ncols = self.value_columns();
        let inst = &mut self.instruments[o];
        if exponents.len() == ncols && exponents != inst.exponents {
            for c in 0..ncols {
                if exponents[c] > inst.exponents[c] {
                    let up = abt_agg::exact::pow10((exponents[c] - inst.exponents[c]) as u32) as i64;
                    for s in &mut inst.sealed {
                        s.summaries[c] = scaled_summary(&s.summaries[c], up);
                        s.after[c] = scaled_cumulative(&s.after[c], up);
                    }
                }
            }
            inst.exponents = exponents;
        }
        // The block's summaries by decoding it (a sequential read).
        let k0 = inst.published_rows + inst.sealed.iter().map(|s| s.rows as u64).sum::<u64>();
        let mut summaries = Vec::with_capacity(ncols);
        let mut after = Vec::with_capacity(ncols);
        let before: Vec<Cumulative> = (0..ncols).map(|c| inst.sealed.last().map(|s| s.after[c].clone()).unwrap_or(Cumulative::ZERO)).collect();
        let exps = inst.exponents.clone();
        for c in 0..ncols {
            let (ints, _) = self.decode_at(locs[c], exps[c], self.kinds[c])?;
            summaries.push(BlockSummary::of(&ints));
            after.push(before[c].plus(&ints, k0));
        }
        // The rows' buckets, from the block's labels, residuals and
        // sequence numbers.
        let (res, _) = self.decode_at(locs[ncols], 0, Kind::Residual)?;
        let (labels, _) = self.decode_at(locs[ncols + 1], 0, Kind::Int)?;
        let (seqs, _) = self.decode_at(locs[ncols + 2], 0, Kind::Int)?;
        for k in 0..rows as usize {
            self.note_bucket(labels[k], labels[k] + res[k], seqs[k] as u64);
        }
        let inst = &mut self.instruments[o];
        inst.sealed.push(SealedRef {
            first_label,
            last_label,
            rows,
            seq_max,
            locs,
            summaries,
            after,
        });
        self.seq_high = self.seq_high.max(seq_max);
        Ok(())
    }

    /// The bytes of a sealed block in its append file.
    pub fn bytes_at(&mut self, loc: AppendLoc) -> Result<Vec<u8>, DataError> {
        let map = self.files[loc.file as usize].map()?;
        Ok(map[loc.offset as usize..(loc.offset + loc.len as u64) as usize].to_vec())
    }

    /// A sealed block decoded from its append file: integers at
    /// `exponent` and the source's values.
    pub fn decode_at(&mut self, loc: AppendLoc, exponent: u8, kind: Kind) -> Result<(Vec<i64>, Vec<f64>), DataError> {
        let map = self.files[loc.file as usize].map()?;
        let bytes = &map[loc.offset as usize..(loc.offset + loc.len as u64) as usize];
        Ok(decode_block(bytes, exponent, kind))
    }

    /// The appended rows of an instrument with labels in `[t0, t1]`, in
    /// label order: the sealed blocks' and the tail's.
    #[allow(clippy::needless_range_loop)]
    pub fn rows_of(&mut self, o: usize, t0: Option<i64>, t1: Option<i64>) -> Result<Vec<TailRow>, DataError> {
        let ncols = self.value_columns();
        let sealed = self.instruments[o].sealed.clone();
        let exps = self.instruments[o].exponents.clone();
        let mut out = Vec::new();
        for s in &sealed {
            if t0.is_some_and(|a| s.last_label < a) || t1.is_some_and(|b| s.first_label > b) {
                continue;
            }
            let mut cols: Vec<(Vec<i64>, Vec<f64>)> = Vec::with_capacity(ncols);
            for c in 0..ncols {
                cols.push(self.decode_at(s.locs[c], exps[c], self.kinds[c])?);
            }
            let (res, _) = self.decode_at(s.locs[ncols], 0, Kind::Residual)?;
            let (labels, _) = self.decode_at(s.locs[ncols + 1], 0, Kind::Int)?;
            let (seqs, _) = self.decode_at(s.locs[ncols + 2], 0, Kind::Int)?;
            for k in 0..s.rows as usize {
                let label = labels[k];
                if t0.is_some_and(|a| label < a) || t1.is_some_and(|b| label > b) {
                    continue;
                }
                out.push(TailRow {
                    label,
                    r: label + res[k],
                    seq: seqs[k] as u64,
                    ints: cols.iter().map(|c| c.0[k]).collect(),
                    floats: cols.iter().map(|c| c.1[k]).collect(),
                });
            }
        }
        for r in &self.instruments[o].tail {
            if t0.is_none_or(|a| r.label >= a) && t1.is_none_or(|b| r.label <= b) {
                out.push(r.clone());
            }
        }
        Ok(out)
    }

    /// The labels the overlay added beyond the published ones, sorted.
    pub fn labels(&self) -> Vec<i64> {
        let mut out: Vec<i64> = Vec::new();
        for i in &self.instruments {
            for s in &i.sealed {
                out.push(s.first_label);
                out.push(s.last_label);
            }
            out.extend(i.tail.iter().map(|r| r.label));
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn scaled_summary(s: &BlockSummary, up: i64) -> BlockSummary {
    BlockSummary {
        min: s.min.wrapping_mul(up),
        max: s.max.wrapping_mul(up),
        first: s.first.wrapping_mul(up),
        last: s.last.wrapping_mul(up),
        ..*s
    }
}

fn scaled_cumulative(c: &Cumulative, up: i64) -> Cumulative {
    Cumulative {
        count: c.count,
        sum: c.sum * up as i128,
        sum_sq: c.sum_sq.mul_u128((up as u128) * (up as u128)),
        sum_kx: c.sum_kx.mul_u128(up as u128),
    }
}

/// A sealed block's bytes: the 32-byte header, the words (64-byte
/// aligned), the exception count and list, then for a value column its
/// 48-byte summary and 88-byte totals after it.
fn block_bytes(enc: &Encoded, summary: Option<&BlockSummary>, after: Option<&Cumulative>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut h = Header::of(enc, false);
    h.exceptions = if enc.exceptions.is_empty() { 0 } else { 1 };
    out.extend_from_slice(&h.to_bytes());
    while !out.len().is_multiple_of(64) {
        out.push(0);
    }
    out.extend_from_slice(&pack(&enc.offsets, enc.width).to_bytes());
    while !out.len().is_multiple_of(64) {
        out.push(0);
    }
    out.extend_from_slice(&(enc.exceptions.len() as u32).to_le_bytes());
    for x in &enc.exceptions {
        let (kind, value) = match *x {
            Exception::Offset { offset, .. } => (0u8, offset),
            Exception::Raw { bits, .. } => (1u8, bits),
        };
        out.extend_from_slice(&x.position().to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&[0u8; 5]);
        out.extend_from_slice(&value.to_le_bytes());
    }
    while !out.len().is_multiple_of(64) {
        out.push(0);
    }
    if let (Some(s), Some(a)) = (summary, after) {
        out.extend_from_slice(&summary_to_bytes(s));
        out.extend_from_slice(&cumulative_to_bytes(a));
        let _ = int_to_le256;
        let _ = SUMMARY_BYTES;
    }
    out
}

/// A block's bytes as a seal writes them, without its summary (what the
/// hash of a copy is checked against).
pub fn bytes_of(enc: &Encoded) -> Vec<u8> {
    block_bytes(enc, None, None)
}

/// A sealed block's header, packed words and exceptions, as its bytes
/// hold them.
pub fn parse_block(bytes: &[u8]) -> (Header, Packed, Vec<Exception>) {
    let h = Header::from_bytes(&bytes[..HEADER_BYTES]);
    let words_at = 64;
    let words = &bytes[words_at..words_at + h.words_bytes()];
    let packed = Packed::from_bytes(h.width, words);
    let mut at = (words_at + h.words_bytes()).div_ceil(64) * 64;
    let n = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    at += 4;
    let mut exceptions = Vec::with_capacity(n);
    for _ in 0..n {
        let position = u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        let kind_b = bytes[at + 2];
        let value = u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap());
        exceptions.push(match kind_b {
            0 => Exception::Offset { position, offset: value },
            _ => Exception::Raw { position, bits: value },
        });
        at += 16;
    }
    (h, packed, exceptions)
}

/// A sealed block as its encoder produced it (for the byte-for-byte copy
/// of consolidation, ST-8.7).
pub fn encoded_of(bytes: &[u8]) -> Encoded {
    let (h, packed, exceptions) = parse_block(bytes);
    let mut out = [0u64; BLOCK];
    unpack(&packed, &mut out);
    Encoded {
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

/// The hash of a sealed block as Proposition 7.1 defines it: BLAKE3 over
/// its header, packed words and exception list (what a base segment's
/// `block_hash` covers).
pub fn block_hash_of(bytes: &[u8]) -> blake3::Hash {
    let h = Header::from_bytes(&bytes[..HEADER_BYTES]);
    let mut hasher = blake3::Hasher::new();
    let mut header = [0u8; HEADER_BYTES];
    header.copy_from_slice(&bytes[..HEADER_BYTES]);
    header[22..26].fill(0);
    hasher.update(&header);
    hasher.update(&bytes[64..64 + h.words_bytes()]);
    let at = (64 + h.words_bytes()).div_ceil(64) * 64;
    let n = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    if n != 0 {
        hasher.update(&bytes[at..at + 4 + n * 16]);
    }
    hasher.finalize()
}

/// A sealed block decoded: integers at `exponent` and the source's values.
pub fn decode_block(bytes: &[u8], exponent: u8, kind: Kind) -> (Vec<i64>, Vec<f64>) {
    let (h, packed, exceptions) = parse_block(bytes);
    let mut out = [0u64; BLOCK];
    unpack(&packed, &mut out);
    patch(&exceptions, 0, BLOCK, &mut out);
    let rows = h.n as usize;
    let scale = abt_agg::exact::pow10((exponent as i32 - h.e as i32).max(0) as u32) as i64;
    let ints: Vec<i64> = (0..rows).map(|j| h.base.wrapping_add(h.g.wrapping_mul(out[j]) as i64).wrapping_mul(scale)).collect();
    let floats: Vec<f64> = match kind {
        Kind::Tick | Kind::TickF32 => {
            let mut raw = exceptions
                .iter()
                .filter_map(|x| if let Exception::Raw { position, bits } = x { Some((*position as usize, *bits)) } else { None })
                .peekable();
            (0..rows)
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
    (ints, floats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_block_round_trips_through_its_bytes() {
        let values: Vec<f64> = (0..1024).map(|i| (1000 + 5 * (i % 97)) as f64 / 100.0).collect();
        let enc = encode(&values, Source::F64);
        let ints = enc.integers().unwrap();
        let s = BlockSummary::of(&ints);
        let a = Cumulative::ZERO.plus(&ints, 0);
        let bytes = block_bytes(&enc, Some(&s), Some(&a));
        let (back_ints, back) = decode_block(&bytes, enc.e, Kind::Tick);
        assert_eq!(back, values);
        assert_eq!(back_ints, ints);
        let (scaled, _) = decode_block(&bytes, enc.e + 2, Kind::Tick);
        assert_eq!(scaled, ints.iter().map(|v| v * 100).collect::<Vec<_>>());
        let seqs: Vec<i64> = (0..700).map(|i| 5_000 + 13 * i).collect();
        let (back, _) = decode_block(&block_bytes(&encode_integers(&seqs), None, None), 0, Kind::Int);
        assert_eq!(back, seqs);
    }
}
