//! Interleaved ("transposed", FastLanes-style) bit-packing: a block of 1024 values is 32 lanes of
//! 32 values; value i of every lane sits at the same bit offset, so unpacking is one vector shift
//! across lanes. Compared with raw scans at the bit widths measured on the real columns.
use std::hint::black_box;
use std::time::Instant;
const N: usize = 1024; const L: usize = 32; const PER: usize = N / L;

fn pack<const W: usize>(v: &[u32; N]) -> Vec<u32> {
    let mut out = vec![0u32; W * L];
    for l in 0..L { for i in 0..PER {
        let x = v[i * L + l] as u64; let p = i * W; let (w, o) = (p / 32, p % 32);
        out[w * L + l] |= (x << o) as u32; if o + W > 32 { out[(w + 1) * L + l] |= (x >> (32 - o)) as u32; }
    } }
    out
}
#[inline(always)]
fn unpack<const W: usize>(p: &[u32], base: u32, out: &mut [u32; N]) {
    let mask: u32 = if W == 32 { u32::MAX } else { (1u32 << W) - 1 };
    for i in 0..PER {
        let bit = i * W; let (w, o) = (bit / 32, bit % 32);
        let (lo, out_row) = (&p[w * L..w * L + L], &mut out[i * L..i * L + L]);
        if o + W > 32 { let hi = &p[(w + 1) * L..(w + 1) * L + L];
            for l in 0..L { out_row[l] = (((lo[l] >> o) | (hi[l] << (32 - o))) & mask).wrapping_add(base); }
        } else { for l in 0..L { out_row[l] = ((lo[l] >> o) & mask).wrapping_add(base); } }
    }
}
#[inline(always)]
fn unpack_rows<const W: usize>(p: &[u32], rows: usize, out: &mut [u32; N]) {
    let mask: u32 = if W == 32 { u32::MAX } else { (1u32 << W) - 1 };
    for i in 0..rows {
        let bit = i * W; let (w, o) = (bit / 32, bit % 32);
        if o + W > 32 { for l in 0..L { out[i * L + l] = ((p[w * L + l] >> o) | (p[(w + 1) * L + l] << (32 - o))) & mask; } }
        else { for l in 0..L { out[i * L + l] = (p[w * L + l] >> o) & mask; } }
    }
}
fn best<F: FnMut()>(mut f: F) -> f64 { let mut b = f64::MAX; for _ in 0..9 { let t = Instant::now(); f(); b = b.min(t.elapsed().as_secs_f64()); } b }

