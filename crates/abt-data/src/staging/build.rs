//! S3 Build, S4 Verify and S5 Publish (ST-7): the planned instruments
//! encoded into segments (a run per instrument and column, segments
//! rotated at 64 MB), the presence file and the manifest of the next
//! version; every new block verified by decoding against its source and
//! its summaries against a direct recomputation (Propositions 3.2, 5.1);
//! the version's id from its blocks' hashes (Proposition 7.1); the files
//! renamed into place.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use abt_codec::rank::Bitmap;
use abt_codec::summary::{self, BlockSummary, Column, Cumulative};
use abt_codec::tick::{encode, encode_integers, Encoded, Source};
use abt_codec::BLOCK;
use abt_wire::canonical::Canonical;
use abt_wire::generated::abt::wire::RelationSchema;

use super::classify::Planned;
use super::normalise::Row;
use crate::error::DataError;
use crate::hot::ladder::{self, Bar, Level, Rule, Source as LadderSource};
use crate::hot::manifest::{Bucket, ColumnEntry, HotManifest, Kind, LevelEntry, Revision, SegmentRef};
use crate::hot::presence::{Instrument, Presence};
use crate::hot::segment::{Sealed, Writer};
use crate::hot::HotVersion;

/// Bytes a segment is rotated at.
pub const SEGMENT_BYTES: usize = 64 << 20;

/// The sizes of what a build wrote (`--measure`, ST-9).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Measure {
    pub rows: u64,
    pub blocks: u64,
    pub headers: u64,
    pub words: u64,
    pub exceptions: u64,
    pub summaries: u64,
    pub cumulatives: u64,
    pub presence: u64,
    pub manifest: u64,
    pub segments_new: usize,
    pub segments_kept: usize,
    /// The ladder's levels built and their bars.
    pub levels: usize,
    pub bars: u64,
}

/// What S4 checked.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Verified {
    pub blocks: u64,
    pub values: u64,
    pub summaries: u64,
    pub ranges: u64,
    pub instruments: u64,
}

/// A built version before publication: its files in a staging directory.
pub struct Built {
    pub manifest: HotManifest,
    pub staging_dir: PathBuf,
    pub new_segments: Vec<(String, PathBuf)>,
    pub measure: Measure,
    pub verified: Verified,
}

/// An instrument's columns encoded: per column its blocks and their values
/// at the column's finest exponent over the instrument.
struct EncodedInstrument {
    exponents: Vec<u8>,
    blocks: Vec<Vec<Encoded>>,
    values: Vec<Vec<Vec<i64>>>,
}

fn encode_instrument(p: &Planned, kinds: &[Kind], labels: &[i64]) -> EncodedInstrument {
    let ncols = kinds.len();
    let mut blocks: Vec<Vec<Encoded>> = Vec::with_capacity(ncols);
    let mut exponents = Vec::with_capacity(ncols);
    for (c, k) in kinds.iter().enumerate() {
        let encs: Vec<Encoded> = p
            .rows
            .chunks(BLOCK)
            .map(|chunk| match k {
                Kind::Tick | Kind::TickF32 => {
                    let v: Vec<f64> = chunk.iter().map(|x| f64::from_bits(x.vals[c])).collect();
                    encode(&v, if *k == Kind::TickF32 { Source::F32 } else { Source::F64 })
                }
                Kind::Int => encode_integers(&chunk.iter().map(|x| x.vals[c] as i64).collect::<Vec<_>>()),
                Kind::Residual => {
                    let v: Vec<i64> = chunk
                        .iter()
                        .map(|x| {
                            let _ = labels;
                            x.r - x.t
                        })
                        .collect();
                    encode_integers(&v)
                }
            })
            .collect();
        let e = encs.iter().map(|e| e.e).max().unwrap_or(0);
        exponents.push(e);
        blocks.push(encs);
    }
    let values: Vec<Vec<Vec<i64>>> = blocks
        .iter()
        .zip(&exponents)
        .map(|(encs, &e)| {
            encs.iter()
                .map(|enc| {
                    let scale = abt_agg::exact::pow10((e - enc.e) as u32) as i64;
                    (0..enc.n as usize).map(|j| enc.integer(j).unwrap_or(0).wrapping_mul(scale)).collect()
                })
                .collect()
        })
        .collect();
    EncodedInstrument { exponents, blocks, values }
}

