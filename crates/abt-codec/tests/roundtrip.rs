//! ST-13, the codec's property tests: encode then decode is the identity on
//! random blocks at every width 0 to 64, with and without exceptions, with
//! `recip_exact` set and unset (Proposition 3.2); the quantum is the data's
//! (ST-2.2's binary fractions: eighths, 1/64, 1/256, 0.25); and the lane
//! layout packs and unpacks every width.

use abt_codec::block::{extract, pack, patch, unpack, unpack_rows, Horizontal, Words};
use abt_codec::tick::{best_width, encode, exponent, Exception, Source};
use abt_codec::BLOCK;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn offsets(r: &mut Rng, n: usize, width: u8) -> Vec<u64> {
    let m = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
    (0..n).map(|_| if width >= 64 { r.next() } else { r.next() & m }).collect()
}

#[test]
fn the_lane_layout_packs_and_unpacks_every_width() {
    let mut r = Rng(7);
    for width in 0..=64u8 {
        for &n in &[BLOCK, 1, 31, 32, 33, 1000] {
            let u = offsets(&mut r, n, width);
            let mut full = u.clone();
            full.resize(BLOCK, 0);
            let p = pack(&full, width);
            match (&p.words, width) {
                (Words::W32(w), 0..=32) => assert_eq!(w.len(), 32 * width as usize),
                (Words::W64(w), 33..=64) => assert_eq!(w.len(), 32 * (width as usize).div_ceil(2)),
                _ => panic!("words of the wrong size at width {}", width),
            }
            let mut out = [0u64; BLOCK];
            unpack(&p, &mut out);
            assert_eq!(&out[..n], &u[..], "width {} n {}", width, n);
            for _ in 0..20 {
                let j = r.below(n as u64) as usize;
                assert_eq!(extract(&p, j), u[j], "extract at width {}", width);
            }
            // Row ranges equal the full decoding, at every kind of boundary.
            for (a, b) in [
                (0, n),
                (0, 1),
                (n - 1, n),
                (n / 3, n / 3 + 1),
                (31usize.min(n - 1), 33usize.min(n)),
                (n / 2, n),
                (5usize.min(n), 300usize.min(n)),
            ] {
                if a > b {
                    continue;
                }
                let mut rows = vec![0u64; b - a];
                unpack_rows(&p, a, b, &mut rows);
                assert_eq!(&rows[..], &u[a..b], "rows {}..{} at width {}", a, b, width);
            }
        }
    }
}

#[test]
fn encode_then_decode_is_the_identity_with_and_without_exceptions() {
    let mut r = Rng(3);
    for case in 0..300 {
        let n = 1 + r.below(BLOCK as u64) as usize;
        let source = if case % 2 == 0 { Source::F64 } else { Source::F32 };
        // A grid: a quantum, a base, offsets of a width; some blocks get a
        // few wide outliers (patched), some a value off the grid (raw).
        let e = r.below(7) as u8;
        let g = [1u64, 1, 2, 5, 25, 125, 3, 7][r.below(8) as usize];
        let width = r.below(20) as u8;
        let base = r.below(1 << 30) as i64;
        let mut ints: Vec<i64> = offsets(&mut r, n, width).iter().map(|&u| base + (g * u) as i64).collect();
        let outliers = if case % 3 == 0 { r.below(4) as usize } else { 0 };
        for _ in 0..outliers {
            let j = r.below(n as u64) as usize;
            ints[j] = base + (g * (1u64 << 40) + g * r.below(1000)) as i64;
        }
        let scale = 10f64.powi(e as i32);
        let mut values: Vec<f64> = ints.iter().map(|&i| i as f64 / scale).collect();
        if source == Source::F32 {
            values = values.iter().map(|&x| (x as f32) as f64).collect();
        }
        let raw_at = if case % 5 == 0 {
            let j = r.below(n as u64) as usize;
            values[j] = 0.1234567891234 + r.uniform(); // off every grid up to 10^-9
            Some(j)
        } else {
            None
        };
        let enc = encode(&values, source);
        assert_eq!(enc.n as usize, n);
        let back = enc.decode();
        for (j, (&a, &b)) in values.iter().zip(&back).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "case {} row {}: {} came back as {}", case, j, a, b);
        }
        assert_eq!(enc.exact(), raw_at.is_none(), "case {}", case);
        if let Some(j) = raw_at {
            assert!(enc.exceptions.iter().any(|x| matches!(x, Exception::Raw { position, .. } if *position as usize == j)));
        }
        assert!(enc.exceptions.windows(2).all(|w| w[0].position() < w[1].position()), "exceptions are sorted");
        // The packed words with the exceptions patched decode to the offsets.
        let mut full = enc.offsets.clone();
        full.resize(BLOCK, 0);
        let p = pack(&full, enc.width);
        let mut out = [0u64; BLOCK];
        unpack(&p, &mut out);
        patch(&enc.exceptions, 0, n, &mut out[..n]);
        for (j, &o) in out.iter().enumerate().take(n) {
            if let Some(u) = enc.offset(j) {
                assert_eq!(o, u, "case {} row {}", case, j);
            }
        }
        // The exact sum of any range is the sum of the integers.
        if enc.exact() {
            let (a, b) = (r.below(n as u64) as usize, n);
            let want: i128 = (a..b).map(|j| enc.integer(j).unwrap() as i128).sum();
            assert_eq!(enc.sum(a, b), Some(want));
        }
    }
}