fn bench<const W: usize>(nblocks: usize) {
    let mut s = 0x9e3779b97f4a7c15u64;
    let mut raw: Vec<u32> = Vec::with_capacity(nblocks * N);
    let blocks: Vec<Vec<u32>> = (0..nblocks).map(|_| {
        let mut b = [0u32; N];
        for x in b.iter_mut() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; *x = (s as u32) & (((1u64 << W) - 1) as u32); }
        raw.extend_from_slice(&b); pack::<W>(&b) }).collect();
    let n = nblocks * N;
    // Correctness.
    let mut out = [0u32; N];
    for (k, b) in blocks.iter().enumerate() { unpack::<W>(b, 0, &mut out); assert_eq!(&out[..], &raw[k * N..(k + 1) * N]); }
    let raw64: Vec<i64> = raw.iter().map(|x| *x as i64).collect();
    let rawf: Vec<f64> = raw.iter().map(|x| *x as f64).collect();
    let t_packed = best(|| { let mut acc = 0u64; for b in &blocks { unpack::<W>(b, 7, &mut out); let mut s = 0u64; for x in out.iter() { s += *x as u64; } acc = acc.wrapping_add(s); } black_box(acc); });
    let t_u32 = best(|| { let mut acc = 0u64; for x in &raw { acc += *x as u64; } black_box(acc); });
    let t_i64 = best(|| { let mut acc = 0i64; for x in &raw64 { acc += *x; } black_box(acc); });
    let t_f64 = best(|| { let mut acc = 0f64; for x in &rawf { acc += *x; } black_box(acc); });
    let ns = |t: f64| t * 1e9 / n as f64;
    println!("width {:>2}: {:>5.2} B/v packed {:.3} ns/v | raw u32 (4 B) {:.3} | raw i64 (8 B) {:.3} | raw f64 canonical sum (8 B) {:.3}",
        W, W as f64 / 8.0, ns(t_packed), ns(t_u32), ns(t_i64), ns(t_f64));
}
fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } }
/// Encode cost: min, GCD, width, pack (what a build or a sealing append pays per block).
fn encode_cost<const W: usize>(nblocks: usize) -> f64 {
    let mut s = 0x1234_5678_9abc_def1u64;
    let vals: Vec<[u32; N]> = (0..nblocks).map(|_| { let mut b = [0u32; N]; for x in b.iter_mut() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; *x = 1_000_000 + ((s as u32) & (((1u64 << W) - 1) as u32)) * 25; } b }).collect();
    let t = best(|| { let mut tot = 0usize; for b in &vals {
        let mn = *b.iter().min().unwrap(); let g = b.iter().fold(0u64, |g, x| gcd(g, (x - mn) as u64)).max(1) as u32;
        let mut off = [0u32; N]; for i in 0..N { off[i] = (b[i] - mn) / g; }
        let w = 32 - off.iter().max().unwrap().leading_zeros(); assert!(w as usize <= W);
        tot += pack::<W>(&off).len(); } black_box(tot); });
    t * 1e9 / (nblocks * N) as f64
}
/// Random windows from cumulative block sums + the two edge blocks decoded.
fn window_cost<const W: usize>(nblocks: usize, lo: usize, span: usize) -> (f64, f64) {
    let mut s = 0x9e3779b97f4a7c15u64;
    let mut raw: Vec<u32> = Vec::with_capacity(nblocks * N);
    let blocks: Vec<Vec<u32>> = (0..nblocks).map(|_| { let mut b = [0u32; N]; for x in b.iter_mut() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; *x = (s as u32) & (((1u64 << W) - 1) as u32); } raw.extend_from_slice(&b); pack::<W>(&b) }).collect();
    let mut cum = vec![0u64; nblocks + 1]; for k in 0..nblocks { cum[k + 1] = cum[k] + raw[k * N..(k + 1) * N].iter().map(|x| *x as u64).sum::<u64>(); }
    let n = nblocks * N; let q = 200_000usize;
    let qs: Vec<(usize, usize)> = (0..q).map(|_| { s ^= s << 13; s ^= s >> 7; s ^= s << 17; let len = lo + (s % span as u64) as usize; let a = (s >> 20) as usize % (n - len); (a, a + len) }).collect();
    let mut out = [0u32; N];
    // Sum of raw[a..b): whole blocks from the cumulative array, the partial edges by decoding.
    // Prefix of a block up to `upto` values: decode the shorter side only (block total from the cumulative array).
    let bsum = |k: usize| cum[k + 1] - cum[k];
    let mut prefix_in = |k: usize, upto: usize, out: &mut [u32; N]| -> u64 {
        if upto == 0 { return 0; }
        if upto <= N / 2 { unpack_rows::<W>(&blocks[k], (upto + L - 1) / L, out); out[..upto].iter().map(|x| *x as u64).sum() }
        else { unpack::<W>(&blocks[k], 0, out); bsum(k) - out[upto..].iter().map(|x| *x as u64).sum::<u64>() } };
    let mut check = 0u64; for &(a, b) in qs.iter().take(1000) { let (ka, kb) = (a / N, b / N);
        let sa = cum[ka] + prefix_in(ka, a % N, &mut out); let sb = cum[kb] + if kb < nblocks { prefix_in(kb, b % N, &mut out) } else { 0 };
        assert_eq!(sb - sa, raw[a..b].iter().map(|x| *x as u64).sum::<u64>()); check += sb - sa; }
    black_box(check);
    let t_cum = best(|| { let mut acc = 0u64; for &(a, b) in &qs { let (ka, kb) = (a / N, b / N);
        let sa = cum[ka] + prefix_in(ka, a % N, &mut out); let sb = cum[kb] + if kb < nblocks { prefix_in(kb, b % N, &mut out) } else { 0 }; acc = acc.wrapping_add(sb - sa); } black_box(acc); });
    let t_scan = best(|| { let mut acc = 0u64; for &(a, b) in &qs[..q / 10] { acc = acc.wrapping_add(raw[a..b].iter().map(|x| *x as u64).sum::<u64>()); } black_box(acc); });
    (t_cum * 1e9 / q as f64, t_scan * 1e9 / (q / 10) as f64)
}
fn main() {
    let nb = 32_768; // 33.5M values per width: far larger than cache, like a real scan
    println!("{} values per test, sum of the decoded column (exact integer), best of 9", nb * N);
    if std::env::args().nth(1).as_deref() != Some("extra") { bench::<5>(nb); bench::<8>(nb); bench::<11>(nb); bench::<14>(nb); bench::<20>(nb); bench::<27>(nb); return; }
    println!("encode (min, GCD, width, pack): w8 {:.2} ns/v, w11 {:.2} ns/v, w20 {:.2} ns/v", encode_cost::<8>(4096), encode_cost::<11>(4096), encode_cost::<20>(4096));
    for (lo, span) in [(5usize, 250usize), (250, 2000), (2000, 20000)] {
        let (c, sc) = window_cost::<11>(nb, lo, span);
        println!("random window sum, length {}..{}, width 11: cumulative block sums + shorter-side edge decode {:.0} ns/query; scanning raw u32 {:.0} ns/query", lo, lo + span, c, sc);
    }
}
