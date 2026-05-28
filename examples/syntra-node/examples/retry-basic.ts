/**
 * retry-basic.ts — minimal usage demo for @ashhart/syntra-client.
 *
 * Run (after building):
 *   node --loader ts-node/esm examples/retry-basic.ts
 *
 * Or compile first:
 *   npm run build
 *   node dist/examples/retry-basic.js   # (copy example to src/ first)
 *
 * Requires a running Syntra instance and a capsule installed at CAPSULE_PATH.
 * Apache-2.0
 */

import { RetryClient } from "../src/index.js";

const SYNTRA_URL = process.env["SYNTRA_URL"] ?? "http://localhost:8787";
const ADMIN_KEY = process.env["SYNTRA_ADMIN_KEY"] ?? "dev-key";
const CAPSULE_PATH =
  process.env["SYNTRA_CAPSULE_PATH"] ??
  "/tenants/myteam/jobs/retry/capsules/router";
const TARGET_URL = process.env["TARGET_URL"] ?? "https://httpbin.org/get";

const client = new RetryClient({
  baseUrl: SYNTRA_URL,
  adminKey: ADMIN_KEY,
  capsulePath: CAPSULE_PATH,
  timeoutMs: 3000,
  onFeedbackError: (err) => {
    console.warn("Feedback error (non-fatal):", err);
  },
});

console.log(`Sending request to ${TARGET_URL} via RetryClient...`);

const response = await client.request("GET", TARGET_URL);

console.log(`Response status: ${response.status}`);

if (response.ok) {
  const body = await response.json();
  console.log("Response body (truncated):", JSON.stringify(body).slice(0, 200));
} else {
  console.warn("Request did not succeed with 2xx status.");
}