#[test]
fn the_quantum_is_found_from_the_data() {
    // Eighths at exponent 3 with GCD 125 (1995's close), 0.25 ticks (ES),
    // 1/64 (ZN), whole volumes.
    for (values, e, g) in [
        (vec![12.125, 12.25, 12.5, 13.0, 12.375], 3u8, 125u64),
        (vec![4_200.25, 4_200.5, 4_201.0, 4_199.75], 2, 25),
        (vec![110.0 + 1.0 / 64.0, 110.0 + 3.0 / 64.0, 110.0 + 10.0 / 64.0], 6, 15625), // 1/64 = 0.015625
        (vec![1_000.0, 2_500.0, 1_000_000.0], 0, 1500),
    ] {
        let enc = encode(&values, Source::F64);
        assert_eq!((enc.e, enc.g), (e, g), "{:?}", values);
        assert_eq!(enc.decode(), values);
        assert!(enc.exact());
    }
    assert_eq!(exponent(&[1.5, 2.25], Source::F64), Some(2));
    assert_eq!(exponent(&[0.1 + 0.2], Source::F64), None);
    // A constant block has width 0 and GCD 1.
    let enc = encode(&[7.5; 100], Source::F64);
    assert_eq!((enc.width, enc.g, enc.e), (0, 1, 1));
    // The width minimises bits plus 74 per exception.
    assert_eq!(best_width(&[1, 2, 3, 1 << 40]), 2);
    assert_eq!(best_width(&[1 << 40; 100]), 41);
    // An f32 source: values exactly f32, with the view rounding back to the bits.
    let values: Vec<f64> = (0..200).map(|i| (123.456f32 + i as f32 * 0.01) as f64).collect();
    let enc = encode(&values, Source::F32);
    assert!(enc.exact(), "f32 prices at 2 decimals are on a grid: exponent {}", enc.e);
    assert_eq!(
        enc.decode().iter().map(|&x| (x as f32).to_bits()).collect::<Vec<_>>(),
        values.iter().map(|&x| (x as f32).to_bits()).collect::<Vec<_>>()
    );
}

#[test]
fn the_tail_appends_in_place_and_reads_its_visible_prefix() {
    let mut r = Rng(5);
    for width in [0u8, 1, 7, 12, 31, 32, 33, 50, 64] {
        let u = offsets(&mut r, 1000, width);
        let mut h = Horizontal::new(width);
        for (j, &v) in u.iter().enumerate() {
            assert!(h.fits(v));
            h.push(v);
            assert_eq!(h.get(j), v);
        }
        assert_eq!(h.values(1000), u);
        assert_eq!(h.values(10), &u[..10]);
        assert_eq!(Horizontal::of(&u, width), h);
        if width < 64 {
            assert!(!h.fits(1u64 << width), "a value above the width does not fit");
        }
    }
}