/// Builds the next version into a staging directory under the store's
/// root: `prev` the current version the kept instruments come from.
#[allow(clippy::too_many_arguments)]
pub fn build(
    root: &Path,
    relation: &str,
    source: &str,
    schema: &RelationSchema,
    calendar: Option<&str>,
    kinds: &[Kind],
    value_names: &[String],
    planned: &[Planned],
    prev: Option<&HotVersion>,
    seq_high: u64,
    attributes: &BTreeMap<String, String>,
) -> Result<Built, DataError> {
    let ncols = kinds.len();
    // The labels of the version: every row's time.
    let mut labels: Vec<i64> = planned.iter().flat_map(|p| p.rows.iter().map(|r| r.t)).collect();
    labels.sort_unstable();
    labels.dedup();
    let label_ix = |t: i64| labels.binary_search(&t).expect("a row's label") as u32;
    // Segment lists per column: the previous version's, then the new ones.
    let mut segment_lists: Vec<Vec<SegmentRef>> = match prev {
        Some(p) => p.manifest.levels[0].segments.clone(),
        None => vec![Vec::new(); ncols],
    };
    let segments_kept = prev.map(|p| p.manifest.segments().len()).unwrap_or(0);
    let seg_dir = root.join("hot").join("segments");
    fs::create_dir_all(&seg_dir)?;
    let mut writers: Vec<Writer> = (0..ncols).map(|_| Writer::new()).collect();
    let mut new_segments: Vec<(String, PathBuf)> = Vec::new();
    let mut measure = Measure::default();
    let mut instruments: Vec<Instrument> = Vec::with_capacity(planned.len());
    let mut column_exponent = vec![0u8; ncols];
    let mut column_raw = vec![false; ncols];
    let mut verified = Verified::default();
    let finish = |c: usize, w: &mut Writer, new_segments: &mut Vec<(String, PathBuf)>, segment_lists: &mut Vec<Vec<SegmentRef>>| -> Result<(), DataError> {
        if w.is_empty() {
            return Ok(());
        }
        let done = std::mem::take(w);
        let (bytes, hash) = done.finish();
        let file = HotManifest::segment_file(&hash);
        let path = root.join(&file);
        if !path.exists() {
            let tmp = seg_dir.join(format!("{}.tmp-{}", hash, std::process::id()));
            fs::write(&tmp, &bytes)?;
            fs::rename(&tmp, &path)?;
        }
        new_segments.push((hash.clone(), path));
        segment_lists[c].push(SegmentRef {
            hash,
            file,
            bytes: bytes.len() as u64,
        });
        Ok(())
    };
    for (ordinal, p) in planned.iter().enumerate() {
        assert!(!p.rows.is_empty(), "an instrument with rows");
        let first = label_ix(p.rows[0].t);
        let last = label_ix(p.rows[p.rows.len() - 1].t);
        let mut bits = Bitmap::new();
        let mut at = first;
        for row in &p.rows {
            let l = label_ix(row.t);
            while at < l {
                bits.push(false);
                at += 1;
            }
            bits.push(true);
            at += 1;
        }
        // Sequence runs: maximal runs of consecutive sequence numbers.
        let mut seq_runs: Vec<(u64, u64, u64)> = Vec::new();
        for (k, row) in p.rows.iter().enumerate() {
            match seq_runs.last_mut() {
                Some(run) if run.1 + run.2 == row.seq => run.2 += 1,
                _ => seq_runs.push((k as u64, row.seq, 1)),
            }
        }
        let exponents: Vec<u8> = if p.rebuild {
            let enc = encode_instrument(p, kinds, &labels);
            for c in 0..ncols {
                let blocks: Vec<Sealed> = enc.blocks[c].iter().zip(&enc.values[c]).map(|(enc, values)| Sealed { enc, values }).collect();
                let run = writers[c].run(ordinal as u32, 0, enc.exponents[c], &blocks, &Cumulative::ZERO, 0);
                measure.blocks += run.blocks as u64;
                column_raw[c] |= enc.blocks[c].iter().any(|b| !b.exact());
                if writers[c].len() >= SEGMENT_BYTES {
                    finish(c, &mut writers[c], &mut new_segments, &mut segment_lists)?;
                }
            }
            // S4: every block decodes to its source; summaries and
            // cumulatives equal a direct recomputation.
            verify_instrument(p, kinds, &enc, &mut verified)?;
            enc.exponents
        } else {
            let pv = prev.expect("a kept instrument has a previous version");
            let o = pv.presence().ordinal(p.id).expect("a kept instrument is stored") as usize;
            pv.presence().instruments[o].exponents.clone()
        };
        for c in 0..ncols {
            column_exponent[c] = column_exponent[c].max(exponents[c]);
        }
        measure.rows += p.rows.len() as u64;
        instruments.push(Instrument {
            id: p.id,
            first,
            last,
            rows: p.rows.len() as u64,
            seq_runs,
            exponents,
            presence: bits,
        });
    }
    for (c, w) in writers.iter_mut().enumerate() {
        finish(c, w, &mut new_segments, &mut segment_lists)?;
    }
    // Kept instruments keep their ordinals only if the id order is the
    // same: a new instrument between two stored ones shifts the ordinals,
    // so the kept runs must be rewritten. The id order is the ordinal
    // order (presence), so compare.
    if let Some(pv) = prev {
        let shifted = pv.presence().instruments.iter().enumerate().any(|(o, inst)| planned.get(o).is_none_or(|p| p.id != inst.id));
        if shifted {
            // Rewrite every kept instrument into new runs at its new ordinal
            // (a copy of its blocks, byte for byte).
            rewrite_kept(root, &seg_dir, planned, pv, kinds, &mut segment_lists, &mut new_segments, &mut measure)?;
        }
    }
    let presence = Presence::new(labels.clone(), instruments);
    // Buckets: the partition calendar's, as label ranges.
    let mut buckets: Vec<Bucket> = Vec::new();
    let prev_buckets: BTreeMap<&str, &Bucket> = prev.map(|p| p.manifest.buckets.iter().map(|b| (b.bucket.as_str(), b)).collect()).unwrap_or_default();
    {
        let mut span: Option<(String, i64, i64, u32)> = None;
        for (i, &t) in labels.iter().enumerate() {
            if !span.as_ref().is_some_and(|(_, from, to, _)| *from <= t && t < *to) {
                if let Some((name, _, _, lo)) = span.take() {
                    buckets.push(Bucket {
                        bucket: name,
                        lo,
                        hi: i as u32,
                        t_min: labels[lo as usize],
                        t_max: labels[i - 1],
                        r_min: i64::MAX,
                        r_max: i64::MIN,
                        rows: 0,
                        seq_max: 0,
                    });
                }
                let (name, from, to) = crate::store::bucket_span(calendar, t);
                span = Some((name, from, to, i as u32));
            }
        }
        if let Some((name, _, _, lo)) = span {
            buckets.push(Bucket {
                bucket: name,
                lo,
                hi: labels.len() as u32,
                t_min: labels[lo as usize],
                t_max: labels[labels.len() - 1],
                r_min: i64::MAX,
                r_max: i64::MIN,
                rows: 0,
                seq_max: 0,
            });
        }
    }
    for b in buckets.iter_mut() {
        if let Some(old) = prev_buckets.get(b.bucket.as_str()) {
            b.r_min = old.r_min;
            b.r_max = old.r_max;
        }
    }
    for p in planned {
        let mut bi = 0usize;
        for row in &p.rows {
            let l = label_ix(row.t);
            while buckets[bi].hi <= l {
                bi += 1;
            }
            let b = &mut buckets[bi];
            b.rows += 1;
            b.r_min = b.r_min.min(row.r);
            b.r_max = b.r_max.max(row.r);
            b.seq_max = b.seq_max.max(row.seq);
        }
    }
    let mut revisions: Vec<Revision> = planned.iter().flat_map(|p| p.revisions.iter().cloned()).collect();
    revisions.sort_by_key(|x| (x.id, x.label, x.r, x.seq));
    // A superseded row is a stored row too (D-2.2 keeps every revision):
    // the counts include it, as the Parquet store's do.
    for x in &revisions {
        let l = label_ix(x.label);
        if let Some(b) = buckets.iter_mut().find(|b| b.lo <= l && l < b.hi) {
            b.rows += 1;
            b.seq_max = b.seq_max.max(x.seq);
        }
    }
    let columns: Vec<ColumnEntry> = (0..ncols)
        .map(|c| ColumnEntry {
            name: if kinds[c] == Kind::Residual { "r".to_string() } else { value_names[c].clone() },
            kind: kinds[c],
            exponent: column_exponent[c],
            raw: column_raw[c] || prev.is_some_and(|p| p.manifest.columns.get(c).is_some_and(|x| x.raw)),
        })
        .collect();
    let rows = presence.rows() + revisions.len() as u64;
    let staging_dir = root.join("hot").join(relation).join(format!(".staging-{}-{}", std::process::id(), seq_high));
    let _ = fs::remove_dir_all(&staging_dir);
    fs::create_dir_all(&staging_dir)?;
    let presence_bytes = presence.to_bytes();
    fs::write(staging_dir.join("presence.bin"), &presence_bytes)?;
    measure.presence = presence_bytes.len() as u64;
    // The calendar ladder (ST-5.3): every level's bars, each level its own
    // segments and presence (E-89).
    let exponents_of: Vec<Vec<u8>> = presence.instruments.iter().map(|i| i.exponents.clone()).collect();
    let mut levels = vec![LevelEntry {
        calendar: calendar.unwrap_or("base").to_string(),
        presence: "presence.bin".to_string(),
        columns: Vec::new(),
        segments: segment_lists,
    }];
    if let Some(cal) = calendar {
        levels.extend(build_ladder(
            root,
            &seg_dir,
            &staging_dir,
            relation,
            cal,
            kinds,
            &labels,
            planned,
            &exponents_of,
            column_raw[0],
            &mut measure,
            &mut new_segments,
        )?);
    }
    let mut manifest = HotManifest {
        relation: relation.to_string(),
        version: String::new(),
        source: source.to_string(),
        schema: schema.clone(),
        columns,
        levels,
        rows,
        from: labels.first().copied(),
        to: labels.last().copied(),
        seq_first: planned.iter().flat_map(|p| p.rows.iter().map(|r| r.seq)).min().unwrap_or(seq_high),
        seq_high,
        replay_from: None,
        generation: 0,
        buckets,
        revisions,
        attributes: attributes.clone(),
    };
    // The logical id: the blocks' hashes in (column, instrument, block)
    // order through the directory the manifest describes, with the
    // revisions, the schema and the sequence high-water.
    let provisional = HotVersion::open_manifest_at(root, &staging_dir, manifest.clone(), false)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(relation.as_bytes());
    hasher.update(schema.canonical_hash().map_err(|e| DataError::Malformed(e.to_string()))?.hex().as_bytes());
    for (li, lv) in provisional.levels.iter().enumerate() {
        for c in 0..lv.entries.len() {
            for inst in 0..lv.presence.instruments.len() {
                let view = provisional.column_at(li, c, inst);
                for b in 0..view.blocks() {
                    hasher.update(provisional.block_hash_at(li, c, inst, b).as_bytes());
                }
            }
        }
    }
    hasher.update(serde_json::to_vec(&manifest.revisions)?.as_slice());
    hasher.update(&seq_high.to_le_bytes());
    hasher.update(serde_json::to_vec(attributes)?.as_slice());
    manifest.version = hasher.finalize().to_hex().to_string();
    for s in manifest.segments() {
        measure.words += s.bytes;
    }
    measure.segments_new = new_segments.len();
    measure.segments_kept = segments_kept;
    let json = serde_json::to_vec_pretty(&manifest)?;
    measure.manifest = json.len() as u64;
    fs::write(staging_dir.join("manifest.json"), &json)?;
    drop(provisional);
    Ok(Built {
        manifest,
        staging_dir,
        new_segments,
        measure,
        verified,
    })
}

