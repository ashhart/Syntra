/**
 * Official TypeScript client for the Syntra decision appliance.
 *
 * Retry policy: mutating calls (`POST`/`PUT`/`DELETE` — notably `decide` and
 * `feedback`) are NEVER retried; a transport failure surfaces as a
 * `SyntraNetworkError`. `GET` requests are retried on `5xx` with exponential
 * backoff (idempotent, safe).
 */

import {
  AuthError,
  ForbiddenError,
  NotFoundError,
  RateLimitedError,
  SyntraApiError,
  SyntraNetworkError,
  maskToken,
} from "./errors.js";
import type {
  ContextsResponse,
  CreateJobRequest,
  CreateJobResponse,
  CreateTokenRequest,
  CreateTokenResponse,
  DecideContext,
  DecideOptions,
  DecisionLogEntry,
  DecisionResult,
  FeedbackEvent,
  FeedbackResult,
  FetchLike,
  HealthResponse,
  InstallResponse,
  MemorySidecar,
  ReportPayload,
  SyntraFetchResponse,
} from "./types.js";

export interface SyntraClientOptions {
  /** Appliance origin, e.g. `http://127.0.0.1:8787`. No trailing slash needed. */
  baseUrl: string;
  /** Bearer token: legacy admin key or a scoped token from `createToken`. */
  token: string;
  /** Per-request timeout in milliseconds. Default 10000. */
  timeoutMs?: number;
  /**
   * Injectable `fetch` implementation (defaults to `globalThis.fetch`).
   * Tests can pass a stub to observe/serve requests without a live server.
   */
  fetch?: FetchLike;
  /** Extra attempts for idempotent GETs on 5xx. Default 3 (0 disables). */
  getRetries?: number;
}

const DEFAULT_TIMEOUT_MS = 10_000;
const DEFAULT_GET_RETRIES = 3;
const RETRY_BASE_DELAY_MS = 100;
const RETRY_MAX_DELAY_MS = 2_000;

type HttpMethod = "GET" | "POST" | "PUT" | "DELETE";

interface RequestBodySpec {
  json?: unknown;
  bytes?: Uint8Array;
}

interface RawResponse {
  status: number;
  headers: { get(name: string): string | null };
  text(): Promise<string>;
}

export class SyntraClient {
  readonly baseUrl: string;
  readonly timeoutMs: number;

  private readonly token: string;
  private readonly getRetries: number;
  private readonly doFetch: FetchLike;

  constructor(options: SyntraClientOptions) {
    if (typeof options.baseUrl !== "string" || options.baseUrl.length === 0) {
      throw new TypeError("SyntraClient: baseUrl is required");
    }
    if (typeof options.token !== "string") {
      throw new TypeError(
        'SyntraClient: token must be a string (use "" only for --dev-mode servers)',
      );
    }
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.token = options.token;
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.getRetries = Math.max(0, options.getRetries ?? DEFAULT_GET_RETRIES);
    this.doFetch = options.fetch ?? defaultFetch;
  }

  /** Safe-to-log representation; the bearer token is masked. */
  toString(): string {
    return `SyntraClient(baseUrl=${this.baseUrl}, token=${maskToken(this.token)}, timeoutMs=${this.timeoutMs})`;
  }

  // ── Infra (unversioned, no auth) ─────────────────────────────────────────

  /** `GET /health` — liveness probe. */
  async health(): Promise<HealthResponse> {
    return this.jsonRequest<HealthResponse>("GET", "/health", {}, { auth: false });
  }

  /** `GET /metrics` — Prometheus text exposition format. */
  async metrics(): Promise<string> {
    const res = await this.request("GET", "/metrics", {}, { auth: false });
    return this.textResult(res, "GET", "/metrics");
  }

  // ── Decide / feedback (never retried) ────────────────────────────────────

