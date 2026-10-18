//! The codec's figures of ST-2.4 (decode and sum per width against raw
//! scans; random windows; the range operations of ST-6), to run with
//! `cargo bench -p abt-codec` and record with the machine's name.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use abt_codec::block::{extract, pack, unpack};
use abt_codec::BLOCK;

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

criterion_group!(benches, decode_and_sum);
criterion_main!(benches);
