//! The hot store against the Parquet store (docs/spec/07-storage.md): the
//! same rows ingested with `storage = hot` and without answer every read
//! form of the store alike, byte for byte: every row filter (time ranges,
//! identities, receipt filters, snapshot sequences, latest only), the
//! newest row of an identity, every bucket's rows (the stream's unit), and
//! all rows; through a second ingest that appends rows, adds an instrument
//! between two stored ones and revises a stored row. Versions are
//! content-addressed (the same rows in another store give the same id),
//! unchanged segments are shared by hash (Proposition 7.1), and a
//! corrupted segment is refused by its hash.

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::hot::HotVersion;
use abt_data::{RowFilter, Store};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use arrow::array::{AsArray, Float64Array, Int64Array, RecordBatch, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `(A, T, r, P)` rows of `equity.close`.
fn close_rows(rows: &[(u64, i64, i64, f64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("P", DataType::Float64, false),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// `(A, T, r, V)` rows of `equity.volume` (a count column).
fn volume_rows(rows: &[(u64, i64, i64, i64)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("A", DataType::UInt64, false),
        Field::new("T", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("V", DataType::Int64, false),
        Field::new("r", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(TimestampNanosecondArray::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

fn hot() -> BTreeMap<String, String> {
    BTreeMap::from([("storage".to_string(), "hot".to_string())])
}

/// A market of `ids` over `days` trading days from `start`, each
/// instrument alive over its own span with gaps, prices in cents, receipt
/// an hour after the bar for most rows and later for some.
fn market(rng: &mut Rng, ids: &[u64], start: i64, days: i64) -> Vec<(u64, i64, i64, f64)> {
    let mut rows = Vec::new();
    for &id in ids {
        let from = rng.below(days as u64 / 4) as i64;
        let to = days - rng.below(days as u64 / 4) as i64;
        let mut p = 1_000 + rng.below(50_000) as i64;
        for d in from..to {
            if rng.below(20) == 0 {
                continue; // a gap
            }
            p = (p + rng.below(201) as i64 - 100).max(100);
            let t = (start + d) * NANOS_PER_DAY;
            let r = t + NANOS_PER_HOUR * if rng.below(10) == 0 { 30 } else { 1 };
            rows.push((id, t, r, p as f64 / 100.0));
        }
    }
    // Delivered in time order, as a vendor file is.
    rows.sort_by_key(|r| (r.1, r.0));
    rows
}

/// Two renderings equal, else the first differing line with context.
fn assert_same(what: &str, a: &str, b: &str) {
    if a == b {
        return;
    }
    let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let i = la.iter().zip(&lb).position(|(x, y)| x != y).unwrap_or(la.len().min(lb.len()));
    let ctx = |l: &[&str]| l[i.saturating_sub(2)..(i + 3).min(l.len())].join("\n");
    panic!(
        "{}: {} lines against {}; first difference at line {}:\n--- hot\n{}\n--- parquet\n{}",
        what,
        la.len(),
        lb.len(),
        i,
        ctx(&la),
        ctx(&lb)
    );
}

/// The batches' rows as text: one line per row, every column's value in
/// its shortest exact form, the schema first (a Parquet-store read may
/// come in several batches; the rows are what is compared). The sequence
/// number is left out: the hot store numbers a historical ingest's rows
/// per instrument in time order (its log's order, ST-8.2), the Parquet
/// store in the input's order; `seq` is checked for its own semantics.
fn text(batches: &[RecordBatch]) -> String {
    render(batches, false)
}

fn text_with_seq(batches: &[RecordBatch]) -> String {
    render(batches, true)
}

fn render(batches: &[RecordBatch], with_seq: bool) -> String {
    use arrow::array::{Array, AsArray};
    let mut out = String::new();
    if let Some(b) = batches.first().filter(|_| batches.iter().any(|b| b.num_rows() > 0)) {
        out.push_str(&format!("{:?}\n", b.schema().fields()));
    }
    for b in batches {
        for i in 0..b.num_rows() {
            let cells: Vec<String> = b
                .columns()
                .iter()
                .take(if with_seq { b.num_columns() } else { b.num_columns() - 1 })
                .map(|c| match c.data_type() {
                    DataType::UInt64 => c.as_primitive::<arrow::datatypes::UInt64Type>().value(i).to_string(),
                    DataType::Int64 => c.as_primitive::<arrow::datatypes::Int64Type>().value(i).to_string(),
                    DataType::Float64 => format!("{:?}", c.as_primitive::<arrow::datatypes::Float64Type>().value(i)),
                    DataType::Timestamp(_, _) => c.as_primitive::<arrow::datatypes::TimestampNanosecondType>().value(i).to_string(),
                    other => format!("{:?}:{}", other, c.is_null(i)),
                })
                .collect();
            out.push_str(&cells.join(" "));
            out.push('\n');
        }
    }
    out
}

/// Every read form of the two stores on `version_a` (hot) and
/// `version_b` (Parquet), equal.
#[allow(clippy::too_many_arguments)]
fn same_reads(a: &Store, va: &str, b: &Store, vb: &str, rng: &mut Rng, ids: &[u64], start: i64, days: i64) {
    let t_of = |d: i64| (start + d) * NANOS_PER_DAY;
    let mut filters: Vec<RowFilter> = vec![
        RowFilter::default(),
        RowFilter {
            latest_only: true,
            ..RowFilter::default()
        },
        RowFilter {
            t0: Some(t_of(10)),
            t1: Some(t_of(10)),
            ..RowFilter::default()
        },
        RowFilter {
            identity: Some(vec![ids[0], 999]),
            ..RowFilter::default()
        },
    ];
    for _ in 0..24 {
        let d0 = rng.below(days as u64) as i64;
        let d1 = d0 + rng.below((days - d0) as u64) as i64;
        let who: Option<Vec<u64>> = match rng.below(3) {
            0 => None,
            1 => Some(vec![ids[rng.below(ids.len() as u64) as usize]]),
            _ => Some(vec![ids[0], ids[ids.len() - 1], ids[rng.below(ids.len() as u64) as usize]]),
        };
        filters.push(RowFilter {
            t0: Some(t_of(d0)),
            t1: Some(t_of(d1)),
            identity: who,
            r_max: if rng.below(2) == 0 { Some(t_of(d1) + NANOS_PER_HOUR * rng.below(40) as i64) } else { None },
            as_of_seq: if rng.below(3) == 0 { Some(rng.below(ids.len() as u64 * days as u64)) } else { None },
            latest_only: rng.below(2) == 0,
        });
    }
    for f in &filters {
        if let Some(n) = f.as_of_seq {
            // A snapshot: exactly the rows with seq ≤ N of the unfiltered
            // read, in each store by its own numbering (Proposition 8.1).
            for (store, v, name) in [(a, va, "hot"), (b, vb, "parquet")] {
                let snap = store.read(v, f).unwrap();
                let full = store.read(v, &RowFilter { as_of_seq: None, ..f.clone() }).unwrap();
                let expect: Vec<RecordBatch> = full
                    .iter()
                    .map(|b| {
                        let seqs = b.column(b.num_columns() - 1).as_primitive::<arrow::datatypes::UInt64Type>();
                        let keep: arrow::array::BooleanArray = (0..b.num_rows()).map(|i| Some(seqs.value(i) <= n)).collect();
                        arrow::compute::filter_record_batch(b, &keep).unwrap()
                    })
                    .collect();
                assert_same(&format!("{} snapshot at {} with {:?}", name, n, f), &text_with_seq(&snap), &text_with_seq(&expect));
            }
            continue;
        }
        let ra = a.read(va, f).unwrap();
        let rb = b.read(vb, f).unwrap();
        assert_same(&format!("read with {:?}", f), &text(&ra), &text(&rb));
        let na = a.read_newest(va, f).unwrap();
        let nb = b.read_newest(vb, f).unwrap();
        if f.identity.is_some() {
            assert_same(&format!("read_newest with {:?}", f), &text(&na), &text(&nb));
        }
    }
    assert_same("read_all", &text(&[a.read_all(va).unwrap()]), &text(&[b.read_all(vb).unwrap()]));
    // The hot store's sequence numbers: distinct, and the log holds every
    // row under its number.
    {
        let all = a.read_all(va).unwrap();
        let seqs = all.column(all.num_columns() - 1).as_primitive::<arrow::datatypes::UInt64Type>();
        let mut v: Vec<u64> = seqs.values().to_vec();
        v.sort_unstable();
        let n = v.len();
        v.dedup();
        assert_eq!(v.len(), n, "distinct sequence numbers");
    }
    // The buckets as the stream opens them.
    let (ma, mb) = (a.manifest(va).unwrap(), b.manifest(vb).unwrap());
    assert!(ma.hot && !mb.hot);
    assert_eq!(
        ma.partitions.iter().map(|p| (&p.bucket, p.rows, p.t_min, p.t_max)).collect::<Vec<_>>(),
        mb.partitions.iter().map(|p| (&p.bucket, p.rows, p.t_min, p.t_max)).collect::<Vec<_>>()
    );
    for (pa, pb) in ma.partitions.iter().zip(&mb.partitions) {
        assert!(pa.r_min <= pb.r_min && pa.r_max >= pb.r_max, "a hot bucket's receipt span covers the rows'");
        for f in filters.iter().take(8) {
            // Snapshots differ by numbering (checked above); the rest equal.
            let f = RowFilter { as_of_seq: None, ..f.clone() };
            let ra = a.read_partition_rows(&ma, pa, &f).unwrap();
            let rb = b.read_partition_rows(&mb, pb, &f).unwrap();
            assert_same(&format!("bucket {} with {:?}", pa.bucket, f), &text(&[ra]), &text(&[rb]));
        }
    }
    assert_eq!((ma.rows, ma.from, ma.to), (mb.rows, mb.from, mb.to));
}

#[test]
fn the_hot_store_answers_every_read_as_the_parquet_store_does() {
    let mut rng = Rng(1);
    let ids = [3u64, 5, 8, 13, 21, 34, 55];
    let (start, days) = (19_000i64, 700i64);
    let rows = market(&mut rng, &ids, start, days);
    let mut a = Store::temporary("hot-a").unwrap();
    let mut b = Store::temporary("hot-b").unwrap();
    let va = a.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    let vb = b.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, days);

    // Content-addressed: the same rows in another store give the version.
    let mut c = Store::temporary("hot-c").unwrap();
    let vc = c.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    assert_eq!(va, vc, "the same rows give the same hot version");

    // A second ingest: appends, a new instrument between stored ones (its
    // ordinal shifts the others), a revision of a stored row and a late
    // row, with one duplicate of a stored row.
    let mut more: Vec<(u64, i64, i64, f64)> = market(&mut rng, &[3, 21, 9], start + days, 60);
    more.push(rows[5]);
    let t_rev = rows[100].1;
    more.push((rows[100].0, t_rev, rows[100].2 + 2 * NANOS_PER_DAY, rows[100].3 + 0.5));
    more.push((ids[2], (start + 3) * NANOS_PER_DAY, (start + 3) * NANOS_PER_DAY + 5 * NANOS_PER_DAY, 77.25));
    more.sort_by_key(|r| (r.1, r.0));
    let va2 = a.ingest("equity.close", &[close_rows(&more)], "test", hot()).unwrap();
    let vb2 = b.ingest("equity.close", &[close_rows(&more)], "test", BTreeMap::new()).unwrap();
    assert_ne!(va, va2);
    let mut all_ids: Vec<u64> = ids.to_vec();
    all_ids.push(9);
    all_ids.sort_unstable();
    same_reads(&a, &va2, &b, &vb2, &mut rng, &all_ids, start, days + 60);
    // The first version still reads as it did (D-2.6, Proposition 7.1).
    same_reads(&a, &va, &b, &vb, &mut rng, &ids, start, days);
    // The revision's three readings: latest, as received before it, and
    // every revision.
    let at = RowFilter {
        t0: Some(t_rev),
        t1: Some(t_rev),
        identity: Some(vec![rows[100].0]),
        ..RowFilter::default()
    };
    let both = a.read(&va2, &at).unwrap();
    assert_eq!(both.iter().map(|b| b.num_rows()).sum::<usize>(), 2, "{}", text(&both));
    let latest = a.read(&va2, &RowFilter { latest_only: true, ..at.clone() }).unwrap();
    assert_same("latest revision", &text(&latest), &text(&b.read(&vb2, &RowFilter { latest_only: true, ..at.clone() }).unwrap()));
    let before = RowFilter {
        latest_only: true,
        r_max: Some(rows[100].2),
        ..at.clone()
    };
    assert_same("as received before the revision", &text(&a.read(&va2, &before).unwrap()), &text(&b.read(&vb2, &before).unwrap()));

    // Structural sharing: the second version names the first's segments
    // for the instruments it did not touch, plus new ones.
    let (h1, h2) = (a.hot(&va).unwrap(), a.hot(&va2).unwrap());
    let s1: Vec<String> = h1.manifest.segments().iter().map(|s| s.hash.clone()).collect();
    let s2: Vec<String> = h2.manifest.segments().iter().map(|s| s.hash.clone()).collect();
    assert!(s2.len() > s1.len(), "{:?} {:?}", s1, s2);
    // The id order changed (9 came between 8 and 13), so the kept
    // instruments were copied byte for byte into new runs: the old
    // segments drop out, and the version id is a function of the blocks.
    let kept = h2
        .presence
        .instruments
        .iter()
        .filter(|i| i.id != 9 && i.id != 3 && i.id != 21 && i.id != rows[100].0 && i.id != ids[2])
        .count();
    assert!(kept >= 3);
    let touched = [3u64, 21, 9, rows[100].0, ids[2]];
    let mut compared = 0;
    for (o, inst) in h2.presence.instruments.iter().enumerate() {
        if touched.contains(&inst.id) {
            continue;
        }
        let o1 = h1.presence.ordinal(inst.id).expect("a kept instrument was stored") as usize;
        assert_eq!(inst.rows, h1.presence.instruments[o1].rows);
        for b in 0..h2.column(0, o).blocks_len() {
            assert_eq!(h2.block_hash(0, o, b), h1.block_hash(0, o1, b), "instrument {} block {}", inst.id, b);
            compared += 1;
        }
    }
    assert!(compared >= kept, "{} blocks compared", compared);
}

#[test]
fn a_count_column_and_the_receipt_rule_stage_hot() {
    let mut rng = Rng(2);
    let ids = [1u64, 2, 3];
    let rows: Vec<(u64, i64, i64, i64)> = market(&mut rng, &ids, 20_000, 300).into_iter().map(|(id, t, r, p)| (id, t, r, (p * 1000.0) as i64)).collect();
    let mut a = Store::temporary("hot-vol-a").unwrap();
    let mut b = Store::temporary("hot-vol-b").unwrap();
    let va = a.ingest("equity.volume", &[volume_rows(&rows)], "test", hot()).unwrap();
    let vb = b.ingest("equity.volume", &[volume_rows(&rows)], "test", BTreeMap::new()).unwrap();
    assert_same("volume read_all", &text(&[a.read_all(&va).unwrap()]), &text(&[b.read_all(&vb).unwrap()]));
    let h = a.hot(&va).unwrap();
    // The receipt residual packs to nothing where it is constant.
    assert_eq!(h.manifest.columns.len(), 2);
    assert_eq!(h.manifest.columns[1].name, "r");
}

#[test]
fn a_corrupted_segment_is_refused_by_its_hash() {
    let mut rng = Rng(3);
    let rows = market(&mut rng, &[1, 2], 20_000, 200);
    let mut a = Store::temporary("hot-bad").unwrap();
    let va = a.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    let h = a.hot(&va).unwrap();
    let seg = a.root().join(&h.manifest.segments()[0].file);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[200] ^= 0x10;
    std::fs::write(&seg, &bytes).unwrap();
    let err = HotVersion::open(a.root(), "equity.close", &va, true).err().expect("the hash check refuses the segment");
    assert!(err.to_string().contains("hash"), "{}", err);
}
