/**
 * The Rust decision core, compiled to WebAssembly (`wasm/` holds the
 * crate; `npm run build:wasm` generates `src/wasm/`).
 *
 * The module is instantiated once per process, on the first
 * `LocalDecider.connect`. By default its bytes come from next to this file:
 * read with `node:fs` when that is a `file:` URL (Node, Deno, Bun), and
 * fetched otherwise (browsers, through a bundler that follows
 * `new URL(..., import.meta.url)`). Runtimes that allow neither, or that
 * refuse to compile WebAssembly from bytes (edge workers), pass the module
 * themselves as `wasm`.
 */

import initWasm from "./wasm/syntra_wasm.js";

/**
 * The compiled decision core, or where to get it. Edge runtimes turn an
 * imported `.wasm` file into a `WebAssembly.Module`; the module's bytes, a
 * URL to fetch and a `fetch` response work too.
 */
export type WasmSource = WebAssembly.Module | BufferSource | URL | string | Response | Promise<Response>;

let loading: Promise<void> | undefined;

/** Instantiate the decision core once; later calls wait for the first. */
export function loadCore(source?: WasmSource): Promise<void> {
  loading ??= instantiate(source).catch((err: unknown) => {
    // A failed load (a network error, say) may succeed next time.
    loading = undefined;
    throw err;
  });
  return loading;
}

async function instantiate(source: WasmSource | undefined): Promise<void> {
  if (source !== undefined) {
    await initWasm({ module_or_path: source });
    return;
  }
  const url = new URL("./wasm/syntra_wasm_bg.wasm", import.meta.url);
  if (url.protocol === "file:") {
    // Node's fetch does not read file: URLs. The specifier is a variable
    // so that bundlers for the browser leave this import alone.
    const fs = "node:fs/promises";
    const { readFile } = (await import(/* webpackIgnore: true */ /* @vite-ignore */ fs)) as {
      readFile(path: URL): Promise<Uint8Array>;
    };
    await initWasm({ module_or_path: await readFile(url) });
    return;
  }
  await initWasm({ module_or_path: url });
}
