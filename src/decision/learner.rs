//! Online least squares over hashed sparse features.
//!
//! # Algorithm
//!
//! Updates follow NAG, the Normalized Adaptive Gradient method of Ross,
//! Mineiro and Langford, "Normalized Online Learning" (UAI 2013),
//! Algorithm 2, on the squared loss `L = (y_hat - y)^2 / 2` with an
//! importance weight `h > 0` per example. For each example `x`:
//!
//! 1. for every feature with `|x_i| > s_i`: `w_i <- w_i * s_i / |x_i|`
//!    (when `s_i > 0`), then `s_i <- |x_i|`;
//! 2. `y_hat = sum_i w_i x_i`;
//! 3. `t <- t + h` and `N <- N + h * sum_i x_i^2 / s_i^2`;
//! 4. with `g_i = h (y_hat - y) x_i`: `G_i <- G_i + g_i^2` and
//!    `w_i <- w_i - lr * sqrt(t / N) * g_i / (s_i * sqrt(G_i))`.
//!
//! With `h = 1` this is Algorithm 2 verbatim. A weighted example is treated
//! as AdaGrad on the weighted loss `h * L`, and `t` and `N` both accumulate
//! `h` so that `t / N` stays the inverse of the (weighted) mean normalized
//! squared norm. Step 1 uses the linear ratio `s_i / |x_i|` of Algorithm 2:
//! a NAG weight carries one factor of `1 / s_i` (the AdaGrad term supplies
//! the other), so rescaling by `s_i / |x_i|` makes past updates look as if
//! the new scale had been known from the start. (The squared ratio belongs
//! to the non-adaptive NG variant, Algorithm 1.) The result is invariant to
//! rescaling any feature by a constant.
//!
//! # Representation
//!
//! Each slot stores `u_i = w_i * s_i` and `a_i = G_i / s_i^2` instead of `w_i`
//! and `G_i`. In these units step 1 leaves `u_i` unchanged and multiplies
//! `a_i` by `(s_i / |x_i|)^2`, the prediction is `sum_i u_i * (x_i / s_i)`,
//! and step 4 reads `a_i <- a_i + (h d x_i / s_i)^2`,
//! `u_i <- u_i - lr * sqrt(t / N) * (h d x_i / s_i) / sqrt(a_i)` with
//! `d = y_hat - y`. The two forms are algebraically identical, but every
//! stored quantity is now dimensionless, so `f32` storage neither overflows
//! for huge feature values nor underflows for tiny ones. All arithmetic is
//! done in `f64`; only the stored state is `f32`, which halves memory for
//! the dense `2^bits` arrays.

use sha2::{Digest, Sha256};

use super::features::{canonicalize, saturate_f32};

/// Smallest supported number of hash bits.
pub const MIN_BITS: u32 = 10;
/// Largest supported number of hash bits.
pub const MAX_BITS: u32 = 24;

const MAGIC: [u8; 8] = *b"SYNTRALM";
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 52;
const ENTRY_LEN: usize = 16;
const CHECKSUM_LEN: usize = 32;

/// Slot layout: normalized weight `u = w * s`, scale `s = max |x|`,
/// normalized AdaGrad accumulator `a = G / s^2`.
type Slot = [f32; 3];
const U: usize = 0;
const S: usize = 1;
const A: usize = 2;

/// A linear model `r_hat = w . x` over `2^bits` hashed slots, trained
/// online with NAG. See the module documentation.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearModel {
    bits: u32,
    learning_rate: f64,
    slots: Vec<Slot>,
    n_updates: u64,
    /// `t`: sum of importance weights.
    total_weight: f64,
    /// `N`: importance-weighted sum of normalized squared norms.
    norm_sum: f64,
}

impl LinearModel {
    /// An untrained model. `bits` must be in `[10, 24]` and `learning_rate`
    /// finite and positive.
    pub fn new(bits: u32, learning_rate: f64) -> Result<Self, String> {
        check_bits(bits)?;
        check_learning_rate(learning_rate)?;
        Ok(Self {
            bits,
            learning_rate,
            // All-zero `f32` arrays come from the zeroing allocator, so
            // untouched slots cost no resident memory.
            slots: vec![[0.0; 3]; 1 << bits],
            n_updates: 0,
            total_weight: 0.0,
            norm_sum: 0.0,
        })
    }

    pub fn bits(&self) -> u32 {
        self.bits
    }

    pub fn learning_rate(&self) -> f64 {
        self.learning_rate
    }

