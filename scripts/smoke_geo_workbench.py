"""End-to-end road preparation on a local synthetic PBF/catalog and isolated DB."""
import argparse
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request, ProxyHandler, build_opener
import zlib

from playwright.sync_api import expect, sync_playwright
from workbench_fixture import workbench_scenario


def varint(value):
    out = bytearray()
    while value > 127:
        out.append((value & 127) | 128)
        value >>= 7
    out.append(value)
    return bytes(out)


def field(number, value):
    if isinstance(value, bytes):
        return varint((number << 3) | 2) + varint(len(value)) + value
    return varint(number << 3) + varint(value)


def zigzag(value):
    return (value << 1) ^ (value >> 63)


def fixture_pbf():
    def block(kind, payload):
        blob = field(2, len(payload)) + field(3, zlib.compress(payload))
        header = field(1, kind.encode()) + field(3, len(blob))
        return struct.pack('>I', len(header)) + header + blob
    box = b''.join(field(i, zigzag(round(v * 1e9))) for i, v in [(1, 121.3), (2, 121.7), (3, 31.5), (4, 31.0)])
    header = field(1, box) + field(4, b'OsmSchema-V0.6') + field(16, b'vrp-1-synthetic-test') + field(32, 1793808000)
    strings = b''.join(field(1, s) for s in [b'', b'highway', b'residential', b'maxspeed', b'30'])
    nodes = []
    for node_id, lng, lat in [(1, 121.40, 31.20), (2, 121.48, 31.20), (3, 121.52, 31.20),
                              (4, 121.48, 31.26), (5, 121.40, 31.26), (6, 121.52, 31.26)]:
        node = field(1, zigzag(node_id)) + field(8, zigzag(round(lat * 1e7))) + field(9, zigzag(round(lng * 1e7)))
        nodes.append(field(1, node))
    ways = []
    for way_id, refs in [(101, [1, 2, 3]), (102, [2, 4]), (103, [5, 4, 6]), (104, [1, 5]), (105, [3, 6])]:
        previous = 0
        deltas = []
        for ref in refs:
            deltas.append(varint(zigzag(ref - previous)))
            previous = ref
        way = field(1, way_id) + field(2, b'\x01\x03') + field(3, b'\x02\x04') + field(8, b''.join(deltas))
        ways.append(field(3, way))
    primitive = field(1, strings) + field(2, b''.join(nodes + ways))
    return block('OSMHeader', header) + block('OSMData', primitive)


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=root / 'target/debug/vrp-server.exe')
    args = parser.parse_args()
    folder = root / 'corpus' / f'geo-workbench-{time.time_ns()}'
    folder.mkdir(parents=True)
    pbf = fixture_pbf()
    hits = {'pbf': 0, 'not_modified': 0}
    source_state = {'head_etag': '"synthetic-v1"'}

    class Source(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def resource(self):
            if self.path == '/index-v1.json':
                value = {'type': 'FeatureCollection', 'features': [{
                    'type': 'Feature', 'properties': {'id': 'synthetic', 'name': 'Synthetic local roads',
                        'urls': {'pbf': f'http://127.0.0.1:{source.server_port}/synthetic.osm.pbf'}},
                    'geometry': {'type': 'Polygon', 'coordinates': [[[121.0, 30.8], [121.9, 30.8], [121.9, 31.8], [121.0, 31.8], [121.0, 30.8]]]},
                }]}
                return json.dumps(value).encode(), 'application/json'
            if self.path == '/synthetic.osm.pbf':
                return pbf, 'application/octet-stream'
            if self.path == '/synthetic.osm.pbf.md5':
                return (hashlib.md5(pbf).hexdigest() + '  synthetic.osm.pbf\n').encode(), 'text/plain'
            return None, None

        def do_HEAD(self):
            self.respond(False)

        def do_GET(self):
            self.respond(True)

        def respond(self, body):
            if body and self.path == '/synthetic.osm.pbf' and self.headers.get('If-None-Match') == '"synthetic-v1"':
                hits['not_modified'] += 1
                self.send_response(304)
                self.send_header('ETag', '"synthetic-v1"')
                self.end_headers()
                return
            value, mime = self.resource()
            self.send_response(200 if value is not None else 404)
            if value is not None:
                self.send_header('Content-Type', mime)
                self.send_header('Content-Length', str(len(value)))
                self.send_header('ETag', '"synthetic-v1"' if body else source_state['head_etag'])
                self.end_headers()
                if body:
                    if self.path == '/synthetic.osm.pbf':
                        hits['pbf'] += 1
                        time.sleep(.2)
                    self.wfile.write(value)
            else:
                self.end_headers()

    source = ThreadingHTTPServer(('127.0.0.1', 0), Source)
    threading.Thread(target=source.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    opener = build_opener(ProxyHandler({}))

    def api(path, body=None, method=None):
        req = Request(base + path, data=None if body is None else json.dumps(body).encode(),
                      headers={'Accept': 'application/json', 'Content-Type': 'application/json'}, method=method)
        with opener.open(req, timeout=30) as response:
            return json.load(response)

    def wait_for(fn, predicate, seconds=35):
        deadline = time.monotonic() + seconds
        while True:
            value = fn()
            if predicate(value):
                return value
            if time.monotonic() >= deadline:
                raise AssertionError('Timed out: ' + json.dumps(value, ensure_ascii=True)[:2500])
            time.sleep(.15)

    env = dict(os.environ)
    env.pop('VRP_API_TOKEN', None)
    env['NO_PROXY'] = env['no_proxy'] = '127.0.0.1,localhost,::1'
    with (folder / 'server.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(args.server.resolve()), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}',
            '--data-dir', str(folder / 'data'), '--total-threads', '2', '--geo-catalog-url',
            f'http://127.0.0.1:{source.server_port}/index-v1.json', '--geo-road-wait-ms', '5000'],
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
            scene = workbench_scenario('geo-alpha', matrix_ready=False)
            scene['request']['options'].update(timeLimitMs='80', searchStrategy=0)
            saved = api('/v1/scenarios', scene)
            assert not saved.get('matrixReady', False)
            try:
                api('/v1/jobs', {'scenario': {'scenarioId': saved['id'], 'version': saved['version']}})
            except HTTPError as error:
                assert error.code == 409, error.code
                assert json.load(error)['code'] == 'matrix_not_ready'
            else:
                raise AssertionError('Incomplete matrix bypassed solve admission')
            concurrent = workbench_scenario('geo-concurrent', matrix_ready=False)
            concurrent['request']['options'].update(timeLimitMs='80', searchStrategy=0)
            api('/v1/scenarios', concurrent)
            wait_for(lambda: api('/v1/road-packages'), lambda v: bool(v.get('packages')))
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                page = browser.new_page(viewport={'width': 1500, 'height': 1000})
                page.route('https://**', lambda route: route.abort())
                errors = []
                browser_writes = []
                ui_task_state = {'scenario': 'geo-alpha', 'state': 'TASK_STATE_RUNNING'}
                def task_view(route):
                    response = route.fetch()
                    value = response.json()
                    if 'scenario_id=' + ui_task_state['scenario'] in route.request.url:
                        for item in value.get('tasks', []):
                            if item.get('kind') == 'TASK_KIND_GEO_PLAN' and ui_task_state['state']:
                                item['state'] = ui_task_state['state']
                                item['progress'] = {'phase': 'matrix', 'message': 'Synthetic road preparation progress', 'completed': '1', 'total': '3'}
                                if ui_task_state['state'] == 'TASK_STATE_FAILED':
                                    item['error'] = {'code': 'invalid', 'message': 'Synthetic road preparation failure'}
                    route.fulfill(response=response, json=value)
                page.route(base + '/v1/tasks?**', task_view)
                page.add_init_script('''
                    window.completionToasts = [];
                    let visibleCompletion = '';
                    const check = () => {
                        const node = document.querySelector('#notice');
                        const text = document.querySelector('#notice-text')?.textContent || '';
                        const current = node && !node.hidden && /方案.*已就绪/.test(text) ? text : '';
                        if (current && current !== visibleCompletion) window.completionToasts.push(current);
                        visibleCompletion = current;
                    };
                    new MutationObserver(check).observe(document, {subtree:true, childList:true, attributes:true, attributeFilter:['hidden']});
                ''')
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.on('request', lambda request: browser_writes.append(request.url)
                        if request.method == 'POST' and request.url.startswith(base) else None)
                page.goto(base + '/#scene/geo-alpha/data')
                expect(page.locator('.global-nav [data-nav=roads]')).to_be_visible()
                expect(page.locator('.scene-heading [data-action=solve]')).to_have_count(0)
                page.locator('.scene-more summary').click()
                expect(page.locator('.scene-more [data-action=solve-existing]')).to_be_disabled()
                page.locator('.scene-heading [data-action=geo-plan]').click()
                expect(page.locator('#dialog')).to_be_visible()
                expect(page.locator('#dialog-title')).to_have_text('生成道路矩阵并规划')
                expect(page.locator('[name=timeLimit]')).to_have_value('0.08')
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible()
                parent = wait_for(lambda: api('/v1/tasks?scenario_id=geo-alpha'),
                    lambda v: any(t.get('kind') == 'TASK_KIND_GEO_PLAN' for t in v.get('tasks', [])))
                parent_id = next(t['id'] for t in parent['tasks'] if t.get('kind') == 'TASK_KIND_GEO_PLAN')
                task = wait_for(lambda: api('/v1/tasks/' + parent_id),
                    lambda t: t.get('state') in ['TASK_STATE_SUCCEEDED', 'TASK_STATE_PARTIAL', 'TASK_STATE_FAILED', 'TASK_STATE_INTERRUPTED'], seconds=60)
                assert task.get('state') == 'TASK_STATE_SUCCEEDED', task
                expect(page.locator('.geo-workflow-status')).to_contain_text('Synthetic road preparation progress')
                page.wait_for_timeout(2800)
                expect(page.locator('.geo-workflow-status')).to_be_visible()
                assert page.evaluate('completionToasts.length') == 0
                ui_task_state['state'] = ''
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=refresh]').click()
                expect(page.locator('#notice-text')).to_contain_text('方案 #' + task['resultJobId'] + ' 已就绪', timeout=10000)
                page.wait_for_function('completionToasts.length === 1')
                expect(page.locator('.geo-readiness')).to_have_count(0)
                expect(page.locator('#notice')).to_be_hidden(timeout=6500)
                page.wait_for_timeout(2800)
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=refresh]').click()
                assert page.evaluate('completionToasts.length') == 1
                page.reload()
                expect(page.locator('.plan-workspace')).to_be_visible(timeout=12000)
                page.wait_for_timeout(2800)
                assert page.evaluate('completionToasts.length') == 0
                expect(page.locator('#notice')).to_be_hidden()
                assert base + '/v1/scenarios/geo-alpha/geo-plan' in browser_writes
                assert base + '/v1/jobs' not in browser_writes
                assert task.get('resultJobId'), task
                assert not any('coverage is partial' in warning for warning in task.get('warnings', [])), task
                quality = next(a for a in task.get('artifacts', []) if a['id'] == 'quality')
                report = api(quality['href'])
                assert report
                frozen = api('/v1/jobs/' + task['resultJobId'] + '/request')
                for matrix in frozen['problem']['matrices']:
                    values = struct.unpack('<' + 'I' * (len(base64.b64decode(matrix['durations'])) // 4), base64.b64decode(matrix['durations']))
                    assert 4294967294 not in values
                    assert 2 in base64.b64decode(matrix['sources'])
                geometry = api('/v1/jobs/' + task['resultJobId'] + '/geometry')
                assert geometry.get('legs'), geometry
                assert any('roadStartPointIndex' in leg and 'roadEndPointIndex' in leg for leg in geometry['legs'])
                expect(page.locator('#selected-job')).to_have_value(task['resultJobId'], timeout=12000)
                # A second real completion verifies explicit close independently
                # of the first toast's automatic expiry.
                page.goto(base + '/#scene/geo-concurrent/data')
                expect(page.locator('[data-sheet=tickets]')).to_be_visible()
                page.locator('.scene-heading [data-action=geo-plan]').click()
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible()
                expect(page.locator('#notice-text')).to_contain_text('已就绪', timeout=12000)
                expect(page.locator('#notice')).to_be_visible()
                page.locator('#notice-close').click()
                expect(page.locator('#notice')).to_be_hidden()
                assert page.evaluate('completionToasts.length') == 1
                page.wait_for_timeout(2800)
                assert page.evaluate('completionToasts.length') == 1
                # Error and progress banners retain durable, actionable detail.
                # This changes only the isolated browser's GET representation.
                ui_task_state.update(scenario='geo-concurrent', state='TASK_STATE_FAILED')
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=refresh]').click()
                expect(page.locator('.geo-workflow-status')).to_contain_text('Synthetic road preparation failure')
                page.wait_for_timeout(4800)
                expect(page.locator('.geo-workflow-status')).to_contain_text('Synthetic road preparation failure')
                assert page.evaluate('completionToasts.length') == 1
                ui_task_state['state'] = ''
                page.unroute(base + '/v1/tasks?**', task_view)
                page.locator('.global-nav [data-nav=tasks]').click()
                expect(page.locator('#global-content h1')).to_have_text('任务中心')
                page.goto(base + '/#tasks/' + parent_id)
                expect(page.locator('.generic-task-detail')).to_be_visible()
                with page.expect_download() as download:
                    page.locator('.generic-task-detail [data-action=download-task-artifact][data-artifact=quality]').first.click()
                assert download.value.suggested_filename
                page.reload()
                expect(page.locator('.generic-task-detail')).to_be_visible()
                page.locator('.global-nav [data-nav=roads]').click()
                expect(page.locator('#global-content h1')).to_have_text('路网包管理')
                page.locator('[data-action=road-detail]').first.click()
                expect(page.locator('.road-package-detail')).to_be_visible()
                page.screenshot(path=str(folder / 'road-packages.png'), full_page=True)
                page.set_viewport_size({'width': 430, 'height': 900})
                expect(page.locator('.global-nav [data-nav=roads]')).to_be_visible()
                page.screenshot(path=str(folder / 'road-packages-mobile.png'), full_page=True)
                assert not errors, errors
                browser.close()
            second = workbench_scenario('geo-beta', matrix_ready=False)
            second['request']['options'].update(timeLimitMs='80', searchStrategy=0)
            api('/v1/scenarios', second)
            time.sleep(.7)
            assert hits['pbf'] == 1, hits
            package = api('/v1/road-packages')['packages'][0]
            preparation = {'idempotencyKey': 'same-source-prepare', 'input': {'roadPackage': {'regionId': 'synthetic'}}}
            one = api('/v1/tasks', preparation)
            checked = wait_for(lambda: api('/v1/tasks/' + one['id']), lambda t: t.get('state') in ['TASK_STATE_SUCCEEDED', 'TASK_STATE_FAILED'])
            assert checked.get('state') == 'TASK_STATE_SUCCEEDED', checked
            assert api('/v1/tasks', preparation)['id'] == one['id']
            try:
                api('/v1/tasks', {'idempotencyKey': 'same-source-prepare', 'input': {'roadPackage': {'regionId': 'different'}}})
            except HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Task idempotency accepted different input')
            unchanged = api('/v1/road-packages/' + package['id'] + '/update', {})
            checked = wait_for(lambda: api('/v1/tasks/' + unchanged['id']), lambda t: t.get('state') in ['TASK_STATE_SUCCEEDED', 'TASK_STATE_FAILED'])
            assert checked.get('state') == 'TASK_STATE_SUCCEEDED', checked
            assert checked.get('progress', {}).get('phase') == 'source_cache_hit', checked
            assert hits['pbf'] == 1, hits
            source_state['head_etag'] = '"changed-head-validator"'
            conditional = api('/v1/road-packages/' + package['id'] + '/update', {})
            checked = wait_for(lambda: api('/v1/tasks/' + conditional['id']), lambda t: t.get('state') in ['TASK_STATE_SUCCEEDED', 'TASK_STATE_FAILED'])
            assert checked.get('state') == 'TASK_STATE_SUCCEEDED', checked
            assert hits['pbf'] == 1 and hits['not_modified'] == 1, hits
            source_state['head_etag'] = '"synthetic-v1"'
            forced = api('/v1/road-packages/' + package['id'] + '/update', {'forceDownload': True})
            downloaded = wait_for(lambda: api('/v1/tasks/' + forced['id']), lambda t: t.get('state') in ['TASK_STATE_SUCCEEDED', 'TASK_STATE_FAILED'])
            assert downloaded.get('state') == 'TASK_STATE_SUCCEEDED', downloaded
            assert hits['pbf'] == 2, hits
            assert len(api('/v1/road-packages')['packages']) == 1
            try:
                api('/v1/road-packages/' + package['id'], method='DELETE')
            except HTTPError as error:
                assert error.code == 409
            else:
                raise AssertionError('Referenced road source was deleted')
            print(json.dumps({'ok': True, 'network_pbf_downloads': hits['pbf'], 'task': parent_id,
                              'result_job': task['resultJobId'], 'artifacts': str(folder)}, ensure_ascii=True))
        finally:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            source.shutdown()


if __name__ == '__main__':
    main()
