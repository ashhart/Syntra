/**
 * Request and response types of the Syntra v2 HTTP API, after the schemas
 * in `docs/openapi.yaml`. Field names are the wire names.
 */

// ── Shared ──────────────────────────────────────────────────────────────

/** Serving mode of a capsule. */
export type Mode = "learner" | "baselineExplore" | "frozen";

/** `first`: one reward per decision. `sum`: every reward counts. */
export type RewardAggregation = "first" | "sum";

/** A request context: any JSON object. Nested objects give dotted feature names. */
export type Context = Record<string, unknown>;

/** One action: an id, unique within its list, and optional features. */
export interface Action {
  id: string;
  /** Action features (a JSON object), flattened like the context. */
  features?: Record<string, unknown>;
}

export interface RankedAction {
  id: string;
  probability: number;
}

// ── Spec ────────────────────────────────────────────────────────────────

export interface RewardSpec {
  /** `[lo, hi]`; rewards normalize to `(r - lo) / (hi - lo)`, clamped to [0, 1]. */
  range: [number, number];
  /** Reward given to a decision still unrewarded `waitSeconds` after it was made. */
  default?: number;
  waitSeconds: number;
}

export interface ExplorationSpec {
  kind: "squarecb" | "epsilonGreedy";
  gammaScale: number;
  gammaExponent: number;
  /** Exploration rate of `epsilonGreedy`. */
  epsilon: number;
  /** Every eligible action keeps probability of at least `floor / K`. */
  floor: number;
}

export interface LearnerSpec {
  /** The model has `2^bits` hashed weight slots. */
  bits: number;
  learningRate: number;
  importance: "none" | "mtr";
  maxImportanceWeight: number;
}

/** A capsule's spec as the server returns it, every default filled in. */
export interface DecisionSpec {
  actions: Action[];
  reward: RewardSpec;
  exploration: ExplorationSpec;
  learner: LearnerSpec;
  mode: Mode;
  baselineEpsilon: number;
  /** Fixed base seed (a u64; JavaScript numbers are exact only up to 2^53). */
  seed?: number;
  snapshotEvery: number;
  rewards: RewardAggregation;
}

/**
 * An RFC 7396 merge patch of `T`: every field optional, `null` restores a
 * field's default, and arrays (such as `actions`) are replaced whole.
 */
export type MergePatch<T> = {
  [K in keyof T]?: (T[K] extends readonly unknown[] ? T[K] : T[K] extends object ? MergePatch<T[K]> : T[K]) | null;
};

/** A `PUT .../spec` body, or a candidate spec for `evaluate` and `promote`. */
export type SpecPatch = MergePatch<DecisionSpec>;

// ── Decide and reward ───────────────────────────────────────────────────

/** Options of `SyntraClient.decide`. */
export interface DecideOptions {
  /** Actions for this request only, replacing the spec's. */
  actions?: Action[];
  /** Action ids removed from the eligible set. */
  exclude?: readonly string[] | ReadonlySet<string>;
  /** The incumbent action; required in `baselineExplore` mode. */
  baseline?: string;
  /**
   * Becomes the decision id (1-128 characters from `A-Z a-z 0-9 _ . : -`).
   * Sending the same request again returns the stored decision, so the
   * client retries it; a different request with the same id is a 409.
   */
  eventId?: string;
  /** Answer only after the decision is committed to the event log. */
  durable?: boolean;
}

/** `POST .../decide` body. */
export interface DecideRequest {
  context?: Context | null;
  actions?: Action[];
  excludedActions?: string[];
  baselineAction?: string;
  eventId?: string;
  durable?: boolean;
}

export interface DecideResponse {
  /** The request's `eventId`, or a generated `dec_...` id that sorts by time. */
  decisionId: string;
  action: string;
  /** Index of the chosen action in the action list (request or spec). */
  actionIndex: number;
  /** Probability the chosen action was sampled with. */
  probability: number;
  /** Eligible actions, most probable first. */
  ranking: RankedAction[];
  mode: Mode;
  /** Learner updates applied when the decision was made. */
  modelVersion: number;
  /** The feature program's `reason`, when it published one. */
  reason?: string;
  /** True when an `eventId` retry returned the stored decision. */
  replayed?: boolean;
}