    /// Change the learning rate used by future updates.
    pub fn set_learning_rate(&mut self, learning_rate: f64) -> Result<(), String> {
        check_learning_rate(learning_rate)?;
        self.learning_rate = learning_rate;
        Ok(())
    }

    /// Number of calls to [`Self::update`] that have been applied.
    pub fn n_updates(&self) -> u64 {
        self.n_updates
    }

    /// Sum of the importance weights of all updates (`t` in the paper).
    pub fn total_weight(&self) -> f64 {
        self.total_weight
    }

    /// The weight `w_i` of a slot, in the units of the raw feature values.
    /// Slot indices wrap modulo `2^bits`.
    pub fn weight(&self, slot: u32) -> f64 {
        let s = self.slots[self.slot_index(slot)];
        if s[S] > 0.0 {
            f64::from(s[U]) / f64::from(s[S])
        } else {
            0.0
        }
    }

    /// Number of slots that have seen a nonzero feature value.
    pub fn active_slots(&self) -> usize {
        self.slots.iter().filter(|s| s[S] > 0.0).count()
    }

    fn slot_index(&self, slot: u32) -> usize {
        (slot as usize) & ((1usize << self.bits) - 1)
    }

    /// Prediction `sum_i w_i x_i` for a sparse vector of `(slot, value)`
    /// pairs. Slots wrap modulo `2^bits`; repeated slots add up. Values are
    /// expected to be finite, as [`super::features::Featurizer`] produces.
    pub fn predict(&self, x: &[(u32, f32)]) -> f64 {
        x.iter()
            .map(|&(slot, value)| {
                let s = self.slots[self.slot_index(slot)];
                if s[S] > 0.0 {
                    f64::from(s[U]) * (f64::from(value) / f64::from(s[S]))
                } else {
                    // Never-seen slot: its weight is zero.
                    0.0
                }
            })
            .sum()
    }

    /// One NAG step on the squared loss towards `target` with the given
    /// importance weight. Slots wrap modulo `2^bits`; repeated slots are
    /// summed first. Rejects non-finite inputs and non-positive weights
    /// without changing the model.
    pub fn update(
        &mut self,
        x: &[(u32, f32)],
        target: f64,
        importance_weight: f64,
    ) -> Result<(), String> {
        if !target.is_finite() {
            return Err(format!("update target must be finite (got {target})"));
        }
        if !(importance_weight.is_finite() && importance_weight > 0.0) {
            return Err(format!(
                "importance weight must be finite and positive (got {importance_weight})"
            ));
        }
        if let Some(&(slot, value)) = x.iter().find(|(_, v)| !v.is_finite()) {
            return Err(format!("feature slot {slot} has non-finite value {value}"));
        }
        let mut xs: Vec<(u32, f32)> = x
            .iter()
            .map(|&(slot, v)| (self.slot_index(slot) as u32, v))
            .collect();
        canonicalize(&mut xs);
        let h = importance_weight;

        // Step 1: grow scales. In normalized units the weight is unchanged
        // and the accumulator shrinks by (old / new)^2.
        for &(slot, value) in &xs {
            let s = &mut self.slots[slot as usize];
            let magnitude = value.abs();
            if magnitude > s[S] {
                if s[S] > 0.0 {
                    let ratio = f64::from(s[S]) / f64::from(magnitude);
                    s[A] = saturate_f32(f64::from(s[A]) * ratio * ratio);
                }
                s[S] = magnitude;
            }
        }

        // Steps 2 and 3: prediction and normalizer, with scales updated.
        let mut prediction = 0.0;
        let mut norm = 0.0;
        for &(slot, value) in &xs {
            let s = self.slots[slot as usize];
            let xn = f64::from(value) / f64::from(s[S]);
            prediction += f64::from(s[U]) * xn;
            norm += xn * xn;
        }
        self.total_weight += h;
        self.norm_sum += h * norm;
        self.n_updates += 1;

        let residual = prediction - target;
        if residual == 0.0 || self.norm_sum <= 0.0 {
            return Ok(());
        }
        // Step 4.
        let rate = self.learning_rate * (self.total_weight / self.norm_sum).sqrt();
        for &(slot, value) in &xs {
            let s = &mut self.slots[slot as usize];
            let gradient = h * residual * (f64::from(value) / f64::from(s[S]));
            let accumulated = f64::from(s[A]) + gradient * gradient;
            let stored = saturate_f32(accumulated);
            if stored == 0.0 {
                // |gradient| < ~4e-23: no information, and AdaGrad would
                // otherwise turn it into a full-size sign step.
                continue;
            }
            s[A] = stored;
            s[U] = saturate_f32(f64::from(s[U]) - rate * gradient / accumulated.sqrt());
        }
        Ok(())
    }

