"""Correct legacy dependency semantics through new native scenario versions.

Dry-run by default. Original corpus bytes, display data, coordinates, matrices,
execution history and frozen jobs are preserved. Reports/exports stay private.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.request import Request, build_opener, ProxyHandler


def pairs(problem, key):
    values = {(int(p.get('first', 0)), int(p.get('second', 0))) for p in problem.get(key, [])}
    return {tuple(sorted(p)) for p in values} if key == 'relatedPairs' else values


def find_plan(value):
    if not isinstance(value, dict): return None
    if isinstance(value.get('tickets'), list) and isinstance(value.get('agents'), list): return value
    for key in ['plan', 'scenario', 'request']:
        found = find_plan(value.get(key))
        if found is not None: return found
    return None


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', required=True)
    parser.add_argument('--corpus', type=Path, default=root/'corpus')
    parser.add_argument('--vrp', type=Path, default=root/'target/debug'/('vrp.exe' if os.name == 'nt' else 'vrp'))
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    output = args.corpus/f'dependency-migration-{time.time_ns()}'
    output.mkdir(parents=True)
    client = build_opener(ProxyHandler({}))
    def api(path, body=None):
        headers = {'Accept':'application/json', 'Content-Type':'application/json'}
        if os.environ.get('VRP_API_TOKEN'): headers['Authorization']='Bearer '+os.environ['VRP_API_TOKEN']
        req = Request(args.server.rstrip('/')+path, data=None if body is None else json.dumps(body,ensure_ascii=False).encode(), headers=headers)
        with client.open(req, timeout=120) as response: return json.load(response)
    index = json.loads((args.corpus/'index.json').read_text(encoding='utf-8-sig'))
    originals = {v['sha256']:v for v in index['files']}
    records = []
    report = output/'report.json'
    def write_report():
        report.write_text(json.dumps({'apply':args.apply,'records':records},ensure_ascii=False,indent=2),encoding='utf-8')
    page = api('/v1/scenarios?page_size=100')
    scenes = page['scenarios']
    while page.get('nextAfterId'):
        page = api('/v1/scenarios?page_size=100&after_id='+page['nextAfterId'])
        scenes.extend(page.get('scenarios', []))
    for summary in scenes:
        if not summary['id'].startswith('legacy-'): continue
        record = {'scenarioId':summary['id'], 'name':summary['name']}
        records.append(record)
        source = originals.get(summary.get('origin',{}).get('sourceSha256'))
        if not source:
            record['status']='skipped_missing_original'; continue
        file = args.corpus/source['path']
        data = file.read_bytes()
        if hashlib.sha256(data).hexdigest() != source['sha256']:
            raise ValueError('Original content hash changed: '+str(file))
        plan = find_plan(json.loads(data))
        if plan is None: raise ValueError('Original has no plan: '+str(file))
        if not any(t.get('ref_tickets') or t.get('dep_tickets') for t in plan['tickets'] if isinstance(t,dict)):
            record['status']='no_dependencies'; continue
        native = output/(summary['id']+'.json')
        result = subprocess.run([str(args.vrp.resolve()),'export-scene',str(file.resolve()),str(native.resolve()),
            '--id',summary['id'],'--source-name',source['path'],'--sha256',source['sha256']],capture_output=True,check=True)
        (output/(summary['id']+'.log')).write_bytes(result.stdout+result.stderr)
        converted = json.loads(native.read_text(encoding='utf-8-sig'))['request']['problem']
        current = api('/v1/scenarios/'+summary['id'])
        request = copy.deepcopy(current['request']); problem = request['problem']
        positions = {t['id']:i for i,t in enumerate(problem['tickets'])}
        remap = [positions.get(t['id'],positions.get(f'T{i+1:03}')) for i,t in enumerate(converted['tickets'])]
        if any(i is None for i in remap) or len(set(remap)) != len(remap):
            record['status']='skipped_edited_ticket_identities'; continue
        legacy_ids = {t['id']:remap[i] for i,t in enumerate(converted['tickets'])}
        def ident(v): return v.get('id') if isinstance(v,dict) else v
        old = {'relatedPairs':set(), 'precedences':set()}
        for ticket in plan['tickets']:
            if not isinstance(ticket,dict) or ticket.get('id') not in legacy_ids: continue
            t = legacy_ids[ticket['id']]
            for key, target in [('dep_tickets','precedences'),('ref_tickets','relatedPairs')]:
                for ref in ticket.get(key,[]):
                    other = legacy_ids.get(ident(ref))
                    if other is not None and other != t:
                        old[target].add((other,t) if target=='precedences' else tuple(sorted((t,other))))
        desired = {key:{(remap[a],remap[b]) for a,b in pairs(converted,key)} for key in old}
        desired['relatedPairs'] = {tuple(sorted(p)) for p in desired['relatedPairs']}
        existing = {key:pairs(problem,key) for key in old}
        needs_weight = bool(desired['precedences']) and not any(int(v) for v in problem.get('weights',{}).get('refTicketAfterDepTicket',{}).values())
        if existing==desired and not needs_weight:
            record['status']='already_correct'; continue
        if existing != old:
            record['status']='skipped_edited_relations'; continue
        for key, values in desired.items():
            problem[key] = [{'first':a,'second':b} for a,b in sorted(values)]
        if needs_weight: problem.setdefault('weights',{})['refTicketAfterDepTicket']={'hard':'1'}
        record.update(status='planned', expectedVersion=current['scenario']['version'],
                      before={key:len(value) for key,value in old.items()},after={key:len(value) for key,value in desired.items()}, enabledTiming=needs_weight)
        if args.apply:
            write_report()
            metadata = api('/v1/scenarios',{'scenarioId':summary['id'], 'name':current['scenario']['name'],
                'expectedVersion':current['scenario']['version'],'request':request})
            record.update(status='updated', version=metadata['version'])
            write_report()
            saved = api('/v1/scenarios/'+summary['id']+'?version='+metadata['version'])
            assert saved['scenario'].get('display')==current['scenario'].get('display')
            assert saved['scenario'].get('origin')==current['scenario'].get('origin')
            for key in ['locations','matrices','tickets','agents']:
                assert saved['request']['problem'].get(key)==current['request']['problem'].get(key),key
    write_report()
    counts = {status:sum(r['status']==status for r in records) for status in sorted({r['status'] for r in records})}
    print(json.dumps(counts),report)


if __name__=='__main__': main()
