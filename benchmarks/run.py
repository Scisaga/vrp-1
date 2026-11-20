"""Reproducible serial solve evaluation. Developer manifests are not service contracts."""
import argparse
import copy
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time
import zipfile
import psutil
from generate import generate, canonical, digest, write_json, WEIGHTS
from validate import validate, score_of
from resources import ManagedProcess, run_process

ROOT=Path(__file__).resolve().parents[1]
JAVA_FLAGS=['-XX:ActiveProcessorCount=1','-XX:+UseSerialGC','-Xms128m','-Xmx1536m','-Dfile.encoding=UTF-8']


def read(path): return json.loads(Path(path).read_text(encoding='utf-8'))
def command_version(command):
    return subprocess.run(command,capture_output=True,text=True,encoding='utf-8',errors='replace').stdout.strip() or subprocess.run(command,capture_output=True,text=True,encoding='utf-8',errors='replace').stderr.strip()


def common_profile(request):
    p=request['problem']; allowed={'agentCapacity','agentMaxTicket','agentSkillsAccordWithTicketSkills',
        'agentQualificationLevelsMatchTicket','ticketStartServiceTimeMatchExpected','sameDepo','ticketUnassigned','minimizeTravelTime'}
    if p.get('execution') or p.get('precedences') or len(p['matrices'])!=1 or len(p['capacityDimensions'])!=1: raise ValueError('not a shared profile')
    if int(p['capacityDimensions'][0].get('penaltyUnit',1)) != 1: raise ValueError('capacity penalty unit')
    for k,w in p['weights'].items():
        if k not in allowed and any(int(v) for v in w.values()): raise ValueError('unverified objective '+k)
    for a in p['agents']:
        if not a.get('skills') or a.get('pinned') or a.get('startLocation',0)!=a.get('endLocation',0) or a.get('matrix',0): raise ValueError('unverified agent semantics')
        if any(a.get('capacityTolerancePermille',[])) or int(a.get('startTime',0))%60: raise ValueError('unverified capacity/time rounding')
    for t in p['tickets']:
        if t.get('type') not in (0,1,'TICKET_TYPE_SERVICE','TICKET_TYPE_DELIVERY') or t.get('pinned'): raise ValueError('unverified ticket semantics')
        if any(int(t.get(k,0))%60 for k in ('duration','earliestStart','latestStart','latestEnd')): raise ValueError('time rounding')
    import base64,struct
    data=base64.b64decode(p['matrices'][0]['durations'])
    if any(t>=0xfffffffe or t%60 for (t,) in struct.iter_unpack('<I',data)): raise ValueError('unfrozen/non-minute matrix')