#[test]
fn the_header_is_32_bytes_little_endian_and_round_trips() {
    use abt_codec::block::{Header, FLAG_F32, FLAG_PATCHED, FLAG_PREDICTED, FLAG_RECIP_EXACT, HEADER_BYTES};
    let h = Header {
        base: -1_234_567_890_123,
        g: 25,
        e: 4,
        width: 11,
        n: 1024,
        flags: FLAG_RECIP_EXACT | FLAG_PATCHED | FLAG_PREDICTED | FLAG_F32,
        exceptions: 0xDEAD_BEEF,
    };
    let bytes = h.to_bytes();
    assert_eq!(bytes.len(), HEADER_BYTES);
    assert_eq!(&bytes[0..8], &(-1_234_567_890_123i64).to_le_bytes());
    assert_eq!(&bytes[8..16], &25u64.to_le_bytes());
    assert_eq!(bytes[16], 4);
    assert_eq!(bytes[17], 11);
    assert_eq!(&bytes[18..20], &1024u16.to_le_bytes());
    assert_eq!(&bytes[20..22], &15u16.to_le_bytes());
    assert_eq!(&bytes[22..26], &0xDEAD_BEEFu32.to_le_bytes());
    assert_eq!(&bytes[26..32], &[0u8; 6]);
    assert_eq!(Header::from_bytes(&bytes), h);
    assert!(h.recip_exact() && h.patched() && h.predicted() && h.f32());
    assert_eq!(h.words_bytes(), 32 * 11 * 4);
    assert_eq!(Header { width: 33, ..h }.words_bytes(), 32 * 17 * 8);
    assert_eq!(Header { width: 0, ..h }.words_bytes(), 0);
    // Of an encoded block: the flags from its fields.
    let values: Vec<f64> = (0..100).map(|i| 10.0 + 0.25 * i as f64).collect();
    let enc = abt_codec::tick::encode(&values, abt_codec::tick::Source::F64);
    let h = Header::of(&enc, false);
    assert_eq!((h.base, h.g, h.e, h.width, h.n), (enc.base, enc.g, enc.e as i8, enc.width, enc.n));
    assert!(!h.patched() && !h.predicted() && !h.f32());
    assert_eq!(h.recip_exact(), enc.recip_exact);
}

#[test]
fn integers_encode_at_exponent_zero_and_round_trip() {
    use abt_codec::block::{pack, unpack, Packed};
    use abt_codec::tick::encode_integers;
    let cases: Vec<Vec<i64>> = vec![
        vec![],
        vec![7],
        vec![0, 0, 0],
        (0..1024).map(|i| 1_000 + 25 * i).collect(),
        (0..1000).map(|i| if i == 500 { 1 << 40 } else { 3 * (i % 7) - 9 }).collect(),
        vec![i64::MIN, i64::MAX, 0, -1, 1],
        (0..1024).map(|i| 1_700_000_000_000_000_000i64 + 60_000_000_000 * i).collect(),
    ];
    for values in cases {
        let enc = encode_integers(&values);
        assert_eq!(enc.e, 0);
        assert!(enc.exact());
        assert_eq!(enc.integers().as_deref(), Some(values.as_slice()));
        // Through the packed words and back through bytes.
        let packed = pack(&enc.offsets, enc.width);
        let again = Packed::from_bytes(enc.width, &packed.to_bytes());
        assert_eq!(again, packed);
        if !values.is_empty() {
            let mut out = [0u64; abt_codec::BLOCK];
            unpack(&again, &mut out);
            for (j, &v) in values.iter().enumerate() {
                let u = match enc.offset(j) {
                    Some(u) => u,
                    None => unreachable!(),
                };
                let from_words = if enc.exceptions.iter().any(|x| x.position() as usize == j) { u } else { out[j] };
                assert_eq!(from_words, u);
                assert_eq!(enc.base.wrapping_add(enc.g.wrapping_mul(u) as i64), v);
            }
        }
    }
}

