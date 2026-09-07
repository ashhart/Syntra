use serde::{Deserialize, Serialize};

/// Per-option LinUCB state. Maintains the inverse of the regularized
/// design matrix A_inv (d × d) and the response vector b (d × 1).
/// The parameter estimate is theta = A_inv · b.
///
/// References:
///   Li, Chu, Langford, Schapire (2010), "A Contextual-Bandit Approach
///   to Personalized News Article Recommendation"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinUcbState {
    /// Inverse of A = lambda·I + sum(x_i · x_i^T), stored row-major.
    pub a_inv: Vec<Vec<f64>>,
    /// Response vector b = sum(r_i · x_i).
    pub b: Vec<f64>,
    /// Feature dimension.
    pub d: usize,
    /// Regularization (initial lambda·I weight).
    pub lambda: f64,
    /// Number of incremental updates since last full rebuild of a_inv from A.
    pub since_last_rebuild: u64,
    /// A matrix tracked incrementally; used for periodic rebuild of a_inv.
    pub a: Vec<Vec<f64>>,
}

impl LinUcbState {
    /// Initialize with regularization lambda. A starts as lambda·I, A_inv
    /// starts as (1/lambda)·I, b starts as zero.
    pub fn new(d: usize, lambda: f64) -> Self {
        let mut a = vec![vec![0.0; d]; d];
        let mut a_inv = vec![vec![0.0; d]; d];
        for i in 0..d {
            a[i][i] = lambda;
            a_inv[i][i] = 1.0 / lambda;
        }
        Self {
            a_inv,
            b: vec![0.0; d],
            d,
            lambda,
            since_last_rebuild: 0,
            a,
        }
    }

    /// Compute theta = A_inv · b.
    pub fn theta(&self) -> Vec<f64> {
        matvec(&self.a_inv, &self.b)
    }

    /// Linear Thompson Sampling score: samples `θ̃ ~ N(μ, v²·A⁻¹)` via
    /// Cholesky and returns `x·θ̃`. `rng` supplies iid standard-normal
    /// draws; falls back to posterior mean on Cholesky failure.
    pub fn lin_ts_score<F: FnMut() -> f64>(
        &self, x: &[f64], v: f64, mut rng: F,
    ) -> f64 {
        debug_assert_eq!(x.len(), self.d);
        let theta = self.theta();
        let mean = dot(x, &theta);
        let chol = match cholesky(&self.a_inv) {
            Some(l) => l,
            None => return mean,
        };
        // Sample z ∈ R^d ~ N(0, I).
        let mut z = vec![0.0; self.d];
        for i in 0..self.d { z[i] = rng(); }
        // L · z gives a draw from N(0, A⁻¹). Scale by v to get N(0, v²·A⁻¹).
        let mut lz = vec![0.0; self.d];
        for i in 0..self.d {
            let mut s = 0.0;
            for j in 0..=i { s += chol[i][j] * z[j]; }
            lz[i] = s * v;
        }
        // θ̃ = μ + v·L·z, then return x·θ̃.
        let mut sampled = vec![0.0; self.d];
        for i in 0..self.d { sampled[i] = theta[i] + lz[i]; }
        let score = dot(x, &sampled);
        if score.is_finite() { score } else { mean }
    }

    /// Compute UCB score for the given feature vector x.
    /// score = x · theta + alpha · sqrt(x · A_inv · x)
    /// Returns the score plus a flag indicating whether numerical defenses
    /// fired (caller may want to log).
    pub fn ucb_score(&self, x: &[f64], alpha: f64) -> (f64, bool) {
        debug_assert_eq!(x.len(), self.d);
        let theta = self.theta();
        let mean = dot(x, &theta);

        let a_inv_x = matvec(&self.a_inv, x);
        let variance = dot(x, &a_inv_x).max(0.0); // numerical floor at 0
        let raw_bonus = alpha * variance.sqrt();

        // Clamp at 10·alpha to prevent blow-up from collinear features.
        let clamped_bonus = raw_bonus.min(alpha * 10.0);
        let clamped = raw_bonus > clamped_bonus;

        let score = mean + clamped_bonus;

        // Non-finite ⇒ fall back to greedy on the mean.
        if !score.is_finite() {
            let fallback = if mean.is_finite() { mean } else { 0.0 };
            return (fallback, true);
        }

        (score, clamped)
    }

