/**
 * Real end-to-end smoke test for @syntra/client.
 *
 * Spawns a genuine `syntra serve` appliance (random port, throwaway store,
 * admin key), then exercises the SDK against it:
 *   health → createJob → installCapsule(demo .lyc) → createToken(read) →
 *   decide (read token) → learn=true downgrade → feedback 403 on read token →
 *   admin feedback → report/contexts/memory/decisions → metrics → 401 → 404.
 *
 * Run: npm run build && node dist/scripts/smoke.js   (exits non-zero on failure)
 */

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:net";

import {
  AuthError,
  ForbiddenError,
  NotFoundError,
  SyntraApiError,
  SyntraClient,
} from "../src/index.js";

const TENANT = "smoke-tenant";
const JOB = "llm-routing";
const CAPSULE = "model-router";

function check(condition: unknown, label: string): void {
  if (!condition) {
    throw new Error(`CHECK FAILED: ${label}`);
  }
  console.log(`  ok  ${label}`);
}

async function expectError<TError extends SyntraApiError>(
  call: () => Promise<unknown>,
  label: string,
  isExpected: (err: unknown) => err is TError,
): Promise<TError> {
  let err: unknown = undefined;
  try {
    await call();
  } catch (caught) {
    err = caught;
  }
  if (err === undefined) throw new Error(`CHECK FAILED: ${label} (call unexpectedly succeeded)`);
  if (!isExpected(err)) {
    throw new Error(
      `CHECK FAILED: ${label} (expected typed SyntraApiError, got ${
        err instanceof Error ? `${err.name}: ${err.message}` : String(err)
      })`,
    );
  }
  console.log(`  ok  ${label} [${err.name} ${err.status}]`);
  return err;
}

/** Walk up from the compiled script location to the repo root. */
function findRepoRoot(): string {
  let dir = dirname(fileURLToPath(import.meta.url));
  for (let depth = 0; depth < 8; depth += 1) {
    if (existsSync(join(dir, "Cargo.toml")) && existsSync(join(dir, "examples"))) return dir;
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  throw new Error("could not locate Syntra repo root from " + import.meta.url);
}

function ensureBinary(repoRoot: string): string {
  const binary = join(repoRoot, "target", "release", "syntra");
  if (existsSync(binary)) {
    console.log(`binary: ${binary} (already built)`);
    return binary;
  }
  console.log("binary: target/release/syntra missing — building (cargo build --release --bin syntra)…");
  const build = spawnSync("cargo", ["build", "--release", "--bin", "syntra"], {
    cwd: repoRoot,
    stdio: "inherit",
  });
  if (build.error) throw new Error(`cargo build failed to launch: ${build.error.message}`);
  if (build.status !== 0) throw new Error(`cargo build failed with status ${build.status}`);
  if (!existsSync(binary)) throw new Error(`cargo build succeeded but ${binary} still missing`);
  return binary;
}

async function pickFreePort(): Promise<number> {
  const { promise, resolve } = Promise.withResolvers<number>();
  const probe = createServer();
  probe.listen(0, "127.0.0.1", () => {
    const addr = probe.address();
    const port = typeof addr === "object" && addr !== null ? addr.port : 0;
    probe.close(() => resolve(port));
  });
  return promise;
}

async function waitForHealth(client: SyntraClient, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const h = await client.health();
      if (h.ok === true) return;
    } catch {
      // not up yet — retry until the deadline
    }
    if (Date.now() > deadline) throw new Error("server never became healthy within deadline");
    const { promise, resolve } = Promise.withResolvers<void>();
    setTimeout(resolve, 250);
    await promise;
  }
}

