"""Read the private provenance index; verify original bytes and candidate-pair mutations."""
import argparse
from collections import Counter
import json
from pathlib import Path
import subprocess
import sys
from generate import digest,write_json
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'scripts'))
from organize_corpus import plan


def audit(corpus,output,vrp=None):
    corpus=Path(corpus).resolve(); output=Path(output).resolve(); output.mkdir(parents=True,exist_ok=True)
    index=json.loads((corpus/'index.json').read_text(encoding='utf-8-sig'))
    originals=[]; sources=[]; pairs=[]; counts=Counter()
    for source in index['sources']:
        root=Path(source['path'])
        sources.append({**source,'exists':root.exists(),'actual_head':subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip() if root.exists() else None})
    for entry in index['files']:
        path=corpus/entry['path']; present=path.exists()
        same=present and digest(path.read_bytes())==entry['sha256']; counts['copied_sha_ok' if same else 'copied_sha_failed']+=1
        origins=[]
        for origin in entry['origins']:
            original=Path(origin['root'])/origin['relative']
            origins.append({'path':str(original),'exists':original.exists(),'sha_equal':original.exists() and digest(original.read_bytes())==entry['sha256']})
        originals.append({'path':entry['path'],'sha_ok':same,'origins':origins})
    def by_id(items): return {str(x.get('id')):x for x in items if isinstance(x,dict)}
    for case in index['cases']:
        request=corpus/case['request']; left=plan(json.loads(request.read_text(encoding='utf-8-sig')))
        for candidate in case['result_candidates']:
            path=corpus/candidate['path']; right=plan(json.loads(path.read_text(encoding='utf-8-sig')))
            differences={}
            for entity,fields in [('tickets',['min_start_time','max_end_time','duration']),('agents',['shift_start_time','shift_off_time'])]:
                a,b=by_id(left[entity]),by_id(right[entity])
                for field in fields:
                    differences[entity+'.'+field]=sum(a[k].get(field)!=b[k].get(field) for k in a.keys()&b.keys())
            differences['matrix_changed']=digest(left.get('matrix'))!=digest(right.get('matrix'))
            differences['weights_changed']=digest(left.get('constraint_configuration'))!=digest(right.get('constraint_configuration'))
            item={'case':case['id'],'request':case['request'],'result':candidate['path'],'basis':candidate['basis'],'differences':differences,
                  'interpretation':'candidate history only; request supplies the problem, result supplies assignments/explicit matrix; no same-budget claim'}
            if vrp and case['within_target']:
                folder=output/case['id']/str(len(pairs)); folder.mkdir(parents=True,exist_ok=True)
                with (folder/'stdout.log').open('wb') as out,(folder/'stderr.log').open('wb') as err:
                    command=[str(Path(vrp).resolve()),'eval',str(request),'--solution',str(path),'--matrix',str(path)]
                    write_json(folder/'command.json',command)
                    try:
                        r=subprocess.run(command,stdout=out,stderr=err,timeout=120)
                        item['replay_status']='passed' if r.returncode==0 else 'failed'
                    except subprocess.TimeoutExpired: item['replay_status']='timeout'
                counts['replay_'+item['replay_status']]+=1
            pairs.append(item)
    counts.update({'requests':len(index['cases']),'paired_requests':sum(bool(c['result_candidates']) for c in index['cases']),'candidate_pairs':len(pairs)})
    counts['pairs_with_mutations']=sum(any(p['differences'].values()) for p in pairs)
    result={'sources':sources,'counts':dict(counts),'originals':originals,'pairs':pairs}
    write_json(output/'audit.json',result)
    print(json.dumps({'private_corpus_audit':dict(counts)}),flush=True)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--corpus',default='corpus');p.add_argument('--output',required=True);p.add_argument('--vrp')
    a=p.parse_args();audit(a.corpus,a.output,a.vrp)
