//! Per-token token-bucket rate limiter.
//!
//! Each principal (legacy admin key or scoped-token hash) gets its own
//! bucket. Buckets refill at `rate` tokens/sec up to `burst` capacity.
//! `try_consume()` either decrements one token and returns `Allow`, or
//! returns `Deny { retry_after_seconds }` when empty.
//!
//! No external dep — stdlib only. Lock-protected per-bucket; the global
//! map is locked briefly only when getting or inserting a bucket.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub rate_per_second: f64,
    pub burst: f64,
}

impl Default for RateLimitConfig {
    /// Default: 1000 req/sec/token, 2000 burst. Strict enough to throttle a
    /// runaway client or leaked-key attacker, loose enough that the e2e
    /// benchmark suite's decide+feedback bursts (200 req/sec sustained,
    /// 600+ req/sec peak) don't trip it. Operators can override via the
    /// (future, deferred) per-capsule rate-limit config.
    fn default() -> Self {
        Self { rate_per_second: 1000.0, burst: 2000.0 }
    }
}

#[derive(Debug)]
pub enum Decision {
    Allow,
    Deny { retry_after_seconds: f64 },
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(burst: f64) -> Self {
        Self { tokens: burst, last_refill: Instant::now() }
    }

    fn try_consume(&mut self, cfg: &RateLimitConfig) -> Decision {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * cfg.rate_per_second).min(cfg.burst);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Decision::Allow
        } else {
            let deficit = 1.0 - self.tokens;
            let wait = deficit / cfg.rate_per_second;
            Decision::Deny { retry_after_seconds: wait.max(0.001) }
        }
    }
}

pub struct RateLimiter {
    cfg: RateLimitConfig,
    /// principal-id → bucket. The principal-id is the auth token hash or
    /// the literal string "legacy-admin" for the bearer-key path. Each
    /// bucket is wrapped in its own Arc<Mutex> so concurrent principals
    /// don't serialize through the outer map lock.
    buckets: Mutex<HashMap<String, Arc<Mutex<Bucket>>>>,
}

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Self {
        Self { cfg, buckets: Mutex::new(HashMap::new()) }
    }

    pub fn check(&self, principal: &str) -> Decision {
        let bucket = {
            let mut buckets = self.buckets.lock().unwrap();
            buckets.entry(principal.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(Bucket::new(self.cfg.burst))))
                .clone()
        };
        bucket.lock().unwrap().try_consume(&self.cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn allows_burst_then_denies() {
        let cfg = RateLimitConfig { rate_per_second: 10.0, burst: 5.0 };
        let rl = RateLimiter::new(cfg);
        for _ in 0..5 {
            assert!(matches!(rl.check("p1"), Decision::Allow));
        }
        match rl.check("p1") {
            Decision::Deny { retry_after_seconds } => {
                assert!(retry_after_seconds > 0.0 && retry_after_seconds < 1.0);
            }
            Decision::Allow => panic!("expected deny after burst exhausted"),
        }
    }

    #[test]
    fn refills_after_wait() {
        let cfg = RateLimitConfig { rate_per_second: 50.0, burst: 1.0 };
        let rl = RateLimiter::new(cfg);
        assert!(matches!(rl.check("p"), Decision::Allow));
        assert!(matches!(rl.check("p"), Decision::Deny { .. }));
        thread::sleep(Duration::from_millis(30));
        // 30ms × 50/s = 1.5 tokens refilled — should allow.
        assert!(matches!(rl.check("p"), Decision::Allow));
    }

    #[test]
    fn separate_principals_have_separate_budgets() {
        let cfg = RateLimitConfig { rate_per_second: 1.0, burst: 1.0 };
        let rl = RateLimiter::new(cfg);
        assert!(matches!(rl.check("alice"), Decision::Allow));
        assert!(matches!(rl.check("alice"), Decision::Deny { .. }));
        // Bob still has his full bucket.
        assert!(matches!(rl.check("bob"), Decision::Allow));
    }

    #[test]
    fn retry_after_scales_inversely_with_rate() {
        let slow = RateLimiter::new(RateLimitConfig { rate_per_second: 1.0, burst: 1.0 });
        let fast = RateLimiter::new(RateLimitConfig { rate_per_second: 100.0, burst: 1.0 });
        let _ = slow.check("p"); let _ = fast.check("p");
        let s = match slow.check("p") { Decision::Deny { retry_after_seconds } => retry_after_seconds, _ => panic!() };
        let f = match fast.check("p") { Decision::Deny { retry_after_seconds } => retry_after_seconds, _ => panic!() };
        // Slow limiter requires waiting ~100× longer than fast.
        assert!(s > f * 50.0, "slow={s} fast={f}");
    }
}