def prepare(config,output,adapter,legacy):
    config=copy.deepcopy(config)
    if config['budget_ms']<=0 or config['memory_mib']<=0 or not 1<=config['threads']<=len(psutil.Process().cpu_affinity()):
        raise ValueError('positive time/memory budgets and an available CPU for each thread are required')
    if not config['seeds'] or len(set(config['seeds']))!=len(config['seeds']) or any(not 0<=s<2**63 for s in config['seeds']):
        raise ValueError('unique nonnegative signed-64-bit seeds are required for both runtimes')
    ids=[s['id'] for s in config['scenarios']]
    if not ids or len(ids)!=len(set(ids)): raise ValueError('scenario IDs must be nonempty and unique')
    for spec in config['scenarios']:
        if spec.get('request_file'): spec['private']=True
    output=Path(output).resolve(); adapter=Path(adapter).resolve()
    if (output/'plan.json').exists(): raise ValueError('prepare requires a fresh output directory')
    output.mkdir(parents=True,exist_ok=True)
    if legacy:
        from gate import gate
        gate(adapter,legacy,output/'score-gate')
    # A campaign owns the executed bytes, independent of subsequent builds.
    artifacts=output/'adapters'; artifacts.mkdir()
    pinned_adapter=artifacts/adapter.name; shutil.copy2(adapter,pinned_adapter); adapter=pinned_adapter
    legacy_meta=read(legacy) if legacy else None
    if legacy_meta:
        classroot=Path(legacy_meta['classpath'].split(os.pathsep)[0]); dest=artifacts/'java'; dest.mkdir()
        for rel in legacy_meta['classes']:
            target=dest/rel; target.parent.mkdir(parents=True,exist_ok=True); shutil.copy2(classroot/rel,target)
        jars=artifacts/'jars'; jars.mkdir(); paths=[str(dest)]
        for jar in legacy_meta['classpath'].split(os.pathsep)[1:]:
            target=jars/Path(jar).name; shutil.copy2(jar,target); paths.append(str(target))
        legacy_meta['classpath']=os.pathsep.join(paths)
    tool_files=sorted((ROOT/'benchmarks').rglob('*.py'))+sorted((ROOT/'benchmarks/java').glob('*.java'))
    files={str(p.relative_to(ROOT)):digest(p.read_bytes()) for p in tool_files}
    source_files=set(subprocess.check_output(['git','ls-files'],cwd=ROOT,text=True).splitlines())
    source_files.update(files)
    source_files.update(str(p.relative_to(ROOT)).replace('\\','/') for p in (ROOT/'crates/vrp-cli/src/bin').glob('*.rs'))
    with zipfile.ZipFile(output/'source.zip','w',zipfile.ZIP_DEFLATED) as z:
        z.writestr('evaluation-config.json',canonical(config))
        for rel in sorted(source_files):
            path=ROOT/rel
            if path.suffix in ('.rs','.proto','.toml','.lock','.py','.java') and path.is_file(): z.write(path,rel)
    cpu_name=os.environ.get('PROCESSOR_IDENTIFIER',platform.processor())
    if os.name=='nt':
        import winreg
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,r'HARDWARE\DESCRIPTION\System\CentralProcessor\0') as key:
            cpu_name=winreg.QueryValueEx(key,'ProcessorNameString')[0].strip()
    meta={'created_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
          'vrp1_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
          'vrp1_status':subprocess.check_output(['git','status','--porcelain'],cwd=ROOT,text=True).strip(),
          'source_archive_sha256':digest((output/'source.zip').read_bytes()),'tools':files,
          'rust':command_version(['rustc','-Vv']),'java':command_version(['java','-version']),
          'python':platform.python_version(),'psutil':psutil.__version__,
          'hardware':{'os':platform.platform(),'cpu':cpu_name,
                      'logical_cpus':psutil.cpu_count(),'physical_cpus':psutil.cpu_count(logical=False),
                      'ram_bytes':psutil.virtual_memory().total,'affinity':psutil.Process().cpu_affinity()[:config['threads']]},
          'adapter':str(adapter),'adapter_sha256':digest(adapter.read_bytes()),
          'build_command':'cargo build --release -p vrp-cli --bin vrp-bench-adapter',
          'java_flags':[f'-XX:ActiveProcessorCount={config["threads"]}',*JAVA_FLAGS[1:]],'legacy':legacy_meta,
          'config':config,'config_sha256':digest(config)}
    write_json(output/'environment.json',meta)
    inputs=[]
    for spec in config['scenarios']:
        if spec.get('request_file'):
            req=read(spec['request_file']); initial=req.get('initialSolution',{'routes':[]}); transforms=spec.get('transformations',[])
        else: req,initial,transforms=generate(spec,config['generator_seed'])
        for mode in spec['modes']:
            r=copy.deepcopy(req)
            if mode!='cold': r['initialSolution']=initial
            folder=output/'inputs'/spec['id']/mode; folder.mkdir(parents=True)
            write_json(folder/'generated.json',r)
            subprocess.run([str(adapter),'freeze',str(folder/'generated.json'),str(folder/'request.json')],check=True,capture_output=True)
            frozen=read(folder/'request.json')
            if spec['suite']=='common': common_profile(frozen)
            # Before timing, check the common initial plan with all three scorers.
            # Cold and continuation share the same problem, but preserve separate input hashes.
            check=copy.deepcopy(frozen); check['initialSolution']=initial
            write_json(folder/'initial.json',check)
            subprocess.run([str(adapter),'eval',str(folder/'initial.json'),str(folder/'initial.result.json')],check=True,capture_output=True)
            validation=validate(check,read(folder/'initial.result.json'))
            if not validation['valid']: raise ValueError((spec['id'],mode,validation))
            if spec['suite']=='common' or spec['id'].startswith(('b01','b02')):
                if not validation['usable']: raise ValueError('declared feasible witness does not meet standard: '+spec['id'])
            item={'scenario':spec,'mode':mode,'input':str(folder/'request.json'),
                  'input_sha256':digest((folder/'request.json').read_bytes()),'problem_sha256':digest(frozen['problem']),
                  'matrix_sha256':digest(frozen['problem']['matrices']),
                  'initial_sha256':digest(frozen.get('initialSolution')),'initial_validation':validation,
                  'transforms':transforms,'source':spec.get('source','public deterministic synthetic generator')}
            write_json(folder/'provenance.json',item); inputs.append(item)
    # Large, real input-mapping checks as well as the small score gate.
    if legacy:
        commands=[]
        for item in inputs:
            if item['scenario']['suite']=='common':
                folder=Path(item['input']).parent
                commands.append({'input':str(folder/'initial.json'),'output':str(folder/'initial.java.json')})
        write_json(output/'mapping-checks.json',commands)
        subprocess.run(['java',*JAVA_FLAGS,'-cp',meta['legacy']['classpath'],'LegacyAdapter','eval-many',str(output/'mapping-checks.json')],check=True,stdout=subprocess.DEVNULL)
        for command in commands:
            native=read(Path(command['input']).parent/'initial.result.json'); old=read(command['output'])
            if score_of(native)!=score_of(old) or not validate(read(command['input']),old)['valid']: raise ValueError('large-input mapping/score mismatch')
    plans=[]
    for item in inputs:
        for engine in ('vrp0','vrp1'):
            for seed in config['seeds']:
                identity={'scenario':item['scenario']['id'],'mode':item['mode'],'engine':engine,'seed':seed,
                          'input_sha256':item['input_sha256'],'config_sha256':meta['config_sha256'],
                          'binary':meta['adapter_sha256'] if engine=='vrp1' else digest(meta['legacy']) if legacy else None}
                identity['id']=f"{identity['scenario']}-{item['mode']}-{engine}-s{seed}-{digest(identity)[:10]}"
                plans.append({**identity,'input':item['input'],'suite':item['scenario']['suite'],'scenario_spec':item['scenario']})
    write_json(output/'plan.json',{'inputs':inputs,'runs':plans,'environment_sha256':digest(meta)})
    print(json.dumps({'prepared_inputs':len(inputs),'planned_records':len(plans),
                      'timed_runs':sum(p['suite']=='common' or p['engine']=='vrp1' for p in plans)}),flush=True)


