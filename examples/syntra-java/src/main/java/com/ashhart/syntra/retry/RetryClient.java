// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra.retry;

import com.ashhart.syntra.Decision;
import com.ashhart.syntra.SyntraClient;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.function.Consumer;

/**
 * HTTP client that asks Syntra for a retry policy on every request.
 *
 * <p>Each call to {@link #request} follows this sequence:
 * <ol>
 *   <li>Compute the per-host feature vector from the rolling window.</li>
 *   <li>POST {@code /decide} to Syntra; fall back silently on any error.</li>
 *   <li>Execute the real HTTP request with the chosen retry policy.</li>
 *   <li>Record the outcome in the per-host rolling window.</li>
 *   <li>POST {@code /feedback} to Syntra (errors silently swallowed or
 *       passed to the optional {@link Builder#onFeedbackError} hook).</li>
 * </ol>
 *
 * <p>A Syntra outage degrades adaptive retry to "always use fallback policy"
 * without throwing any exception to the caller.
 *
 * <p>Instances are safe for concurrent use after construction.
 *
 * <h2>Usage</h2>
 * <pre>{@code
 * RetryClient client = RetryClient.builder()
 *     .syntraUrl("http://localhost:8787")
 *     .adminKey(System.getenv("SYNTRA_ADMIN_KEY"))
 *     .capsulePath("/tenants/myteam/jobs/retry/capsules/router")
 *     .fallbackPolicy(RetryPolicy.SINGLE)
 *     .build();
 *
 * HttpResponse<String> response = client.request(
 *     "GET", URI.create("https://api.example.com/users"),
 *     HttpRequest.BodyPublishers.noBody(),
 *     HttpResponse.BodyHandlers.ofString());
 * }</pre>
 */
public final class RetryClient {

    /**
     * Abstraction over {@link Thread#sleep} so tests can inject an instant no-op
     * instead of using real wall-clock delays.
     */
    @FunctionalInterface
    public interface Sleeper {
        /**
         * Sleeps for approximately {@code ms} milliseconds.
         *
         * @param ms duration in milliseconds; implementors may ignore this in tests
         * @throws InterruptedException if the sleep is interrupted
         */
        void sleep(long ms) throws InterruptedException;
    }

    private static final Sleeper REAL_SLEEP = Thread::sleep;

    private final SyntraClient syntra;
    private final RetryPolicy fallbackPolicy;
    private final Consumer<Throwable> onFeedbackError;
    private final EndpointTracker tracker;
    private final HttpClient httpClient;
    private final Duration httpTimeout;
    private final Sleeper sleeper;

