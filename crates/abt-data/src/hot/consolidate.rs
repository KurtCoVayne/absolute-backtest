//! Consolidation (ST-8.7; A-D42, A-D51): the journaled blocks of a
//! version's append files folded into base segments in instrument order,
//! each block copied as its encoder produced it (its header, packed words
//! and exception list are the same bytes, so its hash carries over; the
//! version's id is the published state's and never recomputed,
//! Proposition 7.1); the presence extended with the folded rows' labels
//! and sequence numbers; the manifest rewritten in place by an atomic
//! rename under the next journal generation. Every block but an
//! instrument's last is full (ST-4.1), so where an instrument's published
//! history ends in a partial block, that block (the tail of ST-8.3, held
//! immutable in its segment under A-D48) is re-encoded together with the
//! folded rows into full blocks: the same values at the same rows
//! (Proposition 3.2), checked by decoding. Tails are not folded: they are a
//! function of the log (ST-8.8), replayed from the least sequence number a
//! tail holds. One column at a time, so the extra memory is one segment
//! being written and the blocks of the instrument being copied.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use abt_codec::rank::Bitmap;
use abt_codec::summary::{Column, Cumulative};
use abt_codec::tick::{encode, encode_integers, Encoded, Source};
use abt_codec::BLOCK;

use super::append::{block_hash_of, decode_block, encoded_of, Overlay};
use super::manifest::{Bucket, HotManifest, Kind, SegmentRef};
use super::presence::{Instrument, Presence};
use super::segment::{Sealed, Writer};
use super::version::HotVersion;
use crate::error::DataError;
use crate::staging::build::SEGMENT_BYTES;

/// What a consolidation did.
#[derive(Clone, Debug)]
pub struct Consolidated {
    pub manifest: HotManifest,
    /// Blocks copied from the append files.
    pub folded_blocks: u64,
    pub folded_rows: u64,
    pub segments_written: u64,
}

/// One instrument of the new layout: its old ordinal (when published) and
/// its overlay ordinal (every published instrument has one).
struct Plan {
    id: u64,
    old: Option<usize>,
    ov: usize,
}

