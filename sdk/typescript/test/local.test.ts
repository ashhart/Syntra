/**
 * `LocalDecider` against a scripted `node:http` server that publishes
 * models built by test/model.ts: model verification, sync with ETags,
 * decision ids and upload items, queueing, requeueing and the background
 * timer. test/local-e2e.test.ts checks the uploads against a real server.
 */

import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { describe, it } from "node:test";

import { LocalDecider, SyntraError } from "../src/index.ts";
import type { LocalDeciderOptions } from "../src/index.ts";
import { decideSection, fixedSeedReference, published, snapshot, splitmix64 } from "./model.ts";
import type { Published } from "./model.ts";

const TOKEN = "local-test-token-3b9e1f07";
const PREFIX = "/v1/tenants/acme/jobs/prod/capsules/router";

interface Seen {
  method: string;
  path: string;
  headers: IncomingMessage["headers"];
  body: string;
}

type Handler = (body: string) => { status: number; body?: unknown };

/** A capsule's model and upload routes, scripted. */
class Stub {
  model: Published;
  readonly seen: Seen[] = [];
  decisions: Handler = (body) => {
    const n = (JSON.parse(body) as { decisions: unknown[] }).decisions.length;
    return { status: 200, body: { accepted: n, duplicates: 0, unverified: 0, rejected: [] } };
  };
  rewards: Handler = (body) => {
    const items = (JSON.parse(body) as { rewards: unknown[] }).rewards;
    return { status: 200, body: { results: items.map(() => ({ ok: true, applied: true, learned: true, modelVersion: 1 })) } };
  };
  url = "";
  readonly #server = createServer((req, res) => this.#answer(req, res));

  constructor(model: Published) {
    this.model = model;
  }

  async start(): Promise<this> {
    await new Promise<void>((done) => this.#server.listen(0, "127.0.0.1", done));
    this.url = `http://127.0.0.1:${(this.#server.address() as AddressInfo).port}`;
    return this;
  }

  close(): Promise<void> {
    this.#server.closeAllConnections();
    return new Promise((done) => this.#server.close(() => done()));
  }

  options(extra: Partial<LocalDeciderOptions> = {}): LocalDeciderOptions {
    return {
      url: this.url,
      token: TOKEN,
      tenant: "acme",
      job: "prod",
      capsule: "router",
      syncIntervalMs: 0,
      maxRetries: 0,
      ...extra,
    };
  }

  /** Bodies of the uploads to `route` (`decisions:batch` or `rewards:batch`). */
  uploads(route: string): string[] {
    return this.seen.filter((s) => s.path === `${PREFIX}/${route}`).map((s) => s.body);
  }

  #answer(req: IncomingMessage, res: ServerResponse): void {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk: string) => {
      body += chunk;
    });
    req.on("end", () => {
      const path = req.url ?? "";
      this.seen.push({ method: req.method ?? "", path, headers: req.headers, body });
      const send = (status: number, payload?: unknown, headers: Record<string, string> = {}): void => {
        const text = payload === undefined ? "" : typeof payload === "string" ? payload : JSON.stringify(payload);
        res.writeHead(status, { "content-type": "application/json", ...headers });
        res.end(text);
      };
      if (req.headers.authorization !== `Bearer ${TOKEN}`) return send(401, { error: "unauthorized" });
      if (req.method === "GET" && path === `${PREFIX}/model?snapshot=true`) {
        const etag = `"${this.model.tag}"`;
        if (req.headers["if-none-match"] === etag) return send(304, undefined, { etag });
        return send(200, this.model.text, { etag });
      }
      if (req.method === "POST" && path === `${PREFIX}/decisions:batch`) {
        const a = this.decisions(body);
        return send(a.status, a.body);
      }
      if (req.method === "POST" && path === `${PREFIX}/rewards:batch`) {
        const a = this.rewards(body);
        return send(a.status, a.body);
      }
      send(404, { error: "no such route" });
    });
  }
}

const MODEL = published(decideSection(), snapshot({ updates: 7n }));

/** Runs `body` with a started stub and a connected decider, then closes both. */
async function withDecider(
  model: Published,
  body: (decider: LocalDecider, stub: Stub) => Promise<void>,
  extra: Partial<LocalDeciderOptions> = {},
): Promise<void> {
  const stub = await new Stub(model).start();
  const decider = await LocalDecider.connect(stub.options(extra));
  try {
    await body(decider, stub);
  } finally {
    await decider.close().catch(() => undefined);
    await stub.close();
  }
}

