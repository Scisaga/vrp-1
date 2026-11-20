"""Fail-closed score/mapping gate, executed before common-profile solver measurements."""
import argparse
import json
from pathlib import Path
import subprocess
from generate import tiny_cases, write_json, digest
from validate import validate, score_of


def gate(adapter,legacy,output):
    output=Path(output).resolve(); output.mkdir(parents=True,exist_ok=True)
    commands=[]; cases=tiny_cases()
    for i,case in enumerate(cases):
        path=output/f'{i:03}.json'; write_json(path,case)
        commands.append({'input':str(path),'output':str(output/f'{i:03}.java.json')})
    write_json(output/'commands.json',commands)
    cp=json.loads(Path(legacy).read_text())['classpath']
    subprocess.run(['java','-XX:ActiveProcessorCount=1','-XX:+UseSerialGC','-Xmx1536m','-cp',cp,'LegacyAdapter','eval-many',str(output/'commands.json')],check=True,stdout=subprocess.DEVNULL)
    checks=[]
    for i,(case,command) in enumerate(zip(cases,commands)):
        out=output/f'{i:03}.rust.json'
        subprocess.run([str(adapter),'eval',command['input'],str(out)],check=True,stdout=subprocess.DEVNULL)
        java=json.loads(Path(command['output']).read_text()); rust=json.loads(out.read_text())
        independent=validate(case,java); native=validate(case,rust)
        if not independent['valid'] or not native['valid'] or score_of(java)!=score_of(rust):
            raise AssertionError((i,independent,native,score_of(java),score_of(rust)))
        checks.append({'case_sha256':digest(case),'score':score_of(java)})
    result={'status':'passed','cases':len(checks),'checks':checks,'profile':'common-minute-grid-v1',
            'distinct_input_mappings':len({c['case_sha256'] for c in checks}),
            'adapter_sha256':digest(Path(adapter).read_bytes()),'legacy_build_sha256':digest(Path(legacy).read_bytes()),
            'criterion':'exact triple equality: legacy search score, v2 oracle, independent business score'}
    write_json(output/'gate.json',result)
    print(json.dumps({'gate':'passed','cases':len(checks)}),flush=True)
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(); p.add_argument('--adapter',required=True); p.add_argument('--legacy-build',required=True); p.add_argument('--output',required=True)
    a=p.parse_args(); gate(Path(a.adapter).resolve(),a.legacy_build,a.output)
