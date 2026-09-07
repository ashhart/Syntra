"""Background poller threads.

One daemon thread per source. Each thread loops:

    1. Call the dispatched poll_* function.
    2. If it returned a float, cache it.
    3. Sleep for `interval_seconds`.

Threads are daemons, so SIGINT on the main process kills them too. There is
no graceful shutdown handshake — for a stateless cache that's fine; nothing
needs to be flushed.
"""

from __future__ import annotations

import logging
import threading
import time

from .cache import FeatureCache
from .config import Config, SourceConfig
from .sources import POLLERS

log = logging.getLogger(__name__)


class Poller:
    def __init__(self, config: Config, cache: FeatureCache) -> None:
        self._config = config
        self._cache = cache
        self._stop = threading.Event()
        self._threads: list[threading.Thread] = []

    def start(self) -> None:
        for src in self._config.sources:
            t = threading.Thread(
                target=self._run_one,
                args=(src,),
                name=f"poll-{src.name}",
                daemon=True,
            )
            t.start()
            self._threads.append(t)
            log.info(
                "started poller name=%s type=%s interval=%ss",
                src.name,
                src.type,
                src.interval_seconds,
            )

    def stop(self) -> None:
        self._stop.set()

    def _run_one(self, src: SourceConfig) -> None:
        fn = POLLERS.get(src.type)
        if fn is None:
            log.error("[%s] no poller registered for type %s", src.name, src.type)
            return

        while not self._stop.is_set():
            start = time.time()
            try:
                value = fn(src)
            except Exception as exc:  # noqa: BLE001 — defence in depth
                log.exception("[%s] poller raised unexpectedly: %s", src.name, exc)
                value = None

            if value is not None:
                self._cache.set(src.name, value, src.type)
                log.debug("[%s] updated value=%s", src.name, value)

            # Sleep the remainder of the interval, but wake early on stop.
            elapsed = time.time() - start
            remaining = max(0.0, src.interval_seconds - elapsed)
            self._stop.wait(remaining)
