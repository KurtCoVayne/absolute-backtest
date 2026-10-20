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

mod hot_common;

use std::collections::BTreeMap;

use abt_data::hot::HotVersion;
use abt_data::{RowFilter, Store};
use abt_wire::scalar::{NANOS_PER_DAY, NANOS_PER_HOUR};
use arrow::array::{AsArray, RecordBatch};
use arrow::datatypes::DataType;
use hot_common::*;

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
        .presence()
        .instruments
        .iter()
        .filter(|i| i.id != 9 && i.id != 3 && i.id != 21 && i.id != rows[100].0 && i.id != ids[2])
        .count();
    assert!(kept >= 3);
    let touched = [3u64, 21, 9, rows[100].0, ids[2]];
    let mut compared = 0;
    for (o, inst) in h2.presence().instruments.iter().enumerate() {
        if touched.contains(&inst.id) {
            continue;
        }
        let o1 = h1.presence().ordinal(inst.id).expect("a kept instrument was stored") as usize;
        assert_eq!(inst.rows, h1.presence().instruments[o1].rows);
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

#[test]
fn a_query_asking_for_ticks_answers_exact_decimals_that_view_as_the_floats() {
    use abt_data::query::{execute, ResolvedView};
    use abt_wire::generated::abt::wire::{Atom, Constant, ConstantKind, Disjunction, Identity, Op, Predicate, QueryForm, Scan, Series};
    let mut rng = Rng(4);
    let ids = [2u64, 4, 6];
    let rows = market(&mut rng, &ids, 20_000, 120);
    let mut a = Store::temporary("hot-ticks").unwrap();
    a.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    let ent = |x: u64| Identity {
        values: vec![Constant {
            kind: ConstantKind::Entity,
            num: 0.0,
            int: x as i64,
            text: None,
        }],
    };
    let series = QueryForm::Series(Box::new(Series {
        relation: "equity.close".into(),
        identity: Some(Box::new(ent(4))),
        t0: 20_010 * NANOS_PER_DAY,
        t1: 20_100 * NANOS_PER_DAY,
    }));
    let (_, floats) = execute(&a, &ResolvedView::default(), &series, None, false, false).unwrap();
    let (schema, ticks) = execute(&a, &ResolvedView::default(), &series, None, false, true).unwrap();
    let p = schema.index_of("P").unwrap();
    assert_eq!(schema.field(p).data_type(), &DataType::Decimal128(18, 2), "cents at scale 2");
    let f = floats[0].column(p).as_primitive::<arrow::datatypes::Float64Type>();
    let d = ticks[0].column(p).as_primitive::<arrow::datatypes::Decimal128Type>();
    assert_eq!(f.len(), d.len());
    assert!(f.len() > 10);
    for i in 0..f.len() {
        assert_eq!(abt_agg::exact::tick_to_f64(d.value(i), 2).to_bits(), f.value(i).to_bits(), "row {}", i);
    }
    // A predicate over the ticks compares their decimal view.
    let scan = |ticks: bool| {
        let form = QueryForm::Scan(Box::new(Scan {
            relation: "equity.close".into(),
            t0: 20_000 * NANOS_PER_DAY,
            t1: 20_200 * NANOS_PER_DAY,
            predicate: Some(Box::new(Predicate {
                all: Some(vec![Disjunction {
                    atoms: vec![Atom {
                        column: "P".into(),
                        op: Op::Gt,
                        values: Some(vec![Constant {
                            kind: ConstantKind::Num,
                            num: 250.0,
                            int: 0,
                            text: None,
                        }]),
                    }],
                }]),
            })),
            columns: None,
        }));
        execute(&a, &ResolvedView::default(), &form, None, false, ticks).unwrap().1
    };
    let (f, d) = (scan(false), scan(true));
    let n = |b: &[RecordBatch]| b.iter().map(|x| x.num_rows()).sum::<usize>();
    assert_eq!(n(&f), n(&d));
    assert!(n(&f) > 0 && n(&f) < rows.len());
}

/// ST-3.2 and A-D54: a value off every decimal grid keeps its bits in the
/// `f64` view, counts as 0 in the decoded integers as in the summaries (so
/// S4's range checks and every window through the cumulatives hold across
/// its block), and its column has no tick view.
#[test]
fn a_value_off_every_grid_keeps_its_bits_counts_as_zero_and_has_no_tick_view() {
    use abt_codec::summary::{self, Column, Cumulative};
    use abt_codec::tick::{exponent, Source};
    use abt_data::query::{execute, ResolvedView};
    use abt_wire::generated::abt::wire::{Constant, ConstantKind, Identity, QueryForm, Series};
    let mut rng = Rng(9);
    let mut rows = market(&mut rng, &[7u64], 21_000, 8_000);
    assert!(rows.len() > 3 * 1024, "{} rows", rows.len());
    // One quotient among the cents, in the second block.
    let k = 1_500;
    let off = 0.1 + 0.2;
    assert!(exponent(&[off], Source::F64).is_none());
    rows[k].3 = off;
    let mut a = Store::temporary("hot-raw").unwrap();
    // S4 passes: the summaries and the decoded integers agree.
    let va = a.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    let h = a.hot(&va).unwrap();
    assert!(h.manifest.columns[0].raw, "the manifest flags the column");
    let col = h.column(0, 0);
    assert!(col.blocks() >= 3);
    let whole: Vec<i64> = (0..col.blocks()).flat_map(|b| col.block(b)).collect();
    assert_eq!(whole.len(), rows.len());
    assert_eq!(whole[k], 0, "the raw position counts as 0");
    // The raw block is encoded at exponent 9 (every grid holds its other
    // values alike), so the instrument's integers are at that scale.
    let e = h.manifest.columns[0].exponent;
    assert_eq!(
        whole[k + 1],
        (rows[k + 1].3 * abt_agg::exact::pow10(e as u32) as f64).round() as i64,
        "its neighbours are the cents at exponent {}",
        e
    );
    for (lo, hi) in [(10u64, 2_900u64), (1_024, 2_048), (1_499, 1_501), (0, rows.len() as u64), (1_400, 3_100)] {
        let direct = Cumulative::ZERO.plus(&whole[lo as usize..hi as usize], lo);
        assert_eq!(summary::moments(&col, lo, hi), direct, "moments over [{}, {})", lo, hi);
    }
    // The f64 view returns the bits; the tick view is refused naming the column.
    let series = QueryForm::Series(Box::new(Series {
        relation: "equity.close".into(),
        identity: Some(Box::new(Identity {
            values: vec![Constant {
                kind: ConstantKind::Entity,
                num: 0.0,
                int: 7,
                text: None,
            }],
        })),
        t0: rows[k].1,
        t1: rows[k].1 + NANOS_PER_HOUR,
    }));
    let (schema, floats) = execute(&a, &ResolvedView::default(), &series, None, false, false).unwrap();
    let p = schema.index_of("P").unwrap();
    let f = floats[0].column(p).as_primitive::<arrow::datatypes::Float64Type>();
    assert_eq!(f.len(), 1);
    assert_eq!(f.value(0).to_bits(), off.to_bits());
    let err = execute(&a, &ResolvedView::default(), &series, None, false, true).expect_err("no tick view");
    assert!(err.to_string().contains("`equity.close`.`P` holds values off any decimal grid"), "{}", err);
}

#[test]
fn the_ladder_bars_equal_their_buckets_recomputed_from_base_rows() {
    use abt_data::hot::ladder::{Level, Rule};
    use abt_wire::scalar::NANOS_PER_MINUTE;
    // A daily market: close (last), high (max) and volume (sum) relations.
    let mut rng = Rng(5);
    let ids = [11u64, 12, 13, 14];
    let (start, days) = (19_700i64, 420i64);
    let closes = market(&mut rng, &ids, start, days);
    let highs: Vec<(u64, i64, i64, f64)> = closes.iter().map(|&(a, t, r, p)| (a, t, r, p + 0.5)).collect();
    let volumes: Vec<(u64, i64, i64, i64)> = closes.iter().map(|&(a, t, r, p)| (a, t, r, (p * 10.0) as i64 + 1)).collect();
    let mut s = Store::temporary("hot-ladder").unwrap();
    let vc = s.ingest("equity.close", &[close_rows(&closes)], "test", hot()).unwrap();
    let vh = s.ingest("equity.high", &[close_rows_named(&highs, "H")], "test", hot()).unwrap();
    let vv = s.ingest("equity.volume", &[volume_rows(&volumes)], "test", hot()).unwrap();
    let h = s.hot(&vc).unwrap();
    assert_eq!(h.levels.iter().skip(1).map(|l| l.calendar.clone()).collect::<Vec<_>>(), vec!["1w", "1mo", "1q", "1y"]);
    for (version, rows, rule, scale) in [(&vc, &closes, Rule::Last, 100.0), (&vh, &highs, Rule::Max, 100.0)] {
        for level in Level::of_base("1d") {
            let (schema, batches) = s.read_level(version, &level.name(), &RowFilter::default()).unwrap();
            assert_eq!(schema.calendar.as_deref(), Some(level.name().as_str()));
            let got = text(&batches);
            // The direct recomputation: per instrument, per bucket of its
            // base rows, the rule over the values in time order, the count,
            // the label (the calendar's last trade date in the bucket), the
            // receipt (the bucket's end: the label plus the last row's delay).
            let base_labels: Vec<i64> = {
                let mut l: Vec<i64> = rows.iter().map(|r| r.1).collect();
                l.sort_unstable();
                l.dedup();
                l
            };
            let labels = level.labels(&base_labels);
            let label_of = |key: i64| labels.iter().find(|l| l.0 == key).unwrap().1;
            let mut expect: Vec<(i64, u64, f64, i64, i64)> = Vec::new();
            for &id in &ids {
                let mine: Vec<&(u64, i64, i64, f64)> = rows.iter().filter(|r| r.0 == id).collect();
                let mut i = 0;
                while i < mine.len() {
                    let key = level.bucket_start(mine[i].1);
                    let mut j = i;
                    let mut acc = (mine[i].3 * scale).round() as i64;
                    let mut max_r = i64::MIN;
                    while j < mine.len() && level.bucket_start(mine[j].1) == key {
                        acc = rule.fold(acc, (mine[j].3 * scale).round() as i64);
                        max_r = max_r.max(mine[j].2);
                        j += 1;
                    }
                    let last = mine[j - 1];
                    let label = label_of(key);
                    let r = (label + (last.2 - last.1)).max(max_r);
                    expect.push((label, id, acc as f64 / scale, (j - i) as i64, r));
                    i = j;
                }
            }
            expect.sort_by_key(|e| (e.0, e.1));
            let want = expect.iter().map(|e| format!("{} {} {:?} {} {}", e.1, e.0, e.2, e.3, e.4)).collect::<Vec<_>>().join("\n");
            let got_rows: Vec<String> = got.lines().skip(1).map(|l| l.to_string()).collect();
            assert_eq!(got_rows.join("\n"), want, "level {} of {}", level.name(), schema.name);
        }
    }
    // Volume sums.
    let (_, batches) = s.read_level(&vv, "1mo", &RowFilter::default()).unwrap();
    let got = text(&batches);
    assert!(got.lines().count() > 10);
    let total_got: f64 = batches[0].column(2).as_primitive::<arrow::datatypes::Float64Type>().values().iter().sum();
    let total: i64 = volumes.iter().map(|v| v.3).sum();
    assert_eq!(total_got, total as f64, "the month sums add up to the volume");
    // A minute market: the intraday levels nest and the day level is the
    // trade date.
    let minutes: Vec<(u64, i64, i64, f64)> = (0..3u64)
        .flat_map(|a| {
            (0..4i64).flat_map(move |d| {
                (0..390i64).map(move |m| {
                    let t = (start + d) * NANOS_PER_DAY + 14 * NANOS_PER_HOUR + (31 + m) * NANOS_PER_MINUTE;
                    (20 + a, t, t + NANOS_PER_MINUTE, 50.0 + a as f64 + (m % 17) as f64 * 0.25)
                })
            })
        })
        .collect();
    let mut m = Store::temporary("hot-ladder-m").unwrap();
    let mut rows = close_rows(&minutes);
    rows = rows.clone();
    let vm = m.ingest("future.close", &[rows], "test", hot()).unwrap();
    let hm = m.hot(&vm).unwrap();
    assert_eq!(
        hm.levels.iter().skip(1).map(|l| l.calendar.clone()).collect::<Vec<_>>(),
        vec!["5m", "15m", "30m", "1h", "1d", "1w", "1mo"]
    );
    let (_, day) = m.read_level(&vm, "1d", &RowFilter::default()).unwrap();
    assert_eq!(day[0].num_rows(), 12, "4 days of 3 instruments");
    let counts = day[0].column(3).as_primitive::<arrow::datatypes::Int64Type>();
    assert!(counts.values().iter().all(|&c| c == 390));
    // Bars from 14:31 to 21:00: the first bucket (14:30) holds four, the
    // last (21:00) one, 79 buckets a day.
    let (_, five) = m.read_level(&vm, "5m", &RowFilter::default()).unwrap();
    assert_eq!(five[0].num_rows(), 12 * 79);
    let counts = five[0].column(3).as_primitive::<arrow::datatypes::Int64Type>();
    assert_eq!(counts.values().iter().sum::<i64>(), 12 * 390);
    // Rows come in (label, instrument) order: the day's last bucket of the
    // first instrument is row 78 × 3.
    assert_eq!((counts.value(0), counts.value(78 * 3)), (4, 1));
}

/// E-111: an ingest whose rows need a finer exponent for one instrument
/// than its stored ones (six decimals after three), and that is no pure
/// append (another instrument's row revised), stages a new version: the
/// instrument rebuilt at the finer exponent, an untouched one kept in the
/// previous segments, which hold the rebuilt instrument's earlier run too.
/// The open checked that run against the presence before the new run
/// superseded it and refused the version (`equity.open`, Norgate 2015 onto
/// 1990–2014). The version opens and reads as its Parquet twin.
#[test]
fn a_finer_exponent_in_a_new_version_opens() {
    let day = |d: i64| (19_000 + d) * NANOS_PER_DAY;
    let rows: Vec<(u64, i64, i64, f64)> = (0..20)
        .flat_map(|d| [(1u64, day(d), day(d), 10.125 + d as f64), (2u64, day(d), day(d), 20.5 + d as f64), (3u64, day(d), day(d), 30.25 + d as f64)])
        .collect();
    // Instrument 1's next days at six decimals; instrument 2's day 3 revised
    // (received on day 22), so the ingest is no append; instrument 3 kept.
    let mut more: Vec<(u64, i64, i64, f64)> = (20..25).map(|d| (1u64, day(d), day(d), 30.123456 + d as f64)).collect();
    more.push((2u64, day(3), day(22), 99.5));
    more.sort_by_key(|r| (r.1, r.0));
    let mut a = Store::temporary("hot-exponent-a").unwrap();
    let mut b = Store::temporary("hot-exponent-b").unwrap();
    let v1 = a.ingest("equity.close", &[close_rows(&rows)], "test", hot()).unwrap();
    b.ingest("equity.close", &[close_rows(&rows)], "test", BTreeMap::new()).unwrap();
    let va = a.ingest("equity.close", &[close_rows(&more)], "test", hot()).unwrap();
    let vb = b.ingest("equity.close", &[close_rows(&more)], "test", BTreeMap::new()).unwrap();
    assert_ne!(v1, va, "a new version");
    let h = a.hot(&va).unwrap();
    let o = h.presence().ordinal(1).unwrap() as usize;
    assert!(h.presence().instruments[o].exponents[0] > 3, "instrument 1 at a finer exponent than its stored three");
    same_reads(&a, &va, &b, &vb, &mut Rng(2), &[1, 2, 3], 19_000, 25);
}
