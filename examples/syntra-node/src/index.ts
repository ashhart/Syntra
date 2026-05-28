/**
 * @ashhart/syntra-client
 *
 * HTTP client for the Syntra adaptive decision appliance.
 * Wraps /decide and /feedback with fail-safe fallback semantics.
 *
 * Apache-2.0
 */

export interface SyntraClientOptions {
  /** Base URL of the Syntra instance, e.g. "http://localhost:8787". */
  baseUrl: string;
  /** Bearer token for the Authorization header. */
  adminKey: string;
  /** Capsule path, e.g. "/tenants/myteam/jobs/retry/capsules/router". */
  capsulePath: string;
  /** Per-request timeout in milliseconds. Default: 2000. */
  timeoutMs?: number;
}

/** Shape of a single decision entry returned by /decide. */
export interface DecisionEntry {
  chosen_option: number;
  [key: string]: unknown;
}

/** Full /decide response envelope. */
export interface DecideResponse {
  decisionId?: string;
  decisions?: DecisionEntry[];
  refused?: boolean;
  confidence?: Record<string, unknown>;
}

/** Input for the decide() method. One of contextKey or features is required. */
export type DecideInput =
  | { contextKey: string; features?: never }
  | { features: Record<string, number>; contextKey?: never };

/** Input for the feedback() method. */
export interface FeedbackInput {
  decisionId: string;
  reward: number;
}

/**
 * Low-level Syntra HTTP client.
 *
 * All network errors are surfaced as thrown Error instances; callers are
 * responsible for fallback. For the high-level retry-domain client that
 * handles fallback automatically, see RetryClient in ./retry.
 */
export class SyntraClient {
  private readonly baseUrl: string;
  private readonly capsulePath: string;
  private readonly timeoutMs: number;
  private readonly authHeader: Record<string, string>;

  constructor(options: SyntraClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/$/, "");
    this.capsulePath = options.capsulePath.replace(/\/$/, "");
    this.timeoutMs = options.timeoutMs ?? 2000;
    this.authHeader = { Authorization: `Bearer ${options.adminKey}` };
  }

  /**
   * POST /decide — returns the full response envelope.
   * Throws on HTTP error or transport failure.
   */
  async decide(input: DecideInput): Promise<DecideResponse> {
    const url = `${this.baseUrl}${this.capsulePath}/decide`;
    const body: Record<string, unknown> =
      "contextKey" in input && input.contextKey !== undefined
        ? { contextKey: input.contextKey }
        : { features: input.features };

    const response = await this._fetch(url, {
      method: "POST",
      headers: { ...this.authHeader, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });

    if (!response.ok) {
      throw new Error(`Syntra /decide returned HTTP ${response.status}`);
    }

    return response.json() as Promise<DecideResponse>;
  }

  /**
   * POST /feedback — sends a reward for a prior decision.
   * Throws on HTTP error or transport failure.
   */
  async feedback(input: FeedbackInput): Promise<void> {
    const url = `${this.baseUrl}${this.capsulePath}/feedback`;
    const response = await this._fetch(url, {
      method: "POST",
      headers: { ...this.authHeader, "Content-Type": "application/json" },
      body: JSON.stringify({
        decisionId: input.decisionId,
        reward: input.reward,
      }),
    });

    if (!response.ok) {
      throw new Error(`Syntra /feedback returned HTTP ${response.status}`);
    }
  }

  /** Wraps global fetch with an AbortController timeout. */
  private async _fetch(url: string, init: RequestInit): Promise<Response> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      return await fetch(url, { ...init, signal: controller.signal });
    } finally {
      clearTimeout(timer);
    }
  }
}

export { RetryClient, RetryPolicy, RequestOutcome } from "./retry.js";
export type { RetryClientOptions } from "./retry.js";
