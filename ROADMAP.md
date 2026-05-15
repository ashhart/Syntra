# Syntra Roadmap

Syntra is early, so the roadmap is deliberately small. The goal is to make the adoption path clearer without pretending the appliance is already a large platform.

## 0.2 - YAML Authoring

Status: MVP landed.

- Compile simple YAML bandit specs into `.lyc` capsules with `syntra author`.
- Keep optional generated `.lycs` source for inspection.
- Turn options and context keys into executable capsule behaviour.
- Preserve declared reward weights in generated source; automatic weighted reward computation is next.
- Expand this into richer JSON/YAML capsule authoring in [#1](https://github.com/ashhart/Syntra/issues/1).

## 0.3 - Hero Demo Contract

Status: planned.

- Keep the LLM-routing demo as the primary adoption proof.
- Add CI coverage for the demo output shape.
- Re-run the demo whenever learning defaults or reward policy changes.
- Update README numbers when convergence changes materially.

## 0.4 - Operator Hardening

Status: planned.

- Improve system-level troubleshooting around logs, volumes, auth, and startup failures.
- Add more regression coverage for store persistence and failure modes.
- Continue tightening admin-console observability.

## 1.0 - Security Hardening

Status: planned.

- Complete the threat model and production-hardening track in [#2](https://github.com/ashhart/Syntra/issues/2).
- Review admin console security posture.
- Decide the multi-key/user model.
- Document deployment expectations for TLS, retention, backups, and log redaction.