async function rejection<T extends Error>(promise: Promise<unknown>, type: new (...args: never[]) => T): Promise<T> {
  try {
    await promise;
  } catch (err) {
    assert.ok(err instanceof type, `expected ${type.name}, got ${String(err)}`);
    return err;
  }
  assert.fail(`expected ${type.name}, but the call succeeded`);
}

interface UploadedItem {
  decisionId: string;
  tsMs: number;
  modelTag: string;
  modelVersion?: number;
  seed: string;
  input: Record<string, unknown>;
  chosenIndex: number;
  chosenId: string;
  probability: number;
  pmf: number[];
  eligible: number[];
}

function uploadedDecisions(stub: Stub): UploadedItem[] {
  return stub.uploads("decisions:batch").flatMap((b) => (JSON.parse(b) as { decisions: UploadedItem[] }).decisions);
}

describe("connecting", () => {
  it("loads a verified model and decides synchronously", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      assert.equal(decider.modelTag, MODEL.tag);
      assert.equal(decider.modelVersion, 7);
      assert.equal(stub.seen[0]?.path, `${PREFIX}/model?snapshot=true`);
      assert.equal(stub.seen[0]?.headers["if-none-match"], undefined);

      const before = Date.now();
      const d = decider.decide({ user: { tier: "pro" }, tokens: 812 });
      const after = Date.now();
      assert.match(d.decisionId, /^loc_[0-9a-f]{11}[0-9a-f]{16}$/);
      const ts = Number.parseInt(d.decisionId.slice(4, 15), 16);
      assert.ok(ts >= before && ts <= after, `${ts} not in [${before}, ${after}]`);
      assert.ok(["a", "b", "c"].includes(d.action));
      assert.equal(d.actionIndex, ["a", "b", "c"].indexOf(d.action));
      assert.equal(d.modelVersion, 7);
      assert.equal(d.ranking.length, 3);
      assert.equal(d.ranking.find((r) => r.id === d.action)?.probability, d.probability);
      for (let k = 1; k < d.ranking.length; k++) {
        assert.ok((d.ranking[k - 1]?.probability ?? 0) >= (d.ranking[k]?.probability ?? 0));
      }
      const total = d.ranking.reduce((sum, r) => sum + r.probability, 0);
      assert.ok(Math.abs(total - 1) < 1e-12);
      assert.equal(decider.pending, 1);
      // Ids differ in their random bits.
      const ids = new Set(Array.from({ length: 200 }, () => decider.decide().decisionId.slice(15)));
      assert.equal(ids.size, 200);
    });
  });

  it("refuses a model whose tag does not match, that is newer than the SDK, or that needs a feature program", async () => {
    const snap = snapshot();
    for (const [model, message] of [
      [published(decideSection(), snap, "0123456789abcdef"), /^model: snapshot does not match its tag$/],
      [published(decideSection({ version: 2 }), snap), /decide spec is version 2.*upgrade the SDK/],
      [published(decideSection({ bits: 12 }), snap), /learner\.bits = 10 but the spec sets 12/],
      [{ text: '{"decide": {}}', tag: "x" }, /^model: no snapshot$/],
      [{ text: MODEL.text.replace('"programSha256":null', '"programSha256":"9f2c"'), tag: MODEL.tag }, /feature program/],
    ] as const) {
      const stub = await new Stub(model).start();
      try {
        const err = await rejection(LocalDecider.connect(stub.options()), SyntraError);
        assert.match(err.message, message);
      } finally {
        await stub.close();
      }
    }
  });

  it("an HTTP error while connecting is thrown as is", async () => {
    const stub = await new Stub(MODEL).start();
    try {
      const err = await rejection(LocalDecider.connect({ ...stub.options(), token: "wrong-token-1234" }), SyntraError);
      assert.match(err.message, /HTTP 401: unauthorized/);
    } finally {
      await stub.close();
    }
  });
});