#[test]
fn sums_are_exact_across_quanta() {
    // Proposition 3.3: blocks at different quanta (cents, sixteenths at
    // exponent 4, whole volumes) sum to the exact integer at the finest
    // exponent, over whole blocks and over row ranges; the `f64` of the
    // sum is the one correctly rounded division; a sum beyond `i64` is
    // exact in `i128`.
    use abt_agg::exact::{pow10, Int};
    use abt_codec::tick::{aligned, to_f64};
    let mut r = Rng(0x5EED_0033);
    for round in 0..40 {
        let n = 1 + r.below(BLOCK as u64) as usize;
        let (a, b) = {
            let a = r.below(n as u64) as usize;
            (a, a + 1 + r.below((n - a) as u64) as usize)
        };
        // (exponent, unit of the grid at that exponent, range of the base)
        let kinds = [(2u8, 1i64, 1_000_000u64), (4, 625, 160), (0, 1, 10_000_000)];
        let e_star = kinds.iter().map(|k| k.0).max().unwrap();
        let (mut whole, mut range) = (Int::ZERO, Int::ZERO);
        let (mut whole_ref, mut range_ref) = (0i128, 0i128);
        for &(e, unit, base_range) in &kinds {
            let base = r.below(base_range) as i64 * unit;
            let ints: Vec<i64> = (0..n).map(|_| base + r.below(50_000) as i64 * unit).collect();
            let values: Vec<f64> = ints.iter().map(|&i| i as f64 / pow10(e as u32) as f64).collect();
            let enc = encode(&values, Source::F64);
            assert!(enc.exact() && enc.e <= e, "round {round}: exponent {} for grid {e}", enc.e);
            whole = whole.add(&aligned(enc.sum(0, n).unwrap(), enc.e, e_star));
            range = range.add(&aligned(enc.sum(a, b).unwrap(), enc.e, e_star));
            let scale = pow10((e_star - e) as u32) as i128;
            whole_ref += ints.iter().map(|&i| i as i128 * scale).sum::<i128>();
            range_ref += ints[a..b].iter().map(|&i| i as i128 * scale).sum::<i128>();
        }
        assert_eq!(whole, Int::from_i128(whole_ref), "round {round}: the whole blocks");
        assert_eq!(range, Int::from_i128(range_ref), "round {round}: rows {a}..{b}");
        // Below 2^53 the quotient of the two exact doubles is the correctly
        // rounded value of the rational.
        for (sum, reference) in [(&whole, whole_ref), (&range, range_ref)] {
            assert!(reference.unsigned_abs() < 1 << 53);
            let expected = reference as f64 / pow10(e_star as u32) as f64;
            assert_eq!(to_f64(sum, e_star).to_bits(), expected.to_bits(), "round {round}");
        }
    }
    // A block of large whole values whose sum exceeds i64: exact in i128,
    // and its f64 the correctly rounded conversion.
    let ints: Vec<i64> = (0..BLOCK).map(|k| (1i64 << 62) + (k as i64) * (1 << 11)).collect();
    let values: Vec<f64> = ints.iter().map(|&i| i as f64).collect();
    assert!(values.iter().zip(&ints).all(|(&v, &i)| v as i64 == i), "the values are exact in f64");
    let enc = encode(&values, Source::F64);
    assert!(enc.exact() && enc.e == 0);
    let reference: i128 = ints.iter().map(|&i| i as i128).sum();
    assert!(reference > i64::MAX as i128);
    assert_eq!(enc.sum(0, BLOCK), Some(reference));
    assert_eq!(to_f64(&Int::from_i128(reference), 0).to_bits(), (reference as f64).to_bits());
}
