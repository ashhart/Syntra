"""Source pollers. One function per source type.

Contract: each `poll_*` function takes a `SourceConfig` and returns either a
`float` (the latest value) or `None` (transient failure). Failures are logged
here so callers don't need to. The poller loop in `poller.py` treats `None` as
"skip this tick" — the cache keeps its previous value (which may be stale, and
that staleness shows up in `_meta`).

Why this shape? Source failures should not stop the sidecar. A flaky Datadog
endpoint shouldn't take down a perfectly good SQL feed.
"""

from __future__ import annotations

import json
import logging
import os
import sqlite3
import time
from pathlib import Path
from typing import Any

import requests

from .config import SourceConfig

log = logging.getLogger(__name__)


# ----- prometheus -----------------------------------------------------------

def poll_prometheus(cfg: SourceConfig) -> float | None:
    """Scalar-style Prometheus query.

    Hits `GET {url}?query=<query>` (Prometheus `/api/v1/query`-shaped endpoint)
    and parses `data.result[0].value[1]` as a float.

    Range queries (`/api/v1/query_range`) are intentionally not supported —
    the sidecar's job is a single current value, not a window.
    """

    url = cfg.raw.get("url")
    query = cfg.raw.get("query")
    if not url or not query:
        log.error("[%s] prometheus source missing url or query", cfg.name)
        return None

    try:
        resp = requests.get(
            url,
            params={"query": query},
            timeout=cfg.timeout_seconds,
        )
        resp.raise_for_status()
        payload = resp.json()
    except Exception as exc:  # noqa: BLE001 — best-effort: log and skip
        log.warning("[%s] prometheus request failed: %s", cfg.name, exc)
        return None

    try:
        results = payload["data"]["result"]
        if not results:
            log.warning("[%s] prometheus returned no results for %r", cfg.name, query)
            return None
        # value is [timestamp, "string-value"]
        return float(results[0]["value"][1])
    except (KeyError, IndexError, TypeError, ValueError) as exc:
        log.warning("[%s] prometheus response shape unexpected: %s", cfg.name, exc)
        return None


# ----- datadog --------------------------------------------------------------

_DD_AGGS = {
    "last": lambda vs: vs[-1],
    "mean": lambda vs: sum(vs) / len(vs),
    "max": max,
    "min": min,
}


def poll_datadog(cfg: SourceConfig) -> float | None:
    """Datadog `/api/v1/query` over `from_seconds_ago` to now.

    Credentials come from env: DD_API_KEY and DD_APP_KEY. We do not read keys
    from the YAML — that's a deliberate choice to keep secrets out of config
    files.
    """

    api_key = os.environ.get("DD_API_KEY")
    app_key = os.environ.get("DD_APP_KEY")
    if not api_key or not app_key:
        log.error(
            "[%s] datadog source needs DD_API_KEY and DD_APP_KEY in env",
            cfg.name,
        )
        return None

    query = cfg.raw.get("query")
    if not query:
        log.error("[%s] datadog source missing query", cfg.name)
        return None

    from_ago = float(cfg.raw.get("from_seconds_ago", 60))
    agg = cfg.raw.get("aggregation", "last")
    if agg not in _DD_AGGS:
        log.error(
            "[%s] datadog aggregation must be one of %s, got %r",
            cfg.name,
            sorted(_DD_AGGS),
            agg,
        )
        return None

    url = cfg.raw.get("url", "https://api.datadoghq.com/api/v1/query")
    now = int(time.time())
    params = {"from": now - int(from_ago), "to": now, "query": query}
    headers = {"DD-API-KEY": api_key, "DD-APPLICATION-KEY": app_key}

    try:
        resp = requests.get(
            url,
            params=params,
            headers=headers,
            timeout=cfg.timeout_seconds,
        )
        resp.raise_for_status()
        payload = resp.json()
    except Exception as exc:  # noqa: BLE001
        log.warning("[%s] datadog request failed: %s", cfg.name, exc)
        return None

    try:
        series = payload.get("series") or []
        if not series:
            log.warning("[%s] datadog returned no series for %r", cfg.name, query)
            return None
        # pointlist is [[ts_ms, value], ...]; values may be null
        points = [p[1] for p in series[0]["pointlist"] if p[1] is not None]
        if not points:
            log.warning("[%s] datadog series had no non-null points", cfg.name)
            return None
        return float(_DD_AGGS[agg](points))
    except (KeyError, IndexError, TypeError, ValueError) as exc:
        log.warning("[%s] datadog response shape unexpected: %s", cfg.name, exc)
        return None