describe("deciding", () => {
  it("validates requests as the server would, and queues nothing for a refused one", async () => {
    await withDecider(MODEL, async (decider) => {
      const refuse = (f: () => unknown, message: string | RegExp): void => {
        assert.throws(f, (err: unknown) => {
          assert.ok(err instanceof SyntraError, String(err));
          if (typeof message === "string") assert.equal(err.message, message);
          else assert.match(err.message, message);
          return true;
        });
      };
      refuse(() => decider.decide({}, { exclude: ["zzz"] }), 'excludedActions names unknown action "zzz"');
      refuse(
        () => decider.decide({}, { exclude: ["a", "b", "c"] }),
        "no eligible actions remain after the eligible filter and exclusions",
      );
      refuse(() => decider.decide([1] as never), "context must be a JSON object");
      refuse(() => decider.decide({ x: 1e39 }), /not a finite 32-bit number/);
      refuse(() => decider.decide({}, { actions: [{ id: "x" }, { id: "x" }] }), /duplicates actions\[0\]\.id/);
      refuse(() => decider.decide({}, { actions: [{ id: "x", extra: 1 } as never] }), /unknown field `extra`/);
      refuse(() => decider.decide({}, { baseline: "nope" }), /baselineAction names unknown action/);
      assert.throws(() => decider.decide({}, { exclude: "a" as never }), TypeError);
      assert.throws(() => decider.decide({ n: 1n } as never), TypeError);
      assert.equal(decider.pending, 0);

      const d = decider.decide(null, { exclude: new Set(["a", "b"]) });
      assert.equal(d.action, "c");
      assert.equal(d.probability, 1);
      const own = decider.decide({}, { actions: [{ id: "x", features: { w: 2 } }, { id: "y" }], exclude: ["y"] });
      assert.equal(own.action, "x");
      assert.deepEqual(own.ranking, [{ id: "x", probability: 1 }]);
    });
  });

  it("the queue is bounded; a flush makes room", async () => {
    await withDecider(
      MODEL,
      async (decider) => {
        decider.decide({ i: 1 });
        const d = decider.decide({ i: 2 });
        decider.reward(d.decisionId, 1);
        assert.equal(decider.pending, 3);
        assert.throws(() => decider.decide({ i: 3 }), /upload queue is full/);
        assert.throws(() => decider.reward(d.decisionId, 0), /upload queue is full/);
        assert.equal(decider.pending, 3);
        const report = await decider.flush();
        assert.equal(report.decisionsAccepted, 2);
        assert.equal(report.rewardsApplied, 1);
        assert.equal(decider.pending, 0);
        decider.decide({ i: 4 });
      },
      { maxQueue: 3 },
    );
  });

  it("a capsule with a fixed u64 seed decides reproducibly, with the Rust client's seeds", async () => {
    const base = (1n << 64n) - 1n; // above 2^53: exact only if it never passes through a JS number
    const model = published(decideSection({ seed: base }), snapshot({ updates: 3n }));
    assert.equal(splitmix64(1234567n), 6457827717110365317n); // Vigna's reference output
    const contexts = Array.from({ length: 40 }, (_, i) => ({ i, tier: ["free", "pro", "team"][i % 3] }));
    const runs: Array<Array<{ action: string; probability: number }>> = [];
    const seeds: string[][] = [];
    for (let run = 0; run < 2; run++) {
      await withDecider(model, async (decider, stub) => {
        runs.push(contexts.map((c) => decider.decide(c)).map(({ action, probability }) => ({ action, probability })));
        await decider.flush();
        seeds.push(uploadedDecisions(stub).map((item) => item.seed));
      });
    }
    assert.deepEqual(runs[0], runs[1]);
    assert.deepEqual(seeds[0], seeds[1]);
    assert.deepEqual(
      seeds[0],
      contexts.map((_, n) => fixedSeedReference(base, BigInt(n)).toString()),
    );
    // Different draws, not one seed repeated.
    assert.equal(new Set(seeds[0]).size, contexts.length);
  });
});

