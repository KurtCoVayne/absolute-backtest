//! The codec's figures of ST-2.4 (decode and sum per width against raw
//! scans; random windows; the range operations of ST-6), to run with
//! `cargo bench -p abt-codec` and record with the machine's name.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use abt_codec::block::{extract, pack, unpack, Packed};
use abt_codec::summary::{self, BlockSummary, Column, Cumulative, SparseTable};
use abt_codec::wavelet::WaveletMatrix;
use abt_codec::BLOCK;

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

fn decode_and_sum(c: &mut Criterion) {
    let blocks = 32_768; // 33.5M values
    let mut group = c.benchmark_group("decode_and_sum");
    group.sample_size(10);
    for width in [5u8, 8, 11, 14, 20, 27] {
        let m = (1u64 << width) - 1;
        let offsets: Vec<u64> = (0..BLOCK as u64).map(|j| (j * 2_654_435_761) & m).collect();
        let packed: Vec<_> = (0..blocks).map(|_| pack(&offsets, width)).collect();
        group.bench_function(format!("packed_w{}", width), |b| {
            b.iter(|| {
                let mut out = [0u64; BLOCK];
                let mut sum = 0u64;
                for p in &packed {
                    unpack(p, &mut out);
                    sum = sum.wrapping_add(out.iter().sum::<u64>());
                }
                black_box(sum)
            })
        });
        let raw: Vec<u32> = (0..blocks * BLOCK).map(|j| (j as u64 & m) as u32).collect();
        group.bench_function(format!("raw_u32_w{}", width), |b| b.iter(|| black_box(raw.iter().map(|&x| x as u64).sum::<u64>())));
        group.bench_function(format!("point_w{}", width), |b| {
            let mut j = 0usize;
            b.iter(|| {
                j = (j + 7_919) % (blocks * BLOCK);
                black_box(extract(&packed[j / BLOCK], j % BLOCK))
            })
        });
    }
    group.finish();
}

/// A column of packed blocks at one width with its structures: the form
/// the range rules read (a decode per block they touch).
struct PackedColumn {
    packed: Vec<Packed>,
    raw: Vec<u32>,
    cumulatives: Vec<Cumulative>,
    summaries: Vec<BlockSummary>,
    sparse: SparseTable,
}

impl PackedColumn {
    fn of(rng: &mut Rng, blocks: usize, width: u8) -> PackedColumn {
        let m = (1u64 << width) - 1;
        let raw: Vec<u32> = (0..blocks * BLOCK).map(|_| (rng.next() & m) as u32).collect();
        let packed: Vec<Packed> = raw.chunks(BLOCK).map(|c| pack(&c.iter().map(|&x| x as u64).collect::<Vec<_>>(), width)).collect();
        let values: Vec<Vec<i64>> = raw.chunks(BLOCK).map(|c| c.iter().map(|&x| x as i64).collect()).collect();
        let cumulatives = summary::cumulatives(values.iter().map(|v| v.as_slice()));
        let summaries: Vec<BlockSummary> = values.iter().map(|v| BlockSummary::of(v)).collect();
        let sparse = SparseTable::of(&summaries);
        PackedColumn {
            packed,
            raw,
            cumulatives,
            summaries,
            sparse,
        }
    }

    fn rows(&self) -> u64 {
        self.raw.len() as u64
    }
}

impl Column for PackedColumn {
    fn block(&self, b: usize) -> Vec<i64> {
        let mut out = [0u64; BLOCK];
        unpack(&self.packed[b], &mut out);
        out.iter().map(|&u| u as i64).collect()
    }
    fn len(&self, _b: usize) -> usize {
        BLOCK
    }
    fn blocks(&self) -> usize {
        self.packed.len()
    }
    fn cumulative(&self, boundary: usize) -> &Cumulative {
        &self.cumulatives[boundary]
    }
    fn summary(&self, b: usize) -> &BlockSummary {
        &self.summaries[b]
    }
    fn sparse(&self) -> &SparseTable {
        &self.sparse
    }
}

/// Random `[a, b)` with `b − a` in `[lo, hi)`.
fn windows(rng: &mut Rng, rows: u64, lo: u64, hi: u64, count: usize) -> Vec<(u64, u64)> {
    (0..count)
        .map(|_| {
            let len = lo + rng.below(hi - lo);
            let a = rng.below(rows - len);
            (a, a + len)
        })
        .collect()
}

const LENGTHS: [(u64, u64, &str); 3] = [(5, 256, "5-255"), (250, 2_251, "250-2250"), (2_000, 22_001, "2000-22000")];

