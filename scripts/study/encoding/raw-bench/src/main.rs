//! Uncompressed, decode-free layouts on the real NDLake 2024 closes (instrument-contiguous):
//! exact integer scan, canonical f64 scan, and rolling means by sliding state vs by prefix sums,
//! plus random window queries (what a parameter grid or the query layer asks).
use std::hint::black_box;
use std::time::Instant;
fn read_i64(p: &str) -> Vec<i64> { std::fs::read(p).unwrap().chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect() }
fn best<F: FnMut()>(mut f: F) -> f64 { let mut b = f64::MAX; for _ in 0..7 { let t = Instant::now(); f(); b = b.min(t.elapsed().as_secs_f64()); } b }
fn main() {
    let d = std::env::args().nth(1).unwrap();
    // Scale every block to one exponent (6) so the column is one integer type: what the resident format holds.
    let ints = read_i64(&format!("{d}/bench_nd2024_close.i64"));
    let exps = read_i64(&format!("{d}/bench_nd2024_close.exp"));
    let mut starts: Vec<usize> = read_i64(&format!("{d}/bench_nd2024_close.starts")).into_iter().map(|x| x as usize).collect();
    let n = ints.len(); starts.push(n);
    let mut t = vec![0i64; n];
    for (k, w) in starts.windows(2).enumerate() { let m = 10i64.pow((6 - exps[k]) as u32); for i in w[0]..w[1] { t[i] = ints[i] * m; } }
    let f: Vec<f64> = t.iter().map(|x| *x as f64 / 1e6).collect();
    // Instrument boundaries (blocks never straddle instruments; consecutive blocks may share one, fine for the timing).
    let ns = |s: f64| s * 1e9 / n as f64;
    let t_isum = best(|| { let mut acc = 0i128; for c in t.chunks(4096) { let s: i64 = c.iter().sum(); acc += s as i128; } black_box(acc); });
    let t_fsum = best(|| { let mut acc = 0f64; for x in &f { acc += x; } black_box(acc); });
    // Prefix sums (exact): built once at ingest.
    let mut p = vec![0i64; n + 1]; for i in 0..n { p[i + 1] = p[i] + t[i]; }
    let mut p2 = vec![0i128; n + 1]; for i in 0..n { p2[i + 1] = p2[i] + (t[i] as i128) * (t[i] as i128); }
    let w = 252usize;
    let mut out = vec![0f64; n];
    let t_var_prefix = best(|| { for i in (252 - 1)..n { let s = (p[i + 1] - p[i + 1 - 252]) as i128; let q = p2[i + 1] - p2[i + 1 - 252]; out[i] = ((252 * q - s * s) as f64) / (252.0 * 251.0 * 1e12); } black_box(&out); });
    let t_slide = best(|| { let mut s = 0i64; for i in 0..n { s += t[i]; if i >= w { s -= t[i - w]; } out[i] = if i + 1 >= w { s as f64 / (w as f64 * 1e6) } else { f64::NAN }; } black_box(&out); });
    let t_prefix = best(|| { for i in (w - 1)..n { out[i] = (p[i + 1] - p[i + 1 - w]) as f64 / (w as f64 * 1e6); } black_box(&out); });
    // Random window queries: (end, length) pairs, as a grid over lookbacks or an ad-hoc question asks.
    let mut rng = 0x9e3779b97f4a7c15u64; let q = 2_000_000usize;
    let qs: Vec<(usize, usize)> = (0..q).map(|_| { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; let len = 5 + (rng % 500) as usize; let end = len + (rng >> 20) as usize % (n - len); (end, len) }).collect();
    let t_rand_prefix = best(|| { let mut acc = 0f64; for &(e, l) in &qs { acc += (p[e] - p[e - l]) as f64 / l as f64; } black_box(acc); });
    let t_rand_scan = best(|| { let mut acc = 0f64; for &(e, l) in &qs[..q / 20] { let s: i64 = t[e - l..e].iter().sum(); acc += s as f64 / l as f64; } black_box(acc); });
    println!("NDLake 2024 equity closes, {} values, resident uncompressed (i64 ticks + f64 values + i64 prefix sums, i128 for squares)", n);
    println!("  exact i64 scan (order-free, vectorised)      {:.3} ns/value", ns(t_isum));
    println!("  f64 scan in canonical order (two_pass mode)   {:.3} ns/value", ns(t_fsum));
    println!("  rolling 252-row mean, sliding state            {:.3} ns/output", ns(t_slide));
    println!("  rolling 252-row mean, prefix-sum difference    {:.3} ns/output", ns(t_prefix));
    println!("  rolling 252-row variance, exact (i128 squares)  {:.3} ns/output", ns(t_var_prefix));
    println!("  random window mean, prefix sums                {:.1} ns/query", t_rand_prefix * 1e9 / q as f64);
    println!("  random window mean, scanning the window        {:.1} ns/query (mean length 255)", t_rand_scan * 1e9 / (q / 20) as f64);
}