    /// Sherman-Morrison rank-1 update for A_inv given new observation x:
    ///   A_new = A + x · x^T
    ///   A_inv_new = A_inv - (A_inv · x · x^T · A_inv) / (1 + x^T · A_inv · x)
    /// And update b = b + reward · x.
    pub fn update(&mut self, x: &[f64], reward: f64) {
        debug_assert_eq!(x.len(), self.d);

        // Update A directly (used for periodic rebuild).
        for i in 0..self.d {
            for j in 0..self.d {
                self.a[i][j] += x[i] * x[j];
            }
        }

        // Sherman-Morrison update of A_inv.
        let a_inv_x = matvec(&self.a_inv, x);          // d-vector
        let denom = 1.0 + dot(x, &a_inv_x);            // scalar
        let denom = denom.max(1e-12);

        // outer product of a_inv_x with itself, divided by denom
        let mut delta = vec![vec![0.0; self.d]; self.d];
        for i in 0..self.d {
            for j in 0..self.d {
                delta[i][j] = (a_inv_x[i] * a_inv_x[j]) / denom;
            }
        }
        for i in 0..self.d {
            for j in 0..self.d {
                self.a_inv[i][j] -= delta[i][j];
            }
        }

        // Update b.
        for i in 0..self.d {
            self.b[i] += reward * x[i];
        }

        self.since_last_rebuild += 1;
    }

    /// Rebuild A_inv from A via Gauss-Jordan (O(d³)). Call periodically.
    pub fn rebuild_inverse(&mut self) {
        if let Some(inv) = gauss_jordan_invert(&self.a) {
            self.a_inv = inv;
        }
        // On inversion failure (shouldn't happen for PSD A) keep the
        // existing a_inv rather than corrupt state.
        self.since_last_rebuild = 0;
    }

    /// Whether a rebuild is recommended based on update count.
    pub fn rebuild_due(&self, threshold: u64) -> bool {
        self.since_last_rebuild >= threshold
    }
}

// ── Shared-state LinUCB with action embeddings ──
// Single (A, b) over `[x_context, x_option]`; generalises across actions
// sharing embedding dimensions. Li-Chu-Langford-Schapire 2010 § 4.1.

/// Shared-state LinUCB over the concatenated feature vector
/// `[x_context, x_option]`. Same storage + update shape as `LinUcbState`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinUcbSharedState {
    /// Design matrix A = λ·I + Σ x_i·x_iᵀ, where each x_i is a full
    /// `[x_context, x_option]` concatenation. Row-major (d_total × d_total).
    pub a: Vec<Vec<f64>>,
    /// Inverse of A. Maintained incrementally via Sherman-Morrison and
    /// periodically rebuilt via Gauss-Jordan for numerical stability.
    pub a_inv: Vec<Vec<f64>>,
    /// Response vector b = Σ r_i · x_i.
    pub b: Vec<f64>,
    /// Total feature dimension. Equals the length of
    /// `concat_features(x_context, x_option)`.
    pub d_total: usize,
    /// Regularisation strength (initial λ·I weight).
    pub lambda: f64,
    /// Number of Sherman-Morrison updates since the last full rebuild.
    pub since_last_rebuild: u64,
}

impl LinUcbSharedState {
    /// Initialise shared state with regularisation λ. A starts as λ·I,
    /// A_inv as (1/λ)·I, b as the zero vector. `d_total` must be the
    /// concatenated feature dimension `d_context + d_option`.
    pub fn new(d_total: usize, lambda: f64) -> Self {
        let mut a = vec![vec![0.0; d_total]; d_total];
        let mut a_inv = vec![vec![0.0; d_total]; d_total];
        for i in 0..d_total {
            a[i][i] = lambda;
            a_inv[i][i] = 1.0 / lambda;
        }
        Self {
            a,
            a_inv,
            b: vec![0.0; d_total],
            d_total,
            lambda,
            since_last_rebuild: 0,
        }
    }

    /// Posterior-mean parameter estimate θ = A_inv · b. Length `d_total`.
    pub fn shared_theta(&self) -> Vec<f64> {
        matvec(&self.a_inv, &self.b)
    }

    /// LinUCB score for a `(context, option)` pair. Returns
    /// `(score, clamped)`; `clamped` is true when the bonus hit the 10·α cap.
    pub fn shared_ucb_score(
        &self, x_context: &[f64], x_option: &[f64], alpha: f64,
    ) -> (f64, bool) {
        let x = concat_features(x_context, x_option);
        debug_assert_eq!(x.len(), self.d_total);
        let theta = self.shared_theta();
        let mean = dot(&x, &theta);

        let a_inv_x = matvec(&self.a_inv, &x);
        let variance = dot(&x, &a_inv_x).max(0.0); // numerical floor at 0
        let raw_bonus = alpha * variance.sqrt();

        // Clamp at 10·alpha to prevent blow-up from collinear features.
        let clamped_bonus = raw_bonus.min(alpha * 10.0);
        let clamped = raw_bonus > clamped_bonus;

        let score = mean + clamped_bonus;

        if !score.is_finite() {
            let fallback = if mean.is_finite() { mean } else { 0.0 };
            return (fallback, true);
        }

        (score, clamped)
    }

