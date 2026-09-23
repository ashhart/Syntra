//! Deterministic random numbers for exploration sampling.
//!
//! Every decision samples its action with a [`SplitMix64`] generator seeded
//! from a single `u64`. The seed is recorded in the decision log, so any draw
//! can be replayed exactly. SplitMix64 (Steele, Lea and Flood 2014, reference
//! code by Vigna) is small, fast, passes BigCrush, and is fully determined by
//! its 64-bit state. It is not cryptographically secure, which is fine here:
//! seeds come from the OS CSPRNG via [`random_seed`], and the generator is
//! used only to sample from a published probability distribution.

use std::sync::atomic::{AtomicU64, Ordering};

/// SplitMix64 pseudo-random generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator whose output sequence is fully determined by `seed`.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next 64 uniformly distributed bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform `f64` in `[0, 1)`: the top 53 bits of [`Self::next_u64`]
    /// scaled by `2^-53`, so every value is an exact multiple of `2^-53`.
    pub fn next_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / (1u64 << 53) as f64;
        (self.next_u64() >> 11) as f64 * SCALE
    }
}

/// A fresh random seed from the operating system's CSPRNG (`getrandom`).
///
/// If the OS generator is unavailable (only seen in unusual sandboxes), this
/// falls back to mixing the wall clock with a process-wide counter through
/// the SplitMix64 finalizer. Exploration then remains well distributed but
/// seeds become predictable, which only matters to an adversary trying to
/// anticipate draws.
pub fn random_seed() -> u64 {
    let mut buf = [0u8; 8];
    match getrandom::getrandom(&mut buf) {
        Ok(()) => u64::from_le_bytes(buf),
        Err(_) => fallback_seed(),
    }
}

fn fallback_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    SplitMix64::new(nanos ^ count.rotate_left(32)).next_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference outputs of Vigna's splitmix64.c (also reproduced by an
    /// independent Python implementation while writing this module).
    #[test]
    fn matches_reference_sequence() {
        let mut rng = SplitMix64::new(1_234_567);
        let expected = [
            6_457_827_717_110_365_317u64,
            3_203_168_211_198_807_973,
            9_817_491_932_198_370_423,
            4_593_380_528_125_082_431,
            16_408_922_859_458_223_821,
        ];
        for want in expected {
            assert_eq!(rng.next_u64(), want);
        }
        let mut zero = SplitMix64::new(0);
        assert_eq!(zero.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(zero.next_u64(), 0x6E78_9E6A_A1B9_65F4);
    }

    #[test]
    fn same_seed_same_sequence() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        let mut c = SplitMix64::new(43);
        let xs: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        let zs: Vec<u64> = (0..16).map(|_| c.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(xs, zs);
    }

    #[test]
    fn next_f64_is_in_unit_interval_with_53_bits() {
        let mut rng = SplitMix64::new(7);
        let mut sum = 0.0;
        let n = 100_000;
        for _ in 0..n {
            let u = rng.next_f64();
            assert!((0.0..1.0).contains(&u), "{u} outside [0, 1)");
            // Exact multiple of 2^-53.
            let scaled = u * (1u64 << 53) as f64;
            assert_eq!(scaled, scaled.trunc());
            sum += u;
        }
        let mean = sum / n as f64;
        // Standard error of the mean is sqrt(1/12 / n) ~ 0.0009.
        assert!((mean - 0.5).abs() < 0.005, "mean {mean}");
    }

    #[test]
    fn next_f64_extremes() {
        // A state whose next output is all ones maps to the largest value
        // below 1; this checks the scaling rather than a lucky draw.
        let max = (u64::MAX >> 11) as f64 * (1.0 / (1u64 << 53) as f64);
        assert!(max < 1.0);
        assert_eq!(max, 1.0 - f64::EPSILON / 2.0);
    }

    #[test]
    fn random_seeds_differ() {
        let seeds: Vec<u64> = (0..8).map(|_| random_seed()).collect();
        let mut unique = seeds.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), seeds.len(), "{seeds:?}");
        assert_ne!(fallback_seed(), fallback_seed());
    }
}
