#!/usr/bin/env python3
"""Opt-in bounded-resource workload; needs Python 3, ps and a built release binary."""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('--seconds', type=int, default=600)
parser.add_argument('--output', type=Path)
args = parser.parse_args()
ROOT = Path(__file__).resolve().parent.parent
binary = ROOT / 'target/release/herdr-tokens'
with tempfile.TemporaryDirectory(prefix='ht-soak-', dir='/tmp') as tmp:
    root = Path(tmp).resolve()
    config = root / 'config'
    config.mkdir()
    workspaces = {}
    for i in range(20):
        path = root / f'w{i}'
        path.mkdir()
        workspaces[f'w{i}'] = str(path)
    (root / 'fake.json').write_text(json.dumps({'workspaces': workspaces}))
    text = 'schema_version=1\n[runtime]\nmax_concurrency=4\ndiscovery_interval_ms=1000\n'
    for i, script in enumerate(["printf '{\"status\":1}'", "sleep .1; printf '{\"status\":2}'", 'sleep .3; exit 1', 'sleep 20']):
        text += f'''\n[[collectors]]
name="c{i}"
provider="command"
command={json.dumps(['/bin/sh', '-c', script])}
interval_ms=10000
timeout_ms=1000
ttl_ms=30000
[collectors.tokens]
token{i}="status"
'''
    (config / 'tokens.toml').write_text(text)
    common = ['--config-dir', str(config), '--state-dir', str(root / 'state'), '--socket', str(root / 'api.sock'),
              '--runtime-dir', str(root / 'runtime'), '--herdr-bin', str(ROOT / 'tests/fixtures/fake-herdr')]
    log = (root / 'runner.log').open('w')
    runner = subprocess.Popen([str(binary), 'run', *common], stdout=log, stderr=log)
    samples = []
    start = time.monotonic()
    try:
        while time.monotonic() - start < args.seconds:
            time.sleep(1)
            assert runner.poll() is None, 'runner exited'
            status = subprocess.run([str(binary), 'status', '--json', *common], capture_output=True, timeout=6, check=True)
            status = json.loads(status.stdout)['result']
            jobs = status['jobs']
            running = sum(j['running'] for j in jobs)
            pending = sum(j['publication_pending'] for j in jobs)
            assert len(jobs) <= 80 and running <= 4 and pending <= 80
            rss = int(subprocess.check_output(['ps', '-o', 'rss=', '-p', str(runner.pid)]).strip())
            latencies = []
            for job in jobs:
                d = job['diagnostics']
                collected, acknowledged = d['last_collection_age_ms'], d['last_acknowledgement_age_ms']
                if collected is not None and acknowledged is not None and collected >= acknowledged:
                    latencies.append(collected - acknowledged)
            samples.append({'second': round(time.monotonic() - start, 2), 'rss_kib': rss, 'running': running,
                            'pending': pending, 'jobs': len(jobs), 'latencies_ms': latencies,
                            'missed_deadlines': sum(j['diagnostics']['missed_deadlines'] for j in jobs)})
        reports = [json.loads(s) for s in (root / 'reports.jsonl').read_text().splitlines()]
        sequences = [int(r['args'][r['args'].index('--seq') + 1]) for r in reports]
        assert all(a < b for a, b in zip(sequences, sequences[1:]))
        warm = [s for s in samples if s['second'] >= min(60, args.seconds / 4)]
        rss = [s['rss_kib'] for s in warm]
        latencies = sorted(v for s in warm for v in s['latencies_ms'])
        summary = {'platform': os.uname().sysname, 'duration_seconds': args.seconds, 'workspaces': 20, 'collectors': 4,
                   'collector_interval_ms': 10000, 'failing_collectors': 2, 'reports': len(reports),
                   'max_running': max(s['running'] for s in samples), 'max_pending': max(s['pending'] for s in samples),
                   'max_jobs': max(s['jobs'] for s in samples), 'last_missed_deadlines': samples[-1]['missed_deadlines'],
                   'post_warmup_rss_min_kib': min(rss), 'post_warmup_rss_max_kib': max(rss),
                   'first_warm_quarter_median_rss_kib': statistics.median(rss[:max(1, len(rss)//4)]),
                   'last_quarter_median_rss_kib': statistics.median(rss[-max(1, len(rss)//4):]),
                   'observed_publication_latency_p50_ms': statistics.median(latencies) if latencies else None,
                   'observed_publication_latency_p99_ms': latencies[int(.99 * (len(latencies)-1))] if latencies else None}
        print(json.dumps(summary, indent=2), flush=True)
        if args.output:
            args.output.write_text(json.dumps({'summary': summary, 'samples': samples}, indent=2) + '\n')
    finally:
        subprocess.run([str(binary), 'stop', *common], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=6)
        try:
            runner.wait(timeout=6)
        except subprocess.TimeoutExpired:
            runner.kill()
            runner.wait()
        log.close()
        # No child of the runner (including shell descendants) may retain the fixture path.
        processes = subprocess.check_output(['ps', '-axo', 'command=']).decode()
        assert str(root) not in processes, 'process leaked after shutdown'
