/**
 * RetryClient — Syntra-driven retry policy selection for HTTP clients.
 *
 * Mirrors the Python syntra_retry package:
 *   1. /decide on Syntra with current endpoint features (failure_rate, p99, hour).
 *   2. Execute the real HTTP request with the chosen retry policy.
 *   3. /feedback to Syntra with success + latency reward.
 *
 * Falls back to a configured default policy whenever Syntra is unreachable,
 * refuses, or returns a malformed response — a Syntra outage degrades adaptive
 * retry to "always fall back" without breaking the request flow.
 *
 * Apache-2.0
 */
import type { SyntraClientOptions } from "./index.js";
/** Concrete retry behavior for one policy option. */
export interface RetryPolicy {
    name: string;
    maxRetries: number;
    initialBackoffMs: number;
    backoffMultiplier: number;
}
/** All five canonical policies. Index order matches the demo capsule YAML. */
export declare const DEFAULT_POLICIES: readonly RetryPolicy[];
/** Returns the policy at `index`, falling back to index 0 if out of range. */
export declare function policyFromOption(index: number): RetryPolicy;
/** Observed outcome of a single request execution (after all retries). */
export interface RequestOutcome {
    success: boolean;
    totalLatencyMs: number;
    retriesUsed: number;
    statusCode: number | null;
}
/**
 * Rolling window of (success, latency_ms) outcomes per host.
 * Drives the feature vectors sent to Syntra's /decide.
 */
export declare class EndpointTracker {
    private readonly window;
    private readonly outcomes;
    private readonly latencies;
    constructor(window?: number);
    record(endpoint: string, success: boolean, latencyMs: number): void;
    features(endpoint: string): Record<string, number>;
}
export interface RetryClientOptions extends SyntraClientOptions {
    /** Policy to use when Syntra is unreachable or refuses. Default: "single". */
    fallbackPolicy?: RetryPolicy;
    /**
     * Optional callback invoked when the /feedback POST fails.
     * If omitted, feedback failures are silently swallowed.
     */
    onFeedbackError?: (err: unknown) => void;
}
/**
 * HTTP client that asks Syntra for a retry policy on every request.
 *
 * Uses the built-in `fetch` API (requires Node 18+). Never throws for
 * Syntra-side failures; falls back to `fallbackPolicy` instead.
 */
export declare class RetryClient {
    private readonly syntra;
    private readonly fallbackPolicy;
    private readonly onFeedbackError;
    private readonly tracker;
    constructor(options: RetryClientOptions);
    /**
     * Execute a fetch-compatible request with Syntra-chosen retry policy.
     *
     * Never rejects due to Syntra unavailability — always falls back.
     * May still reject if all retries of the target URL are exhausted.
     */
    request(method: string, url: string, init?: Omit<RequestInit, "method" | "signal">): Promise<Response>;
    /** Resolve a Syntra retry policy; returns fallback on any error. */
    private _getPolicy;
    /** Execute the request up to policy.maxRetries + 1 times. */
    private _executeWithPolicy;
    /** Send reward feedback to Syntra. Errors are propagated to the caller for handling. */
    private _sendFeedback;
}
//# sourceMappingURL=retry.d.ts.map