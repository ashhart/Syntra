// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra;

import java.util.List;
import java.util.Map;
import java.util.Optional;

/**
 * Parsed response envelope from a {@code /decide} call.
 *
 * <p>Fields mirror the Syntra REST response exactly:
 * <ul>
 *   <li>{@code decisionId} — opaque ID; pass to {@link SyntraClient#feedback} when the outcome
 *       resolves.</li>
 *   <li>{@code decisions} — one entry per slot; use {@code decisions.get(0).chosenOption()} as
 *       the policy index.</li>
 *   <li>{@code refused} — {@code true} when the bandit's confidence interval was too wide or the
 *       input was OOD. Callers must apply their fallback policy.</li>
 *   <li>{@code confidence} — raw confidence block; present when refusal is configured on the
 *       capsule.</li>
 * </ul>
 */
public record Decision(
    String decisionId,
    List<DecisionItem> decisions,
    boolean refused,
    Optional<Confidence> confidence
) {

    /** Single entry in the {@code decisions} array. */
    public record DecisionItem(int chosenOption, Optional<String> label) {}
}
