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
"""Unit tests for syntra_export.

All tests are entirely offline: urllib.request is mocked via unittest.mock
so no running Syntra server is required.
"""
from __future__ import annotations

import io
import json
import sys
import unittest
import urllib.error
from pathlib import Path
from typing import Any, Dict
from unittest.mock import MagicMock, patch

# Ensure the package root is on sys.path when run directly.
_ROOT = Path(__file__).resolve().parent.parent
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from syntra_export import (
    SyntraExportError,
    derive_policy_by_context,
    fetch_capsule_export,
)
import export as export_cli


# ── Fixtures ──────────────────────────────────────────────────────────────────


def _memory_with_leader(
    context_key: str = "ctx_high_load",
    weights: list[float] | None = None,
    leader: str = "Ucb",
    also_legacy: bool = False,
) -> Dict[str, Any]:
    """Build a minimal /memory response with a meta-bandit leader."""
    weights = weights or [0.05, 0.90, 0.05]
    candidate_contexts = {
        f"{leader}|{context_key}": {
            "weights": weights,
            "stats": [],
            "updatedAt": 1747500000,
            "optionStates": [],
        }
    }
    legacy_contexts: Dict[str, Any] = {}
    if also_legacy:
        legacy_contexts[context_key] = {
            "weights": [0.33, 0.34, 0.33],
            "stats": [],
            "updatedAt": 1747499000,
            "optionStates": [],
        }
    return {
        "version": 7,
        "strategies": {
            "1": {
                "nodeId": 1,
                "nOptions": 3,
                "contexts": legacy_contexts,
                "candidateContexts": candidate_contexts,
                "metaBandit": {"leader": leader},
                "contextDetectors": {},
            }
        },
    }


def _memory_no_leader(context_key: str = "ctx_default") -> Dict[str, Any]:
    """Build a /memory response where no meta-bandit leader has been elected."""
    return {
        "version": 7,
        "strategies": {
            "1": {
                "nodeId": 1,
                "nOptions": 2,
                "contexts": {
                    context_key: {
                        "weights": [0.3, 0.7],
                        "stats": [],
                        "updatedAt": 1747400000,
                        "optionStates": [],
                    }
                },
                "candidateContexts": {},
                "metaBandit": None,
                "contextDetectors": {},
            }
        },
    }


def _make_urlopen_mock(responses: list[tuple[int, Any]]):
    """Return a context-manager mock that yields responses in sequence.

    Each element of *responses* is ``(status_code, body)``.  The body may be a
    dict (serialised to JSON) or a raw string.
    """
    call_count = [0]

    def fake_urlopen(req, **kwargs):
        idx = call_count[0]
        call_count[0] += 1
        if idx >= len(responses):
            raise AssertionError(f"Unexpected HTTP call #{idx}")
        status, body = responses[idx]
        if isinstance(body, (dict, list)):
            raw = json.dumps(body).encode()
        else:
            raw = body.encode() if isinstance(body, str) else body
        if status != 200:
            err = urllib.error.HTTPError(
                url="http://x", code=status, msg="err", hdrs=None, fp=io.BytesIO(raw)
            )
            raise err
        cm = MagicMock()
        cm.__enter__ = MagicMock(return_value=cm)
        cm.__exit__ = MagicMock(return_value=False)
        cm.read = MagicMock(return_value=raw)
        return cm

    return fake_urlopen


