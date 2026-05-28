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
export type DecideInput = {
    contextKey: string;
    features?: never;
} | {
    features: Record<string, number>;
    contextKey?: never;
};
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
export declare class SyntraClient {
    private readonly baseUrl;
    private readonly capsulePath;
    private readonly timeoutMs;
    private readonly authHeader;
    constructor(options: SyntraClientOptions);
    /**
     * POST /decide — returns the full response envelope.
     * Throws on HTTP error or transport failure.
     */
    decide(input: DecideInput): Promise<DecideResponse>;
    /**
     * POST /feedback — sends a reward for a prior decision.
     * Throws on HTTP error or transport failure.
     */
    feedback(input: FeedbackInput): Promise<void>;
    /** Wraps global fetch with an AbortController timeout. */
    private _fetch;
}
export { RetryClient, RetryPolicy, RequestOutcome } from "./retry.js";
export type { RetryClientOptions } from "./retry.js";
//# sourceMappingURL=index.d.ts.map