//! Helpers shared by the hot store's tests: a deterministic market, row
//! batches of the bar relations, renderings of batches as text, and the
//! comparison of every read form of a hot version against its Parquet
//! twin (`same_reads`).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use abt_data::{RowFilter, Store};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use arrow::array::{AsArray, Float64Array, Int64Array, RecordBatch, TimestampNanosecondArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `(A, T, r, P)` rows of `equity.close`.
pub fn close_rows(rows: &[(u64, i64, i64, f64)]) -> RecordBatch {
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

/// `(A, T, r, P)` rows with the value column named `name`.
pub fn close_rows_named(rows: &[(u64, i64, i64, f64)], name: &str) -> RecordBatch {
    let b = close_rows(rows);
    let fields: Vec<Field> = b
        .schema()
        .fields()
        .iter()
        .map(|f| {
            if f.name() == "P" {
                Field::new(name, f.data_type().clone(), f.is_nullable())
            } else {
                f.as_ref().clone()
            }
        })
        .collect();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), b.columns().to_vec()).unwrap()
}

/// `(A, T, r, V)` rows of `equity.volume` (a count column).
pub fn volume_rows(rows: &[(u64, i64, i64, i64)]) -> RecordBatch {
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

pub fn hot() -> BTreeMap<String, String> {
    BTreeMap::from([("storage".to_string(), "hot".to_string())])
}

/// A market of `ids` over `days` trading days from `start`, each
/// instrument alive over its own span with gaps, prices in cents, receipt
/// an hour after the bar for most rows and later for some.
pub fn market(rng: &mut Rng, ids: &[u64], start: i64, days: i64) -> Vec<(u64, i64, i64, f64)> {
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
pub fn assert_same(what: &str, a: &str, b: &str) {
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
pub fn text(batches: &[RecordBatch]) -> String {
    render(batches, false)
}

pub fn text_with_seq(batches: &[RecordBatch]) -> String {
    render(batches, true)
}

pub fn render(batches: &[RecordBatch], with_seq: bool) -> String {
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
pub fn same_reads(a: &Store, va: &str, b: &Store, vb: &str, rng: &mut Rng, ids: &[u64], start: i64, days: i64) {
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
