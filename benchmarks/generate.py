"""Seeded public fixtures; no network, dates, customer data, or implicit matrices."""
import base64
import copy
import hashlib
import json
import random
import struct
from pathlib import Path

WEIGHTS = '''agentCapacity agentMaxTicket agentSkillsAccordWithTicketSkills
agentQualificationLevelsMatchTicket refTicketAfterDepTicket refTicketSameAgentWithDepTicket
serviceFinishedAfterMaxEndTime ticketStartServiceTimeMatchExpected ticketArrivalTimeSameDateWithPlanTime
relationTicketsSameAgent minimizeTravelTime minimizeTravelDistance minimizeAgentFixedCost sameDepo
balanceAgentLoading balanceAgentLoadingRatio balanceAgentWorkingTime minimizeTicketChanging ticketUnassigned
unreachableLeg agentShiftOvertime executionConsistency minimizeVisitOrderChanging minimizeServiceTimeChanging'''.split()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode()


def digest(value):
    return hashlib.sha256(value if isinstance(value, bytes) else canonical(value)).hexdigest()


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix(path.suffix + '.tmp')
    temp.write_bytes(canonical(value) + b'\n')
    temp.replace(path)


def matrix(points, unreachable=()):
    n = len(points)
    times, distances = bytearray(n*n*4), bytearray(n*n*4)
    blocked = set(unreachable)
    for i, (x,y) in enumerate(points):
        for j, (u,v) in enumerate(points):
            d = (abs(x-u)+abs(y-v))*100
            # All clocks, including every possible path, stay on whole minutes.
            t = ((d+499)//500)*60
            if i != j and (i in blocked or j in blocked):
                t = d = 0xffffffff
            struct.pack_into('<I', times, (i*n+j)*4, t)
            struct.pack_into('<I', distances, (i*n+j)*4, d)
    enc = lambda b: base64.b64encode(b).decode('ascii')
    return {'profile':'synthetic-frozen-grid', 'durations':enc(times), 'distances':enc(distances)}


def arrays(problem):
    return [list(struct.unpack('<'+'I'*(len(base64.b64decode(m['durations']))//4),base64.b64decode(m['durations']))) for m in problem['matrices']]


def generate(spec, seed):
    rng = random.Random(int(digest([seed,spec['id']])[:16],16))
    n, v = spec['tickets'], spec['agents']
    features = set(spec['features'])
    depots = 4 if 'depots' in features else 1
    points = [(d*80, d%2*60) for d in range(depots)]
    for t in range(n):
        region = (t%v)%depots if 'depots' in features else (t%4 if spec['layout']=='regional' else 0)
        spread = 8 if spec['layout']=='dense' else 25
        points.append((region*80+rng.randrange(1,spread), region%2*60+rng.randrange(1,spread)))
    weights = {w:{} for w in WEIGHTS}
    for w in ('agentCapacity','agentMaxTicket','agentSkillsAccordWithTicketSkills','agentQualificationLevelsMatchTicket','sameDepo'):
        weights[w] = {'hard':'1'}
    weights['ticketUnassigned'] = {'medium':'1000'}
    weights['minimizeTravelTime'] = {'soft':'1'}
    if 'windows' in features:
        weights['ticketStartServiceTimeMatchExpected'] = {'hard':'1'}
    if spec['suite']=='acceptance':
        weights['unreachableLeg'] = {'hard':'1'}
        if 'shift' in features: weights['agentShiftOvertime'] = {'hard':'1'}
        if 'precedence' in features: weights['refTicketAfterDepTicket'] = {'hard':'1'}
    p = {'id':spec['id'], 'horizon':{'startLocal':'2026-01-02T00:00:00','timezone':'Asia/Shanghai'},
         'locations':[{'id':f'L{i:04}', 'lng':round(120+x*.001,6),'lat':round(30+y*.001,6)} for i,(x,y) in enumerate(points)],
         'capacityDimensions':[{'name':'units','unit':'piece','penaltyUnit':'1'}],
         'agents':[], 'tickets':[], 'weights':weights, 'estimateBuffer':{'modelPermille':0,'fallbackPermille':0}}
    dimensions = 2 if 'two_dimensions' in features else 1
    if dimensions==2: p['capacityDimensions'].append({'name':'volume','unit':'box','penaltyUnit':'1'})
    for a in range(v):
        limit = (n+v-1)//v+4
        if 'heterogeneous' in features: limit += a%3
        p['agents'].append({'id':f'V{a:03}', 'resourceId':f'R{a:03}', 'startLocation':a%depots,
            'endLocation':a%depots, 'startTime':'28800', 'endTime':'82800', 'maxTickets':limit,
            'capacity':[str(limit*3)]*dimensions, 'skills':['base',f'skill{a%3}'],
            'qualificationLevels':{'level':float(a%3+1)}, 'depotId':f'D{a%depots}' if depots>1 else ''})
    for t in range(n):
        owner=t%v
        demand=[1+rng.randrange(2)] if 'capacity' in features else [0]
        if dimensions==2: demand.append(1+rng.randrange(3))
        ticket={'id':f'T{t:04}', 'location':depots+t, 'type':1, 'duration':str(120+(t%3)*60),
            'earliestStart':'28800', 'latestStart':'82800', 'latestEnd':'84600', 'demand':[str(x) for x in demand],
            'skillsRequired':[f'skill{owner%3}'] if 'skills' in features else ['base'],
            'qualificationRequired':{'level':float(owner%3+1)} if 'qualifications' in features else {},
            'depotId':f'D{owner%depots}' if depots>1 else ''}
        if 'pickup' in features and t%3==0: ticket['type']=2
        p['tickets'].append(ticket)
    blocked = [depots+n-1] if 'unreachable' in features else []
    p['matrices']=[matrix(points,blocked)]
    initial={'routes':[{'agent':a,'tickets':list(range(a,n,v))} for a in range(v)]}
    times=arrays(p)[0]; size=len(points)
    for route in initial['routes']:
        a=route['agent']; clock=28800; previous=a%depots
        total=[0]*dimensions
        for position,t in enumerate(route['tickets']):
            tk=p['tickets'][t]
            clock += times[previous*size+tk['location']] if times[previous*size+tk['location']]<0xfffffffe else 0
            if 'windows' in features:
                earliest=max(28800,clock-600)
                if position%3==0: earliest=clock+600
                tk['earliestStart']=str(earliest); clock=max(clock,earliest)
                tk['latestStart']=str(clock+900)
            clock += int(tk['duration']); previous=tk['location']
            total=[x+int(y) for x,y in zip(total,tk['demand'])]
        if 'capacity' in features: p['agents'][a]['capacity']=[str(x+2) for x in total]
    if 'precedence' in features:
        p['precedences']=[{'first':t,'second':t+1} for t in range(0,min(n-1,40),2)]
        # Preserve a documented feasible witness after adding cross-route waits.
        predecessors={t:[] for t in range(n)}
        for first,second in ((x['first'],x['second']) for x in p['precedences']): predecessors[second].append(first)
        for route in initial['routes']:
            for previous,t in zip(route['tickets'],route['tickets'][1:]): predecessors[t].append(previous)
        finished={}
        while len(finished)<n:
            before=len(finished)
            for route in initial['routes']:
                a=route['agent']
                for position,t in enumerate(route['tickets']):
                    if t in finished or any(x not in finished for x in predecessors[t]): continue
                    previous=route['tickets'][position-1] if position else None
                    loc=p['tickets'][previous]['location'] if previous is not None else a%depots
                    arrival=(finished[previous] if previous is not None else 28800)+times[loc*size+p['tickets'][t]['location']]
                    start=max([arrival,int(p['tickets'][t]['earliestStart'])]+[finished[x] for x in predecessors[t]])
                    p['tickets'][t]['latestStart']=str(max(int(p['tickets'][t]['latestStart']),start+900))
                    finished[t]=start+int(p['tickets'][t]['duration'])
            if len(finished)==before: raise ValueError('witness dependency cycle')
    transformations=[]
    if 'execution' in features:
        weights['executionConsistency']={'hard':'1'}
        weights['minimizeTicketChanging']={'soft':'100'}
        weights['minimizeVisitOrderChanging']={'soft':'10'}
        weights['minimizeServiceTimeChanging']={'soft':'1'}
        execution={'routes':[], 'tickets':[]}
        for route in initial['routes']:
            a=route['agent']; agent=p['agents'][a]
            agent['startTime']='32400'
            load=sum(int(p['tickets'][t]['demand'][0]) for t in route['tickets'])
            execution['routes'].append({'agent':a,'initialLoad':{'values':[str(load)]},'fixedCostPaid':True})
            for position,t in enumerate(route['tickets']):
                p['tickets'][t]['originalAgent']=a
                execution['tickets'].append({'ticket':t,'onlyAgent':a,'originalPosition':position,
                                             'originalStart':str(32400+position*1200)})
        if 'insert' in features:
            t=n-1; owner=t%v
            initial['routes'][owner]['tickets'].remove(t)
            execution['tickets']=[e for e in execution['tickets'] if e['ticket']!=t]
            p['tickets'][t].pop('originalAgent',None)
            p['tickets'][t]['type']=0; p['tickets'][t]['demand']=['0']
            p['tickets'][t]['latestStart']='34800'; weights['ticketStartServiceTimeMatchExpected']={'soft':'100'}
            transformations.append({'operation':'synthetic_insert','ticket':t,'new_service_order':True})
        if 'cancel' in features:
            # A loaded cancellation becomes a physical return-to-depot unload visit.
            t=n-1; owner=t%v; p['tickets'][t]['location']=owner%depots
            next(e for e in execution['tickets'] if e['ticket']==t)['auxiliary']=True
            p['tickets'][t]['id']='AUX-CANCEL-UNLOAD'
            transformations.append({'operation':'synthetic_cancel_loaded','ticket':t,'replacement':'auxiliary_unload'})
        if 'fault' in features:
            execution['routes'][0]['unavailable']=True
            for t in initial['routes'][0]['tickets']:
                next(e for e in execution['tickets'] if e['ticket']==t)['blocked']=True
            initial['routes'][0]['tickets']=[]
            transformations.append({'operation':'synthetic_vehicle_fault','agent':0,'cargo_transfer':False})
        if 'completed' in features:
            execution['routes'][1]['completedTickets']=3
            execution['routes'][1]['workedSeconds']='3600'
            execution['pastDependencies']=[{'ticket':1,'agent':1,'finishedAt':'33000','afterFinish':True}]
            weights['refTicketAfterDepTicket']={'hard':'1'}
            transformations.append({'operation':'synthetic_completed_prefix','agent':1,'count':3})
        if 'lock' in features:
            t=initial['routes'][1]['tickets'][0]
            p['tickets'][t]['pinned']=True
            execution['routes'][1]['lockedPrefix']=[t]
        p['execution']=execution
    if 'conflict' in features:
        for a in p['agents']: a['capacity']=['2']; a['maxTickets']=2
        p['tickets'][0]['skillsRequired']=['missing-skill']
        transformations.append({'operation':'infeasible_capacity_and_skill_supply','full_assignment_possible':False})
    return {'problem':p}, initial, transformations


def tiny_cases():
    """Eighty fixed-seed mapping/order samples of a handcrafted four-ticket fixture."""
    spec={'id':'gate','tickets':4,'agents':2,'layout':'dense','features':['capacity','skills','qualifications','windows'], 'suite':'common'}
    request,initial,_=generate(spec,123)
    p=request['problem']; p['agents'][0]['capacity']=['2']; p['agents'][0]['maxTickets']=1
    p['agents'][1]['depotId']='D1'; p['tickets'][0]['depotId']='D0'
    p['tickets'][2]['latestStart']='28800'
    rng=random.Random(23)
    cases=[]
    for _ in range(80):
        r=copy.deepcopy(request); routes=[[],[]]
        order=list(range(4)); rng.shuffle(order)
        for t in order:
            a=rng.randrange(3)
            if a<2: routes[a].append(t)
        r['initialSolution']={'routes':[{'agent':a,'tickets':ts} for a,ts in enumerate(routes)]}
        cases.append(r)
    return cases