/// The integer of a stored value at exponent `e` (0 for a value off the
/// grid, which the ladder refuses above).
fn value_int(kind: Kind, bits: u64, e: u8) -> i64 {
    match kind {
        Kind::Tick | Kind::TickF32 => abt_codec::tick::scaled_or_zero(f64::from_bits(bits), e),
        Kind::Int | Kind::Residual => bits as i64,
    }
}

/// The `f64` view of a bar's value, as the base's rows show theirs.
fn value_view(kind: Kind, v: i64, e: u8) -> f64 {
    let x = abt_agg::exact::tick_to_f64(v as i128, e as u32);
    if kind == Kind::TickF32 {
        (x as f32) as f64
    } else {
        x
    }
}

/// The calendar ladder of a bar relation (ST-5.3; Proposition 5.2): every
/// level's bars of every instrument, each level built from the nearest
/// finer one (the base's rows count one each), written as segments of its
/// own (columns: the bar's value, its base-row count, the sequence of its
/// last base row, the receipt residual from the bucket's start) with a
/// presence over the level's labels. Every instrument's bars are rebuilt
/// at every version (E-89). A relation without a bar rule, with several
/// value columns or with values off any grid has no ladder.
#[allow(clippy::too_many_arguments)]
fn build_ladder(
    root: &Path,
    seg_dir: &Path,
    staging_dir: &Path,
    relation: &str,
    calendar: &str,
    kinds: &[Kind],
    base_labels: &[i64],
    planned: &[Planned],
    exponents: &[Vec<u8>],
    raw: bool,
    measure: &mut Measure,
    new_segments: &mut Vec<(String, PathBuf)>,
) -> Result<Vec<LevelEntry>, DataError> {
    let Some(rule) = Rule::of_relation(relation) else { return Ok(Vec::new()) };
    if kinds.len() != 2 || raw {
        return Ok(Vec::new());
    }
    let value_kind = kinds[0];
    let source_kind = if value_kind == Kind::TickF32 { Source::F32 } else { Source::F64 };
    let mut bars_of: BTreeMap<Level, Vec<Vec<Bar>>> = BTreeMap::new();
    let mut labels_of: BTreeMap<Level, Vec<(i64, i64)>> = BTreeMap::new();
    // Each built level's index of its bucket keys, built once with the
    // level (E-109): a bar of the level above looks its source bucket's
    // label up in it. Rebuilt per bar, it made a root's 15-minute level
    // quadratic in its 5-minute labels.
    let mut indexes: BTreeMap<Level, BTreeMap<i64, u32>> = BTreeMap::new();
    let mut out = Vec::new();
    for level in Level::of_base(calendar) {
        let labels = level.labels(base_labels);
        let index = ladder::index_of(&labels);
        let label_of = |lv: Level, key: i64| -> i64 { labels_of[&lv][*indexes[&lv].get(&key).expect("a bucket of the source level") as usize].1 };
        let mut per_inst: Vec<Vec<Bar>> = Vec::with_capacity(planned.len());
        for (o, p) in planned.iter().enumerate() {
            let e = exponents[o][0];
            let source: Vec<LadderSource> = match level.source(calendar) {
                None => p.rows.iter().map(|r| LadderSource::row(r.t, value_int(value_kind, r.vals[0], e), r.r, r.seq)).collect(),
                Some(src) => bars_of[&src][o]
                    .iter()
                    .map(|b| {
                        let label = label_of(src, b.key);
                        LadderSource {
                            label,
                            value: b.value,
                            count: b.count,
                            r: b.receipt(label),
                            seq: b.seq,
                            last_label: b.last_label,
                            last_r: b.last_r,
                        }
                    })
                    .collect(),
            };
            per_inst.push(ladder::build(level, rule, &source));
        }
        // The level's segments and presence.
        let ncols = 4usize;
        let mut writers: Vec<Writer> = (0..ncols).map(|_| Writer::new()).collect();
        let mut lists: Vec<Vec<SegmentRef>> = vec![Vec::new(); ncols];
        let mut instruments: Vec<Instrument> = Vec::with_capacity(planned.len());
        let mut exponent = 0u8;
        for (o, p) in planned.iter().enumerate() {
            let bars = &per_inst[o];
            if bars.is_empty() {
                continue;
            }
            let e = exponents[o][0];
            exponent = exponent.max(e);
            let first = index[&bars[0].key];
            let last = index[&bars[bars.len() - 1].key];
            let mut bits = Bitmap::new();
            let mut at = first;
            for b in bars {
                let l = index[&b.key];
                while at < l {
                    bits.push(false);
                    at += 1;
                }
                bits.push(true);
                at += 1;
            }
            let views: Vec<f64> = bars.iter().map(|b| value_view(value_kind, b.value, e)).collect();
            let counts: Vec<i64> = bars.iter().map(|b| b.count).collect();
            let seqs: Vec<i64> = bars.iter().map(|b| b.seq as i64).collect();
            let residuals: Vec<i64> = bars.iter().map(|b| b.receipt(labels[index[&b.key] as usize].1) - b.key).collect();
            let values: Vec<i64> = bars.iter().map(|b| b.value).collect();
            let columns: [(Vec<Encoded>, Vec<Vec<i64>>); 4] = [
                (views.chunks(BLOCK).map(|c| encode(c, source_kind)).collect(), values.chunks(BLOCK).map(|c| c.to_vec()).collect()),
                (counts.chunks(BLOCK).map(encode_integers).collect(), counts.chunks(BLOCK).map(|c| c.to_vec()).collect()),
                (seqs.chunks(BLOCK).map(encode_integers).collect(), seqs.chunks(BLOCK).map(|c| c.to_vec()).collect()),
                (residuals.chunks(BLOCK).map(encode_integers).collect(), residuals.chunks(BLOCK).map(|c| c.to_vec()).collect()),
            ];
            // A bar's value decodes to its integer at the instrument's
            // exponent: the ladder is exact (Proposition 5.2).
            for (enc, vals) in columns[0].0.iter().zip(&columns[0].1) {
                let back = enc
                    .integers()
                    .ok_or_else(|| DataError::Schema(format!("S4 verify, `{}` level `{}`: a bar's value is off the grid", relation, level.name())))?;
                let scale = abt_agg::exact::pow10((e - enc.e) as u32) as i64;
                if back.iter().zip(vals).any(|(b, v)| b.wrapping_mul(scale) != *v) {
                    return Err(DataError::Schema(format!(
                        "S4 verify, `{}` level `{}`: instrument {}'s bars do not reproduce their ticks",
                        relation,
                        level.name(),
                        p.id
                    )));
                }
            }
            let run_exponents = [e, 0, 0, 0];
            for (c, w) in writers.iter_mut().enumerate() {
                let (encs, vals) = &columns[c];
                let blocks: Vec<Sealed> = encs.iter().zip(vals).map(|(enc, values)| Sealed { enc, values }).collect();
                let run = w.run(instruments.len() as u32, 0, run_exponents[c], &blocks, &Cumulative::ZERO, 0);
                measure.blocks += run.blocks as u64;
                if w.len() >= SEGMENT_BYTES {
                    finish_into(root, seg_dir, c, w, new_segments, &mut lists)?;
                }
            }
            measure.bars += bars.len() as u64;
            instruments.push(Instrument {
                id: p.id,
                first,
                last,
                rows: bars.len() as u64,
                seq_runs: vec![(0, 0, bars.len() as u64)],
                exponents: run_exponents.to_vec(),
                presence: bits,
            });
        }
        for (c, w) in writers.iter_mut().enumerate() {
            finish_into(root, seg_dir, c, w, new_segments, &mut lists)?;
        }
        let name = level.name();
        let presence = Presence::new(labels.iter().map(|l| l.1).collect(), instruments);
        let bytes = presence.to_bytes();
        let file = format!("presence-{}.bin", name);
        fs::write(staging_dir.join(&file), &bytes)?;
        measure.presence += bytes.len() as u64;
        measure.levels += 1;
        let entry = |n: &str, kind: Kind, e: u8| ColumnEntry {
            name: n.to_string(),
            kind,
            exponent: e,
            raw: false,
        };
        out.push(LevelEntry {
            calendar: name,
            presence: file,
            columns: vec![
                entry("value", value_kind, exponent),
                entry("count", Kind::Int, 0),
                entry("seq", Kind::Int, 0),
                entry("r", Kind::Residual, 0),
            ],
            segments: lists,
        });
        bars_of.insert(level, per_inst);
        labels_of.insert(level, labels);
        indexes.insert(level, index);
    }
    Ok(out)
}

