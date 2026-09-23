/**
 * A real `syntra serve` for the end-to-end tests: a free loopback port, a
 * throwaway store under the OS temp directory, and an admin key.
 *
 * The binary is `$SYNTRA_BIN`, or `target/debug/syntra` at the repository
 * root (`cargo build --bin syntra`).
 */

import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { closeSync, existsSync, mkdtempSync, openSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
export const SYNTRA_BIN = process.env.SYNTRA_BIN ?? join(ROOT, "target", "debug", "syntra");
export const ADMIN_KEY = "ts-sdk-test-admin-key-5f0c2a9e";

export class TestServer {
  readonly url: string;
  readonly #child: ChildProcess;
  readonly #dir: string;
  readonly #onExit: () => void;

  private constructor(url: string, child: ChildProcess, dir: string) {
    this.url = url;
    this.#child = child;
    this.#dir = dir;
    // A test process that dies early must not leave the server running.
    this.#onExit = () => child.kill("SIGKILL");
    process.once("exit", this.#onExit);
  }

  static async start(): Promise<TestServer> {
    if (!existsSync(SYNTRA_BIN)) {
      throw new Error(`no server binary at ${SYNTRA_BIN}: run cargo build --bin syntra, or set SYNTRA_BIN`);
    }
    const dir = mkdtempSync(join(tmpdir(), "syntra-ts-"));
    const log = join(dir, "server.log");
    const env: Record<string, string | undefined> = {
      ...process.env,
      SYNTRA_RATE_LIMIT_RPS: "10000000",
      SYNTRA_RATE_LIMIT_BURST: "10000000",
      RUST_LOG: "warn",
    };
    delete env.SYNTRA_ADMIN_KEY;
    delete env.LYCAN_ADMIN_KEY;
    // Another process can take the port between probing and binding; try
    // a few.
    for (let tries = 0; tries < 10; tries++) {
      const addr = `127.0.0.1:${await freePort()}`;
      const logFd = openSync(log, "a");
      const child = spawn(
        SYNTRA_BIN,
        ["serve", "--addr", addr, "--store", join(dir, "store"), "--admin-key", ADMIN_KEY],
        { env, stdio: ["ignore", "ignore", logFd] },
      );
      closeSync(logFd);
      const url = `http://${addr}`;
      if (await answers(url, child)) return new TestServer(url, child, dir);
      child.kill("SIGKILL");
    }
    const output = readFileSync(log, "utf8");
    rmSync(dir, { recursive: true, force: true });
    throw new Error(`syntra serve did not start; its log:\n${output}`);
  }

  async stop(): Promise<void> {
    process.removeListener("exit", this.#onExit);
    if (this.#child.exitCode === null && this.#child.signalCode === null) {
      const exited = new Promise((done) => this.#child.once("exit", done));
      this.#child.kill("SIGKILL");
      await exited;
    }
    rmSync(this.#dir, { recursive: true, force: true });
  }
}

/** A port nothing listens on right now. */
export function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const probe = createServer();
    probe.once("error", fail);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      const port = typeof address === "object" && address !== null ? address.port : 0;
      probe.close(() => done(port));
    });
  });
}

/** True once `whoami` accepts our key: the server on the port is ours. */
async function answers(url: string, child: ChildProcess): Promise<boolean> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline && child.exitCode === null) {
    try {
      const res = await fetch(`${url}/v1/auth/whoami`, {
        headers: { authorization: `Bearer ${ADMIN_KEY}` },
        signal: AbortSignal.timeout(1000),
      });
      await res.arrayBuffer();
      if (res.status === 200) return true;
    } catch {
      // Not listening yet.
    }
    await new Promise((done) => setTimeout(done, 20));
  }
  return false;
}
