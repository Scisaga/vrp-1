"""Verify completeness, frozen inputs and actual incumbent/resource evidence."""
import argparse
from collections import Counter
from pathlib import Path
from generate import digest,write_json
from run import read,previous
from validate import score_of


def verify(root):
    root=Path(root).resolve(); plan=read(root/'plan.json'); env=read(root/'environment.json')
    issues=[]; counts=Counter(); timed=0; overruns=[]
    if digest(env)!=plan['environment_sha256']: issues.append('environment fingerprint mismatch')
    if digest((root/'source.zip').read_bytes())!=env['source_archive_sha256']: issues.append('source snapshot fingerprint mismatch')
    if digest(Path(env['adapter']).read_bytes())!=env['adapter_sha256']: issues.append('executed binary fingerprint mismatch')
    if len(set(p['id'] for p in plan['runs']))!=len(plan['runs']): issues.append('duplicate planned run IDs')
    for item in plan['inputs']:
        request=read(item['input'])
        if digest(Path(item['input']).read_bytes())!=item['input_sha256']: issues.append('input fingerprint mismatch')
        if digest(request['problem']['matrices'])!=item['matrix_sha256']: issues.append('matrix fingerprint mismatch')
    for p in plan['runs']:
        r=previous(root,p)
        if not r: issues.append('missing terminal record: '+p['id']); continue
        counts[r['status']]+=1
        if r['plan']['id']!=p['id'] or r['plan']['input_sha256']!=p['input_sha256']: issues.append('run provenance mismatch')
        if r['status']=='unsupported': continue
        timed+=1; trace=r.get('quality_trace',[])
        for a,b in zip(trace,trace[1:]):
            if a['elapsed_ms']>b['elapsed_ms'] or tuple(a['score'])>tuple(b['score']):
                issues.append('nonmonotone incumbent history: '+p['id']); break
        if r['status'] in ('succeeded','business_standard_not_met'):
            if not r.get('validation',{}).get('valid'): issues.append('successful status without valid independent check')
            folder=sorted((root/'runs'/p['id']).glob('attempt-*/record.json'))[-1].parent
            result=read(folder/'result.json')
            if trace and trace[-1]['score']!=score_of(result): issues.append('final result differs from last actual incumbent')
            if digest((folder/'result.json').read_bytes())!=r.get('result_sha256'): issues.append('result fingerprint mismatch')
            if not r.get('cpu_seconds',0)>0: issues.append('missing CPU accounting')
            if not r.get('peak_tree_commit_bytes',0): issues.append('missing Windows process-tree high-water memory')
            elapsed=r.get('solve_ms',0)
            if elapsed<env['config']['budget_ms']-100: issues.append('short run substituted for requested solve budget')
            if trace and trace[-1]['elapsed_ms']>elapsed+5: issues.append('incumbent timestamp exceeds actual solve duration')
            overruns.append(r.get('budget_overrun_ms',0))
    result={'verified':not issues,'expected_records':len(plan['runs']),'terminal_records':sum(counts.values()),
            'timed_attempts':timed,'statuses':dict(counts),'issues':issues,
            'max_observed_search_overrun_ms':max(overruns,default=0),
            'notes':'Failures are valid outcomes; verification checks that evidence is complete, never silently removes them.'}
    write_json(root/'verification.json',result)
    print({k:v for k,v in result.items() if k!='notes'},flush=True)
    if issues: raise SystemExit(1)
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('campaign');a=p.parse_args();verify(a.campaign)
