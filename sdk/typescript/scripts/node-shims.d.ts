/**
 * Minimal ambient typings for the Node builtins used by `scripts/`.
 * Keeps the SDK's devDependencies to just `typescript` (no @types/node);
 * the runtime is a real Node >= 22 and these declarations describe the
 * subset of its API the smoke test touches.
 */

declare module "node:child_process" {
  export interface ChildProcess {
    readonly pid?: number;
    kill(signal?: string): boolean;
    once(event: "exit", listener: (code: number | null, signal: string | null) => void): ChildProcess;
    once(event: "error", listener: (err: Error) => void): ChildProcess;
  }
  export interface SpawnOptions {
    cwd?: string;
    stdio?: "pipe" | "inherit" | "ignore" | Array<"pipe" | "inherit" | "ignore">;
  }
  export function spawn(command: string, args?: readonly string[], options?: SpawnOptions): ChildProcess;
  export interface SpawnSyncResult {
    status: number | null;
    signal: string | null;
    error?: Error;
  }
  export function spawnSync(command: string, args?: readonly string[], options?: SpawnOptions): SpawnSyncResult;
}

declare module "node:fs" {
  export function readFileSync(path: string): Uint8Array;
  export function existsSync(path: string): boolean;
  export function mkdtempSync(prefix: string): string;
  export function rmSync(path: string, options?: { recursive?: boolean; force?: boolean }): void;
}

declare module "node:os" {
  export function tmpdir(): string;
}

declare module "node:path" {
  export function join(...parts: string[]): string;
  export function dirname(path: string): string;
}

declare module "node:url" {
  export function fileURLToPath(url: string | URL): string;
}

declare module "node:net" {
  export interface AddressInfo {
    port: number;
    family: string;
    address: string;
  }
  export interface Server {
    listen(port: number, host: string, onListening?: () => void): Server;
    address(): AddressInfo | string | null;
    close(onClose?: () => void): Server;
  }
  export function createServer(): Server;
}

declare var process: {
  argv: readonly string[];
  env: Record<string, string | undefined>;
  platform: string;
  exit(code?: number): never;
  exitCode: number | undefined;
};
