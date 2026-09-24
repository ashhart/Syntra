/**
 * End-to-end: `LocalDecider` decides in-process on the WebAssembly build of
 * the decision core, and a real `syntra serve` (see server.ts) replays
 * every uploaded decision with its native build. The server stores a
 * decision only if the eligible set, the chosen action and every
 * probability match, so these tests fail on any divergence between the two
 * builds.
 */

import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";

import { LocalDecider, SyntraClient } from "../src/index.ts";
import type { Action, Context, Decision, LocalDecideOptions, LocalDecision } from "../src/index.ts";
import { ADMIN_KEY, TestServer } from "./server.ts";

let server: TestServer;

before(async () => {
  server = await TestServer.start();
});

after(async () => {
  await server?.stop();
});

function options(capsule: string) {
  return { url: server.url, token: ADMIN_KEY, tenant: "acme", job: "local", capsule, timeoutMs: 10_000 };
}

function admin(capsule: string): SyntraClient {
  return new SyntraClient(options(capsule));
}

/** `PUT .../spec` with a body given as text (a u64 seed does not fit a JS number). */
async function putSpecText(capsule: string, body: string): Promise<void> {
  const res = await fetch(`${server.url}/v1/tenants/acme/jobs/local/capsules/${capsule}/spec`, {
    method: "PUT",
    headers: { authorization: `Bearer ${ADMIN_KEY}`, "content-type": "application/json" },
    body,
  });
  const text = await res.text();
  assert.ok(res.status === 200 || res.status === 201, `spec PUT: ${res.status} ${text}`);
}

/** Deterministic pseudo-random numbers, so a failure can be replayed. */
function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const SPEC_ACTIONS: Action[] = [
  { id: "small", features: { cost: 0.1, latencyMs: 120 } },
  { id: "medium", features: { cost: 0.4 } },
  { id: "large", features: { cost: 1.0, tier: "premium", tags: ["gpu", "long"] } },
];

/** A varied context: floats that need 17 digits, integers, nesting, arrays, unicode, nulls. */
function context(i: number, random: () => number): Context {
  const tasks = ["chat", "code", "extract", "summarize"];
  const c: Context = {
    task: tasks[i % tasks.length],
    user: { tier: ["free", "pro", "team"][i % 3], age: 18 + (i % 50), score: random(), country: i % 7 === 0 ? "Åland" : "NZ" },
    promptTokens: 50 + ((i * 37) % 4000),
    ratio: random() * 2 - 1,
    tags: i % 2 === 0 ? ["a", "b", "a"] : [],
    flags: [true, false, null, { nested: i % 4 }],
    big: 2 ** 40 + i,
    tiny: random() * 1e-30,
    missing: null,
  };
  if (i % 11 === 0) c["10"] = "ten";
  if (i % 13 === 0) c["2"] = [1.5, -2.25];
  return c;
}

/** Per-request actions, exclusions, or neither. */
function requestOptions(i: number, random: () => number): LocalDecideOptions {
  if (i % 4 === 1) {
    return {
      actions: [
        { id: "p", features: { w: random(), kind: "fast" } },
        { id: "q", features: { nested: { k: "v", n: i }, list: [1, 2, random()] } },
        { id: "r" },
      ],
      exclude: i % 8 === 1 ? ["q"] : [],
    };
  }
  if (i % 5 === 2) return { exclude: ["medium"] };
  return {};
}

function rewardFor(d: LocalDecision, c: Context): number {
  if (d.action === "large" && c["task"] === "code") return 1;
  if (d.action === "small" && c["task"] === "chat") return 1;
  return d.action === "p" ? 0.7 : 0.1;
}

/** Every logged decision of a capsule, by id. */
async function logged(client: SyntraClient): Promise<Map<string, Decision>> {
  const all = new Map<string, Decision>();
  let after: string | undefined;
  for (;;) {
    const page = await client.decisions({ limit: 1000, after });
    for (const d of page.decisions) all.set(d.decisionId, d);
    if (page.next === null) return all;
    after = page.next;
  }
}

/**
 * Probabilities computed by the WebAssembly build and by the server's
 * native build: equal to the last bit, except that libm's `pow` (SquareCB's
 * exploration rate) can differ in its last bit between platforms, and that
 * the log keeps the PMF as JSON text, which the server reads back with
 * serde_json's default float parser (off by one unit in the last place for
 * some 16- and 17-digit numbers). The server accepts differences up to 1e-9.
 */
function assertClose(logged: number | undefined, mine: number | undefined): void {
  assert.ok(logged !== undefined && mine !== undefined);
  assert.ok(Math.abs(logged - mine) <= 4 * Number.EPSILON * Math.max(logged, mine), `${logged} != ${mine}`);
}