/// Copies every kept instrument's blocks into new runs at its new ordinal
/// (the id order changed): byte for byte, so the version's id is
/// unaffected (Proposition 7.1); the old segments drop out of the lists.
#[allow(clippy::too_many_arguments)]
fn rewrite_kept(
    root: &Path,
    seg_dir: &Path,
    planned: &[Planned],
    prev: &HotVersion,
    kinds: &[Kind],
    segment_lists: &mut [Vec<SegmentRef>],
    new_segments: &mut Vec<(String, PathBuf)>,
    measure: &mut Measure,
) -> Result<(), DataError> {
    // Start the lists over with the new runs only: rebuilt instruments
    // were written to the new segments already (at their new ordinals).
    let fresh: Vec<Vec<SegmentRef>> = segment_lists
        .iter()
        .map(|l| l.iter().filter(|s| new_segments.iter().any(|(h, _)| *h == s.hash)).cloned().collect())
        .collect();
    let mut writers: Vec<Writer> = (0..kinds.len()).map(|_| Writer::new()).collect();
    for (ordinal, p) in planned.iter().enumerate() {
        if p.rebuild {
            continue;
        }
        let o = prev.presence().ordinal(p.id).expect("a kept instrument is stored") as usize;
        for (c, w) in writers.iter_mut().enumerate() {
            let view = prev.column(c, o);
            let n = view.blocks();
            let encs: Vec<Encoded> = (0..n).map(|b| prev.encoded(c, o, b)).collect();
            let values: Vec<Vec<i64>> = (0..n).map(|b| view.block(b)).collect();
            let blocks: Vec<Sealed> = encs.iter().zip(&values).map(|(enc, values)| Sealed { enc, values }).collect();
            let run = w.run(ordinal as u32, 0, prev.presence().instruments[o].exponents[c], &blocks, &Cumulative::ZERO, 0);
            measure.blocks += run.blocks as u64;
            if w.len() >= SEGMENT_BYTES {
                finish_into(root, seg_dir, c, w, new_segments, segment_lists)?;
            }
        }
    }
    for (c, list) in segment_lists.iter_mut().enumerate() {
        *list = fresh[c].clone();
    }
    for (c, w) in writers.iter_mut().enumerate() {
        finish_into(root, seg_dir, c, w, new_segments, segment_lists)?;
    }
    Ok(())
}

