# Security Policy

Syntra is a self-hosted adaptive decision appliance. It is designed to run inside infrastructure you control, behind your normal edge security.

## Current Posture

- `/health` and the static `/admin` login shell are public.
- All data/API routes require `Authorization: Bearer <admin-key>`.
- The server refuses to start without an admin key unless `--dev-mode` is explicitly used.
- Admin key comparison is constant-time.
- Capsule execution is policy-bounded.
- File capabilities are sandboxed.
- HTTP capabilities require explicit `allowed_hosts` when policy is active.
- Private network targets are denied by default for sandboxed HTTP capabilities.

## Deployment Requirements

- Put Syntra behind a TLS-terminating reverse proxy such as Caddy, nginx, Traefik, or your platform ingress.
- Use a strong random `LYCAN_ADMIN_KEY`.
- Do not expose Syntra directly to the public internet.
- Treat the store volume as sensitive operational data.
- Back up the store volume if learned state matters.
- Avoid sending raw PII in decision inputs unless your deployment has a retention and redaction policy.

## Known Gaps Before 1.0

- Single shared admin key; no user/role model yet.
- No built-in rate limiting.
- No built-in field-level encryption for store files.
- Admin console security posture needs a dedicated review.
- Decision logs can contain application-provided input fields.
- Public internet hardening requires external security review.

The 1.0 security-hardening track is maintained in [#2](https://github.com/ashhart/Syntra/issues/2).

## Reporting

Please report security issues privately to the repository owner rather than opening a public issue with exploit details.
