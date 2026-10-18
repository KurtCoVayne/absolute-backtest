use std::hint::black_box;
use std::time::Instant;
const BLOCK: usize = 1024;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn unif(&mut self) -> f64 { (self.next() >> 11) as f64 / (1u64 << 53) as f64 }
    fn norm(&mut self) -> f64 { let (u, v) = (self.unif().max(1e-300), self.unif()); (-2.0 * u.ln()).sqrt() * (6.283185307179586 * v).cos() }
}
fn bits(x: u64) -> u32 { 64 - x.leading_zeros() }

/// FOR + bit-packed block of 1024 values: 16 groups of 64 values, each group W words.
struct Packed { base: i64, width: u32, words: Vec<u64> }
fn pack(vals: &[i64]) -> Packed {
    let base = *vals.iter().min().unwrap();
    let width = bits(vals.iter().map(|v| (v - base) as u64).max().unwrap()).max(1);
    let w = width as usize; let mut words = vec![0u64; BLOCK / 64 * w];
    for (i, v) in vals.iter().enumerate() {
        let x = (v - base) as u64; let (g, j) = (i / 64, i % 64); let p = j * w; let (wi, o) = (g * w + p / 64, p % 64);
        words[wi] |= x << o; if o + w > 64 { words[wi + 1] |= x >> (64 - o); }
    }
    Packed { base, width, words }
}
/// Unpack 64 values from W words; W is a constant, so the 64-step loop unrolls and every shift is folded.
#[inline(always)]
fn unpack64<const W: usize>(src: &[u64], base: i64, out: &mut [i64]) {
    let mask: u64 = if W == 64 { u64::MAX } else { (1u64 << W) - 1 };
    for j in 0..64 {
        let p = j * W; let (wi, o) = (p / 64, p % 64);
        let mut x = src[wi] >> o;
        if o + W > 64 { x |= src[wi + 1] << (64 - o); }
        out[j] = (x & mask) as i64 + base;
    }
}
fn unpack_w<const W: usize>(p: &Packed, out: &mut [i64]) {
    for g in 0..BLOCK / 64 { unpack64::<W>(&p.words[g * W..(g + 1) * W], p.base, &mut out[g * 64..(g + 1) * 64]); }
}
macro_rules! dispatch { ($p:expr, $out:expr, $($w:literal)*) => { match $p.width as usize { $($w => unpack_w::<$w>($p, $out),)* _ => unreachable!() } } }
fn unpack(p: &Packed, out: &mut [i64]) { dispatch!(p, out, 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40) }

