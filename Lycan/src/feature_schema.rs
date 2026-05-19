use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Top-level context declaration in a capsule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContextSpec {
    /// Discrete context: an opaque string key. Backward-compatible default.
    Discrete,
    /// Feature-vector context: named features with declared types.
    Features { features: Vec<FeatureSpec> },
}

impl Default for ContextSpec {
    fn default() -> Self {
        ContextSpec::Discrete
    }
}

/// A single feature in a feature-vector context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeatureSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub feature_type: FeatureType,
}

impl FeatureSpec {
    /// Number of dimensions this feature contributes to the encoded vector.
    ///
    /// For `TimeSeries`, this equals the number of declared aggregations.
    pub fn encoded_dimension(&self) -> usize {
        self.feature_type.dimension()
    }

    /// Validate the feature spec. P95 needs `window_size >= 5`; Slope needs `>= 2`.
    pub fn validate(&self) -> Result<(), String> {
        match &self.feature_type {
            FeatureType::Continuous { range } => {
                if let Some([min, max]) = range {
                    if !(min < max) {
                        return Err(format!(
                            "feature '{}' has invalid range [{}, {}] (need min < max)",
                            self.name, min, max
                        ));
                    }
                }
                Ok(())
            }
            FeatureType::Categorical { values } => {
                if values.is_empty() {
                    return Err(format!("feature '{}' has no categorical values", self.name));
                }
                Ok(())
            }
            FeatureType::Cyclic { period } => {
                if *period <= 0.0 {
                    return Err(format!(
                        "feature '{}' has non-positive period {}",
                        self.name, period
                    ));
                }
                Ok(())
            }
            FeatureType::TimeSeries { window_size, aggregations } => {
                if *window_size == 0 {
                    return Err(format!(
                        "feature '{}' time-series window_size must be >= 1",
                        self.name
                    ));
                }
                if aggregations.is_empty() {
                    return Err(format!(
                        "feature '{}' time-series must declare at least one aggregation",
                        self.name
                    ));
                }
                for agg in aggregations {
                    match agg {
                        Aggregation::P95 => {
                            if *window_size < 5 {
                                return Err(format!(
                                    "feature '{}' aggregation 'p95' requires window_size >= 5, got {}",
                                    self.name, window_size
                                ));
                            }
                        }
                        Aggregation::Slope => {
                            if *window_size < 2 {
                                return Err(format!(
                                    "feature '{}' aggregation 'slope' requires window_size >= 2, got {}",
                                    self.name, window_size
                                ));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(())
            }
        }
    }
}

/// Feature type declared in a capsule's context spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeatureType {
    /// Continuous numeric, optionally normalized to [0, 1] via range.
    Continuous { range: Option<[f64; 2]> },
    /// Categorical, one-hot encoded with (n-1) dimensions (reference category dropped).
    Categorical { values: Vec<String> },
    /// Cyclic numeric (time-of-day, day-of-year). Encoded as (sin, cos).
    Cyclic { period: f64 },
    /// Rolling-window time series. Encoded via the declared aggregations.
    TimeSeries {
        window_size: usize,
        aggregations: Vec<Aggregation>,
    },
}

impl FeatureType {
    /// Number of dimensions this feature produces in the encoded vector.
    pub fn dimension(&self) -> usize {
        match self {
            FeatureType::Continuous { .. } => 1,
            FeatureType::Categorical { values } => values.len().saturating_sub(1).max(0),
            FeatureType::Cyclic { .. } => 2,
            FeatureType::TimeSeries { aggregations, .. } => aggregations.len(),
        }
    }
}

/// Rolling-window aggregation. Percentiles use linear interpolation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    /// Arithmetic mean.
    Mean,
    /// Maximum value in the window.
    Max,
    /// Minimum value in the window.
    Min,
    /// 95th percentile (linear interpolation on a sorted copy).
    P95,
    /// 50th percentile / median (linear interpolation on a sorted copy).
    P50,
    /// Least-squares slope of `(index, value)` pairs in insertion order.
    Slope,
}

/// Rolling fixed-capacity window of numeric observations per
/// `(capsule, feature_name)` pair.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSeriesWindow {
    /// Buffered observations, oldest first.
    pub values: VecDeque<f64>,
    pub max_size: usize,
}

impl TimeSeriesWindow {
    /// Construct an empty window with the given capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Append a new observation. If the window is at capacity the oldest
    /// observation is dropped first, so the buffer always honours `max_size`.
    pub fn push(&mut self, v: f64) {
        if self.max_size == 0 {
            // Degenerate but well-defined: a zero-capacity window stays empty.
            return;
        }
        if self.values.len() >= self.max_size {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    /// Compute a single aggregation over the current window.
    ///
    /// Empty windows produce `0.0` for every variant. Slope on a window with
    /// fewer than two points is also `0.0` (no line is defined by a single
    /// point). All variants are total and never panic.
    pub fn aggregate(&self, agg: &Aggregation) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        match agg {
            Aggregation::Mean => {
                let sum: f64 = self.values.iter().sum();
                sum / self.values.len() as f64
            }
            Aggregation::Max => self
                .values
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
            Aggregation::Min => self
                .values
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min),
            Aggregation::P50 => percentile(&self.values, 50.0),
            Aggregation::P95 => percentile(&self.values, 95.0),
            Aggregation::Slope => slope(&self.values),
        }
    }

