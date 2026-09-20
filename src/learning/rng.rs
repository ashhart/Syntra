/// Global seedable PRNG state. `None` ⇒ legacy SystemTime entropy;
/// `Some(state)` ⇒ deterministic SplitMix64. Set via `LYCAN_RNG_SEED` env
/// var or `POST /admin/rng/seed`. Determinism requires sequential requests.
static SEEDED_RNG: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// Seed the global PRNG. Pass `Some(seed)` for deterministic mode;
/// `None` to revert to legacy SystemTime entropy.
pub fn seed_rng(seed: Option<u64>) {
    let mut g = SEEDED_RNG.lock().expect("SEEDED_RNG mutex poisoned");
    *g = seed;
}

/// Inspect the current seed state (test/diagnostics).
pub fn rng_seed_state() -> Option<u64> {
    *SEEDED_RNG.lock().expect("SEEDED_RNG mutex poisoned")
}

pub(crate) fn rand_f64() -> f64 {
    {
        let mut g = SEEDED_RNG.lock().expect("SEEDED_RNG mutex poisoned");
        if let Some(state) = g.as_mut() {
            // SplitMix64: advance state, mix, normalise to f64 in [0, 1).
            *state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            z = z ^ (z >> 31);
            return (z >> 11) as f64 / (1u64 << 53) as f64;
        }
    }
    // Fallback: SystemTime + thread + counter mix.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    std::thread::current().id().hash(&mut h);
    c.hash(&mut h);
    (h.finish() % 1_000_000) as f64 / 1_000_000.0
}

#[cfg(test)]
mod seeded_rng_tests {
    use super::{rand_f64, seed_rng};

    /// Same seed ⇒ same sequence. Re-seeding ⇒ restart sequence.
    #[test]
    fn deterministic_when_seeded() {
        seed_rng(Some(42));
        let a: Vec<f64> = (0..5).map(|_| rand_f64()).collect();
        seed_rng(Some(42));
        let b: Vec<f64> = (0..5).map(|_| rand_f64()).collect();
        assert_eq!(a, b, "seeded RNG must reproduce same sequence");

        seed_rng(Some(43));
        let c: Vec<f64> = (0..5).map(|_| rand_f64()).collect();
        assert_ne!(a, c, "different seed must produce different sequence");

        // Range check.
        for v in &a {
            assert!(*v >= 0.0 && *v < 1.0, "sample {v} out of [0, 1)");
        }
        seed_rng(None); // leave fallback for other tests
    }
}
