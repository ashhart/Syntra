#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"

if [[ ! -x "$LYCAN" ]]; then
  (cd "$ROOT" && cargo build --release)
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lycan-realworld.XXXXXX")"
DB="$WORK/orders.db"
AUDIT="$WORK/audit.txt"
SRC="$WORK/demo_realworld_capabilities.lycs"
PORT_FILE="$WORK/http.port"
SERVER_LOG="$WORK/http.log"

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

python3 - "$DB" <<'PY'
import sqlite3
import sys

db = sys.argv[1]
rows = [
    ("mon", 17, 38), ("mon", 18, 42), ("mon", 19, 47),
    ("tue", 17, 40), ("tue", 18, 43), ("tue", 19, 48),
    ("wed", 17, 44), ("wed", 18, 51), ("wed", 19, 59),
    ("thu", 17, 55), ("thu", 18, 71), ("thu", 19, 88),
    ("fri", 17, 108), ("fri", 18, 132), ("fri", 19, 124),
]
conn = sqlite3.connect(db)
conn.execute("create table hourly(day text, hour integer, orders integer)")
conn.executemany("insert into hourly(day, hour, orders) values (?, ?, ?)", rows)
conn.commit()
conn.close()
PY

python3 - "$PORT_FILE" >"$SERVER_LOG" 2>&1 <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import sys

port_file = sys.argv[1]

class Handler(BaseHTTPRequestHandler):
    def _send(self, payload):
        data = json.dumps(payload).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path != "/live":
            self.send_error(404)
            return
        self._send({
            "restaurant": "Friday Bento",
            "period": "friday_18_00",
            "orders": 118,
            "recentOrders": [38, 42, 47, 51, 59, 71, 88, 108, 132, 124],
            "signals": {
                "weatherBoost": 1.25,
                "eventBoost": 1.15
            }
        })

    def do_POST(self):
        size = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(size).decode("utf-8")
        self._send({
            "accepted": True,
            "bytes": len(body),
            "body": body
        })

    def log_message(self, *args):
        pass

server = HTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w", encoding="utf-8") as f:
    f.write(str(server.server_port))
server.serve_forever()
PY
SERVER_PID=$!

for _ in $(seq 1 50); do
  [[ -s "$PORT_FILE" ]] && break
  sleep 0.05
done
if [[ ! -s "$PORT_FILE" ]]; then
  echo "HTTP test server did not start" >&2
  cat "$SERVER_LOG" >&2 || true
  exit 1
fi
PORT="$(cat "$PORT_FILE")"

python3 - "$SRC" "$DB" "$AUDIT" "$PORT" <<'PY'
import sys

src, db, audit, port = sys.argv[1:5]
http = f"http://127.0.0.1:{port}"
code = f'''
;; REAL-WORLD CAPABILITY DEMO
;;
;; Lycan reads a local SQLite history database, fetches live HTTP signals,
;; parses JSON, computes demand statistics, recommends capacity, sends
;; feedback, and writes an audit record.

($ db "{db}")
($ auditPath "{audit}")
($ live (!cap "http.get" "{http}/live"))
($ restaurant (!cap "json.get" live "restaurant"))
($ currentOrders (!cap "json.get" live "orders"))
($ recentOrders (!cap "json.get" live "recentOrders"))
($ weatherBoost (!cap "json.get" live "signals.weatherBoost"))
($ eventBoost (!cap "json.get" live "signals.eventBoost"))

($ dbRows (!cap "sql.sqliteQuery" db "select round(avg(orders)), max(orders), count(*) from hourly"))
($ dbSummary (I dbRows 0))
($ dbAverage (I dbSummary 0))
($ dbPeak (I dbSummary 1))
($ dbCount (I dbSummary 2))

($ recentMean (!cap "stats.mean" recentOrders))
($ recentP95 (!cap "stats.percentile" recentOrders 95.0))
($ ewma (!cap "series.ewmaForecast" recentOrders 0.55))
($ blended (+ (* 0.6 ewma) (* 0.4 currentOrders)))
($ boosted (* (* blended weatherBoost) eventBoost))
($ instances (!cap "ops.autoScaleRecommend" boosted 45.0 2 20))

($ feedbackBody (+ "{{\\"decision\\":\\"scale\\",\\"instances\\":" (+ instances "}}")))
($ feedback (!cap "http.post" "{http}/feedback" feedbackBody "application/json"))
($ _ (!cap "file.writeText" auditPath (+ "restaurant=" (+ restaurant (+ ",instances=" instances)))))
($ audit (!cap "file.readText" auditPath))

(!p "=== LYCAN REAL-WORLD CAPABILITY DEMO ===")
(!p "restaurant:" restaurant)
(!p "sqlite_rows:" dbCount)
(!p "sqlite_avg_orders:" dbAverage)
(!p "sqlite_peak_orders:" dbPeak)
(!p "live_orders:" currentOrders)
(!p "recent_mean:" (!round recentMean))
(!p "recent_p95:" (!round recentP95))
(!p "ewma_forecast:" (!round ewma))
(!p "boosted_forecast:" (!round boosted))
(!p "recommended_instances:" instances)
(!p "feedback_accepted:" (!cap "json.get" feedback "accepted"))
(!p "audit_exists:" (!cap "file.exists" auditPath))
(!p "audit:" audit)
'''
with open(src, "w", encoding="utf-8") as f:
    f.write(code)
PY

echo "=== SOURCE RUN ==="
"$LYCAN" "$SRC"

echo
echo "=== COMPILE ==="
"$LYCAN" compile "$SRC"

echo
echo "=== BINARY RUN ==="
"$LYCAN" "${SRC%.lycs}.lyc"