/** The server's stored copy of a local decision matches it. */
function assertSameDecision(local: LocalDecision, stored: Decision | undefined): void {
  assert.ok(stored !== undefined, `${local.decisionId} is not in the log`);
  assert.equal(stored.action, local.action);
  assert.equal(stored.chosenIndex, local.actionIndex);
  assertClose(stored.probability ?? undefined, local.probability);
  const eligible = stored.eligible.map((k) => stored.actions[k]?.id);
  const byId = new Map(local.ranking.map((r) => [r.id, r.probability]));
  assert.deepEqual(eligible.slice().sort(), [...byId.keys()].sort());
  stored.eligible.forEach((k, j) => assertClose(stored.pmf?.[j], byId.get(stored.actions[k]?.id ?? "")));
}

describe("local decisions replay on the server", () => {
  it("hundreds of decisions on an untrained and then a trained model all verify", async () => {
    const capsule = "e2e-learner";
    const c = admin(capsule);
    await c.putSpec({ actions: SPEC_ACTIONS });
    const decider = await LocalDecider.connect({ ...options(capsule), syncIntervalMs: 0 });
    try {
      assert.equal(decider.modelVersion, 0);
      assert.equal(await decider.sync(), false);
      const random = rng(1);
      const made: LocalDecision[] = [];

      // Round 1: the untrained model (every PMF uniform over the eligible set).
      for (let i = 0; i < 300; i++) {
        const ctx = context(i, random);
        const d = decider.decide(ctx, requestOptions(i, random));
        made.push(d);
        decider.reward(d.decisionId, rewardFor(d, ctx));
      }
      const first = await decider.flush();
      assert.deepEqual(first, {
        decisionsAccepted: 300,
        decisionsRejected: 0,
        rewardsApplied: 300,
        rewardsFailed: 0,
        requeued: 0,
        errors: [],
      });
      assert.equal((await c.model()).modelVersion, 300);

      // Decisions made on the old model and uploaded after the sync still verify.
      const stale: LocalDecision[] = [];
      for (let i = 300; i < 320; i++) stale.push(decider.decide(context(i, random), requestOptions(i, random)));
      const oldTag = decider.modelTag;

      // Round 2: the server publishes the learned model at most once a second.
      const deadline = Date.now() + 10_000;
      while (!(await decider.sync())) {
        assert.ok(Date.now() < deadline, "no new model was published");
        await new Promise((done) => setTimeout(done, 100));
      }
      assert.notEqual(decider.modelTag, oldTag);
      assert.equal(decider.modelVersion, 300);
      let skewed = 0;
      for (let i = 320; i < 620; i++) {
        const ctx = context(i, random);
        const d = decider.decide(ctx, requestOptions(i, random));
        assert.equal(d.modelVersion, 300);
        if (d.ranking.length > 1 && d.ranking[0]!.probability > d.ranking.at(-1)!.probability + 1e-6) skewed += 1;
        made.push(d);
        decider.reward(d.decisionId, rewardFor(d, ctx));
      }
      assert.ok(skewed > 100, `the trained model should prefer some actions (${skewed} skewed PMFs)`);
      made.push(...stale);
      const second = await decider.flush();
      assert.equal(second.decisionsRejected, 0, second.errors.join("; "));
      assert.equal(second.decisionsAccepted, 320);
      assert.equal(second.rewardsApplied, 300);
      assert.equal(decider.pending, 0);

      // The log holds every decision exactly as it was made here.
      const log = await logged(c);
      assert.equal(log.size, 620);
      for (const d of made) assertSameDecision(d, log.get(d.decisionId));
      const report = await c.evaluate({ policy: "logged", bootstrap: 0 });
      assert.equal(report.data.rows, 620);
      assert.equal(report.diagnostics.n, 600);
      assert.equal((await c.model()).modelVersion, 600);
    } finally {
      await decider.close();
    }
  });

  it("baselineExplore, epsilonGreedy and frozen capsules verify too", async () => {
    const specs = {
      "e2e-baseline": { actions: SPEC_ACTIONS, mode: "baselineExplore", baselineEpsilon: 0.3 },
      "e2e-epsilon": { actions: SPEC_ACTIONS, exploration: { kind: "epsilonGreedy", epsilon: 0.2, floor: 0.1 } },
      "e2e-frozen": { actions: SPEC_ACTIONS, mode: "frozen" },
    } as const;
    for (const [capsule, spec] of Object.entries(specs)) {
      await admin(capsule).putSpec(spec);
      const decider = await LocalDecider.connect({ ...options(capsule), syncIntervalMs: 0 });
      try {
        const random = rng(capsule.length);
        for (let i = 0; i < 120; i++) {
          const baseline = capsule === "e2e-baseline" ? (["small", "medium", "large"] as const)[i % 3] : undefined;
          const d = decider.decide(context(i, random), baseline === undefined ? {} : { baseline });
          decider.reward(d.decisionId, random());
        }
        const report = await decider.flush();
        assert.equal(report.decisionsRejected, 0, `${capsule}: ${report.errors.join("; ")}`);
        assert.equal(report.decisionsAccepted, 120, capsule);
        assert.equal(report.rewardsApplied, 120, capsule);
      } finally {
        await decider.close();
      }
    }
  });

  it("a capsule with a fixed u64 seed decides the same in two deciders, and as the server does", async () => {
    const capsule = "e2e-seeded";
    await putSpecText(
      capsule,
      `{"actions":${JSON.stringify(SPEC_ACTIONS)},"seed":18446744073709551615,"exploration":{"floor":0.5}}`,
    );
    const contexts = Array.from({ length: 60 }, (_, i) => context(i, rng(7)));
    const runs: LocalDecision[][] = [];
    for (let run = 0; run < 2; run++) {
      const decider = await LocalDecider.connect({ ...options(capsule), syncIntervalMs: 0 });
      try {
        runs.push(contexts.map((ctx) => decider.decide(ctx)));
        const report = await decider.flush();
        assert.equal(report.decisionsAccepted, 60);
        assert.equal(report.decisionsRejected, 0, report.errors.join("; "));
      } finally {
        await decider.close();
      }
    }
    const [a, b] = runs as [LocalDecision[], LocalDecision[]];
    const strip = (d: LocalDecision) => ({ action: d.action, probability: d.probability, ranking: d.ranking });
    assert.deepEqual(a.map(strip), b.map(strip));
    assert.ok(new Set(a.map((d) => d.action)).size > 1, "the draws differ from one decision to the next");
    // The server's first decision on this capsule draws the same seed as a
    // decider's first.
    const served = await admin(capsule).decide(contexts[0]);
    assert.equal(served.action, a[0]?.action);
    assert.equal(served.probability, a[0]?.probability);
    assert.deepEqual(served.ranking, a[0]?.ranking);
  });

  it("a read token for the capsule is enough", async () => {
    const capsule = "e2e-read-token";
    const c = admin(capsule);
    await c.putSpec({ actions: SPEC_ACTIONS });
    const { token } = await c.issueToken({ kind: "read", tenant: "acme", job: "local", capsule }, "local-decider", 600);
    const decider = await LocalDecider.connect({ ...options(capsule), token, syncIntervalMs: 0 });
    try {
      const d = decider.decide({ task: "chat" });
      decider.reward(d.decisionId, 1);
      assert.equal(await decider.sync(), false);
      const report = await decider.flush();
      assert.equal(report.decisionsAccepted, 1);
      assert.equal(report.rewardsApplied, 1);
    } finally {
      await decider.close();
    }
  });

  it("large inputs upload in requests under the server's 4 MiB body limit", async () => {
    const capsule = "e2e-large";
    await admin(capsule).putSpec({ actions: SPEC_ACTIONS });
    const decider = await LocalDecider.connect({ ...options(capsule), syncIntervalMs: 0 });
    try {
      // About 6 KB per item: 1000 items, the item cap per request, would be
      // about 6 MB.
      const note = "é".repeat(3000);
      for (let i = 0; i < 1200; i++) decider.decide({ i, note }, { exclude: i % 2 === 0 ? ["small"] : [] });
      const report = await decider.flush();
      assert.equal(report.decisionsRejected, 0, report.errors.join("; "));
      assert.equal(report.decisionsAccepted, 1200);
    } finally {
      await decider.close();
    }
  });

  it("under rewards: sum each queued reward gets its own key and counts once", async () => {
    const capsule = "e2e-sum";
    const c = admin(capsule);
    await c.putSpec({ actions: SPEC_ACTIONS, rewards: "sum" });
    const decider = await LocalDecider.connect({ ...options(capsule), syncIntervalMs: 0 });
    try {
      const d = decider.decide({ task: "code" });
      for (let k = 0; k < 3; k++) decider.reward(d.decisionId, 0.25);
      const report = await decider.flush();
      assert.equal(report.rewardsApplied, 3);
      const stored = await c.decision(d.decisionId);
      assert.equal(stored.rewards.length, 3);
      assert.equal(new Set(stored.rewards.map((r) => r.idempotencyKey)).size, 3);
      assert.equal((await c.model()).modelVersion, 3);
    } finally {
      await decider.close();
    }
  });
});
