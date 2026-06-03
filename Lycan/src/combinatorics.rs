use serde::{Deserialize, Serialize};

pub const UNASSIGNED: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BadApKind {
    Monochromatic,
    Rainbow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BadAp {
    pub kind: BadApKind,
    pub terms: Vec<usize>,
    pub colors: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchStatus {
    Exists,
    Unsat,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColoringSearchResult {
    pub status: SearchStatus,
    pub n: usize,
    pub k: usize,
    pub max_colors: Option<usize>,
    pub nodes: usize,
    pub node_limit: usize,
    pub coloring: Option<Vec<usize>>,
}

pub fn arithmetic_progressions(n: usize, k: usize) -> Vec<Vec<usize>> {
    if k == 0 || n == 0 || k > n {
        return Vec::new();
    }
    let mut aps = Vec::new();
    for start in 1..=n {
        let mut step = 1;
        while start + (k - 1) * step <= n {
            aps.push((0..k).map(|i| start + i * step).collect());
            step += 1;
        }
    }
    aps
}

pub fn bad_arithmetic_progression(colors: &[usize], k: usize) -> Option<BadAp> {
    if k == 0 || k > colors.len() {
        return None;
    }
    for ap in arithmetic_progressions(colors.len(), k) {
        let ap_colors: Vec<usize> = ap.iter().map(|&term| colors[term - 1]).collect();
        if all_equal(&ap_colors) {
            return Some(BadAp {
                kind: BadApKind::Monochromatic,
                terms: ap,
                colors: ap_colors,
            });
        }
        if all_distinct(&ap_colors) {
            return Some(BadAp {
                kind: BadApKind::Rainbow,
                terms: ap,
                colors: ap_colors,
            });
        }
    }
    None
}

pub fn is_good_coloring(colors: &[usize], k: usize) -> bool {
    bad_arithmetic_progression(colors, k).is_none()
}

pub fn search_good_coloring(
    n: usize,
    k: usize,
    max_colors: Option<usize>,
    node_limit: usize,
) -> ColoringSearchResult {
    if k == 0 || n == 0 || node_limit == 0 {
        return ColoringSearchResult {
            status: SearchStatus::Inconclusive,
            n,
            k,
            max_colors,
            nodes: 0,
            node_limit,
            coloring: None,
        };
    }
    if let Some(0) = max_colors {
        return ColoringSearchResult {
            status: SearchStatus::Unsat,
            n,
            k,
            max_colors,
            nodes: 0,
            node_limit,
            coloring: None,
        };
    }

    let aps0: Vec<Vec<usize>> = arithmetic_progressions(n, k)
        .into_iter()
        .map(|ap| ap.into_iter().map(|term| term - 1).collect())
        .collect();
    let mut colors = vec![UNASSIGNED; n];
    let mut nodes = 0usize;
    let mut hit_limit = false;
    let witness = dfs_search(
        0,
        0,
        &mut colors,
        &aps0,
        max_colors,
        node_limit,
        &mut nodes,
        &mut hit_limit,
    );

    let status = if witness.is_some() {
        SearchStatus::Exists
    } else if hit_limit {
        SearchStatus::Inconclusive
    } else {
        SearchStatus::Unsat
    };

    ColoringSearchResult {
        status,
        n,
        k,
        max_colors,
        nodes,
        node_limit,
        coloring: witness,
    }
}

fn dfs_search(
    pos: usize,
    max_used: usize,
    colors: &mut [usize],
    aps0: &[Vec<usize>],
    max_colors: Option<usize>,
    node_limit: usize,
    nodes: &mut usize,
    hit_limit: &mut bool,
) -> Option<Vec<usize>> {
    if *nodes >= node_limit {
        *hit_limit = true;
        return None;
    }
    *nodes += 1;

    if pos == colors.len() {
        return Some(colors.to_vec());
    }

    let candidates: Vec<usize> = if pos == 0 {
        vec![0]
    } else if let Some(limit) = max_colors {
        (0..limit).collect()
    } else {
        (0..=max_used + 1).collect()
    };

    for color in candidates {
        if let Some(limit) = max_colors {
            if color >= limit {
                continue;
            }
        }
        colors[pos] = color;
        let next_max = max_used.max(color);
        if !partial_has_bad_complete_ap(colors, aps0) {
            if let Some(found) = dfs_search(
                pos + 1,
                next_max,
                colors,
                aps0,
                max_colors,
                node_limit,
                nodes,
                hit_limit,
            ) {
                return Some(found);
            }
        }
        colors[pos] = UNASSIGNED;
        if *hit_limit {
            return None;
        }
    }
    None
}

fn partial_has_bad_complete_ap(colors: &[usize], aps0: &[Vec<usize>]) -> bool {
    for ap in aps0 {
        let mut ap_colors = Vec::with_capacity(ap.len());
        let mut complete = true;
        for &idx in ap {
            let color = colors[idx];
            if color == UNASSIGNED {
                complete = false;
                break;
            }
            ap_colors.push(color);
        }
        if complete && (all_equal(&ap_colors) || all_distinct(&ap_colors)) {
            return true;
        }
    }
    false
}

fn all_equal(values: &[usize]) -> bool {
    values
        .first()
        .map(|first| values.iter().all(|value| value == first))
        .unwrap_or(false)
}

fn all_distinct(values: &[usize]) -> bool {
    for i in 0..values.len() {
        for j in i + 1..values.len() {
            if values[i] == values[j] {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Erdos #160 kernel: "rainbow-ish" 4-term arithmetic progressions.
//
// Problem #160 (erdosproblems.com/160, status OPEN): h(N) is the smallest k
// such that {1..N} can be k-coloured so that EVERY four-term arithmetic
// progression contains AT LEAST THREE DISTINCT COLOURS. The open question asks
// for the asymptotic growth of h(N); a finite search cannot resolve that and
// only produces small exact values plus certificates.
//
// This kernel uses a DIFFERENT predicate from #190: for #160 a 4-term AP is
// BAD iff it has at most 2 distinct colours (i.e. fewer than 3 distinct).
// ---------------------------------------------------------------------------

/// Number of distinct colours required in every 4-AP for a valid #160 colouring.
pub const ERDOS160_AP_LEN: usize = 4;

/// A 4-term arithmetic progression that is "bad" for Erdos #160, i.e. it
/// contains fewer than three distinct colours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BadAp160 {
    /// 1-based positions of the offending 4-term AP.
    pub terms: Vec<usize>,
    /// Colours on those positions.
    pub colors: Vec<usize>,
    /// Number of distinct colours present (always <= 2 for a violation).
    pub distinct_colors: usize,
}

/// Outcome of the per-N minimum-colour search for Erdos #160.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum H160Status {
    /// Both an upper-bound witness (c colours) and a complete `c-1` exhaustion
    /// were obtained, so h(N) = c exactly within this bounded run.
    Exact,
    /// A complete exhaustion proved no valid colouring exists with `lower_bound
    /// - 1` colours, but the search could not confirm a minimal witness without
    /// hitting the node limit. Establishes h(N) >= lower_bound only.
    LowerBound,
    /// The bounded search hit the node limit before resolving the value. No
    /// claim is made.
    Inconclusive,
}

/// Result of `h160`: a per-N minimum-colour determination for Erdos #160.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct H160Result {
    pub n: usize,
    pub status: H160Status,
    /// Exact h(N) value when `status == Exact`, else `None`.
    pub h: Option<usize>,
    /// Proven lower bound: h(N) >= lower_bound (always valid; equals `h` when Exact).
    pub lower_bound: usize,
    /// 1-based witness colouring achieving the upper bound, when known.
    pub witness_1_based: Option<Vec<usize>>,
    /// Number of distinct colours used by the witness, when known.
    pub witness_colors: Option<usize>,
    /// Total DFS nodes expanded across all colour counts tried.
    pub nodes: usize,
    pub node_limit: usize,
}

/// Return the first 4-term arithmetic progression in `colors` (1-based terms)
/// that is bad for Erdos #160 (fewer than three distinct colours), or `None`
/// if every 4-AP already has at least three distinct colours.
pub fn bad_arithmetic_progression_160(colors: &[usize]) -> Option<BadAp160> {
    let n = colors.len();
    for ap in arithmetic_progressions(n, ERDOS160_AP_LEN) {
        let ap_colors: Vec<usize> = ap.iter().map(|&term| colors[term - 1]).collect();
        let distinct = distinct_count(&ap_colors);
        if distinct < 3 {
            return Some(BadAp160 {
                terms: ap,
                colors: ap_colors,
                distinct_colors: distinct,
            });
        }
    }
    None
}

/// True iff `colors` is a valid Erdos #160 colouring: every 4-term AP has at
/// least three distinct colours. (Vacuously true when `colors.len() < 4`.)
pub fn is_good_coloring_160(colors: &[usize]) -> bool {
    bad_arithmetic_progression_160(colors).is_none()
}

/// Bounded exhaustive search for a valid #160 colouring of `{1..n}` using at
/// most `max_colors` colours. Uses canonical colour assignment (position 0 is
/// colour 0; position i may use a colour in `0..=min(max_colors-1, used+1)`)
/// and incrementally checks the 4-APs *ending at* each position to prune.
///
/// Returns `Exists` with a 0-based witness, `Unsat` after a complete search, or
/// `Inconclusive` if the node limit was reached first.
pub fn search_coloring_160(n: usize, max_colors: usize, node_limit: usize) -> ColoringSearchResult {
    if n == 0 || max_colors == 0 || node_limit == 0 {
        // n == 0: nothing to colour. max_colors == 0: no colours available.
        // node_limit == 0: cannot expand any node.
        let status = if node_limit == 0 {
            SearchStatus::Inconclusive
        } else if max_colors == 0 {
            SearchStatus::Unsat
        } else {
            // n == 0 with colours available and budget: vacuously satisfiable.
            SearchStatus::Exists
        };
        let coloring = if matches!(status, SearchStatus::Exists) {
            Some(Vec::new())
        } else {
            None
        };
        return ColoringSearchResult {
            status,
            n,
            k: ERDOS160_AP_LEN,
            max_colors: Some(max_colors),
            nodes: 0,
            node_limit,
            coloring,
        };
    }

    let mut colors = vec![UNASSIGNED; n];
    let mut nodes = 0usize;
    let mut hit_limit = false;
    let witness = dfs_search_160(
        0,
        0,
        &mut colors,
        max_colors,
        node_limit,
        &mut nodes,
        &mut hit_limit,
    );

    let status = if witness.is_some() {
        SearchStatus::Exists
    } else if hit_limit {
        SearchStatus::Inconclusive
    } else {
        SearchStatus::Unsat
    };

    ColoringSearchResult {
        status,
        n,
        k: ERDOS160_AP_LEN,
        max_colors: Some(max_colors),
        nodes,
        node_limit,
        coloring: witness,
    }
}

#[allow(clippy::too_many_arguments)]
fn dfs_search_160(
    pos: usize,
    max_used: usize,
    colors: &mut [usize],
    max_colors: usize,
    node_limit: usize,
    nodes: &mut usize,
    hit_limit: &mut bool,
) -> Option<Vec<usize>> {
    if *nodes >= node_limit {
        *hit_limit = true;
        return None;
    }
    *nodes += 1;

    if pos == colors.len() {
        return Some(colors.to_vec());
    }

    // Canonical colour assignment: never introduce colour `j+1` before colour
    // `j` has been used, and never exceed `max_colors` distinct colours.
    let ceiling = (max_used + 1).min(max_colors.saturating_sub(1));
    for color in 0..=ceiling {
        colors[pos] = color;
        // Incrementally check every 4-AP ending at `pos`; all earlier positions
        // are assigned, so each such AP is complete.
        if !bad_complete_ap_160_ending_at(colors, pos) {
            let next_max = max_used.max(color);
            if let Some(found) = dfs_search_160(
                pos + 1,
                next_max,
                colors,
                max_colors,
                node_limit,
                nodes,
                hit_limit,
            ) {
                return Some(found);
            }
        }
        colors[pos] = UNASSIGNED;
        if *hit_limit {
            return None;
        }
    }
    None
}

/// True iff some 4-term AP ending at position `pos` (0-based) is already bad for
/// #160 (fewer than three distinct colours). Assumes positions `0..=pos` are
/// assigned, so every such AP is complete.
fn bad_complete_ap_160_ending_at(colors: &[usize], pos: usize) -> bool {
    let mut step = 1;
    // AP is {pos - 3*step, pos - 2*step, pos - step, pos}.
    while pos >= 3 * step {
        let a = colors[pos - 3 * step];
        let b = colors[pos - 2 * step];
        let c = colors[pos - step];
        let d = colors[pos];
        if distinct_count(&[a, b, c, d]) < 3 {
            return true;
        }
        step += 1;
    }
    false
}

/// Minimum-colour search for Erdos #160 at a single `n`. Tries colour counts
/// `c = 1, 2, 3, ...` with a complete bounded search at each, returning:
///
/// - `Exact` h(N) = c when a `c`-colour witness is found and the `(c-1)`-colour
///   search was a complete exhaustion (Unsat),
/// - `LowerBound` when an exhaustion is proven but the matching witness search
///   exceeds the node limit,
/// - `Inconclusive` when the node limit is hit before any exhaustion resolves
///   the value.
///
/// Each colour count receives the full `node_limit` budget.
pub fn h160(n: usize, node_limit: usize) -> H160Result {
    let mut total_nodes = 0usize;
    // h(N) is at most N (give each position its own colour), and at most the
    // number of positions that can appear in a 4-AP; N is a safe ceiling.
    let ceiling = n.max(1);
    let mut lower_bound = 1usize; // h(N) >= 1 trivially.

    for c in 1..=ceiling {
        let result = search_coloring_160(n, c, node_limit);
        total_nodes += result.nodes;
        match result.status {
            SearchStatus::Exists => {
                let witness_1_based: Vec<usize> = result
                    .coloring
                    .as_ref()
                    .map(|colors| colors.iter().map(|color| color + 1).collect())
                    .unwrap_or_default();
                let witness_colors = result
                    .coloring
                    .as_ref()
                    .map(|colors| distinct_count(colors));
                // A witness at colour-count c means h(N) <= c. Because every
                // smaller count `c-1` was a complete Unsat (we only reach here
                // after exhausting them in increasing order), c is minimal.
                return H160Result {
                    n,
                    status: H160Status::Exact,
                    h: Some(c),
                    lower_bound: c,
                    witness_1_based: Some(witness_1_based),
                    witness_colors,
                    nodes: total_nodes,
                    node_limit,
                };
            }
            SearchStatus::Unsat => {
                // No valid colouring with c colours: h(N) > c, i.e. h(N) >= c+1.
                lower_bound = c + 1;
            }
            SearchStatus::Inconclusive => {
                // Hit the node limit before resolving colour-count c. We can
                // still report the strongest exhaustion proven so far, but no
                // exact value and no witness.
                let status = if lower_bound > 1 {
                    H160Status::LowerBound
                } else {
                    H160Status::Inconclusive
                };
                return H160Result {
                    n,
                    status,
                    h: None,
                    lower_bound,
                    witness_1_based: None,
                    witness_colors: None,
                    nodes: total_nodes,
                    node_limit,
                };
            }
        }
    }

    // Exhausted every colour count up to the ceiling with only Unsat results.
    // This is structurally impossible (c = n always succeeds), so report the
    // lower bound honestly rather than fabricating an exact value.
    H160Result {
        n,
        status: H160Status::LowerBound,
        h: None,
        lower_bound,
        witness_1_based: None,
        witness_colors: None,
        nodes: total_nodes,
        node_limit,
    }
}

/// Count of distinct values in a small slice.
fn distinct_count(values: &[usize]) -> usize {
    let mut seen: Vec<usize> = Vec::with_capacity(values.len());
    for &v in values {
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ap_tuples_are_one_based() {
        assert_eq!(
            arithmetic_progressions(5, 3),
            vec![vec![1, 2, 3], vec![1, 3, 5], vec![2, 3, 4], vec![3, 4, 5]]
        );
    }

    #[test]
    fn detects_monochromatic_and_rainbow_aps() {
        let mono = bad_arithmetic_progression(&[1, 1, 1], 3).unwrap();
        assert_eq!(mono.kind, BadApKind::Monochromatic);

        let rainbow = bad_arithmetic_progression(&[1, 2, 3], 3).unwrap();
        assert_eq!(rainbow.kind, BadApKind::Rainbow);
    }

    #[test]
    fn finds_h3_floor_witness_and_unsat_at_9() {
        let n8 = search_good_coloring(8, 3, None, 10_000);
        assert_eq!(n8.status, SearchStatus::Exists);
        assert!(is_good_coloring(n8.coloring.as_ref().unwrap(), 3));

        let n9 = search_good_coloring(9, 3, None, 10_000);
        assert_eq!(n9.status, SearchStatus::Unsat);
    }

    #[test]
    fn fixed_two_color_witness_avoids_rainbow_by_construction() {
        let result = search_good_coloring(8, 3, Some(2), 10_000);
        assert_eq!(result.status, SearchStatus::Exists);
        assert!(result.coloring.unwrap().iter().all(|&c| c < 2));
    }

    // ----- Erdos #160 kernel -----

    /// The published N=22 witness uses 4 colours and every 4-AP has >= 3
    /// distinct colours, so h(22) <= 4.
    const ERDOS160_N22_WITNESS: [usize; 22] = [
        1, 1, 3, 4, 2, 4, 1, 2, 1, 3, 3, 2, 4, 1, 2, 1, 3, 3, 2, 4, 1, 4,
    ];

    #[test]
    fn bad_160_detects_too_few_distinct_colours() {
        // {1,2,3,4} all one colour: 1 distinct < 3 -> bad.
        let bad = bad_arithmetic_progression_160(&[1, 1, 1, 1]).unwrap();
        assert_eq!(bad.terms, vec![1, 2, 3, 4]);
        assert_eq!(bad.distinct_colors, 1);
        // Exactly two distinct in the 4-AP is still bad for #160.
        assert!(bad_arithmetic_progression_160(&[1, 2, 1, 2]).is_some());
        // Three distinct -> good.
        assert!(bad_arithmetic_progression_160(&[1, 2, 3, 1]).is_none());
    }

    #[test]
    fn no_four_ap_means_vacuously_good_for_small_n() {
        // Fewer than 4 positions: no 4-AP exists, so any colouring is valid.
        assert!(is_good_coloring_160(&[1, 1, 1]));
        assert!(is_good_coloring_160(&[1]));
        assert!(is_good_coloring_160(&[]));
    }

    #[test]
    fn published_n22_witness_validates_with_four_colours() {
        assert!(is_good_coloring_160(&ERDOS160_N22_WITNESS));
        assert_eq!(distinct_count(&ERDOS160_N22_WITNESS), 4);
        assert!(bad_arithmetic_progression_160(&ERDOS160_N22_WITNESS).is_none());
    }

    #[test]
    fn h160_exact_values_for_small_n() {
        // h(1) = h(2) = h(3) = 1: no 4-AP exists yet.
        for n in 1..=3 {
            let r = h160(n, 200_000);
            assert_eq!(r.status, H160Status::Exact, "n={n}");
            assert_eq!(r.h, Some(1), "n={n}");
        }
        // h(4) = 3: the single 4-AP {1,2,3,4} forces >= 3 colours.
        let r4 = h160(4, 200_000);
        assert_eq!(r4.status, H160Status::Exact);
        assert_eq!(r4.h, Some(3));
    }

    #[test]
    fn h160_exact_three_at_12_and_four_at_13_with_witness_and_exhaustion() {
        // h(12) = 3 exactly: a valid 3-colouring exists and 2 colours is unsat.
        let r12 = h160(12, 500_000);
        assert_eq!(r12.status, H160Status::Exact, "{r12:?}");
        assert_eq!(r12.h, Some(3));
        let w12 = r12.witness_1_based.expect("witness at n=12");
        assert!(is_good_coloring_160(&w12));
        assert_eq!(distinct_count(&w12), 3);

        // h(13) = 4 exactly: jumps from 3 to 4 at N=13.
        let r13 = h160(13, 1_000_000);
        assert_eq!(r13.status, H160Status::Exact, "{r13:?}");
        assert_eq!(r13.h, Some(4));
        let w13 = r13.witness_1_based.expect("witness at n=13");
        assert!(is_good_coloring_160(&w13));
        assert_eq!(distinct_count(&w13), 4);
        assert_eq!(r13.witness_colors, Some(4));
        assert_eq!(r13.lower_bound, 4);
    }

    #[test]
    fn h160_is_monotone_non_decreasing_over_verified_range() {
        let mut prev = 0usize;
        let expected = [
            1usize, 1, 1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4,
        ];
        for (idx, &want) in expected.iter().enumerate() {
            let n = idx + 1;
            let r = h160(n, 1_000_000);
            let h = r.h.unwrap_or_else(|| panic!("expected exact h at n={n}, got {r:?}"));
            assert_eq!(h, want, "h({n})");
            assert!(h >= prev, "monotonicity violated at n={n}: {h} < {prev}");
            prev = h;
        }
    }

    #[test]
    fn h160_reports_inconclusive_under_tiny_node_limit() {
        // A 1-node budget cannot even complete the c=1 search at a size with a
        // 4-AP, so the determination is inconclusive (no claim).
        let r = h160(13, 1);
        assert_eq!(r.status, H160Status::Inconclusive, "{r:?}");
        assert_eq!(r.h, None);
        assert!(r.witness_1_based.is_none());
    }
}
