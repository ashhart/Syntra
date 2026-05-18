"""Config loading and validation for syntra-ingest.

The config is YAML. Top-level keys:

    staleness_seconds: 120          # /healthz tolerance window
    sources:
      - type: prometheus | datadog | sql | file_watch
        name: <feature_key>          # used in the snapshot dict
        interval_seconds: 30         # poll cadence
        timeout_seconds: 10          # per-poll timeout
        ...source-specific fields...

Validation is intentionally minimal: enough to fail fast on typos, not enough
to second-guess the operator. If a source-specific field is wrong, the poller
will log the error at runtime and the feature will simply not appear in the
snapshot.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml

log = logging.getLogger(__name__)

VALID_SOURCE_TYPES = {"prometheus", "datadog", "sql", "file_watch"}


@dataclass
class SourceConfig:
    """One feature source. The raw dict is preserved on `.raw` so each poller
    function can read its own fields without us re-modelling every variant."""

    type: str
    name: str
    interval_seconds: float
    timeout_seconds: float
    raw: dict[str, Any] = field(default_factory=dict)


@dataclass
class Config:
    staleness_seconds: float
    sources: list[SourceConfig]


def _coerce_source(idx: int, entry: dict[str, Any]) -> SourceConfig:
    if not isinstance(entry, dict):
        raise ValueError(f"sources[{idx}] is not a mapping")

    stype = entry.get("type")
    name = entry.get("name")

    if stype not in VALID_SOURCE_TYPES:
        raise ValueError(
            f"sources[{idx}].type={stype!r} is not one of {sorted(VALID_SOURCE_TYPES)}"
        )
    if not name or not isinstance(name, str):
        raise ValueError(f"sources[{idx}].name must be a non-empty string")

    interval = float(entry.get("interval_seconds", 30))
    timeout = float(entry.get("timeout_seconds", 10))

    if interval <= 0:
        raise ValueError(f"sources[{idx}].interval_seconds must be > 0")
    if timeout <= 0:
        raise ValueError(f"sources[{idx}].timeout_seconds must be > 0")

    return SourceConfig(
        type=stype,
        name=name,
        interval_seconds=interval,
        timeout_seconds=timeout,
        raw=entry,
    )


def load_config(path: str | Path) -> Config:
    """Read a YAML file from disk and return a validated Config."""

    p = Path(path)
    with p.open("r", encoding="utf-8") as fh:
        data = yaml.safe_load(fh) or {}

    if not isinstance(data, dict):
        raise ValueError(f"{p}: top-level YAML must be a mapping")

    staleness = float(data.get("staleness_seconds", 120))
    if staleness <= 0:
        raise ValueError("staleness_seconds must be > 0")

    raw_sources = data.get("sources") or []
    if not isinstance(raw_sources, list) or not raw_sources:
        raise ValueError("config must define a non-empty 'sources' list")

    sources = [_coerce_source(i, s) for i, s in enumerate(raw_sources)]

    # Duplicate names would silently overwrite each other in the snapshot.
    seen: set[str] = set()
    for s in sources:
        if s.name in seen:
            raise ValueError(f"duplicate source name: {s.name!r}")
        seen.add(s.name)

    log.info(
        "loaded config from %s: %d source(s), staleness=%ss",
        p,
        len(sources),
        staleness,
    )
    return Config(staleness_seconds=staleness, sources=sources)