async function main(): Promise<void> {
  const repoRoot = findRepoRoot();
  const binary = ensureBinary(repoRoot);
  const lycPath = join(repoRoot, "examples", "demo_llm_model_router.lyc");
  if (!existsSync(lycPath)) throw new Error(`demo capsule missing: ${lycPath}`);

  const port = await pickFreePort();
  const storeDir = mkdtempSync(join(tmpdir(), "syntra-ts-smoke-"));
  const adminKey = `smoke-${crypto.randomUUID().replace(/-/g, "")}`;
  const baseUrl = `http://127.0.0.1:${port}`;

  console.log(`serve: ${binary} --addr 127.0.0.1:${port} --store ${storeDir}`);
  const server = spawn(
    binary,
    ["serve", "--addr", `127.0.0.1:${port}`, "--store", storeDir, "--admin-key", adminKey],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  server.once("error", (err) => {
    throw new Error(`failed to spawn syntra serve: ${err.message}`);
  });

  try {
    const probe = new SyntraClient({ baseUrl, token: "" });
    await waitForHealth(probe, 15_000);
    console.log("  ok  server healthy (GET /health)");

    const admin = new SyntraClient({ baseUrl, token: adminKey, timeoutMs: 5_000 });
    check(
      !admin.toString().includes(adminKey) && admin.toString().includes("\u2026"),
      "client.toString() masks the admin key",
    );

    // 1. Job + capsule install (raw .lyc bytes, octet-stream).
    const job = await admin.createJob(TENANT, { id: JOB, name: "LLM Routing" });
    check(job.ok === true && job.tenant === TENANT, "createJob ok");

    const install = await admin.installCapsule(TENANT, JOB, CAPSULE, readFileSync(lycPath));
    check(install.ok === true, "installCapsule ok");
    check(/^[0-9a-f]{64}$/.test(install.hash), "install hash is SHA-256 hex");
    check((await admin.report(TENANT, JOB, CAPSULE)).hash === install.hash, "report graph hash matches install");

    // 2. Scoped read token (serving path).
    const issued = await admin.createToken({
      scope: { kind: "read", tenant: TENANT, job: JOB, capsule: CAPSULE },
      label: "ts-smoke-read",
      ttlSeconds: 3600,
    });
    check(typeof issued.token === "string" && issued.token.length > 0, "createToken returns raw token");
    check(/^[0-9a-f]{64}$/.test(issued.hash), "createToken returns token hash");
    check(issued.expiresAt !== null && issued.expiresAt > Date.now() / 1000, "expiresAt honored");

    const reader = new SyntraClient({ baseUrl, token: issued.token });

    // 3. Decide on the read token; must be read-only shadow.
    const decision = await reader.decide(TENANT, JOB, CAPSULE, {
      contextKey: "support-low-cost",
    });
    check(decision.ok === true, "decide ok");
    check(/^dec_[0-9a-f]{16}$/.test(decision.decisionId), "decisionId format dec_<16hex>");
    check(decision.refused === false, "decision not refused");
    check(decision.decisions.length >= 1, "decisions[] non-empty");
    const chosen = decision.decisions[0]?.chosen_option ?? -1;
    check(chosen >= 0, `chosen_option in range (got ${chosen})`);
    check(decision.learned === false, "default decide is read-only (learned=false)");
    check(decision.warmup.state === "warmup", "capsule starts in warmup lifecycle");

    // 4. learn=true is silently downgraded for read-scoped tokens.
    const learnAttempt = await reader.decide(
      TENANT,
      JOB,
      CAPSULE,
      { contextKey: "support-low-cost" },
      { learn: true },
    );
    check(learnAttempt.learned === false, "read token learn=true downgraded to learned=false");

    // 5. Read token must NOT be able to mutate the learner.
    const forbidden = await expectError(
      () => reader.feedback(TENANT, JOB, CAPSULE, { decisionId: decision.decisionId, reward: 0.85 }),
      "read-token feedback rejected",
      (err): err is ForbiddenError => err instanceof ForbiddenError && err.status === 403,
    );
    check(forbidden instanceof AuthError, "ForbiddenError is an AuthError subclass");
    check(
      !forbidden.toString().includes(issued.token),
      "error toString() masks the scoped token",
    );

    // 6. Admin feedback roundtrip.
    const fb = await admin.feedback(TENANT, JOB, CAPSULE, {
      decisionId: decision.decisionId,
      reward: 0.85,
    });
    check(fb.ok === true, "feedback ok");
    check(fb.before.length === fb.after.length && fb.before.length >= 2, "feedback before/after weight vectors match");
    check(fb.contextKey === "support-low-cost", "feedback bound to decision contextKey");

    // 7. Inspection endpoints.
    const report = await reader.report(TENANT, JOB, CAPSULE);
    check(/^[0-9a-f]{64}$/.test(report.hash), "report hash is SHA-256 hex (post-feedback graph)");
    check(report.strategies.length >= 1, "report has strategy nodes");
    check((report.strategies[0]?.options.length ?? 0) >= 2, "strategy exposes options");
    check(typeof report.algorithm === "string" || report.algorithm === null, "report algorithm field present");

    const contexts = await reader.contexts(TENANT, JOB, CAPSULE);
    check(contexts.contexts.length >= 1, "contexts row appears after feedback");
    check(
      contexts.contexts.some((row) => row.contextKey === "support-low-cost" && row.totalTries >= 1),
      "context bucket counted the feedback round",
    );

    const memory = await reader.memory(TENANT, JOB, CAPSULE);
    check(typeof memory === "object" && memory !== null, "memory sidecar is an object");

    const log = await reader.decisions(TENANT, JOB, CAPSULE);
    check(log.length >= 2, "decision log has both decides");
    check(log.some((entry) => entry.id === decision.decisionId), "decision log contains our decisionId");

    const metrics = await admin.metrics();
    check(metrics.includes("syntra_requests_total"), "metrics expose syntra_requests_total");

    // 8. Auth failures are typed.
    const bogus = new SyntraClient({ baseUrl, token: "definitely-not-a-token" });
    await expectError(
      () => bogus.decide(TENANT, JOB, CAPSULE, { contextKey: "x" }),
      "bad token rejected with 401",
      (err): err is AuthError => err instanceof AuthError && !(err instanceof ForbiddenError) && err.status === 401,
    );

    // 9. Unknown capsule is a typed 404.
    await expectError(
      () => admin.report(TENANT, JOB, "no-such-capsule"),
      "unknown capsule 404s",
      (err): err is NotFoundError => err instanceof NotFoundError && err.status === 404,
    );

    console.log("\nSMOKE PASSED: @syntra/client roundtrip green against live syntra serve");
  } finally {
    server.kill("SIGKILL");
    rmSync(storeDir, { recursive: true, force: true });
  }
}

main().catch((err: unknown) => {
  console.error(err instanceof Error ? err.stack ?? err.message : String(err));
  process.exitCode = 1;
});
