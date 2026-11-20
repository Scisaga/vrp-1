"""Synthetic admission check: two concurrent 1000-order / 200-vehicle replans.

This checks bounds, isolation, result validity and adoption; it is not a route
quality benchmark. Each job explicitly uses two threads and 1000 ms of search.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.request import Request, build_opener, ProxyHandler

from workbench_fixture import synthetic_matrix


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument('--server', type=Path, default=root / 'target/release' / ('vrp-server.exe' if os.name == 'nt' else 'vrp-server'))
    args = parser.parse_args()
    folder = root / 'corpus' / f'capacity-smoke-{time.time_ns()}'
    folder.mkdir()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    local = build_opener(ProxyHandler({}))

    def api(path, body=None):
        request = Request(base + path, data=None if body is None else json.dumps(body).encode(), headers={'Content-Type': 'application/json', 'Accept': 'application/json'})
        with local.open(request, timeout=30) as response:
            return json.load(response)

    with (folder / 'server.log').open('w') as log:
        env = dict(os.environ)
        env.pop('VRP_API_TOKEN', None)
        process = subprocess.Popen([str(args.server), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}', '--data-dir', str(folder / 'data'), '--total-threads', '4', '--geo-prefetch', 'false'], cwd=root, env=env, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        try:
            deadline = time.monotonic() + 20
            while True:
                try:
                    api('/healthz')
                    break
                except OSError:
                    assert process.poll() is None and time.monotonic() < deadline
                    time.sleep(.1)
            scenes = ['synthetic-1', 'synthetic-2']
            for scene in scenes:
                problem = {'id': scene, 'locations': [{'id': f'L{i}', 'lng': 121.4 + (i % 50) * .001, 'lat': 31.1 + (i // 50) * .001} for i in range(1001)], 'agents': [{'id': f'V{i}', 'resourceId': f'{scene}-V{i}', 'endTime': '86400'} for i in range(200)], 'tickets': [{'id': f'T{i}', 'location': i + 1, 'duration': '60', 'latestStart': '80000', 'latestEnd': '86400'} for i in range(1000)]}
                problem['matrices'] = [synthetic_matrix(problem['locations'])]
                api('/v1/scenarios', {'scenarioId': scene, 'name': scene, 'request': {'problem': problem, 'options': {'threads': 2, 'timeLimitMs': '1000'}}})
                api('/v1/operations/start', {'scenario': {'scenarioId': scene, 'version': '1'}, 'policy': {'clock': 'EXECUTION_CLOCK_REPLAY'}})
            started = time.monotonic()
            jobs = [(scene, api(f'/v1/operations/{scene}/replan', {'scenarioId': scene, 'expectedRevision': '1'})['id']) for scene in scenes]
            peak = 0
            while True:
                peak = max(peak, api('/healthz').get('reservedThreads', 0))
                assert peak <= 4
                states = [api(f'/v1/jobs/{job}') for _, job in jobs]
                if all(s.get('state') in ['JOB_STATE_SUCCEEDED', 'JOB_STATE_FAILED', 'JOB_STATE_CANCELLED'] for s in states):
                    break
                assert time.monotonic() - started < 60
                time.sleep(.02)
            for (scene, job), state in zip(jobs, states):
                assert state.get('hasResult'), state
                result = api(f'/v1/jobs/{job}/result')
                assert int(result['score'].get('hard', '0')) == 0, result['score']
                assert sum(len(r.get('tickets', [])) for r in result['solution']['routes']) == 1000
                op = api(f'/v1/operations/{scene}')
                adopted = api(f'/v1/operations/{scene}/adopt', {'scenarioId': scene, 'expectedRevision': op['revision'], 'jobId': job, 'actor': 'synthetic-capacity-check'})
                assert adopted['plan']['version'] == '1'
            report = {'scenes': 2, 'ordersPerScene': 1000, 'vehiclesPerScene': 200, 'threadsPerJob': 2, 'searchLimitMs': 1000, 'observedPeakReservedThreads': peak, 'elapsedSecondsIncludingProjectionAndAdoption': round(time.monotonic() - started, 3), 'allOrdersAssigned': True, 'adoptions': 2}
            (folder / 'summary.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
            print(json.dumps(report))
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == '__main__':
    main()