fn alp_encode(xs: &[f32]) -> Option<(i32, Vec<i64>)> {
    'e: for e in 0..=6i32 { let s = 10f64.powi(e); let mut out = Vec::with_capacity(xs.len());
        for &x in xs { let i = (x as f64 * s).round() as i64; if ((i as f64) / s) as f32 != x { continue 'e; } out.push(i); }
        return Some((e, out)); }
    None
}
fn time<F: FnMut()>(reps: usize, n: usize, mut f: F) -> f64 {
    let mut best = f64::MAX; for _ in 0..reps { let t = Instant::now(); f(); best = best.min(t.elapsed().as_nanos() as f64 / n as f64); } best
}

fn main() {
    let (ninst, nbars) = (4000usize, 4096usize); let n = ninst * nbars;
    let mut rng = Rng(0x9e3779b97f4a7c15); let mut px = Vec::with_capacity(n); let mut vol = Vec::with_capacity(n);
    for _ in 0..ninst {
        let mut c = (rng.unif() * 30000.0 + 300.0).round(); let adv = (rng.unif() * 6.0 + 11.0).exp();
        for _ in 0..nbars { c = (c * (1.0 + 0.02 * rng.norm())).round().max(1.0); px.push((c / 100.0) as f32); vol.push((adv * (0.6 * rng.norm()).exp()).round() as i64); }
    }
    let alp: Vec<(i32, Packed)> = px.chunks(BLOCK).map(|c| { let (e, iv) = alp_encode(c).unwrap(); (e, pack(&iv)) }).collect();
    let dpk: Vec<(i64, i32, Packed)> = px.chunks(BLOCK).map(|c| { let (e, iv) = alp_encode(c).unwrap(); let d: Vec<i64> = std::iter::once(0).chain(iv.windows(2).map(|w| w[1] - w[0])).collect(); (iv[0], e, pack(&d)) }).collect();
    let vpk: Vec<Packed> = vol.chunks(BLOCK).map(pack).collect();
    let bytes = |ps: &mut dyn Iterator<Item = &Packed>| ps.map(|p| p.words.len() * 8 + 12).sum::<usize>() as f64 / n as f64;
    let (b_alp, b_delta, b_vol) = (bytes(&mut alp.iter().map(|x| &x.1)), bytes(&mut dpk.iter().map(|x| &x.2)), bytes(&mut vpk.iter()));

    let mut f64buf = vec![0f64; n]; let mut f32buf = vec![0f32; n]; let mut ibuf = vec![0i64; n]; let mut tmp = vec![0i64; BLOCK];
    let reps = 7;
    // Decode to a buffer (what the kernel does: a block becomes an Arrow array).
    let t_raw = time(reps, n, || { for (o, x) in f64buf.iter_mut().zip(&px) { *o = *x as f64; } black_box(&f64buf); });
    let t_int = time(reps, n, || { for (k, (_, p)) in alp.iter().enumerate() { unpack(p, &mut ibuf[k * BLOCK..(k + 1) * BLOCK]); } black_box(&ibuf); });
    let t_alp = time(reps, n, || { for (k, (e, p)) in alp.iter().enumerate() { unpack(p, &mut tmp); let s = 10f64.powi(*e);
        for (o, i) in f32buf[k * BLOCK..(k + 1) * BLOCK].iter_mut().zip(&tmp) { *o = ((*i as f64) / s) as f32; } } black_box(&f32buf); });
    let t_delta = time(reps, n, || { for (k, (first, _, p)) in dpk.iter().enumerate() { unpack(p, &mut tmp); let mut acc = *first;
        for (o, d) in ibuf[k * BLOCK..(k + 1) * BLOCK].iter_mut().zip(&tmp) { acc += d; *o = acc; } } black_box(&ibuf); });
    let t_vol = time(reps, n, || { for (k, p) in vpk.iter().enumerate() { unpack(p, &mut ibuf[k * BLOCK..(k + 1) * BLOCK]); } black_box(&ibuf); });
    // Exactness of the f32 path.
    let ok = f32buf.iter().zip(&px).all(|(a, b)| a.to_bits() == b.to_bits());
    // Reductions: the canonical-order f64 sum (one accumulator, required for bit identity) vs an exact integer sum (order-free, vectorises).
    let t_fsum = time(reps, n, || { let mut s = 0f64; for x in &f64buf { s += x; } black_box(s); });
    let t_isum = time(reps, n, || { let mut s = 0i128; for c in ibuf.chunks(BLOCK) { let b: i64 = c.iter().sum(); s += b as i128; } black_box(s); });
    let t_isq = time(reps, n, || { let mut s = 0i128; for c in ibuf.chunks(BLOCK) { let mut b = 0i128; for &x in c { b += (x as i128) * (x as i128); } s += b; } black_box(s); });
    let t_fsq = time(reps, n, || { let mut s = 0f64; for x in &f64buf { s += x * x; } black_box(s); });

    // Fused: decode one block into an L1-resident buffer and consume it at once (the kernel's access pattern).
    let f64col: Vec<f64> = px.iter().map(|x| *x as f64).collect();
    let t_f64_scan = time(reps, n, || { let mut s = 0f64; for x in &f64col { s += x; } black_box(s); });
    let t_f32_scan = time(reps, n, || { let mut s = 0f64; for x in &px { s += *x as f64; } black_box(s); });
    let t_alp_fused = time(reps, n, || { let mut s = 0i128; for (_, p) in &alp { unpack(p, &mut tmp); let b: i64 = tmp.iter().sum(); s += b as i128; } black_box(s); });
    let t_delta_fused = time(reps, n, || { let mut s = 0i128; for (first, _, p) in &dpk { unpack(p, &mut tmp); let mut acc = *first; let mut b = 0i64; for d in &tmp { acc += d; b += acc; } s += b as i128; } black_box(s); });
    let t_vol_raw = time(reps, n, || { let mut s = 0i128; for c in vol.chunks(BLOCK) { let b: i64 = c.iter().sum(); s += b as i128; } black_box(s); });
    let t_vol_fused = time(reps, n, || { let mut s = 0i128; for p in &vpk { unpack(p, &mut tmp); let b: i64 = tmp.iter().sum(); s += b as i128; } black_box(s); });
    println!("{:<44} {:>8} {:>10}", "scan + sum, fused (read from memory)", "B/value", "ns/value");
    println!("{:<44} {:>8.2} {:>10.3}", "f64 column, canonical-order sum", 8.0, t_f64_scan);
    println!("{:<44} {:>8.2} {:>10.3}", "f32 column, widen, canonical-order sum", 4.0, t_f32_scan);
    println!("{:<44} {:>8.2} {:>10.3}", "ALP+FOR block -> L1 -> exact i64 sum", b_alp, t_alp_fused);
    println!("{:<44} {:>8.2} {:>10.3}", "ALP+delta block -> L1 -> exact i64 sum", b_delta, t_delta_fused);
    println!("{:<44} {:>8.2} {:>10.3}", "volume i64 column, exact sum", 8.0, t_vol_raw);
    println!("{:<44} {:>8.2} {:>10.3}", "volume FOR block -> L1 -> exact sum", b_vol, t_vol_fused);
    let med = |w: Vec<u32>| { let mut w = w; w.sort(); w[w.len() / 2] };
    println!("{} values; ALP round-trip exact on every value: {}", n, ok);
    println!("{:<44} {:>8} {:>10} {:>7}", "decode into a buffer", "B/value", "ns/value", "width");
    println!("{:<44} {:>8.2} {:>10.3} {:>7}", "f32 raw -> f64", 4.0, t_raw, "-");
    println!("{:<44} {:>8.2} {:>10.3} {:>7}", "ALP+FOR+bitpack -> i64 ticks", b_alp, t_int, med(alp.iter().map(|x| x.1.width).collect()));
    println!("{:<44} {:>8.2} {:>10.3} {:>7}", "ALP+FOR+bitpack -> exact f32 bits", b_alp, t_alp, "");
    println!("{:<44} {:>8.2} {:>10.3} {:>7}", "ALP+delta+bitpack -> i64 ticks", b_delta, t_delta, med(dpk.iter().map(|x| x.2.width).collect()));
    println!("{:<44} {:>8.2} {:>10.3} {:>7}", "volume FOR+bitpack -> i64 (raw is 8 B)", b_vol, t_vol, med(vpk.iter().map(|p| p.width).collect()));
    println!("{:<44} {:>8} {:>10}", "reduction over the decoded column", "", "ns/value");
    println!("{:<44} {:>8} {:>10.3}", "sum f64, canonical order (1 accumulator)", "", t_fsum);
    println!("{:<44} {:>8} {:>10.3}", "sum i64 ticks, exact (any order)", "", t_isum);
    println!("{:<44} {:>8} {:>10.3}", "sum of squares f64, canonical order", "", t_fsq);
    println!("{:<44} {:>8} {:>10.3}", "sum of squares i128, exact", "", t_isq);
}