def attempts(root,plan): return sorted((root/'runs'/plan['id']).glob('attempt-*/record.json'))
def previous(root,plan):
    all_attempts=attempts(root,plan)
    return read(all_attempts[-1]) if all_attempts else None
def new_attempt(root,plan):
    base=root/'runs'/plan['id']; base.mkdir(parents=True,exist_ok=True)
    folders=sorted(base.glob('attempt-*')); target=base/f'attempt-{len(folders)+1:03}'
    target.mkdir(); write_json(target/'started.json',{'plan':plan,'utc':time.time()})
    # An incomplete prior attempt is evidence of interruption, not a successful run.
    for old in folders:
        if not (old/'record.json').exists(): write_json(old/'record.json',{'plan':plan,'status':'interrupted','diagnostic':'no terminal record; resumed in a new attempt'})
    return target


def read_events(folder):
    raw=folder/'events.jsonl'
    events=[]
    if raw.exists():
        for line in raw.read_text(encoding='utf-8',errors='replace').splitlines():
            try: events.append(json.loads(line))
            except json.JSONDecodeError: continue
    return events


def finalize(root,plan,folder,resource):
    try: return finalize_result(root,plan,folder,resource)
    except Exception as error:
        events=read_events(folder)
        record={'plan':plan,**resource,'events':events,'attempt':folder.name,
                'quality_trace':[e for e in events if isinstance(e,dict) and e.get('event')=='incumbent'],
                'first_full_feasible_ms':None,'first_full_feasible_censored':True,
                'status':'validation_failed' if resource['status']=='succeeded' else resource['status'],
                'error_type':type(error).__name__,'diagnostic':str(error)}
        write_json(folder/'record.json',record)
        return record


