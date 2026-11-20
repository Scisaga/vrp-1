"""Check score axes, English levels, unassigned explanations and planning layout."""
import argparse
import json
import os
import re
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
    folder = root / 'corpus' / f'planning-layout-{time.time_ns()}'
    folder.mkdir()
    (folder / 'map.json').write_text('{"disabled":true}', encoding='utf-8')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    opener = build_opener(ProxyHandler({}))

    def api(path, data=None):
        request = Request(base + path, data=None if data is None else json.dumps(data).encode(),
                          headers={'Accept': 'application/json', 'Content-Type': 'application/json'})
        with opener.open(request, timeout=20) as response:
            return json.load(response)

    request = {'problem': {
        'id': 'chart', 'horizon': {'startLocal': '2026-11-02T00:00:00', 'timezone': 'Asia/Shanghai'},
        'locations': [{'id': 'depot', 'lng': 121.5, 'lat': 31.2}],
        'matrices': [{'durations': 'AAAAAA==', 'distances': 'AAAAAA==', 'sources': 'AA=='}],
        'agents': [{'id': 'A', 'skills': ['basic'], 'startTime': '21600', 'endTime': '43200'}],
        'tickets': [{'id': f'T{i}', 'duration': 60, 'earliestStart': '21600', 'latestStart': '43000',
                     'latestEnd': '43200', 'skillsRequired': ['basic' if i == 0 else 'expert']} for i in range(11)],
        'weights': {'ticketUnassigned': {'medium': '1000'}, 'agentSkillsAccordWithTicketSkills': {'hard': '1'}, 'balanceAgentLoading': {}},
    }, 'options': {'timeLimitMs': '3000', 'threads': 1}}
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
            api('/v1/scenarios', {'scenarioId': 'chart', 'name': '评分与排程演示', 'request': request,
                                  'display': {'agents': {'A': {'name': '资源001'}}}})
            job = api('/v1/jobs', {'scenario': {'scenarioId': 'chart', 'version': '1'}})
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                page = browser.new_page(viewport={'width': 1600, 'height': 1000})
                errors = []
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.goto(base + '/#scene/chart/plans')
                expect(page.locator('.progress-chart')).to_be_visible(timeout=10000)
                expect(page.locator('.plan-workspace')).to_be_visible(timeout=15000)
                result = api('/v1/jobs/' + job['id'] + '/result')
                assert len(result['unassigned']) == 10
                assert int(result['score'].get('hard', 0)) == 0
                assert int(result['score']['medium']) == -10000
                expect(page.locator('.progress-panel .score')).to_contain_text('0 Hard')
                expect(page.locator('.progress-panel .score')).to_contain_text('-10,000 Medium')
                expect(page.locator('[data-score-explanation]')).to_have_attribute('title', re.compile('10 未分配 → -10,000 Medium'))
                expect(page.locator('.plan-overview #selected-job')).to_be_visible()
                expect(page.locator('.plan-state-line,.plan-workspace > .work-toolbar')).to_have_count(0)
                expect(page.locator('#plan-diagnostics,.workspace-view-tabs')).to_have_count(0)
                expect(page.locator('[data-result-tab]')).to_have_count(3)
                expect(page.locator('#result-tab-schedule')).to_have_attribute('aria-selected', 'true')
                expect(page.locator('.schedule-head #resource-filter')).to_be_visible()
                expect(page.locator('.schedule-head #schedule-filter')).to_be_visible()
                expect(page.locator('.schedule-head h2,.work-toolbar #resource-filter,.work-toolbar #schedule-filter')).to_have_count(0)
                page.locator('#schedule-filter').fill('__no_match__')
                expect(page.locator('.resource-cell')).to_have_count(0)
                page.locator('#schedule-filter').fill('')
                expect(page.locator('.resource-cell')).to_have_count(1)
                expect(page.locator('.brand-symbol')).to_have_attribute('src', '/brand.svg?v=2')
                assert page.locator('.brand-symbol').evaluate('(img)=>img.complete && img.naturalWidth>0')
                assert page.locator('link[rel=icon]').get_attribute('href') == '/brand.svg?v=2'
                expect(page.locator('#scenes')).to_contain_text('执行单元')
                expect(page.locator('.inspector-header')).to_contain_text('执行单元001')
                saved_trend = api('/v1/jobs/' + job['id'] + '/trend')
                assert not saved_trend.get('partialHistory')
                assert len(saved_trend['samples']) >= 2
                expect(page.locator('.progress-chart')).to_have_count(1)
                expect(page.locator('.progress-chart .trend-series')).to_have_count(2)
                expect(page.locator('[data-score-level=soft] .trend-point')).to_have_count(len(saved_trend['samples']))
                page.reload()
                expect(page.locator('[data-score-level=soft] .trend-point')).to_have_count(len(saved_trend['samples']))
                assert api('/v1/jobs/' + job['id'] + '/trend')['samples'] == saved_trend['samples']
                expect(page.locator('.trend-axis-title').last).to_have_text('求解耗时（秒）')
                expect(page.locator('.trend-y-tick').first).to_be_visible()
                page.locator('#progress-score').select_option('hard')
                expect(page.locator('.progress-chart')).to_have_attribute('data-score-level', 'hard')
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=refresh]').click()
                expect(page.locator('#progress-score')).to_have_value('hard')
                page.locator('#progress-score').select_option('soft')
                page.locator('[data-action=show-unassigned]').click()
                expect(page.locator('#result-tab-unassigned')).to_have_attribute('aria-selected', 'true')
                expect(page.locator('#plan-unassigned tbody tr')).to_have_count(10)
                expect(page.locator('.schedule-panel')).to_have_count(0)
                page.locator('#plan-unassigned [data-select-ticket]').first.click()
                expect(page.locator('#result-tab-schedule')).to_have_attribute('aria-selected', 'true')
                expect(page.locator('.inspector-header')).to_contain_text('未分配工单')
                page.locator('[data-select-agent="A"]').click()
                expect(page.locator('.inspector-header')).to_contain_text('执行单元001')
                page.locator('#result-tab-unassigned').click()
                page.locator('#result-tab-unassigned').press('End')
                expect(page.locator('#result-tab-constraints')).to_have_attribute('aria-selected', 'true')
                expect(page.locator('#plan-constraints')).to_contain_text('-10,000 Medium')
                page.locator('#result-tab-constraints').press('Home')
                expect(page.locator('#result-tab-schedule')).to_have_attribute('aria-selected', 'true')
                page.locator('[data-action=task-info]').first.click()
                expect(page.locator('#dialog-fields')).to_contain_text('候选评估')
                expect(page.locator('#dialog-fields [data-action=download-input]')).to_be_visible()
                page.locator('#dialog-submit').click()
                page.evaluate('window.scrollTo(0,0)')
                page.screenshot(path=str(folder / 'desktop.png'), full_page=True)

                # Exercise the pure SVG renderer against numeric edge cases.
                fixtures = page.evaluate('''async () => {
                    const {progressChart,progressCharts} = await import('/progress.js');
                    const render = (values, level='soft') => {
                        const html=progressChart(values.map((value,i)=>({elapsedMs:String(i*10000),bestScore:{soft:String(value)}})),{level,width:420,elapsedMs:'30000'});
                        const node=document.createElement('div'); node.innerHTML=html;
                        return {html, ys:[...node.querySelectorAll('circle')].map(e=>Number(e.getAttribute('cy'))),
                            tickValues:[...node.querySelectorAll('.trend-y-tick')].map(e=>e.dataset.value),
                            xTicks:[...node.querySelectorAll('.trend-x-tick')].map(e=>e.dataset.value)};
                    };
                    const overlayHtml=progressChart([
                        {elapsedMs:'0',bestScore:{hard:'-1',medium:'-1000',soft:'-1152921504606846976'}},
                        {elapsedMs:'10000',bestScore:{hard:'0',medium:'-500',soft:'-1152921504606846975'}}
                    ],{levels:['hard','medium','soft'],width:280,elapsedMs:'10000'});
                    const overlayNode=document.createElement('div'); overlayNode.innerHTML=overlayHtml;
                    const overlay={html:overlayHtml,ys:[...overlayNode.querySelectorAll('.trend-series[data-score-level=soft] circle')].map(e=>Number(e.getAttribute('cy'))),
                        plots:overlayNode.querySelectorAll('.progress-chart').length,axes:overlayNode.querySelectorAll('.trend-y-axis').length,
                        offset:overlayNode.querySelector('[data-score-legend=soft]').textContent};
                    return {empty:render([]),one:render([-100]),flat:render([-100,-100]),
                        precise:render(['-1152921504606846976','-1152921504606846975']),fall:render([-100,-200]),zero:render([0,0],'hard'),overlay};
                }''')
                assert len(fixtures['one']['ys']) == 1
                assert fixtures['flat']['ys'][0] == fixtures['flat']['ys'][1]
                assert fixtures['precise']['ys'][1] < fixtures['precise']['ys'][0]
                assert fixtures['fall']['ys'][1] > fixtures['fall']['ys'][0]
                assert '0' in fixtures['zero']['tickValues']
                assert '30' in fixtures['one']['xTicks']
                assert fixtures['overlay']['plots'] == 1 and fixtures['overlay']['axes'] == 3
                assert fixtures['overlay']['ys'][1] < fixtures['overlay']['ys'][0]
                assert 'Δ 基准 -1,152,921,504,606,846,975' in fixtures['overlay']['offset']
                for case in fixtures.values():
                    assert 'NaN' not in case['html'] and 'Infinity' not in case['html']

                detail = page.evaluate('''async () => {
                    const {progressCharts}=await import('/progress.js');
                    const points=Array.from({length:20},(_,i)=>({elapsedMs:String(i*1000),
                        bestScore:{hard:'0',medium:'-20',soft:String(i ? -1000+i*10 : -1000000)}}));
                    const snapshot=JSON.stringify(points);
                    const render=range=>{
                        const node=document.createElement('div');
                        node.innerHTML=progressCharts(points,{mode:'soft',score:points.at(-1).bestScore,elapsedMs:'19000',range});
                        return {overview:node.querySelectorAll('.trend-overview').length,
                            range:node.querySelector('.trend-charts').dataset.progressRange,
                            firstTime:node.querySelector('.trend-x-tick').dataset.value,
                            dots:node.querySelectorAll('.trend-point').length,
                            summary:node.querySelector('.trend-summary').textContent,
                            lastTitle:[...node.querySelectorAll('.trend-point title')].at(-1).textContent,
                            html:node.innerHTML};
                    };
                    return {auto:render('auto'),full:render('full'),unchanged:JSON.stringify(points)===snapshot};
                }''')
                assert detail['unchanged']
                assert detail['auto']['range'] == 'detail' and detail['auto']['overview'] == 1
                assert detail['auto']['firstTime'] == '1' and detail['auto']['dots'] == 19
                assert detail['full']['range'] == 'full' and detail['full']['overview'] == 0
                assert detail['full']['firstTime'] == '0' and detail['full']['dots'] == 20
                assert '19 次改善' in detail['auto']['summary']
                assert 'Soft -810' in detail['auto']['lastTitle']
                assert all('NaN' not in item['html'] for item in [detail['auto'], detail['full']])
                page.locator('#progress-range').select_option('full')
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=refresh]').click()
                expect(page.locator('#progress-range')).to_have_value('full')

                page.set_viewport_size({'width': 1000, 'height': 950})
                page.wait_for_function('()=>Math.abs(document.querySelector(".progress-chart").viewBox.baseVal.width-document.querySelector(".progress-chart").clientWidth)<5')
                expect(page.locator('.workspace-view-tabs')).to_have_count(0)
                expect(page.locator('.schedule-panel')).to_be_visible()
                expect(page.locator('.inspector')).to_be_visible()
                expect(page.locator('.selected-schedule')).to_be_visible()
                expect(page.locator('.inspector .map-frame')).to_be_visible()
                page.locator('[data-action=zoom-in]').click()
                page.locator('#gantt-scroll').evaluate('(e)=>e.scrollLeft=30')
                before = page.locator('#gantt-scroll').evaluate('(e)=>e.scrollLeft')
                page.locator('#result-tab-constraints').click()
                expect(page.locator('.schedule-panel')).to_have_count(0)
                page.locator('#result-tab-schedule').click()
                expect(page.locator('#gantt-scroll')).to_be_visible()
                assert abs(page.locator('#gantt-scroll').evaluate('(e)=>e.scrollLeft') - before) < 1
                page.evaluate('window.scrollTo(0,0)')
                page.screenshot(path=str(folder / 'narrow.png'), full_page=True)
                page.locator('.route-row [data-select-ticket]').first.click()
                expect(page.locator('.inspector')).to_be_visible()
                expect(page.locator('.detail-body')).to_contain_text('工单编号')
                page.set_viewport_size({'width': 420, 'height': 900})
                page.wait_for_function('()=>Math.abs(document.querySelector(".progress-chart").viewBox.baseVal.width-document.querySelector(".progress-chart").clientWidth)<5')
                expect(page.locator('.progress-chart')).to_be_visible()
                expect(page.locator('.schedule-panel')).to_be_visible()
                expect(page.locator('.inspector')).to_be_visible()
                expect(page.locator('[data-selected-schedule]')).to_be_visible()
                expect(page.locator('.inspector .map-frame')).to_be_visible()
                assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth')
                page.screenshot(path=str(folder / 'mobile.png'), full_page=True)
                assert not errors, errors
                assert api('/v1/scenarios/chart')['scenario']['display']['agents']['A']['name'] == '资源001'

                # Large workloads use the native compact layout and can return
                # from the global route overview to an individual unit.
                large = json.loads(json.dumps(request))
                large['problem'].update({'id': 'large', 'agents': [
                    {'id': f'A{i:02}', 'skills': ['basic'], 'startTime': '21600', 'endTime': '43200', 'maxTickets': 2}
                    for i in range(80)], 'tickets': [
                    {'id': f'L{i:03}', 'duration': 60, 'earliestStart': '21600', 'latestEnd': '43200', 'skillsRequired': ['basic']}
                    for i in range(160)]})
                large['initialSolution'] = {'routes': [{'agent': i, 'tickets': [i*2, i*2+1]} for i in range(80)]}
                large['options']['timeLimitMs'] = '1000'
                api('/v1/scenarios', {'scenarioId': 'large', 'name': '80 执行单元排程', 'request': large})
                api('/v1/jobs', {'scenario': {'scenarioId': 'large', 'version': '1'}})
                page.set_viewport_size({'width': 1920, 'height': 1680})
                page.goto(base + '/#scene/large/plans')
                expect(page.locator('.workbench.compact')).to_be_visible(timeout=10000)
                expect(page.locator('.route-row')).to_have_count(80)
                expect(page.locator('.schedule-count')).to_contain_text('80')
                visible = page.locator('.route-row').evaluate_all('(rows)=>rows.filter(r=>{const b=r.getBoundingClientRect();return b.top>=0&&b.bottom<=innerHeight}).length')
                assert visible >= 40, visible
                page.locator('[data-action=schedule-density]').click()
                expect(page.locator('.workbench.compact')).to_have_count(0)
                page.locator('[data-action=schedule-density]').click()
                expect(page.locator('.workbench.compact')).to_be_visible()
                page.locator('[data-action=clear-selection]').click()
                expect(page.locator('.inspector-header')).to_contain_text('全局路线')
                page.locator('#resource-filter').select_option('A00')
                expect(page.locator('.route-row')).to_have_count(1)
                expect(page.locator('.inspector-header')).not_to_contain_text('全局路线')
                page.locator('#resource-filter').select_option('')
                expect(page.locator('.route-row')).to_have_count(80)
                expect(page.locator('.inspector-header')).to_contain_text('全局路线')
                page.locator('.resource-cell[data-select-agent]').first.click()
                expect(page.locator('.inspector-header')).not_to_contain_text('全局路线')
                expect(page.locator('.detail-body')).to_contain_text('执行单元编号')
                assert not errors, errors
                browser.close()
                print(json.dumps({'passed': True, 'folder': str(folder), 'checks': [
                    'actual axes', 'English levels', 'unassigned Medium explanation', 'score selector persistence',
                    'single and flat values', 'large integer precision', 'real lower-level decreases',
                    'parallel result tabs', 'durable chart after reload', 'task info', 'dedicated icon',
                    'simultaneous narrow schedule and map', 'scroll preservation', 'no page overflow',
                    'honest optimization detail with full overview', 'range selector persistence',
                    '80-unit density toggle', '40 visible units', 'global route selection',
                ]}, ensure_ascii=False))
        finally:
            process.terminate()
            process.wait(timeout=15)


if __name__ == '__main__':
    main()