    /// LinTS score for a `(context, option)` pair. See `LinUcbState::lin_ts_score`.
    pub fn shared_lin_ts_score<F: FnMut() -> f64>(
        &self, x_context: &[f64], x_option: &[f64], v: f64, mut rng: F,
    ) -> f64 {
        let x = concat_features(x_context, x_option);
        debug_assert_eq!(x.len(), self.d_total);
        let theta = self.shared_theta();
        let mean = dot(&x, &theta);
        let chol = match cholesky(&self.a_inv) {
            Some(l) => l,
            None => return mean,
        };
        // Sample z ∈ R^d_total ~ N(0, I).
        let mut z = vec![0.0; self.d_total];
        for i in 0..self.d_total { z[i] = rng(); }
        // L · z gives a draw from N(0, A⁻¹). Scale by v for N(0, v²·A⁻¹).
        let mut lz = vec![0.0; self.d_total];
        for i in 0..self.d_total {
            let mut s = 0.0;
            for j in 0..=i { s += chol[i][j] * z[j]; }
            lz[i] = s * v;
        }
        // θ̃ = μ + v·L·z, then return x · θ̃.
        let mut sampled = vec![0.0; self.d_total];
        for i in 0..self.d_total { sampled[i] = theta[i] + lz[i]; }
        let score = dot(&x, &sampled);
        if score.is_finite() { score } else { mean }
    }

    /// Sherman-Morrison rank-1 update on the shared design matrix:
    ///   A_new = A + x·xᵀ
    ///   A_inv_new = A_inv − (A_inv·x · xᵀ·A_inv) / (1 + xᵀ·A_inv·x)
    /// And b_new = b + reward · x, where `x = concat_features(...)`.
    pub fn shared_update(
        &mut self, x_context: &[f64], x_option: &[f64], reward: f64,
    ) {
        let x = concat_features(x_context, x_option);
        debug_assert_eq!(x.len(), self.d_total);

        // Update A directly (used for periodic rebuild).
        for i in 0..self.d_total {
            for j in 0..self.d_total {
                self.a[i][j] += x[i] * x[j];
            }
        }

        // Sherman-Morrison update of A_inv.
        let a_inv_x = matvec(&self.a_inv, &x);
        let denom = 1.0 + dot(&x, &a_inv_x);
        let denom = denom.max(1e-12);

        let mut delta = vec![vec![0.0; self.d_total]; self.d_total];
        for i in 0..self.d_total {
            for j in 0..self.d_total {
                delta[i][j] = (a_inv_x[i] * a_inv_x[j]) / denom;
            }
        }
        for i in 0..self.d_total {
            for j in 0..self.d_total {
                self.a_inv[i][j] -= delta[i][j];
            }
        }

        for i in 0..self.d_total {
            self.b[i] += reward * x[i];
        }

        self.since_last_rebuild += 1;
    }

    /// Rebuild A_inv from A via Gauss-Jordan. Call periodically.
    pub fn shared_rebuild_inverse(&mut self) {
        if let Some(inv) = gauss_jordan_invert(&self.a) {
            self.a_inv = inv;
        }
        self.since_last_rebuild = 0;
    }

    /// Whether a rebuild is recommended based on update count.
    pub fn shared_rebuild_due(&self, threshold: u64) -> bool {
        self.since_last_rebuild >= threshold
    }
}

/// Concatenate context and option feature slices into a fresh
/// `[x_context, x_option]` vector of length `x_context.len() + x_option.len()`.
/// Allocates a new `Vec<f64>`; callers in the hot path may wish to keep
/// the result for the duration of a decide / update batch.
pub fn concat_features(x_context: &[f64], x_option: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(x_context.len() + x_option.len());
    out.extend_from_slice(x_context);
    out.extend_from_slice(x_option);
    out
}

/// Validate context/option feature slices for shared-state LinUCB.
pub fn validate_shared_features(
    x_context: &[f64],
    x_option: &[f64],
    expected_d_context: usize,
    expected_d_option: usize,
) -> Result<(), String> {
    if x_context.len() != expected_d_context {
        return Err(format!(
            "context feature vector length {} does not match expected {}",
            x_context.len(),
            expected_d_context
        ));
    }
    if x_option.len() != expected_d_option {
        return Err(format!(
            "option feature vector length {} does not match expected {}",
            x_option.len(),
            expected_d_option
        ));
    }
    for (i, v) in x_context.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("context feature[{}] is non-finite: {}", i, v));
        }
    }
    for (i, v) in x_option.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("option feature[{}] is non-finite: {}", i, v));
        }
    }
    Ok(())
}

// ── Linear algebra primitives ──

