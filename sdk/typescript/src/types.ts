/**
 * Wire types for the Syntra `/v1` HTTP API.
 *
 * Field names and optionality mirror the server implementations in
 * `src/server/routes.rs`, `src/server/decide.rs`, `src/server/feedback.rs`,
 * and `src/server/inspect.rs`. Responses are forward-compatible: unknown
 * fields are preserved via index signatures where noted.
 */

/** Extractable fetch surface so callers can inject a stub in tests. */
export interface SyntraFetchResponse {
  readonly status: number;
  readonly headers: { get(name: string): string | null };
  text(): Promise<string>;
}

/** Minimal `fetch` shape the client needs; `globalThis.fetch` satisfies it. */
export type FetchLike = (url: string, init: RequestInit) => Promise<SyntraFetchResponse>;

// ── Health ─────────────────────────────────────────────────────────────────

export interface HealthResponse {
  ok: true;
  service: string;
}

// ── Warmup / lifecycle ─────────────────────────────────────────────────────

export type WarmupStateName = "warmup" | "active" | "frozen";

/** `warmup` block shared by `/decide` and `/feedback` responses. */
export interface WarmupInfo {
  state: WarmupStateName;
  /** Present while `state === "warmup"`. */
  collected?: number;
  /** Present while `state === "warmup"`. */
  target?: number;
  /** Present while `state` is `"active"` or `"frozen"` (decide/feedback). */
  algorithm?: string;
  /** Present while `state === "active"` (report). */
  characterization?: string;
  /** Present while `state === "frozen"`. */
  reason?: string;
}

// ── Decide ─────────────────────────────────────────────────────────────────

/**
 * Decide request body. Discrete-context capsules send `contextKey`;
 * feature-context capsules send `features`. `input` is logged and made
 * available to the graph but does not affect option selection.
 */
export interface DecideContext {
  contextKey?: string;
  features?: Record<string, number>;
  input?: Record<string, unknown>;
}

export interface DecideOptions {
  /** In-band weight mutation (`?learn=true`). Silently forced off for `read`-scoped tokens. */
  learn?: boolean;
}

/** One entry of the `decisions[]` array; extra server fields are preserved. */
export interface DecisionEntry {
  node_id: number;
  chosen_option: number;
  confidence?: number;
  objective?: string;
  weights?: number[];
  activations?: number;
  /** Meta-bandit candidate that served this choice (e.g. `"LinUcb"`, `"Thompson"`). */
  candidateId?: string;
  chosenAction?: number;
  contextKey?: string;
  contextWeights?: number[];
  featureVector?: number[];
  algorithmChose?: number;
  predictionSet?: number[];
  setWidth?: number;
  posteriorMeans?: number[];
  conformalBandRadius?: number;
  coverage?: number;
  sharedStateScores?: Array<{ option: string; score: number }>;
  published?: Record<string, unknown>;
  [key: string]: unknown;
}

/** `confidence` block on decide responses (refusal bookkeeping). */
export interface ConfidenceBlock {
  oodScore: number;
  intervalWidth: number | null;
  coverage: number;
  refused: boolean;
  refusalReason: string | null;
}

/**
 * `/decide` response. When `refused` is true, `decisions` is empty and the
 * caller should use its own fallback; `algorithm`/`learned`/`result`/`stdout`
 * are only present on non-refused responses.
 */
export interface DecisionResult {
  ok: true;
  tenant: string;
  job: string;
  capsule: string;
  decisionId: string;
  contextKey: string;
  algorithm?: string;
  learned?: boolean;
  warmup: WarmupInfo;
  decisions: DecisionEntry[];
  result?: unknown;
  stdout?: string[];
  oodScore: number;
  refused: boolean;
  confidence: ConfidenceBlock;
}

// ── Feedback ───────────────────────────────────────────────────────────────

/**
 * Feedback request body. Preferred form pairs `decisionId` with a scalar
 * `reward` (or `components` reduced by the installed reward spec). The
 * explicit `strategyId`/`option`/`contextKey` form bypasses the decision-log
 * lookup and skips refusal accounting — use only when necessary.
 */