  /**
   * `POST /v1/.../decide` — run the capsule and return a decision.
   * Default is read-only shadow mode; `opts.learn` opts into in-band weight
   * mutation (silently forced off for `read`-scoped tokens). Never retried.
   */
  async decide(
    tenant: string,
    job: string,
    capsule: string,
    context: DecideContext,
    opts?: DecideOptions,
  ): Promise<DecisionResult> {
    const query = opts?.learn === true ? "?learn=true" : "";
    const path = `${capsulePath(tenant, job, capsule)}/decide${query}`;
    return this.jsonRequest<DecisionResult>("POST", path, { json: context ?? {} });
  }

  /**
   * `POST /v1/.../feedback` — record a reward for a prior decision.
   * Never retried: a duplicate reward would double-count in the learner.
   */
  async feedback(
    tenant: string,
    job: string,
    capsule: string,
    event: FeedbackEvent,
  ): Promise<FeedbackResult> {
    const path = `${capsulePath(tenant, job, capsule)}/feedback`;
    return this.jsonRequest<FeedbackResult>("POST", path, { json: event });
  }

  // ── Capsule install ──────────────────────────────────────────────────────

  /**
   * `POST /v1/.../install` — upload raw `.lyc` graph bytes (magic header
   * `LYCN`). Returns the SHA-256 of the uploaded bytes.
   */
  async installCapsule(
    tenant: string,
    job: string,
    capsule: string,
    lycBytes: Uint8Array,
  ): Promise<InstallResponse> {
    const path = `${capsulePath(tenant, job, capsule)}/install`;
    return this.jsonRequest<InstallResponse>("POST", path, { bytes: lycBytes });
  }

  // ── Inspection (GETs, retried on 5xx) ────────────────────────────────────

  /** `GET /v1/.../report` — live graph view: weights, tries, graph hash. */
  async report(tenant: string, job: string, capsule: string): Promise<ReportPayload> {
    return this.jsonRequest<ReportPayload>("GET", `${capsulePath(tenant, job, capsule)}/report`);
  }

  /** `GET /v1/.../contexts` — one row per (nodeId, contextKey) bucket. */
  async contexts(tenant: string, job: string, capsule: string): Promise<ContextsResponse> {
    return this.jsonRequest<ContextsResponse>("GET", `${capsulePath(tenant, job, capsule)}/contexts`);
  }

  /** `GET /v1/.../memory` — full memory sidecar (opaque, schema v7). */
  async memory(tenant: string, job: string, capsule: string): Promise<MemorySidecar> {
    return this.jsonRequest<MemorySidecar>("GET", `${capsulePath(tenant, job, capsule)}/memory`);
  }

