"""Browser coverage for data sheets and matrix interaction, isolated synthetic data."""
import argparse
import base64
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import time
from urllib.request import Request, build_opener, ProxyHandler
from playwright.sync_api import sync_playwright, expect
from workbench_fixture import workbench_scenario


def check_compact_workbar(browser, base, api, original, folder, errors):
    request=json.loads(json.dumps(original['request']))
    request['problem']['horizon']={'startLocal':'2026-03-07T06:30:00','timezone':'Asia/Shanghai'}
    for id in ['compact-a','compact-b']:
        api('/v1/scenarios',{'scenarioId':id,'name':'紧凑场景 '+id,'request':request})
    page=browser.new_page(viewport={'width':1600,'height':1000})
    page.on('pageerror',lambda e:errors.append(str(e)))
    page.route('https://tile.openstreetmap.org/**',lambda r:r.abort())
    page.goto(base+'/#scene/compact-a/data')
    expect(page.locator('[data-sheet=tickets]')).to_be_visible()
    assert page.locator('.sheet-table thead').bounding_box()['y']<195
    expect(page.locator('.data-summary,.sheet-help')).to_have_count(0)
    tabs=page.locator('[data-data-tab]').evaluate_all('(els)=>els.map(e=>e.dataset.dataTab)')
    assert tabs==['tickets','agents','locations','matrix','rules','advanced']
    before=api('/v1/scenarios/compact-a')
    date=page.locator('[data-action=scene-date]')
    date.click();page.locator('#dialog-fields [name=date]').fill('2026-03-08')
    page.locator('#dialog-cancel').click()
    expect(date).to_contain_text('2026-03-07')
    expect(page.locator('[data-action=save-draft]')).to_be_disabled()
    date.click();page.locator('#dialog-fields [name=date]').fill('2026-03-08')
    page.locator('#dialog-submit').click();expect(page.locator('#dialog')).not_to_be_visible()
    expect(date).to_contain_text('2026-03-08')
    expect(page.locator('#scene-save-state')).to_contain_text('未保存')
    expect(page.locator('#workspace-status .banner')).to_have_count(0)
    assert before==api('/v1/scenarios/compact-a')
    page.locator('[data-sheet-command=undo]').click();expect(date).to_contain_text('2026-03-07')
    page.locator('[data-sheet-command=redo]').click();expect(date).to_contain_text('2026-03-08')
    # Opening the menu directly after typing commits the cell without closing it.
    page.locator('#sheet-tickets-0-name').fill('菜单打开前的草稿')
    page.locator('.scene-more summary').click();page.wait_for_timeout(180)
    expect(page.locator('.scene-more [data-action=discard-draft]')).to_be_visible()
    page.keyboard.press('Escape');expect(page.locator('.scene-more summary')).to_be_focused()
    page.locator('.scene-more summary').click()
    with page.expect_download() as download:
        page.locator('.scene-more [data-action=export-scene]').click()
    exported=json.loads(Path(download.value.path()).read_text(encoding='utf-8'))
    assert exported['request']['problem']['horizon']['startLocal']=='2026-03-08T06:30:00'
    page.locator('[data-scene=compact-b]').click()
    expect(page.locator('[data-action=scene-date]')).to_contain_text('2026-03-07')
    page.locator('[data-scene=compact-a]').click()
    expect(date).to_contain_text('2026-03-08')
    expect(page.locator('#sheet-tickets-0-name')).to_have_value('菜单打开前的草稿')
    page.locator('#workspace-tab-data').press('ArrowRight')
    expect(page.locator('#workspace-tab-plans')).to_have_attribute('aria-selected','true')
    expect(page.locator('#workspace-tab-plans')).to_be_focused()
    if page.locator('#notice').is_visible():
        page.locator('#notice-close').click()
    page.evaluate('window.scrollTo(0,0)')
    page.locator('[data-action=save-draft]').click()
    expect(page.locator('#scene-save-state')).to_have_text('已保存 v2')
    saved=api('/v1/scenarios/compact-a')['request']['problem']
    assert saved['horizon']=={'startLocal':'2026-03-08T06:30:00','timezone':'Asia/Shanghai'}
    assert saved.get('matrices',[])==request['problem'].get('matrices',[])
    page.locator('#workspace-tab-plans').press('Home')
    expect(page.locator('#workspace-tab-data')).to_have_attribute('aria-selected','true')
    page.locator('[data-data-tab=advanced]').click()
    raw=json.loads(page.locator('#json-editor').input_value())
    raw.setdefault('options',{})['seed']='45'
    page.locator('#json-editor').fill(json.dumps(raw))
    date.click();page.locator('#dialog-fields [name=date]').fill('2026-03-09')
    page.locator('#dialog-submit').click();expect(page.locator('#dialog')).not_to_be_visible()
    if page.locator('#notice').is_visible():
        page.locator('#notice-close').click()
    page.evaluate('window.scrollTo(0,0)')
    page.locator('[data-action=save-draft]').click()
    expect(page.locator('#scene-save-state')).to_have_text('已保存 v3')
    saved=api('/v1/scenarios/compact-a')['request']
    assert saved['options']['seed']=='45'
    assert saved['problem']['horizon']['startLocal']=='2026-03-09T06:30:00'
    page.locator('[data-data-tab=tickets]').click()
    page.locator('[data-action=sheet-help]').click()
    expect(page.locator('#dialog-cancel')).not_to_be_visible()
    page.locator('#dialog-submit').click()
    expect(page.locator('#dialog')).not_to_be_visible()
    page.locator('.data-tools [data-action=scene-properties]').click()
    expect(page.locator('#dialog-cancel')).to_be_visible()
    expect(page.locator('#dialog-fields [name=timezone]')).to_have_value('Asia/Shanghai')
    page.locator('#dialog-cancel').click()
    page.screenshot(path=str(folder/'compact-workbar.png'))
    assert not errors,errors
    page.close()
    print('Compact workbar passed: date draft/cancel/undo, menu after editing, export, scene isolation, raw input preservation, keyboard navigation and save from plans.')


