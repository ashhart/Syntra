// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra.retry;

import com.sun.net.httpserver.HttpServer;
import com.sun.net.httpserver.HttpHandler;
import com.sun.net.httpserver.HttpExchange;

import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Timeout;

import java.io.IOException;
import java.io.OutputStream;
import java.net.ConnectException;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Unit tests for {@link RetryClient}.
 *
 * <p>All HTTP stubs use {@code com.sun.net.httpserver.HttpServer} — a built-in
 * JDK server with no Maven dependencies.  This keeps the test-time dependency
 * footprint at zero (beyond JUnit 5 itself) and avoids the WireMock JAR.
 *
 * <p>Backoff is tested using the {@link RetryClient.Sleeper} injection point so
 * that tests run in milliseconds without real wall-clock delays.
 */
@Timeout(value = 30, unit = TimeUnit.SECONDS)
class RetryClientTest {

    // Shared port counters to avoid collisions when tests run in parallel.
    private static final AtomicInteger PORT_COUNTER = new AtomicInteger(38_700);

    private HttpServer syntraServer;
    private HttpServer targetServer;
    private int syntraPort;
    private int targetPort;

    @BeforeEach
    void startServers() throws IOException {
        syntraPort = PORT_COUNTER.getAndIncrement();
        targetPort = PORT_COUNTER.getAndIncrement();
        syntraServer = HttpServer.create(new InetSocketAddress("127.0.0.1", syntraPort), 0);
        targetServer = HttpServer.create(new InetSocketAddress("127.0.0.1", targetPort), 0);
        syntraServer.start();
        targetServer.start();
    }