/** Options of `SyntraClient.reward`. */
export interface RewardOptions {
  /**
   * Under `rewards: "sum"`, deduplicates retries (1-256 bytes, no NUL);
   * with it the client retries the request. Under `"first"` the server
   * keys every reward by its decision id.
   */
  idempotencyKey?: string;
  /** Any JSON stored with the reward. The key `learned` is reserved. */
  detail?: unknown;
  /** Answer only after the reward is committed to the event log. */
  durable?: boolean;
}

/** `POST .../reward` body. */
export interface RewardRequest {
  decisionId: string;
  reward: number;
  idempotencyKey?: string;
  detail?: unknown;
  durable?: boolean;
}

export interface RewardResponse {
  ok: true;
  /** False for a duplicate, and for a reward held for a deferred decision. */
  applied: boolean;
  /** Present when applied; false in `frozen` mode. */
  learned?: boolean;
  /** Present (true) for a duplicate. */
  duplicate?: boolean;
  /** Present (true) when the decision awaits Personalizer activation. */
  held?: boolean;
  modelVersion: number;
}

// ── Logs and model ──────────────────────────────────────────────────────

/** A logged decision, as `GET .../decisions` serves it. */
export interface Decision {
  decisionId: string;
  /** Decision time, ms since the epoch. */
  tsMs: number;
  modelVersion: number;
  mode: Mode;
  /** The context as featurized. */
  context: Context;
  /** Features the feature program published; null without a program. */
  derived: Record<string, unknown> | null;
  /** The action list decided over. */
  actions: Action[];
  /** Indices into `actions` of the eligible actions, ascending. */
  eligible: number[];
  /** Probability of each eligible action, aligned with `eligible`. */
  pmf: number[] | null;
  chosenIndex: number;
  action: string;
  probability: number | null;
  /** Sampling seed, a u64 in decimal. */
  seed: string;
  reason: string | null;
  /** SHA-256 of the request body. */
  requestSha256: string;
  programSha256: string | null;
  /** Only on `GET .../decisions/{id}`. */
  rewards?: RewardEvent[];
}

/** `GET .../decisions/{id}`: the decision and every reward recorded for it. */
export interface DecisionWithRewards extends Decision {
  rewards: RewardEvent[];
}

export interface RewardEvent {
  /** The model applies rewards in `seq` order. */
  seq: number;
  tsMs: number;
  reward: number;
  rewardNormalized: number;
  idempotencyKey: string;
  /** The stored detail; `learned` is false for rewards recorded while frozen. */
  detail: Record<string, unknown> | null;
}

/** Options of `SyntraClient.decisions`. */
export interface ListDecisionsOptions {
  /** Page size, clamped to [1, 1000]; default 100. */
  limit?: number;
  /** Cursor: the previous page's `next`. */
  after?: string;
  /** Earliest decision time, ms since the epoch (inclusive). */
  since?: number;
  /** Latest decision time, ms since the epoch (exclusive). */
  until?: number;
}

export interface DecisionList {
  /** Oldest first. */
  decisions: Decision[];
  /** Cursor for the next page, or null when this page is not full. */
  next: string | null;
}

/** `GET .../model`: the live spec and model version. */
export interface Model {
  spec: DecisionSpec;
  /** Learner updates applied (of the published model, with `snapshot`). */
  modelVersion: number;
  /** Sequence number of the last reward covered by the latest snapshot or startup replay. */
  rewardWatermark: number;
  programSha256: string | null;
}

/** `GET .../model?snapshot=true`: the model published for local evaluation. */
export interface PublishedModel extends Model {
  decide: DecideSection;
  /** Names this published model; uploaded decisions carry it. Also the ETag. */
  modelTag: string;
  snapshotBytes: number;
  /** The learner state, base64. */
  snapshot: string;
}

