/**
 * In-process decide latency of `LocalDecider` in Node, in the scenario of
 * the Rust benchmark `examples/bench_local.rs`: capsule `bench/bench/local`
 * with three actions, trained on 2000 decisions, then timed `decide`
 * calls (each includes queueing the upload item), then one `flush`.
 *
 *   cargo build --bin syntra
 *   npm run bench                  # or: node bench/local-decide.ts --decisions 100000
 *
 * It starts `target/debug/syntra` (or `$SYNTRA_BIN`) on a free port with a
 * temporary store. Numbers depend on the machine, the Node version and
 * garbage collection; report them with the hardware. The upload rate
 * depends on the server build: a release binary verifies several times
 * faster than the debug one.
 */

import { arch, cpus, platform } from "node:os";

import { LocalDecider, SyntraClient } from "../src/index.ts";
import { ADMIN_KEY, TestServer } from "../test/server.ts";

function argument(name: string, fallback: number): number {
  const at = process.argv.indexOf(name);
  return at === -1 ? fallback : Number(process.argv[at + 1]);
}

const decisions = argument("--decisions", 100_000);
const warmup = argument("--warmup", 20_000);

function percentile(sorted: Float64Array, p: number): number {
  return sorted[Math.min(sorted.length - 1, Math.round((p / 100) * (sorted.length - 1)))] ?? 0;
}

const server = await TestServer.start();
try {
  const where = { url: server.url, token: ADMIN_KEY, tenant: "bench", job: "bench", capsule: "local" };
  await new SyntraClient(where).putSpec({
    actions: [
      { id: "small", features: { cost: 0.1 } },
      { id: "medium", features: { cost: 0.4 } },
      { id: "large", features: { cost: 1.0 } },
    ],
  });
  const decider = await LocalDecider.connect({ ...where, syncIntervalMs: 0, maxQueue: 10_000_000 });
  const tasks = ["chat", "code", "extract", "summarize"];
  const tiers = ["free", "pro", "enterprise"];
  const context = (i: number) => ({
    task: tasks[i % tasks.length],
    tier: tiers[Math.floor(i / 4) % tiers.length],
    promptTokens: 50 + ((i * 37) % 4000),
  });

  // Train a little so predictions are not all zero, then sync the model.
  for (let i = 0; i < 2000; i++) {
    const d = decider.decide(context(i));
    const task = tasks[i % tasks.length];
    const good = (d.action === "large" && task === "code") || (d.action === "small" && task === "chat") || d.action === "medium";
    decider.reward(d.decisionId, good ? 1 : 0);
  }
  const warm = await decider.flush();
  if (warm.decisionsRejected !== 0) throw new Error(`warm-up uploads were rejected: ${warm.errors.join("; ")}`);
  const deadline = Date.now() + 10_000;
  while (!(await decider.sync())) {
    if (Date.now() > deadline) throw new Error("no new model was published");
    await new Promise((done) => setTimeout(done, 100));
  }

  const contexts = Array.from({ length: 4096 }, (_, i) => context(i));
  for (let i = 0; i < warmup; i++) decider.decide(contexts[i % contexts.length]);
  await decider.flush();

  const latency = new Float64Array(decisions);
  let sink = 0;
  const started = performance.now();
  for (let i = 0; i < decisions; i++) {
    const ctx = contexts[(i * 7919) % contexts.length];
    const t0 = performance.now();
    const d = decider.decide(ctx);
    latency[i] = performance.now() - t0;
    sink += d.actionIndex;
  }
  const wallMs = performance.now() - started;
  latency.sort();
  const us = (ms: number) => (ms * 1000).toFixed(2);
  console.log(`${platform()} ${arch()}, ${cpus()[0]?.model ?? "unknown CPU"}, Node ${process.version}`);
  console.log(`model version ${decider.modelVersion} (${decider.modelTag})`);
  console.log(
    `local decide (1 thread, ${decisions} decisions after ${warmup} warm-up): ` +
      `p50 ${us(percentile(latency, 50))} us  p90 ${us(percentile(latency, 90))} us  ` +
      `p99 ${us(percentile(latency, 99))} us  p99.9 ${us(percentile(latency, 99.9))} us  ` +
      `max ${us(latency[latency.length - 1] ?? 0)} us`,
  );
  console.log(`throughput: ${Math.round(decisions / (wallMs / 1000))} decisions/s (checksum ${sink})`);

  const t0 = performance.now();
  const report = await decider.flush();
  const seconds = (performance.now() - t0) / 1000;
  console.log(
    `upload: ${report.decisionsAccepted} accepted, ${report.decisionsRejected} rejected in ${seconds.toFixed(2)} s ` +
      `(${Math.round(report.decisionsAccepted / seconds)} verified decisions/s)`,
  );
  await decider.close();
} finally {
  await server.stop();
}