/// Matrix-vector multiply: y = A · x.
pub fn matvec(a: &[Vec<f64>], x: &[f64]) -> Vec<f64> {
    let m = a.len();
    let mut y = vec![0.0; m];
    for i in 0..m {
        let row = &a[i];
        let mut s = 0.0;
        for j in 0..x.len() {
            s += row[j] * x[j];
        }
        y[i] = s;
    }
    y
}

/// Vector dot product.
pub fn dot(x: &[f64], y: &[f64]) -> f64 {
    debug_assert_eq!(x.len(), y.len());
    let mut s = 0.0;
    for i in 0..x.len() {
        s += x[i] * y[i];
    }
    s
}

/// Gauss-Jordan matrix inversion. Returns None if singular.
/// O(d³). Use only for periodic rebuilds, not in the hot path.
pub fn gauss_jordan_invert(a: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    if n == 0 || a[0].len() != n {
        return None;
    }
    // Build augmented matrix [A | I]
    let mut aug = vec![vec![0.0; 2 * n]; n];
    for i in 0..n {
        for j in 0..n {
            aug[i][j] = a[i][j];
        }
        aug[i][n + i] = 1.0;
    }
    // Forward elimination with partial pivoting
    for k in 0..n {
        // Find pivot
        let mut max_row = k;
        let mut max_val = aug[k][k].abs();
        for r in (k + 1)..n {
            if aug[r][k].abs() > max_val {
                max_val = aug[r][k].abs();
                max_row = r;
            }
        }
        if max_val < 1e-12 {
            return None; // singular
        }
        aug.swap(k, max_row);
        // Scale row k
        let pivot = aug[k][k];
        for j in 0..(2 * n) {
            aug[k][j] /= pivot;
        }
        // Eliminate other rows
        for r in 0..n {
            if r != k {
                let factor = aug[r][k];
                for j in 0..(2 * n) {
                    aug[r][j] -= factor * aug[k][j];
                }
            }
        }
    }
    // Extract inverse from right half
    let mut inv = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            inv[i][j] = aug[i][n + j];
        }
    }
    Some(inv)
}

/// Cholesky factorisation L of symmetric PSD matrix M = L·Lᵀ. Returns
/// the lower-triangular L (rows i, cols 0..=i populated; upper triangle
/// left as zeros). Returns None if M is not strictly PSD (a diagonal
/// element drops to ≤ 0 during the decomposition).
pub fn cholesky(m: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = m.len();
    if n == 0 || m.iter().any(|row| row.len() != n) { return None; }
    let mut l = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = m[i][j];
            for k in 0..j { s -= l[i][k] * l[j][k]; }
            if i == j {
                if s <= 1e-12 { return None; }
                l[i][j] = s.sqrt();
            } else {
                l[i][j] = s / l[j][j];
            }
        }
    }
    Some(l)
}