    /// Convenience: compute all declared aggregations in order, producing a
    /// vector of length `aggs.len()` suitable for direct concatenation into
    /// the encoded feature vector.
    pub fn aggregate_all(&self, aggs: &[Aggregation]) -> Vec<f64> {
        aggs.iter().map(|a| self.aggregate(a)).collect()
    }

    /// Serialize to JSON using the project's camelCase convention.
    ///
    /// Shape: `{"values": [..], "maxSize": N}`.
    pub fn serialize(&self) -> serde_json::Value {
        serde_json::json!({
            "values": self.values.iter().copied().collect::<Vec<f64>>(),
            "maxSize": self.max_size,
        })
    }

    /// Deserialize from the same JSON shape produced by [`serialize`].
    pub fn deserialize(json: &serde_json::Value) -> Result<Self, String> {
        let obj = json
            .as_object()
            .ok_or_else(|| "time-series window: expected JSON object".to_string())?;
        let max_size = obj
            .get("maxSize")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| "time-series window: missing or invalid 'maxSize'".to_string())?
            as usize;
        let values_json = obj
            .get("values")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "time-series window: missing or invalid 'values'".to_string())?;
        let mut values = VecDeque::with_capacity(values_json.len());
        for v in values_json {
            let n = v
                .as_f64()
                .ok_or_else(|| "time-series window: non-numeric entry in 'values'".to_string())?;
            values.push_back(n);
        }
        Ok(Self { values, max_size })
    }
}

/// Linear-interpolation percentile over an unordered buffer.
///
/// `pct` is in [0, 100]. The buffer is cloned and sorted in ascending order;
/// the index `(pct/100) * (n-1)` is split into integer and fractional parts
/// and a linear blend of the bracketing values is returned. Matches the
/// "linear" / "type 7" definition used by numpy and pandas by default.
fn percentile(values: &VecDeque<f64>, pct: f64) -> f64 {
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return values[0];
    }
    let mut sorted: Vec<f64> = values.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (pct / 100.0) * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Least-squares slope of `(i, values[i])` pairs in insertion order.
///
/// Returns `0.0` when fewer than two points are available or when the x
/// variance is zero (which only happens with `n < 2` here).
fn slope(values: &VecDeque<f64>) -> f64 {
    let n = values.len();
    if n < 2 {
        return 0.0;
    }
    let n_f = n as f64;
    let mean_x = (n_f - 1.0) / 2.0;
    let mean_y: f64 = values.iter().sum::<f64>() / n_f;
    let mut num = 0.0_f64;
    let mut den = 0.0_f64;
    for (i, y) in values.iter().enumerate() {
        let dx = i as f64 - mean_x;
        num += dx * (y - mean_y);
        den += dx * dx;
    }
    if den.abs() < 1e-12 {
        return 0.0;
    }
    num / den
}

impl ContextSpec {
    /// Total encoded dimension for this context spec.
    /// For Discrete: 0 (no feature vector).
    /// For Features: sum of feature dimensions, plus 1 for bias term.
    pub fn encoded_dimension(&self) -> usize {
        match self {
            ContextSpec::Discrete => 0,
            ContextSpec::Features { features } => {
                features.iter().map(|f| f.encoded_dimension()).sum::<usize>() + 1
            }
        }
    }

