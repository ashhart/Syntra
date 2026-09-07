"""Scoped token issuance and access control demonstration.

Demonstrates:
  - POST /admin/tokens to mint Read-scoped, TenantAdmin-scoped, and Admin-scoped tokens.
  - For each token, attempts a known-forbidden action (POST /install requires
    CapsuleMutate or AdminGlobal; a Read token returns 403).
  - Prints the three tokens (hash + label) and the 403 responses they collect
    on unauthorized routes.

Prerequisites:
  - Syntra running at $SYNTRA_URL (default http://localhost:8787).
  - Admin key exported as $SYNTRA_ADMIN_KEY (or passed via --admin-key).
  - The admin key must carry AdminGlobal scope (the LYCAN_ADMIN_KEY always does).

Usage:
    python3 01_scoped_tokens.py [--syntra-url URL] [--admin-key KEY]
    python3 01_scoped_tokens.py --syntra-url http://localhost:8787 --admin-key mysecret

Apache-2.0.
"""
from __future__ import annotations

import argparse
import json
import os
import urllib.request
import urllib.error


def api(url: str, method: str, path: str, body: object | None = None,
        token: str | None = None) -> tuple[int, dict]:
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(
        f"{url}{path}", data=data, headers=headers, method=method
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as exc:
        try:
            body_bytes = exc.read()
            return exc.code, json.loads(body_bytes)
        except Exception:
            return exc.code, {"error": exc.reason}


def mint_token(url: str, admin_key: str, scope: dict, label: str) -> dict:
    status, body = api(url, "POST", "/admin/tokens",
                       {"scope": scope, "label": label}, admin_key)
    if status != 200:
        raise RuntimeError(f"Failed to mint token ({label}): {status} {body}")
    return body


def print_section(title: str) -> None:
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"))
    parser.add_argument("--admin-key", default=os.environ.get("SYNTRA_ADMIN_KEY", ""))
    parser.add_argument("--tenant", default="demo")
    parser.add_argument("--job", default="pricing")
    parser.add_argument("--capsule", default="router")
    args = parser.parse_args()

    url = args.syntra_url.rstrip("/")
    admin_key = args.admin_key
    if not admin_key:
        raise SystemExit("ERROR: --admin-key or $SYNTRA_ADMIN_KEY is required")

    capsule_path = f"/tenants/{args.tenant}/jobs/{args.job}/capsules/{args.capsule}"

    # ── 1. Mint three tokens ────────────────────────────────────────────────

    print_section("1. Minting tokens")

    read_tok = mint_token(url, admin_key, {
        "type": "Read",
        "tenant": args.tenant,
        "job": args.job,
        "capsule": args.capsule,
    }, label="read-only-demo")

    tenant_admin_tok = mint_token(url, admin_key, {
        "type": "TenantAdmin",
        "tenant": args.tenant,
    }, label="tenant-admin-demo")

    global_admin_tok = mint_token(url, admin_key, {
        "type": "Admin",
    }, label="global-admin-demo")

    tokens = [
        ("Read",        read_tok),
        ("TenantAdmin", tenant_admin_tok),
        ("Admin",       global_admin_tok),
    ]
    for scope_name, tok in tokens:
        print(json.dumps({
            "scope":    scope_name,
            "label":    tok.get("label", ""),
            "hash":     tok.get("hash", ""),
            "expiresAt": tok.get("expiresAt"),
        }))

    # ── 2. Probe forbidden actions ─────────────────────────────────────────

    print_section("2. Forbidden-action probes (expect 403)")

    # Read token: POST /install is CapsuleMutate — forbidden for Read scope.
    status, body = api(url, "POST", f"{capsule_path}/install",
                       body=None, token=read_tok["token"])
    print(json.dumps({
        "probe": "read-token -> POST /install",
        "status": status,
        "error": body.get("error", body),
        "expected": 403,
        "pass": status == 403,
    }))

    # Read token: POST /admin/tokens is AdminGlobal — forbidden.
    status, body = api(url, "POST", "/admin/tokens",
                       {"scope": {"type": "Admin"}, "label": "attempted"}, read_tok["token"])
    print(json.dumps({
        "probe": "read-token -> POST /admin/tokens",
        "status": status,
        "error": body.get("error", body),
        "expected": 403,
        "pass": status == 403,
    }))

    # TenantAdmin token: POST /admin/backup is AdminGlobal — forbidden.
    status, body = api(url, "POST", "/admin/backup",
                       token=tenant_admin_tok["token"])
    print(json.dumps({
        "probe": "tenant-admin-token -> POST /admin/backup",
        "status": status,
        "error": body.get("error", body),
        "expected": 403,
        "pass": status == 403,
    }))

    # Global Admin: GET /admin/tokens is AdminGlobal — should succeed.
    status, body = api(url, "GET", "/admin/tokens",
                       token=global_admin_tok["token"])
    print(json.dumps({
        "probe": "global-admin-token -> GET /admin/tokens",
        "status": status,
        "tokenCount": len(body.get("tokens", [])),
        "expected": 200,
        "pass": status == 200,
    }))

    # ── 3. Cleanup — revoke the demo tokens ───────────────────────────────

    print_section("3. Revoking demo tokens")
    for scope_name, tok in tokens:
        status, body = api(url, "DELETE", f"/admin/tokens/{tok['hash']}",
                           token=admin_key)
        print(json.dumps({
            "scope": scope_name,
            "hash":  tok["hash"][:12] + "...",
            "revoked": body.get("revoked", False),
            "status": status,
        }))

    print("\nDone.")


if __name__ == "__main__":
    main()