/// Validate a feature vector at API boundary.
/// Returns Err with reason if any element is non-finite.
pub fn validate_features(x: &[f64], expected_d: usize) -> Result<(), String> {
    if x.len() != expected_d {
        return Err(format!(
            "feature vector length {} does not match expected {}",
            x.len(),
            expected_d
        ));
    }
    for (i, v) in x.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("feature[{}] is non-finite: {}", i, v));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_state_has_correct_dimensions() {
        let s = LinUcbState::new(5, 1.0);
        assert_eq!(s.d, 5);
        assert_eq!(s.b.len(), 5);
        assert_eq!(s.a_inv.len(), 5);
        assert_eq!(s.a_inv[0].len(), 5);
        // a_inv should start as (1/lambda) · I
        for i in 0..5 {
            for j in 0..5 {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!((s.a_inv[i][j] - expected).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn theta_starts_at_zero() {
        let s = LinUcbState::new(3, 1.0);
        let theta = s.theta();
        for v in &theta {
            assert!(v.abs() < 1e-9);
        }
    }

    #[test]
    fn ucb_score_uses_exploration_bonus_initially() {
        let s = LinUcbState::new(3, 1.0);
        let x = vec![1.0, 0.0, 0.0];
        let (score, _clamped) = s.ucb_score(&x, 1.0);
        // mean is 0 (theta = 0), bonus is alpha · sqrt(x · A_inv · x) = 1 · sqrt(1) = 1
        assert!((score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sherman_morrison_matches_full_inverse() {
        // After a few updates, check Sherman-Morrison's A_inv against a fresh
        // Gauss-Jordan inversion of the explicit A.
        let mut s = LinUcbState::new(3, 1.0);
        let xs = vec![
            vec![1.0, 0.5, 0.2],
            vec![0.3, 1.0, 0.1],
            vec![0.2, 0.1, 1.0],
            vec![0.7, 0.4, 0.3],
        ];
        let rs = vec![0.5, 0.8, 0.3, 0.6];
        for (x, r) in xs.iter().zip(rs.iter()) {
            s.update(x, *r);
        }
        // Compute A_inv from scratch
        let fresh_inv = gauss_jordan_invert(&s.a).unwrap();
        // Compare element-by-element
        for i in 0..3 {
            for j in 0..3 {
                let diff = (s.a_inv[i][j] - fresh_inv[i][j]).abs();
                assert!(diff < 1e-9, "[{},{}] sherman={} fresh={}", i, j, s.a_inv[i][j], fresh_inv[i][j]);
            }
        }
    }

    #[test]
    fn theta_recovers_linear_function() {
        // y = 2*x_0 + 3*x_1 - 1*x_2 (no noise).
        // After enough observations, theta should be close to [2, 3, -1].
        let mut s = LinUcbState::new(3, 0.001); // low regularization to converge fast
        let xs = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 1.0],
            vec![1.0, 1.0, 0.0],
            vec![1.0, 0.0, 1.0],
            vec![0.0, 1.0, 1.0],
            vec![1.0, 1.0, 1.0],
            vec![0.5, 0.5, 0.5],
        ];
        let true_theta = vec![2.0, 3.0, -1.0];
        for x in &xs {
            let r = dot(x, &true_theta);
            s.update(x, r);
        }
        let estimated = s.theta();
        for i in 0..3 {
            assert!(
                (estimated[i] - true_theta[i]).abs() < 0.1,
                "theta[{}] = {}, expected {}",
                i, estimated[i], true_theta[i]
            );
        }
    }

    #[test]
    fn rebuild_after_many_updates_corrects_drift() {
        // Force drift by doing many updates, then rebuild, and verify A_inv
        // matches a fresh inversion.
        let mut s = LinUcbState::new(4, 1.0);
        let mut rng_state: u64 = 12345;
        let next_rand = |st: &mut u64| -> f64 {
            *st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*st >> 33) as u32 as f64 / (u32::MAX as f64 + 1.0)
        };
        for _ in 0..1500 {
            let x = vec![
                next_rand(&mut rng_state),
                next_rand(&mut rng_state),
                next_rand(&mut rng_state),
                next_rand(&mut rng_state),
            ];
            let r = next_rand(&mut rng_state);
            s.update(&x, r);
        }
        // Save the Sherman-Morrison version
        let before_rebuild = s.a_inv.clone();
        s.rebuild_inverse();
        assert_eq!(s.since_last_rebuild, 0);

        // Compare. They should be very close — drift after 1500 updates at d=4
        // is small but non-zero. Rebuild gives the precise value.
        let fresh = gauss_jordan_invert(&s.a).unwrap();
        for i in 0..4 {
            for j in 0..4 {
                let diff = (s.a_inv[i][j] - fresh[i][j]).abs();
                assert!(diff < 1e-12);
            }
        }
        // The before-rebuild version may have minor drift; we don't assert
        // that the drift is large (it usually isn't at d=4), but the rebuild
        // is unambiguously correct.
        let _ = before_rebuild;
    }

    #[test]
    fn rebuild_due_flags_correctly() {
        let mut s = LinUcbState::new(3, 1.0);
        let x = vec![0.5, 0.5, 0.5];
        assert!(!s.rebuild_due(1000));
        for _ in 0..1000 {
            s.update(&x, 0.5);
        }
        assert!(s.rebuild_due(1000));
        s.rebuild_inverse();
        assert!(!s.rebuild_due(1000));
    }

    #[test]
    fn ucb_score_clamps_blowup() {
        // Construct a degenerate state where A_inv has a huge diagonal element.
        let s = LinUcbState::new(2, 0.001);
        // Don't update with any observation — A_inv stays at (1/lambda)·I = 1000·I
        let x = vec![1.0, 0.0];
        let (score, clamped) = s.ucb_score(&x, 1.0);
        // raw bonus = 1.0 · sqrt(1000) ≈ 31.6
        // clamped at 10.0 · alpha = 10.0
        assert!(clamped);
        assert!((score - 10.0).abs() < 1e-9);
    }

    #[test]
    fn validate_features_rejects_non_finite() {
        let good = vec![1.0, 2.0, 3.0];
        assert!(validate_features(&good, 3).is_ok());

        let nan = vec![1.0, f64::NAN, 3.0];
        assert!(validate_features(&nan, 3).is_err());

        let inf = vec![1.0, f64::INFINITY, 3.0];
        assert!(validate_features(&inf, 3).is_err());

        let wrong_len = vec![1.0, 2.0];
        assert!(validate_features(&wrong_len, 3).is_err());
    }

    #[test]
    fn gauss_jordan_handles_identity() {
        let i3: Vec<Vec<f64>> = (0..3)
            .map(|i| (0..3).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
            .collect();
        let inv = gauss_jordan_invert(&i3).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                let expected = if i == j { 1.0 } else { 0.0 };
                assert!((inv[i][j] - expected).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn gauss_jordan_returns_none_for_singular() {
        // Singular: row 1 = 2 × row 0
        let singular = vec![
            vec![1.0, 2.0, 3.0],
            vec![2.0, 4.0, 6.0],
            vec![1.0, 1.0, 1.0],
        ];
        assert!(gauss_jordan_invert(&singular).is_none());
    }

    /// Deterministic Box-Muller standard-normal generator for tests.
    struct DetRng(u64);
    impl DetRng {
        fn next_u01(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // Avoid 0 to keep log() finite.
            let v = ((self.0 >> 32) as f64 / u32::MAX as f64).max(1e-12);
            v.min(1.0 - 1e-12)
        }
        fn next_normal(&mut self) -> f64 {
            let u1 = self.next_u01();
            let u2 = self.next_u01();
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }

    #[test]
    fn cholesky_factorises_identity() {
        let i = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 1.0],
        ];
        let l = cholesky(&i).unwrap();
        for row in 0..3 {
            for col in 0..3 {
                let want = if row == col { 1.0 } else { 0.0 };
                assert!((l[row][col] - want).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn cholesky_factorises_psd_and_reconstructs() {
        // Build M = L·Lᵀ from a known L (lower-triangular, positive diag).
        let l_truth: Vec<Vec<f64>> = vec![
            vec![2.0, 0.0, 0.0],
            vec![1.0, 3.0, 0.0],
            vec![0.5, 0.7, 1.5],
        ];
        let n = 3;
        let mut m = vec![vec![0.0; n]; n];
        for i in 0..n {
            for j in 0..n {
                let mut s = 0.0;
                for k in 0..n { s += l_truth[i][k] * l_truth[j][k]; }
                m[i][j] = s;
            }
        }
        let l = cholesky(&m).unwrap();
        // Recover M' = L·Lᵀ and check M' ≈ M.
        for i in 0..n {
            for j in 0..n {
                let mut s = 0.0;
                for k in 0..n { s += l[i][k] * l[j][k]; }
                assert!((m[i][j] - s).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn cholesky_rejects_non_psd() {
        // Negative diagonal makes M not PSD.
        let bad = vec![
            vec![1.0, 2.0],
            vec![2.0, 1.0],
        ];
        assert!(cholesky(&bad).is_none());
    }

    #[test]
    fn lin_ts_score_is_stochastic_around_posterior_mean() {
        // Build a state with some history so theta != 0.
        let mut state = LinUcbState::new(2, 1.0);
        state.update(&[1.0, 0.0], 0.8);
        state.update(&[0.0, 1.0], 0.2);
        state.update(&[1.0, 0.0], 0.9);
        state.update(&[0.0, 1.0], 0.1);

        let theta = state.theta();
        let x = [1.0, 0.0];
        let posterior_mean = dot(&x, &theta);

        let mut rng = DetRng(42);
        let mut samples = Vec::new();
        for _ in 0..100 {
            samples.push(state.lin_ts_score(&x, 0.5, || rng.next_normal()));
        }

        // Empirical mean should be near the posterior mean.
        let emp_mean: f64 = samples.iter().sum::<f64>() / samples.len() as f64;
        assert!((emp_mean - posterior_mean).abs() < 0.5,
            "lin_ts empirical mean {} too far from posterior mean {}",
            emp_mean, posterior_mean);

        // Samples should vary — not all identical.
        let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = samples.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(max - min > 1e-6, "lin_ts samples are not stochastic");
    }

    #[test]
    fn lin_ts_score_converges_with_data() {
        // True reward function: r = 0.9·x0 + 0.1·x1. Drive LinUcb's state
        // with that mapping and verify lin_ts_score's expected value
        // approaches 0.9 for x = [1, 0] as data accumulates.
        let mut state = LinUcbState::new(2, 1.0);
        for _ in 0..200 {
            state.update(&[1.0, 0.0], 0.9);
            state.update(&[0.0, 1.0], 0.1);
        }
        let x = [1.0, 0.0];
        let mut rng = DetRng(7);
        let mut mean = 0.0;
        let n = 200;
        for _ in 0..n {
            mean += state.lin_ts_score(&x, 0.1, || rng.next_normal());
        }
        mean /= n as f64;
        assert!((mean - 0.9).abs() < 0.05, "got {mean}");
    }

    // ─────────────────────────────────────────────────────────────────
    // Shared-state LinUCB tests.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn shared_init_state_has_correct_dimensions() {
        let s = LinUcbSharedState::new(5, 1.0);
        assert_eq!(s.d_total, 5);
        assert_eq!(s.b.len(), 5);
        assert_eq!(s.a.len(), 5);
        assert_eq!(s.a_inv.len(), 5);
        assert_eq!(s.a[0].len(), 5);
        assert_eq!(s.a_inv[0].len(), 5);
        assert_eq!(s.since_last_rebuild, 0);
        // a_inv = (1/λ)·I, a = λ·I, b = 0.
        for i in 0..5 {
            assert!(s.b[i].abs() < 1e-12);
            for j in 0..5 {
                let want_inv = if i == j { 1.0 } else { 0.0 };
                let want_a = if i == j { 1.0 } else { 0.0 };
                assert!((s.a_inv[i][j] - want_inv).abs() < 1e-9);
                assert!((s.a[i][j] - want_a).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn concat_features_concatenates_in_order() {
        let ctx = vec![1.0, 2.0];
        let opt = vec![3.0, 4.0];
        let joined = concat_features(&ctx, &opt);
        assert_eq!(joined, vec![1.0, 2.0, 3.0, 4.0]);

        // Empty option slice still produces a valid vector.
        let empty: Vec<f64> = vec![];
        assert_eq!(concat_features(&ctx, &empty), vec![1.0, 2.0]);
        // Empty context slice too.
        assert_eq!(concat_features(&empty, &opt), vec![3.0, 4.0]);
    }

    #[test]
    fn shared_options_differ_and_update_improves_own_score() {
        // d_context = 2, d_option = 2 → d_total = 4.
        // Two options with different option-features but the same context
        // should produce different UCB scores. Updating one with positive
        // reward should raise its posterior-mean prediction.
        let mut s = LinUcbSharedState::new(4, 1.0);
        let ctx = vec![1.0, 0.0];
        let opt_a = vec![1.0, 0.0];
        let opt_b = vec![0.0, 1.0];

        // Initial scores at α = 1.0. With θ = 0 and identical ‖x‖, A and B
        // have identical mean (0) and identical bonus — initial scores match.
        let (a_init, _) = s.shared_ucb_score(&ctx, &opt_a, 1.0);
        let (b_init, _) = s.shared_ucb_score(&ctx, &opt_b, 1.0);
        assert!((a_init - b_init).abs() < 1e-9);

        // Drive option A with positive reward.
        for _ in 0..5 {
            s.shared_update(&ctx, &opt_a, 1.0);
        }

        // The exploration bonus on A also shrinks (variance reduction in
        // the option-half AND the shared context-half), which can make the
        // total UCB score on A lower than on B — that's correct behaviour
        // for a high-α regime. The discriminating test is the POSTERIOR MEAN
        // (α = 0): A's predicted reward must exceed B's, since A was the
        // one rewarded.
        let (a_mean, _) = s.shared_ucb_score(&ctx, &opt_a, 0.0);
        let (b_mean, _) = s.shared_ucb_score(&ctx, &opt_b, 0.0);
        assert!(
            a_mean > b_mean,
            "A's mean {} should exceed B's mean {} after A was rewarded",
            a_mean, b_mean
        );
        // And A's mean should be clearly positive (close to 1, the reward).
        assert!(a_mean > 0.5, "A's mean {} should be > 0.5", a_mean);

        // Bonuses should now differ too: A's variance shrank more (it got
        // five updates), so A's UCB score component (bonus only) is lower
        // than B's. Verify by computing α=1 score minus α=0 score, which
        // isolates the bonus magnitude.
        let (a_ucb, _) = s.shared_ucb_score(&ctx, &opt_a, 1.0);
        let (b_ucb, _) = s.shared_ucb_score(&ctx, &opt_b, 1.0);
        let a_bonus = a_ucb - a_mean;
        let b_bonus = b_ucb - b_mean;
        assert!(
            a_bonus < b_bonus,
            "A's bonus {} should be smaller than B's bonus {} after A updates",
            a_bonus, b_bonus
        );
    }

    #[test]
    fn shared_state_generalises_to_unseen_option() {
        // The key property: if option C's embedding is a linear combination
        // of A's and B's embeddings, then C's predicted reward should be
        // the same linear combination of A's and B's predicted rewards.
        //
        // Setup: d_context = 0 (context is empty; pure action-embedding
        // generalisation), d_option = 2 → d_total = 2.
        // Train on options A = [1, 0] and B = [0, 1] with linear true
        // rewards r_A = 1.0, r_B = 0.5.
        // Then query unseen C = α·A + β·B = [0.4, 0.6] and verify
        // predicted r_C ≈ α·r_A + β·r_B = 0.4·1.0 + 0.6·0.5 = 0.7.
        let mut s = LinUcbSharedState::new(2, 0.01); // low λ → fast convergence
        let ctx: Vec<f64> = vec![];
        let opt_a = vec![1.0, 0.0];
        let opt_b = vec![0.0, 1.0];
        for _ in 0..50 {
            s.shared_update(&ctx, &opt_a, 1.0);
            s.shared_update(&ctx, &opt_b, 0.5);
        }
        // Read the posterior mean (no exploration bonus, α=0).
        let (pred_a, _) = s.shared_ucb_score(&ctx, &opt_a, 0.0);
        let (pred_b, _) = s.shared_ucb_score(&ctx, &opt_b, 0.0);
        let opt_c = vec![0.4, 0.6];
        let (pred_c, _) = s.shared_ucb_score(&ctx, &opt_c, 0.0);
        let expected_c = 0.4 * pred_a + 0.6 * pred_b;
        assert!(
            (pred_c - expected_c).abs() < 1e-9,
            "linear generalisation failed: pred_c={} expected={} (pred_a={}, pred_b={})",
            pred_c, expected_c, pred_a, pred_b
        );
        // Sanity: pred_a and pred_b should be close to their training rewards.
        assert!((pred_a - 1.0).abs() < 0.05, "pred_a={}", pred_a);
        assert!((pred_b - 0.5).abs() < 0.05, "pred_b={}", pred_b);
        // And pred_c should be near the true linear combination 0.7.
        assert!((pred_c - 0.7).abs() < 0.05, "pred_c={}", pred_c);
    }

    #[test]
    fn shared_lin_ts_score_stochastic_and_centered() {
        // Same idea as the per-option test: build a state with history,
        // then sample many times and check mean ≈ posterior mean and
        // variance > 0.
        let mut state = LinUcbSharedState::new(4, 1.0);
        let ctx = vec![1.0, 0.0];
        let opt = vec![1.0, 0.0];
        for _ in 0..20 {
            state.shared_update(&ctx, &opt, 0.8);
            state.shared_update(&ctx, &vec![0.0, 1.0], 0.2);
        }
        let (posterior_mean, _) = state.shared_ucb_score(&ctx, &opt, 0.0);

        let mut rng = DetRng(99);
        let mut samples = Vec::new();
        for _ in 0..150 {
            samples.push(state.shared_lin_ts_score(&ctx, &opt, 0.5, || rng.next_normal()));
        }
        let emp_mean: f64 = samples.iter().sum::<f64>() / samples.len() as f64;
        assert!(
            (emp_mean - posterior_mean).abs() < 0.5,
            "shared lin_ts empirical mean {} far from posterior mean {}",
            emp_mean, posterior_mean
        );
        let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = samples.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(max - min > 1e-6, "shared lin_ts samples are not stochastic");
    }

    #[test]
    fn validate_shared_features_catches_bad_inputs() {
        let ctx_good = vec![1.0, 2.0];
        let opt_good = vec![3.0, 4.0];
        assert!(validate_shared_features(&ctx_good, &opt_good, 2, 2).is_ok());

        // Wrong-length context.
        assert!(validate_shared_features(&vec![1.0], &opt_good, 2, 2).is_err());
        // Wrong-length option.
        assert!(validate_shared_features(&ctx_good, &vec![3.0, 4.0, 5.0], 2, 2).is_err());
        // NaN in context.
        assert!(validate_shared_features(&vec![1.0, f64::NAN], &opt_good, 2, 2).is_err());
        // NaN in option.
        assert!(validate_shared_features(&ctx_good, &vec![f64::NAN, 4.0], 2, 2).is_err());
        // +Inf in context.
        assert!(validate_shared_features(&vec![1.0, f64::INFINITY], &opt_good, 2, 2).is_err());
        // -Inf in option.
        assert!(validate_shared_features(&ctx_good, &vec![3.0, f64::NEG_INFINITY], 2, 2).is_err());
    }

    #[test]
    fn shared_rebuild_due_and_sherman_morrison_consistent() {
        // Sanity that the shared-state rebuild path mirrors the per-option one.
        let mut s = LinUcbSharedState::new(3, 1.0);
        let ctx = vec![0.5];
        let opt = vec![0.5, 0.5];
        assert!(!s.shared_rebuild_due(100));
        for _ in 0..100 {
            s.shared_update(&ctx, &opt, 0.3);
        }
        assert!(s.shared_rebuild_due(100));
        // Sherman-Morrison's a_inv should match a Gauss-Jordan inversion of a.
        let fresh = gauss_jordan_invert(&s.a).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                let diff = (s.a_inv[i][j] - fresh[i][j]).abs();
                assert!(diff < 1e-9, "[{},{}] drifted: {}", i, j, diff);
            }
        }
        s.shared_rebuild_inverse();
        assert_eq!(s.since_last_rebuild, 0);
        assert!(!s.shared_rebuild_due(100));
    }
}
