#!/usr/bin/env python3
"""Controlled local subprocesses only: never starts a VM, D-Bus or tracefs."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import perf_operation_budget as budget
import perf_trace_diagnostics as diagnostics
ROOT=Path(__file__).resolve().parents[2]

def load(name,path):
    loader=importlib.machinery.SourceFileLoader(name,str(path))
    spec=importlib.util.spec_from_loader(name,loader);module=importlib.util.module_from_spec(spec)
    loader.exec_module(module);return module

class BudgetTests(unittest.TestCase):
    def test_shared_remaining(self):
        clock=[0.0];work=budget.Budget(20,lambda:clock[0])
        clock[0]=8;self.assertEqual(work.remaining(15),12)
        clock[0]=19;self.assertEqual(work.remaining(12),1)
        clock[0]=20
        with self.assertRaises(TimeoutError):work.remaining(5)
        cleanup=budget.Budget(35,lambda:clock[0]);self.assertEqual(cleanup.remaining(15),15)
        clock[0]=34;self.assertEqual(cleanup.remaining(15),1)
        clock[0]=35
        with self.assertRaises(TimeoutError):cleanup.remaining(15)
    def test_invalid_deadlines(self):
        for value in (float('inf'),float('nan'),'20',True):
            with self.assertRaises(ValueError):budget.Budget(value)
    def test_success(self):
        self.assertEqual(budget.supervise(lambda:{'valid':True},lambda:False,work_seconds=1,cleanup_seconds=.5),{'valid':True})
    def test_remaining_budget_reaches_worker_and_profiler_environment(self):
        def work():
            time.sleep(.02)
            return {'remaining':budget.remaining(20),'environment':budget.profiler_environment()}
        result=budget.supervise(work,lambda:False,work_seconds=1,cleanup_seconds=.5)
        self.assertGreater(result['remaining'],0)
        self.assertLess(result['remaining'],1)
        self.assertTrue(result['environment'][0].startswith('TUNA_TRACE_WORK_DEADLINE='))
    def test_inner_stage_timeout_is_preserved(self):
        def work():
            budget.stage('kernel-start')
            raise subprocess.TimeoutExpired(['fixed'],.01)
        with self.assertRaises(budget.OperationFailure) as caught:
            budget.supervise(work,lambda:False,work_seconds=1,cleanup_seconds=.5)
        self.assertEqual(caught.exception.receipt['trace_failure']['outcome'],'expired')
        self.assertEqual(caught.exception.receipt['trace_failure']['stage'],'kernel-start')
    def test_hung_provenance(self):
        before=time.monotonic()
        with self.assertRaises(budget.OperationFailure) as caught:
            budget.supervise(lambda:time.sleep(60),lambda:False,work_seconds=.05,cleanup_seconds=.5)
        row=caught.exception.receipt['trace_failure']
        self.assertLess(time.monotonic()-before,1)
        self.assertEqual(row['stage'],'provenance');self.assertEqual(row['kernel_state'],'not-entered')
        self.assertEqual(row['helper_retirement'],'confirmed');self.assertEqual(row['outcome'],'expired')
    def test_kernel_entry_without_marker_is_not_untouched(self):
        def work():budget.stage('kernel-start');time.sleep(60)
        with self.assertRaises(budget.OperationFailure) as caught:
            budget.supervise(work,lambda:False,work_seconds=.05,cleanup_seconds=.5)
        self.assertEqual(caught.exception.receipt['trace_failure']['kernel_state'],'entered-without-marker')
    def test_subprocess_group_retirement_and_detached_effect_are_distinct(self):
        # A separately owned process represents an already-accepted operation;
        # killing the helper group must neither kill it nor claim its completion.
        effect=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],start_new_session=True)
        try:
            def work():
                budget.stage('kernel-start');budget.stage('profiler')
                subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
                time.sleep(60)
            with self.assertRaises(budget.OperationFailure) as caught:
                # This is a real-OS ownership integration test, not a claim
                # that a loaded host schedules every departure within 0.5s.
                budget.supervise(work,lambda:True,work_seconds=.1,cleanup_seconds=budget.CLEANUP_SECONDS)
            row=caught.exception.receipt['trace_failure']
            self.assertEqual(row['start_acceptance'],'uncertain')
            self.assertTrue(row['vm_retirement_required']);self.assertIsNone(effect.poll())
            self.assertEqual(row['helper_retirement'],'confirmed')
        finally:effect.kill();effect.wait(timeout=2)
    def test_deadline_drains_queued_stage_after_termination(self):
        with tempfile.TemporaryDirectory() as temp:
            entered=Path(temp)/'entered'
            def work():
                budget.stage('kernel-start');budget.stage('profiler')
                entered.write_text('ready')
                time.sleep(60)
            def delayed_select(*args):
                end=time.monotonic()+1
                while not entered.exists() and time.monotonic()<end:time.sleep(.001)
                time.sleep(.08)
                return [],[],[]
            with patch.object(budget.select,'select',side_effect=delayed_select):
                with self.assertRaises(budget.OperationFailure) as caught:
                    budget.supervise(work,lambda:True,work_seconds=.06,cleanup_seconds=.5)
            row=caught.exception.receipt['trace_failure']
            self.assertEqual(row['stage'],'profiler')
            self.assertEqual(row['kernel_state'],'entered-with-marker')
            self.assertEqual(row['start_acceptance'],'uncertain')
    def test_rechecks_leader_group_after_presetsid_observation(self):
        with tempfile.TemporaryDirectory() as temp:
            child_file=Path(temp)/'child'
            def work():
                child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
                child_file.write_text(str(child.pid))
                raise RuntimeError('controlled failure')
            original_getpgid=os.getpgid
            queried=[]
            def race_getpgid(pid):
                queried.append(pid)
                return os.getpgrp() if len(queried)==1 else original_getpgid(pid)
            def delayed_select(*args):
                end=time.monotonic()+1
                while not child_file.exists() and time.monotonic()<end:time.sleep(.001)
                time.sleep(.03)
                return [],[],[]
            child_pidfd=None
            try:
                with patch.object(budget.os,'getpgid',side_effect=race_getpgid),patch.object(budget.select,'select',side_effect=delayed_select):
                    with self.assertRaises(budget.OperationFailure) as caught:
                        budget.supervise(work,lambda:False,work_seconds=1,cleanup_seconds=.5)
                self.assertGreaterEqual(len(queried),2)
                self.assertEqual(caught.exception.receipt['trace_failure']['helper_retirement'],'confirmed')
                child_pid=int(child_file.read_text())
                try:
                    child_pidfd=os.pidfd_open(child_pid)
                    # pidfd readiness denotes actual task departure, including a
                    # zombie awaiting its init/subreaper, rather than kill intent.
                    self.assertTrue(budget.select.select([child_pidfd],[],[],1)[0])
                except ProcessLookupError:pass
            finally:
                if child_pidfd is not None:
                    try:signal.pidfd_send_signal(child_pidfd,signal.SIGKILL)
                    except ProcessLookupError:pass
                    os.close(child_pidfd)
    def test_lost_progress_is_conservative(self):
        # A marker contradicts a purported provenance-only observation.
        with self.assertRaises(budget.OperationFailure) as caught:
            budget.supervise(lambda:time.sleep(60),lambda:True,work_seconds=.03,cleanup_seconds=.5)
        row=caught.exception.receipt['trace_failure']
        self.assertEqual(row['stage'],'unknown');self.assertEqual(row['kernel_state'],'uncertain')
        self.assertEqual(row['start_acceptance'],'uncertain')
        diagnostics.encode(caught.exception.receipt,'start')
    def test_late_observation_never_confirms_cleanup(self):
        # Advance a logical clock only after the real SIGKILL. No wall-clock
        # sleep or timeout increase is needed to reproduce missed evidence.
        original_waitid=os.waitid;original_killpg=os.killpg;original_kill=os.kill
        clock_offset=[0.0];signaled=[False];advanced=[False];owned={}
        def remember(pid):
            if not owned:owned.update(pid=pid,pidfd=os.pidfd_open(pid))
        def killpg(pid,sig):
            remember(pid);result=original_killpg(pid,sig);signaled[0]=True;return result
        def kill(pid,sig):
            remember(pid);result=original_kill(pid,sig);signaled[0]=True;return result
        def waitid(*args):
            if signaled[0] and not advanced[0]:
                clock_offset[0]+=.51;advanced[0]=True
            return original_waitid(*args)
        clock=SimpleNamespace(monotonic=lambda:time.monotonic()+clock_offset[0],sleep=time.sleep)
        try:
            with patch.object(budget,'time',clock),patch.object(budget.os,'killpg',side_effect=killpg), \
                 patch.object(budget.os,'kill',side_effect=kill),patch.object(budget.os,'waitid',side_effect=waitid):
                with self.assertRaises(budget.OperationFailure) as caught:
                    budget.supervise(lambda:time.sleep(60),lambda:False,work_seconds=.03,cleanup_seconds=.5)
            row=caught.exception.receipt['trace_failure']
            self.assertTrue(advanced[0])
            self.assertEqual(row['cleanup'],'expired')
            self.assertEqual(row['helper_retirement'],'unconfirmed')
            self.assertTrue(row['vm_retirement_required'])
            # Later identity-bound departure is useful cleanup evidence only;
            # it must not rewrite the within-budget refusal above.
            self.assertTrue(budget.select.select([owned['pidfd']],[],[],2)[0])
            self.assertEqual(row['helper_retirement'],'unconfirmed')
        finally:
            if owned:
                try:signal.pidfd_send_signal(owned['pidfd'],signal.SIGKILL)
                except ProcessLookupError:pass
                if budget.select.select([owned['pidfd']],[],[],2)[0]:
                    try:os.waitpid(owned['pid'],os.WNOHANG)
                    except ChildProcessError:pass
                os.close(owned['pidfd'])
    def test_cleanup_expiry_is_failure(self):
        with patch.object(budget,'group_departed',return_value=False):
            with self.assertRaises(budget.OperationFailure) as caught:
                budget.supervise(lambda:time.sleep(60),lambda:False,work_seconds=.03,cleanup_seconds=.03)
        self.assertEqual(caught.exception.receipt['trace_failure']['cleanup'],'expired')
        self.assertEqual(caught.exception.receipt['trace_failure']['helper_retirement'],'unconfirmed')
    def test_failure_schema_and_old_reader_fail_closed(self):
        value=budget.receipt('profiler','expired',True,True,False)
        self.assertEqual(diagnostics.decode(diagnostics.encode(value,'start'),'start'),value)
        # Frozen actual v1 decoder: independent of checkout HEAD and Git history.
        old=load('frozen_trace_diagnostics_v1',ROOT/'scripts/lib/fixtures/perf_trace_diagnostics_v1.py')
        v1={'trace_failure':{'schema':1,'action':'start','stage':'owner-name','error_class':'RuntimeError'}}
        self.assertEqual(old.decode(json.dumps(v1),'start'),v1)
        self.assertEqual(diagnostics.decode(json.dumps(v1),'start'),v1)
        with self.assertRaises(ValueError):old.decode(json.dumps(value),'start')
        for field,item in [('schema',True),('performance_qualified',True),('vm_retirement_required',False),
                           ('cleanup','done'),('start_acceptance','confirmed'),('outcome','success')]:
            invalid=json.loads(json.dumps(value));invalid['trace_failure'][field]=item
            with self.assertRaises(ValueError):diagnostics.encode(invalid,'start')
    def test_failure_payload_bound_and_unknown_fields(self):
        value=budget.receipt('provenance','failed',True,False,False)
        value['trace_failure']['private_text']='not allowed'
        with self.assertRaises(ValueError):diagnostics.encode(value,'start')
    def test_host_admission_refuses_before_exec(self):
        host=load('operation_host',ROOT/'scripts/tuna-vm-perf')
        class Agent:
            def command(self,*args,**kwargs):raise AssertionError('must refuse before execution')
        with self.assertRaisesRegex(RuntimeError,'original live disposable'):
            host.guest_probe(Agent(),'gnome-trace-start',start_owner=object())
    def test_host_poll_failures_retain_original_pid_without_retry(self):
        host=load('operation_host_failures',ROOT/'scripts/tuna-vm-perf')
        from perf_vm_lifetime import Handoff
        child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
        try:
            for failure in (TimeoutError('private timeout'),ConnectionError('private disconnect'),
                            RuntimeError('invalid envelope'),{},'malformed',{'exited':1}):
                with self.subTest(failure=failure),tempfile.TemporaryDirectory() as temp:
                    owner=Handoff(Path(temp),'diagnostic','gnome',1);owner.attach(child)
                    calls=[]
                    class Agent:
                        def command(self,name,**kwargs):
                            calls.append((name,kwargs))
                            if name=='guest-exec':return {'pid':123}
                            if isinstance(failure,Exception):raise failure
                            return failure
                    try:
                        with self.assertRaises((TimeoutError,ConnectionError,RuntimeError,ValueError)):
                            host.guest_probe(Agent(),'gnome-trace-start',out=Path(temp),start_owner=owner)
                        self.assertEqual([row[0] for row in calls],['guest-exec','guest-exec-status'])
                        retained=json.loads((Path(temp)/'gnome-trace-start-unobserved.json').read_text())
                        self.assertEqual(retained['guest_pid'],123)
                        self.assertFalse(retained['terminal_observed'])
                        self.assertTrue(retained['vm_retirement_required'])
                        self.assertNotIn('private',json.dumps(retained))
                    finally:owner.close()
        finally:child.kill();child.wait(timeout=2)
    def test_host_budget_observes_original_pid_only(self):
        host=load('operation_host2',ROOT/'scripts/tuna-vm-perf')
        from perf_vm_lifetime import Handoff
        child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
        try:
            with tempfile.TemporaryDirectory() as temp:
                owner=Handoff(Path(temp),'diagnostic','gnome',1);owner.attach(child)
                calls=[]
                class Agent:
                    def command(self,name,**kwargs):
                        calls.append((name,kwargs))
                        if name=='guest-exec':return {'pid':123}
                        return {'exited':False}
                clock=iter((100,100,100,136))
                with patch.object(host.time,'monotonic',side_effect=lambda:next(clock)),patch.object(host.time,'sleep'):
                    with self.assertRaisesRegex(RuntimeError,'timed out'):
                        host.guest_probe(Agent(),'gnome-trace-start',out=Path(temp),start_owner=owner)
                self.assertEqual([c[0] for c in calls],['guest-exec','guest-exec-status'])
                self.assertEqual(calls[0][1]['arg'],['gnome-trace-start','--supervised-start'])
                self.assertEqual(calls[1][1],{'pid':123,'deadline':135})
                retained=json.loads((Path(temp)/'gnome-trace-start-unobserved.json').read_text())
                self.assertEqual(retained['guest_pid'],123);self.assertFalse(retained['terminal_observed'])
                owner.close()
        finally:child.kill();child.wait(timeout=2)

if __name__=='__main__':unittest.main()