    private RetryClient(final Builder b) {
        this.syntra = new SyntraClient(b.syntraUrl, b.adminKey, b.capsulePath, b.syntraTimeout);
        this.fallbackPolicy = b.fallbackPolicy;
        this.onFeedbackError = b.onFeedbackError;
        this.tracker = b.tracker != null ? b.tracker : new EndpointTracker();
        this.httpClient = b.httpClient != null ? b.httpClient
            : HttpClient.newBuilder().connectTimeout(b.httpTimeout).build();
        this.httpTimeout = b.httpTimeout;
        this.sleeper = b.sleeper != null ? b.sleeper : REAL_SLEEP;
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /**
     * Executes an HTTP request with a Syntra-selected retry policy.
     *
     * <p>Never throws because of a Syntra failure; only throws if the real target
     * is unreachable after all retry attempts.
     *
     * @param <T>           response body type
     * @param method        HTTP method string (e.g. {@code "GET"})
     * @param uri           target URI
     * @param bodyPublisher request body; use
     *                      {@link HttpRequest.BodyPublishers#noBody()} for GET
     * @param bodyHandler   response body handler
     * @return last HTTP response
     * @throws IOException if all retry attempts fail due to transport errors
     */
    public <T> HttpResponse<T> request(
        final String method,
        final URI uri,
        final HttpRequest.BodyPublisher bodyPublisher,
        final HttpResponse.BodyHandler<T> bodyHandler
    ) throws IOException {
        final String host = endpointKey(uri);
        final Map<String, Double> featureMap = tracker.features(host);
        // Convert Map<String,Double> -> Map<String,Object> for the decide body
        final Map<String, Object> features = Map.copyOf(
            Map.of(
                "recent_failure_rate", (Object) featureMap.get("recent_failure_rate"),
                "p99_latency_ms",      (Object) featureMap.get("p99_latency_ms"),
                "hour",                (Object) featureMap.get("hour")
            )
        );

        final PolicyAndDecisionId pad = getPolicy(features);
        final ExecutionResult<T> result = executeWithPolicy(method, uri, bodyPublisher, bodyHandler, pad.policy());

        tracker.record(host, result.success(), result.latencyMs());

        if (pad.decisionId() != null) {
            sendFeedback(pad.decisionId(), result.success(), result.latencyMs());
        }

        if (result.response() != null) {
            return result.response();
        }
        throw new IOException("All retries exhausted for " + method + " " + uri);
    }

    /** Exposes the per-host tracker for inspection and testing. */
    public EndpointTracker tracker() {
        return tracker;
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    private record PolicyAndDecisionId(RetryPolicy policy, String decisionId) {}

    private PolicyAndDecisionId getPolicy(final Map<String, Object> features) {
        try {
            final Decision d = syntra.decide(Map.of("features", features));
            if (d.refused() || d.decisions().isEmpty()) {
                return new PolicyAndDecisionId(fallbackPolicy,
                    d.decisionId().isEmpty() ? null : d.decisionId());
            }
            final Decision.DecisionItem first = d.decisions().get(0);
            final RetryPolicy policy = RetryPolicy.fromOptionIndex(first.chosenOption());
            return new PolicyAndDecisionId(policy, d.decisionId().isEmpty() ? null : d.decisionId());
        } catch (final IOException e) {
            return new PolicyAndDecisionId(fallbackPolicy, null);
        }
    }

    private record ExecutionResult<T>(
        HttpResponse<T> response,
        boolean success,
        double latencyMs
    ) {}

    private <T> ExecutionResult<T> executeWithPolicy(
        final String method,
        final URI uri,
        final HttpRequest.BodyPublisher bodyPublisher,
        final HttpResponse.BodyHandler<T> bodyHandler,
        final RetryPolicy policy
    ) throws IOException {
        final long start = System.currentTimeMillis();
        long backoffMs = policy.initialBackoffMs;
        HttpResponse<T> lastResponse = null;

        for (int attempt = 0; attempt <= policy.maxRetries; attempt++) {
            try {
                final HttpRequest req = HttpRequest.newBuilder()
                    .uri(uri)
                    .timeout(httpTimeout)
                    .method(method, bodyPublisher)
                    .build();

                final HttpResponse<T> resp = httpClient.send(req, bodyHandler);
                lastResponse = resp;

                if (resp.statusCode() < 500) {
                    final double latency = System.currentTimeMillis() - start;
                    return new ExecutionResult<>(resp, resp.statusCode() < 400, latency);
                }
                // 5xx: fall through to retry logic
            } catch (final InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            } catch (final IOException e) {
                // Transport error — retriable
            }

            if (attempt < policy.maxRetries) {
                if (backoffMs > 0) {
                    try {
                        sleeper.sleep(backoffMs);
                    } catch (final InterruptedException ie) {
                        Thread.currentThread().interrupt();
                        break;
                    }
                    backoffMs = (long) (backoffMs * policy.backoffMultiplier);
                }
            }
        }

        final double latency = System.currentTimeMillis() - start;
        return new ExecutionResult<>(lastResponse, false, latency);
    }

    private void sendFeedback(final String decisionId, final boolean success, final double latencyMs) {
        final double latencyPenalty = Math.min(latencyMs / 10_000.0, 1.0);
        final double reward = Math.max(-1.0, Math.min(1.0,
            (success ? 1.0 : 0.0) - 0.3 * latencyPenalty));
        try {
            syntra.feedback(decisionId, reward);
        } catch (final IOException e) {
            if (onFeedbackError != null) {
                onFeedbackError.accept(e);
            }
            // Always swallowed — feedback failure must not break the caller.
        }
    }

    private static String endpointKey(final URI uri) {
        final String host = uri.getHost();
        final int port = uri.getPort();
        if (host == null) return uri.toString();
        return port < 0 ? host : host + ":" + port;
    }

    // -----------------------------------------------------------------------
    // Builder
    // -----------------------------------------------------------------------

    /** Returns a new {@link Builder} for constructing a {@link RetryClient}. */
    public static Builder builder() {
        return new Builder();
    }

    /**
     * Fluent builder for {@link RetryClient}.
     */
    public static final class Builder {
        private String syntraUrl;
        private String adminKey;
        private String capsulePath;
        private RetryPolicy fallbackPolicy = RetryPolicy.SINGLE;
        private Consumer<Throwable> onFeedbackError;
        private Duration syntraTimeout = Duration.ofSeconds(2);
        private Duration httpTimeout = Duration.ofSeconds(30);
        private EndpointTracker tracker;
        private HttpClient httpClient;
        private Sleeper sleeper;

        private Builder() {}

        /** Syntra appliance base URL (trailing slash stripped). */
        public Builder syntraUrl(final String url) { this.syntraUrl = url; return this; }

        /** Bearer token for the Authorization header. */
        public Builder adminKey(final String key) { this.adminKey = key; return this; }

        /** Capsule path, e.g. {@code "/tenants/myteam/jobs/retry/capsules/router"}. */
        public Builder capsulePath(final String path) { this.capsulePath = path; return this; }

        /**
         * Policy used when Syntra is unreachable, refuses, or returns a malformed
         * response.  Default: {@link RetryPolicy#SINGLE}.
         */
        public Builder fallbackPolicy(final RetryPolicy policy) {
            this.fallbackPolicy = policy;
            return this;
        }

        /**
         * Optional callback invoked when a {@code /feedback} POST fails.
         * The hook must not block; feedback errors never propagate to the caller.
         */
        public Builder onFeedbackError(final Consumer<Throwable> hook) {
            this.onFeedbackError = hook;
            return this;
        }

        /** Per-request timeout for Syntra calls. Default: 2 s. */
        public Builder syntraTimeout(final Duration d) { this.syntraTimeout = d; return this; }

        /** Per-request timeout for real target calls. Default: 30 s. */
        public Builder httpTimeout(final Duration d) { this.httpTimeout = d; return this; }

        /**
         * Override the per-host tracker.  Useful in tests to inspect recorded
         * outcomes without reflection.
         */
        public Builder tracker(final EndpointTracker t) { this.tracker = t; return this; }

        /**
         * Override the HTTP client used for real target calls.  Useful in
         * integration tests.
         */
        public Builder httpClient(final HttpClient client) { this.httpClient = client; return this; }

        /**
         * Override the {@link Sleeper} used for backoff delays.
         * Inject {@code (ms) -> {}} in unit tests to avoid real sleeps.
         */
        public Builder sleeper(final Sleeper s) { this.sleeper = s; return this; }

        /** Builds the {@link RetryClient}. */
        public RetryClient build() {
            if (syntraUrl == null || syntraUrl.isBlank()) throw new IllegalStateException("syntraUrl is required");
            if (adminKey == null || adminKey.isBlank()) throw new IllegalStateException("adminKey is required");
            if (capsulePath == null || capsulePath.isBlank()) throw new IllegalStateException("capsulePath is required");
            return new RetryClient(this);
        }
    }
}