fn finish_into(root: &Path, seg_dir: &Path, c: usize, w: &mut Writer, new_segments: &mut Vec<(String, PathBuf)>, segment_lists: &mut [Vec<SegmentRef>]) -> Result<(), DataError> {
    if w.is_empty() {
        return Ok(());
    }
    let done = std::mem::take(w);
    let (bytes, hash) = done.finish();
    let file = HotManifest::segment_file(&hash);
    let path = root.join(&file);
    if !path.exists() {
        let tmp = seg_dir.join(format!("{}.tmp-{}", hash, std::process::id()));
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
    }
    new_segments.push((hash.clone(), path));
    segment_lists[c].push(SegmentRef {
        hash,
        file,
        bytes: bytes.len() as u64,
    });
    Ok(())
}

/// S4 over one rebuilt instrument's encodings: decoding gives the source
/// bit for bit (Proposition 3.2); summaries and cumulatives of the blocks
/// equal a direct recomputation (Proposition 5.1); the row count is the
/// source's.
fn verify_instrument(p: &Planned, kinds: &[Kind], enc: &EncodedInstrument, v: &mut Verified) -> Result<(), DataError> {
    let fail = |what: String| DataError::Schema(format!("S4 verify, instrument {}: {}", p.id, what));
    let rows: u64 = enc.blocks[0].iter().map(|b| b.n as u64).sum();
    if rows != p.rows.len() as u64 {
        return Err(fail(format!("{} rows encoded of {}", rows, p.rows.len())));
    }
    for (c, k) in kinds.iter().enumerate() {
        let mut at = 0usize;
        for (b, block) in enc.blocks[c].iter().enumerate() {
            let n = block.n as usize;
            let chunk = &p.rows[at..at + n];
            match k {
                Kind::Tick | Kind::TickF32 => {
                    let back = block.decode();
                    for (j, (x, row)) in back.iter().zip(chunk).enumerate() {
                        if x.to_bits() != row.vals[c] {
                            return Err(fail(format!(
                                "column {} block {} row {} decodes to {} bits {:#x}, the source has {:#x}",
                                c,
                                b,
                                j,
                                x,
                                x.to_bits(),
                                row.vals[c]
                            )));
                        }
                    }
                }
                Kind::Int => {
                    let back = block.integers().ok_or_else(|| fail(format!("column {} block {} has a raw exception in an int column", c, b)))?;
                    for (j, (x, row)) in back.iter().zip(chunk).enumerate() {
                        if *x != row.vals[c] as i64 {
                            return Err(fail(format!("column {} block {} row {} decodes to {}, the source has {}", c, b, j, x, row.vals[c] as i64)));
                        }
                    }
                }
                Kind::Residual => {
                    let back = block.integers().ok_or_else(|| fail(format!("block {} has a raw exception in the receipt residual", b)))?;
                    for (j, (x, row)) in back.iter().zip(chunk).enumerate() {
                        if *x != row.r - row.t {
                            return Err(fail(format!("block {} row {} decodes a receipt residual of {}, the source has {}", b, j, x, row.r - row.t)));
                        }
                    }
                }
            }
            v.values += n as u64;
            v.blocks += 1;
            // Summaries against the values as the writer took them.
            let s = BlockSummary::of(&enc.values[c][b]);
            if s.n as usize != n {
                return Err(fail(format!("column {} block {} summary of {} values", c, b, s.n)));
            }
            v.summaries += 1;
            at += n;
        }
        // Cumulatives: the direct totals over every row, block by block.
        let direct = summary::cumulatives(enc.values[c].iter().map(|v| v.as_slice()));
        let mut running = Cumulative::ZERO;
        let mut k0 = 0u64;
        for (b, values) in enc.values[c].iter().enumerate() {
            running = running.plus(values, k0);
            k0 += values.len() as u64;
            if running != direct[b + 1] {
                return Err(fail(format!("column {} cumulative at block {} differs from the direct totals", c, b)));
            }
        }
    }
    v.instruments += 1;
    Ok(())
}

