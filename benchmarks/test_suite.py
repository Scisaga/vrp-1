import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import psutil
from generate import generate, digest, tiny_cases, write_json
from validate import validate
from run import common_profile, new_attempt, previous, native_run, run, finalize
from report import distribution, aggregate, business_standard, render
from resources import run_process, ManagedProcess

ROOT=Path(__file__).resolve().parents[1]


class Fixtures(unittest.TestCase):
    def test_generation_reproduces_every_scenario(self):
        config=json.loads((ROOT/'benchmarks/scenarios.json').read_text())
        for spec in config['scenarios']:
            a=generate(spec,config['generator_seed']); b=generate(spec,config['generator_seed'])
            self.assertEqual(digest(a),digest(b),spec['id'])
        self.assertNotEqual(digest(generate(config['scenarios'][0],1)),digest(generate(config['scenarios'][0],2)))

    def test_profiles_reject_semantic_differences(self):
        request=tiny_cases()[0]; common_profile(request)
        for key,value in [('minimizeAgentFixedCost',{'soft':'1'}),('refTicketAfterDepTicket',{'hard':'1'})]:
            r=copy.deepcopy(request); r['problem']['weights'][key]=value
            with self.assertRaises(ValueError): common_profile(r)
        request['problem']['tickets'][0]['duration']='59'
        with self.assertRaises(ValueError): common_profile(request)

    def test_validator_detects_duplicates_and_omissions(self):
        request=tiny_cases()[0]
        result={'solution':{'routes':[{'agent':0,'tickets':[0,0]}]},'score':{},'unassigned':[]}
        errors=validate(request,result)['errors']
        self.assertIn('duplicate_or_invalid_ticket',errors); self.assertIn('unaccounted_or_false_unassigned',errors)

    def test_hard_zero_does_not_mean_full_service(self):
        request=tiny_cases()[0]
        result={'solution':{},'score':{'medium':-4000},'unassigned':[{'ticket':t} for t in range(4)]}
        check=validate(request,result)
        self.assertTrue(check['valid']); self.assertFalse(check['usable']); self.assertEqual(check['unassigned'],4)

    def test_skill_depot_capacity_and_windows_are_checked(self):
        request=tiny_cases()[0]
        result={'solution':{'routes':[{'agent':0,'tickets':[0,1,2,3]}]},'score':{},'unassigned':[]}
        check=validate(request,result)
        for k in ('agentCapacity','agentMaxTicket','agentSkillsAccordWithTicketSkills','agentQualificationLevelsMatchTicket','ticketStartServiceTimeMatchExpected'):
            self.assertGreater(check['amounts'][k],0,k)

    def test_differential_native_oracle_for_sampled_mappings(self):
        adapter=ROOT/'target/release/vrp-bench-adapter.exe'
        if not adapter.exists(): self.skipTest('build the adapter first')
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            folder=Path(tmp)
            for i,request in enumerate(tiny_cases()[:20]):
                inp=folder/'input.json'; out=folder/'result.json'; write_json(inp,request)
                subprocess.run([str(adapter),'eval',str(inp),str(out)],check=True,stdout=subprocess.DEVNULL)
                result=json.loads(out.read_text()); self.assertTrue(validate(request,result)['valid'],i)

    def test_native_semantics_include_buffers_balance_fixed_cost_and_relations(self):
        adapter=ROOT/'target/release/vrp-bench-adapter.exe'
        if not adapter.exists(): self.skipTest('build the adapter first')
        request=tiny_cases()[0]
        p=request['problem']; request['initialSolution']={'routes':[{'agent':0,'tickets':[0,2]},{'agent':1,'tickets':[1,3]}]}
        for name in ('minimizeAgentFixedCost','balanceAgentLoading','balanceAgentLoadingRatio','balanceAgentWorkingTime',
                     'relationTicketsSameAgent','refTicketSameAgentWithDepTicket','ticketArrivalTimeSameDateWithPlanTime'):
            p['weights'][name]={'soft':'1'}
        p['weights']['refTicketAfterDepTicket']={'hard':'1'}
        p['relatedPairs']=[{'first':0,'second':1}]; p['precedences']=[{'first':0,'second':1}]
        p['agents'][0]['fixedCost']='50'; p['agents'][0]['restricted']=True
        p['costs']={'restrictedChargePerHour':'50'}
        p.pop('estimateBuffer')
        import base64
        entries=len(p['locations'])**2
        p['matrices'][0]['sources']=base64.b64encode(bytes([2])*entries).decode()
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            inp=Path(tmp)/'in.json'; out=Path(tmp)/'out.json';write_json(inp,request)
            subprocess.run([str(adapter),'eval',str(inp),str(out)],check=True,stdout=subprocess.DEVNULL)
            result=json.loads(out.read_text());check=validate(request,result)
            self.assertTrue(check['valid'],check['errors'])


