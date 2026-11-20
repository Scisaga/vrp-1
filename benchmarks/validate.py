"""Independent business validator for the explicit evaluation profiles (not a production DTO)."""
import base64
from collections import defaultdict, deque
import struct
import bisect
import math
from generate import WEIGHTS


def num(d,k,default=0): return int(d.get(k,default))
def ceildiv(n,d=60): return max(0,(n+d-1)//d)
def vector(d,k): return [int(x) for x in d.get(k,[])]
def score_of(result): return [num(result.get('score',{}),k) for k in ('hard','medium','soft')]


def validate(request,result):
    p=request['problem']; agents=p['agents']; tickets=p['tickets']; n=len(tickets); dims=len(p.get('capacityDimensions',[]))
    errors=[]; owner={}; position={}; routes={}
    for r in result.get('solution',{}).get('routes',[]):
        a=num(r,'agent')
        if a<0 or a>=len(agents) or a in routes:
            errors.append('duplicate_or_invalid_agent'); continue
        routes[a]=[]
        for t in r.get('tickets',[]):
            t=int(t)
            if t<0 or t>=n or t in owner:
                errors.append('duplicate_or_invalid_ticket'); continue
            position[t]=len(routes[a]); owner[t]=a; routes[a].append(t)
    er={num(x,'agent'):x for x in p.get('execution',{}).get('routes',[])}
    et={num(x,'ticket'):x for x in p.get('execution',{}).get('tickets',[])}
    auxiliary={t for t,x in et.items() if x.get('auxiliary')}
    missing=set(range(n))-set(owner)
    if 'unassigned' in result:
        declared=[num(x,'ticket') for x in result['unassigned']]
        # Engine omits auxiliary visits from the business unassigned list.
        if len(set(declared))!=len(declared) or set(declared)!=(missing-auxiliary): errors.append('unaccounted_or_false_unassigned')
    for t,tk in enumerate(tickets):
        if tk.get('pinned') and owner.get(t)!=num(tk,'originalAgent'): errors.append('pinned')
        rule=et.get(t,{})
        if t in owner and (rule.get('blocked') or er.get(owner[t],{}).get('unavailable')): errors.append('blocked_or_unavailable')
        if t in owner and 'onlyAgent' in rule and owner[t]!=num(rule,'onlyAgent'): errors.append('cargo_owner')
    for a,r in er.items():
        prefix=r.get('lockedPrefix',[])
        if routes.get(a,[])[:len(prefix)]!=prefix: errors.append('locked_prefix')
    mats=[]; distances=[]; sources=[]
    for m in p['matrices']:
        b=base64.b64decode(m['durations']); mats.append(struct.unpack('<'+'I'*(len(b)//4),b))
        b=base64.b64decode(m['distances']); distances.append(struct.unpack('<'+'I'*(len(b)//4),b))
        sources.append(base64.b64decode(m.get('sources','')))
    size=len(p['locations']); amounts={k:0 for k in WEIGHTS}; travel=distance=0
    def edge(a,i,j):
        nonlocal travel,distance
        m=num(agents[a],'matrix'); ix=i*size+j; d=mats[m][ix]
        if d==0xffffffff:
            amounts['unreachableLeg']+=1; return 0
        if d==0xfffffffe: raise ValueError('unfrozen matrix')
        travel+=d; distance+=distances[m][ix]
        source=sources[m][ix] if sources[m] else 0
        buffer=p.get('estimateBuffer',{'modelPermille':100,'fallbackPermille':200})
        margin=num(buffer,'modelPermille') if source==2 else num(buffer,'fallbackPermille') if source==3 else 0
        return (d*(1000+margin)+999)//1000
    starts={}; ends={}; arrivals={}; route_end={}; incoming={}; graph=defaultdict(set)
    for a,agent in enumerate(agents):
        route=routes.get(a,[]); history=er.get(a,{})
        used=bool(route or num(history,'completedTickets') or num(history,'workedSeconds') or history.get('fixedCostPaid') or history.get('mustReturn') or any(vector(history.get('initialLoad',{}),'values')))
        if not used: continue
        clock=num(agent,'startTime'); here=num(agent,'startLocation')
        for i,t in enumerate(route):
            tk=tickets[t]; leg=edge(a,here,num(tk,'location'))
            arrivals[t]=clock+leg; starts[t]=max(arrivals[t],num(tk,'earliestStart')); ends[t]=starts[t]+num(tk,'duration')
            incoming[t]=(route[i-1] if i else None,leg)
            if i: graph[route[i-1]].add(t)
            clock=ends[t]; here=num(tk,'location')
        back=0 if history.get('unavailable') else edge(a,here,num(agent,'endLocation'))
        route_end[a]=(route[-1] if route else None,back)
        count=len([t for t in route if t not in auxiliary])+num(history,'completedTickets')
        amounts['balanceAgentLoading']+=count*count
        here=num(agent,'startLocation'); raw_travel=0
        for t in route:
            value=mats[num(agent,'matrix')][here*size+num(tickets[t],'location')]
            if value<0xfffffffe: raw_travel+=value
            here=num(tickets[t],'location')
        if not history.get('unavailable'):
            value=mats[num(agent,'matrix')][here*size+num(agent,'endLocation')]
            if value<0xfffffffe: raw_travel+=value
        fixed=0 if history.get('fixedCostPaid') else num(agent,'fixedCost')
        amounts['minimizeAgentFixedCost']+=fixed+(num(p.get('costs',{}),'restrictedChargePerHour')*raw_travel//3600 if agent.get('restricted') else 0)
        hours=ceildiv(raw_travel+sum(num(tickets[t],'duration') for t in route)+num(history,'workedSeconds'),3600)
        amounts['balanceAgentWorkingTime']+=hours*hours
        ratio=0.0
        for d,capacity in enumerate(vector(agent,'capacity')):
            if capacity>0:
                delivered=sum(vector(tickets[t],'demand')[d] for t in route if tickets[t].get('type',0) in (1,'TICKET_TYPE_DELIVERY'))
                delivered+=int(history.get('delivered',[0]*dims)[d]) if d<len(history.get('delivered',[0]*dims)) else 0
                ratio+=delivered/capacity
        amounts['balanceAgentLoadingRatio']+=math.floor(max(10*ratio,1)**2)
        if num(agent,'maxTickets'): amounts['agentMaxTicket']+=max(0,count-num(agent,'maxTickets'))
        load=vector(history.get('initialLoad',{}),'values') if 'initialLoad' in history else [sum(vector(tickets[t],'demand')[d] for t in route if tickets[t].get('type',0) in (1,'TICKET_TYPE_DELIVERY')) for d in range(dims)]
        peak=load.copy()
        for t in route:
            tk=tickets[t]; kind=tk.get('type',0)
            for d,value in enumerate(vector(tk,'demand')):
                load[d]+=value if kind in (2,'TICKET_TYPE_PICKUP') else (-value if kind in (1,'TICKET_TYPE_DELIVERY') else 0)
                peak[d]=max(peak[d],load[d])
                amounts['executionConsistency']+=max(0,-load[d]) if 'execution' in p else 0
        for d,peak_value in enumerate(peak):
            cap=vector(agent,'capacity')[d]; tol=agent.get('capacityTolerancePermille',[0]*dims)
            limit=cap*(1000+int(tol[d] if d<len(tol) else 0))//1000
            amounts['agentCapacity']+=ceildiv(peak_value-limit,num(p['capacityDimensions'][d],'penaltyUnit',1) or 1)
    precedence=p.get('precedences',[]) if any(num(p['weights'].get('refTicketAfterDepTicket',{}),k) for k in ('hard','medium','soft')) else []
    for pair in precedence:
        first,second=num(pair,'first'),num(pair,'second')
        if second in owner:
            if first in owner: graph[first].add(second)
            else: amounts['refTicketAfterDepTicket']+=1
    indegree={t:0 for t in owner}
    for children in graph.values():
        for t in children: indegree[t]+=1
    queue=deque(t for t,d in indegree.items() if not d); topo=[]
    while queue:
        t=queue.popleft(); topo.append(t)
        for child in graph[t]:
            indegree[child]-=1
            if not indegree[child]: queue.append(child)
    cyclic=len(topo)!=len(owner)
    if cyclic: amounts['refTicketAfterDepTicket']+=len(owner)-len(topo)
    parents=defaultdict(list)
    if not cyclic:
        for pair in precedence:
            if num(pair,'first') in owner: parents[num(pair,'second')].append(num(pair,'first'))
    past=defaultdict(list)
    for rule in p.get('execution',{}).get('pastDependencies',[]):
        if rule.get('afterFinish'): past[num(rule,'ticket')].append(num(rule,'finishedAt'))
    for t in (list(owner) if cyclic else topo):
        a=owner[t]; prev,leg=incoming[t]
        arrivals[t]=(ends[prev] if prev is not None else num(agents[a],'startTime'))+leg
        starts[t]=max([arrivals[t],num(tickets[t],'earliestStart')]+[ends[x] for x in parents[t]]+past[t])
        ends[t]=starts[t]+num(tickets[t],'duration')
    for pair in precedence:
        first,second=num(pair,'first'),num(pair,'second')
        if first in owner and second in owner: amounts['refTicketAfterDepTicket']+=ceildiv(ends[first]-starts[second])
    for pair in p.get('precedences',[]):
        first,second=num(pair,'first'),num(pair,'second')
        if first in owner and second in owner and owner[first]!=owner[second]: amounts['refTicketSameAgentWithDepTicket']+=1
    related={tuple(sorted((num(pair,'first'),num(pair,'second')))) for pair in p.get('relatedPairs',[])}
    for first,second in related:
        if first in owner and second in owner and owner[first]!=owner[second]: amounts['relationTicketsSameAgent']+=1
    appointments=0
    for t,a in owner.items():
        tk=tickets[t]; agent=agents[a]; rule=et.get(t,{})
        if not set(tk.get('skillsRequired',[]))<=set(agent.get('skills',[])): amounts['agentSkillsAccordWithTicketSkills']+=1
        if any(float(agent.get('qualificationLevels',{}).get(k,-1))<float(v) for k,v in tk.get('qualificationRequired',{}).items()): amounts['agentQualificationLevelsMatchTicket']+=1
        if tk.get('depotId') and agent.get('depotId') and tk['depotId']!=agent['depotId']: amounts['sameDepo']+=1
        if t not in auxiliary:
            day_starts=[int(x) for x in p.get('horizon',{}).get('dayStarts',[])]
            day=(bisect.bisect_right(day_starts,starts[t])-1) if day_starts else starts[t]//86400
            amounts['ticketArrivalTimeSameDateWithPlanTime']+=int(day!=num(tk,'day'))
            late=ceildiv(starts[t]-num(tk,'latestStart')); appointments+=int(late>0)
            amounts['ticketStartServiceTimeMatchExpected']+=late
            amounts['serviceFinishedAfterMaxEndTime']+=ceildiv(ends[t]-num(tk,'latestEnd'))
            if 'originalPosition' in rule:
                pos=len([q for q in routes[a][:position[t]] if q not in auxiliary])
                amounts['minimizeVisitOrderChanging']+=int(pos!=num(rule,'originalPosition'))
            if 'originalStart' in rule: amounts['minimizeServiceTimeChanging']+=ceildiv(abs(starts[t]-num(rule,'originalStart')))
        if 'loadBefore' in rule:
            before=num(rule,'loadBefore')
            if owner.get(before)!=a or position.get(before,n)>=position[t]: amounts['executionConsistency']+=1
        if 'committedEarliest' in rule: amounts['executionConsistency']+=ceildiv(num(rule,'committedEarliest')-starts[t])
        if 'committedLatest' in rule: amounts['executionConsistency']+=ceildiv(starts[t]-num(rule,'committedLatest'))
    for t,tk in enumerate(tickets):
        if t not in auxiliary and 'originalAgent' in tk and owner.get(t)!=num(tk,'originalAgent'): amounts['minimizeTicketChanging']+=1
    for a,(last,back) in route_end.items():
        end=(ends[last] if last is not None else num(agents[a],'startTime'))+back
        if 'endTime' in agents[a]: amounts['agentShiftOvertime']+=ceildiv(end-num(agents[a],'endTime'))
    amounts['ticketUnassigned']=len(missing-auxiliary)
    amounts['minimizeTravelTime']=travel; amounts['minimizeTravelDistance']=distance
    supported={'agentCapacity','agentMaxTicket','agentSkillsAccordWithTicketSkills','agentQualificationLevelsMatchTicket',
               'sameDepo','ticketUnassigned','minimizeTravelTime','minimizeTravelDistance','ticketStartServiceTimeMatchExpected',
               'serviceFinishedAfterMaxEndTime','unreachableLeg','agentShiftOvertime','refTicketAfterDepTicket',
               'executionConsistency','minimizeTicketChanging','minimizeVisitOrderChanging','minimizeServiceTimeChanging'}
    supported.update({'minimizeAgentFixedCost','balanceAgentLoading','balanceAgentLoadingRatio','balanceAgentWorkingTime',
                      'ticketArrivalTimeSameDateWithPlanTime','refTicketSameAgentWithDepTicket','relationTicketsSameAgent'})
    score=[0,0,0]
    for k,w in p['weights'].items():
        if k not in supported and any(num(w,level) for level in ('hard','medium','soft')): errors.append('unsupported_validator_weight:'+k)
        for i,level in enumerate(('hard','medium','soft')): score[i]-=amounts[k]*num(w,level)
    if score!=score_of(result): errors.append('independent_score_mismatch')
    return {'valid':not errors,'errors':errors,'score':score,'unassigned':len(missing-auxiliary),
            'travel_seconds':travel,'distance_meters':distance,'appointment_violations':appointments,
            'reassignments':amounts['minimizeTicketChanging'],'order_changes':amounts['minimizeVisitOrderChanging'],
            'appointment_change_minutes':amounts['minimizeServiceTimeChanging'],
            'amounts':amounts,'usable':not errors and score[0]==0 and not (missing-auxiliary)}