describe("uploads", () => {
  it("decisions upload in the Rust client's item shape, then rewards", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      const context = { b: 1, a: { y: 0.1 + 0.2, x: 0.9704863283649415 }, "10": "ten", "2": [true, null, "s"] };
      const d1 = decider.decide(context, { exclude: ["a"] });
      // Later changes to the caller's objects do not reach the upload.
      context.b = 2;
      const d2 = decider.decide({}, { actions: [{ id: "x" }, { id: "y", features: { w: 1.5 } }] });
      const d3 = decider.decide(undefined, { baseline: "b" });
      decider.reward(d1.decisionId, 0.5);
      decider.reward(d2.decisionId, 1, { idempotencyKey: "k-2", detail: { latencyMs: 840, note: "é" } });
      assert.equal(decider.pending, 5);

      const report = await decider.flush();
      assert.deepEqual(report, {
        decisionsAccepted: 3,
        decisionsRejected: 0,
        rewardsApplied: 2,
        rewardsFailed: 0,
        requeued: 0,
        errors: [],
      });
      assert.equal(decider.pending, 0);
      const order = stub.seen.filter((s) => s.method === "POST").map((s) => s.path);
      assert.deepEqual(order, [`${PREFIX}/decisions:batch`, `${PREFIX}/rewards:batch`]);

      const [body] = stub.uploads("decisions:batch");
      // The input is uploaded as the exact text the decision was made from.
      const decidedOn = JSON.stringify({ context: { b: 1, a: { y: 0.1 + 0.2, x: 0.9704863283649415 }, "10": "ten", "2": [true, null, "s"] }, excludedActions: ["a"] });
      assert.ok(body?.includes(`"input":${decidedOn}`), body);

      const items = uploadedDecisions(stub);
      assert.equal(items.length, 3);
      const [i1, i2, i3] = items as [UploadedItem, UploadedItem, UploadedItem];
      assert.deepEqual(Object.keys(i1).sort(), [
        "chosenId",
        "chosenIndex",
        "decisionId",
        "eligible",
        "input",
        "modelTag",
        "modelVersion",
        "pmf",
        "probability",
        "seed",
        "tsMs",
      ]);
      assert.equal(i1.decisionId, d1.decisionId);
      assert.equal(i1.modelTag, MODEL.tag);
      assert.equal(i1.modelVersion, 7);
      assert.equal(i1.tsMs, Number.parseInt(d1.decisionId.slice(4, 15), 16));
      assert.match(i1.seed, /^[0-9]{1,20}$/);
      assert.equal(i1.chosenId, d1.action);
      assert.equal(i1.chosenIndex, d1.actionIndex);
      assert.equal(i1.probability, d1.probability);
      assert.deepEqual(i1.eligible, [1, 2]);
      assert.equal(i1.pmf.length, 2);
      assert.equal(i1.pmf[i1.eligible.indexOf(i1.chosenIndex)], i1.probability);

      assert.deepEqual(i2.input, { context: {}, actions: [{ id: "x" }, { id: "y", features: { w: 1.5 } }] });
      assert.deepEqual(i2.eligible, [0, 1]);
      assert.deepEqual(i3.input, { context: {}, baselineAction: "b" });

      const [rewards] = stub.uploads("rewards:batch");
      assert.deepEqual(JSON.parse(rewards ?? ""), {
        rewards: [
          { decisionId: d1.decisionId, reward: 0.5 },
          { decisionId: d2.decisionId, reward: 1, idempotencyKey: "k-2", detail: { latencyMs: 840, note: "é" } },
        ],
      });
    });
  });

  it("under rewards: sum every reward gets its own idempotency key", async () => {
    const model = published(decideSection({ rewards: "sum" }), snapshot());
    await withDecider(model, async (decider, stub) => {
      const d = decider.decide();
      decider.reward(d.decisionId, 1);
      decider.reward(d.decisionId, 1);
      decider.reward(d.decisionId, 1, { idempotencyKey: "mine" });
      assert.throws(() => decider.reward(d.decisionId, 1, { idempotencyKey: "" }), TypeError);
      assert.throws(() => decider.reward(d.decisionId, 1, { idempotencyKey: "x".repeat(257) }), TypeError);
      assert.throws(() => decider.reward(d.decisionId, Number.NaN), TypeError);
      await decider.flush();
      const keys = (JSON.parse(stub.uploads("rewards:batch")[0] ?? "") as { rewards: Array<{ idempotencyKey?: string }> })
        .rewards.map((r) => r.idempotencyKey);
      assert.match(keys[0] ?? "", new RegExp(`^${d.decisionId}:[0-9a-f]{16}$`));
      assert.match(keys[1] ?? "", new RegExp(`^${d.decisionId}:[0-9a-f]{16}$`));
      assert.notEqual(keys[0], keys[1]);
      assert.equal(keys[2], "mine");
    });
  });

  it("retryable rejections are queued again, and rewards wait for them", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      const busy = { error: "the event log is backlogged", retryable: true };
      stub.decisions = (body) => {
        const items = (JSON.parse(body) as { decisions: Array<{ decisionId: string }> }).decisions;
        return {
          status: 200,
          body: {
            accepted: items.length - 2,
            duplicates: 0,
            unverified: 0,
            rejected: [
              { index: 0, decisionId: items[0]?.decisionId, ...busy },
              { index: 2, decisionId: items[2]?.decisionId, error: "decision does not replay", retryable: false },
            ],
          },
        };
      };
      const ds = [0, 1, 2, 3].map((i) => decider.decide({ i }));
      for (const d of ds) decider.reward(d.decisionId, 1);
      const first = await decider.flush();
      assert.equal(first.decisionsAccepted, 2);
      assert.equal(first.decisionsRejected, 1);
      assert.deepEqual(first.errors, ["decision does not replay"]);
      assert.equal(first.requeued, 1 + 4);
      assert.equal(first.rewardsApplied, 0);
      assert.equal(stub.uploads("rewards:batch").length, 0);
      assert.equal(decider.pending, 5);

      stub.decisions = () => ({ status: 200, body: { accepted: 1, duplicates: 0, unverified: 0, rejected: [] } });
      const second = await decider.flush();
      assert.equal(second.decisionsAccepted, 1);
      assert.equal(second.rewardsApplied, 4);
      assert.equal(second.requeued, 0);
      const retried = uploadedDecisions(stub).slice(4);
      assert.deepEqual(
        retried.map((i) => i.decisionId),
        [ds[0]?.decisionId],
      );
      assert.equal(decider.pending, 0);
    });
  });

  it("rewards the server cannot take now (503) are queued again; other failures are final", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      stub.rewards = () => ({
        status: 200,
        body: {
          results: [
            { ok: true, applied: true, learned: true, modelVersion: 8 },
            { ok: false, decisionId: "x", status: 503, error: "backlogged" },
            { ok: false, decisionId: "y", status: 404, error: 'decision "y" not found' },
          ],
        },
      });
      const d = decider.decide();
      decider.reward(d.decisionId, 1);
      decider.reward("x", 1);
      decider.reward("y", 1);
      const report = await decider.flush();
      assert.equal(report.rewardsApplied, 1);
      assert.equal(report.rewardsFailed, 1);
      assert.equal(report.requeued, 1);
      assert.deepEqual(report.errors, ['decision "y" not found']);
      assert.equal(decider.pending, 1);
      stub.rewards = () => ({ status: 200, body: { results: [{ ok: true, applied: true, modelVersion: 9 }] } });
      const again = await decider.flush();
      assert.equal(again.rewardsApplied, 1);
      assert.deepEqual(JSON.parse(stub.uploads("rewards:batch")[1] ?? ""), { rewards: [{ decisionId: "x", reward: 1 }] });
    });
  });

  it("an upload that fails keeps every event queued, in order, and throws", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      const ok = stub.decisions;
      stub.decisions = () => ({ status: 500, body: { error: "disk full" } });
      const ds = [1, 2, 3].map((i) => decider.decide({ i }));
      decider.reward(ds[0]?.decisionId ?? "", 1);
      const err = await rejection(decider.flush(), SyntraError);
      assert.match(err.message, /decisions:batch: HTTP 500: disk full/);
      assert.equal(decider.pending, 4);
      const later = decider.decide({ i: 4 });
      stub.decisions = ok;
      const report = await decider.flush();
      assert.equal(report.decisionsAccepted, 4);
      assert.equal(report.rewardsApplied, 1);
      assert.deepEqual(
        uploadedDecisions(stub)
          .slice(3)
          .map((i) => i.decisionId),
        [...ds.map((d) => d.decisionId), later.decisionId],
      );
    });
  });

  it("uploads are split by item count and by bytes; an event too large for any request is dropped", async () => {
    await withDecider(
      MODEL,
      async (decider, stub) => {
        for (let i = 0; i < 2500; i++) decider.decide({ i });
        let report = await decider.flush();
        assert.equal(report.decisionsAccepted, 2500);
        assert.deepEqual(
          stub.uploads("decisions:batch").map((b) => (JSON.parse(b) as { decisions: unknown[] }).decisions.length),
          [1000, 1000, 500],
        );

        // ~1.5 MB of context each, in multi-byte characters: 3 per request would pass 4 MiB.
        const big = "é".repeat(750_000);
        for (let i = 0; i < 5; i++) decider.decide({ i, big });
        const huge = decider.decide({ huge: "x".repeat(4 * 1024 * 1024) });
        report = await decider.flush();
        assert.equal(report.decisionsAccepted, 5);
        assert.equal(report.decisionsRejected, 1);
        assert.match(report.errors[0] ?? "", new RegExp(`^decision ${huge.decisionId} is \\d+ bytes, more than`));
        const sizes = stub.uploads("decisions:batch").slice(3).map((b) => new TextEncoder().encode(b).length);
        assert.deepEqual(
          stub.uploads("decisions:batch").slice(3).map((b) => (JSON.parse(b) as { decisions: unknown[] }).decisions.length),
          [2, 2, 1],
        );
        for (const size of sizes) assert.ok(size <= 4 * 1024 * 1024, `${size} bytes`);
        assert.equal(decider.pending, 0);
      },
      { maxQueue: 10_000 },
    );
  });
});

