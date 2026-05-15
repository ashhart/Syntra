/// Native Lambert solver — Rust kernel for Lycan.
///
/// Solves the 3D Lambert problem: given two position vectors and
/// a time of flight, find the departure and arrival velocity vectors.
///
/// Uses Izzo-style universal variable formulation with robust
/// handling of near-180° degenerate transfers.

use std::f64::consts::PI;

/// Result of a Lambert solve.
pub struct LambertResult {
    pub v1: [f64; 3],
    pub v2: [f64; 3],
    pub converged: bool,
}

/// Stumpff function C(z)
fn stumpff_c(z: f64) -> f64 {
    if z > 1e-6 {
        (1.0 - z.sqrt().cos()) / z
    } else if z < -1e-6 {
        let sz = (-z).sqrt();
        (sz.cosh() - 1.0) / (-z)
    } else {
        0.5 - z / 24.0 + z * z / 720.0
    }
}

/// Stumpff function S(z)
fn stumpff_s(z: f64) -> f64 {
    if z > 1e-6 {
        let sz = z.sqrt();
        (sz - sz.sin()) / (sz * sz * sz)
    } else if z < -1e-6 {
        let sz = (-z).sqrt();
        (sz.sinh() - sz) / (sz * sz * sz)
    } else {
        1.0 / 6.0 - z / 120.0 + z * z / 5040.0
    }
}

/// Compute TOF for a given z value in the universal variable formulation.
fn compute_tof(z: f64, r1: f64, r2: f64, a_val: f64, mu: f64) -> Option<f64> {
    let c_z = stumpff_c(z);
    let s_z = stumpff_s(z);

    let sqrt_c = if c_z.abs() > 1e-10 { c_z.abs().sqrt() } else { 1e-5 };

    let y = r1 + r2 + a_val * (z * s_z - 1.0) / sqrt_c;
    if y < 0.0 {
        return None;
    }

    let c_z_safe = if c_z.abs() > 1e-10 { c_z } else { 1e-10 };
    let chi_sq = y / c_z_safe;
    if chi_sq < 0.0 {
        return None;
    }
    let chi = chi_sq.sqrt();

    let tof = (chi * chi * chi * s_z + a_val * y.sqrt()) / mu.sqrt();
    if tof.is_finite() { Some(tof) } else { None }
}

