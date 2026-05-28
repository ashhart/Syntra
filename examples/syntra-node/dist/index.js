/**
 * @ashhart/syntra-client
 *
 * HTTP client for the Syntra adaptive decision appliance.
 * Wraps /decide and /feedback with fail-safe fallback semantics.
 *
 * Apache-2.0
 */
/**
 * Low-level Syntra HTTP client.
 *
 * All network errors are surfaced as thrown Error instances; callers are
 * responsible for fallback. For the high-level retry-domain client that
 * handles fallback automatically, see RetryClient in ./retry.
 */
export class SyntraClient {
    baseUrl;
    capsulePath;
    timeoutMs;
    authHeader;
    constructor(options) {
        this.baseUrl = options.baseUrl.replace(/\/$/, "");
        this.capsulePath = options.capsulePath.replace(/\/$/, "");
        this.timeoutMs = options.timeoutMs ?? 2000;
        this.authHeader = { Authorization: `Bearer ${options.adminKey}` };
    }
    /**
     * POST /decide — returns the full response envelope.
     * Throws on HTTP error or transport failure.
     */
    async decide(input) {
        const url = `${this.baseUrl}${this.capsulePath}/decide`;
        const body = "contextKey" in input && input.contextKey !== undefined
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
        return response.json();
    }
    /**
     * POST /feedback — sends a reward for a prior decision.
     * Throws on HTTP error or transport failure.
     */
    async feedback(input) {
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
    async _fetch(url, init) {
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), this.timeoutMs);
        try {
            return await fetch(url, { ...init, signal: controller.signal });
        }
        finally {
            clearTimeout(timer);
        }
    }
}
export { RetryClient } from "./retry.js";
//# sourceMappingURL=index.js.map