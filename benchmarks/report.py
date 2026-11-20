"""Instance-level statistics, censoring and public synthetic-only publication."""
import argparse
from collections import Counter,defaultdict
import json
import math
from pathlib import Path
import statistics
from generate import write_json, digest
from run import read, previous


def distribution(values):
    values=sorted(x for x in values if x is not None)
    if not values: return {'n':0,'median':None,'min':None,'q1':None,'q3':None,'max':None,'values':[]}
    q=statistics.quantiles(values,n=4,method='inclusive') if len(values)>1 else values*3
    return {'n':len(values),'median':statistics.median(values),'min':values[0],'q1':q[0],'q3':q[2],'max':values[-1],'values':values}


def business_standard(record,request):
    """Derive the preregistered target from actual events, including fault exceptions."""
    spec=record['plan']['scenario_spec']; p=request['problem']
    if record['status']=='unsupported' or spec.get('known_infeasible'):
        return {'business_standard_applicable':False,'first_business_standard_ms':None,
                'business_standard_reason':'unsupported' if record['status']=='unsupported' else 'full service is known infeasible; separate boundary acceptance'}
    expected_unassigned=0
    if 'fault' in spec['features']:
        expected_unassigned=sum(bool(t.get('blocked')) and not t.get('auxiliary') for t in p.get('execution',{}).get('tickets',[]))
    medium={k:int(w.get('medium',0)) for k,w in p['weights'].items() if int(w.get('medium',0))}
    times=[]
    for e in record.get('quality_trace',[]):
        if e['score'][0]!=0: continue
        if set(medium)=={'ticketUnassigned'}:
            reached=e['score'][1]==-expected_unassigned*medium['ticketUnassigned']
        else: reached=e['assigned']==len(p['tickets'])-expected_unassigned
        if reached: times.append(e['elapsed_ms'])
    return {'business_standard_applicable':True,'business_standard_expected_unassigned':expected_unassigned,
            'first_business_standard_ms':min(times) if times else None,'business_standard_censored':not bool(times),
            'business_standard_met':bool(record.get('validation',{}).get('valid')) and record.get('validation',{}).get('score',[1])[0]==0 and record.get('validation',{}).get('unassigned')==expected_unassigned,
            'business_standard_time_method':'actual incumbent events; explicit unassigned-only medium profile or initialized assigned count'}