def _standard_responses(
    memory: Dict[str, Any],
    include_decisions: bool = False,
    include_audits: bool = False,
    include_snapshots: bool = False,
) -> list[tuple[int, Any]]:
    """Build the sequence of (status, body) that fetch_capsule_export expects."""
    inspect_body = {
        "hash": "abc123",
        "syntraVersion": "0.2.0",
        "warmupState": "active",
    }
    learning_body = {"algorithm": "Ucb1"}
    report_body = {
        "tenant": "t",
        "job": "j",
        "capsule": "c",
        "strategies": [
            {
                "node_id": 1,
                "activations": 500,
                "n_options": 3,
                "options": [],
                "weightsSource": "meta_bandit_leader",
                "leaderCandidate": "Ucb",
                "contextKey": "ctx_high_load",
            }
        ],
    }
    responses: list[tuple[int, Any]] = [
        (200, inspect_body),
        (200, learning_body),
        (200, report_body),
        (200, memory),
    ]
    if include_decisions:
        responses.append((200, '{"decisionId":"d1","option":1}\n'))
    if include_audits:
        responses.append((200, '{"auditId":"a1","reason":"safety"}\n'))
    if include_snapshots:
        responses.append((200, {"snapshots": [{"id": "snap1", "ts": 1747500000}]}))
    return responses


# ── Test cases ────────────────────────────────────────────────────────────────


class TestDerivePolicy(unittest.TestCase):
    """Test 1: policyByContext derivation correctly identifies argmax weights."""

    def test_argmax_uses_candidate_context_when_leader_present(self):
        # weights [0.05, 0.90, 0.05] -> bestOption == 1
        memory = _memory_with_leader(
            context_key="ctx_high_load",
            weights=[0.05, 0.90, 0.05],
            leader="Ucb",
        )
        policy = derive_policy_by_context(memory)
        self.assertIn("ctx_high_load", policy)
        entry = policy["ctx_high_load"]
        self.assertEqual(entry["bestOption"], 1)
        self.assertEqual(entry["weights"], [0.05, 0.90, 0.05])

    def test_argmax_correct_for_first_option(self):
        # weights [0.80, 0.10, 0.10] -> bestOption == 0
        memory = _memory_with_leader(
            context_key="ctx_idle",
            weights=[0.80, 0.10, 0.10],
            leader="Thompson",
        )
        policy = derive_policy_by_context(memory)
        self.assertEqual(policy["ctx_idle"]["bestOption"], 0)

    def test_argmax_correct_for_last_option(self):
        # weights [0.1, 0.2, 0.7] -> bestOption == 2
        memory = _memory_with_leader(
            context_key="ctx_z",
            weights=[0.1, 0.2, 0.7],
            leader="Weighted",
        )
        policy = derive_policy_by_context(memory)
        self.assertEqual(policy["ctx_z"]["bestOption"], 2)


class TestOutOfBandFieldsExcluded(unittest.TestCase):
    """Test 2: decisions/audits/snapshots absent when flags are not passed."""

    def test_out_of_band_fields_absent_by_default(self):
        memory = _memory_with_leader()
        responses = _standard_responses(memory)
        with patch("urllib.request.urlopen", side_effect=_make_urlopen_mock(responses)):
            result = fetch_capsule_export(
                syntra_url="http://localhost:8787",
                admin_key="key",
                tenant="t",
                job="j",
                capsule="c",
            )
        self.assertNotIn("decisions", result)
        self.assertNotIn("audits", result)
        self.assertNotIn("snapshots", result)


class TestOutOfBandFieldsIncluded(unittest.TestCase):
    """Test 3: decisions/audits/snapshots present when flags are set."""

    def test_out_of_band_fields_present_when_requested(self):
        memory = _memory_with_leader()
        responses = _standard_responses(
            memory,
            include_decisions=True,
            include_audits=True,
            include_snapshots=True,
        )
        with patch("urllib.request.urlopen", side_effect=_make_urlopen_mock(responses)):
            result = fetch_capsule_export(
                syntra_url="http://localhost:8787",
                admin_key="key",
                tenant="t",
                job="j",
                capsule="c",
                include_decisions=True,
                include_audits=True,
                include_snapshots=True,
            )
        self.assertIn("decisions", result)
        self.assertIn("audits", result)
        self.assertIn("snapshots", result)
        self.assertEqual(len(result["decisions"]), 1)
        self.assertEqual(result["decisions"][0]["decisionId"], "d1")
        self.assertEqual(len(result["audits"]), 1)
        self.assertEqual(len(result["snapshots"]), 1)


