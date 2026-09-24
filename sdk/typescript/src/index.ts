/**
 * @syntra/client: TypeScript client for the Syntra decision service (v2 API).
 *
 * ```ts
 * import { SyntraClient } from "@syntra/client";
 *
 * const router = new SyntraClient({
 *   url: "http://127.0.0.1:8787",
 *   token: process.env.SYNTRA_TOKEN!,
 *   tenant: "acme",
 *   job: "prod",
 *   capsule: "router",
 * });
 * const d = await router.decide({ task: "code", promptTokens: 812 });
 * // ... act on d.action, then report how it went:
 * await router.reward(d.decisionId, 0.8);
 * ```
 *
 * `LocalDecider` decides in-process instead, on the Rust decision core
 * compiled to WebAssembly, and uploads its decisions for the server to
 * verify and learn from:
 *
 * ```ts
 * import { LocalDecider } from "@syntra/client";
 *
 * const router = await LocalDecider.connect({ url, token, tenant: "acme", job: "prod", capsule: "router" });
 * const d = router.decide({ task: "code", promptTokens: 812 }); // synchronous, no network
 * router.reward(d.decisionId, 0.8);
 * ```
 */

export { SyntraClient } from "./client.ts";
export type { SyntraClientOptions } from "./client.ts";

export { HttpError, SyntraError, TransportError } from "./errors.ts";
export type { HttpErrorInit, TransportErrorInit } from "./errors.ts";

export { LocalDecider, MAX_UPLOAD_ITEMS } from "./local.ts";
export type { FlushReport, LocalDecideOptions, LocalDeciderOptions, LocalDecision } from "./local.ts";
export type { WasmSource } from "./core.ts";

export type {
  Action,
  AuditEvent,
  AuditList,
  Context,
  DecideOptions,
  DecideRequest,
  DecideResponse,
  DecideSection,
  Decision,
  DecisionList,
  DecisionSpec,
  DecisionUploadResponse,
  DecisionWithRewards,
  EvaluateRequest,
  ExplorationSpec,
  GateResult,
  Health,
  IssueTokenResponse,
  LearnerSpec,
  ListDecisionsOptions,
  MergePatch,
  Mode,
  Model,
  OpeByEstimator,
  OpeEstimate,
  OpeInterval,
  OpeMean,
  OpeReport,
  PromoteRefusal,
  PromoteRequest,
  PromoteResponse,
  PromoteResult,
  PublishedModel,
  RankedAction,
  Revoked,
  RewardAggregation,
  RewardEvent,
  RewardOptions,
  RewardRequest,
  RewardResponse,
  RewardSpec,
  RewardUploadFailure,
  RewardUploadItem,
  RewardUploadResponse,
  Scope,
  SpecPatch,
  TokenList,
  TokenRecord,
  UploadInput,
  UploadRejection,
  UploadedDecision,
  WhoAmI,
} from "./types.ts";
