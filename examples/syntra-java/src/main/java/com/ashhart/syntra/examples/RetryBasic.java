// Copyright 2024 Ash Hart. Apache-2.0.
package com.ashhart.syntra.examples;

import com.ashhart.syntra.retry.RetryClient;
import com.ashhart.syntra.retry.RetryPolicy;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;

/**
 * Minimal usage example for {@link RetryClient}.
 *
 * <p>Run from the project root after building:
 * <pre>{@code
 * mvn -B package -DskipTests
 * java -cp target/syntra-client-0.1.0.jar \
 *      -DSYNTRA_ADMIN_KEY=mykey \
 *      com.ashhart.syntra.examples.RetryBasic
 * }</pre>
 */
public final class RetryBasic {

    private RetryBasic() {}

    public static void main(final String[] args) throws IOException {
        final RetryClient client = RetryClient.builder()
            .syntraUrl("http://localhost:8787")
            .adminKey(System.getenv().getOrDefault("SYNTRA_ADMIN_KEY", "demo-key"))
            .capsulePath("/tenants/myteam/jobs/retry/capsules/router")
            .fallbackPolicy(RetryPolicy.SINGLE)
            .onFeedbackError(err -> System.err.println("Feedback error (ignored): " + err.getMessage()))
            .build();

        // Simple GET — Syntra picks the retry policy; fallback fires if Syntra is down.
        final HttpResponse<String> response = client.request(
            "GET",
            URI.create("https://httpbin.org/get"),
            HttpRequest.BodyPublishers.noBody(),
            HttpResponse.BodyHandlers.ofString()
        );

        System.out.println("Status: " + response.statusCode());
        System.out.println("Body:   " + response.body().substring(0, Math.min(200, response.body().length())));
    }
}