# ----- sql ------------------------------------------------------------------

_SQL_PREFIXES = ("SELECT", "WITH", "PRAGMA")


def poll_sql(cfg: SourceConfig) -> float | None:
    """Run a single read-only SQL query against a SQLite file.

    The query must start with SELECT, WITH, or PRAGMA. This mirrors Lycan's
    sandbox: it's not a real security boundary against a determined attacker
    with write access to the YAML, but it stops the obvious foot-gun of
    sticking a DELETE in the config.
    """

    db_path = cfg.raw.get("database_path")
    sql = cfg.raw.get("sql")
    if not db_path or not sql:
        log.error("[%s] sql source missing database_path or sql", cfg.name)
        return None

    stripped = sql.lstrip().upper()
    if not stripped.startswith(_SQL_PREFIXES):
        log.error(
            "[%s] sql source rejected: query must start with SELECT/WITH/PRAGMA",
            cfg.name,
        )
        return None

    if not Path(db_path).exists():
        log.warning("[%s] sql database not found: %s", cfg.name, db_path)
        return None

    try:
        # uri=True + mode=ro means SQLite itself refuses writes. Belt and braces.
        uri = f"file:{db_path}?mode=ro"
        conn = sqlite3.connect(uri, uri=True, timeout=cfg.timeout_seconds)
        try:
            cur = conn.execute(sql)
            row = cur.fetchone()
        finally:
            conn.close()
    except Exception as exc:  # noqa: BLE001
        log.warning("[%s] sql query failed: %s", cfg.name, exc)
        return None

    if row is None or len(row) == 0:
        log.warning("[%s] sql query returned no rows", cfg.name)
        return None

    try:
        return float(row[0])
    except (TypeError, ValueError) as exc:
        log.warning("[%s] sql result not numeric: %s", cfg.name, exc)
        return None


# ----- file_watch -----------------------------------------------------------

def poll_file_watch(cfg: SourceConfig) -> float | None:
    """Re-read a file on each poll. No inotify; simple polling.

    Format options:
      - raw_float: file contains a single number, optionally with whitespace.
      - json_path: file is JSON; `json_path` is a dot-separated key path.
    """

    path = cfg.raw.get("path")
    fmt = cfg.raw.get("format", "raw_float")
    if not path:
        log.error("[%s] file_watch source missing path", cfg.name)
        return None

    p = Path(path)
    if not p.exists():
        log.warning("[%s] file_watch path not found: %s", cfg.name, path)
        return None

    try:
        text = p.read_text(encoding="utf-8")
    except Exception as exc:  # noqa: BLE001
        log.warning("[%s] file_watch read failed: %s", cfg.name, exc)
        return None

    if fmt == "raw_float":
        try:
            return float(text.strip())
        except ValueError as exc:
            log.warning("[%s] file_watch not a float: %s", cfg.name, exc)
            return None

    if fmt == "json_path":
        json_path = cfg.raw.get("json_path")
        if not json_path:
            log.error("[%s] file_watch json_path required for format=json_path", cfg.name)
            return None
        try:
            data: Any = json.loads(text)
        except json.JSONDecodeError as exc:
            log.warning("[%s] file_watch JSON parse failed: %s", cfg.name, exc)
            return None
        cursor: Any = data
        for key in json_path.split("."):
            if isinstance(cursor, dict) and key in cursor:
                cursor = cursor[key]
            else:
                log.warning(
                    "[%s] file_watch json_path %r missed at key %r",
                    cfg.name,
                    json_path,
                    key,
                )
                return None
        try:
            return float(cursor)
        except (TypeError, ValueError) as exc:
            log.warning("[%s] file_watch json value not numeric: %s", cfg.name, exc)
            return None

    log.error("[%s] file_watch unknown format: %s", cfg.name, fmt)
    return None


# ----- dispatch -------------------------------------------------------------

POLLERS = {
    "prometheus": poll_prometheus,
    "datadog": poll_datadog,
    "sql": poll_sql,
    "file_watch": poll_file_watch,
}