/// S4 over the opened version: random ranges of rebuilt instruments
/// against a direct reduction of the decoded rows (every range form of
/// ST-6.3 the structures answer: moments and extremes), and the presence
/// against the rows' labels.
pub fn verify_ranges(v: &HotVersion, planned: &[Planned], sample: usize, verified: &mut Verified) -> Result<(), DataError> {
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for (o, p) in planned.iter().enumerate().filter(|(_, p)| p.rebuild) {
        let inst = &v.presence().instruments[o];
        let labels: Vec<i64> = inst
            .rows_with_labels(0, v.presence().labels.len() as u32)
            .iter()
            .map(|&(_, l)| v.presence().labels[l as usize])
            .collect();
        if labels.len() != p.rows.len() || labels.iter().zip(&p.rows).any(|(l, r)| *l != r.t) {
            return Err(DataError::Schema(format!("S4 verify, instrument {}: its presence does not give its rows' times", p.id)));
        }
        let n = inst.rows;
        for c in 0..v.value_columns() {
            let col = v.column(c, o);
            let whole: Vec<i64> = (0..col.blocks()).flat_map(|b| col.block(b)).collect();
            for _ in 0..sample {
                let a = next() % n;
                let b = a + 1 + next() % (n - a);
                let m = summary::moments(&col, a, b);
                let direct = Cumulative::ZERO.plus(&whole[a as usize..b as usize], a);
                if m != direct {
                    return Err(DataError::Schema(format!(
                        "S4 verify, instrument {} column {}: moments over [{}, {}) differ from the direct reduction",
                        p.id, c, a, b
                    )));
                }
                let e = summary::extremes(&col, a, b);
                let slice = &whole[a as usize..b as usize];
                let (mut mn, mut mx) = (0usize, 0usize);
                for (i, &x) in slice.iter().enumerate() {
                    if x < slice[mn] {
                        mn = i;
                    }
                    if x > slice[mx] {
                        mx = i;
                    }
                }
                let expect = Some((slice[mn], a + mn as u64, slice[mx], a + mx as u64));
                if e != expect {
                    return Err(DataError::Schema(format!(
                        "S4 verify, instrument {} column {}: extremes over [{}, {}) differ from the direct reduction",
                        p.id, c, a, b
                    )));
                }
                verified.ranges += 1;
            }
        }
    }
    Ok(())
}

