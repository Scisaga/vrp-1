"""Exercise refreshing and polling a running solve alongside an older result."""
import argparse
import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.request import ProxyHandler, Request, build_opener

from playwright.sync_api import expect, sync_playwright


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=root / 'target/debug/vrp-server.exe')
    args = parser.parse_args()
    folder = root / 'corpus' / f'progress-refresh-{time.time_ns()}'
    folder.mkdir()
    (folder / 'map.json').write_text('{"disabled":true}', encoding='utf-8')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    opener = build_opener(ProxyHandler({}))

    def api(path, data=None):
        with opener.open(Request(base + path, data=None if data is None else json.dumps(data).encode(),
                                 headers={'Accept': 'application/json', 'Content-Type': 'application/json'}), timeout=20) as response:
            return json.load(response)

    def wait_job(job):
        deadline = time.monotonic() + 20
        while True:
            current = api('/v1/jobs/' + job['id'])
            if current.get('hasResult'):
                return current
            assert time.monotonic() < deadline
            time.sleep(.05)

    request = {'problem': {
        'id': 'poll', 'horizon': {'startLocal': '2026-11-03T00:00:00', 'timezone': 'Asia/Shanghai'},
        'locations': [{'id': 'L', 'lng': 121.5, 'lat': 31.2}],
        'matrices': [{'durations': 'AAAAAA==', 'distances': 'AAAAAA==', 'sources': 'AA=='}],
        'agents': [{'id': 'A', 'skills': ['basic'], 'startTime': '21600', 'endTime': '43200'}],
        'tickets': [{'id': f'T{i}', 'duration': 60, 'earliestStart': '21600', 'latestStart': '43000',
                     'latestEnd': '43200', 'skillsRequired': ['basic' if i == 0 else 'expert']} for i in range(11)],
        'weights': {'ticketUnassigned': {'medium': '1000'}, 'balanceAgentLoading': {}},
    }, 'options': {'timeLimitMs': '150', 'threads': 1}}
    env = dict(os.environ)
    env.pop('VRP_API_TOKEN', None)
    with (folder / 'server.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(args.server.resolve()), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}', '--data-dir', str(folder / 'data'),
                                    '--total-threads', '2', '--map-config', str(folder / 'map.json'), '--geo-prefetch', 'false'], cwd=root,
                                   env=env, stdout=log, stderr=log,
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        try:
            deadline = time.monotonic() + 20
            while True:
                try:
                    api('/healthz')
                    break
                except OSError:
                    assert process.poll() is None and time.monotonic() < deadline
                    time.sleep(.1)
            api('/v1/scenarios', {'scenarioId': 'poll', 'name': 'Progress polling', 'request': request})
            old = wait_job(api('/v1/jobs', {'scenario': {'scenarioId': 'poll', 'version': '1'}}))
            job = api('/v1/jobs', {'scenario': {'scenarioId': 'poll', 'version': '1'},
                                    'options': {'timeLimitMs': '14000', 'threads': 1}})
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                page = browser.new_page(viewport={'width': 1200, 'height': 950})
                errors = []
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.goto(base + '/#scene/poll/plans')
                expect(page.locator('.progress-panel [data-action=cancel-job]')).to_have_attribute('data-job', job['id'], timeout=5000)
                expect(page.locator('.progress-panel')).to_have_attribute('data-progress-job', job['id'])
                expect(page.locator('.progress-chart')).to_have_count(1)
                expect(page.locator('.progress-chart .trend-series')).to_have_count(2)
                expect(page.locator('#progress-score')).to_have_value('auto')
                page.reload()
                expect(page.locator('.progress-panel')).to_have_attribute('data-progress-job', job['id'])
                expect(page.locator('.progress-chart')).to_have_count(1)
                expect(page.locator('.progress-chart .trend-series')).to_have_count(2)
                page.locator('#progress-score').select_option('hard')
                page.locator('#progress-score').focus()
                expect(page.locator('.progress-chart')).to_have_count(1)
                before = page.locator('.progress-chart').get_attribute('data-last-elapsed-ms')
                page.wait_for_function('(value)=>Number(document.querySelector(".progress-chart").dataset.lastElapsedMs)>Number(value)', arg=before, timeout=6500)
                expect(page.locator('#progress-score')).to_be_focused()
                page.locator('#progress-score').select_option('auto')
                page.locator('#progress-score').focus()
                expect(page.locator('.progress-chart')).to_have_count(1)
                expect(page.locator('.progress-chart .trend-series')).to_have_count(2)
                before = page.locator('.progress-chart').first.get_attribute('data-last-elapsed-ms')
                page.wait_for_function('(value)=>Number(document.querySelector(".progress-chart").dataset.lastElapsedMs)>Number(value)', arg=before, timeout=6500)
                page.screenshot(path=str(folder / 'running.png'), full_page=True)
                # Explicit historical selection remains stable even with a live solve.
                page.goto(base + '/#scene/poll/plans/' + old['id'])
                expect(page.locator('.progress-panel')).to_have_attribute('data-progress-job', old['id'])
                page.reload()
                expect(page.locator('.progress-panel')).to_have_attribute('data-progress-job', old['id'])
                page.goto(base + '/#scene/poll/plans/' + job['id'])
                page.locator('#progress-score').focus()
                expect(page.locator('.plan-workspace')).to_be_visible(timeout=20000)
                expect(page.locator('#progress-score')).to_be_focused()
                result = api('/v1/jobs/' + job['id'] + '/result')
                trend = api('/v1/jobs/' + job['id'] + '/trend')
                assert trend['samples'][-1]['bestScore'] == result['score']
                expect(page.locator('[data-score-level=soft] .trend-point')).to_have_count(len(trend['samples']))

                # A pinned skill mismatch cannot be repaired by unassigning.
                hard_input = copy.deepcopy(request)
                hard_input['problem']['tickets'] = [{'id': 'pinned', 'duration': 60, 'pinned': True,
                                                      'originalAgent': 0, 'skillsRequired': ['expert']}]
                api('/v1/scenarios', {'scenarioId': 'hard', 'request': hard_input})
                hard = wait_job(api('/v1/jobs', {'scenario': {'scenarioId': 'hard', 'version': '1'}}))
                assert int(hard['score']['hard']) < 0
                page.goto(base + '/#scene/hard/plans')
                expect(page.locator('.progress-chart')).to_have_count(1)
                expect(page.locator('.progress-chart')).to_have_attribute('data-score-level', 'hard')
                page.locator('#progress-score').select_option('all')
                expect(page.locator('.progress-chart')).to_have_count(1)
                expect(page.locator('.progress-chart .trend-series')).to_have_count(3)
                expect(page.locator('.progress-chart .trend-y-axis')).to_have_count(3)
                expect(page.locator('.progress-chart .trend-x-tick').first).to_be_visible()
                page.screenshot(path=str(folder / 'all-levels.png'), full_page=True)
                rules = page.evaluate('''async()=>{
                    const {progressLevels}=await import('/progress.js');
                    return [progressLevels('auto',{hard:'-1'}),progressLevels('auto',{}),
                        progressLevels('all',{hard:'-1'}),progressLevels('soft',{hard:'-1'}),progressLevels('auto')];
                }''')
                assert rules == [['hard'], ['medium', 'soft'], ['hard', 'medium', 'soft'], ['soft'], []], rules
                assert not errors, errors
                browser.close()
                print(json.dumps({'passed': True, 'folder': str(folder), 'samples': len(trend['samples']),
                                  'checks': ['running job after reload', 'polling with focused score selector',
                                             'terminal result with focused selector', 'historical selection after reload',
                                             'Hard priority', 'one plot with independent score axes', 'manual override']}))
        finally:
            process.terminate()
            process.wait(timeout=15)


if __name__ == '__main__':
    main()
