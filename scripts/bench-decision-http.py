#!/usr/bin/env python3
"""Measure actual HTTP decisions, including rejected requests and arrival queues.

Starts an authenticated local runtime with its normal filesystem persistence.
No production data or existing store is used. Raw results stay in target/.
Open-loop response time includes time waiting for a client worker, avoiding
the coordinated-omission error of reporting only requests the server can take.
"""
import argparse
import concurrent.futures
import http.client
import json
import math
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BODY = b'{"latencies":[10,20,30,40,50]}'
CAPSULE = '''($ values (!cap "runtime.inputGet" "latencies"))
($ mean (!cap "stats.mean" values))
($ route (strategy "fast" "balanced" "safe"))
(A mean route)
'''


def valid_decision(status, data):
    if status != 200 or not isinstance(data, dict) or not data.get('decisionId') or data.get('refused'):
        return False
    decisions = data.get('decisions')
    if not isinstance(decisions, list) or len(decisions) != 1:
        return False
    chosen = decisions[0].get('chosen_option')
    return (type(chosen) is int and 0 <= chosen < 3
            and data.get('result') == f"(A 30 {['fast', 'balanced', 'safe'][chosen]})")


def summary(values):
    if not values:
        return None
    values = sorted(values)
    def pct(p):
        return values[max(0, math.ceil(len(values) * p) - 1)] * 1000
    return {"samples": len(values), "p50_ms": pct(.5), "p95_ms": pct(.95),
            "p99_ms": pct(.99), "p999_ms": pct(.999), "max_ms": values[-1]*1000,
            "over_1ms": sum(v >= .001 for v in values)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, default=ROOT/'target/decision-http.json')
    parser.add_argument('--duration', type=float, default=5)
    parser.add_argument('--rates', type=int, nargs='+', default=[100, 500, 1000, 2000])
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--require-p99-ms', type=float, help='exit 2 if any measured scenario misses this arrival-to-response p99 target')
    args = parser.parse_args()
    assert args.duration > 0 and all(r > 0 for r in args.rates)
    if not args.no_build:
        subprocess.run(['cargo', 'build', '--locked', '--release', '--bins'], cwd=ROOT, check=True)
    with tempfile.TemporaryDirectory(prefix='syntra-http-bench-') as directory:
        work = Path(directory)
        source = work/'router.lycs'
        source.write_text(CAPSULE)
        subprocess.run([str(ROOT/'target/release/lycan'), 'compile', str(source)], check=True, capture_output=True)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        key = secrets.token_hex(24)
        headers = {'Authorization': 'Bearer '+key, 'Content-Type': 'application/json'}
        path = '/v1/tenants/bench/jobs/default/capsules/router'
        # Explicitly benchmark capacity without the token quota being the bottleneck.
        env = dict(os.environ, SYNTRA_RATE_LIMIT_RPS='100000', LYCAN_RNG_SEED='42')
        with (work/'server.log').open('wb') as log:
            server = subprocess.Popen([str(ROOT/'target/release/syntra'), 'serve', '--addr', f'127.0.0.1:{port}',
                                       '--store', str(work/'store'), '--admin-key', key], env=env, stdout=log, stderr=log)
            try:
                def request(conn, suffix, body):
                    conn.request('POST', path+suffix, body, headers)
                    response = conn.getresponse()
                    data = response.read()
                    return response.status, json.loads(data)
                for _ in range(100):
                    try:
                        conn = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
                        conn.request('GET', '/health')
                        response = conn.getresponse()
                        response.read()
                        if response.status == 200:
                            break
                    except OSError:
                        time.sleep(.05)
                    finally:
                        conn.close()
                else:
                    raise RuntimeError('runtime failed to start')
                conn = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
                status, installed = request(conn, '/install', source.with_suffix('.lyc').read_bytes())
                assert status == 200, (status, installed)
                conn.close()
                local = threading.local()
                connections = []
                connection_lock = threading.Lock()
                def decision(scheduled=None, feedback=False):
                    if not hasattr(local, 'conn'):
                        local.conn = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
                        with connection_lock:
                            connections.append(local.conn)
                    start = time.perf_counter()
                    scheduled = start if scheduled is None else scheduled
                    stage = 'decide'
                    status, valid, end = 0, False, None
                    try:
                        status, data = request(local.conn, '/decide', BODY)
                        end = time.perf_counter()
                        valid = valid_decision(status, data)
                        feedback_ok = True
                        if feedback and valid:
                            stage = 'feedback'
                            s, feedback_data = request(local.conn, '/feedback', json.dumps({'decisionId': data['decisionId'], 'reward': .8}).encode())
                            feedback_ok = s == 200 and feedback_data.get('ok') is True
                        return {'status': status, 'valid': valid, 'feedback_ok': feedback_ok,
                                'error': None if valid and feedback_ok else stage+' rejected or invalid',
                                'service': end-start, 'response': end-scheduled, 'queue': start-scheduled}
                    except (OSError, ValueError, http.client.HTTPException) as exc:
                        local.conn.close()
                        end = time.perf_counter() if end is None else end
                        return {'status': status, 'valid': valid, 'feedback_ok': False,
                                'error': stage+': '+type(exc).__name__,
                                'service': end-start, 'response': end-scheduled, 'queue': start-scheduled}
                results = []
                def record(name, rows, elapsed, offered=None):
                    counts = {}
                    for r in rows:
                        counts[str(r['status'])] = counts.get(str(r['status']), 0)+1
                    errors = sum(not r['valid'] or not r['feedback_ok'] for r in rows)
                    row = {'name': name, 'requests': len(rows), 'errors': errors, 'status_counts': counts,
                           'error_types': sorted(set(r['error'] for r in rows if r['error'])),
                           'elapsed_s': elapsed, 'completed_rps': len(rows)/elapsed, 'offered_rps': offered,
                           'request_latency': summary([r['service'] for r in rows]),
                           'arrival_to_response': summary([r['response'] for r in rows]),
                           'client_queue': summary([r['queue'] for r in rows])}
                    row['passes_p99_1ms'] = errors == 0 and row['arrival_to_response']['p99_ms'] < 1
                    results.append(row)
                    print(f"{name}: {len(rows)} requests, {errors} errors, p99 {row['arrival_to_response']['p99_ms']:.3f} ms", flush=True)
                cold = decision()
                for _ in range(100):
                    assert decision()['valid'], 'warmup request failed'
                start = time.perf_counter()
                record('sequential', [decision() for _ in range(2000)], time.perf_counter()-start)
                with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                    start = time.perf_counter()
                    rows = list(pool.map(lambda i: decision(feedback=i%10 == 0), range(4000)))
                    record('8-workers-10pct-feedback', rows, time.perf_counter()-start)
                    for rate in args.rates:
                        start = time.perf_counter()
                        futures = []
                        for i in range(round(rate * args.duration)):
                            scheduled = start + i/rate
                            remaining = scheduled-time.perf_counter()
                            if remaining > 0:
                                time.sleep(remaining)
                            futures.append(pool.submit(decision, scheduled))
                        rows = [f.result() for f in futures]
                        record(f'open-loop-{rate}rps', rows, time.perf_counter()-start, rate)
                for c in connections:
                    c.close()
                output = {'scope': 'loopback HTTP, eight client threads, persistent TCP, authenticated, normal filesystem writes, rate limit 100000 rps',
                          'cold_request_ms': cold['service']*1000, 'cold_valid': cold['valid'], 'results': results}
                args.out.parent.mkdir(parents=True, exist_ok=True)
                args.out.write_text(json.dumps(output, indent=2)+'\n')
                if not cold['valid'] or any(r['errors'] for r in results):
                    raise SystemExit('invalid requests observed; inspect result counts')
                if args.require_p99_ms is not None and any(r['arrival_to_response']['p99_ms'] >= args.require_p99_ms for r in results):
                    raise SystemExit(2)
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == '__main__':
    main()
