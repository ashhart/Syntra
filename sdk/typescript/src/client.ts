/**
 * `SyntraClient`: the Syntra v2 HTTP API for one capsule, plus the
 * operator's token routes.
 *
 * Retries: GET requests and writes that are safe to repeat (decide with an
 * `eventId`, reward with an `idempotencyKey`, decision uploads, reward
 * uploads whose items all carry a key) are retried on transport errors and
 * on 503 and 429: after `Retry-After` when the server sends one, otherwise
 * with capped exponential backoff. Every other write is sent once.
 */

import {
  HttpError,
  SyntraError,
  TransportError,
  httpErrorMessage,
  transportErrorMessage,
} from "./errors.ts";
import type {
  AuditList,
  Context,
  DecideOptions,
  DecideRequest,
  DecideResponse,
  DecisionList,
  DecisionSpec,
  DecisionUploadResponse,
  DecisionWithRewards,
  EvaluateRequest,
  Health,
  IssueTokenResponse,
  ListDecisionsOptions,
  Model,
  OpeReport,
  PromoteRefusal,
  PromoteRequest,
  PromoteResponse,
  PromoteResult,
  PublishedModel,
  Revoked,
  RewardOptions,
  RewardRequest,
  RewardResponse,
  RewardUploadItem,
  RewardUploadResponse,
  Scope,
  SpecPatch,
  TokenList,
  UploadedDecision,
  WhoAmI,
} from "./types.ts";

const DEFAULT_TIMEOUT_MS = 10_000;
const DEFAULT_MAX_RETRIES = 3;
const DEFAULT_MAX_RETRY_DELAY_MS = 10_000;
/** First backoff step when there is no `Retry-After`; it doubles per attempt. */
const BACKOFF_BASE_MS = 100;
/** Shorter tokens are not scrubbed from messages: they would match ordinary words. */
const MIN_REDACTED_TOKEN_LENGTH = 8;

export interface SyntraClientOptions {
  /** Server URL, e.g. `http://127.0.0.1:8787`. A path (a proxy prefix) is kept. */
  url: string;
  /** The operator key or a scoped token, sent as `Authorization: Bearer`. */
  token: string;
  tenant: string;
  job: string;
  capsule: string;
  /** Per-attempt timeout in milliseconds (default 10000). */
  timeoutMs?: number;
  /** Extra attempts for requests that are safe to repeat (default 3; 0 turns retries off). */
  maxRetries?: number;
  /**
   * Longest wait before a retry, in milliseconds (default 10000). When the
   * server's `Retry-After` asks for longer, the error is thrown instead.
   */
  maxRetryDelayMs?: number;
  /** A `fetch` implementation (default `globalThis.fetch`). */
  fetch?: typeof fetch;
}

type Method = "GET" | "POST" | "PUT" | "DELETE";
type Query = Record<string, string | number | boolean | undefined>;

interface Call {
  body?: unknown;
  query?: Query;
  /** Safe to send again. GET always is. */
  idempotent?: boolean;
  /** Send the credential (default true). */
  auth?: boolean;
  ifNoneMatch?: string;
  /** Keep a 2xx answer as text, unparsed (`Reply.text`). */
  raw?: boolean;
}

interface Reply {
  method: Method;
  /** Path and query, as error messages show them. */
  path: string;
  status: number;
  headers: Headers;
  /** Parsed JSON, the raw text when it is not JSON, or undefined when empty (or kept raw). */
  body: unknown;
  json: boolean;
  /** The response text. */
  text: string;
}

export class SyntraClient {
  /** The server URL without a trailing slash. */
  readonly url: string;
  readonly tenant: string;
  readonly job: string;
  readonly capsule: string;
  readonly timeoutMs: number;
  readonly maxRetries: number;
  readonly maxRetryDelayMs: number;
  // Private fields stay out of util.inspect, console.log and JSON.stringify.
  readonly #token: string;
  readonly #fetch: typeof fetch;
  readonly #capsulePath: string;