class TestCLIArgumentParser(unittest.TestCase):
    """Test 4: CLI rejects invocations missing required arguments."""

    def test_missing_syntra_url_exits(self):
        with self.assertRaises(SystemExit) as ctx:
            export_cli.build_parser().parse_args(
                ["--admin-key", "k", "--tenant", "t", "--job", "j", "--capsule", "c"]
            )
        self.assertNotEqual(ctx.exception.code, 0)

    def test_missing_admin_key_exits(self):
        with self.assertRaises(SystemExit) as ctx:
            export_cli.build_parser().parse_args(
                ["--syntra-url", "http://x", "--tenant", "t", "--job", "j", "--capsule", "c"]
            )
        self.assertNotEqual(ctx.exception.code, 0)

    def test_missing_tenant_exits(self):
        with self.assertRaises(SystemExit) as ctx:
            export_cli.build_parser().parse_args(
                ["--syntra-url", "http://x", "--admin-key", "k", "--job", "j", "--capsule", "c"]
            )
        self.assertNotEqual(ctx.exception.code, 0)

    def test_missing_capsule_exits(self):
        with self.assertRaises(SystemExit) as ctx:
            export_cli.build_parser().parse_args(
                ["--syntra-url", "http://x", "--admin-key", "k", "--tenant", "t", "--job", "j"]
            )
        self.assertNotEqual(ctx.exception.code, 0)


class TestHttp401Propagation(unittest.TestCase):
    """Test 5: HTTP 401 from Syntra propagates as SyntraExportError."""

    def test_401_raises_syntra_export_error(self):
        def fake_urlopen(req, **kwargs):
            raise urllib.error.HTTPError(
                url="http://x",
                code=401,
                msg="Unauthorized",
                hdrs=None,
                fp=io.BytesIO(b'{"error":"unauthorized"}'),
            )

        with patch("urllib.request.urlopen", side_effect=fake_urlopen):
            with self.assertRaises(SyntraExportError) as ctx:
                fetch_capsule_export(
                    syntra_url="http://localhost:8787",
                    admin_key="bad-key",
                    tenant="t",
                    job="j",
                    capsule="c",
                )
        exc = ctx.exception
        self.assertEqual(exc.status, 401)
        self.assertIn("401", str(exc))

    def test_401_exits_with_nonzero_in_cli(self):
        def fake_urlopen(req, **kwargs):
            raise urllib.error.HTTPError(
                url="http://x",
                code=401,
                msg="Unauthorized",
                hdrs=None,
                fp=io.BytesIO(b'{"error":"unauthorized"}'),
            )

        with patch("urllib.request.urlopen", side_effect=fake_urlopen):
            rc = export_cli.main(
                [
                    "--syntra-url", "http://localhost:8787",
                    "--admin-key", "bad-key",
                    "--tenant", "t",
                    "--job", "j",
                    "--capsule", "c",
                ]
            )
        self.assertEqual(rc, 1)


class TestEmptyStrategies(unittest.TestCase):
    """Test 6: empty memory.strategies produces empty policyByContext."""

    def test_empty_strategies_gives_empty_policy(self):
        memory_empty = {"version": 7, "strategies": {}}
        policy = derive_policy_by_context(memory_empty)
        self.assertEqual(policy, {})

    def test_strategy_with_zero_n_options_is_skipped(self):
        memory = {
            "version": 7,
            "strategies": {
                "1": {
                    "nodeId": 1,
                    "nOptions": 0,
                    "contexts": {},
                    "candidateContexts": {},
                    "metaBandit": None,
                }
            },
        }
        policy = derive_policy_by_context(memory)
        self.assertEqual(policy, {})

    def test_no_crash_on_missing_strategies_key(self):
        policy = derive_policy_by_context({})
        self.assertEqual(policy, {})

    def test_full_export_with_empty_memory(self):
        memory_empty: Dict[str, Any] = {"version": 7, "strategies": {}}
        responses = _standard_responses(memory_empty)
        with patch("urllib.request.urlopen", side_effect=_make_urlopen_mock(responses)):
            result = fetch_capsule_export(
                syntra_url="http://localhost:8787",
                admin_key="key",
                tenant="t",
                job="j",
                capsule="c",
            )
        self.assertEqual(result["policyByContext"], {})
        self.assertEqual(result["v"], 1)


