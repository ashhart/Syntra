/**
 * End-to-end tests against a real `syntra serve` (see server.ts). Each test
 * uses its own capsule, so they share one server.
 */

import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
import { inspect } from "node:util";

import { HttpError, SyntraClient, SyntraError, TransportError } from "../src/index.ts";
import type { UploadedDecision } from "../src/index.ts";
import { ADMIN_KEY, TestServer, freePort } from "./server.ts";

const ACTIONS = [{ id: "a" }, { id: "b" }, { id: "c" }];

let server: TestServer;

before(async () => {
  server = await TestServer.start();
});

after(async () => {
  await server?.stop();
});

function client(capsule: string, token: string = ADMIN_KEY): SyntraClient {
  return new SyntraClient({ url: server.url, token, tenant: "acme", job: "prod", capsule, timeoutMs: 5000 });
}

/** The HttpError `promise` rejects with; fails unless its status is `status`. */
async function httpError(promise: Promise<unknown>, status: number): Promise<HttpError> {
  try {
    await promise;
  } catch (err) {
    assert.ok(err instanceof HttpError, `expected an HttpError, got ${String(err)}`);
    assert.equal(err.status, status, err.message);
    return err;
  }
  assert.fail(`expected HTTP ${status}, but the call succeeded`);
}

/** However the error is printed, the token is not in it. */
function assertNoToken(err: Error, token: string): void {
  for (const text of [err.message, String(err), err.stack ?? "", inspect(err, { depth: 10 }), JSON.stringify(err)]) {
    assert.ok(!text.includes(token), `the token leaked into: ${text}`);
  }
}

describe("decide and reward", () => {
  it("a reward raises the model version", async () => {
    const c = client("round-trip");
    const spec = await c.putSpec({ actions: ACTIONS });
    assert.deepEqual(spec.actions, ACTIONS);

    const d = await c.decide({ task: "code", promptTokens: 812 });
    assert.match(d.decisionId, /^dec_[0-9a-f]+$/);
    assert.ok(["a", "b", "c"].includes(d.action));
    assert.equal(d.modelVersion, 0);
    assert.equal(d.mode, "learner");
    assert.equal(d.ranking.length, 3);
    assert.equal(d.ranking.find((r) => r.id === d.action)?.probability, d.probability);
    const total = d.ranking.reduce((sum, r) => sum + r.probability, 0);
    assert.ok(Math.abs(total - 1) < 1e-9);

    const r = await c.reward(d.decisionId, 0.8, { detail: { latencyMs: 840 } });
    assert.deepEqual(r, { ok: true, applied: true, learned: true, modelVersion: 1 });
    assert.equal((await c.model()).modelVersion, 1);
    assert.equal((await c.decide({ task: "code" })).modelVersion, 1);

    // Under `rewards: "first"` a second reward is a duplicate.
    const again = await c.reward(d.decisionId, 0.1, { durable: true });
    assert.deepEqual(again, { ok: true, applied: false, duplicate: true, modelVersion: 1 });
  });

  it("an eventId replays its decision; a different request with it is a 409", async () => {
    const c = client("event-ids");
    await c.putSpec({ actions: ACTIONS });
    const first = await c.decide({ user: 7 }, { eventId: "order:42", durable: true });
    assert.equal(first.decisionId, "order:42");
    assert.equal(first.replayed, undefined);

    const again = await c.decide({ user: 7 }, { eventId: "order:42", durable: true });
    const { replayed, ...rest } = again;
    assert.equal(replayed, true);
    assert.deepEqual(rest, first);

    const conflict = await httpError(c.decide({ user: 8 }, { eventId: "order:42", durable: true }), 409);
    assert.match(conflict.message, /already used for a different request/);

    // The `:` stays literal in the path, which is how the server looks it up.
    const stored = await c.decision("order:42");
    assert.equal(stored.decisionId, "order:42");
    assert.deepEqual(stored.context, { user: 7 });
  });

  it("per-request actions, exclusions and a baseline", async () => {
    const c = client("per-request");
    await c.putSpec({ actions: ACTIONS });

    const left = await c.decide({}, { exclude: ["a", "b"] });
    assert.equal(left.action, "c");
    assert.equal(left.probability, 1);
    assert.deepEqual(left.ranking, [{ id: "c", probability: 1 }]);

    const own = await c.decide(
      { page: "home" },
      { actions: [{ id: "x", features: { cost: 1 } }, { id: "y" }], exclude: new Set(["x"]) },
    );
    assert.equal(own.action, "y");
    assert.equal(own.actionIndex, 1);
    const stored = await c.decision(own.decisionId);
    assert.deepEqual(stored.actions, [{ id: "x", features: { cost: 1 } }, { id: "y" }]);
    assert.deepEqual(stored.eligible, [1]);
    assert.deepEqual(stored.pmf, [1]);

    await c.putSpec({ mode: "baselineExplore", baselineEpsilon: 0.3 });
    const baseline = await c.decide({}, { baseline: "b" });
    assert.equal(baseline.mode, "baselineExplore");
    assert.equal(baseline.ranking[0]?.id, "b");
    assert.ok(Math.abs((baseline.ranking[0]?.probability ?? 0) - 0.8) < 1e-9);
    const missing = await httpError(c.decide({}), 400);
    assert.match(missing.message, /baselineAction is required/);
  });
});

