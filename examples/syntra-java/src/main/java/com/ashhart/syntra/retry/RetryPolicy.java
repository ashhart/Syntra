// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra.retry;

/**
 * Named retry policies.  Index ordering matches the demo capsule YAML's
 * {@code options:} list; do not reorder without updating the capsule.
 *
 * <table>
 *   <caption>Policy table</caption>
 *   <thead><tr><th>Index</th><th>Name</th><th>Max retries</th><th>Initial backoff</th><th>Multiplier</th></tr></thead>
 *   <tbody>
 *     <tr><td>0</td><td>NONE</td><td>0</td><td>0 ms</td><td>1.0</td></tr>
 *     <tr><td>1</td><td>SINGLE</td><td>1</td><td>0 ms</td><td>1.0</td></tr>
 *     <tr><td>2</td><td>TRIPLE</td><td>3</td><td>0 ms</td><td>1.0</td></tr>
 *     <tr><td>3</td><td>EXPONENTIAL_FAST</td><td>3</td><td>100 ms</td><td>2.0</td></tr>
 *     <tr><td>4</td><td>EXPONENTIAL_SLOW</td><td>3</td><td>500 ms</td><td>2.0</td></tr>
 *   </tbody>
 * </table>
 */
public enum RetryPolicy {

    NONE            ("none",             0, 0,   1.0),
    SINGLE          ("single",           1, 0,   1.0),
    TRIPLE          ("triple",           3, 0,   1.0),
    EXPONENTIAL_FAST("exponential_fast", 3, 100, 2.0),
    EXPONENTIAL_SLOW("exponential_slow", 3, 500, 2.0);

    private static final RetryPolicy[] VALUES = values();

    /** Capsule option name as defined in the capsule YAML. */
    public final String policyName;
    /** Maximum number of retry attempts (not counting the initial attempt). */
    public final int maxRetries;
    /** Initial backoff in milliseconds; 0 means no sleep between retries. */
    public final long initialBackoffMs;
    /** Multiplicative factor applied to the backoff after each retry. */
    public final double backoffMultiplier;

    RetryPolicy(
        final String policyName,
        final int maxRetries,
        final long initialBackoffMs,
        final double backoffMultiplier
    ) {
        this.policyName = policyName;
        this.maxRetries = maxRetries;
        this.initialBackoffMs = initialBackoffMs;
        this.backoffMultiplier = backoffMultiplier;
    }

    /**
     * Returns the policy at {@code index} in the capsule options list.
     * Falls back to {@link #NONE} when the index is out of range.
     */
    public static RetryPolicy fromOptionIndex(final int index) {
        if (index >= 0 && index < VALUES.length) {
            return VALUES[index];
        }
        return NONE;
    }
}
