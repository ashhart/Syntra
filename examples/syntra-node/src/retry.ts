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

import { SyntraClient } from "./index.js";
import type { SyntraClientOptions } from "./index.js";

// ---------------------------------------------------------------------------
// Policy table
// ---------------------------------------------------------------------------

/** Concrete retry behavior for one policy option. */
export interface RetryPolicy {
  name: string;
  maxRetries: number;
  initialBackoffMs: number;
  backoffMultiplier: number;
}

/** All five canonical policies. Index order matches the demo capsule YAML. */
export const DEFAULT_POLICIES: readonly RetryPolicy[] = [
  { name: "none",             maxRetries: 0, initialBackoffMs: 0,   backoffMultiplier: 1.0 },
  { name: "single",           maxRetries: 1, initialBackoffMs: 0,   backoffMultiplier: 1.0 },
  { name: "triple",           maxRetries: 3, initialBackoffMs: 0,   backoffMultiplier: 1.0 },
  { name: "exponential_fast", maxRetries: 3, initialBackoffMs: 100, backoffMultiplier: 2.0 },
  { name: "exponential_slow", maxRetries: 3, initialBackoffMs: 500, backoffMultiplier: 2.0 },
];

/** Returns the policy at `index`, falling back to index 0 if out of range. */
export function policyFromOption(index: number): RetryPolicy {
  if (index >= 0 && index < DEFAULT_POLICIES.length) {
    return DEFAULT_POLICIES[index];
  }
  return DEFAULT_POLICIES[0];
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/** Observed outcome of a single request execution (after all retries). */
export interface RequestOutcome {
  success: boolean;
  totalLatencyMs: number;
  retriesUsed: number;
  statusCode: number | null;
}

// ---------------------------------------------------------------------------
// Per-host tracker
// ---------------------------------------------------------------------------

/**
 * Rolling window of (success, latency_ms) outcomes per host.
 * Drives the feature vectors sent to Syntra's /decide.
 */
export class EndpointTracker {
  private readonly window: number;
  private readonly outcomes: Map<string, number[]>;
  private readonly latencies: Map<string, number[]>;

  constructor(window = 100) {
    this.window = window;
    this.outcomes = new Map();
    this.latencies = new Map();
  }

  record(endpoint: string, success: boolean, latencyMs: number): void {
    if (!this.outcomes.has(endpoint)) {
      this.outcomes.set(endpoint, []);
      this.latencies.set(endpoint, []);
    }

    const outs = this.outcomes.get(endpoint)!;
    const lats = this.latencies.get(endpoint)!;

    outs.push(success ? 1 : 0);
    lats.push(latencyMs);

    if (outs.length > this.window) outs.shift();
    if (lats.length > this.window) lats.shift();
  }

  features(endpoint: string): Record<string, number> {
    const outs = this.outcomes.get(endpoint) ?? [];
    const lats = this.latencies.get(endpoint) ?? [];
    const hour = ((Date.now() / 3_600_000) % 24);

    if (outs.length === 0) {
      return { recent_failure_rate: 0.5, p99_latency_ms: 1000.0, hour };
    }

    const failureRate = 1.0 - outs.reduce((a, b) => a + b, 0) / outs.length;

    let p99 = 1000.0;
    if (lats.length > 0) {
      const sorted = [...lats].sort((a, b) => a - b);
      const idx = Math.max(0, Math.floor(sorted.length * 0.99) - 1);
      p99 = sorted[idx];
    }

    return { recent_failure_rate: failureRate, p99_latency_ms: p99, hour };
  }
}

// ---------------------------------------------------------------------------
// RetryClient options
// ---------------------------------------------------------------------------

export interface RetryClientOptions extends SyntraClientOptions {
  /** Policy to use when Syntra is unreachable or refuses. Default: "single". */
  fallbackPolicy?: RetryPolicy;
  /**
   * Optional callback invoked when the /feedback POST fails.
   * If omitted, feedback failures are silently swallowed.
   */
  onFeedbackError?: (err: unknown) => void;
}

// ---------------------------------------------------------------------------
// RetryClient
// ---------------------------------------------------------------------------

/**
 * HTTP client that asks Syntra for a retry policy on every request.
 *
 * Uses the built-in `fetch` API (requires Node 18+). Never throws for
 * Syntra-side failures; falls back to `fallbackPolicy` instead.
 */
export class RetryClient {
  private readonly syntra: SyntraClient;
  private readonly fallbackPolicy: RetryPolicy;
  private readonly onFeedbackError: ((err: unknown) => void) | undefined;
  private readonly tracker: EndpointTracker;

  constructor(options: RetryClientOptions) {
    this.syntra = new SyntraClient(options);
    this.fallbackPolicy = options.fallbackPolicy ?? policyFromOption(1); // "single"
    this.onFeedbackError = options.onFeedbackError;
    this.tracker = new EndpointTracker();
  }

  /**
   * Execute a fetch-compatible request with Syntra-chosen retry policy.
   *
   * Never rejects due to Syntra unavailability — always falls back.
   * May still reject if all retries of the target URL are exhausted.
   */
  async request(
    method: string,
    url: string,
    init?: Omit<RequestInit, "method" | "signal">,
  ): Promise<Response> {
    const endpoint = endpointKey(url);
    const features = this.tracker.features(endpoint);

    const [policy, decisionId] = await this._getPolicy(features);
    const [outcome, response] = await this._executeWithPolicy(method, url, policy, init);

    this.tracker.record(endpoint, outcome.success, outcome.totalLatencyMs);

    if (decisionId !== null) {
      // Fire-and-forget; failures never propagate to the caller.
      this._sendFeedback(decisionId, outcome).catch((err: unknown) => {
        if (this.onFeedbackError) {
          this.onFeedbackError(err);
        }
      });
    }

    if (response !== null) {
      return response;
    }

    throw new Error(`All retries exhausted for ${method} ${url}`);
  }

  /** Resolve a Syntra retry policy; returns fallback on any error. */
  private async _getPolicy(
    features: Record<string, number>,
  ): Promise<[RetryPolicy, string | null]> {
    try {
      const data = await this.syntra.decide({ features });

      if (data.refused) {
        return [this.fallbackPolicy, data.decisionId ?? null];
      }

      const decisions = data.decisions ?? [];
      if (decisions.length === 0) {
        return [this.fallbackPolicy, null];
      }

      const optionIdx = decisions[0].chosen_option;
      return [policyFromOption(optionIdx), data.decisionId ?? null];
    } catch {
      return [this.fallbackPolicy, null];
    }
  }

  /** Execute the request up to policy.maxRetries + 1 times. */
  private async _executeWithPolicy(
    method: string,
    url: string,
    policy: RetryPolicy,
    init?: Omit<RequestInit, "method" | "signal">,
  ): Promise<[RequestOutcome, Response | null]> {
    const start = Date.now();
    let retriesUsed = 0;
    let lastResponse: Response | null = null;
    let backoffMs = policy.initialBackoffMs;

    for (let attempt = 0; attempt <= policy.maxRetries; attempt++) {
      try {
        const response = await fetch(url, { ...init, method });
        lastResponse = response;

        if (response.status < 500) {
          const totalLatencyMs = Date.now() - start;
          return [
            {
              success: response.status < 400,
              totalLatencyMs,
              retriesUsed,
              statusCode: response.status,
            },
            response,
          ];
        }
        // 5xx — fall through to retry logic
      } catch {
        // Transport error — treat as retriable
      }

      if (attempt < policy.maxRetries) {
        retriesUsed++;
        if (backoffMs > 0) {
          await sleep(backoffMs);
          backoffMs = Math.floor(backoffMs * policy.backoffMultiplier);
        }
      }
    }

    const totalLatencyMs = Date.now() - start;
    return [
      {
        success: false,
        totalLatencyMs,
        retriesUsed,
        statusCode: lastResponse?.status ?? null,
      },
      lastResponse,
    ];
  }

  /** Send reward feedback to Syntra. Errors are propagated to the caller for handling. */
  private async _sendFeedback(
    decisionId: string,
    outcome: RequestOutcome,
  ): Promise<void> {
    const latencyPenalty = Math.min(outcome.totalLatencyMs / 10_000, 1.0);
    const reward = (outcome.success ? 1.0 : 0.0) - 0.3 * latencyPenalty;
    await this.syntra.feedback({ decisionId, reward });
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function endpointKey(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
