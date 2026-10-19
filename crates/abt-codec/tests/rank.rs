//! Presence bitmaps with rank and select (ST-5.4): rank, rank0, select and
//! select0 equal their definitions over random bitmaps of every density at
//! and around the directory's word and block boundaries; a bitmap built by
//! appends (ST-8.4's in-place update of the directory) equals one built
//! whole; setting a bit rebuilds the directory to the same state.

use abt_codec::rank::Bitmap;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn random_bits(rng: &mut Rng, len: usize, density: f64) -> Vec<bool> {
    (0..len).map(|_| rng.unit() < density).collect()
}

/// Every rank and select against the definitions.
fn check(bits: &[bool], b: &Bitmap) {
    assert_eq!(b.len(), bits.len());
    let ones = bits.iter().filter(|&&x| x).count() as u64;
    assert_eq!(b.ones(), ones);
    let mut prefix = 0u64;
    for i in 0..=bits.len() {
        assert_eq!(b.rank(i), prefix, "rank({}) of {} bits", i, bits.len());
        assert_eq!(b.rank0(i), i as u64 - prefix, "rank0({})", i);
        if i < bits.len() {
            assert_eq!(b.get(i), bits[i]);
            if bits[i] {
                assert_eq!(b.select(prefix), Some(i), "select({}) of {} bits", prefix, bits.len());
                prefix += 1;
            } else {
                assert_eq!(b.select0(i as u64 - prefix), Some(i), "select0({}) of {} bits", i as u64 - prefix, bits.len());
            }
        }
    }
    assert_eq!(b.select(ones), None);
    assert_eq!(b.select0(bits.len() as u64 - ones), None);
}

const LENGTHS: [usize; 12] = [0, 1, 63, 64, 65, 127, 511, 512, 513, 1024, 1537, 5000];
const DENSITIES: [f64; 5] = [0.0, 0.01, 0.5, 0.99, 1.0];

#[test]
fn rank_and_select_equal_their_definitions_at_every_density_and_boundary() {
    let mut rng = Rng(7);
    for &len in &LENGTHS {
        for &density in &DENSITIES {
            let bits = random_bits(&mut rng, len, density);
            let b = Bitmap::from_bools(&bits);
            check(&bits, &b);
        }
    }
}

#[test]
fn a_bitmap_built_by_appends_equals_one_built_whole() {
    let mut rng = Rng(11);
    for &len in &LENGTHS {
        for &density in &DENSITIES {
            let bits = random_bits(&mut rng, len, density);
            let whole = Bitmap::from_bools(&bits);
            let mut pushed = Bitmap::new();
            for (i, &x) in bits.iter().enumerate() {
                pushed.push(x);
                // The directory is right after every append, not only at
                // the end.
                if i % 97 == 0 || i + 1 == bits.len() {
                    check(&bits[..=i], &pushed);
                }
            }
            assert_eq!(pushed, whole, "{} bits at density {}", len, density);
        }
    }
}

#[test]
fn setting_a_bit_rebuilds_the_directory() {
    let mut rng = Rng(13);
    let mut bits = random_bits(&mut rng, 3000, 0.3);
    let mut b = Bitmap::from_bools(&bits);
    for &i in &[0usize, 63, 64, 511, 512, 513, 1500, 2999] {
        bits[i] = true;
        b.set(i);
        assert_eq!(b, Bitmap::from_bools(&bits));
    }
    check(&bits, &b);
}

#[test]
fn the_directory_is_about_a_quarter_of_the_bits() {
    let b = Bitmap::of(1 << 20, (0..1 << 20).step_by(3));
    // Two words per 512-bit block: 128 bits per 512, 25 %.
    assert_eq!(b.directory_words() * 64, (1 << 20) / 4);
}

#[test]
fn a_bitmap_rebuilt_from_its_words_is_the_same() {
    let mut rng = Rng(17);
    for &len in &LENGTHS {
        let bits = random_bits(&mut rng, len, 0.4);
        let b = Bitmap::from_bools(&bits);
        let again = Bitmap::from_words(len, b.words());
        assert_eq!(again, b);
        check(&bits, &again);
    }
}
