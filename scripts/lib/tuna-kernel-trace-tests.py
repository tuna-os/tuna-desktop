#!/usr/bin/env python3
"""Data/retention regressions; no kernel build, code generation or tracing."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch, MagicMock

REPO = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('tuna_kernel_trace', str(REPO / 'packaging/marlin/perf/tuna-kernel-trace'))
spec = importlib.util.spec_from_loader(loader.name, loader)
observer = importlib.util.module_from_spec(spec)
loader.exec_module(observer)
loader = importlib.machinery.SourceFileLoader('observer_perf_host', str(REPO / 'scripts/tuna-vm-perf'))
spec = importlib.util.spec_from_loader(loader.name, loader)
host = importlib.util.module_from_spec(spec)
loader.exec_module(host)


class Fixture:
    def __init__(self, base=None):
        self.names = {}
        self.strings = bytearray(b'\0')
        self.records = []
        self.base = base
    def name(self, value):
        if value not in self.names:
            self.names[value] = (len(self.base.strings) if self.base else 0) + len(self.strings)
            self.strings.extend(value.encode() + b'\0')
        return self.names[value]
    def add(self, kind, name, size, count=0, payload=b'', flag=False):
        self.records.append(struct.pack('<III', self.name(name), kind << 24 | count | (0x80000000 if flag else 0), size) + payload)
        return (max(self.base.types) if self.base else 0) + len(self.records)
    def integer(self, name, size):
        return self.add(1, name, size, payload=struct.pack('<I', size * 8))
    def pointer(self, target):
        return self.add(2, '', target)
    def aggregate(self, kind, name, size, fields):
        payload = b''.join(struct.pack('<III', self.name(member), kind_id, offset * 8) for member, kind_id, offset in fields)
        return self.add(kind, name, size, len(fields), payload)
    def function(self, name, arguments, returned=0):
        payload = b''.join(struct.pack('<II', self.name('arg' + str(index)), kind_id) for index, kind_id in enumerate(arguments))
        proto = self.add(13, '', returned, len(arguments), payload)
        return self.add(12, name, proto)
    def raw(self):
        types = b''.join(self.records)
        return struct.pack('<HBBIIIII', 0xeb9f, 1, 0, 24, 0, len(types), len(types), len(self.strings)) + types + self.strings


def fixture():
    f = Fixture()
    u32 = f.integer('unsigned int', 4)
    u64 = f.integer('unsigned long long', 8)
    p64 = f.pointer(u64)
    mode = f.aggregate(4, 'drm_mode_object', 4, [('id', u32, 0)])
    minor = f.aggregate(4, 'drm_minor', 4, [('index', u32, 0)])
    minor_pointer = f.pointer(minor)
    device = f.aggregate(4, 'drm_device', 24, [('primary', minor_pointer, 16)])
    device_pointer = f.pointer(device)
    crtc = f.aggregate(4, 'drm_crtc', 40, [('dev', device_pointer, 0), ('base', mode, 24), ('index', u32, 32)])
    crtc_pointer = f.pointer(crtc)
    commit = f.aggregate(4, 'drm_atomic_commit', 8, [])
    commit_pointer = f.pointer(commit)
    pending_base = f.aggregate(4, 'drm_pending_event', 8, [('file_priv', p64, 0)])
    vbl = f.aggregate(4, 'drm_event_vblank', 8, [('user_data', u64, 0)])
    event = f.aggregate(5, '', 8, [('vbl', vbl, 0)])
    pending = f.aggregate(4, 'drm_pending_vblank_event', 40, [('base', pending_base, 0), ('event', event, 8), ('pipe', u32, 24), ('sequence', u64, 32)])
    pending_pointer = f.pointer(pending)
    node = f.aggregate(4, 'timerqueue_node', 8, [('expires', u64, 0)])
    timer = f.aggregate(4, 'hrtimer', 32, [('node', node, 16)])
    timer_pointer = f.pointer(timer)
    f.aggregate(4, 'drm_vblank_crtc_timer', 56, [('timer', timer, 8), ('interval', u64, 40), ('crtc', crtc_pointer, 48)])
    f.function('virtio_gpu_crtc_atomic_flush', [crtc_pointer, commit_pointer])
    f.function('drm_crtc_arm_vblank_event', [crtc_pointer, pending_pointer])
    f.function('drm_crtc_accurate_vblank_count', [crtc_pointer], u64)
    f.function('drm_crtc_vblank_get_vblank_timeout', [crtc_pointer, p64])
    f.function('send_vblank_event', [device_pointer, pending_pointer, u64, u64])
    f.function('drm_vblank_timer_function', [timer_pointer], u32)
    return f


class Parser(unittest.TestCase):
    def test_offsets_and_nested_union_come_from_fixture_btf(self):
        btf = observer.Btf(fixture().raw())
        definitions, offsets = observer.build_probes(btf, btf, btf, 'tuna_vblank_1234567890abcdef')
        self.assertEqual(offsets['drm_crtc.base.id'], {'offset': 24, 'bytes': 4})
        self.assertEqual(offsets['drm_pending_vblank_event.event.vbl.user_data']['offset'], 8)
        self.assertTrue(any('target=+32($arg2)' in row for row in definitions))
        self.assertTrue(any('interval=+32($arg1)' in row for row in definitions), 'timer container offset must be subtracted')
        self.assertTrue(any('minor=+0(+16(+0($arg1)))' in row for row in definitions))
    def test_builtin_drm_uses_exact_base_btf_and_unprefixed_symbols(self):
        btf = observer.Btf(fixture().raw())
        definitions, _ = observer.build_probes(btf, btf, btf, 'tuna_vblank_1234567890abcdef', drm_module=False)
        self.assertFalse(any(' drm:' in row for row in definitions))
        self.assertTrue(any(' drm_crtc_arm_vblank_event ' in row for row in definitions))
        self.assertTrue(any(' virtio_gpu:virtio_gpu_crtc_atomic_flush ' in row for row in definitions))

    def test_split_module_uses_live_base_type_and_string_ids(self):
        base = observer.Btf(fixture().raw())
        f = Fixture(base)
        f.aggregate(4, 'module_test', 8, [('value', base.named('unsigned long long', 1), 0)])
        split = observer.Btf(f.raw(), base)
        self.assertEqual(split.offset('module_test', 'value'), (0, 8))
        self.assertEqual(split.offset('drm_crtc', 'base', 'id'), (24, 4))
    def test_short_invalid_endian_and_section_bounds_rejected(self):
        raw = fixture().raw()
        for value in (b'', raw[:23], b'\x00\x00' + raw[2:], raw[:-1]):
            with self.assertRaises(ValueError): observer.Btf(value)
    def test_missing_and_duplicate_struct_names_rejected(self):
        f = fixture();f.aggregate(4, 'drm_crtc', 4, [])
        btf = observer.Btf(f.raw())
        with self.assertRaises(ValueError): btf.offset('drm_crtc', 'index')
        with self.assertRaises(ValueError): btf.offset('missing', 'index')
    def test_wrong_argument_prototype_is_not_a_proxy(self):
        btf = observer.Btf(fixture().raw())
        with self.assertRaises(ValueError): btf.signature('send_vblank_event', ['drm_crtc'], 'void')
        with self.assertRaises(ValueError): btf.signature('drm_crtc_accurate_vblank_count', ['drm_device'], 'scalar64')
    def test_bitfield_is_not_guessed_as_a_byte_offset(self):
        f = Fixture();u32=f.integer('unsigned int', 4)
        f.add(4, 'bitfield', 4, 1, struct.pack('<III', f.name('field'), u32, 3 << 24 | 1), flag=True)
        btf=observer.Btf(f.raw())
        with self.assertRaises(ValueError): btf.offset('bitfield', 'field')


class ActualRecordQualification(unittest.TestCase):
    def test_actual_timer_inventory_filters_all_and_rejects_new_identity(self):
        group = 'tuna_test'
        record = b'<idle>-0 [000] 1.0: tuna_test_timer: (fn) timer=0xffff1000 crtc=0xffff2000 object=39 pipe=0 dev=0xffff3000 minor=1\n'
        inventory = observer.timer_mappings(record, group)
        actual_format = 'field:void * hrtimer;\toffset:8;\tsize:8;\tsigned:0;'
        self.assertEqual(observer.timer_filter(inventory, actual_format), 'hrtimer == 0xffff1000')
        self.assertEqual(observer.timer_mappings(record, group, inventory), inventory)
        with self.assertRaisesRegex(ValueError, 'escaped discovered'):
            observer.timer_mappings(record.replace(b'0xffff1000', b'0xffff1001'), group, inventory)
        with self.assertRaisesRegex(ValueError, 'CRTC escaped'):
            observer.timer_mappings(record.replace(b'object=39', b'object=40'), group, inventory)
    def test_timer_inventory_missing_faulted_and_inconsistent_reject(self):
        record = b'<idle>-0 [000] 1.0: tuna_test_timer: (fn) timer=0xffff1000 crtc=0xffff2000 object=39 pipe=0 dev=0xffff3000 minor=1\n'
        for bad in [b'', record.replace(b'timer=0xffff1000', b'timer=(fault)'), record.replace(b'minor=1', b'minor=100'), record + record.replace(b'object=39', b'object=40'), record + record.replace(b'timer=0xffff1000', b'timer=0xffff1001')]:
            with self.assertRaises(ValueError):
                observer.timer_mappings(bad, 'tuna_test')
    def test_timer_filter_requires_live_pointer_field_and_bounded_inventory(self):
        inventory = {'4294901760': {}}
        for actual in ['field:void * hrtimer; offset:8; size:4; signed:0;', 'field:void * timer; offset:8; size:8; signed:0;']:
            with self.assertRaises(ValueError):
                observer.timer_filter(inventory, actual)
        with self.assertRaises(ValueError):
            observer.timer_filter({}, 'field:void * hrtimer; offset:8; size:8; signed:0;')
    def test_required_fields_not_faulted_or_missing(self):
        definition=['p:tuna_test/tuna_test_arm fn crtc=$arg1:x64 object=+1($arg1):u32 pipe=+2($arg1):u32']
        good=b'<task>-1 [000] 1.000: tuna_test_arm: (fn) crtc=0xffff object=39 pipe=0\n'
        self.assertEqual(observer.validate_trace_fields(good,'tuna_test',definition), {'tuna_test_arm':1})
        for raw in (good.replace(b'0xffff',b'0x0'),good.replace(b'object=39',b'object=(fault)'),good.replace(b'pipe=0',b'pipe=99'),b''):
            with self.assertRaises(ValueError): observer.validate_trace_fields(raw,'tuna_test',definition)
    def test_profile_names_are_unique_event_names_not_group_paths(self):
        text=b'tuna_test_arm 12 0\nother_arm 40 2\n'
        with patch.object(observer,'bounded',return_value=text):
            self.assertEqual(observer.profiles('tuna_test'), {'tuna_test_arm':{'hits':12,'misses':0}})


# Contiguous verbatim lines of run 43224217367, repeat-2/gnome/kernel-vblank-trace.txt.
REENTRY_43224217367 = b'''\
   kworker/u16:4-84      [001] .....   134.161689: tuna_vblank_486e88873cc9183b_flush: (virtio_gpu_crtc_atomic_flush+0x4/0xb0 [virtio_gpu]) crtc=0xffff8eb897e40040 object=39 pipe=0 dev=0xffff8eb896e08000 minor=1 commit=0xffff8eb8a4e09700
   kworker/u16:4-84      [001] d.h2.   134.161711: hrtimer_expire_entry: hrtimer=00000000194832d4 function=drm_vblank_timer_function now=134161709553
   kworker/u16:4-84      [001] d.h3.   134.161714: drm_vblank_event: crtc=0, seq=9826, time=134161690381, high-prec=true
   kworker/u16:4-84      [001] d.h2.   134.161715: hrtimer_expire_exit: hrtimer=00000000194832d4
   kworker/u16:4-84      [001] d.h3.   134.161715: hrtimer_start: hrtimer=00000000194832d4 function=drm_vblank_timer_function expires=134175024807 softexpires=134175024807 mode=ABS was_armed=0
   kworker/u16:4-84      [001] d..1.   134.161729: tuna_vblank_486e88873cc9183b_arm: (drm_crtc_arm_vblank_event+0x4/0x80) crtc=0xffff8eb897e40040 object=39 pipe=0 dev=0xffff8eb896e08000 minor=1 event=0xffff8eb896e48200 file=0xffff8eb8a526d800 userdata=0x55f218bc6c00
'''
# Contiguous verbatim lines of run 42807208426, repeat-2/gnome: the IRQ precedes
# the interrupted handler's own record, reserved after the IRQ returns.
REENTRY_42807208426 = b'''\
   kworker/u16:4-85      [001] d.h2.   139.079461: hrtimer_expire_entry: hrtimer=00000000b927d468 function=drm_vblank_timer_function now=139079459206
   kworker/u16:4-85      [001] d.h3.   139.079465: drm_vblank_event: crtc=0, seq=10152, time=139079438640, high-prec=true
   kworker/u16:4-85      [001] d.h2.   139.079466: hrtimer_expire_exit: hrtimer=00000000b927d468
   kworker/u16:4-85      [001] d.h3.   139.079467: hrtimer_start: hrtimer=00000000b927d468 function=drm_vblank_timer_function expires=139092773066 softexpires=139092773066 mode=ABS was_armed=0
   kworker/u16:4-85      [001] .....   139.079479: tuna_vblank_ac5abe8e98bab67d_timeout: (drm_crtc_vblank_get_vblank_timeout+0x4/0x150) crtc=0xffff8c7896c40040 object=39 pipe=0 dev=0xffff8c789510f000 minor=1 output=0xffffd2ff408a7e08
'''


class ProbeLoss(unittest.TestCase):
    DEFINITIONS = ['p:tuna_test/tuna_test_timer drm_vblank_timer_function timer=$arg1:x64',
                   'r64:tuna_test/tuna_test_timer_return drm_vblank_timer_function restart=$retval:s32',
                   'p:tuna_test/tuna_test_flush virtio_gpu_crtc_atomic_flush crtc=$arg1:x64']

    def test_every_miss_is_named_and_none_is_tolerated(self):
        before = {name: {'hits': 2, 'misses': 0} for name in ('tuna_test_timer', 'tuna_test_timer_return', 'tuna_test_flush')}
        self.assertEqual(observer.profile_errors(before, before, self.DEFINITIONS), ([], {}))
        after = dict(before, tuna_test_timer={'hits': 9, 'misses': 1}, tuna_test_timer_return={'hits': 9, 'misses': 1})
        errors, misses = observer.profile_errors(before, after, self.DEFINITIONS)
        self.assertEqual(misses, {'tuna_test_timer': 1, 'tuna_test_timer_return': 1})
        self.assertIn('actual kprobe missed 1 hit(s): tuna_test_timer (', errors[0])
        self.assertIn('another kprobe handler runs on that CPU', errors[0])
        self.assertIn('actual kretprobe missed 1 hit(s): tuna_test_timer_return (', errors[1])
        errors, _ = observer.profile_errors(before, {'tuna_test_timer': {'hits': 1, 'misses': 0}}, self.DEFINITIONS)
        self.assertEqual(errors, ['actual probe hit counter regressed: tuna_test_timer',
                                  'actual probe profile disappeared during capture: tuna_test_timer_return',
                                  'actual probe profile disappeared during capture: tuna_test_flush'])

    def test_irq_after_process_context_record_locates_reentered_flush(self):
        located = observer.locate_reentrant_misses(REENTRY_43224217367, 'tuna_vblank_486e88873cc9183b')
        self.assertEqual(len(located), 1)
        self.assertEqual((located[0]['cpu'], located[0]['irq_timestamp'], located[0]['interrupted_pid']), (1, '134.161711', 84))
        self.assertEqual([probe['event'] for probe in located[0]['interrupted_probe_candidates']],
                         ['tuna_vblank_486e88873cc9183b_flush'])
        self.assertTrue(located[0]['process_context_kprobe_reentered'])
        error, = observer.reentry_errors(located)
        self.assertIn('IRQ re-entered a process-context ftrace kprobe handler on the same vCPU', error)

    def test_irq_before_its_handler_record_locates_reentered_timeout(self):
        located = observer.locate_reentrant_misses(REENTRY_42807208426, 'tuna_vblank_ac5abe8e98bab67d')
        self.assertEqual([probe['event'] for probe in located[0]['interrupted_probe_candidates']],
                         ['tuna_vblank_ac5abe8e98bab67d_timeout'])

    def test_recorded_timer_irq_and_unattributed_irq_are_distinct(self):
        recorded = REENTRY_43224217367.replace(
            b'drm_vblank_event: crtc=0, seq=9826, time=134161690381, high-prec=true',
            b'tuna_vblank_486e88873cc9183b_timer: (drm_vblank_timer_function+0x4/0xe0) timer=0xffff8eb883457f60')
        self.assertEqual(observer.locate_reentrant_misses(recorded, 'tuna_vblank_486e88873cc9183b'), [])
        # Without an adjacent IRQs-enabled probe record the handler is not named.
        bare = b'\n'.join(REENTRY_43224217367.splitlines()[1:4]) + b'\n'
        located = observer.locate_reentrant_misses(bare, 'tuna_vblank_486e88873cc9183b')
        self.assertFalse(located[0]['process_context_kprobe_reentered'])
        self.assertIn('no adjacent process-context probe record', observer.reentry_errors(located)[0])


class Retention(unittest.TestCase):
    def test_actual_guest_probe_accepts_only_fixed_bounded_discovery_action(self):
        import base64
        agent = MagicMock()
        agent.command.side_effect = [{'pid':1}, {'exited':True, 'exitcode':0, 'out-data':base64.b64encode(json.dumps({'boottime_s':1,'kernel_trace':{'index':3}}).encode()).decode()}]
        result = host.guest_probe(agent, 'kernel-discovery-read', 3)
        self.assertEqual(result['kernel_trace']['index'], 3)
        self.assertEqual(agent.command.call_args_list[0].kwargs['arg'], ['kernel-discovery-read','--index','3'])
        for index in [-1,1024,True]:
            with self.assertRaises(ValueError):
                host.guest_probe(agent, 'kernel-discovery-read', index)
        with self.assertRaises(ValueError):
            host.guest_probe(agent, 'kernel-arbitrary-command', 0)
    def test_failed_discovery_does_not_prevent_whole_other_artifacts(self):
        import base64,hashlib
        raws = {'raw':b'whole actual kernel trace\n','formats':b'whole actual formats\n','discovery_raw':b'whole discovery\n'}
        metadata = {key:{'bytes':len(value),'sha256':hashlib.sha256(value).hexdigest()}for key,value in raws.items()}
        def read(agent, action, index):
            if action == 'kernel-discovery-read':
                raise RuntimeError('actual discovery read failed')
            raw = raws['raw' if action == 'kernel-trace-read' else 'formats']
            return {'kernel_trace':{'index':index,'data':base64.b64encode(raw).decode()}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=read):
            out = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'actual discovery read failed'):
                host.retain_kernel_trace(None, out, metadata)
            self.assertEqual((out/'kernel-vblank-trace.txt').read_bytes(),raws['raw'])
            self.assertEqual((out/'kernel-vblank-formats.json').read_bytes(),raws['formats'])
            self.assertEqual((out/'kernel-vblank-discovery.txt').read_bytes(),b'')
            self.assertEqual(json.loads((out/'kernel-vblank-artifact-errors.json').read_text())[0]['artifact'],'kernel-vblank-discovery.txt')
    def test_whole_discovery_retained_before_incomplete_measurement_rejection(self):
        raw = b'entire discovery: actual timer entry and return\n'
        metadata = {'qualified_acquisition':False, 'discovery_raw':{'bytes':len(raw),'sha256':__import__('hashlib').sha256(raw).hexdigest()}}
        import base64
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', return_value={'kernel_trace':{'index':0,'data':base64.b64encode(raw).decode()}}) as probe:
            with self.assertRaises(ValueError):
                host.retain_kernel_trace(None, Path(tmp), metadata)
            self.assertEqual((Path(tmp) / 'kernel-vblank-discovery.txt').read_bytes(), raw)
            probe.assert_called_once_with(None, 'kernel-discovery-read', 0)
    def test_whole_kernel_trace_retained_before_actual_loss_rejection(self):
        import base64, hashlib
        raw=b'actual trace bytes\n';formats=b'{"actual": "format"}'
        metadata={'raw':{'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()},
                  'formats':{'bytes':len(formats),'sha256':hashlib.sha256(formats).hexdigest()},
                  'qualified_acquisition':False,'errors':['actual dropped events']}
        def probe(agent, action, index):
            value=raw if action=='kernel-trace-read' else formats
            return {'kernel_trace':{'index':index,'data':base64.b64encode(value).decode()}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',side_effect=probe):
            out=Path(tmp)
            with self.assertRaisesRegex(ValueError, 'reports actual loss.*actual dropped events'):host.retain_kernel_trace(None,out,metadata)
            self.assertEqual((out/'kernel-vblank-trace.txt').read_bytes(),raw)
            self.assertEqual((out/'kernel-vblank-formats.json').read_bytes(),formats)
            self.assertEqual(json.loads((out/'kernel-vblank-source.json').read_text()),metadata)
    def test_incomplete_kernel_chunk_cannot_qualify(self):
        metadata={'raw':{'bytes':2,'sha256':'bad'}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',return_value={'kernel_trace':{'index':0,'data':'eA=='}}):
            with self.assertRaises(ValueError):host.retain_kernel_trace(None,Path(tmp),metadata)

class CallbackIdentity(unittest.TestCase):
    def fixture(self):
        messages = [
            ('Clutter::FrameClock::dispatch()', ''),
            ('Clutter::FrameClock::presented()', ''),
            ('Tuna::FrameClock::dispatch-id', 'output=Virtual-1 frame=839 dispatch_us=146603288'),
            ('Tuna::FrameClock::presented-id', 'output=Virtual-1 view_frame=839 global_frame=759 presentation_us=146602970 sequence=10084 flags=1 kms_ready_us=146605881'),
            ('Tuna::KMS::raw-page-flip', 'crtc=39 sequence=10084 seconds=146 microseconds=602970 device=/dev/dri/card1'),
            ('Tuna::KMS::raw-page-flip-identity', 'crtc=39 sequence=10084 user_data=0x123 page_flip_data=0x456'),
            ('Tuna::KMS::atomic-commit-begin', 'impl_device=0x123 update=0x789 req=0xabc fd=9 flags=513'),
            ('Tuna::KMS::atomic-commit-end', 'impl_device=0x123 update=0x789 req=0xabc fd=9 flags=513 ret=0'),
            ('Tuna::KMS::page-flip-listener-identity', 'crtc=39 impl_device=0x123 update=0x789 page_flip_data=0x456 listener_user_data=0xdef'),
        ]
        return {'marks':[{'pid':1135,'name':name,'message':message,'monotonic_ns':index} for index,(name,message) in enumerate(messages)]}
    def test_actual_identity_keeps_original_early_timestamp(self):
        result = host.frame_source_evidence(self.fixture(), 1135)
        self.assertEqual(result['presentations'][0]['source_time_us'],146602970)
        self.assertEqual(result['kernel_callback_identities'][0]['page_flip_data'],'0x456')
        self.assertEqual(len(result['kernel_commit_listener_observations']),3)
        self.assertNotIn('inferred_frame',result)
    def test_wrong_sequence_or_malformed_commit_cannot_qualify(self):
        decoded = self.fixture()
        decoded['marks'][5]['message'] = decoded['marks'][5]['message'].replace('sequence=10084','sequence=10085')
        with self.assertRaisesRegex(ValueError,'do not match'):
            host.frame_source_evidence(decoded,1135)
        decoded = self.fixture();decoded['marks'][6]['message'] += ' extra=guessed'
        with self.assertRaisesRegex(ValueError,'malformed actual commit'):
            host.frame_source_evidence(decoded,1135)


class OriginalRejection(unittest.TestCase):
    def test_diagnostic_failure_retains_original_bounds_and_capture(self):
        import base64, gzip
        fixture = REPO / 'scripts/lib/fixtures'
        metadata = json.loads((fixture/'gnome51-presentation-before-dispatch.json').read_text())
        raw = gzip.decompress((fixture/'gnome51-presentation-before-dispatch.syscap.gz').read_bytes())
        metadata['mutter_cogl_library'] = {'package':'mutter 51.0-1.3'}
        metadata['kernel_trace'] = {'qualified_acquisition':False,'errors':['actual loss']}
        order = []
        def chunk(agent, action, index):
            self.assertEqual(action,'gnome-trace-read')
            return {'gnome_trace':{'index':index,'data':base64.b64encode(raw[index*65536:(index+1)*65536]).decode()}}
        def kernel(*args):
            order.append('kernel')
            raise ValueError('actual loss')
        def identity(*args):
            order.append('identity')
            raise ValueError('missing diagnostic marks')
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',side_effect=chunk), patch.object(host,'retain_kernel_trace',side_effect=kernel), patch.object(host,'frame_source_evidence',side_effect=identity):
            out = Path(tmp)
            with self.assertRaisesRegex(ValueError,'missing diagnostic marks'):
                host.retain_gnome_trace(None,out,metadata)
            self.assertEqual(order,['kernel','identity'])
            self.assertEqual((out/'gnome-overview.syscap').read_bytes(),raw)
            owned = json.loads((out/'gnome-overview-frame-ownership.json').read_text())
            anomalies = [row for row in owned['anomalies'] if row['kind']=='presentation-precedes-dispatch']
            self.assertEqual([row['presentation_upper_ns'] for row in anomalies],[123677789000])
            self.assertIn('no independent source frame identifier',anomalies[0]['limitation'])
            rejected = json.loads((out/'gnome-overview-source-rejection.json').read_text())
            self.assertEqual(rejected['reason'],'missing diagnostic marks')
            self.assertTrue((out/'kernel-vblank-acquisition-rejection.json').exists())


class BufferAllocation(unittest.TestCase):
    def header(self, offset=16, size=4080):
        return f'field: char data; offset:{offset}; size:{size}; signed:1;\n'

    def test_exact_linux_payload_rounding_reserves_reader_within_bound(self):
        result = observer.buffer_allocation(self.header(), '4', 4096)
        self.assertEqual(result['requested_kb'], 2036)
        self.assertEqual(result['ring_pages'], 511)
        self.assertEqual(result['reader_pages'], 1)
        self.assertEqual(result['event_payload_bytes_per_cpu'], 2084880)
        self.assertEqual(result['expected_readback_kb'], '2036')
        self.assertEqual(result['data_page_bytes_per_cpu'], 2 * 1024 * 1024)
        # The failed 2048-KB request really rounds to 515 ring pages.
        failed_pages = (2048 * 1024 + 4080 - 1) // 4080
        self.assertEqual(failed_pages * 4080 // 1024, 2051)
        self.assertGreater((failed_pages + 1) * 4096, result['bound_bytes_per_cpu'])

    def test_larger_actual_subbuffers_keep_exact_data_page_bound(self):
        result = observer.buffer_allocation(self.header(size=8176), '8', 4096)
        self.assertLessEqual(result['data_page_bytes_per_cpu'], 2 * 1024 * 1024)
        self.assertEqual(result['data_page_bytes_per_cpu'],
                         (result['ring_pages'] + result['reader_pages']) * 8192)
        self.assertEqual(int(result['expected_readback_kb']), result['ring_pages'] * 8176 // 1024)

    def test_malformed_or_inconsistent_actual_layouts_reject(self):
        for header, subbuffer, page in (
            ('', '4', 4096), (self.header() * 2, '4', 4096),
            (self.header(offset=0), '4', 4096), (self.header(size=4081), '4', 4096),
            (self.header(), '3', 4096), (self.header(), '8', 4096),
            (self.header(), 'X', 4096), (self.header(), '4', 3000),
            (self.header(size=2 * 1024 * 1024 - 16), '2048', 4096),
        ):
            with self.subTest(header=header, subbuffer=subbuffer, page=page), self.assertRaises(ValueError):
                observer.buffer_allocation(header, subbuffer, page)

    def test_all_cpu_readbacks_must_match_exact_bound(self):
        with tempfile.TemporaryDirectory() as tmp:
            instance = Path(tmp)
            (instance / 'buffer_size_kb').write_text('2036\n')
            for cpu in range(2):
                path = instance / 'per_cpu' / f'cpu{cpu}'
                path.mkdir(parents=True)
                (path / 'buffer_size_kb').write_text('2036\n')
            with patch.object(observer, 'bounded', side_effect=lambda path, limit: path.read_bytes()):
                self.assertEqual(observer.buffer_readbacks(instance, 2, '2036'),
                                 {'all':'2036', 'cpu0':'2036', 'cpu1':'2036'})
                (instance / 'per_cpu/cpu1/buffer_size_kb').write_text('2037\n')
                with self.assertRaisesRegex(RuntimeError, 'exact bounded allocation'):
                    observer.buffer_readbacks(instance, 2, '2036')
                with self.assertRaisesRegex(RuntimeError, 'incomplete'):
                    observer.buffer_readbacks(instance, 3, '2036')


class Lifecycle(unittest.TestCase):
    def test_preflight_failure_retains_unqualified_metadata(self):
        root = MagicMock()
        root.exists.return_value = True
        root.__truediv__.return_value.exists.return_value = False
        root.stat.return_value.st_uid = 0
        root.stat.return_value.st_mode = 0o40700
        root.is_symlink.return_value = False
        with patch.object(observer, 'ROOT', root), patch.object(observer, 'start_observer', side_effect=ValueError('missing actual BTF')), patch.object(observer, 'save') as save:
            with self.assertRaisesRegex(ValueError, 'missing actual BTF'):
                observer.start()
            result = save.call_args.args[1]
            self.assertFalse(result['qualified_acquisition'])
            self.assertEqual(result['stage'], 'preflight')
            self.assertEqual(result['errors'], ['missing actual BTF'])
    def test_disable_failure_does_not_skip_owned_probe_cleanup(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp)
            instance = trace / 'instances/tuna_test'; instance.mkdir(parents=True)
            (trace / 'kprobe_events').write_text('')
            marker = {'group':'tuna_test', 'instance_created':True, 'definitions':['p:tuna_test/tuna_test_arm fn']}
            with patch.object(observer, 'TRACE', trace), patch.object(observer, 'write', side_effect=OSError('disable failed')):
                errors = observer.cleanup(marker)
            self.assertEqual(len(errors), 2)
            self.assertEqual((trace / 'kprobe_events').read_text(), '-:tuna_test/tuna_test_arm\n')
            self.assertFalse(instance.exists())
    def test_probe_control_preserves_other_definitions_without_seek(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp)
            control = trace / 'kprobe_events'
            control.write_text('p:other/event other_function\n')
            with patch.object(observer, 'TRACE', trace), patch.object(observer.os, 'lseek', side_effect=OSError('SEEK_END unsupported')):
                observer.probe_command('p:tuna_test/owned real_function')
                observer.probe_command('-:tuna_test/owned')
            self.assertEqual(control.read_text(), 'p:other/event other_function\np:tuna_test/owned real_function\n-:tuna_test/owned\n')
    def test_probe_control_short_write_rejects_and_closes(self):
        with patch.object(observer.os, 'open', return_value=17) as opened, patch.object(observer.os, 'write', return_value=1), patch.object(observer.os, 'close') as closed:
            with self.assertRaisesRegex(OSError, 'short actual probe'):
                observer.probe_command('p:tuna_test/owned real_function')
            flags = opened.call_args.args[1]
            self.assertTrue(flags & os.O_APPEND)
            self.assertFalse(flags & (os.O_TRUNC | os.O_CREAT))
            closed.assert_called_once_with(17)
    def test_unobserved_branch_is_coverage_not_fabricated_records(self):
        definitions = ['p:tuna_test/tuna_test_arm fn crtc=$arg1:x64', 'p:tuna_test/tuna_test_timeout fn output=$arg2:x64']
        counts = observer.validate_trace_fields(b'<task>-1 [000] 1.000: tuna_test_arm: (fn) crtc=0xffff\n', 'tuna_test', definitions)
        self.assertEqual(counts, {'tuna_test_arm':1, 'tuna_test_timeout':0})


if __name__=='__main__': unittest.main()