/// Folds the sealed blocks of `v`'s overlay into its base segments and
/// publishes the new layout under the version's id; `None` when nothing is
/// sealed.
#[allow(clippy::needless_range_loop)]
pub fn consolidate(root: &Path, v: &HotVersion) -> Result<Option<Consolidated>, DataError> {
    let mut ov = v.overlay.lock().unwrap_or_else(|p| p.into_inner());
    if ov.instruments.iter().all(|i| i.sealed.is_empty()) {
        return Ok(None);
    }
    let base = &v.levels[0];
    let old = &base.presence;
    let ncols = v.manifest.columns.len();
    let nvalues = ov.value_columns();
    debug_assert_eq!(ncols, nvalues + 1, "the base's columns are the values and the receipt residual");
    let kinds: Vec<Kind> = v.manifest.columns.iter().map(|c| c.kind).collect();
    // The instruments of the new layout, in id order: every published one
    // and every overlay instrument with a sealed block.
    let mut ids: Vec<u64> = old.instruments.iter().map(|i| i.id).collect();
    ids.extend(ov.instruments.iter().filter(|i| !i.sealed.is_empty()).map(|i| i.id));
    ids.sort_unstable();
    ids.dedup();
    let plans: Vec<Plan> = ids
        .iter()
        .map(|&id| Plan {
            id,
            old: old.ordinal(id).map(|o| o as usize),
            ov: ov.by_id[&id],
        })
        .collect();
    // The folded rows' labels, receipts and sequence numbers per
    // instrument (decoded once; the value columns are copied below).
    let mut folded: Vec<(Vec<i64>, Vec<i64>, Vec<u64>)> = Vec::with_capacity(plans.len());
    let mut folded_rows = 0u64;
    let mut folded_blocks = 0u64;
    for p in &plans {
        let sealed = ov.instruments[p.ov].sealed.clone();
        let (mut labels, mut res, mut seqs) = (Vec::new(), Vec::new(), Vec::new());
        for s in &sealed {
            let (r, _) = ov.decode_at(s.locs[nvalues], 0, Kind::Residual)?;
            let (l, _) = ov.decode_at(s.locs[nvalues + 1], 0, Kind::Int)?;
            let (q, _) = ov.decode_at(s.locs[nvalues + 2], 0, Kind::Int)?;
            labels.extend_from_slice(&l[..s.rows as usize]);
            res.extend_from_slice(&r[..s.rows as usize]);
            seqs.extend(q[..s.rows as usize].iter().map(|&x| x as u64));
            folded_rows += s.rows as u64;
            folded_blocks += ncols as u64;
        }
        folded.push((labels, res, seqs));
    }
    // The new labels: the published ones and the folded ones.
    let mut labels: Vec<i64> = old.labels.clone();
    for (l, _, _) in &folded {
        labels.extend_from_slice(l);
    }
    labels.sort_unstable();
    labels.dedup();
    let index_of = |t: i64| -> u32 { labels.partition_point(|&l| l < t) as u32 };
    // The new presence: each instrument's rows are its published ones (in
    // label order) then its folded ones (after its last published label).
    let mut instruments: Vec<Instrument> = Vec::with_capacity(plans.len());
    for (pi, p) in plans.iter().enumerate() {
        let (fl, _, fs_) = &folded[pi];
        let mut row_labels: Vec<u32> = Vec::new();
        let mut seq_runs: Vec<(u64, u64, u64)> = Vec::new();
        let mut published_rows = 0u64;
        if let Some(o) = p.old {
            let inst = &old.instruments[o];
            let len = (inst.last - inst.first + 1) as usize;
            for bit in super::version::ones(inst.presence.words(), 0, len) {
                row_labels.push(index_of(old.labels[inst.first as usize + bit]));
            }
            seq_runs = inst.seq_runs.clone();
            published_rows = inst.rows;
            debug_assert_eq!(published_rows, row_labels.len() as u64);
        }
        for (k, (&t, &seq)) in fl.iter().zip(fs_).enumerate() {
            let row = published_rows + k as u64;
            row_labels.push(index_of(t));
            match seq_runs.last_mut() {
                Some(run) if run.0 + run.2 == row && run.1 + run.2 == seq => run.2 += 1,
                _ => seq_runs.push((row, seq, 1)),
            }
        }
        if row_labels.is_empty() {
            continue;
        }
        if row_labels.windows(2).any(|w| w[0] >= w[1]) {
            return Err(DataError::Schema(format!(
                "consolidate `{}`: instrument {}'s folded rows are not after its published ones",
                v.manifest.relation, p.id
            )));
        }
        let (first, last) = (row_labels[0], *row_labels.last().expect("rows"));
        let mut bits = Bitmap::new();
        let mut at = first;
        for &l in &row_labels {
            while at < l {
                bits.push(false);
                at += 1;
            }
            bits.push(true);
            at += 1;
        }
        let mut exponents: Vec<u8> = ov.instruments[p.ov].exponents.clone();
        exponents.resize(ncols, 0);
        if let Some(o) = p.old {
            for (c, e) in exponents.iter_mut().enumerate() {
                *e = (*e).max(old.instruments[o].exponents.get(c).copied().unwrap_or(0));
            }
        }
        instruments.push(Instrument {
            id: p.id,
            first,
            last,
            rows: row_labels.len() as u64,
            seq_runs,
            exponents,
            presence: bits,
        });
    }
    let plans: Vec<&Plan> = plans.iter().filter(|p| instruments.iter().any(|i| i.id == p.id)).collect();
    let folded_of: BTreeMap<u64, usize> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    // The segments, one column at a time (ST-8.7's memory bound): every
    // instrument's published blocks then its folded ones, at its exponent.
    let seg_dir = root.join("hot").join("segments");
    fs::create_dir_all(&seg_dir)?;
    let mut lists: Vec<Vec<SegmentRef>> = vec![Vec::new(); ncols];
    let mut written = 0u64;
    for c in 0..ncols {
        let kind = kinds[c];
        let source = if kind == Kind::TickF32 { Source::F32 } else { Source::F64 };
        let mut w = Writer::new();
        for (ordinal, p) in plans.iter().enumerate() {
            let exponent = instruments[ordinal].exponents[c];
            let mut encs: Vec<Encoded> = Vec::new();
            let mut values: Vec<Vec<i64>> = Vec::new();
            // The rows from a partial published block on: re-encoded.
            let mut tail_ints: Vec<i64> = Vec::new();
            let mut tail_floats: Vec<f64> = Vec::new();
            let sealed = ov.instruments[p.ov].sealed.clone();
            if let Some(o) = p.old {
                let view = v.column(c, o);
                let up = abt_agg::exact::pow10((exponent - old.instruments[o].exponents[c]) as u32) as i64;
                for b in 0..view.blocks() {
                    let block = v.block_at(0, c, o, b);
                    let mut ints = block.ints.clone();
                    if up != 1 {
                        for x in ints.iter_mut() {
                            *x = x.wrapping_mul(up);
                        }
                    }
                    if ints.len() < BLOCK && !sealed.is_empty() {
                        tail_ints.extend_from_slice(&ints);
                        tail_floats.extend_from_slice(&block.floats);
                    } else {
                        encs.push(v.encoded(c, o, b));
                        values.push(ints);
                    }
                }
            }
            for s in &sealed {
                let bytes = ov.bytes_at(s.locs[c])?;
                let (ints, floats) = decode_block(&bytes, exponent, kind);
                if tail_ints.is_empty() {
                    // Aligned: copied as it is. S4: the copy hashes as the
                    // sealed block did.
                    let enc = encoded_of(&bytes);
                    let copied = block_hash_of(&super::append::bytes_of(&enc));
                    if copied != block_hash_of(&bytes) {
                        return Err(DataError::Schema(format!(
                            "consolidate `{}`: instrument {}'s block of column {} does not copy to its hash",
                            v.manifest.relation, p.id, c
                        )));
                    }
                    encs.push(enc);
                    values.push(ints);
                } else {
                    tail_ints.extend_from_slice(&ints);
                    tail_floats.extend_from_slice(&floats);
                }
            }
            for (ci, chunk) in tail_ints.chunks(BLOCK).enumerate() {
                let fl = &tail_floats[ci * BLOCK..ci * BLOCK + chunk.len()];
                let enc = match kind {
                    Kind::Tick | Kind::TickF32 => encode(fl, source),
                    Kind::Int | Kind::Residual => encode_integers(chunk),
                };
                // S4: the re-encoded block decodes to its rows (Proposition
                // 3.2): the source's values bit for bit, and the integers
                // at the run's exponent where the block is on a grid.
                let same = match kind {
                    Kind::Tick | Kind::TickF32 => enc.decode().iter().zip(fl).all(|(a, b)| a.to_bits() == b.to_bits()),
                    Kind::Int | Kind::Residual => enc.integers().is_some_and(|back| back == chunk),
                };
                let grid = match enc.integers() {
                    Some(back) if enc.e <= exponent => {
                        let up = abt_agg::exact::pow10((exponent - enc.e) as u32) as i64;
                        back.iter().zip(chunk).all(|(b, x)| b.wrapping_mul(up) == *x)
                    }
                    Some(_) => false,
                    None => true,
                };
                if !same || !grid {
                    return Err(DataError::Schema(format!(
                        "consolidate `{}`: instrument {}'s re-encoded tail of column {} does not reproduce its rows",
                        v.manifest.relation, p.id, c
                    )));
                }
                encs.push(enc);
                values.push(chunk.to_vec());
            }
            if encs.is_empty() {
                continue;
            }
            let blocks: Vec<Sealed> = encs.iter().zip(&values).map(|(enc, values)| Sealed { enc, values }).collect();
            w.run(ordinal as u32, 0, exponent, &blocks, &Cumulative::ZERO, 0);
            if w.len() >= SEGMENT_BYTES {
                finish(root, &seg_dir, c, &mut w, &mut lists, &mut written)?;
            }
        }
        finish(root, &seg_dir, c, &mut w, &mut lists, &mut written)?;
    }
    let _ = folded_of;
    // The buckets: the published ones re-indexed over the new labels, the
    // folded rows added to theirs.
    let calendar = v.manifest.schema.calendar.as_deref();
    let mut buckets: BTreeMap<String, Bucket> = v.manifest.buckets.iter().map(|b| (b.bucket.clone(), b.clone())).collect();
    for (pi, p) in plans.iter().enumerate() {
        let (fl, fr, fs_) = &folded[folded_of[&p.id]];
        let _ = pi;
        for k in 0..fl.len() {
            let (name, _, _) = crate::store::bucket_span(calendar, fl[k]);
            let r = fl[k] + fr[k];
            let b = buckets.entry(name.clone()).or_insert(Bucket {
                bucket: name,
                lo: 0,
                hi: 0,
                t_min: fl[k],
                t_max: fl[k],
                r_min: r,
                r_max: r,
                rows: 0,
                seq_max: 0,
            });
            b.t_min = b.t_min.min(fl[k]);
            b.t_max = b.t_max.max(fl[k]);
            b.r_min = b.r_min.min(r);
            b.r_max = b.r_max.max(r);
            b.rows += 1;
            b.seq_max = b.seq_max.max(fs_[k]);
        }
    }
    let mut buckets: Vec<Bucket> = buckets.into_values().collect();
    for b in buckets.iter_mut() {
        let (_, from, to) = crate::store::bucket_span(calendar, b.t_min);
        b.lo = labels.partition_point(|&l| l < from) as u32;
        b.hi = labels.partition_point(|&l| l < to) as u32;
    }
    buckets.sort_by_key(|b| b.lo);
    // The new presence file and the manifest, renamed into place.
    let presence = Presence::new(labels.clone(), instruments);
    let dir = root.join(HotManifest::dir(&v.manifest.relation, &v.manifest.version));
    let generation = v.manifest.generation + 1;
    let presence_file = format!("presence-g{}.bin", generation);
    write_synced(&dir.join(&presence_file), &presence.to_bytes())?;
    let folded_seq_max = folded.iter().flat_map(|(_, _, q)| q.iter().copied()).max().unwrap_or(v.manifest.seq_high);
    let mut manifest = v.manifest.clone();
    manifest.levels[0].segments = lists;
    manifest.levels[0].presence = presence_file;
    for (c, col) in manifest.columns.iter_mut().enumerate() {
        col.exponent = presence.instruments.iter().map(|i| i.exponents[c]).max().unwrap_or(col.exponent).max(col.exponent);
    }
    manifest.rows = presence.rows() + manifest.revisions.len() as u64;
    manifest.from = labels.first().copied();
    manifest.to = labels.last().copied();
    manifest.buckets = buckets;
    manifest.generation = generation;
    manifest.replay_from = Some(ov.least_tail_seq().unwrap_or(folded_seq_max + 1).min(folded_seq_max + 1));
    let tmp = dir.join("manifest.json.tmp");
    write_synced(&tmp, &serde_json::to_vec_pretty(&manifest)?)?;
    crate::fault::point("consolidate-before-rename");
    fs::rename(&tmp, dir.join("manifest.json"))?;
    if let Ok(d) = fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    // The folded journal and its append files go at the next open
    // (`Overlay::remove_earlier_generations`); the old base segments when
    // no version names them (ST-7.5).
    drop(ov);
    Ok(Some(Consolidated {
        manifest,
        folded_blocks,
        folded_rows,
        segments_written: written,
    }))
}

