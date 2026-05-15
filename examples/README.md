# Examples

The public demo surface is:

```bash
./examples/showcase/run-all.sh
```

If you only run one thing first, start here:

```bash
cat examples/strategy-learning/README.md
```

That demo shows the core primitive directly: a strategy node keeps the output contract stable while weights move toward the better option.

The showcase suite runs five hard-hitting demos:

1. `apps-learn-contexts` - one capsule, three context memories.
2. `live-mars-mission` - NASA/JPL Horizons data plus Lambert decision.
3. `autonomous-evolution` - candidate accepted, bad proposal rejected.
4. `sandbox-red-team` - file escape and SSRF blocked.
5. `runtime-appliance-memory` - API feedback persists after restart.

Everything else in this directory is lab material: fixtures, raw programs, regression scripts, compiled examples, and development checks.

Use the showcase for humans. Use the rest when hacking on Lycan.
