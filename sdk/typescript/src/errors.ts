/**
 * Typed error hierarchy for the Syntra HTTP API.
 *
 * The raw bearer token is NEVER embedded in any error message; only a masked
 * fingerprint is carried for debuggability. `toString()` (and therefore any
 * logging) is safe to print.
 */

/** Mask a token to `xxxx…yyyy` (or fully masked when too short to leak safely). */
export function maskToken(token: string): string {
  if (token.length <= 8) return "*".repeat(token.length);
  return `${token.slice(0, 4)}\u2026${token.slice(-4)}`;
}

export interface SyntraApiErrorInit {
  method: string;
  path: string;
  status: number;
  /** Parsed JSON body when available, raw text otherwise, `undefined` when empty. */
  body: unknown;
  /** Raw bearer token; stored only to derive the masked fingerprint. */
  token: string;
}

/** Base class for every non-2xx Syntra API response. */
export class SyntraApiError extends Error {
  readonly httpMethod: string;
  readonly path: string;
  readonly status: number;
  readonly body: unknown;
  /** Server-decoded error message (`{"error": "..."}` bodies), when present. */
  readonly apiMessage: string | undefined;
  /** Masked bearer token, safe to log. The raw token is never stored. */
  readonly tokenFingerprint: string;

  constructor(name: string, init: SyntraApiErrorInit) {
    const apiMessage = extractApiMessage(init.body);
    super(
      `${init.method} ${init.path} failed with ${init.status}${apiMessage ? `: ${apiMessage}` : ""}`,
    );
    this.name = name;
    this.httpMethod = init.method;
    this.path = init.path;
    this.status = init.status;
    this.body = init.body;
    this.apiMessage = apiMessage;
    this.tokenFingerprint = maskToken(init.token);
  }

  override toString(): string {
    return `${this.name} [${this.httpMethod} ${this.path} -> ${this.status}] ${this.message} (token=${this.tokenFingerprint})`;
  }
}

/** `401 Unauthorized` — missing or invalid bearer token. */
export class AuthError extends SyntraApiError {
  constructor(init: SyntraApiErrorInit) {
    super("AuthError", init);
  }
}

/** `403 Forbidden` — valid token whose scope does not allow the action. */
export class ForbiddenError extends AuthError {
  constructor(init: SyntraApiErrorInit) {
    super(init);
    this.name = "ForbiddenError";
  }
}

/** `404 Not Found` — unknown tenant/job/capsule or resource. */
export class NotFoundError extends SyntraApiError {
  constructor(init: SyntraApiErrorInit) {
    super("NotFoundError", init);
  }
}

/** `429 Too Many Requests`; `retryAfterMs` from the `Retry-After` header. */
export class RateLimitedError extends SyntraApiError {
  readonly retryAfterMs: number;

  constructor(init: SyntraApiErrorInit, retryAfterMs: number) {
    super("RateLimitedError", init);
    this.retryAfterMs = retryAfterMs;
  }
}

/** Network failure or client-side timeout (no HTTP response was received). */
export class SyntraNetworkError extends SyntraApiError {
  constructor(init: { method: string; path: string; token: string; cause: unknown; timedOut: boolean }) {
    super("SyntraNetworkError", {
      method: init.method,
      path: init.path,
      status: 0,
      body: undefined,
      token: init.token,
    });
    this.cause = init.cause instanceof Error ? init.cause : new Error(String(init.cause));
    this.timedOut = init.timedOut;
  }

  /** True when the request was aborted after `timeoutMs`. */
  readonly timedOut: boolean;
}

function extractApiMessage(body: unknown): string | undefined {
  if (typeof body === "string" && body.length > 0) return body;
  if (body && typeof body === "object" && !Array.isArray(body)) {
    const err = (body as Record<string, unknown>)["error"];
    if (typeof err === "string" && err.length > 0) return err;
  }
  return undefined;
}
