// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra.retry;

import java.util.ArrayDeque;
import java.util.Deque;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.locks.ReentrantLock;

/**
 * Per-host rolling window of (success, latencyMs) outcomes.
 *
 * <p>Drives the feature vectors sent to Syntra's {@code /decide} endpoint:
 * <ul>
 *   <li>{@code recent_failure_rate} — fraction of failures in the last
 *       {@code windowSize} outcomes.</li>
 *   <li>{@code p99_latency_ms} — 99th-percentile latency over the window.</li>
 *   <li>{@code hour} — fractional hour of day (UTC), cyclic feature for
 *       LinUCB candidates.</li>
 * </ul>
 *
 * <p>Thread-safe via a per-tracker {@link ReentrantLock}.
 */
public final class EndpointTracker {

    private static final int DEFAULT_WINDOW = 100;

    private final int windowSize;
    private final Map<String, Deque<Integer>> outcomes = new HashMap<>();
    private final Map<String, Deque<Double>> latencies = new HashMap<>();
    private final ReentrantLock lock = new ReentrantLock();

    /** Constructs a tracker with the default window of 100 outcomes per host. */
    public EndpointTracker() {
        this(DEFAULT_WINDOW);
    }

    /**
     * Constructs a tracker with a custom window size.
     *
     * @param windowSize maximum number of outcomes retained per host
     */
    public EndpointTracker(final int windowSize) {
        if (windowSize <= 0) throw new IllegalArgumentException("windowSize must be > 0");
        this.windowSize = windowSize;
    }

    /**
     * Records a single request outcome for the given host.
     *
     * @param host      host key (typically {@code host:port})
     * @param success   whether the request succeeded (HTTP status &lt; 400)
     * @param latencyMs observed end-to-end latency in milliseconds
     */
    public void record(final String host, final boolean success, final double latencyMs) {
        lock.lock();
        try {
            outcomes.computeIfAbsent(host, k -> new ArrayDeque<>(windowSize + 1))
                    .addLast(success ? 1 : 0);
            latencies.computeIfAbsent(host, k -> new ArrayDeque<>(windowSize + 1))
                     .addLast(latencyMs);

            final Deque<Integer> outs = outcomes.get(host);
            final Deque<Double> lats = latencies.get(host);
            if (outs.size() > windowSize) outs.removeFirst();
            if (lats.size() > windowSize) lats.removeFirst();
        } finally {
            lock.unlock();
        }
    }

    /**
     * Returns the current feature vector for the given host.
     *
     * <p>When no outcomes have been recorded yet, conservative defaults are
     * returned: {@code recent_failure_rate = 0.5}, {@code p99_latency_ms = 1000.0}.
     *
     * @param host host key
     * @return feature map suitable for the Syntra {@code /decide} body
     */
    public Map<String, Double> features(final String host) {
        final int[] outsCopy;
        final double[] latsCopy;

        lock.lock();
        try {
            final Deque<Integer> o = outcomes.getOrDefault(host, new ArrayDeque<>());
            final Deque<Double> l = latencies.getOrDefault(host, new ArrayDeque<>());
            outsCopy = o.stream().mapToInt(Integer::intValue).toArray();
            latsCopy = l.stream().mapToDouble(Double::doubleValue).toArray();
        } finally {
            lock.unlock();
        }

        final double hour = (System.currentTimeMillis() / 3_600_000.0) % 24.0;

        if (outsCopy.length == 0) {
            return Map.of(
                "recent_failure_rate", 0.5,
                "p99_latency_ms",      1000.0,
                "hour",                hour
            );
        }

        int successes = 0;
        for (final int v : outsCopy) successes += v;
        final double failureRate = 1.0 - (double) successes / outsCopy.length;

        final double p99 = computeP99(latsCopy);

        return Map.of(
            "recent_failure_rate", failureRate,
            "p99_latency_ms",      p99,
            "hour",                hour
        );
    }

    /**
     * Returns the rolling failure rate for a host (0–1).
     * Exported for monitoring and testing.
     */
    public double failureRate(final String host) {
        return features(host).get("recent_failure_rate");
    }

    private static double computeP99(final double[] lats) {
        if (lats.length == 0) return 1000.0;
        final double[] sorted = lats.clone();
        java.util.Arrays.sort(sorted);
        final int idx = Math.max(0, (int) (sorted.length * 0.99) - 1);
        return sorted[idx];
    }
}
