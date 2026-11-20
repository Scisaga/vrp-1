"""Delete historical plans through the UI using an isolated synthetic service."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener

from playwright.sync_api import expect, sync_playwright
from smoke_geo_workbench import fixture_pbf
from workbench_fixture import workbench_scenario


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=root / 'target/debug/vrp-server.exe')
    parser.add_argument('--static-only', action='store_true')
    args = parser.parse_args()
    folder = root / 'corpus' / f'plan-delete-{time.time_ns()}'
    folder.mkdir()
    (folder / 'map.json').write_text('{"disabled":true}', encoding='utf-8')
    source = folder / 'synthetic.osm.pbf'
    source.write_bytes(fixture_pbf())
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    opener = build_opener(ProxyHandler({}))

    def api(path, body=None, method=None):
        request = Request(base + path, data=None if body is None else json.dumps(body).encode(),
                          headers={'Accept': 'application/json', 'Content-Type': 'application/json'}, method=method)
        with opener.open(request, timeout=30) as response:
            return json.load(response)

    def wait(read, predicate, seconds=30):
        deadline = time.monotonic() + seconds
        while True:
            value = read()
            if predicate(value):
                return value
            assert time.monotonic() < deadline, json.dumps(value, ensure_ascii=True)[:1500]
            time.sleep(.05)

    def absent(path):
        try:
            api(path)
        except HTTPError as error:
            assert error.code == 404
        else:
            raise AssertionError('Deleted record remains: ' + path)

    def rejected(path):
        try:
            api(path, method='DELETE')
        except HTTPError as error:
            assert error.code == 409
            assert json.load(error).get('message')
        else:
            raise AssertionError('Protected record was deleted: ' + path)

    def scene(scene_id, ready=True):
        value = workbench_scenario(scene_id, matrix_ready=ready)
        value['request']['options'].update(timeLimitMs='80', searchStrategy=0)
        return api('/v1/scenarios', value)

    def solve(scene_id, version='1', milliseconds=80):
        return api('/v1/jobs', {'scenario': {'scenarioId': scene_id, 'version': str(version)},
                               'options': {'timeLimitMs': str(milliseconds), 'threads': 1}})

    def done(job):
        value = wait(lambda: api('/v1/jobs/' + job['id']),
                     lambda value: value.get('state') not in [None, 'JOB_STATE_QUEUED', 'JOB_STATE_RUNNING', 'JOB_STATE_CANCELLING'])
        assert value.get('hasResult'), value
        return value

    def task_done(task):
        value = wait(lambda: api('/v1/tasks/' + task['id']),
                     lambda value: value.get('state') in ['TASK_STATE_SUCCEEDED', 'TASK_STATE_PARTIAL', 'TASK_STATE_FAILED'], 45)
        assert value.get('state') == 'TASK_STATE_SUCCEEDED', value
        return value

    env = dict(os.environ)
    env.pop('VRP_API_TOKEN', None)
    with (folder / 'server.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(args.server.resolve()), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}',
            '--data-dir', str(folder / 'data'), '--total-threads', '2', '--geo-prefetch', 'false',
            '--geo-catalog-url', '', '--geo-road-wait-ms', '0', '--map-config', str(folder / 'map.json')],
            cwd=root, env=env, stdout=log, stderr=log,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        try:
            deadline = time.monotonic() + 25
            while True:
                try:
                    api('/healthz')
                    break
                except OSError:
                    assert process.poll() is None and time.monotonic() < deadline
                    time.sleep(.1)
            scene('static')
            older, newer = done(solve('static')), done(solve('static'))
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                page = browser.new_page(viewport={'width': 1500, 'height': 1000})
                page.route('https://**', lambda route: route.abort())
                errors, delete_requests = [], []
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.on('request', lambda request: delete_requests.append(request.url) if request.method == 'DELETE' else None)

                def open_job(scene_id, job):
                    page.goto(base + f'/#scene/{scene_id}/plans/' + job['id'])
                    if job.get('hasResult'):
                        expect(page.locator('#selected-job')).to_have_value(job['id'], timeout=15000)
                    else:
                        expect(page.locator('.progress-panel[data-progress-job="' + job['id'] + '"]')).to_be_visible(timeout=15000)

                def delete_button(job):
                    return page.locator('[data-action=delete-plan][data-job="' + job['id'] + '"]').first

                def confirm_delete(job):
                    expect(delete_button(job)).to_be_enabled()
                    delete_button(job).click()
                    expect(page.locator('#dialog-title')).to_have_text('删除历史方案')
                    expect(page.locator('#dialog-submit')).to_have_text('删除方案')
                    page.locator('#dialog-submit').click()
                    expect(page.locator('#dialog')).not_to_be_visible()

                def known_protected(scene_id, job):
                    open_job(scene_id, job)
                    expect(delete_button(job)).to_be_disabled()
                    assert re.search('[\u3400-\u9fff]', delete_button(job).get_attribute('title') or '')
                    rejected('/v1/jobs/' + job['id'])
                    assert api('/v1/jobs/' + job['id'])['id'] == job['id']

                open_job('static', older)
                delete_button(older).click()
                expect(page.locator('#dialog-title')).to_have_text('删除历史方案')
                before_cancel = len(delete_requests)
                page.locator('#dialog-cancel').click()
                assert len(delete_requests) == before_cancel
                assert api('/v1/jobs/' + older['id'])['id'] == older['id']
                confirm_delete(older)
                expect(page.locator('#selected-job')).to_have_value(newer['id'])
                absent('/v1/jobs/' + older['id'])
                absent('/v1/jobs/' + older['id'] + '/request')
                absent('/v1/tasks/' + older['id'])
                page.reload()
                expect(page.locator('#selected-job')).to_have_value(newer['id'])
                assert older['id'] not in page.locator('#selected-job option').evaluate_all('(els)=>els.map(el=>el.value)')
                confirm_delete(newer)
                expect(page.locator('#selected-job')).to_have_count(0)
                expect(page.locator('#workspace-content')).to_contain_text('从业务数据生成第一版方案')
                assert not page.url.endswith('/' + newer['id'])
                page.reload()
                expect(page.locator('#workspace-content')).to_contain_text('从业务数据生成第一版方案')
                assert not api('/v1/jobs?scenario_id=static').get('jobs')
                assert not api('/v1/tasks?scenario_id=static').get('tasks')
                if args.static_only:
                    assert not errors, errors
                    browser.close()
                    print(json.dumps({'passed': True, 'staticOnly': True, 'artifacts': str(folder)}))
                    return

                scene('lineage')
                parent = done(solve('lineage'))
                derived = done(api('/v1/jobs/' + parent['id'] + '/continue', {'options': {'timeLimitMs': '80', 'threads': 1}}))
                known_protected('lineage', parent)
                assert api('/v1/jobs/' + derived['id'])['sourceJobId'] == parent['id']
                scene('adopted')
                adopted = done(solve('adopted'))
                api('/v1/operations/start', {'scenario': {'scenarioId': 'adopted', 'version': '1'},
                    'initialJobId': adopted['id'], 'at': '28800', 'policy': {'clock': 'EXECUTION_CLOCK_REPLAY', 'autoReplan': False}})
                known_protected('adopted', adopted)
                scene('running')
                running = solve('running', milliseconds=60000)
                running = wait(lambda: api('/v1/jobs/' + running['id']), lambda job: job.get('state') == 'JOB_STATE_RUNNING')
                known_protected('running', running)
                api('/v1/jobs/' + running['id'] + '/cancel', {})
                wait(lambda: api('/v1/jobs/' + running['id']), lambda job: job.get('state') in ['JOB_STATE_CANCELLED', 'JOB_STATE_SUCCEEDED'])

                imported = task_done(api('/v1/road-packages/import', {'path': str(source), 'name': 'Synthetic shared roads', 'regionId': 'synthetic'}))
                package = api('/v1/road-packages')['packages'][0]
                scene('roads', ready=False)
                first = task_done(api('/v1/scenarios/roads/geo-plan', {'expectedVersion': '1', 'idempotencyKey': 'roads-first', 'options': {'timeLimitMs': '80', 'threads': 1}}))
                version = api('/v1/scenarios/roads/metadata')['version']
                second = task_done(api('/v1/scenarios/roads/geo-plan', {'expectedVersion': version, 'idempotencyKey': 'roads-second', 'options': {'timeLimitMs': '80', 'threads': 1}}))
                first_job, second_job = api('/v1/jobs/' + first['resultJobId']), api('/v1/jobs/' + second['resultJobId'])
                first_children = [api('/v1/tasks/' + child) for child in first.get('childIds', [])]
                first_matrix = next(child for child in first_children if child.get('kind') == 'TASK_KIND_MATRIX_PREPARE')
                first_geometry = next(child for child in first_children if child.get('kind') == 'TASK_KIND_GEOMETRY_GENERATE')
                frozen = api('/v1/jobs/' + second_job['id'] + '/request')
                assert api('/v1/jobs/' + first_job['id'] + '/geometry').get('legs')
                assert api('/v1/jobs/' + second_job['id'] + '/geometry').get('legs')
                saved = api('/v1/scenarios/roads')
                retained_old_input = api('/v1/scenarios/roads?version=' + str(first['resultScenarioVersion']))['request']
                network = next(artifact for artifact in first_matrix['artifacts'] if artifact['id'] == 'network')
                source_hash = hashlib.sha256(source.read_bytes()).hexdigest()
                open_job('roads', first_job)
                confirm_delete(first_job)
                expect(page.locator('#selected-job')).to_have_value(second_job['id'])
                for path in ['/v1/jobs/' + first_job['id'], '/v1/tasks/' + first['id'], '/v1/tasks/' + first_geometry['id']]:
                    absent(path)
                assert api('/v1/tasks/' + first_matrix['id'])['id'] == first_matrix['id']
                assert api('/v1/tasks/' + imported['id'])['id'] == imported['id']
                assert api('/v1/tasks/' + second['id'])['id'] == second['id']
                assert api('/v1/road-packages/' + package['id'])['sha256'] == package['sha256']
                assert hashlib.sha256(source.read_bytes()).hexdigest() == source_hash
                assert api('/v1/scenarios/roads')['request'] == saved['request']
                assert api('/v1/scenarios/roads?version=' + str(first['resultScenarioVersion']))['request'] == retained_old_input
                assert api('/v1/jobs/' + second_job['id'] + '/request') == frozen
                with opener.open(Request(base + network['href'], headers={'Accept': 'application/octet-stream'}), timeout=20) as response:
                    assert response.status == 200 and response.read()
                page.reload()
                expect(page.locator('#selected-job')).to_have_value(second_job['id'])

                # An independent geometry task is an external retained reference,
                # so deletion must fail visibly rather than collecting that task.
                second_children = [api('/v1/tasks/' + child) for child in second.get('childIds', [])]
                second_matrix = next(child for child in second_children if child.get('kind') == 'TASK_KIND_MATRIX_PREPARE')
                external = task_done(api('/v1/tasks', {'input': {'geometry': {'jobId': second_job['id'], 'networkArtifact': second_matrix['id']}}}))
                open_job('roads', second_job)
                expect(delete_button(second_job)).to_be_enabled()
                delete_button(second_job).click()
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog-error')).to_be_visible()
                assert page.locator('#dialog-error').inner_text().strip()
                expect(page.locator('#dialog')).to_be_visible()
                assert api('/v1/jobs/' + second_job['id'])['id'] == second_job['id']
                assert api('/v1/tasks/' + external['id'])['id'] == external['id']
                page.locator('#dialog-cancel').click()
                expect(page.locator('#selected-job')).to_have_value(second_job['id'])
                page.screenshot(path=str(folder / 'protected-plan.png'), full_page=True)
                assert not errors, errors
                browser.close()
            print(json.dumps({'passed': True, 'artifacts': str(folder), 'checks': [
                'cancel is read-only', 'old selection switches to remaining plan', 'last plan empty state',
                'reload retains deletion', 'running/adopted/source references protected',
                'GeoPlan controller and private geometry collected', 'matrix snapshot and shared package retained',
                'external reference displays backend rejection', 'no browser errors']}))
        finally:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == '__main__':
    main()
