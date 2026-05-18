"""CLI entrypoint.

Usage:

    syntra-ingest --config /path/to/config.yaml [--host 127.0.0.1] [--port 9090]

Loads the YAML, starts one daemon thread per source, then runs the Flask app
in the foreground. SIGINT (Ctrl-C) terminates the process — the daemon
threads die with it.
"""

from __future__ import annotations

import argparse
import logging
import sys

from .cache import FeatureCache
from .config import load_config
from .poller import Poller
from .server import create_app


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="syntra-ingest")
    p.add_argument("--config", required=True, help="Path to YAML config file.")
    p.add_argument(
        "--host",
        default="127.0.0.1",
        help="Bind host. Default 127.0.0.1 (localhost only).",
    )
    p.add_argument(
        "--port",
        type=int,
        default=9090,
        help="Bind port. Default 9090.",
    )
    p.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
    )
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)

    logging.basicConfig(
        level=args.log_level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    config = load_config(args.config)
    cache = FeatureCache()
    poller = Poller(config, cache)
    poller.start()

    app = create_app(cache, staleness_seconds=config.staleness_seconds)

    # Flask's built-in server is fine for a sidecar bound to localhost.
    # If you front it with a reverse proxy, gunicorn/uwsgi are drop-ins.
    app.run(host=args.host, port=args.port, threaded=True, use_reloader=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())