  constructor(options: SyntraClientOptions) {
    this.url = baseUrl(options.url);
    if (typeof options.token !== "string") throw new TypeError("token must be a string");
    this.#capsulePath =
      `/v1/tenants/${segment(options.tenant, "tenant")}` +
      `/jobs/${segment(options.job, "job")}` +
      `/capsules/${segment(options.capsule, "capsule")}`;
    this.tenant = options.tenant;
    this.job = options.job;
    this.capsule = options.capsule;
    this.timeoutMs = numberOption(options.timeoutMs, DEFAULT_TIMEOUT_MS, "timeoutMs", 1);
    this.maxRetries = numberOption(options.maxRetries, DEFAULT_MAX_RETRIES, "maxRetries", 0);
    if (!Number.isInteger(this.maxRetries)) throw new TypeError("maxRetries must be an integer");
    this.maxRetryDelayMs = numberOption(
      options.maxRetryDelayMs,
      DEFAULT_MAX_RETRY_DELAY_MS,
      "maxRetryDelayMs",
      0,
    );
    const fetchImpl = options.fetch ?? globalThis.fetch;
    if (typeof fetchImpl !== "function") {
      throw new TypeError("there is no global fetch; pass options.fetch");
    }
    this.#fetch = fetchImpl;
    this.#token = options.token;
  }

  toString(): string {
    return `SyntraClient(${this.url} ${this.tenant}/${this.job}/${this.capsule})`;
  }

  // ── Decide and reward ─────────────────────────────────────────────────

