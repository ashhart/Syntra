/**
 * The seam for local evaluation.
 *
 * A local decider keeps a copy of a capsule's published model and decides
 * in-process with the same code the server runs, so a decision costs
 * microseconds and needs no network. Decisions and rewards upload in
 * batches; the server replays every uploaded decision (same model, input
 * and seed) and stores it only if the eligible set, the chosen action and
 * every probability match.
 *
 * The implementation will wrap a WebAssembly build of the Rust decision
 * core. Uploaded decisions must replay exactly on the server, so they are
 * not reimplemented in TypeScript. This module only declares the interface;
 * `SyntraClient` has the HTTP calls an implementation needs:
 * `model({ snapshot: true, ifNoneMatch })`, `uploadDecisions` and
 * `uploadRewards`. The README describes the protocol.
 */

import type { Action, Context, RankedAction, RewardOptions } from "./types.ts";

/** Most items one `uploadDecisions` or `uploadRewards` call may carry. */
export const MAX_UPLOAD_ITEMS = 4096;

export interface LocalDecideOptions {
  /** Actions for this call only, replacing the spec's. */
  actions?: Action[];
  /** Action ids removed from the eligible set. */
  exclude?: readonly string[] | ReadonlySet<string>;
  /** The incumbent action; required in `baselineExplore` mode. */
  baseline?: string;
}

/** A decision made in-process. */
export interface LocalDecision {
  decisionId: string;
  action: string;
  actionIndex: number;
  probability: number;
  /** Eligible actions, most probable first. */
  ranking: RankedAction[];
  modelVersion: number;
}

/** What one `flush` uploaded. */
export interface FlushReport {
  /** Decisions the server verified and stored, including retried uploads it already had. */
  decisionsAccepted: number;
  /** Decisions refused for good (did not replay, retired model, ...). */
  decisionsRejected: number;
  rewardsApplied: number;
  /** Rewards refused for good (unknown decision, invalid, ...). */
  rewardsFailed: number;
  /** Events the server could not take right now, queued again for the next flush. */
  requeued: number;
  /** The first few refusal messages. */
  errors: string[];
}

/** Decide in-process; learn on the server. */
export interface LocalDecider {
  /** Choose an action with the synced model. Synchronous: no network. */
  decide(context?: Context, options?: LocalDecideOptions): LocalDecision;
  /** Queue the outcome of a decision made here or on the server. */
  reward(decisionId: string, value: number, options?: Omit<RewardOptions, "durable">): void;
  /** Upload queued decisions, then their rewards. */
  flush(): Promise<FlushReport>;
  /** Fetch a newer published model; true if the model changed. */
  sync(): Promise<boolean>;
  /** Stop background work and upload what is still queued. */
  close(): Promise<FlushReport>;
  /** Version of the model decisions are made with. */
  readonly modelVersion: number;
  /** The server's tag for that model. */
  readonly modelTag: string;
  /** Decisions and rewards waiting to be uploaded. */
  readonly pending: number;
}