    /// Serialize to a versioned, checksummed binary blob. Only slots that
    /// have been touched are written, in slot order, so equal models give
    /// equal bytes.
    ///
    /// Layout (little-endian): magic `SYNTRALM`, format version `u16`, bits
    /// `u8`, reserved `u8` (0), learning rate `f64`, update count `u64`,
    /// total weight `f64`, norm sum `f64`, entry count `u64`, then per entry
    /// slot `u32`, normalized weight `f32`, scale `f32`, normalized
    /// accumulator `f32`, and finally the SHA-256 of everything before it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let entries: Vec<(u32, Slot)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.iter().any(|&v| v != 0.0))
            .map(|(i, s)| (i as u32, *s))
            .collect();
        let mut out = Vec::with_capacity(HEADER_LEN + entries.len() * ENTRY_LEN + CHECKSUM_LEN);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.push(self.bits as u8);
        out.push(0);
        out.extend_from_slice(&self.learning_rate.to_le_bytes());
        out.extend_from_slice(&self.n_updates.to_le_bytes());
        out.extend_from_slice(&self.total_weight.to_le_bytes());
        out.extend_from_slice(&self.norm_sum.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_LEN);
        for (slot, s) in entries {
            out.extend_from_slice(&slot.to_le_bytes());
            for v in s {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        let checksum = Sha256::digest(&out);
        out.extend_from_slice(&checksum);
        out
    }

    /// Parse a blob written by [`Self::to_bytes`]. Corrupt, truncated or
    /// inconsistent input is rejected with an error; this never panics.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
            return Err(format!(
                "model snapshot is truncated ({} bytes)",
                bytes.len()
            ));
        }
        let (body, checksum) = bytes.split_at(bytes.len() - CHECKSUM_LEN);
        if Sha256::digest(body).as_slice() != checksum {
            return Err("model snapshot checksum mismatch: the data is corrupt".into());
        }
        let mut r = Reader {
            bytes: body,
            pos: 0,
        };
        if r.take::<8>()? != MAGIC {
            return Err("not a model snapshot (bad magic bytes)".into());
        }
        let version = u16::from_le_bytes(r.take()?);
        if version != FORMAT_VERSION {
            return Err(format!(
                "unsupported model snapshot version {version} (this build reads version {FORMAT_VERSION})"
            ));
        }
        let [bits, reserved] = r.take::<2>()?;
        let bits = u32::from(bits);
        check_bits(bits)?;
        if reserved != 0 {
            return Err("model snapshot has a nonzero reserved byte".into());
        }
        let learning_rate = f64::from_le_bytes(r.take()?);
        check_learning_rate(learning_rate)?;
        let n_updates = u64::from_le_bytes(r.take()?);
        let total_weight = f64::from_le_bytes(r.take()?);
        let norm_sum = f64::from_le_bytes(r.take()?);
        for (name, v) in [("total weight", total_weight), ("norm sum", norm_sum)] {
            if !(v.is_finite() && v >= 0.0) {
                return Err(format!("model snapshot has an invalid {name} ({v})"));
            }
        }
        let count = u64::from_le_bytes(r.take()?);
        let size = 1usize << bits;
        let remaining = body.len() - r.pos;
        if count > size as u64 || remaining != count as usize * ENTRY_LEN {
            return Err(format!(
                "model snapshot entry count {count} does not match its length"
            ));
        }
        let mut slots = vec![[0.0f32; 3]; size];
        let mut previous: Option<u32> = None;
        for _ in 0..count {
            let slot = u32::from_le_bytes(r.take()?);
            let values = [
                f32::from_le_bytes(r.take()?),
                f32::from_le_bytes(r.take()?),
                f32::from_le_bytes(r.take()?),
            ];
            if slot as usize >= size || previous.is_some_and(|p| slot <= p) {
                return Err(format!(
                    "model snapshot has an out-of-order or out-of-range slot {slot}"
                ));
            }
            if !values.iter().all(|v| v.is_finite()) || values[S] < 0.0 || values[A] < 0.0 {
                return Err(format!("model snapshot has invalid values in slot {slot}"));
            }
            slots[slot as usize] = values;
            previous = Some(slot);
        }
        Ok(Self {
            bits,
            learning_rate,
            slots,
            n_updates,
            total_weight,
            norm_sum,
        })
    }
}

