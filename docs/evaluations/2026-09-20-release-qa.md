# Release QA, 20 September 2026

Syntra and its embedded Lycan runtime execute the advertised demo workflows.
The final release-mode Rust run passed **595 tests**, with zero failures and one
deliberately ignored fixture-regeneration test.
This is functional release evidence, not a production security certification or
validation of the scientific models for operational use.

## Reproduce the checks

```bash
cargo build --locked --release
cargo test --locked --release -- --test-threads=1
python3 scripts/demo-science.py --no-build
bash examples/lycan-internals/showcase/run-all.sh
python3 sdk/python/tests/smoke.py
cd sdk/typescript && npm ci --ignore-scripts && npm run smoke
```

The science runner writes each program's output and a JSON receipt containing
source and output hashes, elapsed time, and numerical check results to
`target/science-demo/`.
The live showcase requires NASA/JPL access; the science runner uses bundled data
and synthetic workloads, and runs offline after dependencies have been built.

## Results

| Check | Observed result |
| --- | --- |
| Full Rust suite | 595 passed, 0 failed, 1 maintenance test ignored |
| Mars Lambert demo | 288-day flight, displayed departure C3 9.20 km²/s², within the default energy and flight-time constraints |
| Live NASA/JPL showcase | 147 Earth and 147 Mars vector records fetched; constrained mission and feedback checks passed |
| Infeasible Mars energy budget | A maximum C3 of 0.01 returns no mission and prints an explicit infeasibility result |
| Narrow flight-time regression | A requested 220–225-day interval is preserved through refinement |
| Simplified launch-window demo | Completed its Keplerian search; this approximation reports a 437-day flight and is not interchangeable with the Lambert result |
| Pandemic scoring | All five policy scores and failure counts match an independent arithmetic oracle over 120 synthetic scenarios; policy 3 has the highest score |
| Edge of chaos | Computed boundary error approximately 0.000004214, below the 0.005 test tolerance |
| Other science examples | Chaos control, Apophis propagation, grid resilience, ICU triage, antiviral scoring, planetary defense, spacecraft faults, Lorenz and Mandelbrot tests passed |
| Operational showcases | Context-specific learning, proposal acceptance/rejection, sandbox checks, and exact learned-weight persistence across restart passed |
| End-to-end demo | Governed routing, simulated trial, proof-lab refusal, receipt hashes, containment, TLS, agent governor, and rendezvous checks passed |
| Python and TypeScript SDKs | Passed against a real local server, including authorization errors and feedback |
| Containers | Appliance and demo images built on Linux; demo health, dashboard HTML/state, installation of five capsules, and unauthorized-API rejection passed |
| Helm | Chart lint passed |
| Dependencies | Root, fuzz harness, and Rust SDK lockfiles have zero reported RustSec vulnerabilities or warnings at the audit date |
| Repository checks | Formatting, whitespace checks, and reviewer-context validation passed |

## Changes made during QA

The Mars search's refinement stages could escape the requested time-of-flight
range: a 220–225-day request returned 236 days before the fix.
Both refinement stages now retain the requested flight-time and departure bounds,
and the final result gate returns no mission when the constraints are infeasible.
The regression was run failing before the fix and passing afterward.
The live showcase now checks numerical limits and an infeasible request rather
than accepting a printed heading as evidence.

The new science runner compares pandemic calculations with an independent oracle
and checks numerical launch and chaos results.
The persistence demo now compares actual stored weights before and after restart,
instead of comparing exploration weights with learned weights.
The agent-governor smoke test also requires a successful process exit.
Stale showcase commands and outdated security documentation were corrected, and
the main demo now labels synthetic outcomes accurately.

The dependency audit found a TLS handshake advisory and unsound dependencies.
The lockfile now uses patched Rustls, and YAML handling uses
[`serde_norway`](https://docs.rs/serde_norway/0.9.42/serde_norway/) instead of
the affected `serde_yml`/`libyml` chain; this also removes the affected transitive
`anyhow` version.
See the [Rustls advisory](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc)
and [RustSec's YAML advisory](https://rustsec.org/advisories/RUSTSEC-2025-0068.html).

## Publication and history

The release preserves 98 genuine prior commits, including the archived histories
beginning on 15 May 2026.
Author and committer timestamps were compared against the original repositories
for every retained commit; publication cleanup changes hashes, not development dates.
The original archives remain separate from the release candidate.
New QA and history-restoration commits use their actual dates.

Historical identities use the maintainer-approved GitHub noreply address, without
co-author trailers.
Former organization references and local machine paths were removed from the
publication history.
A workload derived from a private operational export, including its compiled
copies, was removed from historical snapshots and replaced in the current tree
with a clearly labeled synthetic workload.
Required third-party attribution remains intact.

Review included current files, hidden files, all retained Git objects, historical
metadata, graph string tables, readable binary strings, UTF-16 strings, and
base64 payloads, plus secret scanning of the tree and history.
Historical secret-scanner matches were reviewed individually: an example token
in documentation and a prose phrase were false positives; the current docs use
an unmistakable token placeholder and revised wording.
Build products, dependency caches, local stores, local Git checkpoint refs,
private audit evidence, and the original archive metadata are excluded.
No confirmed private-information or secret findings remain in the inspected
release snapshot; that statement is bounded by this inspection and is not a
guarantee that private information can never be introduced later.

## Decision performance follow-up

The [real-data decision benchmark](2026-09-20-decision-benchmark.md) adds held-out
sensor classification, delayed bandit feedback, comparison policies, and HTTP
load tests that count failures and arrival queues.
It finds fast embedded decisions, poor recall for some rare classes, and HTTP
tail latencies that exceed 1 ms under load.
Functional demo success should not be read as a hard real-time guarantee.
The follow-up full-suite run also exposed a stochastic governor persistence check;
it now compares complete learned state across restart instead of requiring the
first exploratory action to match the dominant learned policy.

## Remaining limits

The pandemic, clinical, and intervention workloads use synthetic rules and do not
establish real-world medical or public-health efficacy.
The orbital demonstrations are preliminary models, and the small proof lab emits
finite evidence and unfinished Lean skeletons rather than proofs of open theorems.
Reported timing is machine-specific; these tests do not establish hard real-time
deadlines.

The service remains a single-process runtime with a local filesystem store.
Its capability sandbox is enforced in process, and host allow-lists do not
distinguish HTTP from HTTPS; the containment demo explicitly reports this gap.
Use the deployment boundaries documented in `SECURITY.md` rather than treating
the demo as a hostile-code hosting platform.

An MCP adapter could expose the existing decide, feedback, replay, and inspection
operations to an agent client, but it would be another interface to the tested
runtime; replacing the engine with MCP would not provide its learning or execution
semantics.
No MCP adapter is claimed or shipped in this QA change.
