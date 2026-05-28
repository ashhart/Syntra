/**
 * Tests for @ashhart/syntra-client — RetryClient and EndpointTracker.
 *
 * fetch is mocked via jest.fn() throughout; no real network calls are made.
 *
 * Apache-2.0
 */

import { jest, describe, it, expect, beforeEach, afterEach } from "@jest/globals";
import {
  RetryClient,
  EndpointTracker,
  DEFAULT_POLICIES,
  policyFromOption,
} from "../src/retry.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Build a minimal Response-like object acceptable to the RetryClient. */
function makeResponse(status: number, body: unknown = {}): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    headers: new Headers(),
    redirected: false,
    statusText: String(status),
    type: "basic",
    url: "",
    clone: () => makeResponse(status, body),
    arrayBuffer: async () => new ArrayBuffer(0),
    blob: async () => new Blob([]),
    formData: async () => new FormData(),
    text: async () => JSON.stringify(body),
    body: null,
    bodyUsed: false,
  } as unknown as Response;
}

/** Decide response for a given option index. */
function decideBody(optionIdx: number, decisionId = "dec_test123") {
  return {
    decisionId,
    decisions: [{ chosen_option: optionIdx }],
    refused: false,
  };
}

/** Build a RetryClient with fetch replaced by a mock. */
function buildClient(mockFetch: jest.MockedFunction<typeof fetch>) {
  const client = new RetryClient({
    baseUrl: "http://syntra-client.local:8787",
    adminKey: "test-key",
    capsulePath: "/tenants/t/jobs/j/capsules/c",
    timeoutMs: 500,
    fallbackPolicy: DEFAULT_POLICIES[0], // "none"
  });

  // Inject the mock at the global level (RetryClient uses global fetch).
  (global as Record<string, unknown>)["fetch"] = mockFetch;
  return client;
}

// ---------------------------------------------------------------------------
// 1. Successful decide+feedback round-trip
// ---------------------------------------------------------------------------