fn random_windows(c: &mut Criterion) {
    let mut rng = Rng(1);
    let col = PackedColumn::of(&mut rng, 8_192, 11); // 8.4M values
    let mut group = c.benchmark_group("random_window_sum_w11");
    for (lo, hi, name) in LENGTHS {
        let ws = windows(&mut rng, col.rows(), lo, hi, 4_096);
        group.bench_with_input(BenchmarkId::new("cumulative_plus_edges", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(summary::moments(&col, a, e).sum)
            })
        });
        group.bench_with_input(BenchmarkId::new("raw_u32_scan", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(col.raw[a as usize..e as usize].iter().map(|&x| x as i64).sum::<i64>())
            })
        });
    }
    group.finish();
}

const RANGES: [(u64, u64, &str); 3] = [(1, 33, "1-32"), (32, 1_025, "32-1024"), (1_024, 65_537, "1024-65536")];

fn range_ops(c: &mut Criterion) {
    let mut rng = Rng(2);
    let col = PackedColumn::of(&mut rng, 8_192, 11);
    let bar_cols: Vec<PackedColumn> = (0..4).map(|_| PackedColumn::of(&mut rng, 8_192, 11)).collect();
    let mut group = c.benchmark_group("range_w11");
    for (lo, hi, name) in RANGES {
        let ws = windows(&mut rng, col.rows(), lo, hi, 2_048);
        group.bench_with_input(BenchmarkId::new("series_sum_packed", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(summary::moments(&col, a, e).sum)
            })
        });
        group.bench_with_input(BenchmarkId::new("series_sum_raw_u32", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(col.raw[a as usize..e as usize].iter().map(|&x| x as i64).sum::<i64>())
            })
        });
        group.bench_with_input(BenchmarkId::new("range_max_packed", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(summary::extremes(&col, a, e).map(|x| x.2))
            })
        });
        group.bench_with_input(BenchmarkId::new("range_max_raw_u32", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(col.raw[a as usize..e as usize].iter().max().copied())
            })
        });
        group.bench_with_input(BenchmarkId::new("ohlcv_bar_packed", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(summary::bar(&bar_cols[0], &bar_cols[1], &bar_cols[2], &bar_cols[3], &col, a, e))
            })
        });
        group.bench_with_input(BenchmarkId::new("ohlcv_bar_raw_u32", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                let (a, e) = (a as usize, e as usize);
                let o = bar_cols[0].raw[a];
                let h = bar_cols[1].raw[a..e].iter().max().copied();
                let l = bar_cols[2].raw[a..e].iter().min().copied();
                let cl = bar_cols[3].raw[e - 1];
                let v = col.raw[a..e].iter().map(|&x| x as i64).sum::<i64>();
                black_box((o, h, l, cl, v))
            })
        });
    }
    group.finish();
}

const MEDIAN_LENGTHS: [(u64, u64, &str); 3] = [(20, 301, "20-300"), (300, 3_001, "300-3000"), (3_000, 30_001, "3000-30000")];

fn order_statistics(c: &mut Criterion) {
    let mut rng = Rng(3);
    // A price-like column: 2^14 distinct values over 4M rows.
    let values: Vec<i64> = (0..4_194_304).map(|_| rng.below(1 << 14) as i64).collect();
    let matrix = WaveletMatrix::build(&values);
    let mut group = c.benchmark_group("order_statistics");
    for (lo, hi, name) in MEDIAN_LENGTHS {
        let ws = windows(&mut rng, values.len() as u64, lo, hi, 2_048);
        group.bench_with_input(BenchmarkId::new("median_wavelet", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(matrix.median_low(a as usize, e as usize))
            })
        });
        group.bench_with_input(BenchmarkId::new("median_copy_select", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                let mut v = values[a as usize..e as usize].to_vec();
                let k = (v.len() - 1) / 2;
                black_box(*v.select_nth_unstable(k).1)
            })
        });
        group.bench_with_input(BenchmarkId::new("count_below_wavelet", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(matrix.count_below(a as usize, e as usize, 8_000))
            })
        });
        group.bench_with_input(BenchmarkId::new("count_below_scan", name), &ws, |b, ws| {
            let mut i = 0usize;
            b.iter(|| {
                let (a, e) = ws[i % ws.len()];
                i += 1;
                black_box(values[a as usize..e as usize].iter().filter(|&&x| x < 8_000).count())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, decode_and_sum, random_windows, range_ops, order_statistics);
criterion_main!(benches);