def finalize_result(root,plan,folder,resource):
    output=folder/'result.json'; events=read_events(folder)
    record={'plan':plan,**resource,'events':events,'attempt':folder.name,
            'quality_trace':[e for e in events if isinstance(e,dict) and e.get('event')=='incumbent'],
            'first_full_feasible_ms':None,'first_full_feasible_censored':True}
    if resource['status']=='succeeded' and output.exists():
        request=read(plan['input']); result=read(output)
        # Independently evaluate every returned solution on the frozen native problem.
        rescore=copy.deepcopy(request); rescore['initialSolution']=result.get('solution',{})
        write_json(folder/'rescore-request.json',rescore)
        environment=read(root/'environment.json')
        check=subprocess.run([environment['adapter'],'eval',str(folder/'rescore-request.json'),str(folder/'oracle.json')],capture_output=True,text=True)
        independent=validate(request,result)
        record['validation']=independent
        if result.get('legacyInitScore',0)<0:
            record['status']='no_solution'; record['diagnostic']='legacy construction ended with uninitialized planning values'; record['legacy_init_score']=result['legacyInitScore']
        elif check.returncode or score_of(read(folder/'oracle.json'))!=score_of(result) or not independent['valid']:
            record['status']='validation_failed'; record['oracle_error']=check.stderr[:4000]
        stats=result.get('stats',{})
        finished=next((e for e in reversed(events) if e.get('event')=='finished'),{})
        elapsed=float(finished.get('search_elapsed_ms',stats.get('elapsedMs',finished.get('solve_ms',0))))
        record['solve_ms']=elapsed; record['budget_overrun_ms']=max(0,elapsed-read(root/'environment.json')['config']['budget_ms'])
        started_event=next((e for e in events if e.get('event')=='process_started'),{})
        if started_event and 'process_spawn_unix_ms' in record:
            record['process_startup_ms']=started_event['unix_ms']-record['process_spawn_unix_ms']
        if plan['engine']=='vrp0' and 'process_cpu_seconds' in finished:
            record['cpu_seconds']=finished['process_cpu_seconds']; record['cpu_method']='JVM OperatingSystemMXBean user+kernel, command interval, no child processes'
        total=len(request['problem']['tickets'])
        usable=[e['elapsed_ms'] for e in record['quality_trace'] if e['score'][0]==0 and e.get('assigned')==total]
        record['first_full_feasible_ms']=min(usable) if usable else None
        record['first_full_feasible_censored']=not bool(usable)
        record['result_sha256']=digest(output.read_bytes())
        spec=plan['scenario_spec']
        if plan['suite']=='acceptance':
            amounts=independent['amounts']
            ok=independent['valid'] and independent['score'][0]==0
            if 'fault' in spec['features']:
                blocked=sum(bool(t.get('blocked')) for t in request['problem']['execution']['tickets'])
                ok=ok and independent['unassigned']==blocked
            elif spec.get('known_infeasible'): ok=ok and independent['unassigned']>=spec['tickets']-4
            else: ok=ok and independent['unassigned']==0
            record['acceptance_passed']=ok
            if not ok and record['status']=='succeeded': record['status']='business_standard_not_met'
    elif resource['status']=='succeeded': record['status']='no_solution'
    write_json(folder/'record.json',record)
    return record


def native_run(root,plan):
    meta=read(root/'environment.json'); cfg=meta['config']; folder=new_attempt(root,plan)
    command=[meta['adapter'],'solve',plan['input'],str(folder/'result.json'),str(cfg['budget_ms']),str(plan['seed']),str(cfg['threads']),'0',str(root/'CANCEL')]
    write_json(folder/'command.json',command)
    try:
        metrics=run_process(command,folder/'events.jsonl',folder/'stderr.log',cfg['budget_ms']/1000+30,
                            cfg['memory_mib'],root/'CANCEL',cfg.get('cpu_budget_seconds'),cfg['threads'])
    except NotImplementedError as error:
        metrics={'status':'unsupported','error_type':type(error).__name__,'diagnostic':str(error)}
    except Exception as error:
        metrics={'status':'error','error_type':type(error).__name__,'diagnostic':str(error)}
    result=finalize(root,plan,folder,metrics)
    print(json.dumps({'completed':plan['id'],'status':result['status'],'score':result.get('validation',{}).get('score')}),flush=True)


