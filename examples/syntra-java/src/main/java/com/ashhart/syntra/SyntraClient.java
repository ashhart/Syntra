// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.OptionalDouble;
import java.util.function.Consumer;

/**
 * Low-level HTTP client for the Syntra adaptive decision appliance.
 *
 * <p>Covers {@code POST /decide} and {@code POST /feedback}. Transport errors and
 * non-2xx responses are surfaced as {@link SyntraException}; callers (e.g.
 * {@link com.ashhart.syntra.retry.RetryClient}) are responsible for fallback.
 *
 * <p>Uses {@link java.net.http.HttpClient} (Java 11+); no third-party HTTP library.
 * JSON is handled by the package-private {@link Json} class — no Jackson/Gson dep.
 *
 * <p>Instances are safe for concurrent use after construction.
 */
public final class SyntraClient {

    /**
     * Thrown when Syntra returns a non-2xx status or when the response body
     * cannot be parsed.
     */
    public static final class SyntraException extends IOException {
        public SyntraException(final String message) { super(message); }
        public SyntraException(final String message, final Throwable cause) { super(message, cause); }
    }

    private final String baseUrl;
    private final String capsulePath;
    private final String authHeader;
    private final HttpClient http;
    private final Duration timeout;

    /**
     * Constructs a client.
     *
     * @param baseUrl     appliance root, e.g. {@code "http://localhost:8787"} (trailing slash stripped)
     * @param adminKey    Bearer token for the Authorization header
     * @param capsulePath capsule path, e.g. {@code "/tenants/myteam/jobs/retry/capsules/router"}
     */
    public SyntraClient(final String baseUrl, final String adminKey, final String capsulePath) {
        this(baseUrl, adminKey, capsulePath, Duration.ofSeconds(2));
    }

    /**
     * Constructs a client with a custom per-request timeout.
     *
     * @param baseUrl     appliance root (trailing slash stripped)
     * @param adminKey    Bearer token
     * @param capsulePath capsule path
     * @param timeout     per-request HTTP timeout
     */
    public SyntraClient(
        final String baseUrl,
        final String adminKey,
        final String capsulePath,
        final Duration timeout
    ) {
        this.baseUrl = baseUrl.stripTrailing().replaceAll("/+$", "");
        this.capsulePath = capsulePath.replaceAll("/+$", "");
        this.authHeader = "Bearer " + adminKey;
        this.timeout = timeout;
        this.http = HttpClient.newBuilder()
            .connectTimeout(timeout)
            .build();
    }

    /**
     * Calls {@code POST {capsulePath}/decide} and returns the parsed {@link Decision}.
     *
     * <p>Accepts either a discrete-context body ({@code {"contextKey":"..."}}) or a
     * feature-context body ({@code {"features":{...}}}).
     *
     * @param body request body; must contain either {@code "contextKey"} or {@code "features"}
     * @return parsed decision envelope
     * @throws SyntraException on transport error or non-2xx status
     */
    public Decision decide(final Map<String, Object> body) throws SyntraException {
        final String url = baseUrl + capsulePath + "/decide";
        final String responseBody = post(url, Json.encode(body));
        return parseDecision(responseBody);
    }

    /**
     * Calls {@code POST {capsulePath}/feedback} to supply a reward for a prior decision.
     *
     * @param decisionId  the {@code decisionId} from the preceding {@link #decide} call
     * @param reward      reward in {@code [-1.0, 1.0]}
     * @throws SyntraException on transport error or non-2xx status
     */
    public void feedback(final String decisionId, final double reward) throws SyntraException {
        final String url = baseUrl + capsulePath + "/feedback";
        final String body = Json.encode(Map.of("decisionId", decisionId, "reward", reward));
        post(url, body);
    }

    // -----------------------------------------------------------------------
    // Internal HTTP helpers
    // -----------------------------------------------------------------------

    private String post(final String url, final String jsonBody) throws SyntraException {
        final HttpRequest request;
        try {
            request = HttpRequest.newBuilder()
                .uri(URI.create(url))
                .timeout(timeout)
                .header("Content-Type", "application/json")
                .header("Authorization", authHeader)
                .POST(HttpRequest.BodyPublishers.ofString(jsonBody))
                .build();
        } catch (final IllegalArgumentException e) {
            throw new SyntraException("syntra: invalid URL: " + url, e);
        }

        final HttpResponse<String> response;
        try {
            response = http.send(request, HttpResponse.BodyHandlers.ofString());
        } catch (final IOException e) {
            throw new SyntraException("syntra: transport error on POST " + url, e);
        } catch (final InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new SyntraException("syntra: interrupted on POST " + url, e);
        }

        if (response.statusCode() < 200 || response.statusCode() >= 300) {
            throw new SyntraException(
                "syntra: HTTP " + response.statusCode() + " from " + url + ": " + response.body());
        }
        return response.body();
    }

    // -----------------------------------------------------------------------
    // Parsing
    // -----------------------------------------------------------------------

    private static Decision parseDecision(final String json) throws SyntraException {
        final Map<String, Object> root;
        try {
            root = Json.decodeObject(json);
        } catch (final IllegalArgumentException e) {
            throw new SyntraException("syntra: could not parse /decide response", e);
        }

        final String decisionId = Json.asString(root.get("decisionId"));
        final boolean refused = Json.asBoolean(root.get("refused"), false);

        final List<Decision.DecisionItem> items = new ArrayList<>();
        final Object decisionsRaw = root.get("decisions");
        if (decisionsRaw instanceof List<?> list) {
            for (final Object elem : list) {
                if (elem instanceof Map<?, ?> m) {
                    @SuppressWarnings("unchecked")
                    final Map<String, Object> dm = (Map<String, Object>) m;
                    final int chosen = Json.asInt(dm.get("chosen_option"), 0);
                    final String label = Json.asString(dm.get("label"));
                    items.add(new Decision.DecisionItem(chosen, Optional.ofNullable(label)));
                }
            }
        }

        Optional<Confidence> confidence = Optional.empty();
        final Object confRaw = root.get("confidence");
        if (confRaw instanceof Map<?, ?> cm) {
            @SuppressWarnings("unchecked")
            final Map<String, Object> confMap = (Map<String, Object>) cm;
            final OptionalDouble oodScore = Json.asOptionalDouble(confMap.get("oodScore"));
            final OptionalDouble intervalWidth = Json.asOptionalDouble(confMap.get("intervalWidth"));
            final boolean confRefused = Json.asBoolean(confMap.get("refused"), false);
            confidence = Optional.of(new Confidence(oodScore, intervalWidth, confRefused));
        }

        return new Decision(
            decisionId == null ? "" : decisionId,
            List.copyOf(items),
            refused,
            confidence
        );
    }
}