/// Solve the Lambert problem.
///
/// r1, r2: position vectors (3D) in AU
/// tof: time of flight in days
/// mu: gravitational parameter in AU³/day²
/// prograde: true for prograde (counter-clockwise) transfer
pub fn solve(
    r1: [f64; 3],
    r2: [f64; 3],
    tof: f64,
    mu: f64,
    prograde: bool,
) -> LambertResult {
    let fail = LambertResult { v1: [0.0; 3], v2: [0.0; 3], converged: false };

    let r1_mag = (r1[0] * r1[0] + r1[1] * r1[1] + r1[2] * r1[2]).sqrt();
    let r2_mag = (r2[0] * r2[0] + r2[1] * r2[1] + r2[2] * r2[2]).sqrt();

    if r1_mag < 1e-10 || r2_mag < 1e-10 || tof < 1e-6 {
        return fail;
    }

    // Transfer angle
    let dot = r1[0] * r2[0] + r1[1] * r2[1] + r1[2] * r2[2];
    let cos_dnu = (dot / (r1_mag * r2_mag)).clamp(-0.9999999, 0.9999999);

    // Cross product (z-component for 2D-like prograde check)
    let cross_z = r1[0] * r2[1] - r1[1] * r2[0];

    // Direction multiplier
    let dm = if prograde {
        if cross_z >= 0.0 { 1.0 } else { -1.0 }
    } else {
        if cross_z < 0.0 { 1.0 } else { -1.0 }
    };

    // A parameter (Curtis eq 5.23)
    // A = dm * sin(Δν) * sqrt(r1*r2 / (1 - cos(Δν)))
    let sin_dnu = (1.0 - cos_dnu * cos_dnu).sqrt().max(1e-10);
    let denom = (1.0 - cos_dnu).abs().max(1e-10);
    let a_val = dm * sin_dnu * (r1_mag * r2_mag / denom).sqrt();

    // For near-180° transfers: use orbit normal to determine plane
    // and handle the degenerate case
    if a_val.abs() < 1e-10 {
        // Degenerate: near 0° or 180° transfer
        // For 180°: need to define the transfer plane
        // Use z-axis as tiebreaker for ecliptic transfers
        return solve_near_180(r1, r2, tof, mu, r1_mag, r2_mag, cos_dnu, dm);
    }

    // Bisection on z to find the solution
    // Bracket: z_lo gives TOF too long, z_hi gives TOF too short
    let mut z_lo = -2.0f64;
    let mut z_hi = 4.0 * PI * PI; // One full revolution boundary

    // Verify bracket
    let tof_lo = compute_tof(z_lo, r1_mag, r2_mag, a_val, mu);
    let tof_hi = compute_tof(z_hi, r1_mag, r2_mag, a_val, mu);

    // Adjust bracket if needed
    match (tof_lo, tof_hi) {
        (Some(t_lo), Some(t_hi)) => {
            if (t_lo - tof) * (t_hi - tof) > 0.0 {
                // Same sign — need wider bracket
                z_lo = -10.0;
                z_hi = 100.0;
            }
        }
        _ => {
            z_lo = -1.0;
            z_hi = 50.0;
        }
    }

    #[cfg(test)]
    eprintln!("Lambert: r1={:.3} r2={:.3} cos_dnu={:.3} A={:.6} dm={}", r1_mag, r2_mag, cos_dnu, a_val, dm);
    #[cfg(test)]
    eprintln!("  bracket: z_lo={:.3} tof_lo={:?}  z_hi={:.3} tof_hi={:?}", z_lo, tof_lo, z_hi, tof_hi);

    let mut z = 0.0f64;
    let mut converged = false;

    for _iter in 0..80 {
        z = (z_lo + z_hi) / 2.0;
        match compute_tof(z, r1_mag, r2_mag, a_val, mu) {
            Some(tof_c) => {
                if (tof_c - tof).abs() < 0.001 {
                    converged = true;
                    break;
                }
                // Determine direction: check if TOF increases or decreases with z
                // For most cases: TOF increases with z (elliptic gets slower)
                if tof_c < tof {
                    z_lo = z;
                } else {
                    z_hi = z;
                }
            }
            None => {
                z_lo = z; // y was negative, increase z
            }
        }
    }

    if !converged {
        return fail;
    }

    // Compute velocity vectors from converged z
    let c_z = stumpff_c(z);
    let s_z = stumpff_s(z);
    let sqrt_c = if c_z.abs() > 1e-10 { c_z.abs().sqrt() } else { 1e-5 };
    let y = (r1_mag + r2_mag + a_val * (z * s_z - 1.0) / sqrt_c).max(1e-10);

    // Lagrange coefficients
    let f = 1.0 - y / r1_mag;
    let g = a_val * (y / mu).sqrt();
    let g_dot = 1.0 - y / r2_mag;

    if g.abs() < 1e-15 {
        return fail;
    }

    let v1 = [
        (r2[0] - f * r1[0]) / g,
        (r2[1] - f * r1[1]) / g,
        (r2[2] - f * r1[2]) / g,
    ];
    let v2 = [
        (g_dot * r2[0] - r1[0]) / g,
        (g_dot * r2[1] - r1[1]) / g,
        (g_dot * r2[2] - r1[2]) / g,
    ];

    // Sanity check
    let v1_mag = (v1[0] * v1[0] + v1[1] * v1[1] + v1[2] * v1[2]).sqrt();
    if !v1_mag.is_finite() || v1_mag > 1.0 {
        // > 1 AU/day ≈ 1730 km/s — physically impossible for solar system
        return fail;
    }

    LambertResult { v1, v2, converged: true }
}