def aggregate(records,expected):
    groups=defaultdict(list)
    for record in records:
        p=record['plan']; groups[(p['scenario'],p['mode'],p['engine'])].append(record)
    rows=[]
    for (scenario,mode,engine),runs in sorted(groups.items()):
        valid=[r for r in runs if r.get('validation',{}).get('valid')]
        full=[r for r in valid if r['validation']['usable']]
        rows.append({'scenario':scenario,'mode':mode,'engine':engine,'suite':runs[0]['plan']['suite'],
            'observed':len(runs),'statuses':dict(Counter(r['status'] for r in runs)),
            'full_feasible':len(full),'first_full_feasible_ms':distribution([r.get('first_full_feasible_ms') for r in valid]),
            'first_business_standard_ms':distribution([r.get('first_business_standard_ms') for r in runs]),
            'business_standard_met':sum(r.get('business_standard_met',False) for r in runs),
            'business_standard_censored':sum(r.get('business_standard_censored',False) for r in runs),
            'first_full_feasible_censored':len(runs)-sum(r.get('first_full_feasible_ms') is not None for r in runs),
            'acceptance_passed':sum(r.get('acceptance_passed',False) for r in runs),
            'unassigned':distribution([r['validation']['unassigned'] for r in valid]),
            'travel_seconds_full_feasible':distribution([r['validation']['travel_seconds'] for r in full]),
            'travel_seconds_all_valid':distribution([r['validation']['travel_seconds'] for r in valid]),
            'distance_meters':distribution([r['validation']['distance_meters'] for r in valid]),
            'appointment_violations':distribution([r['validation']['appointment_violations'] for r in valid]),
            **{key:distribution([r.get(key) for r in runs]) for key in ('cpu_seconds','wall_ms','solve_ms','budget_overrun_ms','peak_tree_commit_bytes','peak_tree_rss_sampled_bytes','process_startup_ms')},
            'failures':[{'seed':r['plan']['seed'],'status':r['status'],'errors':r.get('validation',{}).get('errors',[])} for r in runs if r['status']!='succeeded']})
    pairs=defaultdict(dict)
    for r in records:
        p=r['plan']
        if p['suite']=='common': pairs[(p['scenario'],p['mode'],p['seed'])][p['engine']]=r
    comparison=defaultdict(lambda: {'wins':0,'ties':0,'losses':0,'noncomparable':0,
        'business_wins':0,'business_ties':0,'business_losses':0,'business_unavailable':0,
        'improvement_percent':[], 'equal_quality':[], 'equal_quality_stricter':[]})
    for (scenario,mode,seed),engines in sorted(pairs.items()):
        cell=comparison[(scenario,mode)]
        a,b=engines.get('vrp0'),engines.get('vrp1')
        if a and b and a.get('validation',{}).get('valid') and b.get('validation',{}).get('valid'):
            quality0,quality1=tuple(a['validation']['score']),tuple(b['validation']['score'])
            cell['business_wins' if quality1>quality0 else 'business_losses' if quality1<quality0 else 'business_ties']+=1
        else: cell['business_unavailable']+=1
        if not a or not b or not all(r.get('validation',{}).get('usable') and r['status']=='succeeded' for r in (a,b)):
            cell['noncomparable']+=1; continue
        cost0,cost1=a['validation']['travel_seconds'],b['validation']['travel_seconds']
        cell['wins' if cost1<cost0 else 'losses' if cost1>cost0 else 'ties']+=1
        if cost0: cell['improvement_percent'].append(100*(cost0-cost1)/cost0)
        # A posteriori paired target, distinct from the preregistered full-service standard.
        target=max(cost0,cost1)
        hit={}
        for engine,r in engines.items():
            times=[e['elapsed_ms'] for e in r.get('quality_trace',[]) if e['score'][0]==0 and e['score'][1]==0 and -e['score'][2]<=target]
            hit[engine]=min(times) if times else None
        cell['equal_quality'].append({'seed':seed,'target_travel_seconds':target,'first_ms':hit})
        stricter=min(cost0,cost1); hit_stricter={}
        for engine,r in engines.items():
            times=[e['elapsed_ms'] for e in r.get('quality_trace',[]) if e['score'][0]==0 and e['score'][1]==0 and -e['score'][2]<=stricter]
            hit_stricter[engine]=min(times) if times else None
        cell['equal_quality_stricter'].append({'seed':seed,'target_travel_seconds':stricter,
            'first_ms':hit_stricter,'censored':{engine:t is None for engine,t in hit_stricter.items()}})
    comparisons=[{'scenario':s,'mode':m,**{k:v for k,v in c.items() if k!='improvement_percent'},
                  'cost_improvement_percent':distribution(c['improvement_percent'])} for (s,m),c in sorted(comparison.items())]
    return {'expected_records':expected,'observed_records':len(records),'missing_records':expected-len(records),
            'statuses':dict(Counter(r['status'] for r in records)),'instances':rows,'comparisons':comparisons}