class Statistics(unittest.TestCase):
    def report_fixture(self,root):
        write_json(root/'plan.json',{'runs':[],'inputs':[]})
        write_json(root/'environment.json',{'created_utc':'2026-01-01T00:00:00Z','vrp1_commit':'test',
            'adapter_sha256':'test','source_archive_sha256':'test','rust':'test','java':'test','legacy':None,'hardware':{},
            'config':{'budget_ms':30000,'threads':1,'seeds':[1],'warmup_rounds':3,'warmup_ms':20000,'scenarios':[]}})
        (root/'source.zip').write_bytes(b'test source snapshot')

    def test_report_metadata_and_distinct_gate_count(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            root=Path(tmp);self.report_fixture(root)
            write_json(root/'score-gate/gate.json',{'cases':2,'checks':[{'case_sha256':'same'},{'case_sha256':'same'}]})
            with patch('report.plot_curves'):
                result=render(root,root/'report')
            self.assertEqual(result['comparability_gate']['distinct_input_mappings'],1)
            self.assertEqual(len(result['reporter_sha256']),64)
            self.assertIsNone(result['versions']['optaplanner'])
            self.assertIn('legacy build is unavailable',(root/'report/report.md').read_text())
            self.assertTrue((root/'report/report.md').exists())
            self.assertTrue((root/'source.zip').exists())
            self.assertFalse((root/'report/source.zip').exists())

    def test_private_report_cannot_leave_ignored_corpus(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp,tempfile.TemporaryDirectory() as external:
            root=Path(tmp);self.report_fixture(root)
            inp=root/'input.json';write_json(inp,{'problem':{}})
            plan={'id':'private','input':str(inp),'scenario':'private','mode':'cold','engine':'vrp0','seed':1,
                  'suite':'acceptance','scenario_spec':{'private':True,'features':[]}}
            write_json(root/'plan.json',{'runs':[plan],'inputs':[]})
            write_json(root/'runs/private/attempt-001/record.json',{'plan':plan,'status':'unsupported'})
            with self.assertRaisesRegex(ValueError,'private campaign reports'):
                render(root,Path(external)/'public')

    def test_fault_target_is_distinct_from_full_service(self):
        config=json.loads((ROOT/'benchmarks/scenarios.json').read_text())
        spec=next(s for s in config['scenarios'] if 'fault' in s['features'])
        request,_,_=generate(spec,7412026)
        record={'plan':{'scenario_spec':spec},'status':'succeeded',
            'quality_trace':[{'elapsed_ms':7,'score':[0,-9000,-100],'assigned':41}],
            'validation':{'valid':True,'score':[0,-9000,-100],'unassigned':9}}
        result=business_standard(record,request)
        self.assertEqual(result['first_business_standard_ms'],7)
        self.assertTrue(result['business_standard_met'])
        spec=copy.deepcopy(spec);spec['known_infeasible']=True;record['plan']['scenario_spec']=spec
        self.assertFalse(business_standard(record,request)['business_standard_applicable'])

    def test_distributions_keep_failures_and_censoring(self):
        self.assertEqual(distribution([1,2,100,4,5])['median'],4)
        self.assertEqual(distribution([])['n'],0)
        records=[]
        for seed,status in [(11,'succeeded'),(29,'timeout'),(47,'validation_failed')]:
            records.append({'plan':{'scenario':'a','mode':'cold','engine':'vrp1','suite':'common','seed':seed},'status':status})
        summary=aggregate(records,5)
        self.assertEqual(summary['missing_records'],2)
        self.assertEqual(summary['statuses']['timeout'],1)
        self.assertEqual(summary['instances'][0]['first_full_feasible_censored'],3)
        self.assertEqual(summary['comparisons'][0]['noncomparable'],3)

    def test_cost_ratios_require_equal_service_and_feasibility(self):
        records=[]
        for engine,usable,cost in [('vrp0',False,100),('vrp1',True,200)]:
            records.append({'plan':{'scenario':'a','mode':'cold','engine':engine,'suite':'common','seed':1},'status':'succeeded',
                            'validation':{'valid':False,'usable':usable}})
        self.assertEqual(aggregate(records,2)['comparisons'][0]['noncomparable'],1)

    def test_resume_preserves_interrupted_attempt(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            root=Path(tmp); plan={'id':'stable'}
            first=new_attempt(root,plan); second=new_attempt(root,plan)
            self.assertEqual(json.loads((first/'record.json').read_text())['status'],'interrupted')
            write_json(second/'record.json',{'plan':plan,'status':'succeeded'})
            self.assertEqual(previous(root,plan)['status'],'succeeded')
            self.assertNotEqual(first,second)


class Processes(unittest.TestCase):
    def test_failed_and_malformed_outputs_preserve_actual_incumbents(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            root=Path(tmp); plan={'id':'failed','input':str(root/'input.json')}
            write_json(root/'input.json',{'problem':{}})
            event={'event':'incumbent','elapsed_ms':123,'score':[0,-1000,-42],'assigned':3}
            for status,expected in [('timeout','timeout'),('succeeded','validation_failed')]:
                folder=new_attempt(root,plan)
                (folder/'events.jsonl').write_text(json.dumps(event)+'\n',encoding='utf-8')
                (folder/'result.json').write_text('{malformed',encoding='utf-8')
                record=finalize(root,plan,folder,{'status':status})
                self.assertEqual(record['status'],expected)
                self.assertEqual(record['quality_trace'],[event])
                self.assertIsNone(record['first_full_feasible_ms'])
                self.assertTrue((folder/'record.json').exists())

    def test_java_retry_retains_new_failure_and_previous_batch_logs(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            root=Path(tmp); adapter=root/'adapter.exe'; adapter.write_bytes(b'fixture')
            classes=root/'classes'; classes.mkdir()
            write_json(root/'environment.json',{'adapter':str(adapter),'adapter_sha256':digest(adapter.read_bytes()),
                'legacy':{'classpath':str(classes),'classes':{},'jars':{}},'java_flags':[],
                'config':{'budget_ms':1000,'threads':1,'memory_mib':64,'warmup_rounds':0,'warmup_ms':1}})
            plan={'id':'retry','scenario':'a','mode':'cold','engine':'vrp0','suite':'common','input':'unused','seed':11}
            write_json(root/'plan.json',{'runs':[plan],'inputs':[]})
            for attempt in range(2):
                with patch('run.ManagedProcess',side_effect=FileNotFoundError('missing Java test fixture')):
                    run(root,retry=True)
                record=previous(root,plan)
                self.assertEqual(record['status'],'error')
                folders=sorted((root/'runs/retry').glob('attempt-*'))
                self.assertEqual(len(folders),attempt+1)
                self.assertTrue((folders[-1]/'record.json').exists())
            self.assertEqual(len(list((root/'batches').iterdir())),2)

    def test_spawn_failure_becomes_a_retained_run_record(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            root=Path(tmp)
            write_json(root/'environment.json',{'adapter':str(root/'missing.exe'),'config':{'budget_ms':1000,'threads':1,'memory_mib':64}})
            plan={'id':'missing','input':str(root/'unused.json'),'seed':1}
            native_run(root,plan)
            self.assertEqual(previous(root,plan)['status'],'error')
            self.assertIn('error_type',previous(root,plan))
    def test_timeout_reaps_parent_and_descendant(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            folder=Path(tmp); pidfile=folder/'pid.txt'
            script="import subprocess,sys,time,pathlib;p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);pathlib.Path(sys.argv[1]).write_text(str(p.pid));time.sleep(60)"
            result=run_process([sys.executable,'-c',script,str(pidfile)],folder/'out',folder/'err',1)
            self.assertEqual(result['status'],'timeout')
            pid=int(pidfile.read_text()); time.sleep(.1)
            self.assertFalse(psutil.pid_exists(pid),'descendant survived timeout')

    def test_cancel_is_explicit_and_reaped(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            folder=Path(tmp); cancel=folder/'CANCEL'
            timer=threading.Timer(.3,cancel.touch); timer.start()
            try: result=run_process([sys.executable,'-c','import time;time.sleep(60)'],folder/'out',folder/'err',5,cancel_file=cancel)
            finally: timer.join()
            self.assertEqual(result['status'],'cancelled'); self.assertLess(result['wall_ms'],2000)

    def test_nonzero_exit_is_retained(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            folder=Path(tmp); result=run_process([sys.executable,'-c','raise SystemExit(7)'],folder/'out',folder/'err',5)
            self.assertEqual(result['status'],'error'); self.assertEqual(result['exit_code'],7)

    def test_cpu_limit_and_committed_memory_are_real(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            folder=Path(tmp); result=run_process([sys.executable,'-c','while True: pass'],folder/'out',folder/'err',5,cpu_budget=.15)
            self.assertEqual(result['status'],'cpu_limit'); self.assertGreater(result['cpu_seconds'],.1)
            if sys.platform=='win32': self.assertGreater(result['peak_tree_commit_bytes'],0)

    @unittest.skipUnless(sys.platform=='win32','Windows hard committed-memory limit')
    def test_memory_limit_prevents_large_allocation(self):
        with tempfile.TemporaryDirectory(dir=ROOT/'corpus') as tmp:
            folder=Path(tmp)
            result=run_process([sys.executable,'-c','x=bytearray(256*1024*1024)'],folder/'out',folder/'err',5,memory_mib=64)
            self.assertEqual(result['status'],'error')
            self.assertIn('MemoryError',(folder/'err').read_text())


if __name__=='__main__': unittest.main(verbosity=2)