/** The part of the spec a local-evaluation SDK decides with. */
export interface DecideSection {
  /** SDKs refuse a version newer than they understand. */
  version: number;
  actions: Action[];
  exploration: ExplorationSpec;
  mode: Mode;
  baselineEpsilon: number;
  /** The learner's hash bits. */
  bits: number;
  /** Present when the spec fixes a seed. */
  seed?: number;
  rewards: RewardAggregation;
}

export interface AuditEvent {
  seq: number;
  /** Event time, ms since the epoch. */
  tsMs: number;
  /** `capsule_created`, `spec_updated`, `spec_promoted`, `promotion_refused`, ... */
  event: string;
  detail: Record<string, unknown>;
}

export interface AuditList {
  /** The newest events, oldest first. */
  audits: AuditEvent[];
}

// ── Off-policy evaluation ───────────────────────────────────────────────

/** `POST .../evaluate` body: exactly one of `policy` and `spec`. */
export interface EvaluateRequest {
  /** `logged`, `greedy` (cross-fitted) or `constant:<id>`. */
  policy?: "logged" | "greedy" | `constant:${string}`;
  /** A candidate spec, evaluated as the greedy policy of a reward model trained under it. */
  spec?: SpecPatch;
  /** Checks such as `lift.dr.lower >= 0`, `ess >= 200` or `n >= 1000`. */
  gates?: string[];
  /** Earliest decision time to evaluate, ms since the epoch. */
  since?: number;
  /** Latest decision time to evaluate, ms since the epoch. */
  until?: number;
  /** Cross-fitting folds of the reward model (2-100, default 5). */
  folds?: number;
  /** Bootstrap resamples: 0 for normal intervals only, otherwise 100-100000 (default 1000). */
  bootstrap?: number;
  /** Seed of the bootstrap resampling (default 7). */
  seed?: number;
  /** Importance weights are clipped to at most this (default 100). */
  wMax?: number;
  /** Raw reward range for the reward model; default the observed range. */
  rewardRange?: [number, number];
}

/** `POST .../promote` body: a candidate spec patch and at least one gate. */
export interface PromoteRequest extends Omit<EvaluateRequest, "policy" | "spec" | "gates"> {
  spec: SpecPatch;
  gates: string[];
}

/** A value's standard error and 95% intervals; null where undefined. */
export interface OpeInterval {
  se: number | null;
  lower: number | null;
  upper: number | null;
  normalLower: number | null;
  normalUpper: number | null;
}

/** An estimator's value for the evaluated policy. */
export interface OpeEstimate extends OpeInterval {
  estimate: number | null;
}

/** A mean: the logged policy's value, or a lift over it. */
export interface OpeMean extends OpeInterval {
  mean: number | null;
}

export interface OpeByEstimator<T> {
  dm: T;
  ips: T;
  snips: T;
  dr: T;
}

export interface GateResult {
  check: string;
  left: number | null;
  /** The right-hand value, offset included. */
  right: number | null;
  pass: boolean;
}

/** The evaluation report. Values are in raw reward units. */
export interface OpeReport {
  /** The evaluated policy's label. */
  policy: string;
  data: {
    /** Decisions read. */
    rows: number;
    rowsWithoutPmf: number;
    rowsWithoutReward: number;
    rewardAggregation: RewardAggregation;
    aggregatedRewards: number;
    folds: number;
    rewardRange: [number, number];
    rewardRangeSource: "given" | "observed";
    rewardsOutsideRange: number;
  };
  /** The logged policy's on-policy mean. */
  logged: OpeMean;
  estimators: OpeByEstimator<OpeEstimate>;
  /** Each estimator minus the logged mean, paired on the same rows. */
  lift: OpeByEstimator<OpeMean>;
  diagnostics: {
    /** Rows evaluated (rows with a reward). */
    n: number;
    ess: number | null;
    coverage: number | null;
    clipRate: number | null;
    clippedRows: number;
    maxWeight: number | null;
    meanWeight: number | null;
    fallbackRows: number;
    fallbackRate: number | null;
    unsupportedMass: number | null;
  };
  settings: {
    bootstrap: number;
    seed: number;
    wMax: number | null;
    confidence: number;
    interval: "bootstrapPercentile" | "normal";
    rewardModel: LearnerSpec;
  };
  warnings: string[];
  gates: GateResult[];
  /** True when every gate passes, including when there are none. */
  gatesPassed: boolean;
  verdict: string;
}