def java_group(root,plans):
    meta=read(root/'environment.json'); cfg=meta['config']
    folders={p['id']:new_attempt(root,p) for p in plans}
    group=root/'batches'/digest([[p['id'],folders[p['id']].name] for p in plans])[:16]
    group.mkdir(parents=True,exist_ok=True)
    runs=[{'id':p['id'],'input':p['input'],'output':str(folders[p['id']]/'result.json'),
           'events':str(folders[p['id']]/'events.jsonl'),'budget_ms':cfg['budget_ms'],'seed':p['seed'],'threads':cfg['threads']} for p in plans]
    write_json(group/'batch.json',{'runs':runs,'warmup_rounds':cfg['warmup_rounds'],'warmup_ms':cfg['warmup_ms']})
    command=['java',*meta['java_flags'],'-cp',meta['legacy']['classpath'],'LegacyAdapter','batch',str(group/'batch.json')]
    write_json(group/'command.json',command)
    current=None; state={}; warmed=None; ended=set(); start=time.perf_counter(); pending_text=''
    with open(group/'stdout.jsonl','wb') as stdout,open(group/'stderr.log','wb') as stderr,open(group/'stdout.jsonl','r',encoding='utf-8',errors='replace') as stream:
        child=ManagedProcess(command,stdout,stderr,cfg['memory_mib'],cpu_count=cfg['threads'])
        failure=None
        try:
            while child.process.poll() is None:
                rss=child.sample()
                if current: state[current]['rss']=max(state[current]['rss'],rss)
                pending_text+=stream.read()
                lines=pending_text.split('\n'); pending_text=lines.pop()
                for line in lines:
                    try: event=json.loads(line)
                    except json.JSONDecodeError: continue
                    if event.get('event')=='warmed': warmed=event; write_json(group/'warmup.json',event)
                    if event.get('event')=='process_started': write_json(group/'startup.json',{'startup_ms':event['unix_ms']-child.started_unix_ms})
                    if event.get('event')=='run_start':
                        current=event['id']; state[current]={'started':time.perf_counter(),'rss':rss,'cpu_before':child.accounting()['cpu_seconds']}
                        print(json.dumps({'running':current,'prewarmed':cfg['warmup_rounds']>0}),flush=True)
                    if event.get('event')=='run_end':
                        run_id=event['id']; snapshot=state[run_id]; counters=child.accounting()
                        resource={'status':'succeeded','exit_code':0,'wall_ms':(time.perf_counter()-snapshot['started'])*1000,
                                  'peak_tree_rss_sampled_bytes':snapshot['rss'],'rss_sample_interval_ms':20,
                                  'cpu_seconds':counters['cpu_seconds']-snapshot['cpu_before'],'cpu_method':counters['cpu_method'],
                                  'peak_tree_commit_bytes':counters['peak_tree_commit_bytes'],
                                  'memory_scope':'JVM batch lifetime high-water commit including warmup; per-command sampled RSS retains warm heap',
                                  'batch':group.name,'warmup':warmed,'process_startup_and_warmup_ms':(warmed or {}).get('wall_ms')}
                        # Rescoring uses CPU after the timed solve, before the next group starts.
                        # Defer it until this JVM exits, avoiding interference with subsequent seeds.
                        write_json(folders[run_id]/'resources.json',resource); ended.add(run_id); current=None
                        print(json.dumps({'measured':run_id,'pending_validation':True}),flush=True)
                if (root/'CANCEL').exists(): failure='cancelled'; break
                deadline=(cfg['warmup_rounds']*cfg['warmup_ms']+len(plans)*cfg['budget_ms'])/1000+len(plans)*30+30
                if time.perf_counter()-start>deadline: failure='timeout'; break
                if current and cfg.get('cpu_budget_seconds') and child.accounting()['cpu_seconds']-state[current]['cpu_before']>cfg['cpu_budget_seconds']:
                    failure='cpu_limit'; break
                if current and time.perf_counter()-state[current]['started']>cfg['budget_ms']/1000+30:
                    failure='timeout'; break
                time.sleep(.02)
            if failure: child.terminate()
            else: child.process.wait()
            # Capture the final flushed end notification even after process exit.
            for line in (pending_text+stream.read()).splitlines():
                try: event=json.loads(line)
                except json.JSONDecodeError: continue
                if event.get('event')=='run_end':
                    run_id=event['id']; counters=child.accounting(); snap=state.get(run_id,{'started':start,'rss':child.peak_rss,'cpu_before':0})
                    write_json(folders[run_id]/'resources.json',{'status':'succeeded','exit_code':0,
                        'wall_ms':(time.perf_counter()-snap['started'])*1000,'peak_tree_rss_sampled_bytes':snap['rss'],
                        'peak_tree_commit_bytes':counters['peak_tree_commit_bytes'],'memory_scope':'JVM batch lifetime including warmup',
                        'warmup':warmed,'batch':group.name}); ended.add(run_id)
            write_json(group/'resources.json',{'exit_code':child.process.returncode,'wall_ms':(time.perf_counter()-start)*1000,**child.accounting()})
        finally: child.terminate(); child.close()
    for p in plans:
        folder=folders[p['id']]
        resource=read(folder/'resources.json') if p['id'] in ended else {'status':failure or 'error','diagnostic':'batch exited before terminal notification; see batch stderr','batch':group.name}
        record=finalize(root,p,folder,resource)
        print(json.dumps({'completed':p['id'],'status':record['status'],'score':record.get('validation',{}).get('score')}),flush=True)


