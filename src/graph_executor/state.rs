//! Weight adaptation and strategy-stat persistence against the graph state vector.

use super::GraphExecutor;
use super::value::OptionStats;
use crate::graph::*;

impl GraphExecutor {
    /// Apply accumulated weight changes after execution.
    pub(super) fn apply_weight_deltas(&mut self) {
        for (node_id, weight_idx, delta) in &self.weight_deltas {
            if let Some(node) = self.graph.nodes.get_mut(*node_id as usize) {
                if let Some(w) = node.weights.get_mut(*weight_idx) {
                    *w = (*w + delta).clamp(0.01, 0.99);
                }
            }
        }
        // Normalize weights per node
        let node_ids: Vec<u32> = self.weight_deltas.iter().map(|(id, _, _)| *id).collect();
        for id in node_ids {
            if let Some(node) = self.graph.nodes.get_mut(id as usize) {
                let sum: f64 = node.weights.iter().sum();
                if sum > 0.0 {
                    for w in &mut node.weights {
                        *w /= sum;
                    }
                }
            }
        }
        self.weight_deltas.clear();
    }

    /// Load strategy stats from the graph's state vector.
    /// Layout per strategy node: [tries_0, time_0, correct_0, tries_1, time_1, correct_1, ...]
    pub(super) fn load_strategy_stats(&mut self) {
        for node in &self.graph.nodes {
            if !matches!(node.op, OpCode::Strategy | OpCode::AdaptiveChoice) {
                continue;
            }
            if let Some(slot) = node.state_slot {
                let n = node.weights.len();
                let mut stats = vec![OptionStats::default(); n];
                for i in 0..n {
                    let base = slot as usize + i * 3;
                    if base + 2 < self.graph.state.len() {
                        stats[i].tries = self.graph.state[base] as u64;
                        stats[i].total_ns = self.graph.state[base + 1] as u128;
                        stats[i].correct = self.graph.state[base + 2] as u64;
                    }
                }
                self.strategy_stats.insert(node.id, stats);
            }
        }
    }

    /// Save strategy stats back to the graph's state vector.
    pub(super) fn save_strategy_stats(&mut self) {
        for (&node_id, stats) in &self.strategy_stats {
            let node = &mut self.graph.nodes[node_id as usize];
            let n = stats.len();
            let slots_needed = n * 3;

            // Allocate state slots if not yet assigned
            if node.state_slot.is_none() {
                let base = self.graph.state.len();
                self.graph.state.resize(base + slots_needed, 0.0);
                node.state_slot = Some(base as u32);
            }

            if let Some(slot) = node.state_slot {
                // Ensure state vector is large enough
                let end = slot as usize + slots_needed;
                if end > self.graph.state.len() {
                    self.graph.state.resize(end, 0.0);
                }
                for (i, s) in stats.iter().enumerate() {
                    let base = slot as usize + i * 3;
                    self.graph.state[base] = s.tries as f64;
                    self.graph.state[base + 1] = s.total_ns as f64;
                    self.graph.state[base + 2] = s.correct as f64;
                }
            }
        }
    }
}
