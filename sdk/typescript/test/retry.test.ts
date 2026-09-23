/**
 * Retry behavior against a scripted server built on node:http: which calls
 * are retried, how long the client waits, and when it gives up.
 */

import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { describe, it } from "node:test";

import { HttpError, SyntraClient, TransportError } from "../src/index.ts";
import type { SyntraClientOptions } from "../src/index.ts";

const TOKEN = "retry-test-token-7d1e44c9";

interface Seen {
  method: string;
  url: string;
  authorization: string | undefined;
  body: string;
}

type Answer = (res: ServerResponse, req: IncomingMessage) => void;

interface FakeServer {
  url: string;
  seen: Seen[];
  close(): Promise<void>;
}

/** Answers its nth request with `answers[n]`; the last answer repeats. */
async function fakeServer(...answers: Answer[]): Promise<FakeServer> {
  const seen: Seen[] = [];
  const server = createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk: string) => {
      body += chunk;
    });
    req.on("end", () => {
      seen.push({ method: req.method ?? "", url: req.url ?? "", authorization: req.headers.authorization, body });
      const answer = answers[Math.min(seen.length, answers.length) - 1];
      answer?.(res, req);
    });
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}`,
    seen,
    close: () =>
      new Promise((done) => {
        server.closeAllConnections();
        server.close(() => done());
      }),
  };
}

function json(status: number, body: unknown, headers: Record<string, string> = {}): Answer {
  return (res) => {
    res.writeHead(status, { "content-type": "application/json", ...headers });
    res.end(JSON.stringify(body));
  };
}

const hangUp: Answer = (_res, req) => req.socket.destroy();
const never: Answer = () => {};

function client(url: string, options: Partial<SyntraClientOptions> = {}): SyntraClient {
  return new SyntraClient({ url, token: TOKEN, tenant: "acme", job: "prod", capsule: "router", ...options });
}

/** Runs `body` against a fake server and closes it afterwards. */
async function withServer(answers: Answer[], body: (fake: FakeServer) => Promise<void>): Promise<void> {
  const fake = await fakeServer(...answers);
  try {
    await body(fake);
  } finally {
    await fake.close();
  }
}

async function rejection<T extends Error>(
  promise: Promise<unknown>,
  type: new (...args: never[]) => T,
): Promise<T> {
  try {
    await promise;
  } catch (err) {
    assert.ok(err instanceof type, `expected ${type.name}, got ${String(err)}`);
    return err;
  }
  assert.fail(`expected ${type.name}, but the call succeeded`);
}

const SPEC = { actions: [{ id: "a" }], mode: "learner" };
const DECISION = {
  decisionId: "evt-1",
  action: "a",
  actionIndex: 0,
  probability: 1,
  ranking: [{ id: "a", probability: 1 }],
  mode: "learner",
  modelVersion: 0,
};
const REWARDED = { ok: true, applied: true, learned: true, modelVersion: 1 };
const BUSY = { error: "event log is backlogged; retry shortly" };

describe("retries", () => {
  it("a GET waits out Retry-After on a 503, then succeeds", async () => {
    await withServer([json(503, BUSY, { "retry-after": "1" }), json(200, SPEC)], async (fake) => {
      const started = performance.now();
      assert.deepEqual(await client(fake.url).getSpec(), SPEC);
      const waited = performance.now() - started;
      assert.ok(waited >= 950, `retried after ${waited} ms`);
      assert.equal(fake.seen.length, 2);
      for (const request of fake.seen) {
        assert.equal(request.method, "GET");
        assert.equal(request.url, "/v1/tenants/acme/jobs/prod/capsules/router/spec");
        assert.equal(request.authorization, `Bearer ${TOKEN}`);
      }
    });
  });

  it("a 429 is retried too", async () => {
    const limited = json(429, { error: "rate limit exceeded", retryAfterSeconds: 0 }, { "retry-after": "0" });
    await withServer([limited, json(200, SPEC)], async (fake) => {
      assert.deepEqual(await client(fake.url).getSpec(), SPEC);
      assert.equal(fake.seen.length, 2);
    });
  });

  it("without Retry-After the client backs off and retries", async () => {
    await withServer([json(503, BUSY), json(200, SPEC)], async (fake) => {
      assert.deepEqual(await client(fake.url).getSpec(), SPEC);
      assert.equal(fake.seen.length, 2);
    });
  });

  it("a decide with an eventId is retried with the same body", async () => {
    await withServer([json(503, BUSY, { "retry-after": "0" }), json(200, DECISION)], async (fake) => {
      const d = await client(fake.url).decide({ user: 1 }, { eventId: "evt-1", durable: true });
      assert.equal(d.decisionId, "evt-1");
      assert.equal(fake.seen.length, 2);
      assert.equal(fake.seen[0]?.body, fake.seen[1]?.body);
      assert.deepEqual(JSON.parse(fake.seen[0]?.body ?? ""), {
        context: { user: 1 },
        eventId: "evt-1",
        durable: true,
      });
    });
  });

  it("a decide without an eventId is sent once", async () => {
    await withServer([json(503, BUSY, { "retry-after": "0" }), json(200, DECISION)], async (fake) => {
      const err = await rejection(client(fake.url).decide({ user: 1 }), HttpError);
      assert.equal(err.status, 503);
      assert.equal(err.retryAfterMs, 0);
      assert.deepEqual(err.body, BUSY);
      assert.equal(fake.seen.length, 1);
    });
  });

  it("a reward is retried only with an idempotency key", async () => {
    const answers = [json(503, BUSY, { "retry-after": "0" }), json(200, REWARDED)];
    await withServer(answers, async (fake) => {
      assert.deepEqual(await client(fake.url).reward("dec_1", 1, { idempotencyKey: "k-1" }), REWARDED);
      assert.equal(fake.seen.length, 2);
    });
    await withServer(answers, async (fake) => {
      assert.equal((await rejection(client(fake.url).reward("dec_1", 1), HttpError)).status, 503);
      assert.equal(fake.seen.length, 1);
    });
  });

  it("decision uploads are retried; reward uploads only when every item has a key", async () => {
    const answers = [json(503, BUSY, { "retry-after": "0" }), json(200, { accepted: 0, duplicates: 0, rejected: [] })];
    await withServer(answers, async (fake) => {
      await client(fake.url).uploadDecisions([]);
      assert.equal(fake.seen.length, 2);
      assert.equal(fake.seen[0]?.url, "/v1/tenants/acme/jobs/prod/capsules/router/decisions:batch");
    });
    const rewards = [json(503, BUSY, { "retry-after": "0" }), json(200, { results: [] })];
    await withServer(rewards, async (fake) => {
      await client(fake.url).uploadRewards([{ decisionId: "d1", reward: 1, idempotencyKey: "k1" }]);
      assert.equal(fake.seen.length, 2);
    });
    await withServer(rewards, async (fake) => {
      const unkeyed = client(fake.url).uploadRewards([
        { decisionId: "d1", reward: 1, idempotencyKey: "k1" },
        { decisionId: "d2", reward: 1 },
      ]);
      assert.equal((await rejection(unkeyed, HttpError)).status, 503);
      assert.equal(fake.seen.length, 1);
    });
  });

  it("a dropped connection is retried for a GET, not for a plain POST", async () => {
    await withServer([hangUp, json(200, SPEC)], async (fake) => {
      assert.deepEqual(await client(fake.url).getSpec(), SPEC);
      assert.equal(fake.seen.length, 2);
    });
    await withServer([hangUp, json(200, {})], async (fake) => {
      const err = await rejection(client(fake.url).evaluate({ policy: "greedy" }), TransportError);
      assert.equal(err.timedOut, false);
      assert.equal(err.method, "POST");
      assert.equal(fake.seen.length, 1);
    });
  });

  it("gives up after maxRetries", async () => {
    await withServer([json(503, BUSY, { "retry-after": "0" })], async (fake) => {
      const err = await rejection(client(fake.url, { maxRetries: 2 }).getSpec(), HttpError);
      assert.equal(err.status, 503);
      assert.equal(fake.seen.length, 3);
    });
  });

  it("does not wait out a Retry-After longer than maxRetryDelayMs", async () => {
    const limited = json(429, { error: "rate limit exceeded", retryAfterSeconds: 120 }, { "retry-after": "120" });
    await withServer([limited, json(200, SPEC)], async (fake) => {
      const started = performance.now();
      const err = await rejection(client(fake.url).getSpec(), HttpError);
      assert.ok(performance.now() - started < 1000);
      assert.equal(err.status, 429);
      assert.equal(err.retryAfterMs, 120_000);
      assert.equal(fake.seen.length, 1);
    });
    const inAnHour = new Date(Date.now() + 3_600_000).toUTCString();
    await withServer([json(503, BUSY, { "retry-after": inAnHour }), json(200, SPEC)], async (fake) => {
      const err = await rejection(client(fake.url).getSpec(), HttpError);
      assert.ok(err.retryAfterMs !== undefined && Math.abs(err.retryAfterMs - 3_600_000) < 5_000);
      assert.equal(fake.seen.length, 1);
    });
  });

  it("an attempt that gets no answer in time is a TransportError", async () => {
    await withServer([never], async (fake) => {
      const err = await rejection(client(fake.url, { timeoutMs: 200, maxRetries: 0 }).getSpec(), TransportError);
      assert.equal(err.timedOut, true);
      assert.equal(err.message, "GET /v1/tenants/acme/jobs/prod/capsules/router/spec: no response within 200 ms");
      assert.ok(!String(err).includes(TOKEN));
    });
  });

  it("redirects are not followed", async () => {
    const redirect: Answer = (res) => {
      res.writeHead(307, { location: "/elsewhere" });
      res.end();
    };
    await withServer([redirect, json(200, {})], async (fake) => {
      const err = await rejection(client(fake.url).evaluate({ policy: "greedy" }), HttpError);
      assert.equal(err.status, 307);
      assert.equal(fake.seen.length, 1);
    });
  });
});