/** Every gate passed and the patch was applied. */
export interface PromoteResponse {
  promoted: true;
  /** The spec after the change. */
  spec: DecisionSpec;
  report: OpeReport;
}

/** A gate failed; nothing changed (the server answered 409). */
export interface PromoteRefusal {
  promoted: false;
  error: string;
  report: OpeReport;
}

export type PromoteResult = PromoteResponse | PromoteRefusal;

// ── Uploads (local evaluation) ──────────────────────────────────────────

/** The decide input an uploaded decision was made from. */
export interface UploadInput {
  context?: Context | null;
  actions?: Action[];
  excludedActions?: string[];
  baselineAction?: string;
}

/** A decision made in-process, for `POST .../decisions:batch`. */
export interface UploadedDecision {
  /** 1-128 characters from `A-Z a-z 0-9 _ . : -`. */
  decisionId: string;
  /** Decision time, ms since the epoch: at most 7 days old and 5 minutes ahead. */
  tsMs: number;
  /** The `modelTag` of the published model the decision was made with. */
  modelTag: string;
  /** Optional; must match the tag's version. */
  modelVersion?: number;
  /** The sampling seed, a u64. Send a decimal string: numbers above 2^53 lose precision. */
  seed: string | number;
  input: UploadInput;
  chosenIndex: number;
  probability: number;
  pmf: number[];
  eligible: number[];
}

export interface UploadRejection {
  /** Position of the item in the upload. */
  index: number;
  /** The item's `decisionId`, or empty if it had none. */
  decisionId: string;
  error: string;
  /** True when the server could not take the item right now; upload it again later. */
  retryable: boolean;
}

export interface DecisionUploadResponse {
  /** Items stored, including retried uploads of stored decisions. */
  accepted: number;
  /** Accepted items that were already stored. */
  duplicates: number;
  rejected: UploadRejection[];
}

/** A reward for `POST .../rewards:batch`. */
export interface RewardUploadItem {
  decisionId: string;
  reward: number;
  idempotencyKey?: string;
  detail?: unknown;
}

export interface RewardUploadFailure {
  ok: false;
  /** Absent when the item could not be parsed. */
  decisionId?: string;
  /** The status `POST .../reward` would have answered (for example 404, or 503 to retry). */
  status?: number;
  error: string;
}

export interface RewardUploadResponse {
  /** One result per item, in order. */
  results: Array<RewardResponse | RewardUploadFailure>;
}

// ── Auth and infra ──────────────────────────────────────────────────────

/**
 * `admin`: every route. `tenant_admin`: every route of one tenant.
 * `read`: decide, reward, uploads and the read routes of one capsule.
 */
export type Scope =
  | { kind: "admin" }
  | { kind: "tenant_admin"; tenant: string }
  | { kind: "read"; tenant: string; job: string; capsule: string };

export interface WhoAmI {
  ok: boolean;
  /** `legacy_admin` is the operator admin key. */
  kind: "dev_mode" | "legacy_admin" | "scoped_token";
  /** `operator`, the token hash, or null in dev mode. */
  principalId: string | null;
  scope: Scope;
}

export interface IssueTokenResponse {
  /** The raw token. The server keeps only its hash, so this is the only copy. */
  token: string;
  /** SHA-256 of the token (hex); use it to revoke. */
  hash: string;
  scope: Scope;
  /** Unix time in seconds, or null for no expiry. */
  expiresAt: number | null;
}

export interface TokenRecord {
  hash: string;
  scope: Scope;
  /** Unix time in seconds. */
  createdAt: number;
  expiresAt: number | null;
  /** Last successful authentication, recorded at most once a minute. */
  lastUsedAt: number | null;
  label: string;
}

export interface TokenList {
  /** Unexpired tokens; raw tokens are never returned. */
  tokens: TokenRecord[];
}

export interface Revoked {
  ok: boolean;
  revoked: boolean;
}

export interface Health {
  ok: boolean;
  service: string;
}
