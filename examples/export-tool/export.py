#!/usr/bin/env python3
# Copyright 2026 Syntra contributors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
"""syntra-export — dump a capsule's current learned state to portable JSON.

Usage
-----
    syntra-export \\
        --syntra-url http://localhost:8787 \\
        --admin-key  $SYNTRA_ADMIN_KEY \\
        --tenant     t \\
        --job        j \\
        --capsule    c \\
        [--include-decisions] \\
        [--include-audits] \\
        [--include-snapshots] \\
        [--output snapshot.json]

When ``--output`` is omitted the snapshot JSON is written to stdout.

The output is a version-1 export compatible with
``syntra-ope evaluate --mode static --policy-json <snapshot.json>``
(the ``policyByContext`` sub-object maps directly to the
``context_key -> bestOption`` table consumed by EvalPolicy.from_json).
"""
from __future__ import annotations

import argparse
import json
import os
import sys

# Allow running without installing the package.
_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

from syntra_export import SyntraExportError, fetch_capsule_export


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="syntra-export",
        description=(
            "Dump a Syntra capsule's current learned state to a portable JSON "
            "snapshot.  The output can be archived, moved between Syntra "
            "instances, or used as a static policy for offline evaluation."
        ),
    )
    p.add_argument(
        "--syntra-url",
        required=True,
        metavar="URL",
        help="Base URL of the Syntra server (e.g. http://localhost:8787).",
    )
    p.add_argument(
        "--admin-key",
        required=True,
        metavar="KEY",
        help=(
            "Bearer token for the Syntra API.  A scoped read token "
            "(scope::Read{tenant, job, capsule}) works in addition to an "
            "admin key."
        ),
    )
    p.add_argument(
        "--tenant",
        required=True,
        metavar="TENANT",
        help="Tenant identifier.",
    )
    p.add_argument(
        "--job",
        required=True,
        metavar="JOB",
        help="Job identifier.",
    )
    p.add_argument(
        "--capsule",
        required=True,
        metavar="CAPSULE",
        help="Capsule identifier.",
    )
    p.add_argument(
        "--include-decisions",
        action="store_true",
        default=False,
        help=(
            "Include the raw decision log in the snapshot "
            "(GET /decisions — newline-delimited JSON)."
        ),
    )
    p.add_argument(
        "--include-audits",
        action="store_true",
        default=False,
        help=(
            "Include the audit log in the snapshot "
            "(GET /audits — newline-delimited JSON)."
        ),
    )
    p.add_argument(
        "--include-snapshots",
        action="store_true",
        default=False,
        help=(
            "Include snapshot metadata in the export "
            "(GET /snapshots — list of snapshot descriptors, bodies excluded)."
        ),
    )
    p.add_argument(
        "--output",
        metavar="PATH",
        default=None,
        help=(
            "Write the snapshot to this file.  "
            "Defaults to stdout when omitted."
        ),
    )
    return p


def main(argv=None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    try:
        snapshot = fetch_capsule_export(
            syntra_url=args.syntra_url,
            admin_key=args.admin_key,
            tenant=args.tenant,
            job=args.job,
            capsule=args.capsule,
            include_decisions=args.include_decisions,
            include_audits=args.include_audits,
            include_snapshots=args.include_snapshots,
        )
    except SyntraExportError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        if exc.status == 401:
            print(
                "Hint: check that --admin-key is correct and that the token "
                "has at least Read scope for this tenant/job/capsule.",
                file=sys.stderr,
            )
        elif exc.status == 404:
            print(
                "Hint: verify --tenant, --job, and --capsule are correct and "
                "that the capsule has been installed on this Syntra instance.",
                file=sys.stderr,
            )
        return 1
    except OSError as exc:
        print(f"ERROR: network error: {exc}", file=sys.stderr)
        return 1

    serialised = json.dumps(snapshot, indent=2)

    if args.output:
        try:
            with open(args.output, "w", encoding="utf-8") as fh:
                fh.write(serialised)
                fh.write("\n")
        except OSError as exc:
            print(f"ERROR: could not write output file: {exc}", file=sys.stderr)
            return 1
        n_contexts = len(snapshot.get("policyByContext", {}))
        print(
            f"Wrote snapshot to {args.output} "
            f"({n_contexts} context(s) in policyByContext)",
            file=sys.stderr,
        )
    else:
        print(serialised)

    return 0


if __name__ == "__main__":
    sys.exit(main())