describe("spec", () => {
  it("a patch merges; replace starts from the defaults", async () => {
    const c = client("spec-patch");
    const created = await c.putSpec({ actions: ACTIONS, learner: { learningRate: 0.25 } });
    assert.equal(created.learner.learningRate, 0.25);
    assert.equal(created.exploration.floor, 0.05);

    const patched = await c.putSpec({ exploration: { floor: 0.2 } });
    assert.equal(patched.exploration.floor, 0.2);
    assert.equal(patched.learner.learningRate, 0.25);
    assert.deepEqual(patched.actions, ACTIONS);

    // null restores a default.
    const reset = await c.putSpec({ learner: { learningRate: null } });
    assert.equal(reset.learner.learningRate, 0.5);

    const replaced = await c.putSpec({ actions: [{ id: "solo" }] }, { replace: true });
    assert.equal(replaced.exploration.floor, 0.05);
    assert.deepEqual(replaced.actions, [{ id: "solo" }]);
    assert.deepEqual(await c.getSpec(), replaced);

    const { audits } = await c.audits();
    assert.deepEqual(
      audits.map((a) => a.event),
      ["capsule_created", "spec_updated", "spec_updated", "spec_replaced"],
    );
    assert.deepEqual((await c.audits({ limit: 1 })).audits.map((a) => a.event), ["spec_replaced"]);
  });
});