def plot_curves(records,folder,budget_ms,png=False):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import numpy as np
    plt.rcParams.update({'font.size':9,'svg.fonttype':'none'})
    common=[r for r in records if r['plan']['suite']=='common']
    scenarios=sorted(set(r['plan']['scenario'] for r in common))
    if not scenarios:
        measured=[r for r in records if r.get('quality_trace')]
        names=sorted(set(r['plan']['scenario'] for r in measured))
        if not names: return
        fig,axes=plt.subplots(len(names),3,figsize=(13,3.2*len(names)),squeeze=False,constrained_layout=True)
        for i,name in enumerate(names):
            for r in measured:
                if r['plan']['scenario']!=name: continue
                trace=r['quality_trace']
                for level in range(3): axes[i,level].step([e['elapsed_ms']/1000 for e in trace],[e['score'][level] for e in trace],where='post',label=f"seed {r['plan']['seed']}")
            for level,label in enumerate(('Hard','Medium','Soft')):
                axes[i,level].set_title(name+' / '+label); axes[i,level].set_xlabel('Solve seconds')
                axes[i,level].set_xlim(0,budget_ms/1000); axes[i,level].grid(alpha=.2); axes[i,level].legend(fontsize=7)
        fig.suptitle('Native acceptance: actual incumbent scores, one instance per row; no cross-version ranking')
        fig.savefig(folder/'quality-acceptance.svg')
        if png: fig.savefig(folder/'quality-acceptance.png',dpi=120)
        plt.close(fig)
        return
    colors={'vrp0':'#b65c35','vrp1':'#087f8c'}
    for mode in ('cold','continue'):
        fig,axes=plt.subplots(len(scenarios),2,figsize=(12,3*len(scenarios)),squeeze=False,constrained_layout=True)
        for i,scenario in enumerate(scenarios):
            for engine in ('vrp0','vrp1'):
                runs=[r for r in common if r['plan']['scenario']==scenario and r['plan']['mode']==mode and r['plan']['engine']==engine]
                traces=[r.get('quality_trace',[]) for r in runs]
                if not any(traces): continue
                times=sorted(set([0,budget_ms]+[float(e['elapsed_ms']) for tr in traces for e in tr]))
                # Use actual event times, then sample the recorded step functions, never interpolate quality.
                times=np.array(times); values=[[],[]]
                for r in runs:
                    tr=r.get('quality_trace',[]); count=r['plan']['scenario_spec']['tickets']; ix=-1; unassigned=[]; costs=[]
                    for t in times:
                        while ix+1<len(tr) and tr[ix+1]['elapsed_ms']<=t: ix+=1
                        e=tr[ix] if ix>=0 else None
                        unassigned.append(count-e['assigned'] if e else math.nan)
                        costs.append(-e['score'][2] if r['status']=='succeeded' and r.get('validation',{}).get('valid') and e and e['score'][:2]==[0,0] and e['assigned']==count else math.nan)
                    values[0].append(unassigned); values[1].append(costs)
                for j in (0,1):
                    data=np.array(values[j],dtype=float)
                    # A median cost curve exists only where all five (or the configured N) runs are feasible.
                    valid=np.all(np.isfinite(data),axis=0)
                    med=np.full(len(times),np.nan); lo=med.copy(); hi=med.copy()
                    if valid.any():
                        med[valid]=np.median(data[:,valid],axis=0); lo[valid]=np.min(data[:,valid],axis=0); hi[valid]=np.max(data[:,valid],axis=0)
                    axes[i,j].step(times/1000,med,where='post',label=engine,color=colors[engine])
                    axes[i,j].fill_between(times/1000,lo,hi,step='post',alpha=.12,color=colors[engine])
            for j in (0,1):
                axes[i,j].set_title(scenario); axes[i,j].set_xlabel('Solve seconds (preprocessing + construction included)')
                max_time=max([budget_ms]+[e['elapsed_ms'] for r in common if r['plan']['scenario']==scenario and r['plan']['mode']==mode for e in r.get('quality_trace',[])])
                axes[i,j].set_xlim(0,max_time/1000); axes[i,j].axvline(budget_ms/1000,color='gray',linestyle=':',linewidth=.6)
                axes[i,j].grid(alpha=.2); axes[i,j].legend()
            axes[i,0].set_ylabel('Unassigned orders (median; min-max)')
            axes[i,1].set_ylabel('Travel seconds; all runs full + hard=0')
        fig.suptitle(f'{mode}: actual incumbent events; fixed inputs, single CPU; bands show seed range',fontsize=13)
        fig.savefig(folder/f'quality-{mode}.svg')
        if png: fig.savefig(folder/f'quality-{mode}.png',dpi=120)
        plt.close(fig)
    rows=[]
    for r in common:
        if r['plan']['mode']=='cold' and 'cpu_seconds' in r and 'peak_tree_commit_bytes' in r: rows.append(r)
    fig,axes=plt.subplots(1,2,figsize=(12,4),constrained_layout=True)
    for i,engine in enumerate(('vrp0','vrp1')):
        x=np.arange(len(scenarios))+(i-.5)*.35
        for j,key in enumerate(('cpu_seconds','peak_tree_commit_bytes')):
            y=[statistics.median([r[key]/(1024**2 if j else 1) for r in rows if r['plan']['engine']==engine and r['plan']['scenario']==s]) if any(r['plan']['engine']==engine and r['plan']['scenario']==s for r in rows) else math.nan for s in scenarios]
            axes[j].bar(x,y,.35,label=engine,color=colors[engine])
            axes[j].set_xticks(np.arange(len(scenarios)),[s.split('-')[0] for s in scenarios]); axes[j].legend(); axes[j].grid(axis='y',alpha=.2)
    axes[0].set_ylabel('CPU seconds per measured command (median)')
    axes[1].set_ylabel('MiB: kernel peak committed process tree')
    axes[1].set_title('Java: batch lifetime incl. warmup; Rust: one run')
    fig.suptitle('Cold solution construction on a prewarmed JVM; one CPU affinity')
    fig.savefig(folder/'resources.svg')
    if png: fig.savefig(folder/'resources.png',dpi=120)
    plt.close(fig)