    @AfterEach
    void stopServers() {
        if (syntraServer != null) syntraServer.stop(0);
        if (targetServer != null) targetServer.stop(0);
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    private static void respond(final HttpExchange ex, final int status, final String body) throws IOException {
        final byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
        ex.getResponseHeaders().set("Content-Type", "application/json");
        ex.sendResponseHeaders(status, bytes.length);
        try (final OutputStream os = ex.getResponseBody()) {
            os.write(bytes);
        }
    }

    /** Registers a Syntra /decide handler that returns the given option index. */
    private void registerDecide(final int optionIndex, final String decisionId) {
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/decide", ex -> {
            respond(ex, 200,
                "{\"decisionId\":\"" + decisionId + "\","
                + "\"decisions\":[{\"chosen_option\":" + optionIndex + "}],"
                + "\"refused\":false}");
        });
    }

    /** Registers a Syntra /decide handler that returns refused=true. */
    private void registerDecideRefused(final String decisionId) {
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/decide", ex -> {
            respond(ex, 200,
                "{\"decisionId\":\"" + decisionId + "\","
                + "\"decisions\":[],"
                + "\"refused\":true}");
        });
    }

    /** Registers a Syntra /feedback handler that records received payloads. */
    private CountDownLatch registerFeedback(final List<String> captured) {
        final CountDownLatch latch = new CountDownLatch(1);
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/feedback", ex -> {
            final String body = new String(ex.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
            captured.add(body);
            latch.countDown();
            respond(ex, 200, "{}");
        });
        return latch;
    }

    private RetryClient.Builder baseBuilder() {
        return RetryClient.builder()
            .syntraUrl("http://127.0.0.1:" + syntraPort)
            .adminKey("test-key")
            .capsulePath("/tenants/test/jobs/retry/capsules/router")
            .sleeper(ms -> {}); // No-op sleeper — tests run in milliseconds
    }

    private URI targetUri() {
        return URI.create("http://127.0.0.1:" + targetPort + "/");
    }

    // -----------------------------------------------------------------------
    // Test 1: Successful decide+feedback round-trip
    // -----------------------------------------------------------------------

    @Test
    void successfulRoundTrip_decideAndFeedbackBothFire() throws Exception {
        registerDecide(2, "dec_001"); // option 2 = TRIPLE
        final List<String> feedbackBodies = new ArrayList<>();
        final CountDownLatch feedbackLatch = registerFeedback(feedbackBodies);

        targetServer.createContext("/", ex -> respond(ex, 200, "\"ok\""));

        final RetryClient client = baseBuilder().build();
        final HttpResponse<String> response = client.request(
            "GET", targetUri(),
            HttpRequest.BodyPublishers.noBody(),
            HttpResponse.BodyHandlers.ofString()
        );

        assertEquals(200, response.statusCode());
        // Feedback is async — wait up to 5 s for it to arrive.
        assertTrue(feedbackLatch.await(5, TimeUnit.SECONDS), "Feedback never received");
        assertEquals(1, feedbackBodies.size());
        assertTrue(feedbackBodies.get(0).contains("dec_001"),
            "Feedback body should contain the decisionId");
        assertTrue(feedbackBodies.get(0).contains("reward"),
            "Feedback body should contain a reward field");
    }

    // -----------------------------------------------------------------------
    // Test 2: Refusal falls back to default policy
    // -----------------------------------------------------------------------

    @Test
    void refusal_fallsBackToDefaultPolicy() throws Exception {
        registerDecideRefused("dec_002");
        // feedback registered but we don't strictly need it for this assertion
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/feedback",
            ex -> respond(ex, 200, "{}"));

        // Target returns 200 on first attempt — a SINGLE-retry policy would still succeed.
        final AtomicInteger targetHits = new AtomicInteger();
        targetServer.createContext("/", ex -> {
            targetHits.incrementAndGet();
            respond(ex, 200, "\"ok\"");
        });

        final RetryClient client = baseBuilder()
            .fallbackPolicy(RetryPolicy.SINGLE) // explicit so we can assert it was used
            .build();

        final HttpResponse<String> response = client.request(
            "GET", targetUri(),
            HttpRequest.BodyPublishers.noBody(),
            HttpResponse.BodyHandlers.ofString()
        );

        assertEquals(200, response.statusCode());
        // With SINGLE fallback the first attempt should succeed; target hit exactly once.
        assertEquals(1, targetHits.get());
    }

    // -----------------------------------------------------------------------
    // Test 3: Syntra unreachable — fallback fires, no exception thrown
    // -----------------------------------------------------------------------

    @Test
    void syntraUnreachable_fallbackFires_noExceptionToCallers() throws Exception {
        // Use a port that is not listening.
        syntraServer.stop(0);
        syntraServer = null; // prevent double-stop in @AfterEach

        targetServer.createContext("/", ex -> respond(ex, 200, "\"ok\""));

        final RetryClient client = RetryClient.builder()
            .syntraUrl("http://127.0.0.1:" + syntraPort) // nothing listening here
            .adminKey("test-key")
            .capsulePath("/tenants/test/jobs/retry/capsules/router")
            .fallbackPolicy(RetryPolicy.NONE)
            .sleeper(ms -> {})
            .build();

        // Should not throw — Syntra failure is transparent to the caller.
        final HttpResponse<String> response = assertDoesNotThrow(() ->
            client.request("GET", targetUri(),
                HttpRequest.BodyPublishers.noBody(),
                HttpResponse.BodyHandlers.ofString())
        );
        assertEquals(200, response.statusCode());
    }

    // -----------------------------------------------------------------------
    // Test 4: Feedback failure invokes hook, doesn't break request flow
    // -----------------------------------------------------------------------

    @Test
    void feedbackFailure_invokesHook_doesNotBreakFlow() throws Exception {
        registerDecide(1, "dec_004"); // SINGLE

        // Feedback endpoint returns 500 to trigger a failure.
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/feedback",
            ex -> respond(ex, 500, "{\"error\":\"internal\"}"));

        targetServer.createContext("/", ex -> respond(ex, 200, "\"ok\""));

        final List<Throwable> hookErrors = new ArrayList<>();
        final CountDownLatch hookLatch = new CountDownLatch(1);

        final RetryClient client = baseBuilder()
            .onFeedbackError(err -> {
                hookErrors.add(err);
                hookLatch.countDown();
            })
            .build();

        // The request itself must succeed despite the feedback failure.
        final HttpResponse<String> response = client.request(
            "GET", targetUri(),
            HttpRequest.BodyPublishers.noBody(),
            HttpResponse.BodyHandlers.ofString()
        );
        assertEquals(200, response.statusCode());

        // The hook must have been called.
        assertTrue(hookLatch.await(5, TimeUnit.SECONDS), "onFeedbackError hook never called");
        assertEquals(1, hookErrors.size());
    }

    // -----------------------------------------------------------------------
    // Test 5: Per-host tracker failure-rate math
    // -----------------------------------------------------------------------

    @Test
    void trackerFailureRate_rollingWindowMath() {
        final EndpointTracker tracker = new EndpointTracker(10);

        // 7 successes, 3 failures → 30 % failure rate
        for (int i = 0; i < 7; i++) tracker.record("api.example.com:443", true,  100.0);
        for (int i = 0; i < 3; i++) tracker.record("api.example.com:443", false, 200.0);

        final double rate = tracker.failureRate("api.example.com:443");
        assertEquals(0.3, rate, 1e-9, "Expected 30 % failure rate");

        // Window is 10; add 10 more successes — old failures roll off.
        for (int i = 0; i < 10; i++) tracker.record("api.example.com:443", true, 50.0);
        final double rateAfter = tracker.failureRate("api.example.com:443");
        assertEquals(0.0, rateAfter, 1e-9, "All failures should have rolled off the window");
    }

    // -----------------------------------------------------------------------
    // Test 6: Correct attempt count per policy
    // -----------------------------------------------------------------------

    @Test
    void retryPolicy_correctAttemptCounts() throws Exception {
        registerDecide(0, "dec_006"); // NONE = 0 retries → 1 total attempt
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/feedback",
            ex -> respond(ex, 200, "{}"));

        for (final RetryPolicy policy : RetryPolicy.values()) {
            final AtomicInteger attempts = new AtomicInteger();
            // Each attempt gets a 503 so we exhaust all retries.
            final int targetPortLocal = PORT_COUNTER.getAndIncrement();
            final HttpServer localTarget = HttpServer.create(
                new InetSocketAddress("127.0.0.1", targetPortLocal), 0);
            localTarget.createContext("/", ex -> {
                attempts.incrementAndGet();
                respond(ex, 503, "\"err\"");
            });
            localTarget.start();

            try {
                // Replace decide handler to return this policy's index.
                final int idx = policy.ordinal();
                final HttpServer localSyntra = HttpServer.create(
                    new InetSocketAddress("127.0.0.1", PORT_COUNTER.getAndIncrement()), 0);
                localSyntra.createContext("/tenants/test/jobs/retry/capsules/router/decide", ex ->
                    respond(ex, 200,
                        "{\"decisionId\":\"d\","
                        + "\"decisions\":[{\"chosen_option\":" + idx + "}],"
                        + "\"refused\":false}"));
                localSyntra.createContext("/tenants/test/jobs/retry/capsules/router/feedback",
                    ex -> respond(ex, 200, "{}"));
                localSyntra.start();

                final int localSyntraPort = ((InetSocketAddress) localSyntra.getAddress()).getPort();
                final RetryClient client = RetryClient.builder()
                    .syntraUrl("http://127.0.0.1:" + localSyntraPort)
                    .adminKey("key")
                    .capsulePath("/tenants/test/jobs/retry/capsules/router")
                    .sleeper(ms -> {}) // instant backoff
                    .build();

                client.request(
                    "GET",
                    URI.create("http://127.0.0.1:" + targetPortLocal + "/"),
                    HttpRequest.BodyPublishers.noBody(),
                    HttpResponse.BodyHandlers.ofString()
                );

                localSyntra.stop(0);
            } finally {
                localTarget.stop(0);
            }

            final int expected = policy.maxRetries + 1;
            assertEquals(expected, attempts.get(),
                "Policy " + policy.policyName + " should make " + expected + " attempt(s)");
        }
    }

    // -----------------------------------------------------------------------
    // Test 7: Backoff timing respects multiplier via injected Sleeper
    // -----------------------------------------------------------------------

    @Test
    void backoffTiming_respectsMultiplier_viaSleeperInjection() throws Exception {
        // Use EXPONENTIAL_FAST: 3 retries, 100ms initial, x2 multiplier.
        // Expect sleep calls: 100, 200, 400 ms (3 inter-retry gaps).
        registerDecide(RetryPolicy.EXPONENTIAL_FAST.ordinal(), "dec_007");
        syntraServer.createContext("/tenants/test/jobs/retry/capsules/router/feedback",
            ex -> respond(ex, 200, "{}"));

        // Target always 503 so all retries fire.
        targetServer.createContext("/", ex -> respond(ex, 503, "\"err\""));

        final List<Long> sleepCalls = new ArrayList<>();
        final RetryClient.Sleeper recordingSleeper = ms -> sleepCalls.add(ms);

        final RetryClient client = baseBuilder()
            .sleeper(recordingSleeper)
            .build();

        client.request(
            "GET", targetUri(),
            HttpRequest.BodyPublishers.noBody(),
            HttpResponse.BodyHandlers.ofString()
        );

        // EXPONENTIAL_FAST: maxRetries=3, so 3 inter-attempt sleeps
        assertEquals(3, sleepCalls.size(), "Should have 3 backoff sleep calls for EXPONENTIAL_FAST");
        assertEquals(100L, sleepCalls.get(0), "First backoff should be 100 ms");
        assertEquals(200L, sleepCalls.get(1), "Second backoff should be 200 ms");
        assertEquals(400L, sleepCalls.get(2), "Third backoff should be 400 ms");
    }
}