describe("reads", () => {
  it("a decision with its rewards, and the log page by page", async () => {
    const c = client("reads");
    await c.putSpec({ actions: ACTIONS, rewards: "sum" });
    // Ids that sort in creation order: the log is ordered by (time, id).
    const ids = ["r-0", "r-1", "r-2", "r-3", "r-4"];
    for (const [i, id] of ids.entries()) await c.decide({ i }, { eventId: id });

    const keyed = await c.reward("r-0", 1, { idempotencyKey: "order-1", detail: { source: "test" } });
    assert.equal(keyed.applied, true);
    const retried = await c.reward("r-0", 1, { idempotencyKey: "order-1" });
    assert.equal(retried.duplicate, true);
    await c.reward("r-0", 0.5); // no key: under `sum` it counts again

    const d = await c.decision("r-0");
    assert.equal(d.decisionId, "r-0");
    assert.deepEqual(d.context, { i: 0 });
    assert.deepEqual(d.eligible, [0, 1, 2]);
    assert.equal(d.pmf?.length, 3);
    assert.match(d.seed, /^\d+$/);
    assert.equal(d.derived, null);
    assert.deepEqual(
      d.rewards.map((r) => [r.reward, r.idempotencyKey === "order-1", r.detail?.source]),
      [
        [1, true, "test"],
        [0.5, false, undefined],
      ],
    );

    const first = await c.decisions({ limit: 2 });
    assert.deepEqual(first.decisions.map((x) => x.decisionId), ["r-0", "r-1"]);
    assert.equal(first.next, "r-1");
    assert.equal(first.decisions[0]?.rewards, undefined);
    const second = await c.decisions({ limit: 2, after: first.next ?? "" });
    assert.deepEqual(second.decisions.map((x) => x.decisionId), ["r-2", "r-3"]);
    const last = await c.decisions({ limit: 2, after: second.next ?? "" });
    assert.deepEqual(last.decisions.map((x) => x.decisionId), ["r-4"]);
    assert.equal(last.next, null);

    const all = (await c.decisions()).decisions;
    assert.equal(all.length, 5);
    const t = all[2]?.tsMs ?? 0;
    const since = (await c.decisions({ since: t })).decisions;
    const until = (await c.decisions({ until: t })).decisions;
    assert.ok(since.every((x) => x.tsMs >= t));
    assert.ok(until.every((x) => x.tsMs < t));
    assert.equal(since.length + until.length, 5);

    const model = await c.model();
    assert.equal(model.modelVersion, 2);
    assert.equal(model.spec.rewards, "sum");
  });
});

describe("evaluate and promote", () => {
  it("evaluates logged traffic; a promotion whose gate fails is refused", async () => {
    const c = client("ope");
    await c.putSpec({ actions: ACTIONS });
    const empty = await httpError(c.evaluate({ policy: "greedy" }), 409);
    assert.match(empty.message, /no logged decisions to evaluate/);

    for (let i = 0; i < 40; i++) {
      const d = await c.decide({ user: i % 4 });
      await c.reward(d.decisionId, d.action === "b" ? 1 : 0);
    }
    const report = await c.evaluate({ policy: "greedy", gates: ["n >= 10"], bootstrap: 0 });
    assert.deepEqual(Object.keys(report).sort(), [
      "data",
      "diagnostics",
      "estimators",
      "gates",
      "gatesPassed",
      "lift",
      "logged",
      "policy",
      "settings",
      "verdict",
      "warnings",
    ]);
    assert.equal(report.data.rows, 40);
    assert.equal(report.diagnostics.n, 40);
    assert.deepEqual(Object.keys(report.estimators).sort(), ["dm", "dr", "ips", "snips"]);
    assert.equal(typeof report.estimators.dr.estimate, "number");
    assert.equal(typeof report.lift.dr.mean, "number");
    assert.equal(report.settings.interval, "normal");
    assert.equal(report.gatesPassed, true);
    assert.equal(report.gates[0]?.pass, true);

    const candidate = { learner: { learningRate: 0.25 } };
    const refused = await c.promote({ spec: candidate, gates: ["n >= 1000000"], bootstrap: 0 });
    assert.equal(refused.promoted, false);
    assert.ok(!refused.promoted);
    assert.equal(refused.error, "a gate failed");
    assert.equal(refused.report.gatesPassed, false);
    assert.equal((await c.getSpec()).learner.learningRate, 0.5);

    const promoted = await c.promote({ spec: candidate, gates: ["n >= 10"], bootstrap: 0 });
    assert.ok(promoted.promoted);
    assert.equal(promoted.spec.learner.learningRate, 0.25);
    assert.equal((await c.getSpec()).learner.learningRate, 0.25);

    const events = (await c.audits()).audits.map((a) => a.event);
    assert.deepEqual(events.slice(-2), ["promotion_refused", "spec_promoted"]);

    const noGate = await httpError(c.promote({ spec: candidate, gates: [] }), 400);
    assert.match(noGate.message, /at least one gate/);
  });
});

