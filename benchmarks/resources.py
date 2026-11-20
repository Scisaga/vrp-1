"""Serial trusted subprocess containment and resource accounting. No shell commands."""
import ctypes
import os
import signal
import subprocess
import time
import psutil

if os.name=='nt':
    from ctypes import wintypes as W
    U=ctypes.c_size_t
    class Basic(ctypes.Structure):
        _fields_=[('process_time',ctypes.c_int64),('job_time',ctypes.c_int64),('flags',W.DWORD),
                  ('min_ws',U),('max_ws',U),('active',W.DWORD),('affinity',U),('priority',W.DWORD),('schedule',W.DWORD)]
    class IO(ctypes.Structure):
        _fields_=[(x,ctypes.c_uint64) for x in ('read_ops','write_ops','other_ops','read_bytes','write_bytes','other_bytes')]
    class Extended(ctypes.Structure):
        _fields_=[('basic',Basic),('io',IO),('process_memory',U),('job_memory',U),('peak_process',U),('peak_job',U)]
    class Accounting(ctypes.Structure):
        _fields_=[('user',ctypes.c_int64),('kernel',ctypes.c_int64),('user_period',ctypes.c_int64),('kernel_period',ctypes.c_int64),
                  ('faults',W.DWORD),('total',W.DWORD),('active',W.DWORD),('terminated',W.DWORD)]
    K=ctypes.WinDLL('kernel32',use_last_error=True)
    K.CreateJobObjectW.argtypes=[ctypes.c_void_p,W.LPCWSTR]; K.CreateJobObjectW.restype=W.HANDLE
    K.SetInformationJobObject.argtypes=[W.HANDLE,ctypes.c_int,ctypes.c_void_p,W.DWORD]
    K.AssignProcessToJobObject.argtypes=[W.HANDLE,W.HANDLE]
    K.QueryInformationJobObject.argtypes=[W.HANDLE,ctypes.c_int,ctypes.c_void_p,W.DWORD,ctypes.c_void_p]
    K.TerminateJobObject.argtypes=[W.HANDLE,W.UINT]; K.CloseHandle.argtypes=[W.HANDLE]


class ManagedProcess:
    def __init__(self,command,stdout,stderr,memory_mib=2048,cpu=None,cpu_count=1):
        if os.name!='nt':
            raise NotImplementedError('Formal memory enforcement and accounting currently require Windows Job Objects; Linux/Docker runs are unsupported')
        self.started=time.perf_counter(); self.started_unix_ms=time.time()*1000; self.job=None; self.peak_rss=0; self.sampled_cpu=0
        self.cpus=[cpu] if cpu is not None else psutil.Process().cpu_affinity()[:cpu_count]
        if os.name=='nt':
            self.job=K.CreateJobObjectW(None,None)
            if not self.job: raise ctypes.WinError(ctypes.get_last_error())
            limits=Extended(); limits.basic.flags=0x2000 | 0x200 | 0x10  # KILL_ON_CLOSE, JOB_MEMORY, AFFINITY
            limits.basic.affinity=sum(1<<c for c in self.cpus); limits.job_memory=memory_mib*1024*1024
            if not K.SetInformationJobObject(self.job,9,ctypes.byref(limits),ctypes.sizeof(limits)):
                K.CloseHandle(self.job); raise ctypes.WinError(ctypes.get_last_error())
            flags=subprocess.CREATE_NO_WINDOW
        else: flags=0
        try:
            self.process=subprocess.Popen(command,stdout=stdout,stderr=stderr,creationflags=flags,start_new_session=os.name!='nt')
        except BaseException:
            self.close(); raise
        try:
            if os.name=='nt' and not K.AssignProcessToJobObject(self.job,int(self.process._handle)):
                raise ctypes.WinError(ctypes.get_last_error())
            psutil.Process(self.process.pid).cpu_affinity(self.cpus)
        except BaseException:
            self.process.kill(); self.process.wait(); self.close(); raise
        self.seen_cpu={}

    def sample(self):
        rss=0
        try:
            root=psutil.Process(self.process.pid); tree=[root]+root.children(recursive=True)
            for p in tree:
                try:
                    rss+=p.memory_info().rss; cpu=p.cpu_times(); self.seen_cpu[p.pid]=cpu.user+cpu.system
                except psutil.Error: pass
        except psutil.Error: pass
        self.peak_rss=max(self.peak_rss,rss); self.sampled_cpu=sum(self.seen_cpu.values())
        return rss

    def accounting(self):
        if self.job:
            a=Accounting(); e=Extended()
            if not K.QueryInformationJobObject(self.job,1,ctypes.byref(a),ctypes.sizeof(a),None): raise ctypes.WinError(ctypes.get_last_error())
            if not K.QueryInformationJobObject(self.job,9,ctypes.byref(e),ctypes.sizeof(e),None): raise ctypes.WinError(ctypes.get_last_error())
            return {'cpu_seconds':(a.user+a.kernel)/1e7,'peak_tree_commit_bytes':e.peak_job,
                    'cpu_method':'Windows Job Object user+kernel (entire tree)', 'active_processes':a.active}
        return {'cpu_seconds':self.sampled_cpu,'peak_tree_commit_bytes':None,
                'cpu_method':'psutil process-tree sampled lower bound','active_processes':None}

    def terminate(self):
        if self.job: K.TerminateJobObject(self.job,1)
        else:
            try: os.killpg(self.process.pid,signal.SIGKILL)
            except ProcessLookupError: pass
        self.process.wait(timeout=10)

    def close(self):
        if self.job:
            K.CloseHandle(self.job); self.job=None


def run_process(command,out,err,timeout,memory_mib=2048,cancel_file=None,cpu_budget=None,cpu_count=1):
    """Return failures explicitly; caller owns immutable attempt directories."""
    with open(out,'wb') as stdout,open(err,'wb') as stderr:
        child=ManagedProcess(command,stdout,stderr,memory_mib,cpu_count=cpu_count)
        status='succeeded'
        try:
            while child.process.poll() is None:
                child.sample()
                if cancel_file and cancel_file.exists(): status='cancelled'; break
                if time.perf_counter()-child.started>timeout: status='timeout'; break
                if cpu_budget and child.accounting()['cpu_seconds']>cpu_budget: status='cpu_limit'; break
                time.sleep(.02)
            if status!='succeeded': child.terminate()
            else: child.process.wait()
            child.sample(); counters=child.accounting()
            if status=='succeeded' and child.process.returncode: status='error'
            return {'status':status,'exit_code':child.process.returncode,'wall_ms':(time.perf_counter()-child.started)*1000,
                    'process_spawn_unix_ms':child.started_unix_ms,
                    'peak_tree_rss_sampled_bytes':child.peak_rss,'rss_sample_interval_ms':20,**counters}
        finally:
            # A descendant that outlives its parent must be killed before resources are released.
            child.terminate(); child.close()