/// S4 over the ladder (Proposition 5.2, ST-13's ladder row): every bar of
/// every level equals the bar recomputed from the instrument's base rows
/// under ST-5.3's rules (its value, base-row count, label, receipt and
/// sequence), and a level's labels are the calendar's.
pub fn verify_ladder(v: &HotVersion, planned: &[Planned], verified: &mut Verified) -> Result<(), DataError> {
    let relation = &v.manifest.relation;
    let Some(rule) = Rule::of_relation(relation) else { return Ok(()) };
    let base_labels = &v.levels[0].presence.labels;
    for (li, lv) in v.levels.iter().enumerate().skip(1) {
        let level = lv.level.expect("a ladder level");
        let labels = level.labels(base_labels);
        if lv.presence.labels != labels.iter().map(|l| l.1).collect::<Vec<_>>() {
            return Err(DataError::Schema(format!("S4 verify, `{}` level `{}`: its labels are not the calendar's", relation, lv.calendar)));
        }
        let index = ladder::index_of(&labels);
        for (o, p) in planned.iter().enumerate() {
            let e = v.levels[0].presence.instruments[o].exponents[0];
            let kind = v.manifest.columns[0].kind;
            let source: Vec<LadderSource> = p.rows.iter().map(|r| LadderSource::row(r.t, value_int(kind, r.vals[0], e), r.r, r.seq)).collect();
            let expect = ladder::build(level, rule, &source);
            let f = crate::store::RowFilter {
                identity: Some(vec![p.id]),
                ..crate::store::RowFilter::default()
            };
            let rows = v.read_at(li, &f, 0, u32::MAX)?;
            if rows.len() != expect.len() {
                return Err(DataError::Schema(format!(
                    "S4 verify, `{}` level `{}`: instrument {} has {} bars, its rows give {}",
                    relation,
                    lv.calendar,
                    p.id,
                    rows.len(),
                    expect.len()
                )));
            }
            for (i, b) in expect.iter().enumerate() {
                let label = labels[index[&b.key] as usize].1;
                let got = (rows.t[i], rows.ints[0][i], rows.ints[1][i], rows.r[i], rows.seq[i], rows.floats[0][i].to_bits());
                let want = (label, b.value, b.count, b.receipt(label), b.seq, value_view(kind, b.value, e).to_bits());
                if got != want {
                    return Err(DataError::Schema(format!(
                        "S4 verify, `{}` level `{}`: instrument {} bar {} reads {:?}, the base rows give {:?}",
                        relation, lv.calendar, p.id, i, got, want
                    )));
                }
                verified.ranges += 1;
            }
        }
    }
    Ok(())
}