describe("uploads", () => {
  it("uploaded decisions are verified by replay; their rewards apply", async () => {
    const c = client("uploads");
    await c.putSpec({ actions: [{ id: "only" }] });
    const published = await c.model({ snapshot: true });
    assert.equal(published.decide.version, 1);
    assert.deepEqual(published.decide.actions, [{ id: "only" }]);
    assert.match(published.modelTag, /^[0-9a-f]{16}$/);
    assert.ok(published.snapshotBytes > 0);
    assert.equal(typeof published.snapshot, "string");
    assert.equal(await c.model({ snapshot: true, ifNoneMatch: published.modelTag }), null);
    // The same answer as text, unparsed, for LocalDecider.
    const text = await c.modelText();
    assert.deepEqual(JSON.parse(text ?? ""), published);
    assert.equal(await c.modelText({ ifNoneMatch: published.modelTag }), null);
    await httpError(client("no-such-capsule").modelText(), 404);

    // With one action the draw is certain whatever the seed, so this
    // decision replays on the server.
    const made: UploadedDecision = {
      decisionId: "loc_ts_1",
      tsMs: Date.now(),
      modelTag: published.modelTag,
      modelVersion: published.modelVersion,
      seed: "18446744073709551615",
      input: { context: { user: 1 } },
      chosenIndex: 0,
      probability: 1,
      pmf: [1],
      eligible: [0],
    };
    const forged = { ...made, decisionId: "loc_ts_2", probability: 0.5, pmf: [0.5] };
    const upload = await c.uploadDecisions([made, forged]);
    assert.equal(upload.accepted, 1);
    assert.equal(upload.duplicates, 0);
    assert.equal(upload.rejected.length, 1);
    const [rejection] = upload.rejected;
    assert.equal(rejection?.index, 1);
    assert.equal(rejection?.decisionId, "loc_ts_2");
    assert.equal(rejection?.retryable, false);
    assert.match(rejection?.error ?? "", /does not replay/);

    assert.deepEqual(await c.uploadDecisions([made]), { accepted: 1, duplicates: 1, unverified: 0, rejected: [] });

    const rewards = await c.uploadRewards([
      { decisionId: "loc_ts_1", reward: 1, idempotencyKey: "r-1" },
      { decisionId: "loc_ts_missing", reward: 1 },
    ]);
    assert.deepEqual(rewards.results[0], { ok: true, applied: true, learned: true, modelVersion: 1 });
    const failed = rewards.results[1];
    assert.ok(failed !== undefined && !failed.ok);
    assert.equal(failed.status, 404);

    const stored = await c.decision("loc_ts_1");
    assert.equal(stored.seed, "18446744073709551615");
    assert.equal(stored.probability, 1);
    assert.equal(stored.rewards.length, 1);
  });
});

describe("tokens", () => {
  it("a read token decides and rewards but cannot change the spec", async () => {
    const admin = client("scoped");
    await admin.putSpec({ actions: ACTIONS });
    assert.deepEqual(await admin.whoami(), {
      ok: true,
      kind: "legacy_admin",
      principalId: "operator",
      scope: { kind: "admin" },
    });

    const scope = { kind: "read", tenant: "acme", job: "prod", capsule: "scoped" } as const;
    const issued = await admin.issueToken(scope, "ts-sdk-test", 3600);
    assert.match(issued.token, /^[0-9a-f]{64}$/);
    assert.deepEqual(issued.scope, scope);
    assert.ok(issued.expiresAt !== null && issued.expiresAt > Date.now() / 1000);

    const reader = client("scoped", issued.token);
    const me = await reader.whoami();
    assert.equal(me.kind, "scoped_token");
    assert.equal(me.principalId, issued.hash);
    const d = await reader.decide({ user: 1 });
    assert.equal((await reader.reward(d.decisionId, 1)).applied, true);
    assert.equal((await reader.decision(d.decisionId)).rewards.length, 1);

    const denied = await httpError(reader.putSpec({ exploration: { floor: 0.1 } }), 403);
    assert.equal(
      denied.message,
      "PUT /v1/tenants/acme/jobs/prod/capsules/scoped/spec: HTTP 403: forbidden: scope does not allow this action",
    );
    assertNoToken(denied, issued.token);
    // Another capsule is out of its scope.
    await httpError(client("elsewhere", issued.token).decide({}), 403);

    const listed = await admin.listTokens();
    assert.equal(listed.tokens.find((t) => t.hash === issued.hash)?.label, "ts-sdk-test");
    assert.ok(!JSON.stringify(listed).includes(issued.token));

    assert.deepEqual(await admin.revokeToken(issued.hash), { ok: true, revoked: true });
    await httpError(reader.decide({}), 401);
    await httpError(admin.revokeToken(issued.hash), 404);
  });

  it("health needs no credential", async () => {
    const anonymous = client("anything", "");
    assert.equal((await anonymous.health()).ok, true);
    await httpError(anonymous.whoami(), 401);
  });
});

