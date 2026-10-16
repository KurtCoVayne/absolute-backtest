//! The framework's generator (Q-4.1): a 64-bit xorshift* (Vigna 2016), as
//! the kernel's study drew its bootstraps with. Every replication of a
//! resampling test draws from its own stream, keyed by (seed, replication)
//! through SplitMix64, so a result does not depend on how replications are
//! scheduled across threads.

/// SplitMix64's finaliser: a bijection of u64 that spreads nearby keys.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A xorshift* stream.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// The stream of replication `rep` under `seed`.
    pub fn stream(seed: u64, rep: u64) -> Rng {
        let s = mix(mix(seed) ^ rep.wrapping_mul(0xD1B5_4A32_D192_ED03));
        Rng {
            state: if s == 0 { 0x2545_F491_4F6C_DD1D } else { s },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform on [0, 1), from the top 53 bits.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform on 0..n (Lemire's multiply-shift, rejection-free bias below
    /// 2^-64 · n).
    pub fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }

    /// A standard normal draw (Box–Muller, one of the pair).
    pub fn normal(&mut self) -> f64 {
        let u1 = 1.0 - self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// A Fisher–Yates shuffle.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_reproducible_and_distinct() {
        let a: Vec<u64> = (0..4).map(|_| Rng::stream(7, 0).next_u64()).collect();
        assert!(a.windows(2).all(|w| w[0] == w[1]));
        assert_ne!(Rng::stream(7, 0).next_u64(), Rng::stream(7, 1).next_u64());
        assert_ne!(Rng::stream(7, 0).next_u64(), Rng::stream(8, 0).next_u64());
        let mut r = Rng::stream(1, 2);
        let u: Vec<f64> = (0..10_000).map(|_| r.uniform()).collect();
        let mean = u.iter().sum::<f64>() / u.len() as f64;
        assert!((mean - 0.5).abs() < 0.02, "{}", mean);
        assert!(u.iter().all(|x| (0.0..1.0).contains(x)));
    }
}
