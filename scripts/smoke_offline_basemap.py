"""Offline basemap acceptance: isolated DB, local PMTiles/Range source, fresh browser."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
from urllib.request import Request, ProxyHandler, build_opener

from playwright.sync_api import expect, sync_playwright
from basemap_fixture import archive
from workbench_fixture import workbench_scenario


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=root / 'target/debug/vrp-server.exe')
    args = parser.parse_args()
    folder = root / 'corpus' / f'offline-basemap-{time.time_ns()}'
    folder.mkdir(parents=True)
    data = archive(leaves=True)
    hits = {'catalog': 0, 'range': 0, 'head': 0}
    source_online = True

    class Source(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            hits['head'] += 1
            self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.send_header('ETag', '"synthetic-v1"')
            self.end_headers()

        def do_GET(self):
            assert not self.headers.get('Authorization'), 'Token leaked to external map source'
            if not source_online:
                self.send_error(503)
                return
            if self.path == '/builds.json':
                hits['catalog'] += 1
                body = json.dumps([{'key': 'synthetic.pmtiles', 'version': '4.0.0', 'uploaded': '2026-11-12'}]).encode()
                self.send_response(200)
            else:
                hits['range'] += 1
                if hits['range']==1:
                    time.sleep(.5)
                assert self.headers['If-Match'] == '"synthetic-v1"'
                start, end = map(int, self.headers['Range'].removeprefix('bytes=').split('-'))
                body = data[start:end + 1]
                self.send_response(206)
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(data)}')
            self.send_header('Content-Length', str(len(body)))
            self.send_header('ETag', '"synthetic-v1"')
            self.end_headers()
            self.wfile.write(body)

    source = ThreadingHTTPServer(('127.0.0.1', 0), Source)
    threading.Thread(target=source.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    opener = build_opener(ProxyHandler({}))

    def api(path, value=None):
        r = Request(base + path, None if value is None else json.dumps(value).encode(), headers={'Content-Type': 'application/json'})
        with opener.open(r, timeout=30) as response:
            return json.load(response)

    def wait(fn, predicate, seconds=40):
        deadline = time.monotonic() + seconds
        while True:
            result = fn()
            if predicate(result):
                return result
            assert time.monotonic() < deadline, json.dumps(result)
            time.sleep(.1)

    env = dict(os.environ)
    test_token='offline-basemap-acceptance-token'
    env['VRP_API_TOKEN']=test_token
    env['NO_PROXY'] = env['no_proxy'] = '127.0.0.1,localhost,::1'
    with (folder / 'server.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(args.server.resolve()), '--bind', f'127.0.0.1:{port}',
            '--data-dir', str(folder / 'data'), '--total-threads', '4', '--geo-prefetch', 'false',
            '--basemap-catalog-url', f'http://127.0.0.1:{source.server_port}/builds.json',
            '--basemap-archive-base-url', f'http://127.0.0.1:{source.server_port}'], cwd=root, env=env,
            stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        try:
            deadline = time.monotonic() + 20
            while True:
                try:
                    api('/healthz')
                    break
                except OSError:
                    assert process.poll() is None and time.monotonic() < deadline
                    time.sleep(.1)
            scene = workbench_scenario('offline-alpha')
            scene['request']['options'].update(timeLimitMs='50', searchStrategy=0)
            saved = api('/v1/scenarios', scene)
            before = api('/v1/scenarios/offline-alpha')
            job = api('/v1/jobs', {'scenario': {'scenarioId': saved['id'], 'version': saved['version']}})
            wait(lambda: api('/v1/jobs/' + job['id']), lambda j: j.get('hasResult'))
            url = base + f'/#scene/offline-alpha/plans/{job["id"]}'
            with sync_playwright() as pw:
                browser = pw.chromium.launch(args=['--enable-unsafe-swiftshader','--host-resolver-rules=MAP workbench.test 127.0.0.1','--proxy-server=direct://'])
                context = browser.new_context(viewport={'width': 1500, 'height': 1000})
                page = context.new_page()
                errors = []
                page.on('pageerror', lambda e: errors.append(str(e)))
                page.route('https://tile.openstreetmap.org/**', lambda route: route.abort())
                page.goto(url)
                wait(lambda:api('/v1/tasks?kind=TASK_KIND_BASEMAP_PREPARE'),lambda result:result.get('tasks'))
                with ThreadPoolExecutor(max_workers=3) as pool:
                    same_context={'context':{'jobId':job['id']},'automatic':True,'nativeMaxZoom':14}
                    another_context={'context':{'scenarioId':saved['id'],'version':saved['version']},'automatic':True,'nativeMaxZoom':14}
                    ids=list(pool.map(lambda request:api('/v1/basemap-packages/prepare',request)['id'],[same_context,same_context,another_context]))
                assert len(set(ids))==1, ids
                expect(page.locator('.offline-basemap-status')).to_contain_text('离线底图可用', timeout=45000)
                tasks = api('/v1/tasks?kind=TASK_KIND_BASEMAP_PREPARE').get('tasks', [])
                assert len(tasks) == 1, tasks
                selection = api(f'/v1/basemaps/selection?job_id={job["id"]}')
                package = selection['package']
                assert package['nativeMaxZoom'] == 14
                assert before['request'] == api('/v1/scenarios/offline-alpha')['request']
                expect(page.locator('.map-frame')).to_have_class(__import__('re').compile('interactive'))
                page.wait_for_timeout(1200)
                # A style switch preserves the same canvas and camera/markers.
                marker = page.locator('.maplibregl-marker').first
                transform = marker.get_attribute('style') if marker.count() else None
                page.locator('[data-basemap-tone="standard"]').click()
                page.wait_for_timeout(800)
                if transform:
                    assert marker.get_attribute('style') == transform
                for _ in range(6):
                    page.locator('.maplibregl-ctrl-zoom-in').click()
                page.wait_for_timeout(600)
                assert len(api('/v1/tasks?kind=TASK_KIND_BASEMAP_PREPARE').get('tasks', [])) == 1
                context.close()

                # Fresh browser context: no HTTP cache, no local storage. The
                # upstream is unavailable and all external requests are blocked.
                source_online = False
                snapshot = dict(hits)
                offline_context = browser.new_context(viewport={'width': 1500, 'height': 1000})
                external, glyphs, archives, failures, console_errors = [], [], [], [], []
                def network(route):
                    if not route.request.url.startswith(base + '/'):
                        external.append(route.request.url)
                        route.abort()
                    elif route.request.url == base + '/map.js':
                        route.fulfill(content_type='text/javascript',body=(root/'web/map.js').read_text(encoding='utf-8')+'\nwindow.__basemapTest=()=>({center:map?.getCenter().toArray(),zoom:map?.getZoom(),key:activeStyleKey,styleReady,camera:styleCamera,loaded:map?.areTilesLoaded(),source:map?.getStyle().sources.basemap,features:map?.queryRenderedFeatures().map(f=>({layer:f.layer.id,name:f.properties["name:zh"]}))});\n')
                    else:
                        route.continue_()
                offline_context.route('**/*', network)
                page = offline_context.new_page()
                page.on('pageerror', lambda e: errors.append(str(e)))
                page.on('console', lambda m: console_errors.append(m.text) if m.type == 'error' else None)
                page.on('response',lambda r: console_errors.append(f'HTTP {r.status}: {r.url}') if r.status>=400 else None)
                page.on('request', lambda r: glyphs.append(r.url) if '/fonts/' in r.url else archives.append(r.url) if r.url.endswith('.pmtiles') else None)
                page.on('requestfailed', lambda r: failures.append(r.url))
                page.goto(url)
                try:
                    expect(page.locator('.offline-basemap-status')).to_contain_text('离线底图可用', timeout=20000)
                except Exception:
                    print(json.dumps({'map':page.evaluate('window.__basemapTest?.()'),'external':external,'failures':failures,'console':console_errors},ensure_ascii=True))
                    page.screenshot(path=str(folder/'failure.png'))
                    raise
                page.wait_for_timeout(1600)
                map_state=page.evaluate('window.__basemapTest()')
                page.screenshot(path=str(folder/'map-debug.png'))
                assert glyphs and archives, (glyphs, archives, console_errors)
                assert any(f['name']=='离线测试' for f in map_state['features']), map_state
                substantive_errors=[e for e in console_errors if '404' not in e and 'Failed to load resource: the server responded with a status of 404' not in e]
                assert not external and not failures and not errors and not substantive_errors, (external, failures, errors, console_errors)
                assert hits == snapshot, (hits, snapshot)
                page.screenshot(path=str(folder / 'offline.png'))
                # Native Chinese labels were decoded and rendered by MapLibre.
                place = page.evaluate("""async () => { const {managedResource}=await import('/basemaps.js'); return managedResource(location.origin+'/v1/basemaps/example/archive.pmtiles') && !managedResource('https://example.com/v1/basemaps/x/archive.pmtiles'); }""")
                assert place
                page.locator('.offline-basemap-status button', has_text='准备详细底图').click()
                expect(page.locator('#dialog-title')).to_contain_text('详细底图')
                page.locator('#dialog-submit').click()
                # Reuse the pinned same source: HEAD may succeed, Range is
                # deliberately unavailable, so detail failure keeps the old pack.
                expect(page.locator('#dialog-error')).to_be_visible(timeout=20000)
                page.locator('#dialog-cancel').click()
                assert api(f'/v1/basemaps/selection?job_id={job["id"]}')['package']['id'] == package['id']
                source_online=True
                page.locator('.offline-basemap-status button',has_text='准备详细底图').click()
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog-title')).to_have_text('确认准备离线底图',timeout=25000)
                expect(page.locator('#dialog-fields')).to_contain_text('预计新增传输')
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible(timeout=20000)
                detailed=wait(lambda:api(f'/v1/basemaps/selection?job_id={job["id"]}'),lambda value:value.get('package',{}).get('nativeMaxZoom')==15)
                assert detailed['package']['id']!=package['id']
                assert any(p['id']==package['id'] for p in api('/v1/basemap-packages')['packages'])
                page.goto(url)
                expect(page.locator('.offline-basemap-status')).to_contain_text('原生 0–15',timeout=20000)
                page.locator('[data-nav="roads"]').click()
                page.locator('[data-action="package-tab"][data-package-tab="basemaps"]').click()
                expect(page.locator('#global-content h1')).to_have_text('底图包管理')
                expect(page.locator('#global-content')).to_contain_text('0–14')
                offline_context.close()

                remote_base=f'http://workbench.test:{port}'
                remote_context=browser.new_context(viewport={'width':1500,'height':1000})
                protected,denied,remote_external=[],[],[]
                def remote_route(route):
                    if route.request.url.startswith(remote_base+'/'):
                        if '/v1/basemap-assets/' in route.request.url or '/v1/basemaps/' in route.request.url and not '/selection' in route.request.url:
                            protected.append((route.request.url,route.request.headers.get('authorization')))
                        route.continue_()
                    else:
                        remote_external.append(route.request.url);route.abort()
                remote_context.route('**/*',remote_route)
                remote=remote_context.new_page()
                remote.on('response',lambda response:denied.append(response.url) if response.status in [401,403] and ('/v1/basemaps/' in response.url or '/v1/basemap-assets/' in response.url) else None)
                remote.goto(remote_base+f'/#scene/offline-alpha/plans/{job["id"]}')
                remote.locator('#connect').click()
                remote.locator('[name=token]').fill(test_token)
                remote.locator('#dialog-submit').click()
                expect(remote.locator('#dialog')).not_to_be_visible(timeout=15000)
                remote.evaluate("window.dispatchEvent(new HashChangeEvent('hashchange'))")
                expect(remote.locator('.offline-basemap-status')).to_contain_text('离线底图可用',timeout=20000)
                remote.wait_for_timeout(2000)
                assert protected and not denied and not remote_external,(protected,denied,remote_external)
                assert all(token=='Bearer '+test_token for _,token in protected), protected
                assert any('/fonts/' in url for url,_ in protected)
                assert any('/sprites/' in url for url,_ in protected)
                assert any(url.endswith('.pmtiles') for url,_ in protected)
                assert test_token not in remote.evaluate('JSON.stringify({...localStorage})')
                assert remote.evaluate("""async () => {const {mapRequest}=await import('/basemaps.js');return !mapRequest('https://example.invalid/map.pmtiles','sample-secret',['https://example.invalid']).headers && mapRequest('/v1/basemaps/example/archive.pmtiles','sample-secret',[]).headers.Authorization==='Bearer sample-secret';}""")
                remote.screenshot(path=str(folder/'authenticated.png'))
                remote_context.close()
                browser.close()
            report = {'passed': True, 'nativeMaxZoom': 14, 'externalRequestsOnColdOfflineOpen': external,
                      'glyphRequests': len(glyphs), 'localArchiveRequests': len(archives), 'upstreamHits': snapshot,
                      'businessInputUnchanged': True,'authenticatedMapResources':len(protected),'externalTokenLeak':False}
            (folder / 'acceptance.json').write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
            print(json.dumps({'passed': True, 'report': str(folder / 'acceptance.json')}))
        finally:
            process.terminate()
            process.wait(timeout=15)
            source.shutdown()


if __name__ == '__main__':
    main()