describe("errors", () => {
  it("401, 404 and 400 carry the status and the server's message, never the token", async () => {
    const wrong = "not-the-admin-key-0123456789";
    const unauthorized = await httpError(client("errors", wrong).decide({}), 401);
    assert.equal(unauthorized.message, "POST /v1/tenants/acme/jobs/prod/capsules/errors/decide: HTTP 401: unauthorized");
    assert.equal(unauthorized.method, "POST");
    assert.equal(unauthorized.path, "/v1/tenants/acme/jobs/prod/capsules/errors/decide");
    assert.deepEqual(unauthorized.body, { error: "unauthorized" });
    assert.match(unauthorized.requestId ?? "", /^[0-9a-f]{16}$/);
    assert.ok(unauthorized instanceof SyntraError);
    assertNoToken(unauthorized, wrong);

    const c = client("errors");
    const noCapsule = await httpError(c.decide({}), 404);
    assert.match(noCapsule.message, /capsule acme\/prod\/errors not found/);
    await c.putSpec({ actions: ACTIONS });
    const noDecision = await httpError(c.decision("dec_missing"), 404);
    assert.match(noDecision.message, /decision "dec_missing" not found/);

    const unknownField = await httpError(c.putSpec({ bogus: true } as never), 400);
    assert.match(
      unknownField.message,
      /^PUT \/v1\/tenants\/acme\/jobs\/prod\/capsules\/errors\/spec: HTTP 400: invalid spec: unknown field `bogus`/,
    );
    assertNoToken(unknownField, ADMIN_KEY);
    const notAnObject = await httpError(c.decide([] as never), 400);
    assert.match(notAnObject.message, /context must be a JSON object/);
    assertNoToken(notAnObject, ADMIN_KEY);
  });

  it("no answer is a TransportError", async () => {
    const port = await freePort(); // nothing listens there
    const c = new SyntraClient({
      url: `http://127.0.0.1:${port}`,
      token: ADMIN_KEY,
      tenant: "acme",
      job: "prod",
      capsule: "errors",
      maxRetries: 0,
    });
    await assert.rejects(c.getSpec(), (err: unknown) => {
      assert.ok(err instanceof TransportError);
      assert.ok(err instanceof SyntraError);
      assert.equal(err.timedOut, false);
      assert.match(err.message, /^GET \/v1\/tenants\/acme\/jobs\/prod\/capsules\/errors\/spec: /);
      assertNoToken(err, ADMIN_KEY);
      return true;
    });
  });

  it("bad arguments are TypeErrors, raised before any request", async () => {
    const c = client("errors");
    await assert.rejects(c.decision(".."), TypeError);
    await assert.rejects(c.reward("dec_x", Number.NaN), TypeError);
    await assert.rejects(c.decide({}, { exclude: "large" as never }), TypeError);
    assert.throws(
      () => new SyntraClient({ url: "ftp://127.0.0.1", token: "", tenant: "a", job: "b", capsule: "c" }),
      TypeError,
    );
  });
});