    /// Encode a HashMap of feature values into a fixed-length vector.
    ///
    /// The output's length equals `encoded_dimension()`. The final element is
    /// always `1.0` (bias term).
    ///
    /// For `TimeSeries` features no window is supplied here, so each declared
    /// aggregation is emitted as `0.0`. This is the single-shot fallback used
    /// by tests and by capsules that have not yet accumulated observations;
    /// production code should call [`ContextSpec::encode_with_windows`].
    pub fn encode(&self, values: &HashMap<String, FeatureValue>) -> Result<Vec<f64>, String> {
        let empty: HashMap<String, &TimeSeriesWindow> = HashMap::new();
        self.encode_with_windows(values, &empty)
    }

    /// Encode like [`encode`] but draw `TimeSeries` features from per-feature
    /// rolling windows. Pure; missing windows emit zeros.
    pub fn encode_with_windows(
        &self,
        values: &HashMap<String, FeatureValue>,
        windows: &HashMap<String, &TimeSeriesWindow>,
    ) -> Result<Vec<f64>, String> {
        match self {
            ContextSpec::Discrete => Ok(vec![]),
            ContextSpec::Features { features } => {
                let mut out = Vec::with_capacity(self.encoded_dimension());
                for spec in features {
                    match &spec.feature_type {
                        FeatureType::TimeSeries { aggregations, .. } => {
                            if let Some(win) = windows.get(&spec.name) {
                                out.extend(win.aggregate_all(aggregations));
                            } else {
                                out.extend(std::iter::repeat(0.0).take(aggregations.len()));
                            }
                        }
                        _ => {
                            let value = values.get(&spec.name).ok_or_else(|| {
                                format!("missing feature '{}'", spec.name)
                            })?;
                            encode_one(spec, value, &mut out)?;
                        }
                    }
                }
                out.push(1.0); // bias term
                Ok(out)
            }
        }
    }
}

/// User-supplied feature value at the API boundary.
#[derive(Debug, Clone)]
pub enum FeatureValue {
    Number(f64),
    Category(String),
}

impl FeatureValue {
    pub fn from_json(j: &serde_json::Value) -> Option<Self> {
        if let Some(n) = j.as_f64() {
            return Some(FeatureValue::Number(n));
        }
        if let Some(s) = j.as_str() {
            return Some(FeatureValue::Category(s.to_string()));
        }
        None
    }
}