class TestStaticModeCompatibility(unittest.TestCase):
    """Test 7: exported policyByContext is a valid static policy for syntra-ope.

    Simulates the shape consumed by EvalPolicy.from_json in static mode:
    a JSON mapping context_key -> bestOption (string or int).  The
    offline-eval tool loads the file, maps each row's context_key to an
    action index, and computes IPS / DR estimators.
    """

    def _build_export_json(self, weights_map: Dict[str, list]) -> str:
        """Build a minimal export JSON and return its serialised form."""
        # Build a memory payload from the provided weights_map.
        candidate_contexts = {
            f"Ucb|{ctx}": {"weights": w, "stats": [], "updatedAt": 1}
            for ctx, w in weights_map.items()
        }
        memory: Dict[str, Any] = {
            "version": 7,
            "strategies": {
                "1": {
                    "nodeId": 1,
                    "nOptions": len(next(iter(weights_map.values()))),
                    "contexts": {},
                    "candidateContexts": candidate_contexts,
                    "metaBandit": {"leader": "Ucb"},
                }
            },
        }
        responses = _standard_responses(memory)
        with patch("urllib.request.urlopen", side_effect=_make_urlopen_mock(responses)):
            snapshot = fetch_capsule_export(
                syntra_url="http://localhost:8787",
                admin_key="key",
                tenant="t",
                job="j",
                capsule="c",
            )
        return json.dumps(snapshot)

    def test_policy_by_context_is_extractable_for_ope(self):
        """Load the export JSON and extract policyByContext for offline eval."""
        weights_map = {
            "ctx_a": [0.1, 0.8, 0.1],
            "ctx_b": [0.6, 0.2, 0.2],
        }
        export_json_str = self._build_export_json(weights_map)
        snapshot = json.loads(export_json_str)

        # Confirm schema version
        self.assertEqual(snapshot["v"], 1)
        self.assertIn("policyByContext", snapshot)

        policy_by_context = snapshot["policyByContext"]

        # Simulate what EvalPolicy.from_json does: build a context->action table.
        # The offline-eval tool expects {context_key: action_label}.
        # syntra-ope reads bestOption as the action index.
        ope_table: Dict[str, Any] = {
            ctx: entry["bestOption"]
            for ctx, entry in policy_by_context.items()
        }

        # ctx_a: argmax([0.1, 0.8, 0.1]) == 1
        self.assertEqual(ope_table.get("ctx_a"), 1)
        # ctx_b: argmax([0.6, 0.2, 0.2]) == 0
        self.assertEqual(ope_table.get("ctx_b"), 0)

    def test_policy_by_context_covers_all_contexts_in_memory(self):
        weights_map = {
            "ctx_morning": [0.2, 0.5, 0.3],
            "ctx_evening": [0.7, 0.1, 0.2],
            "ctx_night":   [0.3, 0.3, 0.4],
        }
        export_json_str = self._build_export_json(weights_map)
        snapshot = json.loads(export_json_str)
        policy_by_context = snapshot["policyByContext"]

        self.assertEqual(set(policy_by_context.keys()), set(weights_map.keys()))

    def test_fallback_to_legacy_contexts_when_no_leader(self):
        """When metaBandit is None the legacy contexts bucket is used."""
        memory = _memory_no_leader(context_key="ctx_fallback")
        policy = derive_policy_by_context(memory)
        self.assertIn("ctx_fallback", policy)
        # weights [0.3, 0.7] -> bestOption == 1
        self.assertEqual(policy["ctx_fallback"]["bestOption"], 1)


if __name__ == "__main__":
    unittest.main()