def check_predecessor_sheet(browser, base, api, original, folder, errors):
    request=json.loads(json.dumps(original['request']))
    request.pop('initialSolution',None)
    p=request['problem']; p['tickets']=p['tickets'][:5]
    p['precedences']=[]; p['relatedPairs']=[]
    p.setdefault('weights',{})['refTicketAfterDepTicket']={}
    ids=[t['id'] for t in p['tickets']]
    display={'tickets':{id:{'name':f'依赖测试工单{i+1}'} for i,id in enumerate(ids)}}
    api('/v1/scenarios',{'scenarioId':'predecessor-sheet','name':'多值前置工单测试','request':request,'display':display})
    page=browser.new_page(viewport={'width':1500,'height':1000})
    page.on('pageerror',lambda e:errors.append(str(e)))
    page.route('https://tile.openstreetmap.org/**',lambda route:route.abort())
    page.goto(base+'/#scene/predecessor-sheet/data')
    expect(page.locator('[data-data-tab]')).to_have_count(6)
    expect(page.locator('[data-data-tab=relations]')).to_have_count(0)
    def cell(row):return page.locator(f'#sheet-tickets-{row}-predecessors')
    def paste(row,text):
        cell(row).focus()
        cell(row).evaluate('''(el,text)=>{const data=new DataTransfer();data.setData('text/plain',text);
            el.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true}));}''',text)
    def save():
        if page.locator('#notice').is_visible():
            page.locator('#notice-close').click()
        page.evaluate('window.scrollTo(0,0)')
        page.locator('[data-action=save-draft]').click()
        expect(page.locator('[data-action=save-draft]')).to_be_disabled()
        return api('/v1/scenarios/predecessor-sheet')
    page.locator('[data-sheet-predecessors="3"]').click()
    popup=page.locator('.sheet-choice')
    popup.locator('.sheet-choice-search').fill('依赖测试工单1')
    expect(popup.locator('input[type=checkbox]')).to_have_count(1)
    popup.locator('input[type=checkbox]').check()
    popup.locator('.sheet-choice-search').fill('依赖测试工单2')
    popup.locator('input[type=checkbox]').check()
    popup.locator('[data-choice-apply]').click()
    expect(cell(3)).to_have_value(', '.join(ids[:2]))
    page.locator('[data-sheet-command=undo]').click()
    expect(cell(3)).to_have_value('')
    page.locator('[data-sheet-command=redo]').click()
    expect(cell(3)).to_have_value(', '.join(ids[:2]))
    # A cyclic rectangle must not apply its first row before rejecting the next.
    paste(0,ids[1]+'\n'+ids[0])
    expect(page.locator('.sheet-status')).to_contain_text('循环依赖')
    expect(cell(0)).to_have_value(''); expect(cell(1)).to_have_value('')
    paste(0,ids[0]); expect(page.locator('.sheet-status')).to_contain_text('自身')
    paste(3,'不存在的工单'); expect(cell(3)).to_have_value(', '.join(ids[:2]))
    paste(3,ids[0]+', '+ids[0]+', 依赖测试工单2, '+ids[2])
    expect(cell(3)).to_have_value(', '.join(ids[:3]))
    expect(page.locator('[data-sheet-predecessors="3"]')).to_have_text('+1')
    paste(4,ids[3])
    saved=save()['request']['problem']
    pairs={(int(v.get('first',0)),int(v.get('second',0))) for v in saved['precedences']}
    assert pairs=={(0,3),(1,3),(2,3),(3,4)},pairs
    assert not saved.get('relatedPairs')
    assert saved['weights']['refTicketAfterDepTicket']['hard']=='1'
    assert not any('_predecessors' in t or 'predecessors' in t for t in saved['tickets'])
    assert saved.get('matrices',[])==p.get('matrices',[])
    page.reload(); expect(cell(3)).to_have_value(', '.join(ids[:3]))
    cell(3).fill('无效编号'); cell(3).press('Tab')
    expect(cell(3)).to_have_attribute('aria-invalid','true')
    cell(3).press('Escape')
    page.screenshot(path=str(folder/'predecessors.png'),full_page=True)
    for width in [1500,390]:
        page.set_viewport_size({'width':width,'height':900})
        page.locator('[data-sheet-predecessors="3"]').click()
        expect(popup.locator('input[type=checkbox]:checked')).to_have_count(3)
        box=popup.bounding_box()
        assert box['x']>=0 and box['x']+box['width']<=width and box['y']>=0,box
        popup.locator('.sheet-choice-search').press('Escape')
        expect(popup).to_have_count(0)
    page.close()
    print('Predecessor sheet passed: multi-select, names/IDs, atomic cycles, persistence, undo, narrow-screen picker.')


