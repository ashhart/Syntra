/**
 * @syntra/client — official TypeScript SDK for the Syntra decision appliance.
 *
 * @example
 * ```ts
 * import { SyntraClient } from "@syntra/client";
 *
 * const syntra = new SyntraClient({
 *   baseUrl: "http://127.0.0.1:8787",
 *   token: process.env.SYNTRA_TOKEN ?? "",
 * });
 *
 * const decision = await syntra.decide("acme", "llm-routing", "model-router", {
 *   contextKey: "support-low-cost",
 * });
 * if (!decision.refused) {
 *   const option = decision.decisions[0]?.chosen_option;
 *   // ...route accordingly, then report the outcome later:
 *   await syntra.feedback("acme", "llm-routing", "model-router", {
 *     decisionId: decision.decisionId,
 *     reward: 0.85,
 *   });
 * }
 * ```
 */

export { SyntraClient } from "./client.js";
export type { SyntraClientOptions } from "./client.js";

export {
  SyntraApiError,
  SyntraNetworkError,
  AuthError,
  ForbiddenError,
  NotFoundError,
  RateLimitedError,
  maskToken,
} from "./errors.js";
export type { SyntraApiErrorInit } from "./errors.js";

export type {
  ConfidenceBlock,
  ContextRow,
  ContextsResponse,
  CreateJobRequest,
  CreateJobResponse,
  CreateTokenRequest,
  CreateTokenResponse,
  DecideContext,
  DecideOptions,
  DecisionEntry,
  DecisionLogEntry,
  DecisionResult,
  FeedbackEvent,
  FeedbackResult,
  FetchLike,
  HealthResponse,
  InstallResponse,
  MemorySidecar,
  MetaBanditCandidate,
  MetaBanditSummary,
  ReportOptionStats,
  ReportPayload,
  ReportStrategy,
  SyntraFetchResponse,
  TokenScope,
  WarmupInfo,
  WarmupStateName,
} from "./types.js";