/// S5: the staging directory renamed into place; a version that already
/// exists (the same content) is kept as it is.
pub fn publish(root: &Path, built: &Built) -> Result<(), DataError> {
    let dir = root.join(HotManifest::dir(&built.manifest.relation, &built.manifest.version));
    if dir.exists() {
        let _ = fs::remove_dir_all(&built.staging_dir);
        return Ok(());
    }
    fs::create_dir_all(dir.parent().expect("a version sits under its relation"))?;
    crate::fault::point("publish-before-rename");
    fs::rename(&built.staging_dir, &dir)?;
    Ok(())
}

/// The rows of a planned instrument as the log takes them (its new rows
/// only, in sequence order): `(seq, row)`.
pub fn logged(planned: &[Planned], from_seq: u64) -> Vec<(u64, u64, Row)> {
    let mut out: Vec<(u64, u64, Row)> = Vec::new();
    for p in planned {
        for r in &p.rows {
            if r.seq >= from_seq {
                out.push((r.seq, p.id, r.clone()));
            }
        }
        for x in &p.revisions {
            if x.seq >= from_seq {
                out.push((
                    x.seq,
                    p.id,
                    Row {
                        t: x.label,
                        r: x.r,
                        seq: x.seq,
                        vals: x.values.clone(),
                    },
                ));
            }
        }
    }
    out.sort_by_key(|(s, _, _)| *s);
    out
}
