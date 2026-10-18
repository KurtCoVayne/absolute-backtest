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
// ---------------------------------------------------------------------------------------------
// Range queries: rows [r0, r1) of a block decoded directly (offsets are random access), single
// values extracted, range max through a sparse table over block maxima, an OHLCV bar over an
// arbitrary row range, and range quantiles / range counts through a wavelet matrix.
#[inline(always)]
fn unpack_range<const W: usize>(p: &[u32], r0: usize, r1: usize, out: &mut [u32; N]) {
    let mask: u32 = if W == 32 { u32::MAX } else { (1u32 << W) - 1 };
    for i in r0..r1 {
        let bit = i * W; let (w, o) = (bit / 32, bit % 32);
        if o + W > 32 { for l in 0..L { out[i * L + l] = ((p[w * L + l] >> o) | (p[(w + 1) * L + l] << (32 - o))) & mask; } }
        else { for l in 0..L { out[i * L + l] = (p[w * L + l] >> o) & mask; } }
    }
}
#[inline(always)]
fn extract<const W: usize>(p: &[u32], j: usize) -> u32 {
    let mask: u32 = if W == 32 { u32::MAX } else { (1u32 << W) - 1 };
    let (l, i) = (j % L, j / L); let bit = i * W; let (w, o) = (bit / 32, bit % 32);
    if o + W > 32 { ((p[w * L + l] >> o) | (p[(w + 1) * L + l] << (32 - o))) & mask } else { (p[w * L + l] >> o) & mask }
}
struct Col<const W: usize> { blocks: Vec<Vec<u32>>, raw: Vec<u32>, cum: Vec<u64>, bmax: Vec<u32>, bmin: Vec<u32>, smax: Vec<Vec<u32>>, smin: Vec<Vec<u32>> }
impl<const W: usize> Col<W> {
    fn new(nblocks: usize, seed: u64) -> Self {
        let mut s = seed; let mut raw = Vec::with_capacity(nblocks * N);
        let blocks: Vec<Vec<u32>> = (0..nblocks).map(|_| { let mut b = [0u32; N]; for x in b.iter_mut() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; *x = (s as u32) & (((1u64 << W) - 1) as u32); } raw.extend_from_slice(&b); pack::<W>(&b) }).collect();
        let mut cum = vec![0u64; nblocks + 1]; let (mut bmax, mut bmin) = (vec![0u32; nblocks], vec![0u32; nblocks]);
        for k in 0..nblocks { let b = &raw[k * N..(k + 1) * N]; cum[k + 1] = cum[k] + b.iter().map(|x| *x as u64).sum::<u64>(); bmax[k] = *b.iter().max().unwrap(); bmin[k] = *b.iter().min().unwrap(); }
        let build = |base: &Vec<u32>, f: fn(u32, u32) -> u32| { let mut t = vec![base.clone()]; let mut j = 1; while (1 << j) <= base.len() { let prev = &t[j - 1]; let h = 1 << (j - 1); let row: Vec<u32> = (0..base.len() - (1 << j) + 1).map(|k| f(prev[k], prev[k + h])).collect(); t.push(row); j += 1; } t };
        let smax = build(&bmax, |a, b| a.max(b)); let smin = build(&bmin, |a, b| a.min(b));
        Col { blocks, raw, cum, bmax, bmin, smax, smin }
    }
    fn n(&self) -> usize { self.raw.len() }
    /// Visit the decoded values of rows [a, b) block by block (the series read).
    #[inline(always)]
    fn for_range(&self, a: usize, b: usize, out: &mut [u32; N], mut f: impl FnMut(&[u32])) {
        let (mut k, kb) = (a / N, (b - 1) / N);
        while k <= kb { let lo = if k == a / N { a % N } else { 0 }; let hi = if k == kb { (b - 1) % N + 1 } else { N };
            // The fully unrolled decoder (constant shifts) for any block covered by a quarter or more;
            // the row-range decoder (runtime shifts) only for short edges.
            if hi - lo >= N / 4 { unpack::<W>(&self.blocks[k], 0, out); } else { unpack_range::<W>(&self.blocks[k], lo / L, (hi + L - 1) / L, out); }
            f(&out[lo..hi]); k += 1; }
    }
    fn sparse(t: &Vec<Vec<u32>>, k0: usize, k1: usize, f: fn(u32, u32) -> u32) -> u32 { let j = (usize::BITS - 1 - (k1 - k0).leading_zeros()) as usize; f(t[j][k0], t[j][k1 - (1 << j)]) }
    /// Range max (or min): whole blocks from the sparse table, the two edges decoded.
    fn range_ext(&self, a: usize, b: usize, out: &mut [u32; N], is_max: bool) -> u32 {
        let f: fn(u32, u32) -> u32 = if is_max { |x, y| x.max(y) } else { |x, y| x.min(y) };
        let (ka, kb) = (a / N, (b - 1) / N);
        let mut acc = if is_max { 0 } else { u32::MAX };
        // Slices reduced with inlined, vectorisable max/min (not through the function pointer).
        let mut red = |s: &[u32], acc: &mut u32| if is_max { *acc = (*acc).max(s.iter().copied().fold(0, u32::max)); } else { *acc = (*acc).min(s.iter().copied().fold(u32::MAX, u32::min)); };
        if kb <= ka + 1 { self.for_range(a, b, out, |s| red(s, &mut acc)); return acc; }
        let (fa, fb) = (if a % N == 0 { ka } else { ka + 1 }, if b % N == 0 { kb + 1 } else { kb });
        if fb > fa { acc = Self::sparse(if is_max { &self.smax } else { &self.smin }, fa, fb, f); }
        if a % N != 0 { self.for_range(a, fa * N, out, |s| red(s, &mut acc)); }
        if b % N != 0 { self.for_range(fb * N, b, out, |s| red(s, &mut acc)); }
        acc
    }
    /// Range sum: cumulative block sums, the edges decoded on the shorter side.
    fn range_sum(&self, a: usize, b: usize, out: &mut [u32; N]) -> u64 {
        let (ka, kb) = (a / N, (b - 1) / N);
        if kb <= ka + 1 { let mut s = 0u64; self.for_range(a, b, out, |v| for x in v { s += *x as u64; }); return s; }
        let pre = |k: usize, upto: usize, out: &mut [u32; N]| -> u64 { if upto == 0 { return 0; }
            if upto <= N / 2 { let mut s = 0u64; self.for_range(k * N, k * N + upto, out, |v| for x in v { s += *x as u64; }); s }
            else { let mut s = 0u64; self.for_range(k * N + upto, (k + 1) * N, out, |v| for x in v { s += *x as u64; }); (self.cum[k + 1] - self.cum[k]) - s } };
        let sb = self.cum[b / N] + if b % N != 0 { pre(b / N, b % N, out) } else { 0 };
        let sa = self.cum[ka] + pre(ka, a % N, out);
        sb - sa
    }
}
/// Wavelet matrix over W-bit values with a rank directory (cumulative count per 64-bit word).
struct Wm { levels: Vec<(Vec<u64>, Vec<u32>, usize)>, w: usize }
impl Wm {
    fn new(vals: &[u32], w: usize) -> Self {
        let n = vals.len(); let mut cur = vals.to_vec(); let mut levels = Vec::new();
        for lv in (0..w).rev() {
            let mut bits = vec![0u64; n / 64 + 1]; let (mut z, mut o) = (Vec::with_capacity(n), Vec::new());
            for (i, &x) in cur.iter().enumerate() { if (x >> lv) & 1 == 1 { bits[i / 64] |= 1 << (i % 64); o.push(x); } else { z.push(x); } }
            let mut cnt = vec![0u32; bits.len() + 1]; for i in 0..bits.len() { cnt[i + 1] = cnt[i] + bits[i].count_ones(); }
            let zeros = z.len(); z.extend_from_slice(&o); cur = z; levels.push((bits, cnt, zeros));
        }
        Wm { levels, w }
    }
    #[inline(always)] fn rank1(bits: &[u64], cnt: &[u32], i: usize) -> usize { (cnt[i / 64] + (bits[i / 64] & ((1u64 << (i % 64)) - 1)).count_ones()) as usize }
    /// k-th smallest (0-based) of rows [a, b).
    fn quantile(&self, mut a: usize, mut b: usize, mut k: usize) -> u32 {
        let mut r = 0u32;
        for (d, (bits, cnt, zeros)) in self.levels.iter().enumerate() {
            let (oa, ob) = (Self::rank1(bits, cnt, a), Self::rank1(bits, cnt, b)); let z = (b - a) - (ob - oa);
            if k < z { a -= oa; b -= ob; } else { k -= z; a = zeros + oa; b = zeros + ob; r |= 1 << (self.w - 1 - d); }
        }
        r
    }
    /// Number of values below c in rows [a, b).
    fn count_less(&self, mut a: usize, mut b: usize, c: u32) -> usize {
        let mut res = 0;
        for (d, (bits, cnt, zeros)) in self.levels.iter().enumerate() {
            let (oa, ob) = (Self::rank1(bits, cnt, a), Self::rank1(bits, cnt, b));
            if (c >> (self.w - 1 - d)) & 1 == 1 { res += (b - a) - (ob - oa); a = zeros + oa; b = zeros + ob; } else { a -= oa; b -= ob; }
        }
        res
    }
}
fn range_bench() {
    const W: usize = 11; let nb = 32_768;
    let (h, l, v) = (Col::<W>::new(nb, 0x1111_2222_3333_4445), Col::<W>::new(nb, 0x5555_6666_7777_8889), Col::<W>::new(nb, 0x9999_aaaa_bbbb_cccd));
    let n = h.n(); let mut out = [0u32; N]; let mut s = 0xdead_beef_cafe_f00du64;
    let mut rnd = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; s };
    let queries = |rnd: &mut dyn FnMut() -> u64, lo: usize, hi: usize, q: usize| -> Vec<(usize, usize)> { (0..q).map(|_| { let len = lo + (rnd() as usize) % (hi - lo); let a = (rnd() >> 7) as usize % (n - len); (a, a + len) }).collect() };
    // Point extraction.
    let pts: Vec<usize> = (0..1_000_000).map(|_| (rnd() >> 3) as usize % n).collect();
    for &j in pts.iter().take(10_000) { assert_eq!(extract::<W>(&h.blocks[j / N], j % N), h.raw[j]); }
    let t = best(|| { let mut acc = 0u64; for &j in &pts { acc += extract::<W>(&h.blocks[j / N], j % N) as u64; } black_box(acc); });
    let t_raw = best(|| { let mut acc = 0u64; for &j in &pts { acc += h.raw[j] as u64; } black_box(acc); });
    println!("point, random row of 33.5M: packed extract {:.1} ns | raw u32 load {:.1} ns", t * 1e9 / pts.len() as f64, t_raw * 1e9 / pts.len() as f64);
    for (lo, hi) in [(1usize, 32usize), (32, 1024), (1024, 65_536)] {
        let qs = queries(&mut rnd, lo, hi, 100_000); let avg = qs.iter().map(|(a, b)| b - a).sum::<usize>() as f64 / qs.len() as f64;
        for &(a, b) in qs.iter().take(2_000) {
            let mut sum = 0u64; h.for_range(a, b, &mut out, |x| for y in x { sum += *y as u64; }); assert_eq!(sum, h.raw[a..b].iter().map(|x| *x as u64).sum::<u64>());
            assert_eq!(h.range_ext(a, b, &mut out, true), *h.raw[a..b].iter().max().unwrap());
            assert_eq!(l.range_ext(a, b, &mut out, false), *l.raw[a..b].iter().min().unwrap());
            assert_eq!(v.range_sum(a, b, &mut out), v.raw[a..b].iter().map(|x| *x as u64).sum::<u64>());
        }
        let t_dec = best(|| { let mut acc = 0u64; for &(a, b) in &qs { h.for_range(a, b, &mut out, |x| for y in x { acc += *y as u64; }); } black_box(acc); });
        let t_raw = best(|| { let mut acc = 0u64; for &(a, b) in &qs { for y in &h.raw[a..b] { acc += *y as u64; } } black_box(acc); });
        let t_max = best(|| { let mut acc = 0u64; for &(a, b) in &qs { acc += h.range_ext(a, b, &mut out, true) as u64; } black_box(acc); });
        let t_max_raw = best(|| { let mut acc = 0u64; for &(a, b) in &qs { acc += *h.raw[a..b].iter().max().unwrap() as u64; } black_box(acc); });
        // OHLCV over [a, b): open and close are points, high a range max, low a range min, volume a range sum.
        let t_bar = best(|| { let mut acc = 0u64; for &(a, b) in &qs {
            let o = extract::<W>(&v.blocks[a / N], a % N); let c = extract::<W>(&v.blocks[(b - 1) / N], (b - 1) % N);
            acc += (o + c + h.range_ext(a, b, &mut out, true) + l.range_ext(a, b, &mut out, false)) as u64 + v.range_sum(a, b, &mut out); } black_box(acc); });
        let t_bar_raw = best(|| { let mut acc = 0u64; for &(a, b) in &qs {
            acc += (v.raw[a] + v.raw[b - 1] + *h.raw[a..b].iter().max().unwrap() + *l.raw[a..b].iter().min().unwrap()) as u64 + v.raw[a..b].iter().map(|x| *x as u64).sum::<u64>(); } black_box(acc); });
        // The same queries as one batch sorted by start (what the engine does with a batch of windows).
        let mut sorted = qs.clone(); sorted.sort_unstable();
        let t_dec_s = best(|| { let mut acc = 0u64; for &(a, b) in &sorted { h.for_range(a, b, &mut out, |x| for y in x { acc += *y as u64; }); } black_box(acc); });
        let t_raw_s = best(|| { let mut acc = 0u64; for &(a, b) in &sorted { for y in &h.raw[a..b] { acc += *y as u64; } } black_box(acc); });
        let per = |t: f64| t * 1e9 / qs.len() as f64;
        println!("rows {:>5}..{:<6} (mean {:>6.0}): series decode+sum {:>7.0} ns (raw {:>7.0}); batch sorted {:>7.0} ns (raw {:>7.0}) | range max {:>5.0} ns (raw scan {:>7.0}) | OHLCV bar {:>5.0} ns (raw scans {:>7.0})",
            lo, hi, avg, per(t_dec), per(t_raw), per(t_dec_s), per(t_raw_s), per(t_max), per(t_max_raw), per(t_bar), per(t_bar_raw));
    }
    let wm = Wm::new(&h.raw, W);
    let wm_bytes: usize = wm.levels.iter().map(|(b, c, _)| b.len() * 8 + c.len() * 4).sum();
    println!("wavelet matrix: {:.2} bits/value (with the rank directory)", wm_bytes as f64 * 8.0 / n as f64);
    for (lo, hi) in [(20usize, 300usize), (300, 3_000), (3_000, 30_000)] {
        let qs = queries(&mut rnd, lo, hi, 50_000);
        let mut buf: Vec<u32> = Vec::new();
        for &(a, b) in qs.iter().take(500) { let k = (b - a) / 2; buf.clear(); buf.extend_from_slice(&h.raw[a..b]); buf.sort_unstable();
            assert_eq!(wm.quantile(a, b, k), buf[k]); let c = h.raw[a]; assert_eq!(wm.count_less(a, b, c), buf.partition_point(|x| *x < c)); }
        let t_q = best(|| { let mut acc = 0u64; for &(a, b) in &qs { acc += wm.quantile(a, b, (b - a) / 2) as u64; } black_box(acc); });
        let t_c = best(|| { let mut acc = 0usize; for &(a, b) in &qs { acc += wm.count_less(a, b, 1000); } black_box(acc); });
        let t_raw = best(|| { let mut acc = 0u64; for &(a, b) in &qs { buf.clear(); buf.extend_from_slice(&h.raw[a..b]); let k = (b - a) / 2; acc += *buf.select_nth_unstable(k).1 as u64; } black_box(acc); });
        println!("rows {:>5}..{:<6}: wavelet median {:>5.0} ns, count below c {:>5.0} ns | raw copy + select {:>7.0} ns",
            lo, hi, t_q * 1e9 / qs.len() as f64, t_c * 1e9 / qs.len() as f64, t_raw * 1e9 / qs.len() as f64);
    }
}
fn main() {
    let nb = 32_768; // 33.5M values per width: far larger than cache, like a real scan
    println!("{} values per test, sum of the decoded column (exact integer), best of 9", nb * N);
    if std::env::args().nth(1).as_deref() == Some("range") { range_bench(); return; }
    if std::env::args().nth(1).as_deref() != Some("extra") { bench::<5>(nb); bench::<8>(nb); bench::<11>(nb); bench::<14>(nb); bench::<20>(nb); bench::<27>(nb); return; }
    println!("encode (min, GCD, width, pack): w8 {:.2} ns/v, w11 {:.2} ns/v, w20 {:.2} ns/v", encode_cost::<8>(4096), encode_cost::<11>(4096), encode_cost::<20>(4096));
    for (lo, span) in [(5usize, 250usize), (250, 2000), (2000, 20000)] {
        let (c, sc) = window_cost::<11>(nb, lo, span);
        println!("random window sum, length {}..{}, width 11: cumulative block sums + shorter-side edge decode {:.0} ns/query; scanning raw u32 {:.0} ns/query", lo, lo + span, c, sc);
    }
}
