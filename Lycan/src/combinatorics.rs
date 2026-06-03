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
}
