"""Exercise scenario summaries and guarded deletion on isolated synthetic data."""
import argparse
import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.error import HTTPError
from urllib.request import Request, ProxyHandler, build_opener

from playwright.sync_api import expect, sync_playwright
from workbench_fixture import workbench_scenario


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, default=root / 'target/debug/vrp-server.exe')
    args = parser.parse_args()
    folder = root / 'corpus' / f'scenario-management-{time.time_ns()}'
    folder.mkdir()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    opener = build_opener(ProxyHandler({}))

    def api(path, body=None, method=None):
        request = Request(base + path, data=None if body is None else json.dumps(body).encode(),
                          headers={'Accept': 'application/json', 'Content-Type': 'application/json'}, method=method)
        with opener.open(request, timeout=20) as response:
            return json.load(response)

    def absent(path):
        try:
            api(path)
        except HTTPError as error:
            assert error.code == 404
        else:
            raise AssertionError('Deleted record remains: ' + path)

    def wait_job(job):
        deadline = time.monotonic() + 15
        while True:
            job = api('/v1/jobs/' + job['id'])
            if job.get('state') not in [None, 'JOB_STATE_QUEUED', 'JOB_STATE_RUNNING', 'JOB_STATE_CANCELLING']:
                assert job.get('hasResult'), job
                return job
            assert time.monotonic() < deadline
            time.sleep(.05)

    env = dict(os.environ)
    env.pop('VRP_API_TOKEN', None)
    with (folder / 'server.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(args.server.resolve()), '--basemap-auto', 'false', '--bind', f'127.0.0.1:{port}',
                                    '--data-dir', str(folder / 'data'), '--total-threads', '2', '--geo-prefetch', 'false'],
                                   cwd=root, env=env, stdout=log, stderr=log,
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
            with sync_playwright() as pw:
                browser = pw.chromium.launch()
                page = browser.new_page(viewport={'width': 1600, 'height': 1000})
                page.route('https://tile.openstreetmap.org/**', lambda route: route.abort())
                errors = []
                page.on('pageerror', lambda error: errors.append(str(error)))
                page.set_default_timeout(12000)
                api('/v1/scenarios', workbench_scenario('alpha'))
                page.goto(base + '/#scene/alpha/data')
                expect(page.locator('[data-sheet=tickets]')).to_be_visible()
                alpha = api('/v1/scenarios')['scenarios'][0]
                alpha_id = alpha['id']
                template = api('/v1/scenarios/' + alpha_id)['request']
                for scene_id in ['beta', 'gamma']:
                    api('/v1/scenarios', {'scenarioId': scene_id, 'name': '摘要与删除 ' + scene_id, 'request': template})
                beta = page.locator('#scenes [data-scene=beta]')
                gamma = page.locator('#scenes [data-scene=gamma]')
                expect(beta).to_be_visible(timeout=8000)
                expect(beta).to_contain_text('待生成方案')
                tickets = len(template['problem']['tickets'])
                agents = len(template['problem']['agents'])
                expect(beta.locator('.scene-list-counts')).to_have_text(f'{tickets} 工单 · {agents} 执行单元')
                expect(beta.locator('.scene-list-updated')).to_contain_text('更新')
                expect(page.locator('#scene-context')).to_have_text('待生成方案')

                # Counts and live states are present before the scene is opened.
                updated = copy.deepcopy(template)
                extra = copy.deepcopy(updated['problem']['tickets'][0])
                extra['id'] = 'extra-summary-ticket'
                updated['problem']['tickets'].append(extra)
                api('/v1/scenarios', {'scenarioId': 'gamma', 'name': '摘要与删除 gamma', 'expectedVersion': '1', 'request': updated})
                expect(gamma.locator('.scene-list-counts')).to_have_text(f'{tickets + 1} 工单 · {agents} 执行单元', timeout=8000)
                expect(gamma.locator('.scene-list-updated')).to_contain_text('v2')
                options = {'threads': 1, 'timeLimitMs': '100'}
                source = wait_job(api('/v1/jobs', {'scenario': {'scenarioId': 'beta', 'version': '1'}, 'options': options}))
                child = wait_job(api('/v1/jobs/' + source['id'] + '/continue', {'options': options}))
                expect(beta).to_contain_text('已有方案', timeout=8000)

                # A draft in a different workspace must survive deletion.
                page.locator('[data-data-tab=advanced]').click()
                raw_alpha = json.loads(page.locator('#json-editor').input_value())
                raw_alpha.setdefault('options', {})['seed'] = '9988'
                page.locator('#json-editor').fill(json.dumps(raw_alpha, ensure_ascii=False))
                beta.click()
                expect(page.locator('#scene-title')).to_have_text('摘要与删除 beta')
                page.locator('[data-tab=plans]').click()
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=delete-scene]').click()
                expect(page.locator('#dialog')).to_be_visible()
                expect(page.locator('#dialog-fields')).to_contain_text('关联求解任务')
                expect(page.locator('#dialog-fields')).to_contain_text('2')
                page.locator('#dialog-cancel').click()
                assert api('/v1/scenarios/beta/metadata')['version'] == '1'

                # The version is checked again on DELETE, after confirmation.
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=delete-scene]').click()
                api('/v1/scenarios', {'scenarioId': 'beta', 'name': '摘要与删除 beta', 'expectedVersion': '1', 'request': template})
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog-error')).to_be_visible()
                assert api('/v1/scenarios/beta/metadata')['version'] == '2'
                assert api('/v1/jobs/' + child['id'])['id'] == child['id']
                page.locator('#dialog-cancel').click()
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=delete-scene]').click()
                page.screenshot(path=str(folder / 'delete-confirmation.png'), full_page=True)
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible()
                expect(beta).to_have_count(0)
                absent('/v1/scenarios/beta/metadata')
                absent('/v1/jobs/' + source['id'])
                absent('/v1/jobs/' + child['id'])
                page.locator(f'#scenes [data-scene="{alpha_id}"]').click()
                expect(page.locator('#scene-save-state')).to_contain_text('未保存')
                expect(page.locator('#json-editor')).to_have_value(json.dumps(raw_alpha, ensure_ascii=False))

                page.screenshot(path=str(folder / 'sidebar-desktop.png'), full_page=True)
                page.set_viewport_size({'width': 420, 'height': 900})
                expect(gamma.locator('.scene-list-counts')).to_be_visible()
                expect(gamma.locator('.scene-list-updated')).to_be_visible()
                assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth')
                assert page.locator('#scenes').evaluate('(el) => el.scrollWidth <= el.clientWidth')
                page.screenshot(path=str(folder / 'sidebar-mobile.png'), full_page=True)
                page.set_viewport_size({'width': 1600, 'height': 1000})

                # Global deletion does not need to open or download the scene.
                page.locator('.global-nav [data-nav=scenes]').click()
                page.locator('[data-action=delete-scene][data-scenario-id=gamma]').click()
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible()
                absent('/v1/scenarios/gamma')
                page.locator(f'#scenes [data-scene="{alpha_id}"]').click()
                page.locator('.scene-more summary').click()
                page.locator('.scene-more [data-action=delete-scene]').click()
                expect(page.locator('#dialog-fields')).to_contain_text('未保存的草稿')
                page.locator('#dialog-submit').click()
                expect(page.locator('#dialog')).not_to_be_visible()
                expect(page.locator('#scenes [data-scene]')).to_have_count(0)
                expect(page.locator('#initial-loading')).not_to_be_visible()
                assert not api('/v1/scenarios').get('scenarios')
                assert not api('/v1/jobs').get('jobs')
                assert not errors, errors
                browser.close()
                print(json.dumps({'passed': True, 'screenshots': str(folder), 'checks': [
                    'unopened summaries', 'updated counts', 'result status', 'confirmation cancellation',
                    'stale-version refusal', 'cascade deletion', 'other draft retained',
                    'mobile summaries', 'global deletion', 'last scene deletion', 'no browser errors',
                ]}, ensure_ascii=False))
        finally:
            process.terminate()
            process.wait(timeout=15)


if __name__ == '__main__':
    main()
