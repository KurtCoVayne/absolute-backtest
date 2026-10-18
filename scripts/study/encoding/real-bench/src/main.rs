//! Decode throughput on the real columns: per block base + GCD quantum + bit-packed offsets,
//! decoded into an L1 buffer and consumed at once; exact f32 reconstruction where the column
//! came from f32; baselines are scans of the raw f32 / f64 columns.
use std::hint::black_box;
use std::time::Instant;

fn read_i64(p: &str) -> Vec<i64> { std::fs::read(p).unwrap().chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect() }
fn read_f32(p: &str) -> Option<Vec<f32>> { std::fs::read(p).ok().map(|b| b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()) }
fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } }
fn bits(x: u64) -> u32 { 64 - x.leading_zeros() }

struct Block { base: i64, q: i64, width: u32, len: usize, words: Vec<u64>, exp: i32 }

fn pack(vals: &[i64], exp: i32) -> Block {
    let base = *vals.iter().min().unwrap();
    let q = vals.iter().fold(0u64, |g, v| gcd(g, (v - base) as u64)).max(1) as i64;
    let offs: Vec<u64> = vals.iter().map(|v| ((v - base) / q) as u64).collect();
    let width = bits(*offs.iter().max().unwrap()).max(1);
    let w = width as usize; let mut words = vec![0u64; (vals.len() * w + 63) / 64 + 1];
    for (i, x) in offs.iter().enumerate() { let p = i * w; let (wi, o) = (p / 64, p % 64);
        words[wi] |= x << o; if o + w > 64 { words[wi + 1] |= x >> (64 - o); } }
    Block { base, q, width, len: vals.len(), words, exp }
}
#[inline(always)]
fn unpack_w<const W: usize>(b: &Block, out: &mut [i64]) {
    let mask = if W == 64 { u64::MAX } else { (1u64 << W) - 1 };
    for i in 0..b.len { let p = i * W; let (wi, o) = (p / 64, p % 64);
        let mut x = b.words[wi] >> o; if o + W > 64 { x |= b.words[wi + 1] << (64 - o); }
        out[i] = (x & mask) as i64 * b.q + b.base; }
}
macro_rules! dispatch { ($b:expr, $o:expr, $($w:literal)*) => { match $b.width as usize { $($w => unpack_w::<$w>($b, $o),)* _ => unreachable!() } } }
fn unpack(b: &Block, o: &mut [i64]) { dispatch!(b, o, 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48) }

fn best<F: FnMut()>(n: usize, mut f: F) -> f64 { let mut b = f64::MAX; for _ in 0..9 { let t = Instant::now(); f(); b = b.min(t.elapsed().as_nanos() as f64 / n as f64); } b }

fn run(name: &str, stem: &str, reblock: bool) {
    let ints = read_i64(&format!("{stem}.i64")); let n = ints.len();
    let mut starts: Vec<usize> = read_i64(&format!("{stem}.starts")).into_iter().map(|x| x as usize).collect();
    let exps: Option<Vec<i64>> = std::fs::read(format!("{stem}.exp")).ok().map(|_| read_i64(&format!("{stem}.exp")));
    let f32s = read_f32(&format!("{stem}.f32"));
    if reblock { starts = (0..n).step_by(1024).collect(); }
    starts.push(n);
    let blocks: Vec<Block> = starts.windows(2).enumerate().map(|(k, w)| pack(&ints[w[0]..w[1]], if reblock { 0 } else { exps.as_ref().map(|e| e[k] as i32).unwrap_or(0) })).collect();
    let bytes: usize = blocks.iter().map(|b| (b.len * b.width as usize + 7) / 8 + 16).sum();
    let mut tmp = vec![0i64; 1024]; let reps = (20_000_000 / n).max(1);
    let t_int = best(n * reps, || { let mut s = 0i128; for _ in 0..reps { for b in &blocks { unpack(b, &mut tmp[..b.len]); let x: i64 = tmp[..b.len].iter().sum(); s += x as i128; } } black_box(s); });
    let mut line = format!("{name:<26} n={n:>8}  packed {:.2} B/v  med width {:>2}  | fused decode+exact sum {:.2} ns/v", bytes as f64 / n as f64,
        { let mut w: Vec<u32> = blocks.iter().map(|b| b.width).collect(); w.sort(); w[w.len() / 2] }, t_int);
    if let (Some(fs), false) = (&f32s, reblock) {
        // Exact f32 reconstruction: i / 10^e, correctly rounded, then to f32; checked against the source bits.
        let mut out = vec![0f32; n]; let mut k = 0;
        for b in &blocks { unpack(b, &mut tmp[..b.len]); let s = 10f64.powi(b.exp); for i in 0..b.len { out[k] = ((tmp[i] as f64) / s) as f32; k += 1; } }
        let exact = out.iter().zip(fs).all(|(a, b)| a.to_bits() == b.to_bits());
        let t_f32 = best(n * reps, || { let mut acc = 0f64; for _ in 0..reps { for b in &blocks { unpack(b, &mut tmp[..b.len]); let s = 10f64.powi(b.exp); for i in 0..b.len { acc += (((tmp[i] as f64) / s) as f32) as f64; } } } black_box(acc); });
        let t_raw = best(n * reps, || { let mut acc = 0f64; for _ in 0..reps { for x in fs { acc += *x as f64; } } black_box(acc); });
        line += &format!(" | decode->exact f32 + seq sum {:.2} ns/v (bits equal: {exact}) | raw f32 seq sum {:.2} ns/v", t_f32, t_raw);
    }
    println!("{line}");
}

fn main() {
    let d = std::env::args().nth(1).unwrap();
    run("ndlake 1995 close", &format!("{d}/bench_nd1995_close"), false);
    run("ndlake 2024 close", &format!("{d}/bench_nd2024_close"), false);
    run("ndlake 2024 volume", &format!("{d}/bench_nd2024_volume"), false);
    run("databento ES close (ticks)", &format!("{d}/bench_dbn_ES_close"), false);
    run("databento CL close", &format!("{d}/bench_dbn_CL_close"), false);
    run("orats cBidPx (1024 blocks)", &format!("{d}/bench_orats_cBidPx"), true);
    run("orats cMidIv (1024 blocks)", &format!("{d}/bench_orats_cMidIv"), true);
    run("orats cOi (1024 blocks)", &format!("{d}/bench_orats_cOi"), true);
}