describe("syncing", () => {
  it("polls with If-None-Match; a 304 keeps the model, a new one replaces it", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      assert.equal(await decider.sync(), false);
      const poll = stub.seen.at(-1);
      assert.equal(poll?.headers["if-none-match"], `"${MODEL.tag}"`);

      const before = decider.decide({ i: 1 });
      const next = published(
        decideSection({ mode: "frozen" }),
        snapshot({ updates: 9n, slots: [{ slot: 3, u: 0.5, s: 1, a: 0.25 }] }),
      );
      stub.model = next;
      assert.equal(await decider.sync(), true);
      assert.equal(decider.modelTag, next.tag);
      assert.equal(decider.modelVersion, 9);
      const after = decider.decide({ i: 1 });
      assert.equal(after.modelVersion, 9);
      assert.equal(await decider.sync(), false);

      await decider.flush();
      const [i1, i2] = uploadedDecisions(stub);
      assert.equal(i1?.decisionId, before.decisionId);
      assert.equal(i1?.modelTag, MODEL.tag);
      assert.equal(i1?.modelVersion, 7);
      assert.equal(i2?.modelTag, next.tag);
      assert.equal(i2?.modelVersion, 9);
    });
  });

  it("a model that fails verification leaves the current one in place", async () => {
    await withDecider(MODEL, async (decider, stub) => {
      stub.model = published(decideSection(), snapshot({ updates: 8n }), "ffffffffffffffff");
      await rejection(decider.sync(), SyntraError);
      assert.equal(decider.modelTag, MODEL.tag);
      assert.equal(decider.decide().modelVersion, 7);
    });
  });

  it("the background timer uploads and syncs, reports errors, and stops on close", async () => {
    const errors: unknown[] = [];
    const stub = await new Stub(MODEL).start();
    const decider = await LocalDecider.connect(stub.options({ syncIntervalMs: 20, onError: (e) => errors.push(e) }));
    try {
      const d = decider.decide({ i: 1 });
      decider.reward(d.decisionId, 1);
      const next = published(decideSection(), snapshot({ updates: 11n }));
      stub.model = next;
      await waitFor(() => decider.pending === 0 && decider.modelTag === next.tag);
      assert.equal(uploadedDecisions(stub).length, 1);
      assert.equal(stub.uploads("rewards:batch").length, 1);

      stub.decisions = () => ({ status: 500, body: { error: "down" } });
      decider.decide({ i: 2 });
      await waitFor(() => errors.length > 0);
      assert.match(String(errors[0]), /HTTP 500: down/);
      assert.equal(decider.pending, 1);
    } finally {
      stub.decisions = (body) => {
        const n = (JSON.parse(body) as { decisions: unknown[] }).decisions.length;
        return { status: 200, body: { accepted: n, duplicates: 0, unverified: 0, rejected: [] } };
      };
      const report = await decider.close();
      assert.equal(report.decisionsAccepted, 1);
      const requests = stub.seen.length;
      await new Promise((done) => setTimeout(done, 80));
      assert.equal(stub.seen.length, requests, "no background requests after close");
      await stub.close();
    }
  });

  it("close uploads what is queued; afterwards the decider refuses new work", async () => {
    const stub = await new Stub(MODEL).start();
    const decider = await LocalDecider.connect(stub.options());
    try {
      const d = decider.decide();
      decider.reward(d.decisionId, 0.25);
      const report = await decider.close();
      assert.equal(report.decisionsAccepted, 1);
      assert.equal(report.rewardsApplied, 1);
      assert.throws(() => decider.decide(), /closed/);
      assert.throws(() => decider.reward(d.decisionId, 1), /closed/);
      await rejection(decider.sync(), SyntraError);
      assert.equal(decider.modelTag, MODEL.tag);
      assert.equal((await decider.close()).decisionsAccepted, 0);
    } finally {
      await stub.close();
    }
  });
});

async function waitFor(condition: () => boolean, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail("timed out waiting");
    await new Promise((done) => setTimeout(done, 5));
  }
}