fn check_bits(bits: u32) -> Result<(), String> {
    if (MIN_BITS..=MAX_BITS).contains(&bits) {
        Ok(())
    } else {
        Err(format!(
            "learner bits must be in [{MIN_BITS}, {MAX_BITS}] (got {bits})"
        ))
    }
}

fn check_learning_rate(learning_rate: f64) -> Result<(), String> {
    if learning_rate.is_finite() && learning_rate > 0.0 {
        Ok(())
    } else {
        Err(format!(
            "learning rate must be finite and positive (got {learning_rate})"
        ))
    }
}

/// Bounds-checked little-endian cursor.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let end = self.pos.checked_add(N).filter(|&e| e <= self.bytes.len());
        let Some(end) = end else {
            return Err("model snapshot is truncated".into());
        };
        let mut out = [0u8; N];
        out.copy_from_slice(&self.bytes[self.pos..end]);
        self.pos = end;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::rng::SplitMix64;

    fn model() -> LinearModel {
        LinearModel::new(12, 0.5).unwrap()
    }

    /// Recompute the checksum after editing a snapshot body, to reach the
    /// structural checks behind it.
    fn reseal(bytes: &mut Vec<u8>) {
        bytes.truncate(bytes.len() - CHECKSUM_LEN);
        let checksum = Sha256::digest(&bytes);
        bytes.extend_from_slice(&checksum);
    }

    #[test]
    fn new_validates_arguments() {
        assert!(LinearModel::new(9, 0.5).is_err());
        assert!(LinearModel::new(25, 0.5).is_err());
        assert!(LinearModel::new(10, 0.0).is_err());
        assert!(LinearModel::new(10, f64::NAN).is_err());
        let m = LinearModel::new(10, 0.5).unwrap();
        assert_eq!(m.slots.len(), 1024);
        assert_eq!(m.predict(&[(3, 1.0)]), 0.0);
    }

    #[test]
    fn first_update_matches_hand_computation() {
        // One feature with x = 2: scale 2, normalized x = 1, N = 1, t = 1.
        // Gradient (normalized) = 1 * (0 - 1) * 1 = -1, accumulator 1, so
        // u = 0.5 * 1 * 1 / 1 = 0.5 and w = u / s = 0.25, prediction 0.5.
        let mut m = model();
        m.update(&[(7, 2.0)], 1.0, 1.0).unwrap();
        assert_eq!(m.weight(7), 0.25);
        assert_eq!(m.predict(&[(7, 2.0)]), 0.5);
        assert_eq!(m.n_updates(), 1);
        assert_eq!(m.total_weight(), 1.0);
        // Second identical example: residual -0.5, accumulator 1.25,
        // u = 0.5 + 0.5 * 0.5 / sqrt(1.25).
        m.update(&[(7, 2.0)], 1.0, 1.0).unwrap();
        let u = 0.5 + 0.5 * 0.5 / 1.25f64.sqrt();
        assert!((m.predict(&[(7, 2.0)]) - u).abs() < 1e-7);
    }

    #[test]
    fn scale_growth_rescales_the_weight_linearly() {
        // After the first update w = 0.25 at scale 2. A value of 8 raises
        // the scale to 8; Algorithm 2 rescales w by 2 / 8 before predicting.
        let mut m = model();
        m.update(&[(7, 2.0)], 1.0, 1.0).unwrap();
        let before = m.slots[7];
        // Prediction inside the next update uses w = 0.25 * 2/8 = 0.0625,
        // so y_hat = 0.0625 * 8 = 0.5 = u. Target 0.5 means zero residual.
        m.update(&[(7, 8.0)], 0.5, 1.0).unwrap();
        let after = m.slots[7];
        assert_eq!(after[U], before[U], "normalized weight unchanged");
        assert_eq!(after[S], 8.0);
        assert_eq!(after[A], before[A] * (2.0f32 / 8.0).powi(2));
        assert_eq!(m.weight(7), 0.0625);
    }

    #[test]
    fn learns_a_linear_function() {
        // y = 0.3 + 0.5 a - 0.2 b + noise, a and b in [0, 1].
        let mut m = LinearModel::new(10, 0.5).unwrap();
        let mut rng = SplitMix64::new(3);
        for _ in 0..20_000 {
            let a = rng.next_f64() as f32;
            let b = rng.next_f64() as f32;
            let noise = (rng.next_f64() - 0.5) * 0.2;
            let y = 0.3 + 0.5 * f64::from(a) - 0.2 * f64::from(b) + noise;
            m.update(&[(0, 1.0), (1, a), (2, b)], y, 1.0).unwrap();
        }
        assert!((m.weight(0) - 0.3).abs() < 0.03, "bias {}", m.weight(0));
        assert!((m.weight(1) - 0.5).abs() < 0.03, "a {}", m.weight(1));
        assert!((m.weight(2) + 0.2).abs() < 0.03, "b {}", m.weight(2));
    }

    /// NAG is exactly invariant to scaling a feature by a power of two:
    /// every normalized quantity is bit-for-bit the same.
    #[test]
    fn power_of_two_scaling_is_bitwise_invariant() {
        let mut plain = LinearModel::new(10, 0.5).unwrap();
        let mut scaled = LinearModel::new(10, 0.5).unwrap();
        let factor = 1024.0f32;
        let mut rng = SplitMix64::new(11);
        for _ in 0..2000 {
            let v = rng.next_f64() as f32;
            let y = 0.2 + 0.6 * f64::from(v) + (rng.next_f64() - 0.5) * 0.1;
            plain.update(&[(0, 1.0), (1, v)], y, 1.0).unwrap();
            scaled.update(&[(0, 1.0), (1, v * factor)], y, 1.0).unwrap();
            assert_eq!(
                plain.predict(&[(0, 1.0), (1, v)]),
                scaled.predict(&[(0, 1.0), (1, v * factor)])
            );
        }
        assert_eq!(plain.weight(1), scaled.weight(1) * f64::from(factor));
    }

    #[test]
    fn tiny_and_huge_feature_values_stay_finite() {
        for magnitude in [1e-40f32, 1e-20, 1e20, 2e38] {
            let mut m = LinearModel::new(10, 0.5).unwrap();
            let mut rng = SplitMix64::new(5);
            for _ in 0..500 {
                let v = magnitude * (0.5 + rng.next_f64() as f32);
                let y = 0.4 * (f64::from(v) / f64::from(magnitude));
                m.update(&[(0, 1.0), (1, v)], y, 1.0).unwrap();
            }
            let p = m.predict(&[(0, 1.0), (1, magnitude)]);
            assert!(
                p.is_finite() && (p - 0.4).abs() < 0.1,
                "magnitude {magnitude}: {p}"
            );
            assert!(m.slots.iter().flatten().all(|v| v.is_finite()));
            LinearModel::from_bytes(&m.to_bytes()).expect("snapshot must round-trip");
        }
    }

    #[test]
    fn repeated_slots_are_merged_before_updating() {
        let mut a = model();
        let mut b = model();
        a.update(&[(5, 1.0), (5, 1.0), (9, 0.5)], 1.0, 1.0).unwrap();
        b.update(&[(9, 0.5), (5, 2.0)], 1.0, 1.0).unwrap();
        assert_eq!(a, b);
        // Slots wrap modulo 2^bits.
        let mut c = model();
        c.update(&[(5 + 4096, 2.0), (9, 0.5)], 1.0, 1.0).unwrap();
        assert_eq!(b, c);
    }

    #[test]
    fn importance_weight_scales_t_and_n() {
        let mut m = model();
        m.update(&[(1, 1.0), (2, 1.0)], 1.0, 3.0).unwrap();
        assert_eq!(m.total_weight(), 3.0);
        assert_eq!(m.norm_sum, 6.0);
        assert_eq!(m.n_updates(), 1);
    }

    #[test]
    fn rejects_invalid_updates_without_side_effects() {
        let mut m = model();
        m.update(&[(1, 1.0)], 0.5, 1.0).unwrap();
        let before = m.clone();
        assert!(m.update(&[(1, 1.0)], f64::NAN, 1.0).is_err());
        assert!(m.update(&[(1, 1.0)], f64::INFINITY, 1.0).is_err());
        assert!(m.update(&[(1, 1.0)], 0.5, 0.0).is_err());
        assert!(m.update(&[(1, 1.0)], 0.5, -1.0).is_err());
        assert!(m.update(&[(1, 1.0)], 0.5, f64::NAN).is_err());
        assert!(m.update(&[(1, f32::NAN)], 0.5, 1.0).is_err());
        assert!(m.update(&[(1, f32::INFINITY)], 0.5, 1.0).is_err());
        assert_eq!(m, before);
    }

    #[test]
    fn empty_and_zero_feature_updates_only_count() {
        let mut m = model();
        m.update(&[], 1.0, 1.0).unwrap();
        m.update(&[(4, 0.0)], 1.0, 1.0).unwrap();
        assert_eq!(m.n_updates(), 2);
        assert_eq!(m.active_slots(), 0);
        assert_eq!(m.predict(&[(4, 1.0)]), 0.0);
    }

    fn trained() -> LinearModel {
        let mut m = model();
        let mut rng = SplitMix64::new(9);
        for _ in 0..300 {
            let slot = (rng.next_u64() % 4096) as u32;
            let v = (rng.next_f64() * 4.0 - 2.0) as f32;
            m.update(&[(0, 1.0), (slot, v)], rng.next_f64(), 1.0 + rng.next_f64())
                .unwrap();
        }
        m
    }

    #[test]
    fn serialization_round_trips_exactly() {
        let m = trained();
        let bytes = m.to_bytes();
        let restored = LinearModel::from_bytes(&bytes).unwrap();
        assert_eq!(restored, m);
        assert_eq!(restored.to_bytes(), bytes, "encoding is canonical");
        let expected_len = HEADER_LEN + m.active_slots() * ENTRY_LEN + CHECKSUM_LEN;
        assert_eq!(bytes.len(), expected_len, "only touched slots are written");
        let empty = model();
        assert_eq!(LinearModel::from_bytes(&empty.to_bytes()).unwrap(), empty);
    }

    #[test]
    fn corrupt_snapshots_are_rejected() {
        let bytes = trained().to_bytes();
        for len in 0..bytes.len() {
            assert!(
                LinearModel::from_bytes(&bytes[..len]).is_err(),
                "truncated to {len}"
            );
        }
        for pos in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[pos] ^= 0x20;
            assert!(LinearModel::from_bytes(&flipped).is_err(), "flip at {pos}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(LinearModel::from_bytes(&longer).is_err());
    }

    #[test]
    fn structurally_invalid_snapshots_are_rejected() {
        let good = trained().to_bytes();
        let edit = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut b = good.clone();
            f(&mut b);
            reseal(&mut b);
            LinearModel::from_bytes(&b).unwrap_err()
        };
        assert!(edit(&|b| b[0] = b'X').contains("magic"));
        assert!(edit(&|b| b[8] = 2).contains("version 2"));
        assert!(edit(&|b| b[10] = 30).contains("bits"));
        assert!(edit(&|b| b[11] = 1).contains("reserved"));
        assert!(
            edit(&|b| b[12..20].copy_from_slice(&(-1.0f64).to_le_bytes()))
                .contains("learning rate")
        );
        assert!(
            edit(&|b| b[28..36].copy_from_slice(&f64::NAN.to_le_bytes())).contains("total weight")
        );
        assert!(
            edit(&|b| b[36..44].copy_from_slice(&(-2.0f64).to_le_bytes())).contains("norm sum")
        );
        assert!(
            edit(&|b| b[44..52].copy_from_slice(&u64::MAX.to_le_bytes())).contains("entry count")
        );
        // First entry: slot out of range, then a non-finite weight, then a
        // negative scale; swapping the first two entries breaks the order.
        assert!(edit(&|b| b[52..56].copy_from_slice(&5000u32.to_le_bytes())).contains("slot"));
        assert!(
            edit(&|b| b[56..60].copy_from_slice(&f32::INFINITY.to_le_bytes()))
                .contains("invalid values")
        );
        assert!(
            edit(&|b| b[60..64].copy_from_slice(&(-1.0f32).to_le_bytes()))
                .contains("invalid values")
        );
        let swapped = edit(&|b| {
            let first: Vec<u8> = b[52..68].to_vec();
            let second: Vec<u8> = b[68..84].to_vec();
            b[52..68].copy_from_slice(&second);
            b[68..84].copy_from_slice(&first);
        });
        assert!(swapped.contains("out-of-order"), "{swapped}");
    }

    #[test]
    fn learning_rate_can_change() {
        let mut m = model();
        assert!(m.set_learning_rate(0.0).is_err());
        m.set_learning_rate(0.1).unwrap();
        assert_eq!(m.learning_rate(), 0.1);
        let restored = LinearModel::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(restored.learning_rate(), 0.1);
    }
}