def run(root,retry=False,engines=None):
    root=Path(root).resolve(); plan=read(root/'plan.json'); meta=read(root/'environment.json')
    if digest(Path(meta['adapter']).read_bytes())!=meta['adapter_sha256']: raise ValueError('adapter changed; prepare a fresh campaign')
    if meta['legacy']:
        classroot=Path(meta['legacy']['classpath'].split(os.pathsep)[0])
        for rel,sha in meta['legacy']['classes'].items():
            if digest((classroot/rel).read_bytes())!=sha: raise ValueError('legacy classes changed; prepare a fresh campaign')
        for path in meta['legacy']['classpath'].split(os.pathsep)[1:]:
            jar=Path(path)
            if digest(jar.read_bytes())!=meta['legacy']['jars'][jar.name]: raise ValueError('legacy dependency changed')
    for item in plan['inputs']:
        if digest(Path(item['input']).read_bytes())!=item['input_sha256']: raise ValueError('frozen input changed')
    available=[]
    for p in plan['runs']:
        if engines and p['engine'] not in engines: continue
        old=previous(root,p)
        if old and (not retry or old['status'] in ('succeeded','unsupported')): continue
        if p['engine']=='vrp0' and (p['suite']!='common' or not meta['legacy']):
            folder=new_attempt(root,p); write_json(folder/'record.json',{'plan':p,'status':'unsupported','diagnostic':'execution/semantic migration suite excluded from shared algorithm ranking' if meta['legacy'] else 'legacy build unavailable'})
            continue
        available.append(p)
    groups={}
    for p in available: groups.setdefault((p['scenario'],p['mode']),{}).setdefault(p['engine'],[]).append(p)
    interrupted=False
    try:
        for i,engines_group in enumerate(groups.values()):
            order=('vrp0','vrp1') if i%2==0 else ('vrp1','vrp0')
            for engine in order:
                if (root/'CANCEL').exists(): interrupted=True; break
                pending=engines_group.get(engine,[])
                if engine=='vrp0' and pending:
                    try: java_group(root,pending)
                    except Exception as error:
                        for p in pending:
                            folders=sorted((root/'runs'/p['id']).glob('attempt-*'))
                            if folders and (folders[-1]/'record.json').exists(): continue
                            folder=folders[-1] if folders else new_attempt(root,p)
                            write_json(folder/'record.json',{'plan':p,'status':'unsupported' if isinstance(error,NotImplementedError) else 'error','error_type':type(error).__name__,'diagnostic':str(error)})
                        print(json.dumps({'batch_error':type(error).__name__,'runs_retained':len(pending)}),flush=True)
                else:
                    for p in pending:
                        if (root/'CANCEL').exists(): interrupted=True; break
                        native_run(root,p)
                progress(root)
            if interrupted: break
    except KeyboardInterrupt:
        (root/'CANCEL').touch(); interrupted=True
    progress(root)
    if interrupted: print('cancelled; completed records retained; remove CANCEL and rerun to resume',flush=True)


def progress(root):
    plan=read(root/'plan.json'); counts={}; done=0
    for p in plan['runs']:
        rec=previous(root,p)
        if rec: counts[rec['status']]=counts.get(rec['status'],0)+1; done+=1
    value={'completed':done,'expected':len(plan['runs']),'statuses':counts,'updated_utc':time.time()}
    write_json(root/'progress.json',value); print(json.dumps(value),flush=True)


def main():
    p=argparse.ArgumentParser(); sub=p.add_subparsers(dest='action',required=True)
    q=sub.add_parser('prepare'); q.add_argument('--config',default='benchmarks/scenarios.json'); q.add_argument('--output',required=True); q.add_argument('--adapter',default='target/release/vrp-bench-adapter.exe'); q.add_argument('--legacy-build')
    q=sub.add_parser('run'); q.add_argument('output'); q.add_argument('--retry-failed',action='store_true'); q.add_argument('--engines',nargs='+',choices=['vrp0','vrp1'])
    q=sub.add_parser('status'); q.add_argument('output')
    a=p.parse_args()
    if a.action=='prepare': prepare(read(a.config),a.output,a.adapter,a.legacy_build)
    elif a.action=='run': run(a.output,a.retry_failed,a.engines)
    else: progress(Path(a.output))


if __name__=='__main__': main()
