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
| `allow_insecure_http` | bool | false | Permit plain `http://` under the network sandbox (otherwise https only) |
| `file_root` | string/null | null | Subdirectory of the working directory to use as the file sandbox root; relative, no `..` |
| `allowed_hosts` | string[] | [] | Permitted HTTP hosts, bare names or `*.suffix` (empty = deny all when policy active) |
| `deny_private_networks` | bool | true | Block loopback, private, shared, link-local, metadata and reserved addresses. On the server only an admin credential (the operator key or an `admin` token) may set it to false |
| `max_execution_ms` | integer | 30000 | Wall-clock budget per execution, 1 to 60000 |
| `allow_self_modify`, `max_memory_bytes` | bool, integer | — | Accepted for compatibility; not enforced |

A policy document with any other key, a value of the wrong type, an absolute
or escaping `file_root`, a malformed host or an out-of-range budget is
rejected: `PUT .../policy` answers 400, and a stored file that fails
validation makes the server run the capsule deny-all.

## Trust model

| Execution mode | Policy |
|---|---|
| `lycan program.lyc` (direct CLI) | Unrestricted — developer mode |
| `lycan capsule run name.lycap` | Policy loaded from capsule's policy.json |
| Syntra feature program (run on each `/decide`) | The capsule's `policy.json` from the store, fail-closed (invalid = deny-all); a new capsule's policy denies everything |

## File sandbox

When policy is active with `allow_file_read` or `allow_file_write`:
- The root is the execution's working directory, optionally narrowed by a
  relative `file_root`. On the server the working directory is the capsule's
  `data/` directory, so capsule code cannot reach its own policy, learned
  state, program or logs. Without a working directory, file access is denied.
- Absolute paths denied
- `..` traversal denied
- Symlink escape checked via canonicalization, for the requested path and
  for the root itself

## Network sandbox

When policy is active with `allow_network`:
- `https://` only, unless `allow_insecure_http` is true
- `allowed_hosts` must be non-empty (empty = deny all outbound); `*.suffix`
  matches the suffix and its subdomains on a label boundary
- `deny_private_networks` blocks 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10,
  127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24, 192.0.2.0/24,
  192.88.99.0/24, 192.168.0.0/16, 198.18.0.0/15, 198.51.100.0/24,
  203.0.113.0/24, 224.0.0.0/3, ::, ::1, fc00::/7, fe80::/10, fec0::/10,
  ff00::/8, 2001::/32, 2001:db8::/32, 64:ff9b:1::/48, 100::/64, and IPv6
  forms that embed a blocked IPv4 address (mapped, compatible, NAT64, 6to4)
- The allow-list and private-address checks run again inside the HTTP
  client's DNS resolver, on the host it connects to, and only the addresses
  that pass are used. A URL that different parsers read differently, or a DNS
  answer that changes after the first check, cannot reach a blocked host. A
  failed lookup denies the request.
- HTTP redirects disabled for sandboxed requests
- IPv6 bracket syntax handled; URLs with userinfo (`user@host`) are denied

## No policy = unrestricted

When no policy exists (direct CLI execution), all capabilities run without restriction. This is the developer mode.
