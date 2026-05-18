"""In-memory feature cache.

Holds the latest scraped value per feature name. No history, no persistence.
On restart the cache is empty until pollers refill it.

Thread-safe: a single lock guards the dict because pollers run on their own
threads and the HTTP handler reads concurrently. Contention is negligible at
expected scrape rates (seconds, not microseconds).
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass
from typing import Any


@dataclass
class Entry:
    value: float
    source_type: str
    updated_at: float  # monotonic-ish epoch seconds (time.time())


class FeatureCache:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._entries: dict[str, Entry] = {}

    def set(self, name: str, value: float, source_type: str) -> None:
        with self._lock:
            self._entries[name] = Entry(
                value=float(value),
                source_type=source_type,
                updated_at=time.time(),
            )

    def snapshot(self) -> dict[str, Any]:
        """Return the current feature values plus a `_meta` block with the
        source type and how stale each value is."""

        now = time.time()
        with self._lock:
            items = list(self._entries.items())

        out: dict[str, Any] = {}
        meta: dict[str, dict[str, Any]] = {}
        for name, entry in items:
            out[name] = entry.value
            meta[name] = {
                "source": entry.source_type,
                "stale_seconds": round(now - entry.updated_at, 3),
            }
        out["_meta"] = meta
        return out

    def freshest_age(self) -> float | None:
        """Seconds since the most-recently-updated feature. None if cache is
        empty. Used by /healthz."""

        with self._lock:
            if not self._entries:
                return None
            newest = max(e.updated_at for e in self._entries.values())
        return time.time() - newest