fn encode_one(spec: &FeatureSpec, value: &FeatureValue, out: &mut Vec<f64>) -> Result<(), String> {
    match (&spec.feature_type, value) {
        (FeatureType::Continuous { range }, FeatureValue::Number(n)) => {
            let v = if let Some([min, max]) = range {
                let span = max - min;
                if span.abs() < 1e-12 {
                    return Err(format!("feature '{}' has zero range [{}, {}]", spec.name, min, max));
                }
                ((n - min) / span).clamp(0.0, 1.0)
            } else {
                *n
            };
            out.push(v);
            Ok(())
        }
        (FeatureType::Categorical { values }, FeatureValue::Category(c)) => {
            // One-hot encoding, dropping the first level as reference.
            // values = ["a", "b", "c", "d"] → encoded as 3 dims: [is_b, is_c, is_d]
            let idx = values.iter().position(|v| v == c).ok_or_else(|| {
                format!("feature '{}' got value '{}', not in declared values {:?}", spec.name, c, values)
            })?;
            for (i, _) in values.iter().enumerate().skip(1) {
                out.push(if i == idx { 1.0 } else { 0.0 });
            }
            Ok(())
        }
        (FeatureType::Cyclic { period }, FeatureValue::Number(n)) => {
            if *period <= 0.0 {
                return Err(format!("feature '{}' has non-positive period {}", spec.name, period));
            }
            let angle = 2.0 * std::f64::consts::PI * (n / period);
            out.push(angle.sin());
            out.push(angle.cos());
            Ok(())
        }
        (FeatureType::TimeSeries { .. }, _) => {
            // Should never reach here: ContextSpec::encode_with_windows handles
            // TimeSeries variants explicitly and does not delegate to encode_one.
            Err(format!(
                "feature '{}' is a TimeSeries variant; use encode_with_windows",
                spec.name
            ))
        }
        (FeatureType::Continuous { .. }, FeatureValue::Category(_)) => {
            Err(format!("feature '{}' expects number, got category", spec.name))
        }
        (FeatureType::Categorical { .. }, FeatureValue::Number(_)) => {
            Err(format!("feature '{}' expects category, got number", spec.name))
        }
        (FeatureType::Cyclic { .. }, FeatureValue::Category(_)) => {
            Err(format!("feature '{}' expects number, got category", spec.name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val_num(n: f64) -> FeatureValue { FeatureValue::Number(n) }
    fn val_cat(s: &str) -> FeatureValue { FeatureValue::Category(s.to_string()) }

    #[test]
    fn discrete_encodes_to_empty() {
        let spec = ContextSpec::Discrete;
        assert_eq!(spec.encoded_dimension(), 0);
        let v = spec.encode(&HashMap::new()).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn continuous_feature_with_no_range_passes_through() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "x".into(),
                feature_type: FeatureType::Continuous { range: None },
            }],
        };
        assert_eq!(spec.encoded_dimension(), 2); // 1 feature + bias
        let mut values = HashMap::new();
        values.insert("x".into(), val_num(3.7));
        let v = spec.encode(&values).unwrap();
        assert_eq!(v, vec![3.7, 1.0]);
    }

    #[test]
    fn continuous_feature_with_range_normalizes() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "x".into(),
                feature_type: FeatureType::Continuous { range: Some([0.0, 10.0]) },
            }],
        };
        let mut values = HashMap::new();
        values.insert("x".into(), val_num(5.0));
        let v = spec.encode(&values).unwrap();
        assert_eq!(v, vec![0.5, 1.0]);
    }

    #[test]
    fn continuous_feature_with_range_clamps() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "x".into(),
                feature_type: FeatureType::Continuous { range: Some([0.0, 10.0]) },
            }],
        };
        let mut values = HashMap::new();
        values.insert("x".into(), val_num(15.0));
        let v = spec.encode(&values).unwrap();
        assert_eq!(v[0], 1.0); // clamped to 1.0
    }

    #[test]
    fn categorical_one_hot_drops_first_level() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "cat".into(),
                feature_type: FeatureType::Categorical {
                    values: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                },
            }],
        };
        assert_eq!(spec.encoded_dimension(), 4); // 3 one-hot + bias
        let mut values = HashMap::new();
        values.insert("cat".into(), val_cat("c"));
        let v = spec.encode(&values).unwrap();
        // "a" is reference (dropped). For "c", we expect [is_b=0, is_c=1, is_d=0, bias=1]
        assert_eq!(v, vec![0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn categorical_reference_level_is_all_zeros() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "cat".into(),
                feature_type: FeatureType::Categorical {
                    values: vec!["a".into(), "b".into(), "c".into()],
                },
            }],
        };
        let mut values = HashMap::new();
        values.insert("cat".into(), val_cat("a"));
        let v = spec.encode(&values).unwrap();
        assert_eq!(v, vec![0.0, 0.0, 1.0]); // a is reference → both dims zero, plus bias
    }

    #[test]
    fn cyclic_feature_produces_two_dims() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "hour".into(),
                feature_type: FeatureType::Cyclic { period: 24.0 },
            }],
        };
        assert_eq!(spec.encoded_dimension(), 3); // sin + cos + bias
        let mut values = HashMap::new();
        values.insert("hour".into(), val_num(6.0)); // 6:00 = 90 degrees
        let v = spec.encode(&values).unwrap();
        // sin(π/2) = 1, cos(π/2) = 0 (within FP tolerance)
        assert!((v[0] - 1.0).abs() < 1e-9);
        assert!(v[1].abs() < 1e-9);
        assert_eq!(v[2], 1.0); // bias
    }

    #[test]
    fn mixed_features_encode_correctly() {
        let spec = ContextSpec::Features {
            features: vec![
                FeatureSpec {
                    name: "age".into(),
                    feature_type: FeatureType::Continuous { range: Some([0.0, 100.0]) },
                },
                FeatureSpec {
                    name: "country".into(),
                    feature_type: FeatureType::Categorical {
                        values: vec!["us".into(), "uk".into(), "de".into()],
                    },
                },
                FeatureSpec {
                    name: "hour".into(),
                    feature_type: FeatureType::Cyclic { period: 24.0 },
                },
            ],
        };
        // 1 (continuous) + 2 (categorical, drops first) + 2 (cyclic) + 1 (bias) = 6
        assert_eq!(spec.encoded_dimension(), 6);
        let mut values = HashMap::new();
        values.insert("age".into(), val_num(30.0));
        values.insert("country".into(), val_cat("uk"));
        values.insert("hour".into(), val_num(0.0));
        let v = spec.encode(&values).unwrap();
        assert_eq!(v.len(), 6);
        assert!((v[0] - 0.3).abs() < 1e-9);  // age 30 → 0.3
        assert_eq!(v[1], 1.0);                // is_uk = 1
        assert_eq!(v[2], 0.0);                // is_de = 0
        assert!(v[3].abs() < 1e-9);           // sin(0) = 0
        assert!((v[4] - 1.0).abs() < 1e-9);   // cos(0) = 1
        assert_eq!(v[5], 1.0);                // bias
    }

    #[test]
    fn missing_feature_returns_error() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "x".into(),
                feature_type: FeatureType::Continuous { range: None },
            }],
        };
        let result = spec.encode(&HashMap::new());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing feature 'x'"));
    }

    #[test]
    fn unknown_category_returns_error() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "cat".into(),
                feature_type: FeatureType::Categorical {
                    values: vec!["a".into(), "b".into()],
                },
            }],
        };
        let mut values = HashMap::new();
        values.insert("cat".into(), val_cat("z"));
        let result = spec.encode(&values);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in declared values"));
    }

    #[test]
    fn type_mismatch_returns_error() {
        let spec = ContextSpec::Features {
            features: vec![FeatureSpec {
                name: "x".into(),
                feature_type: FeatureType::Continuous { range: None },
            }],
        };
        let mut values = HashMap::new();
        values.insert("x".into(), val_cat("hello"));
        let result = spec.encode(&values);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expects number"));
    }

    #[test]
    fn json_roundtrip_through_serde() {
        let spec = ContextSpec::Features {
            features: vec![
                FeatureSpec {
                    name: "age".into(),
                    feature_type: FeatureType::Continuous { range: Some([0.0, 100.0]) },
                },
                FeatureSpec {
                    name: "country".into(),
                    feature_type: FeatureType::Categorical {
                        values: vec!["us".into(), "uk".into()],
                    },
                },
            ],
        };
        let json = serde_json::to_value(&spec).unwrap();
        let restored: ContextSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec, restored);
    }

    #[test]
    fn discrete_serializes_compactly() {
        let spec = ContextSpec::Discrete;
        let json = serde_json::to_value(&spec).unwrap();
        // Should be {"type": "discrete"}
        assert_eq!(json, serde_json::json!({"type": "discrete"}));
    }

    // ----- time-series tests -----

    fn ts_window_of(seq: &[f64], max_size: usize) -> TimeSeriesWindow {
        let mut w = TimeSeriesWindow::new(max_size);
        for v in seq {
            w.push(*v);
        }
        w
    }

    #[test]
    fn timeseries_push_respects_max_size_and_drops_oldest() {
        let mut w = TimeSeriesWindow::new(3);
        w.push(1.0);
        w.push(2.0);
        w.push(3.0);
        assert_eq!(w.values.len(), 3);
        w.push(4.0);
        assert_eq!(w.values.len(), 3);
        // Oldest (1.0) should have been evicted; window is [2, 3, 4].
        let collected: Vec<f64> = w.values.iter().copied().collect();
        assert_eq!(collected, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn timeseries_aggregations_on_one_through_ten() {
        let w = ts_window_of(&[1., 2., 3., 4., 5., 6., 7., 8., 9., 10.], 10);
        assert!((w.aggregate(&Aggregation::Mean) - 5.5).abs() < 1e-9);
        assert_eq!(w.aggregate(&Aggregation::Max), 10.0);
        assert_eq!(w.aggregate(&Aggregation::Min), 1.0);
        // P50 on a 10-element sorted sequence with linear interpolation:
        //   rank = 0.5 * 9 = 4.5 → blend of sorted[4]=5 and sorted[5]=6 → 5.5
        assert!((w.aggregate(&Aggregation::P50) - 5.5).abs() < 1e-9);
        // P95: rank = 0.95 * 9 = 8.55 → blend of sorted[8]=9 and sorted[9]=10 → 9.55
        assert!((w.aggregate(&Aggregation::P95) - 9.55).abs() < 1e-9);
        // Slope of y = i + 1 over i = 0..10 is 1.0 exactly.
        assert!((w.aggregate(&Aggregation::Slope) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn timeseries_empty_window_returns_zero_for_every_aggregation() {
        let w = TimeSeriesWindow::new(5);
        for a in &[
            Aggregation::Mean,
            Aggregation::Max,
            Aggregation::Min,
            Aggregation::P50,
            Aggregation::P95,
            Aggregation::Slope,
        ] {
            assert_eq!(w.aggregate(a), 0.0, "aggregation {:?} on empty window", a);
        }
    }

    #[test]
    fn timeseries_slope_on_single_point_is_zero() {
        let w = ts_window_of(&[42.0], 5);
        assert_eq!(w.aggregate(&Aggregation::Slope), 0.0);
    }

    #[test]
    fn timeseries_aggregate_all_preserves_order() {
        let w = ts_window_of(&[1.0, 2.0, 3.0], 3);
        let aggs = vec![Aggregation::Max, Aggregation::Min, Aggregation::Mean];
        let out = w.aggregate_all(&aggs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], 3.0);
        assert_eq!(out[1], 1.0);
        assert!((out[2] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn feature_spec_encoded_dimension_for_timeseries() {
        let spec = FeatureSpec {
            name: "latency".into(),
            feature_type: FeatureType::TimeSeries {
                window_size: 10,
                aggregations: vec![Aggregation::Mean, Aggregation::Max, Aggregation::Slope],
            },
        };
        assert_eq!(spec.encoded_dimension(), 3);
    }

    #[test]
    fn context_spec_encoded_dimension_includes_timeseries() {
        let spec = ContextSpec::Features {
            features: vec![
                FeatureSpec {
                    name: "x".into(),
                    feature_type: FeatureType::Continuous { range: None },
                },
                FeatureSpec {
                    name: "latency".into(),
                    feature_type: FeatureType::TimeSeries {
                        window_size: 10,
                        aggregations: vec![Aggregation::Mean, Aggregation::Max, Aggregation::P95],
                    },
                },
            ],
        };
        // 1 (continuous) + 3 (time-series) + 1 (bias) = 5
        assert_eq!(spec.encoded_dimension(), 5);
    }

    #[test]
    fn encode_falls_back_to_zeros_for_timeseries_without_windows() {
        let spec = ContextSpec::Features {
            features: vec![
                FeatureSpec {
                    name: "x".into(),
                    feature_type: FeatureType::Continuous { range: None },
                },
                FeatureSpec {
                    name: "latency".into(),
                    feature_type: FeatureType::TimeSeries {
                        window_size: 5,
                        aggregations: vec![Aggregation::Mean, Aggregation::Max],
                    },
                },
            ],
        };
        let mut values = HashMap::new();
        values.insert("x".into(), val_num(7.0));
        // No window supplied for "latency" → zeros.
        let v = spec.encode(&values).unwrap();
        assert_eq!(v.len(), 4); // 1 + 2 + bias
        assert_eq!(v[0], 7.0);
        assert_eq!(v[1], 0.0);
        assert_eq!(v[2], 0.0);
        assert_eq!(v[3], 1.0);
    }

    #[test]
    fn encode_with_windows_mixed_spec_produces_expected_vector() {
        let spec = ContextSpec::Features {
            features: vec![
                FeatureSpec {
                    name: "age".into(),
                    feature_type: FeatureType::Continuous { range: Some([0.0, 100.0]) },
                },
                FeatureSpec {
                    name: "latency".into(),
                    feature_type: FeatureType::TimeSeries {
                        window_size: 5,
                        aggregations: vec![Aggregation::Mean, Aggregation::Max],
                    },
                },
                FeatureSpec {
                    name: "country".into(),
                    feature_type: FeatureType::Categorical {
                        values: vec!["us".into(), "uk".into(), "de".into()],
                    },
                },
            ],
        };
        let mut values = HashMap::new();
        values.insert("age".into(), val_num(50.0));
        values.insert("country".into(), val_cat("uk"));

        let win = ts_window_of(&[1.0, 2.0, 3.0, 4.0, 5.0], 5);
        let mut windows: HashMap<String, &TimeSeriesWindow> = HashMap::new();
        windows.insert("latency".into(), &win);

        let v = spec.encode_with_windows(&values, &windows).unwrap();
        // 1 (continuous) + 2 (time-series) + 2 (categorical) + 1 (bias) = 6
        assert_eq!(v.len(), 6);
        assert!((v[0] - 0.5).abs() < 1e-9); // age 50 → 0.5
        assert!((v[1] - 3.0).abs() < 1e-9); // mean of 1..5 = 3
        assert!((v[2] - 5.0).abs() < 1e-9); // max  of 1..5 = 5
        assert_eq!(v[3], 1.0);              // is_uk
        assert_eq!(v[4], 0.0);              // is_de
        assert_eq!(v[5], 1.0);              // bias
    }

    #[test]
    fn timeseries_window_json_roundtrip() {
        let w = ts_window_of(&[1.5, 2.5, 3.5], 4);
        let json = w.serialize();
        assert_eq!(json["maxSize"], serde_json::json!(4));
        assert_eq!(json["values"], serde_json::json!([1.5, 2.5, 3.5]));
        let restored = TimeSeriesWindow::deserialize(&json).unwrap();
        assert_eq!(restored, w);
    }

    #[test]
    fn timeseries_window_zero_capacity_stays_empty() {
        let mut w = TimeSeriesWindow::new(0);
        w.push(1.0);
        w.push(2.0);
        assert!(w.values.is_empty());
    }

    #[test]
    fn feature_type_serializes_with_snake_case_kind() {
        let ft = FeatureType::TimeSeries {
            window_size: 5,
            aggregations: vec![Aggregation::Mean, Aggregation::P95],
        };
        let json = serde_json::to_value(&ft).unwrap();
        assert_eq!(json["kind"], serde_json::json!("time_series"));
        assert_eq!(json["window_size"], serde_json::json!(5));
        assert_eq!(json["aggregations"], serde_json::json!(["mean", "p95"]));
        let restored: FeatureType = serde_json::from_value(json).unwrap();
        assert_eq!(restored, ft);
    }

    #[test]
    fn validate_rejects_p95_with_small_window() {
        let spec = FeatureSpec {
            name: "lat".into(),
            feature_type: FeatureType::TimeSeries {
                window_size: 3,
                aggregations: vec![Aggregation::Mean, Aggregation::P95],
            },
        };
        let err = spec.validate().unwrap_err();
        assert!(err.contains("p95"), "expected p95 error, got: {}", err);
        assert!(err.contains("window_size >= 5"));
    }

    #[test]
    fn validate_rejects_slope_with_single_point_window() {
        let spec = FeatureSpec {
            name: "lat".into(),
            feature_type: FeatureType::TimeSeries {
                window_size: 1,
                aggregations: vec![Aggregation::Slope],
            },
        };
        let err = spec.validate().unwrap_err();
        assert!(err.contains("slope"), "expected slope error, got: {}", err);
    }

    #[test]
    fn validate_accepts_well_formed_timeseries() {
        let spec = FeatureSpec {
            name: "lat".into(),
            feature_type: FeatureType::TimeSeries {
                window_size: 10,
                aggregations: vec![Aggregation::Mean, Aggregation::P95, Aggregation::Slope],
            },
        };
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_aggregations() {
        let spec = FeatureSpec {
            name: "lat".into(),
            feature_type: FeatureType::TimeSeries {
                window_size: 10,
                aggregations: vec![],
            },
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_window_size() {
        let spec = FeatureSpec {
            name: "lat".into(),
            feature_type: FeatureType::TimeSeries {
                window_size: 0,
                aggregations: vec![Aggregation::Mean],
            },
        };
        assert!(spec.validate().is_err());
    }
}