  /**
   * `GET /v1/.../decisions` — append-only NDJSON decision log, parsed into
   * typed entries.
   */
  async decisions(
    tenant: string,
    job: string,
    capsule: string,
  ): Promise<DecisionLogEntry[]> {
    const path = `${capsulePath(tenant, job, capsule)}/decisions`;
    const res = await this.request("GET", path, {});
    const text = await this.textResult(res, "GET", path);
    return text
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0)
      .map((line) => JSON.parse(line) as DecisionLogEntry);
  }

  // ── Admin ────────────────────────────────────────────────────────────────

  /**
   * `POST /v1/admin/tokens` — issue a scoped token (Admin scope required).
   * The raw token value appears only in this response.
   */
  async createToken(request: CreateTokenRequest): Promise<CreateTokenResponse> {
    return this.jsonRequest<CreateTokenResponse>("POST", "/v1/admin/tokens", { json: request });
  }

  /** `POST /v1/tenants/{tenant}/jobs` — create a job (409 on duplicate id). */
  async createJob(tenant: string, job: CreateJobRequest): Promise<CreateJobResponse> {
    return this.jsonRequest<CreateJobResponse>(
      "POST",
      `/v1/tenants/${encodeURIComponent(tenant)}/jobs`,
      { json: job },
    );
  }

  // ── Plumbing ─────────────────────────────────────────────────────────────

  private async jsonRequest<T>(
    method: HttpMethod,
    path: string,
    body: RequestBodySpec = {},
    extra?: { auth?: boolean },
  ): Promise<T> {
    const res = await this.request(method, path, body, extra);
    const parsed = await parseJsonBody(res);
    if (res.status < 400) return parsed as T;
    throw this.mapError(method, path, res, parsed);
  }

  private async textResult(res: RawResponse, method: HttpMethod, path: string): Promise<string> {
    const text = await res.text();
    if (res.status < 400) return text;
    throw this.mapError(method, path, res, text.length > 0 ? text : undefined);
  }

  private mapError(
    method: HttpMethod,
    path: string,
    res: RawResponse,
    body: unknown,
  ): SyntraApiError {
    const init = { method, path, status: res.status, body, token: this.token };
    switch (res.status) {
      case 401:
        return new AuthError(init);
      case 403:
        return new ForbiddenError(init);
      case 404:
        return new NotFoundError(init);
      case 429:
        return new RateLimitedError(init, parseRetryAfterMs(res, body));
      default:
        return new SyntraApiError("SyntraApiError", init);
    }
  }

  /**
   * Perform one logical request. GETs retry 5xx with exponential backoff;
   * every other method is attempted exactly once.
   */
  private async request(
    method: HttpMethod,
    path: string,
    body: RequestBodySpec,
    extra?: { auth?: boolean },
  ): Promise<RawResponse> {
    const retry = method === "GET";
    let attempt = 0;
    for (;;) {
      const res = await this.attempt(method, path, body, extra);
      if (res.status >= 500 && retry && attempt < this.getRetries) {
        attempt += 1;
        const { promise: nap, resolve: wake } = Promise.withResolvers<void>();
        setTimeout(wake, Math.min(RETRY_BASE_DELAY_MS * 2 ** (attempt - 1), RETRY_MAX_DELAY_MS));
        await nap;
        continue;
      }
      return res;
    }
  }

  private async attempt(
    method: HttpMethod,
    path: string,
    body: RequestBodySpec,
    extra?: { auth?: boolean },
  ): Promise<RawResponse> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    const headers: Record<string, string> = {};
    if (extra?.auth !== false && this.token.length > 0) {
      headers["authorization"] = `Bearer ${this.token}`;
    }
    let payload: string | Uint8Array | undefined;
    if (body.json !== undefined) {
      payload = JSON.stringify(body.json);
      headers["content-type"] = "application/json";
    } else if (body.bytes !== undefined) {
      payload = body.bytes;
      headers["content-type"] = "application/octet-stream";
    }
    try {
      const res: SyntraFetchResponse = await this.doFetch(this.baseUrl + path, {
        method,
        headers,
        body: payload as BodyInit | undefined,
        signal: controller.signal,
      });
      return res;
    } catch (cause) {
      throw new SyntraNetworkError({
        method,
        path,
        token: this.token,
        cause,
        timedOut: controller.signal.aborted,
      });
    } finally {
      clearTimeout(timer);
    }
  }
}

// The canonical capsule path prefix, encoded identically for every route.
function capsulePath(tenant: string, job: string, capsule: string): string {
  return `/v1/tenants/${encodeURIComponent(tenant)}/jobs/${encodeURIComponent(
    job,
  )}/capsules/${encodeURIComponent(capsule)}`;
}

async function parseJsonBody(res: { text(): Promise<string> }): Promise<unknown> {
  const text = await res.text();
  if (text.length === 0) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/** `Retry-After` header first (whole seconds), then body `retryAfterSeconds`. */
function parseRetryAfterMs(
  res: { headers: { get(name: string): string | null } },
  body: unknown,
): number {
  const header = res.headers.get("retry-after");
  if (header !== null) {
    const seconds = Number(header);
    if (Number.isFinite(seconds) && seconds >= 0) return Math.round(seconds * 1000);
  }
  if (body && typeof body === "object" && !Array.isArray(body)) {
    const seconds = (body as Record<string, unknown>)["retryAfterSeconds"];
    if (typeof seconds === "number" && Number.isFinite(seconds) && seconds >= 0) {
      return Math.round(seconds * 1000);
    }
  }
  return 1_000;
}

// DI seam: keep the live binding to globalThis.fetch lazy and swappable.
const defaultFetch: FetchLike = (url, init) => fetch(url, init);