describe("1. Successful decide+feedback round-trip", () => {
  let mockFetch: jest.MockedFunction<typeof fetch>;

  beforeEach(() => {
    mockFetch = jest.fn<typeof fetch>();
  });

  afterEach(() => {
    jest.restoreAllMocks();
  });

  it("fires both /decide and /feedback when target request succeeds", async () => {
    // Call 1: /decide
    mockFetch.mockResolvedValueOnce(
      makeResponse(200, decideBody(1, "dec_abc"))
    );
    // Call 2: target URL
    mockFetch.mockResolvedValueOnce(makeResponse(200));
    // Call 3: /feedback
    mockFetch.mockResolvedValueOnce(makeResponse(200));

    const client = buildClient(mockFetch);
    const response = await client.request("GET", "https://api.example.com/users");

    // Allow the fire-and-forget feedback promise to settle.
    await new Promise((r) => setTimeout(r, 50));

    expect(response.status).toBe(200);
    expect(mockFetch).toHaveBeenCalledTimes(3);

    const [decideCall, targetCall, feedbackCall] = mockFetch.mock.calls;
    expect((decideCall[0] as string).endsWith("/decide")).toBe(true);
    expect(targetCall[0]).toBe("https://api.example.com/users");
    expect((feedbackCall[0] as string).endsWith("/feedback")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 2. Refusal → fallback policy used
// ---------------------------------------------------------------------------

describe("2. Refusal → fallback policy", () => {
  let mockFetch: jest.MockedFunction<typeof fetch>;

  beforeEach(() => {
    mockFetch = jest.fn<typeof fetch>();
  });

  it("uses fallback policy when Syntra returns refused:true", async () => {
    // /decide returns refusal
    mockFetch.mockResolvedValueOnce(
      makeResponse(200, { decisionId: "dec_ref", refused: true, decisions: [] })
    );
    // target succeeds
    mockFetch.mockResolvedValueOnce(makeResponse(200));
    // feedback
    mockFetch.mockResolvedValueOnce(makeResponse(200));

    const client = buildClient(mockFetch);
    const response = await client.request("GET", "https://api.example.com/data");

    await new Promise((r) => setTimeout(r, 50));

    expect(response.status).toBe(200);
    // Target was still called — fallback doesn't abort the request
    expect(mockFetch.mock.calls[1][0]).toBe("https://api.example.com/data");
  });
});

// ---------------------------------------------------------------------------
// 3. Syntra unreachable → fallback, no throw
// ---------------------------------------------------------------------------

describe("3. Syntra unreachable → fallback, no throw", () => {
  let mockFetch: jest.MockedFunction<typeof fetch>;

  beforeEach(() => {
    mockFetch = jest.fn<typeof fetch>();
  });

  it("does not throw and still executes the request when /decide rejects", async () => {
    // Syntra is down
    mockFetch.mockRejectedValueOnce(new Error("ECONNREFUSED"));
    // target succeeds
    mockFetch.mockResolvedValueOnce(makeResponse(200));

    const client = buildClient(mockFetch);
    await expect(
      client.request("GET", "https://api.example.com/health")
    ).resolves.toBeDefined();

    expect(mockFetch).toHaveBeenCalledTimes(2);
  });
});

// ---------------------------------------------------------------------------
// 4. Feedback failure doesn't reject the original promise
// ---------------------------------------------------------------------------

describe("4. Feedback failure is silently swallowed", () => {
  let mockFetch: jest.MockedFunction<typeof fetch>;

  beforeEach(() => {
    mockFetch = jest.fn<typeof fetch>();
  });

  it("resolves successfully even when /feedback returns a 500", async () => {
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(0, "dec_fb")));
    mockFetch.mockResolvedValueOnce(makeResponse(200));
    // Feedback fails
    mockFetch.mockResolvedValueOnce(makeResponse(500));

    const feedbackErrors: unknown[] = [];
    const client = new RetryClient({
      baseUrl: "http://syntra-client.local:8787",
      adminKey: "k",
      capsulePath: "/tenants/t/jobs/j/capsules/c",
      fallbackPolicy: DEFAULT_POLICIES[0],
      onFeedbackError: (err) => feedbackErrors.push(err),
    });
    (global as Record<string, unknown>)["fetch"] = mockFetch;

    const result = await client.request("GET", "https://api.example.com/x");
    await new Promise((r) => setTimeout(r, 50));

    expect(result.status).toBe(200);
    // onFeedbackError was called, not an unhandled rejection
    expect(feedbackErrors.length).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// 5. Per-host tracker math
// ---------------------------------------------------------------------------

describe("5. EndpointTracker math", () => {
  it("computes correct failure rate after mixed outcomes", () => {
    const tracker = new EndpointTracker();
    const host = "api.example.com";

    tracker.record(host, true, 100);
    tracker.record(host, true, 200);
    tracker.record(host, false, 300);
    tracker.record(host, false, 400);

    const feats = tracker.features(host);
    // 2 successes, 2 failures → failure rate 0.5
    expect(feats.recent_failure_rate).toBeCloseTo(0.5);
  });

  it("computes p99 latency from sorted window", () => {
    const tracker = new EndpointTracker();
    const host = "db.internal";

    // Push 100 distinct latencies: 1ms through 100ms
    for (let i = 1; i <= 100; i++) {
      tracker.record(host, true, i);
    }

    const feats = tracker.features(host);
    // p99 of [1..100]: index = max(0, floor(100*0.99)-1) = max(0, 99-1) = 98 → value 99
    expect(feats.p99_latency_ms).toBe(99);
  });

  it("returns neutral defaults for an unknown endpoint", () => {
    const tracker = new EndpointTracker();
    const feats = tracker.features("unknown.host");
    expect(feats.recent_failure_rate).toBe(0.5);
    expect(feats.p99_latency_ms).toBe(1000.0);
  });
});

// ---------------------------------------------------------------------------
// 6. Retry attempts
// ---------------------------------------------------------------------------

describe("6. Retry executes correct number of attempts", () => {
  let mockFetch: jest.MockedFunction<typeof fetch>;

  beforeEach(() => {
    mockFetch = jest.fn<typeof fetch>();
  });

  it("retries up to maxRetries times on 5xx before succeeding", async () => {
    // /decide → triple (3 retries)
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(2, "dec_retry")));
    // attempt 0 → 500
    mockFetch.mockResolvedValueOnce(makeResponse(503));
    // attempt 1 → 500
    mockFetch.mockResolvedValueOnce(makeResponse(503));
    // attempt 2 → 200 (success)
    mockFetch.mockResolvedValueOnce(makeResponse(200));
    // feedback
    mockFetch.mockResolvedValueOnce(makeResponse(200));

    const client = buildClient(mockFetch);
    const resp = await client.request("GET", "https://api.example.com/flaky");

    await new Promise((r) => setTimeout(r, 50));

    expect(resp.status).toBe(200);
    // 1 decide + 3 target attempts + 1 feedback = 5
    expect(mockFetch).toHaveBeenCalledTimes(5);
  });

  it("stops retrying on a <500 error status", async () => {
    // /decide → triple
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(2, "dec_4xx")));
    // 404 should not retry
    mockFetch.mockResolvedValueOnce(makeResponse(404));
    // feedback
    mockFetch.mockResolvedValueOnce(makeResponse(200));

    const client = buildClient(mockFetch);
    const resp = await client.request("GET", "https://api.example.com/missing");

    await new Promise((r) => setTimeout(r, 50));

    expect(resp.status).toBe(404);
    // 1 decide + 1 target attempt (no retry on 4xx) + 1 feedback = 3
    expect(mockFetch).toHaveBeenCalledTimes(3);
  });
});

// ---------------------------------------------------------------------------
// 7. Backoff timing respects multiplier
// ---------------------------------------------------------------------------

describe("7. Backoff timing", () => {
  beforeEach(() => {
    jest.useFakeTimers();
  });

  afterEach(() => {
    jest.useRealTimers();
  });

  it("waits correct cumulative backoff duration for exponential_fast policy", async () => {
    const mockFetch = jest.fn<typeof fetch>();

    // /decide → exponential_fast (index 3: 3 retries, 100ms initial, ×2)
    mockFetch.mockResolvedValueOnce(makeResponse(200, decideBody(3, "dec_exp")));
    // All target attempts fail with 503
    mockFetch.mockResolvedValue(makeResponse(503));

    const client = buildClient(mockFetch);

    // Start the request but don't await yet
    const requestPromise = client.request("GET", "https://api.example.com/slow");

    // Let the decide + first attempt resolve
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    // Advance through each backoff:
    // attempt 0 → 503, wait 100ms
    await jest.advanceTimersByTimeAsync(100);
    // attempt 1 → 503, wait 200ms (100 × 2)
    await jest.advanceTimersByTimeAsync(200);
    // attempt 2 → 503, wait 400ms (200 × 2)
    await jest.advanceTimersByTimeAsync(400);

    // Drain remaining microtasks
    for (let i = 0; i < 10; i++) {
      await Promise.resolve();
    }

    // The request exhausted all retries; it resolves (with the last 503 response)
    const resp = await requestPromise;
    expect(resp.status).toBe(503);

    // 1 decide + 4 target attempts (0 + 3 retries)
    // feedback is not sent (decisionId present but feedback call happens async)
    const targetCalls = mockFetch.mock.calls.filter(
      (c) => c[0] === "https://api.example.com/slow"
    );
    expect(targetCalls.length).toBe(4);
  });
});

// ---------------------------------------------------------------------------
// policyFromOption guard
// ---------------------------------------------------------------------------

describe("policyFromOption", () => {
  it("returns index 0 for out-of-range indices", () => {
    expect(policyFromOption(-1)).toBe(DEFAULT_POLICIES[0]);
    expect(policyFromOption(999)).toBe(DEFAULT_POLICIES[0]);
  });
});