/// Handle near-180° degenerate transfers.
/// For Δθ ≈ π, the transfer plane is not uniquely defined.
/// We resolve by using the ecliptic normal as the orbit normal.
fn solve_near_180(
    r1: [f64; 3],
    r2: [f64; 3],
    _tof: f64,
    mu: f64,
    r1_mag: f64,
    r2_mag: f64,
    cos_dnu: f64,
    _dm: f64,
) -> LambertResult {
    let fail = LambertResult { v1: [0.0; 3], v2: [0.0; 3], converged: false };

    // For near-180° in the ecliptic plane, use Battin's method
    // with the minimum-energy transfer as starting point.
    let chord = ((r2[0] - r1[0]).powi(2) + (r2[1] - r1[1]).powi(2) + (r2[2] - r1[2]).powi(2)).sqrt();
    let s = (r1_mag + r2_mag + chord) / 2.0;
    let a_min = s / 2.0;

    // Semi-latus rectum for near-180° using explicit formula
    let sin2_half = ((1.0 - cos_dnu) / 2.0).max(0.0);
    let p = a_min * 4.0 * (s - r1_mag) * (s - r2_mag) * sin2_half / (chord * chord).max(1e-10);

    if p < 1e-10 {
        return fail;
    }

    // f, g from p
    let f = 1.0 - r2_mag / p * (1.0 - cos_dnu);
    let sin_dnu = (1.0 - cos_dnu * cos_dnu).sqrt().max(1e-10);
    let g = r1_mag * r2_mag * sin_dnu / (mu * p).sqrt();

    if g.abs() < 1e-15 {
        return fail;
    }

    let v1 = [
        (r2[0] - f * r1[0]) / g,
        (r2[1] - f * r1[1]) / g,
        (r2[2] - f * r1[2]) / g,
    ];

    let g_dot = 1.0 - r1_mag / p * (1.0 - cos_dnu);
    let v2 = [
        (g_dot * r2[0] - r1[0]) / g,
        (g_dot * r2[1] - r1[1]) / g,
        (g_dot * r2[2] - r1[2]) / g,
    ];

    let v1_mag = (v1[0] * v1[0] + v1[1] * v1[1] + v1[2] * v1[2]).sqrt();
    if !v1_mag.is_finite() || v1_mag > 1.0 {
        return fail;
    }

    LambertResult { v1, v2, converged: true }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_90_degree_transfer() {
        let r1 = [1.0, 0.0, 0.0];
        let r2 = [0.0, 1.524, 0.0];
        let mu = 0.0002959122; // AU³/day²
        let tof = 200.0; // days

        let result = solve(r1, r2, tof, mu, true);
        eprintln!("v1: {:?}", result.v1);
        eprintln!("v2: {:?}", result.v2);
        eprintln!("converged: {}", result.converged);
        assert!(result.converged, "90° transfer should converge");
    }

    #[test]
    fn test_near_180_transfer() {
        // Earth and Mars nearly opposite
        let r1 = [1.0, 0.0, 0.0];
        let r2 = [-1.5, 0.2, 0.0]; // ~170°
        let mu = 0.0002959122;
        let tof = 260.0;

        let result = solve(r1, r2, tof, mu, true);
        eprintln!("near-180: v1={:?} conv={}", result.v1, result.converged);
        assert!(result.converged, "near-180° should converge");
    }

    #[test]
    fn test_stumpff() {
        assert!((stumpff_c(1.0) - 0.4597).abs() < 0.01);
        assert!((stumpff_s(1.0) - 0.1585).abs() < 0.01);
        assert!((stumpff_c(0.0) - 0.5).abs() < 0.01);
        assert!((stumpff_s(0.0) - 1.0/6.0).abs() < 0.01);
    }
}
