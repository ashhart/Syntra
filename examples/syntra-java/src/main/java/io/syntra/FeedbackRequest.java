package io.syntra;

/**
 * Request body for a {@code POST /feedback} call.
 *
 * <p>Reward must be in the continuous range {@code [-1.0, 1.0]} as declared in
 * the capsule YAML. See {@link io.syntra.retry.RetryClient} for the
 * recommended reward formula.
 */
public record FeedbackRequest(String decisionId, double reward) {}
