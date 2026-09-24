# Roadmap

What works today, and its limits, is in the README ("Status and limits").
Next, roughly in order. Nothing here is promised by a date.

## Decide in-process everywhere

- **Edge runtimes.** The TypeScript SDK decides in-process through a
  WebAssembly build of the Rust core, tested in Node, Bun and Chromium;
  Deno and edge workers (Cloudflare Workers and the like) are untested.
- **Go and the JVM.** In-process deciders on the same WebAssembly core or
  matching the Rust core on its conformance vectors, or HTTP clients
  first.

## Scale and availability

- **Several servers on one log.** A Postgres event store, so decide nodes
  can share decisions, rewards and models; today one store has one server.
- **Continuous replication** of a store. Today `syntra backup` takes a
  consistent online copy, which a schedule can run.

## Evidence

- **Real logged data.** The comparisons with Vowpal Wabbit and the Open
  Bandit Pipeline ([benchmarks/](benchmarks/README.md)) use simulated and
  synthetic data; next is the full Open Bandit Dataset and other public
  logs.
- **Richer reward models** where the linear model is the limit (the
  catalog row of the README's learning table), and a DM interval that
  includes the model's own uncertainty (a bootstrap that refits it).

## Security hardening before 1.0

- An external security review, including the admin console.
- The capability sandbox behind an OS boundary (it runs in the server
  process today), and `max_memory_bytes` enforced.
- Field-level encryption or redaction for stored contexts.

The current gaps are listed in [SECURITY.md](SECURITY.md).