def check_settings_sheets(browser, base, api, original, folder, errors):
    """Settings use the same atomic edits; coordinate changes invalidate only incident edges."""
    request = json.loads(json.dumps(original['request']))
    p = request['problem']
    p['locations'] = p['locations'][:4]
    p['tickets'] = p['tickets'][:3]
    p['weights'] = {'agentCapacity': {}}
    p.pop('estimateBuffer', None)
    p['costs'] = {'restrictedChargePerHour': '300'}
    durations = [0,120,400,600,900,0,4294967295,60,400,500,0,300,800,200,700,0]
    distances = [v if v >= 4294967294 else v*10 for v in durations]
    encode = lambda values: base64.b64encode(struct.pack('<'+'I'*len(values), *values)).decode()
    sources = bytes(0 if i//4 == i%4 else 1 for i in range(16))
    p['matrices'] = [{'profile': '测试车型', 'durations': encode(durations),
                      'distances': encode(distances), 'sources': base64.b64encode(sources).decode()}]
    display = {'locations': {v['id']: {'name': f'地点{i+1:03}'} for i,v in enumerate(p['locations'])}}
    api('/v1/scenarios', {'scenarioId': 'settings-sheet', 'name': '约束与地点工作表测试',
                          'request': request, 'display': display})
    original = api('/v1/scenarios/settings-sheet')
    page = browser.new_page(viewport={'width': 1500, 'height': 1000})
    page.set_default_timeout(10000)
    page.on('pageerror', lambda e: errors.append(str(e)))
    page.route('https://tile.openstreetmap.org/**', lambda route: route.abort())
    page.goto(base + '/#scene/settings-sheet/data')

    def tab(kind):
        page.locator(f'[data-data-tab={kind}]').click()
        expect(page.locator(f'[data-data-tab={kind}]')).to_have_attribute('aria-selected', 'true')

    def cell(kind, row, key): return page.locator(f'[id="sheet-{kind}-{row}-{key}"]')
    def command(kind, name): page.locator(f'[data-sheet={kind}] [data-sheet-command={name}]').click()
    def paste(kind, row, key, text):
        cell(kind,row,key).focus()
        cell(kind,row,key).evaluate('''(el,text) => { const data=new DataTransfer(); data.setData('text/plain',text);
            el.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true})); }''', text)
    def edit(kind,row,key,value):
        cell(kind,row,key).fill(value)
        cell(kind,row,key).press('Tab')
    def draft():
        tab('advanced')
        value = json.loads(page.locator('#json-editor').input_value())
        return value['problem']
    def save():
        if page.locator('#notice').is_visible():
            page.locator('#notice-close').click()
        page.evaluate('window.scrollTo(0,0)')
        page.locator('[data-action=save-draft]').click()
        expect(page.locator('[data-action=save-draft]')).to_be_disabled()
        return api('/v1/scenarios/settings-sheet')

    tab('rules')
    expect(page.locator('[data-sheet]')).to_have_count(2)
    expect(page.locator('[data-weight=minimizeTravelTime][data-level=soft]')).to_have_value('1')
    expect(page.locator('[data-weight=agentCapacity][data-level=hard]')).to_have_value('0')
    expect(page.locator('[data-weight=executionConsistency][data-level=hard]')).not_to_be_editable()
    paste('rules',0,'hard','2\t3\t4\n5\t6\t7')
    expect(cell('rules',1,'soft')).to_have_value('7')
    command('costs','undo')
    expect(cell('rules',0,'hard')).to_have_value('0')
    command('rules','redo')
    expect(cell('rules',0,'medium')).to_have_value('3')
    paste('rules',0,'hard','8\t9\t10\n11\t12\t1000000001')
    expect(page.locator('[data-sheet=rules] .sheet-status')).to_contain_text('粘贴未应用')
    expect(cell('rules',0,'hard')).to_have_value('2')
    edit('rules',0,'soft','-1')
    expect(cell('rules',0,'soft')).to_have_attribute('aria-invalid','true')
    if page.locator('#notice').is_visible():
        page.locator('#notice-close').click()
    page.evaluate('window.scrollTo(0,0)')
    page.locator('[data-action=save-draft]').click()
    expect(page.locator('#notice')).to_contain_text('标红')
    cell('rules',0,'soft').press('Escape')
    cell('rules',0,'hard').click()
    cell('rules',1,'hard').click(modifiers=['Shift'])
    command('rules','fill')
    expect(cell('rules',1,'hard')).to_have_value('2')

    expect(cell('costs',1,'value')).to_have_value('100')
    expect(cell('costs',2,'value')).to_have_value('200')
    cell('costs',1,'value').fill('250')
    # Holding a pointer press must not detach its click target when the old cell blurs.
    cell('costs',2,'value').click(delay=300)
    expect(cell('costs',2,'value')).to_be_focused()
    edit('costs',2,'value','350')
    paste('costs',0,'value','1200\n10001')
    expect(page.locator('[data-sheet=costs] .sheet-status')).to_contain_text('粘贴未应用')
    expect(cell('costs',0,'value')).to_have_value('300')
    expect(cell('costs',1,'value')).to_have_value('250')
    edit('costs',0,'value','1200')
    saved = save()['request']['problem']
    assert saved['costs']['restrictedChargePerHour'] == '1200'
    assert saved['estimateBuffer'] == {'modelPermille':250,'fallbackPermille':350}
    assert saved['weights']['agentCapacity'] == {'hard':'2','medium':'3','soft':'4'}
    page.locator('#sheet-filter-rules').fill('行驶')
    expect(page.locator('[data-sheet=rules] tbody [data-sheet-row]')).to_have_count(2)
    expect(page.locator('#sheet-filter-rules')).to_be_focused()
    expect(page.locator('[data-sheet=costs] tbody [data-sheet-row]')).to_have_count(3)
    page.locator('#sheet-filter-rules').fill('')
    expect(page.locator('[data-sheet=rules] tbody [data-sheet-row]')).to_have_count(24)
    page.screenshot(path=str(folder/'settings.png'), full_page=True)

    tab('locations')
    expect(cell('locations',0,'id')).not_to_be_editable()
    edit('locations',1,'name','共享服务点')
    edit('locations',1,'address','展示地址')
    edit('locations',1,'note','坐标不变')
    saved = save()
    assert saved['request']['problem']['matrices'] == original['request']['problem']['matrices']
    assert saved['request']['problem']['locations'] == original['request']['problem']['locations']
    assert saved['scenario']['display']['locations'][p['locations'][1]['id']]['name'] == '共享服务点'
    tab('tickets')
    expect(page.locator('#sheet-tickets-0-location option:checked')).to_have_text('共享服务点')
    tab('locations')
    before_lng = cell('locations',1,'lng').input_value()
    paste('locations',1,'lng','121.7\t31.3\n121.8\t91')
    expect(page.locator('.sheet-status')).to_contain_text('粘贴未应用')
    expect(cell('locations',1,'lng')).to_have_value(before_lng)
    paste('locations',1,'lng','121.7\t31.3\n121.8\t31.4')
    command('locations','undo')
    assert draft()['matrices'] == original['request']['problem']['matrices']
    tab('locations'); command('locations','redo')
    saved = save()['request']['problem']
    for field, old_values in [('durations',durations),('distances',distances)]:
        values = struct.unpack('<16I',base64.b64decode(saved['matrices'][0][field]))
        for i,value in enumerate(values):
            a,b = divmod(i,4)
            expected = 0 if a==b else 4294967294 if a in [1,2] or b in [1,2] else old_values[i]
            assert value == expected, (field,a,b,value,expected)
    assert list(base64.b64decode(saved['matrices'][0]['sources'])) == [
        0 if i//4==i%4 else 3 if i//4 in [1,2] or i%4 in [1,2] else sources[i] for i in range(16)]
    assert saved['locations'][1]['lng'] == 121.7 and saved['locations'][2]['lat'] == 31.4
    page.locator('[data-sheet-sort=name]').click()
    edit('locations',2,'name','排序后正确编辑')
    assert save()['scenario']['display']['locations'][p['locations'][2]['id']]['name'] == '排序后正确编辑'
    page.screenshot(path=str(folder/'locations.png'), full_page=True)
    for kind in ['rules','locations']:
        tab(kind)
        for width in [1500,900,390]:
            page.set_viewport_size({'width':width,'height':1000})
            page.wait_for_timeout(200)
            geometry = page.evaluate('''() => ({width:innerWidth, scroll:document.documentElement.scrollWidth,
                row:document.querySelector('.sheet-table tbody tr[data-sheet-row]').getBoundingClientRect().height})''')
            assert geometry['scroll'] <= geometry['width']+1 and geometry['row'] == 32, geometry

    # Explicit empty buffer is zero, not the absent-message default; coordinates
    # omitted by protobuf JSON also represent valid zeroes.
    p['estimateBuffer'] = {}
    p['locations'][1].pop('lng',None); p['locations'][1].pop('lat',None)
    api('/v1/scenarios', {'scenarioId':'zero-settings','name':'零值语义测试','request':request})
    page.set_viewport_size({'width':1500,'height':1000})
    page.goto(base+'/#scene/zero-settings/data'); tab('rules')
    expect(cell('costs',1,'value')).to_have_value('0')
    expect(cell('costs',2,'value')).to_have_value('0')
    edit('costs',1,'value','150')
    zero = draft()['estimateBuffer']
    assert zero.get('modelPermille') == 150 and zero.get('fallbackPermille',0) == 0, zero
    tab('locations')
    expect(cell('locations',1,'lng')).to_have_value('0')
    expect(cell('locations',1,'lat')).to_have_value('0')
    page.close()
    print('Settings sheets passed: inline edits, pointer navigation, atomic paste, zero/defaults, undo and matrix preservation.')


def check_background_refresh(browser, base, api, original, errors):
    """Polling must keep reading state without rebuilding an unchanged matrix."""
    for i in range(16):
        api('/v1/scenarios', {'scenarioId': f'scroll-{i:02}', 'name': f'滚动检查 {i:02}', 'request': original['request']})
    scene = 'scroll-00'
    page = browser.new_page(viewport={'width': 1440, 'height': 900})
    page.on('pageerror', lambda e: errors.append(str(e)))
    page.route('https://tile.openstreetmap.org/**', lambda route: route.abort())
    polls = []
    page.on('response', lambda response: polls.append(response.url)
            if '/v1/jobs?' in response.url and f'scenario_id={scene}' in response.url else None)
    page.goto(base + f'/#scene/{scene}/data')
    page.locator('[data-data-tab=matrix]').click()
    page.locator('#matrix-zoom').select_option('4')
    page.locator('#matrix-canvas').focus()
    page.evaluate('''() => {
        document.querySelector('.matrix-scroll').scrollLeft=300;
        document.querySelector('#scenes').scrollTop=250;
        window.scrollTo(0,500);
        window.beforeCanvas=document.querySelector('#matrix-canvas');
    }''')

    def position():
        return page.evaluate('''() => ({page:scrollY, matrix:document.querySelector('.matrix-scroll').scrollLeft,
            sidebar:document.querySelector('#scenes').scrollTop})''')

    def wait_polls(count):
        deadline = time.monotonic() + 12
        while len(polls) < count:
            if time.monotonic() > deadline:
                raise AssertionError('background polling stopped')
            page.wait_for_timeout(100)
        page.wait_for_timeout(150)

    before = position()
    assert all(v > 0 for v in before.values()), before
    wait_polls(len(polls) + 2)
    assert page.evaluate('beforeCanvas===document.querySelector("#matrix-canvas")')
    assert position() == before, (before, position())

    # A real new solver result still arrives through polling; replacing the
    # default-height canvas must preserve both document and horizontal scroll.
    job = api('/v1/jobs', {'scenario': {'scenarioId': scene, 'version': '1'},
                          'options': {'timeLimitMs': '100', 'threads': 1}})
    page.wait_for_function('() => beforeCanvas!==document.querySelector("#matrix-canvas")', timeout=12000)
    assert position() == before, (before, position())
    deadline = time.monotonic() + 10
    while api('/v1/jobs/' + job['id']).get('state', 'JOB_STATE_QUEUED') in ['JOB_STATE_QUEUED', 'JOB_STATE_RUNNING', 'JOB_STATE_CANCELLING']:
        if time.monotonic() > deadline:
            raise AssertionError('test solve did not finish')
        page.wait_for_timeout(100)
    wait_polls(len(polls) + 1)
    assert position() == before, (before, position())

    # Explicit refresh and resize rebuild the same view, with stable positions.
    page.locator('#scene-search').evaluate('(el)=>el.focus({preventScroll:true})')
    page.wait_for_timeout(250)
    page.evaluate('window.beforeCanvas=document.querySelector("#matrix-canvas")')
    page.locator('[data-action=refresh]').evaluate('(el)=>el.click()')
    page.wait_for_function('() => beforeCanvas!==document.querySelector("#matrix-canvas")')
    assert position() == before, (before, position())
    page.evaluate('window.beforeCanvas=document.querySelector("#matrix-canvas")')
    page.set_viewport_size({'width': 1400, 'height': 900})
    page.wait_for_function('() => beforeCanvas!==document.querySelector("#matrix-canvas")')
    assert position() == before, (before, position())

    # A response that started before typing must not replace the focused cell.
    page.locator('[data-data-tab=tickets]').click()
    held = []
    page.route(base + '/healthz', lambda route: held.append(route))
    page.evaluate('document.activeElement.blur()')
    deadline = time.monotonic() + 8
    while not held:
        if time.monotonic() > deadline:
            raise AssertionError('poll request did not start')
        page.wait_for_timeout(100)
    second = api('/v1/jobs', {'scenario': {'scenarioId': scene, 'version': '1'},
                             'options': {'timeLimitMs': '100', 'threads': 1}})
    cell = page.locator('#sheet-tickets-0-name')
    cell.fill('轮询返回时仍在输入')
    page.evaluate('window.typingCell=document.querySelector("#sheet-tickets-0-name")')
    target_poll = len(polls) + 1
    held.pop().fulfill(status=200, content_type='application/json', body=json.dumps(api('/healthz')))
    wait_polls(target_poll)
    page.unroute(base + '/healthz')
    assert page.evaluate('typingCell===document.querySelector("#sheet-tickets-0-name")')
    expect(cell).to_be_focused()
    expect(cell).to_have_value('轮询返回时仍在输入')
    cell.press('Tab')
    page.locator('[data-tab=plans]').click()
    expect(page.locator(f'#selected-job option[value="{second["id"]}"]')).to_be_attached()

    # Live operation clocks keep ticking even with an unchanged revision.
    live = 'scroll-01'
    api('/v1/operations/start', {'scenario': {'scenarioId': live, 'version': '1'},
        'at': '28800', 'actor': 'browser regression',
        'policy': {'clock': 'EXECUTION_CLOCK_LIVE', 'autoReplan': False, 'autoAdopt': False,
                   'loadingLocationId': original['request']['problem']['locations'][0]['id']}})
    page.goto(base + f'/#scene/{live}/operations')
    expect(page.locator('#scene-context')).to_contain_text('执行中')
    subtitle = page.locator('#scene-subtitle').inner_text()
    page.wait_for_function('old=>document.querySelector("#scene-subtitle").textContent!==old', arg=subtitle, timeout=8000)
    page.close()
    print('Refresh regression passed: unchanged polling, changed job state, manual refresh, resize, editing race and live clock.')


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument('--server', type=Path, default=root/'target/debug/vrp-server.exe')
    args = parser.parse_args()
    folder = root/'corpus'/f'sheet-smoke-{time.time_ns()}'
    folder.mkdir()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    client = build_opener(ProxyHandler({}))
    def api(path, body=None):
        req = Request(base+path, data=json.dumps(body).encode() if body is not None else None, headers={'Content-Type':'application/json','Accept':'application/json'})
        with client.open(req, timeout=15) as r: return json.load(r)
    env = dict(os.environ); env.pop('VRP_API_TOKEN', None)
    with (folder/'server.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(args.server.resolve()), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}', '--data-dir', str(folder/'data'), '--total-threads', '2', '--geo-prefetch', 'false'], cwd=root, env=env, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
        try:
            deadline=time.monotonic()+20
            while True:
                try: api('/healthz'); break
                except OSError:
                    if time.monotonic()>deadline: raise
                    time.sleep(.1)
            with sync_playwright() as pw:
                browser=pw.chromium.launch()
                page=browser.new_page(viewport={'width':1500,'height':1000})
                page.set_default_timeout(10000)
                errors=[]; page.on('pageerror', lambda e: errors.append(str(e)))
                page.route('https://tile.openstreetmap.org/**', lambda route:route.abort())
                api('/v1/scenarios', workbench_scenario('sheet-fixture'))
                page.goto(base + '/#scene/sheet-fixture/data')
                expect(page.locator('[data-sheet=tickets]')).to_be_visible()
                scene=api('/v1/scenarios')['scenarios'][0]['id']
                original=api('/v1/scenarios/'+scene)
                def cell(row,key,kind='tickets'):
                    return page.locator(f'[id="sheet-{kind}-{row}-{key}"]')
                def paste(row,key,text):
                    cell(row,key).focus()
                    cell(row,key).evaluate('(el,text)=>{const data=new DataTransfer();data.setData("text/plain",text);el.dispatchEvent(new ClipboardEvent("paste",{clipboardData:data,bubbles:true,cancelable:true}));}', text)
                def tab(name): page.locator(f'[data-data-tab={name}]').click()
                def save():
                    if page.locator('#notice').is_visible():
                        page.locator('#notice-close').click()
                    page.evaluate('window.scrollTo(0,0)')
                    page.locator('[data-action=save-draft]').click()
                    expect(page.locator('[data-action=save-draft]')).to_be_disabled()
                cell(0,'name').fill('行内编辑测试')
                cell(0,'name').press('Tab')
                expect(cell(0,'name')).to_have_value('行内编辑测试')
                assert api('/v1/scenarios/'+scene)['scenario']['version']==original['scenario']['version']
                # A rectangular Excel paste is validated as a whole before applying.
                paste(0,'earliestStart','08:30\t10:30\n09:00\t11:30')
                expect(cell(1,'latestStart')).to_have_value('11:30')
                page.locator('[data-sheet-command=undo]').click()
                expect(cell(1,'latestStart')).not_to_have_value('11:30')
                page.locator('[data-sheet-command=redo]').click()
                expect(cell(1,'latestStart')).to_have_value('11:30')
                # Undo spans panels without dropping earlier cell edits.
                tab('rules')
                weight=page.locator('[data-weight=minimizeTravelTime][data-level=soft]')
                old_weight=weight.input_value()
                weight.fill('7')
                tab('tickets'); page.locator('[data-sheet-command=undo]').click()
                expect(cell(0,'name')).to_have_value('行内编辑测试')
                tab('rules'); expect(weight).to_have_value(old_weight)
                tab('tickets'); page.locator('[data-sheet-command=redo]').click()
                tab('rules'); expect(weight).to_have_value('7'); tab('tickets')
                before=cell(0,'earliestStart').input_value()
                paste(0,'earliestStart','08:00\t12:00\n25:00\t11:00')
                expect(page.locator('.sheet-status')).to_contain_text('粘贴未应用')
                expect(cell(0,'earliestStart')).to_have_value(before)
                cell(0,'duration').fill('错误')
                cell(0,'duration').press('Tab')
                expect(cell(0,'duration')).to_have_attribute('aria-invalid','true')
                if page.locator('#notice').is_visible():
                    page.locator('#notice-close').click()
                page.evaluate('window.scrollTo(0,0)')
                page.locator('[data-action=save-draft]').click()
                expect(page.locator('#notice')).to_contain_text('标红')
                cell(0,'duration').press('Escape')
                expect(cell(0,'duration')).not_to_have_attribute('aria-invalid','true')
                page.locator('#notice-close').click()
                # Selection, fill down, keyboard movement and undo.
                cell(0,'duration').fill('45'); cell(0,'duration').press('Tab')
                cell(0,'duration').click(); cell(1,'duration').click(modifiers=['Shift'])
                page.locator('[data-sheet-command=fill]').click()
                expect(cell(1,'duration')).to_have_value('45')
                save()
                saved=api('/v1/scenarios/'+scene)
                assert saved['request']['problem']['tickets'][1]['duration']=='2700'
                # Shared coordinates: default to a new location for this resource.
                tab('agents')
                original_locs=saved['request']['problem']['locations']
                page.locator('[data-action=linked-location][data-kind=agents][data-index="0"][data-field=startLocation]').click()
                expect(page.locator('[name=locationScope]')).to_have_value('copy')
                page.locator('[name=name]').fill('新出发点')
                page.locator('[name=lng]').fill('121.48123')
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible()
                expect(cell(0,'startLocation','agents')).to_have_value(str(len(original_locs)))
                save()
                saved=api('/v1/scenarios/'+scene)
                assert saved['request']['problem']['locations'][:-1]==original_locs
                assert float(saved['request']['problem']['locations'][-1]['lng'])==121.48123
                assert saved['request']['problem']['agents'][1].get('startLocation',0)==0
                tab('tickets'); cell(2,'name').fill('切换后保留'); cell(2,'name').press('Tab')
                # A native directional matrix with explicit missing/unreachable values.
                fixture=json.loads(json.dumps(original['request']))
                p=fixture['problem']; p['locations']=p['locations'][:4]; p['tickets']=p['tickets'][:3]
                durations=[0,120,4294967294,600,900,0,4294967295,60,400,500,0,300,800,200,700,0]
                distances=[v if v>=4294967294 else v*10 for v in durations]
                encode=lambda values:base64.b64encode(struct.pack('<'+'I'*len(values),*values)).decode()
                p['matrices']=[{'profile':'测试车型','durations':encode(durations),'distances':encode(distances)}]
                api('/v1/scenarios',{'scenarioId':'matrix-fixture','name':'矩阵交互测试','request':fixture,'origin':{'historicalValuesUnverified':True}})
                page.goto(base+'/#scene/matrix-fixture/data'); tab('matrix')
                expect(page.locator('#matrix-canvas')).to_be_visible()
                expect(page.locator('#matrix-cell-detail')).to_contain_text('120 秒')
                expect(page.locator('#matrix-cell-detail')).to_contain_text('历史导入')
                page.locator('#matrix-from').select_option('1')
                page.locator('#matrix-to').select_option('0')
                expect(page.locator('#matrix-cell-detail')).to_contain_text('900 秒')
                page.locator('[data-matrix-metric=distance]').click()
                expect(page.locator('#matrix-max')).to_contain_text('9 km')
                page.locator('#matrix-to').select_option('2')
                expect(page.locator('#matrix-cell-detail')).to_contain_text('不可达')
                page.locator('#matrix-from').select_option('0')
                expect(page.locator('#matrix-cell-detail')).to_contain_text('待估算')
                page.locator('#matrix-from').select_option(''); page.locator('#matrix-to').select_option('')
                page.locator('#matrix-canvas').press('ArrowRight')
                page.locator('[data-matrix-locate]').first.click()
                expect(page.locator('.data-location-panel')).to_be_visible()
                page.screenshot(path=str(folder/'matrix.png'), full_page=True)
                page.goto(base+'/#scene/'+scene+'/data')
                expect(cell(2,'name')).to_have_value('切换后保留')
                # The grid keeps only a small row window for a larger request.
                large=json.loads(json.dumps(original['request']))
                template=large['problem']['tickets'][0]
                large['problem']['tickets']=[{**template,'id':f'T{i:03}'} for i in range(180)]
                api('/v1/scenarios',{'scenarioId':'large-sheet','name':'工作表滚动测试','request':large})
                page.goto(base+'/#scene/large-sheet/data')
                expect(page.locator('#scene-title')).to_have_text('工作表滚动测试')
                expect(cell(0,'name')).to_be_attached()
                assert page.locator('.sheet-table tbody tr[data-sheet-row]').count()<=70
                page.locator('.sheet-scroll').evaluate('el=>el.scrollTop=4200')
                page.wait_for_timeout(250)
                print('Virtual scroll:',page.locator('.sheet-scroll').evaluate('el=>({top:el.scrollTop,height:el.clientHeight,total:el.scrollHeight,first:el.querySelector("[data-sheet-row]")?.dataset.sheetRow})'))
                expect(cell(145,'name')).to_be_attached()
                cell(145,'name').fill('后部行编辑'); cell(145,'name').press('Tab'); save()
                assert api('/v1/scenarios/large-sheet')['scenario']['display']['tickets']['T145']['name']=='后部行编辑'
                for width in [1500,900,390]:
                    page.set_viewport_size({'width':width,'height':1000})
                    page.wait_for_timeout(150)
                    geometry=page.evaluate('()=>[document.documentElement,document.querySelector("#sidebar")].map(e=>[e.clientWidth,e.scrollWidth])')
                    assert all(scroll<=client+1 for client,scroll in geometry),(width,geometry)
                page.set_viewport_size({'width':1500,'height':1000})
                page.screenshot(path=str(folder/'sheet.png'),full_page=True)
                check_settings_sheets(browser, base, api, original, folder, errors)
                check_predecessor_sheet(browser, base, api, original, folder, errors)
                check_compact_workbar(browser, base, api, original, folder, errors)
                check_background_refresh(browser, base, api, original, errors)
                assert not errors,errors
                browser.close()
            print('Data workbench browser checks passed:',folder)
        finally:
            process.terminate(); process.wait(timeout=15)


if __name__=='__main__': main()
