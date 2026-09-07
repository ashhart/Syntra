# Capsule Format (.lycap)

A capsule is a self-contained Lycan software packet for agent-to-agent exchange.

## Structure

```
name.lycap/
  manifest.json    — identity, intent, SHA256 hashes, capabilities
  program.lyc      — compiled graph binary
  inspect.json     — AI-readable graph structure
  journal.json     — evolution history
  policy.json      — runtime permissions
```

## Creating a capsule

```bash
lycan capsule create program.lyc name "intent description"
```

## Verifying a capsule

```bash
lycan capsule verify name.lycap
```

Verification checks:
- Required files exist (manifest.json, program.lyc, policy.json)
- Graph is structurally valid
- SHA256 hashes match
- Policy grants all effects the graph requires
- Journal node references are valid

## Running a capsule

```bash
lycan capsule run name.lycap
```

Policy is enforced at runtime. The capsule's working directory becomes the file sandbox root.

## Policy

```json
{
  "allow_stdout": true,
  "allow_stdin": false,
  "allow_file_read": false,
  "allow_file_write": false,
  "allow_network": false,
  "allowed_hosts": [],
  "deny_private_networks": true,
  "file_root": null
}
```

Capabilities that violate policy produce structured errors:

```
capability=file.readText effect=file_read denied by policy
```

## Manifest

```json
{
  "name": "router",
  "format": "lycan-capsule-v1",
  "intent": "Route API requests",
  "program_sha256": "abc123...",
  "inspect_sha256": "def456...",
  "capabilities": ["stdout", "file_read"]
}
```
