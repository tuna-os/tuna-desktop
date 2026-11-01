#!/usr/bin/env python3
"""Actual wire schema parser negative cases; no fake runtime provider."""
import ast
from pathlib import Path
import re
import os
import stat
import json
import tempfile
import time
from types import SimpleNamespace
from unittest import mock
import unittest
import xml.etree.ElementTree as ET

ROOT=Path(__file__).resolve().parents[2]
parsed=ast.parse((ROOT/'scripts/lib/gnome-brightness-proof.py').read_text())
namespace={'ET':ET,'NAME':'org.gnome.Shell.Brightness','os':os,'stat':stat,'json':json,'Path':Path,'time':time,'PATH':'/org/gnome/Shell/Brightness','GLib':SimpleNamespace(Variant=lambda signature,args:args)}
function=next(node for node in parsed.body if isinstance(node,ast.FunctionDef) and node.name=='schema')
exec(compile(ast.Module(body=[function],type_ignores=[]),'<actual-schema-parser>','exec'),namespace)
schema=namespace['schema']
for name in ['scalar','retain','process','principal','available','signal_receipt','drain','floor','controlled_manifest','backlight_type','overridden_backlight_environment']:
    node=next(node for node in parsed.body if isinstance(node,ast.FunctionDef) and node.name==name)
    exec(compile(ast.Module(body=[node],type_ignores=[]),'<actual-bounded-proof-io>','exec'),namespace)
scalar=namespace['scalar'];retain=namespace['retain']
XML=re.search(r'const XML: &str = r#"(.*?)"#;', (ROOT/'crates/shell-gtk/src/brightness_service.rs').read_text(),re.S).group(1)

class Contract(unittest.TestCase):
    def test_current_service_schema_matches_gnome51_wire(self):schema(XML)
    def test_changed_property_type_access_or_name_rejected(self):
        for before,after in [('type="b" access="read"','type="i" access="read"'),('access="read"','access="readwrite"'),('HasBrightnessControl','Brightness')]:
            with self.subTest(after=after),self.assertRaises(RuntimeError):schema(XML.replace(before,after))
    def test_wrong_method_argument_direction_or_missing_method_rejected(self):
        for before,after in [('type="d"','type="i"'),('direction="in"','direction="out"'),('SetDimming','OtherMethod')]:
            with self.subTest(after=after),self.assertRaises(RuntimeError):schema(XML.replace(before,after))
    def test_signal_payload_duplicate_method_or_extra_interface_rejected(self):
        for addition in ['<method name="SetDimming"><arg type="b" direction="in"/></method>',
                         '<signal name="OtherSignal"/>']:
            with self.subTest(addition=addition),self.assertRaises(RuntimeError):schema(XML.replace('</interface>',addition+'</interface>'))
        with self.assertRaises(RuntimeError):schema(XML.replace('<signal name="BrightnessChanged"/>','<signal name="BrightnessChanged"><arg type="i"/></signal>'))
        with self.assertRaises(RuntimeError):schema('<node>'+XML+XML+'</node>')
    def test_missing_interface_and_oversized_introspection_rejected(self):
        with self.assertRaises(RuntimeError):schema('<node/>')
        with self.assertRaises(RuntimeError):schema(' '*65537)

