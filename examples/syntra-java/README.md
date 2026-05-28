# syntra-client (Java)

Java 17 client library for the [Syntra](../../README.md) adaptive decision
appliance.  Mirrors the Python `syntra_retry` package and the Node/Go ports in
the same repository.

## Requirements

- Java 17+
- Maven 3.8+ (production build)
- No runtime dependencies — only `java.net.http.HttpClient` (Java 11+) and a
  hand-rolled JSON utility.

## Install via Maven

Add to `pom.xml`:

```xml
<dependency>
  <groupId>com.ashhart</groupId>
  <artifactId>syntra-client</artifactId>
  <version>0.1.0</version>
</dependency>
```

Or build from source:

```bash
cd examples/syntra-java
mvn -B clean install
```

## Quickstart

```java
import com.ashhart.syntra.retry.RetryClient;
import com.ashhart.syntra.retry.RetryPolicy;

import java.net.URI;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;

RetryClient client = RetryClient.builder()
    .syntraUrl("http://localhost:8787")
    .adminKey(System.getenv("SYNTRA_ADMIN_KEY"))
    .capsulePath("/tenants/myteam/jobs/retry/capsules/router")
    .fallbackPolicy(RetryPolicy.SINGLE)
    .onFeedbackError(err -> logger.warn("Feedback failed (ignored): {}", err.getMessage()))
    .build();

HttpResponse<String> response = client.request(
    "GET",
    URI.create("https://api.example.com/users"),
    HttpRequest.BodyPublishers.noBody(),
    HttpResponse.BodyHandlers.ofString()
);
```

### Low-level client

If you only need `decide` / `feedback` without the retry loop:

```java
import com.ashhart.syntra.SyntraClient;
import com.ashhart.syntra.Decision;

SyntraClient syntra = new SyntraClient(
    "http://localhost:8787",
    System.getenv("SYNTRA_ADMIN_KEY"),
    "/tenants/myteam/jobs/retry/capsules/router"
);

// Discrete-context capsule
Decision d = syntra.decide(Map.of("contextKey", "low-latency"));

// Feature-context capsule
Decision d = syntra.decide(Map.of("features", Map.of(
    "recent_failure_rate", 0.15,
    "p99_latency_ms",      1200.0,
    "hour",                3.0
)));

// Send feedback when the outcome resolves
syntra.feedback(d.decisionId(), 0.85);
```

## Retry policies

| Index | Name              | Max retries | Initial backoff | Multiplier |
|-------|-------------------|-------------|-----------------|------------|
| 0     | NONE              | 0           | 0 ms            | 1.0        |
| 1     | SINGLE            | 1           | 0 ms            | 1.0        |
| 2     | TRIPLE            | 3           | 0 ms            | 1.0        |
| 3     | EXPONENTIAL_FAST  | 3           | 100 ms          | 2.0        |
| 4     | EXPONENTIAL_SLOW  | 3           | 500 ms          | 2.0        |

Index order matches the demo capsule YAML `options:` list.

## Fail-safe semantics

- If Syntra is unreachable or returns a non-2xx response, `RetryClient` silently
  applies `fallbackPolicy` and continues.
- If Syntra returns `refused: true`, `RetryClient` applies `fallbackPolicy`.
- If the `/feedback` POST fails, the error is passed to `onFeedbackError` (if
  set) and then swallowed.  A feedback failure never propagates to the caller.
- 5xx responses and `IOException` from the real target URL are retried up to
  `policy.maxRetries` times.  Responses with status < 500 are returned
  immediately without further retries.

## Reward formula

```
latencyPenalty = min(latencyMs / 10_000, 1.0)
reward = clamp((success ? 1.0 : 0.0) - 0.3 * latencyPenalty, -1.0, 1.0)
```

## JSON handling

The library uses a hand-rolled JSON encoder/decoder (`Json.java`) instead of
Jackson or Gson.  This choice keeps the production dependency footprint at zero:
no transitive JAR conflicts, no version pinning for downstream consumers.  The
implementation is intentionally limited to the two request shapes (`/decide`,
`/feedback`) and one response shape (`/decide`) that Syntra exchanges.  If you
need to pass arbitrary JSON via the library, extend `Json.java` accordingly.

## Running the tests

```bash
cd examples/syntra-java
mvn -B clean test
```

Tests use `com.sun.net.httpserver.HttpServer` (built-in JDK) as the HTTP stub,
so there are no additional test dependencies beyond JUnit 5.  Backoff timing is
verified by injecting a no-op `Sleeper` rather than using real wall-clock delays,
keeping the full suite well under one second.

### Manual compile (no Maven)

If Maven is unavailable, download JUnit 5 standalone and compile with:

```bash
JAVA=/opt/homebrew/Cellar/openjdk/25.0.2/bin/java
JAVAC=/opt/homebrew/Cellar/openjdk/25.0.2/bin/javac
JUNIT_JAR=~/junit-platform-console-standalone-1.10.2.jar

# Compile sources
$JAVAC --release 17 -d out/main \
  src/main/java/com/ashhart/syntra/*.java \
  src/main/java/com/ashhart/syntra/retry/*.java \
  src/main/java/com/ashhart/syntra/examples/*.java

# Compile tests
$JAVAC --release 17 -cp out/main:$JUNIT_JAR -d out/test \
  src/test/java/com/ashhart/syntra/retry/*.java

# Run tests
$JAVA -jar $JUNIT_JAR execute \
  --class-path out/main:out/test \
  --scan-class-path out/test
```

## License

Apache-2.0 — see the repository root for the full license text.