def render(root,public):
    root=Path(root).resolve(); public=Path(public).resolve(); public.mkdir(parents=True,exist_ok=True)
    plan=read(root/'plan.json'); meta=read(root/'environment.json')
    records=[r for p in plan['runs'] if (r:=previous(root,p))]
    requests={}
    for record in records:
        p=record['plan']
        if p['input'] not in requests: requests[p['input']]=read(p['input'])
        record.update(business_standard(record,requests[p['input']]))
        path=sorted((root/'runs'/p['id']).glob('attempt-*/record.json'))[-1]
        write_json(path,record)
    summary=aggregate(records,len(plan['runs']))
    summary['reporter_sha256']=digest(Path(__file__).read_bytes())
    if (root/'verification.json').exists(): summary['evidence_verification']=read(root/'verification.json')
    if (root/'score-gate/gate.json').exists():
        gate=read(root/'score-gate/gate.json')
        summary['comparability_gate']={'checks':gate['cases'],'distinct_input_mappings':len({c['case_sha256'] for c in gate['checks']}),
            'full_problem_initial_plans':sum(i['scenario']['suite']=='common' for i in plan['inputs'])}
    startup=[]
    by_id={p['id']:p for p in plan['runs']}
    for batch in sorted((root/'batches').glob('*/batch.json')):
        commands=read(batch)['runs']; first=by_id[commands[0]['id']]
        folder=batch.parent; phases=[]
        for line in (folder/'stdout.jsonl').read_text(encoding='utf-8',errors='replace').splitlines():
            try: event=json.loads(line)
            except json.JSONDecodeError: continue
            if event.get('event')=='warmup_phase': phases.append(event)
        startup.append({'scenario':first['scenario'],'mode':first['mode'],
            'jvm_startup_ms':read(folder/'startup.json').get('startup_ms') if (folder/'startup.json').exists() else None,
            'warmup':read(folder/'warmup.json') if (folder/'warmup.json').exists() else None,
            'cold_first_model_compile_ms':phases[0]['compile_ms'] if phases else None,'warmup_phases':phases})
    summary['java_startup_and_warmup']=startup
    summary['versions']={'vrp1_base_commit':meta['vrp1_commit'],'vrp1_adapter_sha256':meta['adapter_sha256'],
        'source_archive_sha256':meta['source_archive_sha256'],'vrp0_commit':(meta['legacy'] or {}).get('source_commit'),
        'optaplanner':'999-20240618-SNAPSHOT; exact jar hashes retained in build manifest' if meta['legacy'] else None,
        'rust':meta['rust'],'java':meta['java'],'config':meta['config'],'hardware':meta['hardware']}
    # Never publish private individual results or source paths.
    if any(p['scenario_spec'].get('private') for p in plan['runs']) and not public.is_relative_to(Path(__file__).resolve().parents[1]/'corpus'):
        raise ValueError('private campaign reports must remain inside the ignored corpus directory')
    write_json(root/'summary.json',summary); write_json(public/'summary.json',summary)
    write_json(public/'traces.json',[{'scenario':r['plan']['scenario'],'mode':r['plan']['mode'],'engine':r['plan']['engine'],
        'seed':r['plan']['seed'],'input_sha256':r['plan']['input_sha256'],'status':r['status'],
        'incumbents':[[e['elapsed_ms'],*e['score'],e['assigned']] for e in r.get('quality_trace',[])]} for r in records])
    write_json(public/'artifact-hashes.json',{'vrp1_binary':meta['adapter_sha256'],
        'legacy_dependency_jars':(meta['legacy'] or {}).get('jars',{}),
        'legacy_class_files':(meta['legacy'] or {}).get('classes',{})})
    plot_curves(records,public,meta['config']['budget_ms'])
    lines=['# First reproducible multi-scenario evaluation','',
        f"Run created: {meta['created_utc']}. Expected records: {summary['expected_records']}; observed: {summary['observed_records']}; missing: {summary['missing_records']}.",'',
        (f"Evidence audit: `{summary['evidence_verification']['verified']}`; {summary['evidence_verification']['timed_attempts']} timed attempts; maximum observed search overrun {summary['evidence_verification']['max_observed_search_overrun_ms']:.3f} ms. Failures and unsupported records remain in the totals."
         if 'evidence_verification' in summary else 'Evidence verification has not been run for this campaign.'),'',
        (f"Search-objective gate: {summary['comparability_gate']['checks']} sampled checks covering {summary['comparability_gate']['distinct_input_mappings']} distinct assignment/order mappings, plus {summary['comparability_gate']['full_problem_initial_plans']} full-problem initial plans. Legacy scoring, the native oracle and the independent validator agree exactly. This is a gate for the restricted common profile, not an exhaustive proof of all legacy semantics."
         if 'comparability_gate' in summary else 'No legacy common-profile score gate is available.'),'',
        '## Versions and conditions','',
        f"vrp-1 base `{meta['vrp1_commit']}` plus the recorded evaluation instrumentation; adapter SHA-256 `{meta['adapter_sha256']}`. Measured source archive SHA-256 `{meta['source_archive_sha256']}`. To run a new evaluation under the same method, use the current checkout and the [reproduction commands](../../evaluation.md#复现命令) with the pinned legacy dependencies.",'',
        (f"vrp-0 `{meta['legacy'].get('source_commit','unavailable')}`; OptaPlanner `999-20240618-SNAPSHOT`, JDK 21. The complete evaluated domain and solver classes were rebuilt with javac; all dependency JAR hashes are pinned, including the SNAPSHOT artifacts."
         if meta['legacy'] else 'The legacy build is unavailable. Its runs are explicitly unsupported; this report makes no cross-version performance claim.'),'',
        f"{meta['config']['threads']} solver thread(s), {meta['config']['budget_ms']/1000:g} seconds per solve, {len(meta['config']['seeds'])} independent seeds; main runs serial. Each Java group receives {meta['config']['warmup_rounds']} × {meta['config']['warmup_ms']/1000:g}-second warmups. Java CPU excludes startup/warmup; Rust CPU covers its whole short-lived adapter process and native result explanation. Both final solutions are independently rescored outside these command intervals. Memory is committed process-tree high-water from Windows Job Objects: the Java lifetime includes warmup and preceding seeds; per-command sampled RSS is also retained. These scopes differ and do not establish end-to-end deployed-service resource ratios. See [method](../../evaluation.md).",'',
        '## Common objectives: paired final quality','',
        'Only full assignment with zero hard violations is eligible for cost ratios. Business W/T/L compares independently evaluated common-profile objectives on all valid initialized outputs; cost W/T/L uses full feasible pairs. All pairs use identical seeds. Positive percentages mean lower vrp-1 travel cost. Failures remain explicit.','',
        '| Scenario | Start | Business W/T/L | Cost W/T/L | Noncomparable cost | Median cost change |',
        '|---|---|---:|---:|---:|---:|']
    for c in summary['comparisons']:
        ratio=c['cost_improvement_percent']['median']; text='—' if ratio is None else f'{ratio:+.2f}%'
        lines.append(f"| {c['scenario']} | {c['mode']} | {c['business_wins']}/{c['business_ties']}/{c['business_losses']} | {c['wins']}/{c['ties']}/{c['losses']} | {c['noncomparable']} | {text} |")
    lines+=['','## Per-instance distributions and resources','',
            'Each cell reports median [min, max]. Time to first full feasible is right-censored when the standard is not met. Full JSON retains each value, quartiles, statuses and all excluded comparisons.','',
            '| Scenario / start | Engine | Full / runs | Travel seconds (full only) | First full ms | CPU s | Peak commit MiB |',
            '|---|---|---:|---:|---:|---:|---:|']
    def fmt(d,scale=1):
        return '—' if not d['n'] else f"{d['median']/scale:.1f} [{d['min']/scale:.1f}, {d['max']/scale:.1f}]"
    for row in summary['instances']:
        if row['suite']!='common': continue
        lines.append(f"| {row['scenario']} / {row['mode']} | {row['engine']} | {row['full_feasible']}/{row['observed']} | {fmt(row['travel_seconds_full_feasible'])} | {fmt(row['first_full_feasible_ms'])} | {fmt(row['cpu_seconds'])} | {fmt(row['peak_tree_commit_bytes'],1024**2)} |")
    lines+=['','## JVM cold startup and warmup','',
        'Startup is process launch to adapter entry. Cold model compilation is the first warmup factory build, separate from search. JIT columns are compiler milliseconds in each warmup round. These are not additional measured quality samples.','',
        'The fixed warmup is part of this configuration. Compilation remains visible in the last warmup round; these measurements do not demonstrate convergence to a fully compiled steady state. A longer-warmup sensitivity study remains necessary before making that stronger claim.','',
        '| Scenario / start | JVM startup ms | Cold model compile ms | Warmup wall s | JIT ms by round |',
        '|---|---:|---:|---:|---|']
    for item in startup:
        warm=item.get('warmup') or {}
        lines.append(f"| {item['scenario']} / {item['mode']} | {item['jvm_startup_ms']} | {item['cold_first_model_compile_ms']} | {warm.get('wall_ms',0)/1000:.2f} | {warm.get('jit_compilation_ms',[])} |")
    lines+=['','## Migration and capability acceptance','',
        'These are synthetic frozen execution projections and semantic fixtures. They do not claim replay of real operational events. Old-version unsupported records are retained and excluded from algorithm rankings. Full service is required except preregistered blocked cargo after a fault and the known infeasible boundary case.','',
        '| Scenario | Passed / runs | Median unassigned | First standard ms | CPU s | Peak commit MiB | Statuses |','|---|---:|---:|---:|---:|---:|---|']
    for row in summary['instances']:
        if row['suite']=='acceptance' and row['engine']=='vrp1': lines.append(f"| {row['scenario']} | {row['acceptance_passed']}/{row['observed']} | {row['unassigned']['median']} | {fmt(row['first_business_standard_ms'])} | {fmt(row['cpu_seconds'])} | {fmt(row['peak_tree_commit_bytes'],1024**2)} | {row['statuses']} |")
    lines+=['','## Quality versus time','',
        'The curves use actual incumbent events. No curve is fabricated before an observed incumbent; gaps remain missing. Shading is the min–max seed range. Travel curves appear only when every measured seed has full assignment and zero hard violations; no free-travel unassigned solution enters the cost curve. The dotted line marks the requested budget; observed overruns are retained.','',
        *[f'![{name}]({name})\n' for name in ('quality-cold.svg','quality-continue.svg','quality-acceptance.svg','resources.svg') if (public/name).exists()],
        '## Equal quality and limitations','',
        'The JSON `comparisons[].equal_quality` records the first actual time each implementation reaches the worse of the two paired final fully feasible travel costs. This is an explicitly post hoc target, separate from the preregistered first full-service standard; missing hits are censored. No unobserved time is extrapolated.','',
        '| Scenario / start | Paired target travel s | vrp-0 first ms | vrp-1 first ms |',
        '|---|---:|---:|---:|',
        *[f"| {c['scenario']} / {c['mode']} | {fmt(distribution([e['target_travel_seconds'] for e in c['equal_quality']]))} | {fmt(distribution([e['first_ms'].get('vrp0') for e in c['equal_quality']]))} | {fmt(distribution([e['first_ms'].get('vrp1') for e in c['equal_quality']]))} |" for c in summary['comparisons']],
        '',
        'A stricter, also post hoc target is the better of each paired final costs. The following counts show how many seeds reached it within the observed run; missing times are censored, not extrapolated.','',
        '| Scenario / start | Stricter target travel s | vrp-0 hits / pairs | vrp-1 hits / pairs |',
        '|---|---:|---:|---:|',
        *[f"| {c['scenario']} / {c['mode']} | {fmt(distribution([e['target_travel_seconds'] for e in c['equal_quality_stricter']]))} | {sum(e['first_ms'].get('vrp0') is not None for e in c['equal_quality_stricter'])}/{len(c['equal_quality_stricter'])} | {sum(e['first_ms'].get('vrp1') is not None for e in c['equal_quality_stricter'])}/{len(c['equal_quality_stricter'])} |" for c in summary['comparisons']],
        '',
        f"Statuses: `{summary['statuses']}`. See [all public distributions](summary.json). Private raw outputs, resource samples, source snapshots, commands and diagnostics remain in `corpus/evaluation/`. Historical v1 scores and old UI screenshots are not this round's timed results. The synthetic suite does not establish production performance, historical-data quality, global optimality, or a result about the OptaPlanner framework in general."]
    (public/'report.md').write_text('\n'.join(lines)+'\n',encoding='utf-8')
    print(json.dumps({'reported':summary['observed_records'],'missing':summary['missing_records'],'statuses':summary['statuses']}))
    return summary


if __name__=='__main__':
    p=argparse.ArgumentParser(); p.add_argument('campaign'); p.add_argument('--public-output',default='docs/benchmarks/round-1')
    a=p.parse_args(); render(a.campaign,a.public_output)
