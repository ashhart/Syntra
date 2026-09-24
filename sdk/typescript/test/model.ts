/**
 * Published models for tests that do not start the server, built the way
 * the server builds them: a learner snapshot (src/decision/learner.rs), a
 * decide section as serde_json writes it (keys sorted, compact, integral
 * floats with `.0`), and the model tag over both (src/decision/tag.rs).
 *
 * These are independent of the Rust code: if they disagreed with it, the
 * decision core would refuse the models.
 */

import { createHash } from "node:crypto";

/** One trained hash slot: normalized weight, scale, normalized accumulator. */
export interface Slot {
  slot: number;
  u: number;
  s: number;
  a: number;
}

export interface SnapshotOptions {
  bits?: number;
  updates?: bigint;
  slots?: Slot[];
}

/** A learner snapshot: header, slots in order, SHA-256 of both. */
export function snapshot(options: SnapshotOptions = {}): Uint8Array {
  const { bits = 10, updates = 0n, slots = [] } = options;
  const body = new Uint8Array(52 + 16 * slots.length);
  const view = new DataView(body.buffer);
  body.set(new TextEncoder().encode("SYNTRALM"), 0);
  view.setUint16(8, 1, true); // format version
  body[10] = bits;
  body[11] = 0;
  view.setFloat64(12, 0.5, true); // learning rate
  view.setBigUint64(20, updates, true);
  const trained = updates > 0n ? Number(updates) : 0;
  view.setFloat64(28, trained, true); // total weight
  view.setFloat64(36, trained, true); // norm sum
  view.setBigUint64(44, BigInt(slots.length), true);
  slots.forEach((s, k) => {
    const at = 52 + 16 * k;
    view.setUint32(at, s.slot, true);
    view.setFloat32(at + 4, s.u, true);
    view.setFloat32(at + 8, s.s, true);
    view.setFloat32(at + 12, s.a, true);
  });
  const checksum = createHash("sha256").update(body).digest();
  const out = new Uint8Array(body.length + 32);
  out.set(body, 0);
  out.set(checksum, body.length);
  return out;
}

export interface DecideOptions {
  /** serde_json text of the action list. */
  actions?: string;
  bits?: number;
  mode?: "learner" | "baselineExplore" | "frozen";
  rewards?: "first" | "sum";
  /** A u64. */
  seed?: bigint;
  version?: number;
}

export const ACTIONS =
  '[{"features":{"cost":0.1},"id":"a"},{"features":{"cost":0.4,"tier":"pro"},"id":"b"},{"id":"c"}]';

/** A decide section exactly as the server serializes it. */
export function decideSection(options: DecideOptions = {}): string {
  const { actions = ACTIONS, bits = 10, mode = "learner", rewards = "first", seed, version = 1 } = options;
  const exploration = '{"epsilon":0.05,"floor":0.05,"gammaExponent":0.5,"gammaScale":10.0,"kind":"squarecb"}';
  const seedField = seed === undefined ? "" : `"seed":${seed},`;
  return (
    `{"actions":${actions},"baselineEpsilon":0.1,"bits":${bits},"exploration":${exploration},` +
    `"mode":"${mode}","rewards":"${rewards}",${seedField}"version":${version}}`
  );
}

/** First 16 hex digits of SHA-256(decide text, 0, snapshot checksum). */
export function modelTag(decide: string, snap: Uint8Array): string {
  return createHash("sha256")
    .update(decide)
    .update(new Uint8Array([0]))
    .update(snap.subarray(snap.length - 32))
    .digest("hex")
    .slice(0, 16);
}

export interface Published {
  /** The response text of `GET .../model?snapshot=true`. */
  text: string;
  tag: string;
}

/** A model endpoint answer; `tag` overrides the computed tag (to test refusal). */
export function published(decide: string, snap: Uint8Array, tag?: string): Published {
  const modelTagValue = tag ?? modelTag(decide, snap);
  const b64 = Buffer.from(snap).toString("base64");
  const text =
    `{"decide":${decide},"modelTag":"${modelTagValue}","modelVersion":0,"programSha256":null,` +
    `"rewardWatermark":0,"snapshot":"${b64}","snapshotBytes":${snap.length}}`;
  return { text, tag: modelTagValue };
}

const MASK = (1n << 64n) - 1n;

/** Vigna's splitmix64: the first output from `state`. */
export function splitmix64(state: bigint): bigint {
  let z = (state + 0x9e3779b97f4a7c15n) & MASK;
  z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & MASK;
  z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & MASK;
  return z ^ (z >> 31n);
}

/** The seed of draw `n` of a capsule with fixed seed `base` (src/client.rs). */
export function fixedSeedReference(base: bigint, n: bigint): bigint {
  return splitmix64(base ^ ((n * 0x9e3779b97f4a7c15n) & MASK));
}
