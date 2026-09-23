# Security Policy

Syntra is a self-hosted adaptive decision appliance. It is designed to run inside infrastructure you control, behind your normal edge security.

## Current Posture

- `/health`, `/ready` and the static `/admin` login shell are public. `/metrics` needs an admin credential (it names tenants and capsules) unless the server runs with `--metrics-public`.
- All data/API routes require a bearer token with the appropriate scope.
- The server refuses to start without an admin key unless `--dev-mode` is explicitly used. Dev mode binds only loopback addresses; a non-loopback bind additionally needs `--dev-mode-allow-remote`, meant for an isolated container behind another auth boundary.
- Admin key comparison is constant-time.
- Capsule execution is policy-bounded. `policy.json` is validated strictly: unknown fields, wrong types, absolute or `..` file roots, malformed hosts and budgets above 60 s are rejected, and a stored policy that fails validation runs the capsule deny-all. Every policy change is written to the audit log.
- File capabilities are rooted in the capsule's `data/` directory. Capsule code cannot reach its own `policy.json`, learned state, program or logs, other capsules, or anything else in the store.
- HTTP capabilities require explicit `allowed_hosts` when policy is active, and use `https://` unless the policy sets `allow_insecure_http`.
- Private, loopback, shared (CGNAT), link-local, metadata and reserved addresses, including IPv6 forms that embed them, are denied by default. The check runs inside the HTTP client's resolver on the address it connects to, which closes DNS-rebinding and URL-parser-confusion bypasses. Only the operator admin key can set `deny_private_networks: false`.

## Deployment Requirements

- Put Syntra behind a TLS-terminating reverse proxy such as Caddy, nginx, Traefik, or your platform ingress.
- Use a strong random admin key (`SYNTRA_ADMIN_KEY`; `LYCAN_ADMIN_KEY` is still read).
- Do not expose Syntra directly to the public internet.
- Treat the store volume as sensitive operational data.
- Back up the store volume if learned state matters.
- Avoid sending raw PII in decision inputs unless your deployment has a retention and redaction policy.

## Access controls

The runtime supports Admin, TenantAdmin, and Read tokens, token expiry and
revocation, plus token-bucket rate limiting; these are covered by route tests.
It does not provide interactive user accounts or an identity-provider integration.

## Known Gaps Before 1.0

- The capability sandbox runs in the server process, not behind an OS boundary.
- `max_memory_bytes` is accepted in policies but not enforced.
- No clustering or distributed store; isolate hostile tenants at the container/OS level.
- No built-in field-level encryption for store files.
- Admin console security posture needs a dedicated review.
- Decision logs can contain application-provided input fields.
- Public internet hardening requires external security review.

The 1.0 security-hardening track is maintained in [ROADMAP.md](ROADMAP.md).

## Reporting

Please report security issues privately to the repository owner rather than opening a public issue with exploit details.