class ActualBoundedIO(unittest.TestCase):
    def test_scalar_bounds_numeric_range_and_nonregular(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'scalar'
            path.write_text('4294967295');self.assertEqual(scalar(path),4294967295)
            for value in ['1'*65,'-1','4294967296','NaN','1 2']:
                path.write_text(value)
                with self.subTest(value=value),self.assertRaises((RuntimeError,ValueError)):scalar(path)
            link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):scalar(link)
            fifo=Path(directory)/'fifo';os.mkfifo(fifo)
            with self.assertRaises(RuntimeError):scalar(fifo)
    def test_scalar_permission_failure_propagates(self):
        with mock.patch.object(os,'open',side_effect=PermissionError('actual open refusal')):
            with self.assertRaises(PermissionError):scalar('/not-authorized')
    def test_replaced_scalar_identity_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'scalar';path.write_text('505')
            replacement=Path(directory)/'other';replacement.write_text('700')
            actual_read=os.read
            def replace_after_read(fd,count):
                value=actual_read(fd,count);os.replace(replacement,path);return value
            with mock.patch.object(os,'read',side_effect=replace_after_read),self.assertRaises(RuntimeError):scalar(path)
    def test_receipt_exclusive_bounded_and_does_not_replace_existing(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt'
            retain(path,{'pass':False});self.assertEqual(json.loads(path.read_text()),{'pass':False})
            with self.assertRaises(FileExistsError):retain(path,{'pass':True})
            self.assertFalse(json.loads(path.read_text())['pass'])
            other=Path(directory)/'too-large'
            with self.assertRaises(RuntimeError):retain(other,{'data':'x'*1048576})
            self.assertFalse(other.exists())
    def test_receipt_named_replacement_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt';replacement=Path(directory)/'replacement';replacement.write_text('{}')
            actual_stat=os.stat
            def replaced(name,**kwargs):
                os.replace(replacement,path);return actual_stat(name,**kwargs)
            with mock.patch.object(os,'stat',side_effect=replaced),self.assertRaises(RuntimeError):retain(path,{'pass':False})

class OriginalOwner(unittest.TestCase):
    def test_capability_call_targets_original_unique_owner(self):
        calls=[]
        def call(bus,dest,*args):
            calls.append(dest);return SimpleNamespace(unpack=lambda:(False,))
        with mock.patch.dict(namespace,{'call':call}):
            self.assertFalse(namespace['available'](object(),':1.42'))
        self.assertEqual(calls,[':1.42'])
    def test_invalid_process_wrong_executable_and_root_refused(self):
        for pid in [0,-1]:
            with self.assertRaises(RuntimeError):namespace['process'](pid,'/usr/bin/tuna-shell-gtk')
        with self.assertRaises(RuntimeError):namespace['process'](os.getpid(),'/definitely-not-the-original-shell')
        with mock.patch.object(Path,'stat',return_value=SimpleNamespace(st_uid=0)), mock.patch.object(os,'readlink',return_value='/usr/bin/tuna-shell-gtk'):
            with self.assertRaises(RuntimeError):namespace['process'](os.getpid(),'/usr/bin/tuna-shell-gtk')
    def test_replaced_bus_owner_refused_before_process_observation(self):
        def call(bus,dest,path,interface,method,args):
            return SimpleNamespace(unpack=lambda:(':1.42' if args[0]==namespace['NAME'] else ':1.43',))
        with mock.patch.dict(namespace,{'call':call,'GLib':SimpleNamespace(Variant=lambda signature,args:args)}):
            with self.assertRaises(RuntimeError):namespace['principal'](object(),'/usr/bin/tuna-shell-gtk')
    def test_native_original_start_or_compositor_identity_mismatch_refused(self):
        shell=dict(pid=10,uid=1000,start_ticks=20,executable='/usr/bin/tuna-shell-gtk',parent_pid=11)
        compositor=dict(pid=11,uid=1000,start_ticks=21,executable='/usr/bin/tuna-compositor',parent_pid=1)
        def call(bus,dest,path,interface,method,args):
            value=10 if method=='GetConnectionUnixProcessID' else 1000 if method=='GetConnectionUnixUser' else ':1.42'
            return SimpleNamespace(unpack=lambda:(value,))
        def process(pid,exe):return shell if pid==10 else compositor
        with mock.patch.dict(namespace,{'call':call,'process':process,'GLib':SimpleNamespace(Variant=lambda signature,args:args)}), mock.patch.object(os,'readlink',return_value='/usr/bin/tuna-compositor'):
            observed=namespace['principal'](object(),shell['executable'])
            self.assertEqual(observed['compositor'],compositor)
            for expected in [{'start_ticks':19},{'pid':9},{'compositor':dict(compositor,start_ticks=22)}]:
                with self.subTest(expected=expected),self.assertRaises(RuntimeError):namespace['principal'](object(),shell['executable'],expected)

class BoundedSignalDispatch(unittest.TestCase):
    def test_original_signal_overflow_retained_without_callback_exception(self):
        signals=[];overflow=[False]
        for _ in range(513):namespace['signal_receipt'](signals,overflow)
        self.assertEqual(len(signals),512);self.assertTrue(overflow[0])
    def test_continuous_pending_queue_refused_at_fixed_iteration_bound(self):
        iterations=[];context=SimpleNamespace(pending=lambda:True,iteration=lambda block:iterations.append(block))
        with self.assertRaises(RuntimeError):namespace['drain'](context,time.monotonic()+5,[100])
        self.assertEqual(len(iterations),100);self.assertEqual(set(iterations),{False})
    def test_deadline_refuses_dispatch_and_finite_queue_preserves_remaining_budget(self):
        iterations=[];context=SimpleNamespace(pending=lambda:len(iterations)<2,iteration=lambda block:iterations.append(block))
        with self.assertRaises(RuntimeError):namespace['drain'](context,time.monotonic()-1,[100])
        self.assertEqual(iterations,[])
        budget=[100];namespace['drain'](context,time.monotonic()+5,budget)
        self.assertEqual((len(iterations),budget[0]),(2,98))

class ActualControlledAuthority(unittest.TestCase):
    def test_native_environment_rejects_even_empty_fixture_overrides(self):
        with mock.patch.dict(os.environ,{},clear=True):
            self.assertFalse(namespace['overridden_backlight_environment']())
            for key in ('TUNA_BACKLIGHT_ROOT','TUNA_BACKLIGHT_TEST_CONNECTORS'):
                for value in ('','/controlled/fixture'):
                    with mock.patch.dict(os.environ,{key:value},clear=True):
                        self.assertTrue(namespace['overridden_backlight_environment']())
    def test_real_type_floor_exact_raw_boundary_and_other_kinds(self):
        for kind in ['firmware','platform']:
            self.assertEqual(namespace['floor'](2,kind),1)
        for maximum,expected in [(1,0),(2,0),(98,0),(99,1),(1000,10)]:self.assertEqual(namespace['floor'](maximum,'raw'),expected)
        for kind in ['','unknown','RAW']:
            with self.assertRaises(RuntimeError):namespace['floor'](98,kind)
    def test_actual_type_io_rejects_unknown_oversize_fifo_and_permission(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'type'
            for value in ['raw','firmware','platform']:
                path.write_text(value);self.assertEqual(namespace['backlight_type'](path),value)
            for value in ['unknown','raw platform','raw'+'x'*14]:
                path.write_text(value)
                with self.assertRaises(RuntimeError):namespace['backlight_type'](path)
            fifo=Path(directory)/'fifo';os.mkfifo(fifo)
            with self.assertRaises(RuntimeError):namespace['backlight_type'](fifo)
        with mock.patch.object(os,'open',side_effect=PermissionError):
            with self.assertRaises(PermissionError):namespace['backlight_type']('/refused')
    def test_manifest_fd_bounds_regular_no_follow_and_count(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'manifest';path.write_text('[]');self.assertEqual(namespace['controlled_manifest'](path),[])
            for raw in [' '*65537,json.dumps([{}]*65),'{}',json.dumps([{'name':'unowned'}])]:
                path.write_text(raw)
                with self.assertRaises(RuntimeError):namespace['controlled_manifest'](path)
            link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):namespace['controlled_manifest'](link)
            fifo=Path(directory)/'fifo';os.mkfifo(fifo)
            with self.assertRaises(RuntimeError):namespace['controlled_manifest'](fifo)
    def test_manifest_replacement_and_permission_refusal_are_failures(self):
        with mock.patch.object(os,'open',side_effect=PermissionError):
            with self.assertRaises(PermissionError):namespace['controlled_manifest']('/refused')
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'manifest';path.write_text('[]');replacement=Path(directory)/'new';replacement.write_text('[]')
            read=os.read
            def replace(fd,count):
                raw=read(fd,count);os.replace(replacement,path);return raw
            with mock.patch.object(os,'read',side_effect=replace),self.assertRaises(RuntimeError):namespace['controlled_manifest'](path)

class FailedRequestReceipt(unittest.TestCase):
    def request(self,error=None):
        class WireError(Exception):
            domain='wire';code=1
        errors=[];calls=[];clock=[0]
        def monotonic():
            clock[0]+=1
            return clock[0]
        def call(*args):
            calls.append(args)
            if error is not None:raise WireError(error)
        scope=dict(namespace,receipts=errors,signals=[],signal_overflow=[False],bus=object(),owner=':1.42',
                   call=call,available=lambda *args:True,principal=lambda *args:'original',identity='original',
                   args=SimpleNamespace(shell_executable='/actual/shell'),
                   time=SimpleNamespace(monotonic=monotonic,monotonic_ns=lambda:100),
                   GLib=SimpleNamespace(Error=WireError,Variant=lambda signature,args:args,
                                        MainContext=SimpleNamespace(default=lambda:object())),
                   Gio=SimpleNamespace(DBusError=SimpleNamespace(get_remote_error=lambda error:'org.gnome.Shell.Brightness.Error.Failed')))
        node=next(node for node in ast.walk(parsed) if isinstance(node,ast.FunctionDef) and node.name=='request')
        exec(compile(ast.Module(body=[node],type_ignores=[]),'<actual-request-receipt>','exec'),scope)
        return scope['request'],errors,calls
    def test_unexpected_failure_is_retained_before_refusal_without_retry(self):
        request,receipts,calls=self.request('brightness update already pending')
        with self.assertRaises(RuntimeError):request('SetDimming',False)
        self.assertEqual(len(calls),1);self.assertEqual(len(receipts),1)
        self.assertEqual(receipts[0]['error_detail'],'brightness update already pending')
        self.assertFalse(receipts[0]['value']);self.assertFalse(receipts[0]['expected_failure'])
        self.assertNotIn('ended_ns',receipts[0])
    def test_error_detail_is_bounded(self):
        request,receipts,_=self.request('x'*1024)
        with self.assertRaises(RuntimeError):request('SetDimming',False)
        self.assertEqual(len(receipts[0]['error_detail']),512)
    def test_missing_expected_failure_still_refuses_and_retains_actual_success(self):
        request,receipts,calls=self.request()
        with self.assertRaises(RuntimeError):request('SetDimming',True,True)
        self.assertIsNone(receipts[0]['error']);self.assertTrue(receipts[0]['expected_failure'])
        self.assertEqual(len(calls),1)
    def test_expected_failure_and_success_keep_final_signal_receipt(self):
        for error in (None,'controlled denial'):
            request,receipts,calls=self.request(error)
            request('SetDimming',True,error is not None)
            self.assertEqual(len(calls),1);self.assertEqual(len(receipts),1)
            self.assertEqual(receipts[0]['user_signals_after'],0)
            self.assertTrue(receipts[0]['after_capability'])
            self.assertIn('ended_ns',receipts[0])

class IdleRestoreAdmission(unittest.TestCase):
    class WireError(Exception):
        def __init__(self,message='brightness update already pending',remote='org.gnome.Shell.Brightness.Error.Failed'):
            self.remote=remote
            self.message=f'GDBus.Error:{remote}: {message}'
            super().__init__(self.message)

    def helper(self,clock=lambda:0,sleep=lambda seconds:None):
        scope=dict(namespace,time=SimpleNamespace(monotonic=clock,monotonic_ns=lambda:100,sleep=sleep),
                   GLib=SimpleNamespace(Error=self.WireError),
                   Gio=SimpleNamespace(DBusError=SimpleNamespace(get_remote_error=lambda error:error.remote)))
        node=next(node for node in parsed.body if isinstance(node,ast.FunctionDef) and node.name=='idle_restore_call')
        exec(compile(ast.Module(body=[node],type_ignores=[]),'<actual-idle-restore-admission>','exec'),scope)
        return scope['idle_restore_call']

    def test_busy_then_success_retains_refusal_without_duplicate_acceptance(self):
        invoke=mock.Mock(side_effect=[self.WireError(), 'accepted'])
        verify=mock.Mock();attempts=[]
        self.assertEqual(self.helper()(invoke,verify,attempts),'accepted')
        self.assertEqual(invoke.call_count,2)
        self.assertEqual(verify.call_count,3)
        self.assertTrue(attempts[0]['busy_refused'])
        self.assertIsNone(attempts[1]['error'])
        self.assertEqual(sum(row['error'] is None for row in attempts),1)

    def test_permanent_busy_is_bounded_by_attempts_and_shared_deadline(self):
        invoke=mock.Mock(side_effect=self.WireError());attempts=[]
        with self.assertRaisesRegex(RuntimeError,'within bounds'):
            self.helper()(invoke,lambda deadline:None,attempts)
        self.assertEqual(invoke.call_count,100)
        self.assertEqual(len(attempts),100)
        clock=[0];invoke.reset_mock();attempts=[]
        def expires(timeout):
            clock[0]=5
            raise self.WireError()
        invoke.side_effect=expires
        with self.assertRaisesRegex(RuntimeError,'within bounds'):
            self.helper(lambda:clock[0])(invoke,lambda deadline:None,attempts)
        self.assertEqual(invoke.call_count,1)

    def test_busy_admissions_are_paced_until_completion(self):
        clock=[0.0];sleeps=[];attempts=[];checks=[]
        def sleep(seconds):
            sleeps.append(seconds);clock[0]+=seconds
        def invoke(timeout):
            self.assertGreater(timeout,0)
            if clock[0]<0.15:raise self.WireError()
            return 'accepted'
        value=self.helper(lambda:clock[0],sleep)(invoke,checks.append,attempts)
        self.assertEqual(value,'accepted')
        self.assertTrue(all(0<delay<=0.05 for delay in sleeps))
        self.assertGreaterEqual(clock[0],0.15)
        self.assertLessEqual(clock[0],0.20)
        self.assertTrue(all(row['busy_refused'] for row in attempts[:-1]))
        self.assertIsNone(attempts[-1]['error'])
        self.assertEqual(set(checks),{5.0})

    def test_busy_sleep_is_clamped_to_original_deadline(self):
        clock=[0.0];sleeps=[];attempts=[]
        def sleep(seconds):
            sleeps.append(seconds);clock[0]+=seconds
        def busy(timeout):
            clock[0]=4.99
            raise self.WireError()
        invoke=mock.Mock(side_effect=busy)
        with self.assertRaisesRegex(RuntimeError,'within bounds'):
            self.helper(lambda:clock[0],sleep)(invoke,lambda deadline:None,attempts)
        invoke.assert_called_once()
        self.assertEqual(len(sleeps),1)
        self.assertAlmostEqual(sleeps[0],0.01)
        self.assertEqual(clock[0],5.0)

    def test_persistent_busy_uses_deadline_without_hammering_admission(self):
        clock=[0.0];sleeps=[];attempts=[]
        def sleep(seconds):
            sleeps.append(seconds);clock[0]+=seconds
        invoke=mock.Mock(side_effect=self.WireError())
        with self.assertRaisesRegex(RuntimeError,'within bounds'):
            self.helper(lambda:clock[0],sleep)(invoke,lambda deadline:None,attempts)
        self.assertLessEqual(invoke.call_count,100)
        self.assertEqual(len(sleeps),invoke.call_count)
        self.assertTrue(all(0<delay<=0.05 for delay in sleeps))
        self.assertGreaterEqual(clock[0],4.99)
        self.assertLessEqual(clock[0],5.0)

    def test_unrelated_or_lookalike_error_is_not_retried(self):
        for error in (self.WireError('hardware failure'),self.WireError('brightness update already pending extra'),
                      self.WireError(remote='org.other.Failed')):
            invoke=mock.Mock(side_effect=error);attempts=[];sleep=mock.Mock()
            with self.subTest(error=error),self.assertRaises(self.WireError):
                self.helper(sleep=sleep)(invoke,lambda deadline:None,attempts)
            self.assertEqual(invoke.call_count,1)
            self.assertNotIn('busy_refused',attempts[0])
            sleep.assert_not_called()

    def test_principal_or_signal_failure_blocks_admission(self):
        invoke=mock.Mock();attempts=[]
        with self.assertRaisesRegex(RuntimeError,'identity'):
            self.helper()(invoke,mock.Mock(side_effect=RuntimeError('identity')),attempts)
        invoke.assert_not_called()
        self.assertEqual(attempts,[])

    def test_late_verification_fails_without_repeating_accepted_operation(self):
        clock=[0];checks=[0];invoke=mock.Mock(return_value='accepted');attempts=[]
        def verify(deadline):
            checks[0]+=1
            if checks[0]==2:clock[0]=6
        with self.assertRaisesRegex(RuntimeError,'verification deadline exceeded'):
            self.helper(lambda:clock[0])(invoke,verify,attempts)
        self.assertEqual(invoke.call_count,1)
        self.assertIsNone(attempts[0]['error'])

    def test_accepted_operation_past_deadline_is_not_retried(self):
        ticks=iter([0,0,0,6]);invoke=mock.Mock(return_value='accepted');attempts=[]
        with self.assertRaisesRegex(RuntimeError,'deadline exceeded'):
            self.helper(lambda:next(ticks))(invoke,lambda deadline:None,attempts)
        self.assertEqual(invoke.call_count,1)
        self.assertIsNone(attempts[0]['error'])

if __name__=='__main__':unittest.main()
