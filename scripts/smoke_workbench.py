"""Exercise the embedded UI against a real isolated service, synthetic data only.

Requires Playwright + Chromium. Artifacts remain in ignored corpus/ui-smoke-*.
"""
import argparse
import base64
import struct
import zlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.request import Request, build_opener, ProxyHandler

from playwright.sync_api import sync_playwright, expect, Error as PlaywrightError
from workbench_fixture import workbench_scenario


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument('--server', type=Path, default=root / 'target/debug' / ('vrp-server.exe' if os.name == 'nt' else 'vrp-server'))
    args = parser.parse_args()
    directory = root / 'corpus' / f'ui-smoke-{time.time_ns()}'
    directory.mkdir()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    local = build_opener(ProxyHandler({}))

    def api(path, body=None):
        request = Request(base + path, data=None if body is None else json.dumps(body).encode(), headers={'Content-Type': 'application/json', 'Accept': 'application/json'})
        with local.open(request, timeout=15) as response:
            return json.load(response)

    with (directory / 'server.log').open('w', encoding='utf-8') as log:
        env = dict(os.environ)
        env.pop('VRP_API_TOKEN', None)
        process = subprocess.Popen([str(args.server), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}', '--data-dir', str(directory / 'data'), '--total-threads', '2', '--geo-prefetch', 'false'], cwd=root, env=env, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        try:
            deadline = time.monotonic() + 20
            while True:
                try:
                    api('/healthz')
                    break
                except OSError:
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError('server did not start')
                    time.sleep(.1)
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                context = browser.new_context(viewport={'width': 1600, 'height': 1050})
                page = context.new_page()
                # Never contact public tile servers from automated tests.
                def chunk(kind, data):
                    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
                tile = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>2I5B',256,256,8,2,0,0,0)) + chunk(b'IDAT', zlib.compress((b'\x00'+b'\xec\xf1\xf7'*256)*256)) + chunk(b'IEND',b'')
                page.route('https://tile.openstreetmap.org/**', lambda route: route.fulfill(status=200, content_type='image/png', body=tile))
                errors, csp_errors = [], []
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.on('console', lambda message: csp_errors.append(message.text) if 'Content Security Policy' in message.text else None)
                page.set_default_timeout(12000)

                def act(name, **kwargs):
                    button = page.locator(f'[data-action="{name}"]').first
                    if not button.count() and name == 'geometry-import':
                        page.locator('[data-action=task-info]').first.click()
                        button = page.locator(f'[data-action="{name}"]').first
                    if button.count() and not button.is_visible():
                        button.evaluate('(el)=>{for(let p=el.parentElement;p;p=p.parentElement)if(p.tagName==="DETAILS")p.open=true}')
                    button.click(**kwargs)

                def submit():
                    page.locator('#dialog-submit').click()
                    expect(page.locator('#dialog')).not_to_be_visible()
                    expect(page.locator('#dialog-submit')).to_be_enabled()

                def fast_solve():
                    act('solve' if page.locator('[data-action=solve]').count() else 'solve-existing')
                    page.locator('#dialog .advanced summary').click()
                    page.locator('[name=timeLimit]').fill('0.2')
                    submit()

                page.goto(base)
                expect(page.locator('#demo,.sidebar-footer')).to_have_count(0)
                expect(page.locator('.sidebar-actions #import-scene')).to_be_visible()
                page.locator('#new-scene').click()
                expect(page.locator('#dialog-title')).to_have_text('新建场景')
                page.locator('#dialog-cancel').click()
                fixture = workbench_scenario('workbench')
                page.locator('#import-scene').click()
                page.locator('#dialog-fields [name=name]').fill(fixture['name'])
                page.locator('#dialog-fields [name=id]').fill(fixture['scenarioId'])
                page.locator('#dialog-fields [name=file]').set_input_files({
                    'name': 'workbench.json', 'mimeType': 'application/json',
                    'buffer': json.dumps({'request': fixture['request'], 'display': fixture['display']}, ensure_ascii=False).encode(),
                })
                submit()
                expect(page.locator('#scene-title')).to_contain_text('工作台验收')
                scene = api('/v1/scenarios')['scenarios'][0]['id']
                op_path = f'/v1/operations/{scene}'
                expect(page.locator('.data-table tbody tr')).to_have_count(12)
                fast_solve()
                expect(page.locator('[data-action=start]')).to_be_enabled(timeout=15000)
                first = page.locator('#selected-job').input_value()
                page.locator('.route-row [data-select-ticket]').first.click()
                expect(page.locator('.inspector .detail-body')).to_contain_text('工单编号')
                assert page.locator('.route-map [data-select-ticket]').count() > 0
                expect(page.locator('.map-frame')).to_have_class('map-frame interactive')
                canvas = page.locator('.maplibregl-canvas').element_handle()
                page.wait_for_function("document.querySelector('.map-state').textContent.includes('访问顺序示意')")
                expect(page.locator('.map-frame [data-basemap-tone=light]')).to_have_attribute('aria-pressed', 'true')
                capture_map = '''async () => {
                    const {Map} = await import('/vendor/maplibre-gl.mjs');
                    const original = Map.prototype.setPaintProperty;
                    Map.prototype.setPaintProperty = function(...args) {
                        window.testPresentationMap = this;
                        return original.apply(this, args);
                    };
                }'''
                page.evaluate(capture_map)
                page.locator('.map-frame [data-basemap-tone=standard]').click()
                expect(page.locator('.map-frame [data-basemap-tone=standard]')).to_have_attribute('aria-pressed', 'true')
                assert page.evaluate("() => localStorage.getItem('vrp.map.basemapTone')") == 'standard'
                assert page.evaluate("() => ['raster-saturation','raster-contrast','raster-brightness-min','raster-brightness-max'].map(p=>testPresentationMap.getPaintProperty('basemap',p)??null)") == [None] * 4
                standard_tiles = page.evaluate("() => testPresentationMap.getStyle().sources.basemap.tiles")
                preference = page.context.new_page()
                preference.route('https://tile.openstreetmap.org/**', lambda route: route.fulfill(status=200, content_type='image/png', body=tile))
                preference.goto(base + f'/#scene/{scene}/plans/{first}')
                expect(preference.locator('.map-frame [data-basemap-tone=standard]')).to_have_attribute('aria-pressed', 'true')
                preference.reload()
                expect(preference.locator('.map-frame [data-basemap-tone=standard]')).to_have_attribute('aria-pressed', 'true')
                preference.close()
                page.locator('.map-frame [data-basemap-tone=light]').click()
                expect(page.locator('.map-frame [data-basemap-tone=light]')).to_have_attribute('aria-pressed', 'true')
                assert page.evaluate("() => ['raster-saturation','raster-contrast','raster-brightness-min','raster-brightness-max'].map(p=>testPresentationMap.getPaintProperty('basemap',p))") == [-.7, -.1, .4, 1]
                assert page.evaluate("() => testPresentationMap.getStyle().sources.basemap.tiles") == standard_tiles
                assert page.evaluate("() => testPresentationMap.getPaintProperty('vrp-lines','line-width')") == 4.5
                assert page.evaluate("() => testPresentationMap.getPaintProperty('vrp-lines-halo','line-color')") == '#ffffff'
                assert page.locator('.maplibregl-canvas').evaluate('(el)=>getComputedStyle(el).filter') == 'none'
                expect(page.locator('[data-road-plan-hint]')).to_contain_text('此方案未包含道路路径')
                page.locator('[data-road-plan-hint] [data-action=geo-plan]').click()
                expect(page.locator('#dialog-title')).to_have_text('生成道路矩阵并重新规划')
                page.locator('#dialog-cancel').click()
                assert not any(task.get('kind') == 'TASK_KIND_GEO_PLAN'
                               for task in api('/v1/tasks?scenario_id=' + scene).get('tasks', []))
                # External route geometry belongs to the exact frozen job.
                frozen = api(f'/v1/jobs/{first}/request')['problem']
                route = api(f'/v1/jobs/{first}/result')['solution']['routes'][0]
                vehicle = frozen['agents'][route.get('agent',0)]
                origin = frozen['locations'][vehicle.get('startLocation',0)]
                destination = frozen['locations'][frozen['tickets'][route['tickets'][0]].get('location',0)]
                shape = {'jobId':first,'coordinateSystem':'WGS84','provider':'synthetic-test','legs':[{'agentId':vehicle['id'],'legIndex':0,'fromLocationId':origin['id'],'toLocationId':destination['id'],'points':[{'lng':origin['lng'],'lat':origin['lat']},{'lng':destination['lng'],'lat':destination['lat']}]}]}
                shape_file = directory / 'geometry.json'
                shape_file.write_text(json.dumps(shape),encoding='utf-8')
                # A stale empty response must not overwrite geometry imported
                # while that request is still in flight.
                held_geometry = []
                def hold_first_geometry(route):
                    if route.request.method == 'GET' and not held_geometry:
                        held_geometry.append(route)
                    else:
                        route.continue_()
                page.route(base + f'/v1/jobs/{first}/geometry', hold_first_geometry)
                page.evaluate('''async (id) => {
                    const {invalidateGeometry} = await import('/map.js');
                    invalidateGeometry(id);
                }''', first)
                deadline = time.monotonic() + 5
                while not held_geometry:
                    assert time.monotonic() < deadline
                    page.wait_for_timeout(20)
                act('geometry-import')
                page.locator('[name=file]').set_input_files(str(shape_file))
                submit()
                held_geometry[0].fulfill(json={'jobId': first, 'legs': []})
                expect(page.locator('.map-state')).to_contain_text('道路路径已加载')
                page.unroute(base + f'/v1/jobs/{first}/geometry', hold_first_geometry)
                expect(page.locator('[data-road-plan-hint]')).to_have_count(0)
                stored_shape = api(f'/v1/jobs/{first}/geometry')
                assert stored_shape['jobId'] == first and len(stored_shape['legs']) == 1
                stored_leg, expected_leg = stored_shape['legs'][0], shape['legs'][0]
                assert stored_leg.get('legIndex', 0) == 0
                for field in ['agentId', 'fromLocationId', 'toLocationId', 'points']:
                    assert stored_leg[field] == expected_leg[field]
                act('refresh')
                assert page.evaluate('(el)=>el.isConnected', canvas), 'refresh recreated the WebGL canvas'
                # Keyboard selection in the map's accessible object list updates
                # the same business inspector as a map feature click.
                choice=page.locator('.map-object-list button').filter(has_text='设备').first
                choice.focus();choice.press('Enter')
                expect(page.locator('.inspector .detail-body')).to_contain_text('工单编号')
                expect(page.locator('.map-stop-label').first).to_be_visible()
                # A user pan survives polling/refresh; the canvas and marker
                # position stay stable instead of refitting the route.
                box = page.locator('.maplibregl-canvas').bounding_box()
                page.mouse.move(box['x']+box['width']/2, box['y']+box['height']/2)
                page.mouse.down()
                page.mouse.move(box['x']+box['width']/2+45, box['y']+box['height']/2+25, steps=12)
                page.mouse.up()
                page.wait_for_timeout(300)
                pan = page.locator('.map-stop-label').first.bounding_box()
                act('refresh')
                page.wait_for_timeout(300)
                after_pan = page.locator('.map-stop-label').first.bounding_box()
                assert abs(pan['x']-after_pan['x']) < 1 and abs(pan['y']-after_pan['y']) < 1
                act('zoom-in')
                expect(page.locator('.schedule-head')).to_contain_text('125%')
                page.screenshot(path=str(directory / 'planning.png'), full_page=True)

                # Continuation uses the same frozen problem and the actual result.
                act('continue')
                submit()
                expect(page.locator('[data-action=start]')).to_be_enabled(timeout=15000)
                continued = page.locator('#selected-job').input_value()
                assert continued != first
                old_input, new_input = api(f'/v1/jobs/{first}/request'), api(f'/v1/jobs/{continued}/request')
                assert new_input['problem'] == old_input['problem']
                assert new_input['initialSolution'] == api(f'/v1/jobs/{first}/result')['solution']
                assert api(f'/v1/jobs/{continued}')['sourceJobId'] == first

                # A manual edit creates an evaluated candidate, preserving its parent.
                page.locator('.route-row [data-select-ticket]').first.click()
                act('adjust')
                page.locator('[name=agent]').select_option('1')
                page.locator('[name=reason]').fill('人工调整测试')
                submit()
                manual = page.locator('#selected-job').input_value()
                assert api(f'/v1/jobs/{manual}')['kind'] == 'JOB_KIND_MANUAL_ADJUSTMENT'
                expect(page.locator('[data-action=start]')).to_be_enabled()
                act('start')
                page.locator('[name=clock]').select_option('1')
                page.locator('[name=autoReplan]').uncheck()
                submit()
                op = api(op_path)
                assert op['plan']['version'] == '1'
                assert all(t.get('phase', 'TICKET_PHASE_PENDING') == 'TICKET_PHASE_PENDING' for t in op['tickets'])
                assert all(not v.get('acknowledgedPlan') for v in op['vehicles'])
                expect(page.locator('#workspace-tabs [data-tab=operations]')).to_have_class('selected')

                # Record departure; the current plan survives while a new candidate runs.
                route = next(r for r in op['plan']['routes'] if r.get('stops'))
                ticket, agent = route['stops'][0]['ticketId'], route['agentId']
                page.locator(f'.route-row [data-select-ticket="{ticket}"]').first.click()
                act('ticket-progress')
                page.locator('[name=phase]').select_option('EN_ROUTE')
                page.locator('[name=agent]').select_option(agent)
                page.locator('[name=at]').fill('08:00:01')
                submit()
                before_plan = api(op_path)['plan']
                fast_solve()
                expect(page.locator('[data-action=adopt]')).to_be_enabled(timeout=15000)
                assert api(op_path)['plan'] == before_plan
                expect(page.locator('#workspace-content')).to_contain_text('候选尚未生效')
                page.screenshot(path=str(directory / 'candidate.png'), full_page=True)
                act('adopt')
                submit()
                op = api(op_path)
                assert op['plan']['version'] == '2'
                assert next(t for t in op['tickets'] if t['ticketId'] == ticket)['phase'] == 'TICKET_PHASE_EN_ROUTE'

                # Confirmed actual service is separate from the published schedule.
                for phase, at in [('ARRIVED', '08:20:00'), ('SERVING', '08:20:01'), ('COMPLETED', '09:20:01')]:
                    page.locator(f'.route-row [data-select-ticket="{ticket}"]').first.click()
                    act('ticket-progress')
                    page.locator('[name=phase]').select_option(phase)
                    page.locator('[name=at]').fill(at)
                    submit()
                assert next(t for t in api(op_path)['tickets'] if t['ticketId'] == ticket)['finishedAt'] == '33601'

                # Continuous event-driven replanning remains independent of adoption.
                act('policy')
                page.locator('[name=autoReplan]').check()
                page.locator('[name=adoption]').select_option('manual')
                submit()
                act('new-ticket')
                page.locator('[name=id]').fill('URGENT')
                page.locator('[name=name]').fill('紧急插单')
                page.locator('[name=earliestStart]').fill('09:30')
                submit()
                deadline = time.monotonic() + 15
                while True:
                    op = api(op_path)
                    j = api('/v1/jobs/' + op['latestJobId']) if op.get('latestJobId') else {}
                    if j.get('hasResult') and j.get('scenario', {}).get('version') == '2':
                        break
                    if time.monotonic() > deadline:
                        raise AssertionError('urgent order did not trigger a new solve')
                    page.wait_for_timeout(200)
                assert op['plan']['version'] == '2'
                assert op['policy']['autoReplan'] and not op['policy'].get('autoAdopt')

                # Draft, selected result and zoom are isolated per scene.
                page.locator('[data-tab=plans]').click()
                selected_before_switch = page.locator('#selected-job').input_value()
                page.locator('[data-tab=data]').click()
                page.locator('[data-action=edit-object][data-kind=tickets][data-index="1"]').click()
                page.locator('[name=name]').fill('尚未保存的名称')
                submit()
                expect(page.locator('.sheet-table input[value="尚未保存的名称"]')).to_be_visible()
                other_input = json.loads(json.dumps(old_input))
                count = len(other_input['problem']['locations'])
                values = [0 if f==t else 100+f*count+t for f in range(count) for t in range(count)]
                encoded = base64.b64encode(struct.pack('<'+'I'*len(values),*values)).decode()
                other_input['problem']['matrices'] = [{'profile':'synthetic','durations':encoded,'distances':encoded}]
                api('/v1/scenarios', {'scenarioId': 'other', 'name': '另一场景', 'request': other_input})
                page.goto(base + '#scene/other/data')
                expect(page.locator('#scene-title')).to_have_text('另一场景')
                page.locator('[data-data-tab=locations]').click()
                page.locator('[data-action=edit-object][data-kind=locations]:not([data-index])').click()
                page.locator('[name=id]').fill('new-location')
                page.locator('[name=name]').fill('新增服务点')
                submit()
                act('save-draft')
                expect(page.locator('[data-action=save-draft]')).to_be_disabled()
                saved = api('/v1/scenarios/other')
                assert len(saved['request']['problem']['locations'])==count+1
                raw = base64.b64decode(saved['request']['problem']['matrices'][0]['durations'])
                expanded = struct.unpack('<'+'I'*((count+1)**2),raw)
                assert all(expanded[f*(count+1)+t]==values[f*count+t] for f in range(count) for t in range(count))
                assert expanded[count]==4294967294 and expanded[count*(count+1)]==4294967294
                assert int(saved['scenario']['matrixQuality']['estimatedEdges'])==2*count
                page.locator('[data-data-tab=matrix]').click()
                page.locator('#matrix-from').select_option(str(count))
                expect(page.locator('#workspace-content')).to_contain_text('待估算')
                page.locator('[data-data-tab=locations]').click()
                page.locator('[data-action=edit-object][data-kind=locations][data-index="1"]').click()
                page.locator('[name=lng]').fill('121.49')
                submit()
                act('save-draft')
                expect(page.locator('[data-action=save-draft]')).to_be_disabled()
                moved=base64.b64decode(api('/v1/scenarios/other')['request']['problem']['matrices'][0]['durations'])
                assert struct.unpack_from('<I',moved,4)[0]==4294967294
                assert struct.unpack_from('<I',moved,8)[0]==values[2]
                page.locator(f'[data-scene="{scene}"]').click()
                expect(page.locator('#scene-title')).to_contain_text('工作台验收')
                expect(page.locator('.sheet-table input[value="尚未保存的名称"]')).to_be_visible()
                assert api(f'/v1/scenarios/{scene}')['scenario'].get('display', {}).get('tickets', {}).get('ORDER-02', {}).get('name') != '尚未保存的名称'

                # Late metadata from a previously selected scene cannot replace
                # the scene currently on screen. No destructive action is involved.
                page.locator('[data-scene="other"]').click()
                expect(page.locator('#scene-title')).to_have_text('另一场景')
                delayed = []
                page.route(f'**/v1/scenarios/{scene}/metadata', lambda route: delayed.append(route))
                page.locator(f'[data-scene="{scene}"]').click()
                page.wait_for_function('document.querySelector("#scene-title").textContent.includes("工作台验收")')
                page.locator('[data-scene="other"]').click()
                expect(page.locator('#scene-title')).to_have_text('另一场景')
                assert delayed
                for request in delayed:
                    request.continue_()
                page.unroute(f'**/v1/scenarios/{scene}/metadata')
                page.wait_for_timeout(500)
                expect(page.locator('#scene-title')).to_have_text('另一场景')
                page.locator(f'[data-scene="{scene}"]').click()
                expect(page.locator('.sheet-table input[value="尚未保存的名称"]')).to_be_visible()
                page.locator('[data-tab=plans]').click()
                expect(page.locator('.schedule-head')).to_contain_text('125%')
                assert page.locator('#selected-job').input_value() == selected_before_switch

                page.locator('[data-tab=history]').click()
                expect(page.locator('.history-events [data-history]')).not_to_have_count(0)
                expect(page.locator('.history-preview')).to_contain_text('历史')
                assert not page.locator('.history-preview [data-action=ticket-progress]').count()
                page.screenshot(path=str(directory / 'history.png'), full_page=True)
                page.locator('[data-nav=tasks]').click()
                expect(page.locator('#global-content')).to_contain_text('人工调整')
                page.locator('[data-nav=settings]').click()
                expect(page.locator('#global-content')).to_contain_text('计算额度')
                page.locator(f'[data-scene="{scene}"]').click()
                page.locator('[data-tab=operations]').click()
                page.set_viewport_size({'width': 390, 'height': 844})
                expect(page.locator('.schedule-panel')).to_be_visible()
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth + 1')
                page.screenshot(path=str(directory / 'mobile.png'), full_page=True)
                assert not errors, errors
                assert not csp_errors, csp_errors
                slow = browser.new_page(viewport={'width':1600,'height':1050})
                pending_tiles=[]
                slow.route('https://tile.openstreetmap.org/**',lambda route:pending_tiles.append(route))
                slow.goto(base+f'#scene/{scene}/operations')
                expect(slow.locator('.map-stop-label').first).to_be_visible(timeout=5000)
                assert pending_tiles, 'test must hold pending basemap requests'
                expect(slow.locator('.map-state')).to_contain_text('访问顺序示意')
                slow.route('https://tile.openstreetmap.org/**',lambda route:route.abort())
                for pending_tile in pending_tiles:
                    try:
                        pending_tile.abort()
                    except PlaywrightError as error:
                        # Refitting the camera cancels obsolete tile requests.
                        if 'already handled' not in str(error): raise
                slow.unroute_all(behavior='ignoreErrors')
                slow.wait_for_timeout(100)
                slow.close()
                # Basemap errors keep the WebGL business overlay and Gantt usable.
                offline = browser.new_page(viewport={'width':1600,'height':1050})
                offline.route('https://tile.openstreetmap.org/**', lambda route: route.abort())
                offline.goto(base + f'#scene/{scene}/operations')
                expect(offline.locator('.map-frame')).to_have_class('map-frame interactive')
                expect(offline.locator('.map-state')).to_contain_text('底图暂不可用')
                expect(offline.locator('.map-stop-label').first).to_be_visible()
                expect(offline.locator('.schedule-panel')).to_be_visible()
                offline.close()
                # A tiny local PMTiles v3 raster archive exercises the official
                # browser protocol and HTTP Range support without map downloads.
                def varint(number):
                    out = bytearray()
                    while number >= 128:
                        out.append((number & 127) | 128)
                        number >>= 7
                    out.append(number)
                    return bytes(out)
                root_directory = b'\x01\x00\x01' + varint(len(tile)) + b'\x01'
                metadata = b'{"format":"png"}'
                header = bytearray(127)
                header[:8] = b'PMTiles\x03'
                tile_offset = 127 + len(root_directory) + len(metadata)
                for offset, value in {8:127,16:len(root_directory),24:127+len(root_directory),32:len(metadata),40:tile_offset,48:0,56:tile_offset,64:len(tile),72:1,80:1,88:1}.items():
                    struct.pack_into('<Q',header,offset,value)
                header[96:102] = bytes([1,1,1,2,0,0])
                for offset, value in {102:-1800000000,106:-850000000,110:1800000000,114:850000000}.items():
                    struct.pack_into('<i',header,offset,value)
                archive = bytes(header) + root_directory + metadata + tile
                custom = browser.new_page(viewport={'width':1600,'height':1050})
                custom_errors, ranges = [], []
                custom.on('pageerror', lambda error:custom_errors.append(str(error)))
                custom.route(base+'/v1/map-config', lambda route:route.fulfill(json={'styleUrl':base+'/fixture-style.json','resourceOrigins':[],'maxZoom':19}))
                custom_paint = {'raster-saturation':.2,'raster-contrast':.1,'raster-brightness-min':.06,'raster-brightness-max':.92}
                custom.route(base+'/fixture-style.json', lambda route:route.fulfill(json={'version':8,'sources':{'archive':{'type':'raster','url':'pmtiles://'+base+'/fixture.pmtiles','tileSize':256}},'layers':[{'id':'archive','type':'raster','source':'archive','paint':custom_paint}]}))
                def archive_response(route):
                    assert 'authorization' not in route.request.headers
                    value = route.request.headers.get('range','bytes=0-')
                    ranges.append(value)
                    start,end=value.removeprefix('bytes=').split('-')
                    start=int(start);end=min(int(end) if end else len(archive)-1,len(archive)-1)
                    route.fulfill(status=206,headers={'Content-Range':f'bytes {start}-{end}/{len(archive)}','Accept-Ranges':'bytes','Content-Type':'application/octet-stream'},body=archive[start:end+1])
                custom.route(base+'/fixture.pmtiles',archive_response)
                custom.goto(base+f'#scene/{scene}/operations')
                expect(custom.locator('.map-stop-label').first).to_be_visible()
                custom.wait_for_timeout(500)
                assert ranges, 'PMTiles protocol did not issue a Range request'
                custom.evaluate(capture_map)
                custom.locator('.map-frame [data-basemap-tone=standard]').click()
                assert custom.evaluate("() => Object.fromEntries(['raster-saturation','raster-contrast','raster-brightness-min','raster-brightness-max'].map(p=>[p,testPresentationMap.getPaintProperty('archive',p)]))") == custom_paint
                assert '底图暂不可用' not in custom.locator('.map-state').inner_text()
                assert not custom_errors, custom_errors
                custom.close()
                browser.close()
                # A machine without WebGL still has the schematic and full UI.
                software = pw.chromium.launch(args=['--disable-webgl'])
                fallback = software.new_page(viewport={'width':1600,'height':1050})
                fallback.route('https://tile.openstreetmap.org/**', lambda route: route.abort())
                fallback.goto(base + f'#scene/{scene}/operations')
                expect(fallback.locator('.map-state')).to_contain_text('WebGL 不可用')
                expect(fallback.locator('.map-fallback')).to_be_visible()
                expect(fallback.locator('.schedule-panel')).to_be_visible()
                fallback.screenshot(path=str(directory / 'webgl-fallback.png'), full_page=True)
                software.close()
                print(f'Workbench acceptance passed: {directory}')
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == '__main__':
    main()