export interface FeedbackEvent {
  decisionId?: string;
  reward?: number;
  components?: Record<string, number>;
  outcome?: string;
  strategyId?: string | number;
  option?: number;
  contextKey?: string;
  /** Target entry of `decisions[]` on multi-decision capsules; defaults to 0. */
  decisionIndex?: number;
}

export interface FeedbackResult {
  ok: true;
  tenant: string;
  job: string;
  capsule: string;
  nodeId: number;
  option: number;
  reward: number;
  before: number[];
  after: number[];
  contextKey: string;
  warmupTransitioned: boolean;
  changeDetected: boolean;
  contextChangeDetected: boolean;
  warmup: WarmupInfo;
}

// ── Capsule install ────────────────────────────────────────────────────────

/** `POST .../install` response (raw `.lyc` bytes body; SHA-256 of the upload). */
export interface InstallResponse {
  ok: true;
  tenant: string;
  job: string;
  capsule: string;
  hash: string;
}

// ── Inspect: report / contexts / memory / decisions ────────────────────────

export interface ReportOptionStats {
  option: number;
  tries: number;
  correct: number;
  avg_ms: number;
  /** Effective weight (meta-bandit overlay when Active). */
  weight: number;
  /** Raw weight stored in the graph binary. */
  graphWeight: number;
}

export interface ReportStrategy {
  node_id: number;
  activations: number;
  n_options: number;
  options: ReportOptionStats[];
  graphWeights: number[];
  /** Candidate id whose bucket produced the live weights, or null (on-graph). */
  liveSource: string | null;
  [key: string]: unknown;
}

export interface MetaBanditCandidate {
  id: string;
  trials: number;
  meanReward: number;
  cumulativeReward: number;
}

export interface MetaBanditSummary {
  totalRounds: number;
  currentLeader: string | null;
  candidates: MetaBanditCandidate[];
}

/** `GET .../report` payload. `metaBandit` is keyed by stringified node id. */
export interface ReportPayload {
  tenant: string;
  job: string;
  capsule: string;
  /** SHA-256 of the installed graph binary. */
  hash: string;
  strategies: ReportStrategy[];
  warmup: WarmupInfo;
  algorithm: string | null;
  metaBandit: Record<string, MetaBanditSummary>;
}

export interface ContextRow {
  nodeId: number;
  contextKey: string;
  totalTries: number;
  weights: number[];
  updatedAt: number;
}

export interface ContextsResponse {
  tenant: string;
  job: string;
  capsule: string;
  contexts: ContextRow[];
}

/**
 * `GET .../memory` full sidecar (schema v7). The shape varies with capsule
 * history; treated as an opaque object, per the OpenAPI contract.
 */
export type MemorySidecar = Record<string, unknown>;

/** One line of the NDJSON `GET .../decisions` log. */
export interface DecisionLogEntry {
  id: string;
  tenant?: string;
  job?: string;
  capsule?: string;
  contextKey?: string;
  algorithm?: string;
  inputSha256?: string;
  graphHash?: string;
  learned?: boolean;
  decisions?: unknown[];
  refused?: boolean;
  refusalReason?: string | null;
  oodScore?: number;
  intervalWidth?: number | null;
  [key: string]: unknown;
}

// ── Admin: tokens / jobs ───────────────────────────────────────────────────

/** Scope of a scoped token, as serialized by `auth_tokens::Scope`. */
export type TokenScope =
  | { kind: "admin" }
  | { kind: "tenant_admin"; tenant: string }
  | { kind: "read"; tenant: string; job: string; capsule: string };

export interface CreateTokenRequest {
  scope: TokenScope;
  label?: string;
  ttlSeconds?: number;
}

/** Raw `token` value is shown exactly once, in this response only. */
export interface CreateTokenResponse {
  token: string;
  /** SHA-256 hex of the raw token; pass to revoke. */
  hash: string;
  scope: TokenScope;
  expiresAt: number | null;
}

export interface CreateJobRequest {
  id: string;
  name?: string;
  description?: string;
  metadata?: Record<string, unknown>;
}

export interface CreateJobResponse {
  ok: true;
  tenant: string;
  job: Record<string, unknown>;
}
