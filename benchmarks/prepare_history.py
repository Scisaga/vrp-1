"""Prepare a private native-semantics migration campaign from an audited request pair."""
import argparse
import json
from pathlib import Path
import subprocess
from generate import digest,write_json


def prepare(corpus,case_id,adapter,output):
    corpus=Path(corpus).resolve(); output=Path(output).resolve(); output.mkdir(parents=True,exist_ok=True)
    if (output/'request.json').exists(): raise ValueError('use a new historical input directory')
    index=json.loads((corpus/'index.json').read_text(encoding='utf-8-sig'))
    case=next(c for c in index['cases'] if c['id']==case_id)
    if not case['within_target'] or not case['result_candidates']: raise ValueError('an in-scope audited request with an explicit pair is required')
    request=corpus/case['request']; candidate=case['result_candidates'][0]; result=corpus/candidate['path']
    for path,role in [(request,'request'),(result,'result')]:
        entry=next(f for f in index['files'] if f['path']==path.relative_to(corpus).as_posix())
        if entry['kind']!=role or digest(path.read_bytes())!=entry['sha256']: raise ValueError('role or content fingerprint does not match index')
    frozen=output/'request.json'
    command=[str(Path(adapter).resolve()),'freeze-v1',str(request),str(frozen),str(result)]
    write_json(output/'conversion-command.json',command)
    with (output/'conversion.stdout').open('wb') as out,(output/'conversion.stderr').open('wb') as err:
        run=subprocess.run(command,stdout=out,stderr=err)
    if run.returncode: raise ValueError('historical conversion failed; see private conversion.stderr')
    conversion=json.loads((output/'request.conversion.json').read_text(encoding='utf-8'))
    native=json.loads(frozen.read_text(encoding='utf-8'))
    provenance={'request_sha256':digest(request.read_bytes()),'matrix_result_sha256':digest(result.read_bytes()),
        'request_source':case['source'],'source_repositories':index['sources'],'pairing_basis':candidate['basis'],'index_sha256':digest((corpus/'index.json').read_bytes()),
        'input_role':'original request; historical result contributes matrix edges only',
        'conversion':conversion,'historical_scores':'reference only; never used as this campaign result'}
    write_json(output/'provenance.json',provenance)
    config={'format_version':1,'generator_seed':7412026,'seeds':[11,29,47,71,101],
        'budget_ms':30000,'threads':1,'memory_mib':2048,'warmup_rounds':3,'warmup_ms':20000,
        'scenarios':[{'id':'b07-private-historical','private':True,'suite':'acceptance',
            'tickets':len(native['problem']['tickets']),'agents':len(native['problem']['agents']),
            'features':['historical_native_migration'],'modes':['request-initial'],
            'request_file':str(frozen),'source':provenance,
            'transformations':[{'operation':'v1_import_to_native_v2','paired_matrix_explicit':True,'warnings':conversion['warnings']}]}]}
    write_json(output/'config.json',config)
    print(json.dumps({'prepared_private_history':True,'tickets':config['scenarios'][0]['tickets'],
                      'agents':config['scenarios'][0]['agents'],'conversion_warnings':len(conversion['warnings'])}),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--corpus',default='corpus');p.add_argument('--case-id',required=True)
    p.add_argument('--adapter',required=True);p.add_argument('--output',required=True)
    a=p.parse_args();prepare(a.corpus,a.case_id,a.adapter,a.output)