  /**
   * `POST .../decide`: choose an action for `context`.
   *
   * With `eventId` the request is retried: the server answers a repeat of
   * the same request with the stored decision (`replayed: true`). It
   * compares request bytes, so repeat a call with the same arguments.
   */
  async decide(context: Context = {}, options: DecideOptions = {}): Promise<DecideResponse> {
    const body: DecideRequest = { context };
    if (options.actions !== undefined) body.actions = options.actions;
    if (typeof options.exclude === "string") {
      throw new TypeError("exclude must be a list of action ids, not a string");
    }
    if (options.exclude !== undefined) body.excludedActions = Array.from(options.exclude);
    if (options.baseline !== undefined) body.baselineAction = options.baseline;
    if (options.eventId !== undefined) body.eventId = options.eventId;
    if (options.durable === true) body.durable = true;
    return this.#json<DecideResponse>("POST", this.#capsule("decide"), {
      body,
      idempotent: options.eventId !== undefined,
    });
  }

  /**
   * `POST .../reward`: record the outcome of a decision. Retried only with
   * an `idempotencyKey`.
   */
  async reward(decisionId: string, value: number, options: RewardOptions = {}): Promise<RewardResponse> {
    if (typeof value !== "number" || !Number.isFinite(value)) {
      throw new TypeError("reward must be a finite number");
    }
    const body: RewardRequest = { decisionId, reward: value };
    if (options.idempotencyKey !== undefined) body.idempotencyKey = options.idempotencyKey;
    if (options.detail !== undefined) body.detail = options.detail;
    if (options.durable === true) body.durable = true;
    return this.#json<RewardResponse>("POST", this.#capsule("reward"), {
      body,
      idempotent: options.idempotencyKey !== undefined,
    });
  }

  // ── Spec, model and logs ──────────────────────────────────────────────

  /**
   * `PUT .../spec`: apply a JSON merge patch to the spec, creating the
   * capsule if needed; with `replace`, apply it to the default spec
   * instead. Returns the full spec.
   */
  async putSpec(patch: SpecPatch, options: { replace?: boolean } = {}): Promise<DecisionSpec> {
    return this.#json<DecisionSpec>("PUT", this.#capsule("spec"), {
      body: patch,
      query: options.replace === true ? { replace: true } : undefined,
    });
  }

  /** `GET .../spec`, every default filled in. */
  async getSpec(): Promise<DecisionSpec> {
    return this.#json<DecisionSpec>("GET", this.#capsule("spec"));
  }

  /**
   * `GET .../model`: the live spec and model version. With `snapshot`, the
   * model published for local evaluation; with `ifNoneMatch` set to a
   * `modelTag`, null while that is still the published model.
   */
  model(options?: { snapshot?: false }): Promise<Model>;
  model(options: { snapshot: true; ifNoneMatch?: undefined }): Promise<PublishedModel>;
  model(options: { snapshot: true; ifNoneMatch?: string }): Promise<PublishedModel | null>;
  async model(
    options: { snapshot?: boolean; ifNoneMatch?: string } = {},
  ): Promise<Model | PublishedModel | null> {
    const call: Call = {};
    if (options.snapshot === true) {
      call.query = { snapshot: true };
      if (options.ifNoneMatch !== undefined) call.ifNoneMatch = `"${options.ifNoneMatch}"`;
    }
    const reply = await this.#send("GET", this.#capsule("model"), call);
    if (reply.status === 304 && call.ifNoneMatch !== undefined) return null;
    return this.#result<Model | PublishedModel>(reply);
  }

  /**
   * `GET .../model?snapshot=true` as the unparsed response text, for
   * decoders that must not go through `JSON.parse`: a fixed seed is a
   * u64, and the model tag is verified over the decide section as the
   * server serialized it. `LocalDecider` uses it. With `ifNoneMatch` set
   * to a `modelTag`, null while that is still the published model.
   */
  async modelText(options: { ifNoneMatch?: string } = {}): Promise<string | null> {
    const call: Call = { query: { snapshot: true }, raw: true };
    if (options.ifNoneMatch !== undefined) call.ifNoneMatch = `"${options.ifNoneMatch}"`;
    const reply = await this.#send("GET", this.#capsule("model"), call);
    if (reply.status === 304 && call.ifNoneMatch !== undefined) return null;
    if (reply.status < 200 || reply.status >= 300) this.#result<never>(reply);
    return reply.text;
  }

  /** `GET .../decisions/{id}`: one decision with its rewards. */
  async decision(decisionId: string): Promise<DecisionWithRewards> {
    return this.#json<DecisionWithRewards>(
      "GET",
      this.#capsule(`decisions/${segment(decisionId, "decisionId")}`),
    );
  }

  /**
   * `GET .../decisions`: one page of the decision log, oldest first. Pass
   * the page's `next` as `after` for the next one.
   */
  async decisions(options: ListDecisionsOptions = {}): Promise<DecisionList> {
    const { limit, after, since, until } = options;
    return this.#json<DecisionList>("GET", this.#capsule("decisions"), {
      query: { limit, after, since, until },
    });
  }

  /** `GET .../audits`: the newest `limit` audit events (default 100), oldest first. */
  async audits(options: { limit?: number } = {}): Promise<AuditList> {
    return this.#json<AuditList>("GET", this.#capsule("audits"), {
      query: { limit: options.limit },
    });
  }

  // ── Off-policy evaluation ─────────────────────────────────────────────

  /**
   * `POST .../evaluate`: estimate what a policy would have earned on the
   * logged decisions. A failed gate shows in the report; it is not an error.
   */
  async evaluate(body: EvaluateRequest): Promise<OpeReport> {
    return this.#json<OpeReport>("POST", this.#capsule("evaluate"), { body });
  }

  /**
   * `POST .../promote`: apply a spec patch only if every gate passes.
   *
   * A failed gate is a result, `{ promoted: false, error, report }`, not
   * an exception. Other refusals throw `HttpError`, such as 409 when there
   * is nothing to evaluate or the spec changed during the evaluation.
   */
  async promote(body: PromoteRequest): Promise<PromoteResult> {
    const reply = await this.#send("POST", this.#capsule("promote"), { body });
    if (reply.status === 409 && isRefusal(reply.body)) return reply.body;
    return this.#result<PromoteResponse>(reply);
  }

  // ── Uploads (local evaluation) ────────────────────────────────────────

  /**
   * `POST .../decisions:batch`: decisions made in-process, at most 4096 per
   * call. The server verifies each by replaying it and stores a repeated
   * upload once, so the call is retried.
   */
  async uploadDecisions(decisions: UploadedDecision[]): Promise<DecisionUploadResponse> {
    return this.#json<DecisionUploadResponse>("POST", this.#capsule("decisions:batch"), {
      body: { decisions },
      idempotent: true,
    });
  }

  /**
   * `POST .../rewards:batch`: rewards applied in order, at most 4096 per
   * call; upload a decision before its rewards. Retried when every item
   * has an `idempotencyKey`.
   */
  async uploadRewards(rewards: RewardUploadItem[]): Promise<RewardUploadResponse> {
    return this.#json<RewardUploadResponse>("POST", this.#capsule("rewards:batch"), {
      body: { rewards },
      idempotent: rewards.every((r) => r.idempotencyKey !== undefined),
    });
  }

  // ── Tokens and infra ──────────────────────────────────────────────────

  /**
   * `POST /v1/admin/tokens` (operator key): issue a scoped token. The raw
   * token appears only in this answer; `ttlSeconds` omitted means no expiry.
   */
  async issueToken(scope: Scope, label?: string, ttlSeconds?: number): Promise<IssueTokenResponse> {
    const body: { scope: Scope; label?: string; ttlSeconds?: number } = { scope };
    if (label !== undefined) body.label = label;
    if (ttlSeconds !== undefined) body.ttlSeconds = ttlSeconds;
    return this.#json<IssueTokenResponse>("POST", "/v1/admin/tokens", { body });
  }

  /** `GET /v1/admin/tokens` (operator key): unexpired tokens, without their raw values. */
  async listTokens(): Promise<TokenList> {
    return this.#json<TokenList>("GET", "/v1/admin/tokens");
  }

  /** `DELETE /v1/admin/tokens/{hash}` (operator key). */
  async revokeToken(hash: string): Promise<Revoked> {
    return this.#json<Revoked>("DELETE", `/v1/admin/tokens/${segment(hash, "hash")}`);
  }

  /** `GET /v1/auth/whoami`: how this client's credential authenticates, and its scope. */
  async whoami(): Promise<WhoAmI> {
    return this.#json<WhoAmI>("GET", "/v1/auth/whoami");
  }

  /** `GET /health` (no credential needed). */
  async health(): Promise<Health> {
    return this.#json<Health>("GET", "/health", { auth: false });
  }

  // ── Plumbing ──────────────────────────────────────────────────────────

  #capsule(tail: string): string {
    return `${this.#capsulePath}/${tail}`;
  }

  async #json<T>(method: Method, path: string, call: Call = {}): Promise<T> {
    return this.#result<T>(await this.#send(method, path, call));
  }

  #result<T>(reply: Reply): T {
    if (reply.status < 200 || reply.status >= 300) {
      throw new HttpError({
        method: reply.method,
        path: reply.path,
        status: reply.status,
        body: reply.body,
        requestId: reply.headers.get("x-request-id") ?? undefined,
        retryAfterMs: retryAfterMs(reply.headers.get("retry-after")),
        message: this.#redact(httpErrorMessage(reply.method, reply.path, reply.status, reply.body)),
      });
    }
    if (!reply.json) {
      throw new SyntraError(`${reply.method} ${reply.path}: HTTP ${reply.status}: the answer is not JSON`);
    }
    return reply.body as T;
  }

  /** One request, with retries when it is safe to repeat. */
  async #send(method: Method, path: string, call: Call = {}): Promise<Reply> {
    const target = path + queryString(call.query);
    const headers: Record<string, string> = {};
    if (call.auth !== false && this.#token !== "") headers.authorization = `Bearer ${this.#token}`;
    let payload: string | undefined;
    if (call.body !== undefined) {
      // Serialized once: a retry sends the same bytes (decide replays
      // compare them).
      payload = JSON.stringify(call.body);
      headers["content-type"] = "application/json";
    }
    if (call.ifNoneMatch !== undefined) headers["if-none-match"] = call.ifNoneMatch;
    const retryable = method === "GET" || call.idempotent === true;
    for (let attempt = 0; ; attempt++) {
      const mayRetry = retryable && attempt < this.maxRetries;
      let reply: Reply;
      try {
        reply = await this.#attempt(method, target, headers, payload, call.raw === true);
      } catch (err) {
        if (!mayRetry || !(err instanceof TransportError)) throw err;
        await sleep(this.#backoff(attempt));
        continue;
      }
      if (mayRetry && (reply.status === 503 || reply.status === 429)) {
        const wait = retryAfterMs(reply.headers.get("retry-after")) ?? this.#backoff(attempt);
        if (wait <= this.maxRetryDelayMs) {
          await sleep(wait);
          continue;
        }
      }
      return reply;
    }
  }

  async #attempt(
    method: Method,
    target: string,
    headers: Record<string, string>,
    payload: string | undefined,
    raw: boolean,
  ): Promise<Reply> {
    const fetchImpl = this.#fetch; // called unbound: browsers refuse fetch with a foreign `this`
    const signal = AbortSignal.timeout(this.timeoutMs);
    let status: number;
    let responseHeaders: Headers;
    let text: string;
    try {
      // Redirects are not followed: a redirected POST would turn into a GET.
      const res = await fetchImpl(this.url + target, {
        method,
        headers,
        body: payload,
        signal,
        redirect: "manual",
      });
      status = res.status;
      responseHeaders = res.headers;
      text = await res.text();
    } catch (cause) {
      const timeoutMs = signal.aborted ? this.timeoutMs : undefined;
      throw new TransportError({
        method,
        path: target,
        timeoutMs,
        cause,
        message: this.#redact(transportErrorMessage(method, target, timeoutMs, cause)),
      });
    }
    const keepRaw = raw && status >= 200 && status < 300;
    const parsed = keepRaw ? { value: undefined, json: false } : parseBody(text);
    return { method, path: target, status, headers: responseHeaders, body: parsed.value, json: parsed.json, text };
  }

  #backoff(attempt: number): number {
    const ceiling = Math.min(this.maxRetryDelayMs, BACKOFF_BASE_MS * 2 ** attempt);
    return ceiling * (0.5 + Math.random() / 2);
  }

  #redact(message: string): string {
    const token = this.#token;
    return token.length >= MIN_REDACTED_TOKEN_LENGTH ? message.split(token).join("[token]") : message;
  }
}

