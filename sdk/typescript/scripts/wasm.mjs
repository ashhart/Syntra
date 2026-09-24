#!/usr/bin/env node
// Build the decision core for WebAssembly (the `wasm/` crate) and generate
// its JavaScript glue into `src/wasm/`, which is git-ignored:
//
//   node scripts/wasm.mjs build        cargo build + wasm-bindgen --target web
//   node scripts/wasm.mjs copy <dir>   copy the generated files into <dir>
//
// `npm run build` copies them into dist/wasm after tsc. Needs the
// wasm32-unknown-unknown Rust target and the wasm-bindgen CLI of exactly
// the version in wasm/Cargo.lock. CARGO_TARGET_DIR is honored.

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CRATE = join(ROOT, "wasm");
const MANIFEST = join(CRATE, "Cargo.toml");
const OUT = join(ROOT, "src", "wasm");
const NAME = "syntra_wasm";
const FILES = [`${NAME}.js`, `${NAME}.d.ts`, `${NAME}_bg.wasm`, `${NAME}_bg.wasm.d.ts`];

function fail(message) {
  console.error(`wasm.mjs: ${message}`);
  process.exit(1);
}

function output(command, args) {
  return execFileSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
}

function build() {
  // The CLI generates glue for one exact version of the wasm-bindgen crate.
  const lock = readFileSync(join(CRATE, "Cargo.lock"), "utf8");
  const want = /\[\[package\]\]\r?\nname = "wasm-bindgen"\r?\nversion = "([^"]+)"/.exec(lock)?.[1];
  if (want === undefined) fail("no wasm-bindgen in wasm/Cargo.lock");
  let have;
  try {
    have = output("wasm-bindgen", ["--version"]).trim().split(/\s+/)[1];
  } catch {
    have = undefined;
  }
  if (have !== want) {
    fail(
      `needs wasm-bindgen ${want}, found ${have ?? "none"}; install it with\n` +
        `  cargo install wasm-bindgen-cli --version ${want} --locked\n` +
        `and the Rust target with\n  rustup target add wasm32-unknown-unknown`,
    );
  }

  execFileSync(
    "cargo",
    ["build", "--release", "--locked", "--target", "wasm32-unknown-unknown", "--manifest-path", MANIFEST],
    { stdio: "inherit" },
  );
  const metadata = JSON.parse(
    output("cargo", ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", MANIFEST]),
  );
  const wasm = join(metadata.target_directory, "wasm32-unknown-unknown", "release", `${NAME}.wasm`);
  if (!existsSync(wasm)) fail(`cargo did not produce ${wasm}`);

  // Regenerate only when cargo produced a newer module.
  const generated = join(OUT, `${NAME}_bg.wasm`);
  const fresh =
    FILES.every((file) => existsSync(join(OUT, file))) && statSync(generated).mtimeMs >= statSync(wasm).mtimeMs;
  if (fresh) return;
  execFileSync("wasm-bindgen", ["--target", "web", "--out-dir", OUT, "--out-name", NAME, wasm], {
    stdio: "inherit",
  });
  console.log(`wasm.mjs: generated ${FILES.join(", ")} in ${OUT}`);
}

function copy(target) {
  if (target === undefined) fail("usage: node scripts/wasm.mjs copy <dir>");
  const dest = resolve(ROOT, target);
  mkdirSync(dest, { recursive: true });
  for (const file of FILES) {
    const from = join(OUT, file);
    if (!existsSync(from)) fail(`${from} is missing; run node scripts/wasm.mjs build first`);
    copyFileSync(from, join(dest, file));
  }
}

const [command, argument] = process.argv.slice(2);
if (command === "build") build();
else if (command === "copy") copy(argument);
else fail("usage: node scripts/wasm.mjs build | copy <dir>");
