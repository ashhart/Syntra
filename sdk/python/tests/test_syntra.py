"""End-to-end tests for the Python SDK against a real `syntra serve`.

Build first:  sdk/python/scripts/develop.sh  (and `cargo build` for the
server binary; set SYNTRA_BIN to use another one). Run:

    python3 -m unittest discover -s sdk/python/tests -v
"""

import math
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE.parent / "python"))

import syntra  # noqa: E402

SYNTRA_BIN = os.environ.get("SYNTRA_BIN", str(ROOT / "target" / "debug" / "syntra"))
KEY = "py-sdk-test-key"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    def __init__(self) -> None:
        self.store = tempfile.mkdtemp(prefix="syntra-py-")
        self.clients: list = []
        env = dict(os.environ, SYNTRA_RATE_LIMIT_RPS="10000000", SYNTRA_RATE_LIMIT_BURST="10000000")
        for _ in range(10):
            self.addr = f"127.0.0.1:{free_port()}"
            self.proc = subprocess.Popen(
                [SYNTRA_BIN, "serve", "--addr", self.addr, "--store", self.store, "--admin-key", KEY],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            deadline = time.time() + 5
            while time.time() < deadline:
                try:
                    with urllib.request.urlopen(f"http://{self.addr}/health", timeout=1) as r:
                        if r.status == 200:
                            return
                except OSError:
                    time.sleep(0.04)
            self.proc.kill()
            self.proc.wait()
        raise RuntimeError("could not boot syntra")

    @property
    def url(self) -> str:
        return f"http://{self.addr}"

    def client(self, capsule: str) -> syntra.Client:
        c = syntra.Client(self.url, token=KEY, tenant="t", job="j", capsule=capsule)
        self.clients.append(c)
        return c

    def decider(self, capsule: str, **kw) -> syntra.LocalDecider:
        return syntra.LocalDecider(self.url, token=KEY, tenant="t", job="j", capsule=capsule, **kw)

    def close(self) -> None:
        for c in self.clients:
            c.close()
        self.proc.kill()
        self.proc.wait()
        shutil.rmtree(self.store, ignore_errors=True)


ACTIONS = {"actions": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}


class LocalDeciderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.srv = Server()

    @classmethod
    def tearDownClass(cls) -> None:
        cls.srv.close()

    def test_decides_locally_and_the_server_learns(self) -> None:
        self.srv.client("learn").put_spec(ACTIONS)
        decider = self.srv.decider("learn", sync_interval=None)
        self.assertEqual(decider.model_version, 0)
        for i in range(300):
            d = decider.decide({"user": i % 5, "tier": "pro"})
            self.assertIn(d.action, ("a", "b", "c"))
            self.assertTrue(0 < d.probability <= 1)
            self.assertEqual(d.id, d.decision_id)
            self.assertEqual({a for a, _ in d.ranking}, {"a", "b", "c"})
            decider.reward(d.decision_id, 1.0 if d.action == "b" else 0.0)
        report = decider.flush()
        self.assertEqual(report["decisions_accepted"], 300, report)
        self.assertEqual(report["decisions_rejected"], 0, report)
        self.assertEqual(report["rewards_applied"], 300, report)
        self.assertEqual(self.srv.client("learn").model()["modelVersion"], 300)

        deadline = time.time() + 5
        while not decider.sync():
            self.assertLess(time.time(), deadline, "no new model was published")
            time.sleep(0.1)
        self.assertEqual(decider.model_version, 300)
        chosen_b = sum(decider.decide({"user": i % 5, "tier": "pro"}).action == "b" for i in range(200))
        self.assertGreater(chosen_b, 120)
        self.assertEqual(decider.close()["decisions_accepted"], 200)

    def test_context_types(self) -> None:
        self.srv.client("types").put_spec(ACTIONS)
        decider = self.srv.decider("types", sync_interval=None)
        ctx = {"s": "x", "i": 3, "big": 2**63, "f": 0.5, "b": True, "n": None, "l": [1, "two", 3.0], "t": (1, 2), "d": {"k": {"deep": [False]}}}
        d = decider.decide(ctx)
        self.assertEqual(decider.flush()["decisions_accepted"], 1)
        stored = self.srv.client("types").decision(d.decision_id)
        self.assertEqual(stored["context"]["d"], {"k": {"deep": [False]}})
        self.assertEqual(stored["context"]["t"], [1, 2])
        self.assertEqual(stored["context"]["b"], True)
        with self.assertRaises(ValueError):
            decider.decide({"x": math.nan})
        with self.assertRaises(ValueError):
            decider.decide({"x": 2**70})
        with self.assertRaises(TypeError):
            decider.decide({1: "non-string key"})
        with self.assertRaises(TypeError):
            decider.decide({"x": object()})
        with self.assertRaises(syntra.SyntraError):
            decider.decide(actions=[])  # no actions

    def test_per_request_actions_and_exclusions(self) -> None:
        self.srv.client("dyn").put_spec(ACTIONS)
        decider = self.srv.decider("dyn", sync_interval=None)
        for _ in range(50):
            d = decider.decide({"q": 1}, exclude=["a", "b"])
            self.assertEqual(d.action, "c")
            self.assertEqual(d.probability, 1.0)
        d = decider.decide({"q": 1}, actions=[{"id": "gpt-small", "features": {"cost": 0.1}}, {"id": "gpt-large", "features": {"cost": 1.0}}])
        self.assertIn(d.action, ("gpt-small", "gpt-large"))
        report = decider.flush()
        self.assertEqual(report["decisions_accepted"], 51, report)

    def test_background_upload_and_close(self) -> None:
        self.srv.client("bg").put_spec(ACTIONS)
        with self.srv.decider("bg", sync_interval=0.1) as decider:
            ids = []
            for i in range(20):
                d = decider.decide({"i": i})
                decider.reward(d.decision_id, 0.5, detail={"latencyMs": 120})
                ids.append(d.decision_id)
            deadline = time.time() + 5
            while decider.pending:
                self.assertLess(time.time(), deadline, "background thread did not upload")
                time.sleep(0.05)
        stored = self.srv.client("bg").decision(ids[3])
        self.assertEqual(stored["rewards"][0]["detail"]["latencyMs"], 120)

    def test_threads_share_one_decider(self) -> None:
        self.srv.client("mt").put_spec(ACTIONS)
        decider = self.srv.decider("mt", sync_interval=None)
        errors = []

        def work() -> None:
            try:
                for i in range(500):
                    d = decider.decide({"i": i})
                    decider.reward(d.decision_id, 0.0)
            except Exception as e:  # pragma: no cover - reported below
                errors.append(e)

        threads = [threading.Thread(target=work) for _ in range(8)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(errors, [])
        report = decider.flush()
        self.assertEqual(report["decisions_accepted"], 4000, report)
        self.assertEqual(report["rewards_applied"], 4000, report)

    def test_decide_is_fast(self) -> None:
        self.srv.client("fast").put_spec(ACTIONS)
        decider = self.srv.decider("fast", sync_interval=None, max_queue=1_000_000)
        ctx = {"task": "code", "tier": "pro", "promptTokens": 812}
        n = 20000
        started = time.perf_counter()
        for _ in range(n):
            decider.decide(ctx)
        per_call_us = (time.perf_counter() - started) / n * 1e6
        print(f"\n  python decide: {per_call_us:.2f} us/call", file=sys.stderr)
        self.assertLess(per_call_us, 100)

    def test_unreachable_server(self) -> None:
        with self.assertRaises(syntra.SyntraError):
            syntra.LocalDecider(f"http://127.0.0.1:{free_port()}", token=KEY, tenant="t", job="j", capsule="x")


class ClientTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.srv = Server()

    @classmethod
    def tearDownClass(cls) -> None:
        cls.srv.close()

    def test_server_side_decide_and_reward(self) -> None:
        c = self.srv.client("http")
        c.put_spec(ACTIONS)
        d = c.decide({"user": 1}, event_id="evt-1")
        self.assertIn(d["action"], ("a", "b", "c"))
        again = c.decide({"user": 1}, event_id="evt-1")
        self.assertEqual(again["decisionId"], d["decisionId"])
        r = c.reward(d["decisionId"], 1.0)
        self.assertTrue(r["ok"])
        stored = c.decision(d["decisionId"])
        self.assertEqual(len(stored["rewards"]), 1)
        with self.assertRaises(syntra.HttpError) as caught:
            c.reward("no-such-decision", 1.0)
        self.assertEqual(caught.exception.status, 404)
        self.assertIsInstance(caught.exception, syntra.SyntraError)
        self.assertNotIn(KEY, str(caught.exception))

    def test_dropped_keepalive_connections_are_detected(self) -> None:
        class Conn:
            sock = None

        conn = Conn()
        self.assertFalse(syntra._dropped(conn))  # not connected yet
        ours, theirs = socket.socketpair()
        try:
            conn.sock = ours
            self.assertFalse(syntra._dropped(conn))  # idle and open
            theirs.close()  # the server closes the idle connection
            self.assertTrue(syntra._dropped(conn))
        finally:
            ours.close()

if __name__ == "__main__":
    unittest.main()