function baseUrl(url: string): string {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new TypeError("url is not a valid URL");
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new TypeError(`url must be http or https (got ${parsed.protocol})`);
  }
  if (parsed.username !== "" || parsed.password !== "") {
    throw new TypeError("url must not carry credentials; use token");
  }
  if (parsed.search !== "" || parsed.hash !== "") {
    throw new TypeError("url must not have a query or fragment");
  }
  return parsed.origin + parsed.pathname.replace(/\/+$/, "");
}

/**
 * A path segment. `.` and `..` are refused: URL parsing would resolve them
 * and send the request to another route. `:` stays literal: it is legal in
 * a path, decision ids may contain it, and the server matches segments
 * without percent-decoding them.
 */
function segment(value: string, name: string): string {
  if (typeof value !== "string" || value === "" || value === "." || value === "..") {
    throw new TypeError(`${name} must be a non-empty string other than "." and ".."`);
  }
  return encodeURIComponent(value).replaceAll("%3A", ":");
}

function numberOption(value: number | undefined, fallback: number, name: string, min: number): number {
  if (value === undefined) return fallback;
  if (typeof value !== "number" || !Number.isFinite(value) || value < min) {
    throw new TypeError(`${name} must be a finite number >= ${min}`);
  }
  return value;
}

function queryString(query: Query | undefined): string {
  if (query === undefined) return "";
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined) params.set(key, String(value));
  }
  const text = params.toString();
  return text === "" ? "" : `?${text}`;
}

function parseBody(text: string): { value: unknown; json: boolean } {
  if (text === "") return { value: undefined, json: true };
  try {
    return { value: JSON.parse(text), json: true };
  } catch {
    return { value: text, json: false };
  }
}

/** `Retry-After` (seconds or an HTTP date) in milliseconds. */
function retryAfterMs(value: string | null): number | undefined {
  if (value === null) return undefined;
  const text = value.trim();
  if (/^\d+(\.\d+)?$/.test(text)) return Math.round(Number(text) * 1000);
  // An HTTP date names its day and month. V8's Date.parse also takes bare
  // numbers such as "-1" as dates in 2001.
  if (!/[a-z]/i.test(text)) return undefined;
  const at = Date.parse(text);
  return Number.isNaN(at) ? undefined : Math.max(0, at - Date.now());
}

function isRefusal(body: unknown): body is PromoteRefusal {
  if (body === null || typeof body !== "object") return false;
  const b = body as { promoted?: unknown; report?: unknown };
  return b.promoted === false && b.report !== null && typeof b.report === "object";
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