fn finish(root: &Path, seg_dir: &Path, c: usize, w: &mut Writer, lists: &mut [Vec<SegmentRef>], written: &mut u64) -> Result<(), DataError> {
    if w.is_empty() {
        return Ok(());
    }
    let done = std::mem::take(w);
    let (bytes, hash) = done.finish();
    let file = HotManifest::segment_file(&hash);
    let path = root.join(&file);
    if !path.exists() {
        let tmp = seg_dir.join(format!("{}.tmp-{}", hash, std::process::id()));
        write_synced(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
        *written += 1;
    }
    lists[c].push(SegmentRef {
        hash,
        file,
        bytes: bytes.len() as u64,
    });
    Ok(())
}

fn write_synced(path: &PathBuf, bytes: &[u8]) -> Result<(), DataError> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// The appended rows of the overlay that a consolidation would fold, for
/// the tests of ST-13: `(instrument, label, seq)` of every sealed row.
pub fn sealed_rows(ov: &mut Overlay) -> Result<Vec<(u64, i64, u64)>, DataError> {
    let n = ov.value_columns();
    let mut out = Vec::new();
    for o in 0..ov.instruments.len() {
        let id = ov.instruments[o].id;
        let sealed = ov.instruments[o].sealed.clone();
        for s in &sealed {
            let (l, _) = ov.decode_at(s.locs[n + 1], 0, Kind::Int)?;
            let (q, _) = ov.decode_at(s.locs[n + 2], 0, Kind::Int)?;
            for k in 0..s.rows as usize {
                out.push((id, l[k], q[k] as u64));
            }
        }
    }
    Ok(out)
}
