# Execution Policy

Policies define what a Lycan program is allowed to do at runtime.

## Policy fields

| Field | Type | Default | Effect |
|---|---|---|---|
| `allow_stdout` | bool | true | Controls `!p` / Print |
| `allow_stdin` | bool | false | Controls `!r` / ReadLine |
| `allow_file_read` | bool | false | Controls `file.exists`, `file.readText`, `sql.sqliteQuery` |
| `allow_file_write` | bool | false | Controls `file.writeText` |
| `allow_network` | bool | false | Controls `http.get`, `http.post` |
| `file_root` | string/null | null | Root directory for file sandbox |
| `allowed_hosts` | string[] | [] | Permitted HTTP hosts (empty = deny all when policy active) |
| `deny_private_networks` | bool | true | Block localhost, RFC1918, link-local, metadata IPs |

## Trust model

| Execution mode | Policy |
|---|---|
| `lycan program.lyc` (direct CLI) | Unrestricted — developer mode |
| `lycan capsule run name.lycap` | Policy loaded from capsule's policy.json |
| Server `/decide` | Policy loaded from store, fail-closed (corrupt = deny-all) |
| `lycan evolve --policy file.json` | Explicit policy during evolution benchmarks |

## File sandbox

When policy is active with `allow_file_read` or `allow_file_write`:
- Absolute paths denied
- `..` traversal denied
- Paths resolved relative to `file_root` or capsule working directory
- Symlink escape checked via canonicalization

## Network sandbox

When policy is active with `allow_network`:
- `allowed_hosts` must be non-empty (empty = deny all outbound)
- `deny_private_networks` blocks: 127.0.0.0/8, ::1, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, fc00::/7, fe80::/10
- DNS resolution checked — hostnames resolving to private IPs denied
- HTTP redirects disabled for sandboxed requests
- IPv6 bracket syntax handled

## No policy = unrestricted

When no policy exists (direct CLI execution), all capabilities run without restriction. This is the developer mode.
