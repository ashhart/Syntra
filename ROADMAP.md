# Roadmap

What works today, and its limits, is in the README ("Status and limits").
Next, roughly in order. Nothing here is promised by a date.

## Decide in-process everywhere

- **JavaScript and TypeScript.** A WebAssembly build of the Rust decision
  core, so the TypeScript SDK's `LocalDecider` decides in-process in Node,
  Deno, browsers and edge workers, with the same replay-verified uploads as
  the Rust and Python SDKs.
- **Go and the JVM.** In-process deciders that match the Rust core on its
  conformance vectors, or HTTP clients first.
- **LiteLLM.** Check `syntra.llm.ModelRouter` against LiteLLM releases in
  CI.

## Scale and availability

- **Several servers on one log.** A Postgres event store, so decide nodes
  can share decisions, rewards and models; today one store has one server.
- **Continuous replication** of a store. Today `syntra backup` takes a
  consistent online copy, which a schedule can run.

## Evidence

- **Public benchmarks** against Vowpal Wabbit and the Open Bandit
  Pipeline on public datasets, with the hardware and method published.
- **Richer reward models** where the linear model is the limit (the
  catalog row of the README's learning table).

## Security hardening before 1.0

- An external security review, including the admin console.
- The capability sandbox behind an OS boundary (it runs in the server
  process today), and `max_memory_bytes` enforced.
- Field-level encryption or redaction for stored contexts.

The current gaps are listed in [SECURITY.md](SECURITY.md).
