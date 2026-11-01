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
    empty = f.add(3, '', 0, payload=struct.pack('<III', u32, u32, 0))
    f.aggregate(4, 'bpf_ringbuf', 12288, [('consumer_pos', u64, 4096),
                ('producer_pos', u64, 8192), ('data', empty, 12288)])
    f.aggregate(4, 'bpf_ringbuf_map', 16, [])
    f.function('virtio_gpu_crtc_atomic_flush', [crtc_pointer, commit_pointer])
    f.function('drm_crtc_arm_vblank_event', [crtc_pointer, pending_pointer])
    f.function('drm_crtc_accurate_vblank_count', [crtc_pointer], u64)
    f.function('drm_crtc_vblank_get_vblank_timeout', [crtc_pointer, p64])
    f.function('send_vblank_event', [device_pointer, pending_pointer, u64, u64])
    f.function('drm_vblank_timer_function', [timer_pointer], u32)
    return f


def binary_record(name='timer', *, fields=None, cpu=0, sequence=1,
                  monotonic_ns=1_000_000_000, failed_reads=0, mask=None, pid_tgid=42 << 32 | 42):
    """Explicit synthetic data fixture, never a runtime observation."""
    event = observer.EVENT_NAMES.index(name)
    actual = {'crtc':0xffff2000, 'object':39, 'pipe':0, 'dev':0xffff3000,
              'minor':1, 'commit':0xffff4000, 'event':0xffff5000, 'file':0xffff6000,
              'userdata':0, 'target':100, 'event_pipe':0, 'count':99,
              'output':0xffff7000, 'value':1_000_000_000, 'timer':0xffff1000,
              'interval':13_334_426, 'expires':1_013_334_426, 'restart':1,
              'sequence':100, 'timestamp':1_000_000_000}
    actual.update(fields or {})
    expected_mask = sum(1 << observer.FIELD_NAMES.index(key) for key in observer.EVENT_FIELDS[event])
    values = [actual[key] & ((1 << 64)-1) if key in observer.EVENT_FIELDS[event] else 0
              for key in observer.FIELD_NAMES]
    return observer.BPF_RECORD.pack(monotonic_ns, sequence, event, cpu, pid_tgid,
                                   *values, expected_mask if mask is None else mask, failed_reads, 0)


class Parser(unittest.TestCase):
    def test_offsets_and_nested_union_come_from_fixture_btf(self):
        btf = observer.Btf(fixture().raw())
        layout = observer.build_layout(btf, btf, btf)
        self.assertEqual(layout['offsets']['drm_crtc.base.id'], {'offset':24, 'bytes':4})
        self.assertEqual(layout['offsets']['drm_pending_vblank_event.event.vbl.user_data']['offset'], 8)
        self.assertEqual(layout['values'], [24,32,0,16,0,32,24,8,0,8,32,40,16])
        self.assertEqual([row['event'] for row in layout['functions']], list(range(10)))
    def test_builtin_drm_preserves_exact_function_and_module_ownership(self):
        btf = observer.Btf(fixture().raw())
        layout = observer.build_layout(btf, btf, btf, drm_module=False)
        self.assertEqual(layout['functions'][0]['attach_owner'], 'virtio_gpu')
        self.assertTrue(all(row['attach_owner']=='vmlinux' for row in layout['functions'][1:]))
        self.assertEqual(layout['functions'][1]['attach_btf_id'], btf.named('drm_crtc_arm_vblank_event',12))

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
        records = observer.bpf_records(binary_record(), [0])
        inventory = observer.timer_mappings(records)
        actual_format = 'field:void * hrtimer;\toffset:8;\tsize:8;\tsigned:0;'
        self.assertEqual(observer.timer_filter(inventory, actual_format), 'hrtimer == 0xffff1000')
        self.assertEqual(observer.timer_mappings(records, inventory), inventory)
        with self.assertRaisesRegex(ValueError, 'escaped discovered'):
            observer.timer_mappings(observer.bpf_records(binary_record(fields={'timer':0xffff1001}),[0]), inventory)
        with self.assertRaisesRegex(ValueError, 'CRTC escaped'):
            observer.timer_mappings(observer.bpf_records(binary_record(fields={'object':40}),[0]), inventory)
    def test_timer_inventory_missing_faulted_and_inconsistent_reject(self):
        with self.assertRaises(ValueError):observer.timer_mappings([])
        for raw in (binary_record(fields={'timer':0}), binary_record(failed_reads=1),
                    binary_record(fields={'minor':100}),
                    binary_record()+binary_record(fields={'object':40},sequence=2),
                    binary_record()+binary_record(fields={'timer':0xffff1001},sequence=2)):
            with self.assertRaises(ValueError):observer.timer_mappings(observer.bpf_records(raw,[0]))
    def test_timer_filter_requires_live_pointer_field_and_bounded_inventory(self):
        inventory = {'4294901760':{}}
        for actual in ('field:void * hrtimer; offset:8; size:4; signed:0;',
                       'field:void * timer; offset:8; size:8; signed:0;'):
            with self.assertRaises(ValueError):observer.timer_filter(inventory,actual)
        with self.assertRaises(ValueError):
            observer.timer_filter({},'field:void * hrtimer; offset:8; size:8; signed:0;')
    def test_full_original_fields_are_required_and_faults_never_become_zero_values(self):
        for name in observer.EVENT_NAMES:
            record = observer.bpf_records(binary_record(name),[0])[0]
            self.assertEqual(set(record['fields']),set(observer.EVENT_FIELDS[observer.EVENT_NAMES.index(name)]))
        for raw in (b'',binary_record()[:-1],binary_record(mask=0),binary_record(failed_reads=1),
                    binary_record(fields={'crtc':0}),binary_record(fields={'minor':-1}),
                    binary_record(fields={'pipe':99}),binary_record(cpu=1)):
            with self.assertRaises(ValueError):observer.bpf_records(raw,[0])
    def test_no_sequence_loss_reorder_or_duplicate_can_qualify(self):
        for second in (1,3):
            with self.assertRaisesRegex(ValueError,'sequence'):
                observer.bpf_records(binary_record()+binary_record(sequence=second),[0])
        with self.assertRaisesRegex(ValueError,'sequence'):
            observer.bpf_records(binary_record()+binary_record(sequence=2,monotonic_ns=1),[0])
    def test_sparse_online_ids_are_not_confused_with_possible_count(self):
        self.assertEqual(observer.bpf_records(binary_record(cpu=9),[0,9])[0]['cpu'],9)
        for ids in ([0,0],[9,0],list(range(9)),[65536],1):
            with self.assertRaises(ValueError):observer.bpf_records(binary_record(),ids)
        with self.assertRaises(ValueError):observer.bpf_records(binary_record(cpu=1),[0,9])
    def test_online_cpu_source_is_bounded_and_ordered(self):
        for raw,expected in ((b'0-3\n',[0,1,2,3]),(b'0,7-8\n',[0,7,8])):
            with patch.object(observer,'bounded',return_value=raw):
                self.assertEqual(observer.online_cpu_ids(),expected)
        for raw in (b'0-8\n',b'0-65535\n',b'9-1\n',b'2,1\n',b'0,0\n',b'\n',b'0-1,2-65536'):
            with patch.object(observer,'bounded',return_value=raw),self.assertRaises(ValueError):
                observer.online_cpu_ids()


class Retention(unittest.TestCase):
    def test_actual_guest_probe_accepts_only_fixed_bounded_read_actions(self):
        import base64
        for action in ('kernel-trace-read', 'kernel-formats-read', 'kernel-discovery-read', 'kernel-bpf-read'):
            for index in (0, 3, 1023):
                with self.subTest(action=action, index=index):
                    agent = MagicMock()
                    agent.command.side_effect = [{'pid':1}, {'exited':True, 'exitcode':0, 'out-data':base64.b64encode(json.dumps({'boottime_s':1,'kernel_trace':{'index':index}}).encode()).decode()}]
                    result = host.guest_probe(agent, action, index)
                    self.assertEqual(result['kernel_trace']['index'], index)
                    self.assertEqual(agent.command.call_args_list[0].kwargs['arg'], [action,'--index',str(index)])
            for index in (-1, 1024, True, None, '0', 1.0):
                with self.subTest(action=action, index=index), self.assertRaises(ValueError):
                    host.guest_probe(MagicMock(), action, index)
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
        raw=b'actual trace bytes\n';formats=b'{"actual": "format"}';discovery=b'whole original discovery\n'
        metadata={'raw':{'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()},
                  'formats':{'bytes':len(formats),'sha256':hashlib.sha256(formats).hexdigest()},
                  'discovery_raw':{'bytes':len(discovery),'sha256':hashlib.sha256(discovery).hexdigest()},
                  'qualified_acquisition':False,'errors':['actual dropped events']}
        def probe(agent, action, index):
            value={'kernel-trace-read':raw,'kernel-formats-read':formats,'kernel-discovery-read':discovery}[action]
            return {'kernel_trace':{'index':index,'data':base64.b64encode(value).decode()}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',side_effect=probe) as read:
            out=Path(tmp)
            with self.assertRaisesRegex(ValueError, 'reports actual loss'):
                host.retain_kernel_trace(None,out,metadata)
            self.assertEqual((out/'kernel-vblank-trace.txt').read_bytes(),raw)
            self.assertEqual((out/'kernel-vblank-formats.json').read_bytes(),formats)
            self.assertEqual((out/'kernel-vblank-discovery.txt').read_bytes(),discovery)
            self.assertNotIn('kernel-bpf-read', [call.args[1] for call in read.call_args_list])
            self.assertFalse((out/'kernel-vblank-artifact-errors.json').exists())
            self.assertEqual(json.loads((out/'kernel-vblank-source.json').read_text()),metadata)
    def test_incomplete_kernel_chunk_cannot_qualify(self):
        metadata={'raw':{'bytes':2,'sha256':'bad'}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',return_value={'kernel_trace':{'index':0,'data':'eA=='}}):
            with self.assertRaises(ValueError):host.retain_kernel_trace(None,Path(tmp),metadata)


class BpfRetention(unittest.TestCase):
    actions = {'kernel-trace-read':'raw', 'kernel-formats-read':'formats',
               'kernel-discovery-read':'discovery_raw', 'kernel-bpf-read':'bpf_raw'}
    names = {'raw':'kernel-vblank-trace.txt', 'formats':'kernel-vblank-formats.json',
             'discovery_raw':'kernel-vblank-discovery.txt', 'bpf_raw':'kernel-vblank-bpf.bin'}

    def fixture(self):
        import hashlib
        raws = {'raw':b'whole native trace\n', 'formats':b'{"native":"formats"}',
                'discovery_raw':b'whole independent discovery\n',
                'bpf_raw':bytes(range(208)) * 800}
        metadata = {key:{'bytes':len(raw), 'sha256':hashlib.sha256(raw).hexdigest()} for key, raw in raws.items()}
        metadata['discovery_bpf'] = {'bytes':208*333, 'records':333,
                                     'sha256':hashlib.sha256(raws['bpf_raw'][:208*333]).hexdigest()}
        before = []
        after = []
        for event, name in enumerate(('flush', 'arm', 'arm_return', 'accurate', 'accurate_return',
                                      'timeout', 'timeout_return', 'timer', 'timer_return', 'send')):
            program = {'event':event, 'name':name, 'id':event+1, 'attach_btf_obj_id':2,
                       'attach_btf_id':event+100, 'attach_owner':'virtio_gpu' if event == 0 else 'drm',
                       'run_cnt':0, 'recursion_misses':0, 'run_time_ns':0}
            before.append(program)
            after.append(dict(program, run_cnt=80, run_time_ns=80, calls=80, disabled=0, emitted=80,
                              reserve_failed=0, read_failed=0, retained_records=80))
        metadata.update(acquisition_backend='bpf-fentry-fexit-v1', bpf_schema=1,
                        qualified_acquisition=True, errors=[], cleanup_errors=[],
                        group='tuna_vblank_0123456789abcdef', native_pid=42,
                        cpus=1,online_cpu_ids=[0],present_cpu_ids=[0],possible_cpu_count=16,
                        native_trace_clock='local [mono] boot\n',native_trace_clock_after='local [mono] boot\n',
                        cpu_stats={f'cpu{cpu}':{key:'0' for key in ('overrun','commit overrun','dropped events')}
                                   for cpu in range(16)},
                        capture_filter='actual-native-timer-inventory-v1',
                        discovery_started_monotonic_ns=100,native_trace_started_monotonic_ns=101,
                        discovery_finished_monotonic_ns=123,filters_ready_monotonic_ns=125,
                        stopped_monotonic_ns=175,native_trace_stopped_monotonic_ns=176,
                        timer_inventory=observer.timer_mappings(observer.bpf_records(binary_record(),[0])),
                        timer_filters={event:{'requested':'hrtimer == 0xffff1000','actual':'hrtimer == 0xffff1000'}
                                       for event in observer.EVENTS if event.startswith('timer/')},
                        buffer_budget=observer.buffer_budget(observer.Btf(fixture().raw()),1,4096),
                        actual_record_counts=dict.fromkeys(observer.EVENT_NAMES,80),
                        native_timer_reconciliation={'active_window_begin_ns':125,'active_window_end_ns':175,
                            'native_callbacks_definitely_inside':1,'bpf_timer_entries_inside':1,
                            'complete_timer_pairs_inside':1},
                        function_pair_reconciliation={'active_window_begin_ns':125,'active_window_end_ns':175,
                            'entries_inside':dict.fromkeys(('arm','accurate','timeout','timer'),1),
                            'complete_pairs_by_entry_inside':dict.fromkeys(('arm','accurate','timeout','timer'),1)},
                        native_exit={'schema':1, 'group':'tuna_vblank_0123456789abcdef', 'pid':42,
                                     'terminal_observed':True, 'exitcode':0, 'observed_exit_monotonic_ns':204},
                        native_bpf={'schema':1, 'capture_enable_begin_monotonic_ns':99,
                                    'capture_started_monotonic_ns':100,
                                    'capture_stop_begin_monotonic_ns':199,
                                    'capture_stopped_monotonic_ns':200,
                                    'quiescence_begin_monotonic_ns':201,'quiescence_end_monotonic_ns':202,
                                    'detached_monotonic_ns':203,'quiescence_completed':True,
                                    'quiescence_backend':'tracefs-function-ops-tasks-rcu-v1',
                                    'quiescence_registration_before':{'count':0,'raw_line':None},
                                    'quiescence_registration_active':{'count':1,'raw_line':'drm_crtc_arm_vblank_event (1)\n'},
                                    'quiescence_registration_after':{'count':0,'raw_line':None},
                                    'online_cpu_ids':[0],'possible_cpu_count':16,
                                    'qualified_acquisition':True, 'errors':[], 'cleanup_errors':[],
                                    'raw':{'bytes':len(raws['bpf_raw'])},
                                    'programs_before':before, 'programs_after':after})
        return raws, metadata

    def reader(self, raws):
        import base64
        def read(agent, action, index):
            raw = raws[self.actions[action]][index*65536:(index+1)*65536]
            return {'kernel_trace':{'index':index, 'data':base64.b64encode(raw).decode()}}
        return read

    def assert_whole(self, out, raws, except_kind=None):
        for kind, raw in raws.items():
            if kind != except_kind:
                self.assertEqual((out/self.names[kind]).read_bytes(), raw)

    def test_complete_bpf_source_retains_all_whole_artifacts(self):
        raws, metadata = self.fixture()
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)) as read:
            out = Path(tmp)
            host.retain_kernel_trace(None, out, metadata)
            self.assert_whole(out, raws)
            self.assertEqual([call.args[2] for call in read.call_args_list if call.args[1]=='kernel-bpf-read'], [0,1,2])
            self.assertEqual(json.loads((out/'kernel-vblank-source.json').read_text()), metadata)
            self.assertFalse((out/'kernel-vblank-artifact-errors.json').exists())

    def test_missing_bpf_source_refuses_without_losing_independent_artifacts(self):
        raws, metadata = self.fixture()
        del metadata['bpf_raw']
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)) as read:
            out = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'incomplete independent kernel artifacts'):
                host.retain_kernel_trace(None, out, metadata)
            self.assert_whole(out, raws, 'bpf_raw')
            self.assertNotIn('kernel-bpf-read', [call.args[1] for call in read.call_args_list])
            self.assertEqual(json.loads((out/'kernel-vblank-artifact-errors.json').read_text())[0]['artifact'],
                             self.names['bpf_raw'])

    def test_bpf_chunk_failure_does_not_trim_or_qualify(self):
        import base64
        for failure in ('truncated', 'corrupt', 'wrong-index', 'boolean-index', 'invalid-base64', 'oversized', 'read-error'):
            with self.subTest(failure=failure):
                raws, metadata = self.fixture()
                reader = self.reader(raws)
                def read(agent, action, index):
                    row = reader(agent, action, index)
                    if action == 'kernel-bpf-read' and index == 1:
                        if failure == 'read-error':
                            raise RuntimeError('BPF source read refused')
                        if failure == 'truncated':
                            row['kernel_trace']['data'] = base64.b64encode(b'x').decode()
                        if failure == 'corrupt':
                            row['kernel_trace']['data'] = base64.b64encode(b'x'*65536).decode()
                        if failure == 'wrong-index':
                            row['kernel_trace']['index'] = 2
                        if failure == 'boolean-index':
                            row['kernel_trace']['index'] = True
                        if failure == 'invalid-base64':
                            row['kernel_trace']['data'] = '!!!!'
                        if failure == 'oversized':
                            row['kernel_trace']['data'] = 'A'*87385
                    return row
                with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=read):
                    out = Path(tmp)
                    with self.assertRaisesRegex(ValueError, 'incomplete independent kernel artifacts'):
                        host.retain_kernel_trace(None, out, metadata)
                    self.assert_whole(out, raws, 'bpf_raw')
                    errors = json.loads((out/'kernel-vblank-artifact-errors.json').read_text())
                    self.assertEqual([row['artifact'] for row in errors], [self.names['bpf_raw']])

    def test_independent_failure_still_retains_complete_bpf(self):
        for failed_kind in ('raw', 'formats', 'discovery_raw'):
            with self.subTest(failed_kind=failed_kind):
                raws, metadata = self.fixture()
                reader = self.reader(raws)
                def read(agent, action, index):
                    if self.actions[action] == failed_kind:
                        raise RuntimeError('independent source read refused')
                    return reader(agent, action, index)
                with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=read):
                    out = Path(tmp)
                    with self.assertRaises(ValueError):
                        host.retain_kernel_trace(None, out, metadata)
                    self.assert_whole(out, raws, failed_kind)

    def test_combined_bound_refuses_bpf_before_extra_transfer(self):
        raws, metadata = self.fixture()
        metadata['bpf_raw']['bytes'] = 64*1024*1024
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)) as read:
            out = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'combined native and BPF raw artifacts exceed'):
                host.retain_kernel_trace(None, out, metadata)
            self.assert_whole(out, raws, 'bpf_raw')
            self.assertFalse((out/self.names['bpf_raw']).exists())
            self.assertNotIn('kernel-bpf-read', [call.args[1] for call in read.call_args_list])
            self.assertEqual(json.loads((out/'kernel-vblank-artifact-errors.json').read_text())[0]['artifact'],
                             self.names['bpf_raw'])

    def test_oversized_native_does_not_prevent_safe_whole_bpf(self):
        raws, metadata = self.fixture()
        metadata['raw']['bytes'] = 64*1024*1024+1
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)) as read:
            out = Path(tmp)
            with self.assertRaises(ValueError):
                host.retain_kernel_trace(None, out, metadata)
            self.assert_whole(out, raws, 'raw')
            self.assertNotIn('kernel-trace-read', [call.args[1] for call in read.call_args_list])

    def test_invalid_bpf_artifact_metadata_refuses_before_transfer(self):
        for field, value in (('bytes', 0), ('bytes', True), ('bytes', 64*1024*1024+1),
                             ('bytes', '208'), ('sha256', 'invalid'), ('sha256', 'A'*64)):
            with self.subTest(field=field, value=value):
                raws, metadata = self.fixture()
                metadata['bpf_raw'][field] = value
                with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)) as read:
                    out = Path(tmp)
                    with self.assertRaises(ValueError):
                        host.retain_kernel_trace(None, out, metadata)
                    self.assert_whole(out, raws, 'bpf_raw')
                    self.assertNotIn('kernel-bpf-read', [call.args[1] for call in read.call_args_list])

    def test_old_qualified_kprobe_receipt_cannot_fallback(self):
        raws, metadata = self.fixture()
        for key in ('acquisition_backend', 'bpf_schema', 'bpf_raw', 'native_bpf', 'discovery_bpf'):
            del metadata[key]
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)) as read:
            out = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'lacks supported complete BPF source evidence'):
                host.retain_kernel_trace(None, out, metadata)
            self.assert_whole(out, raws, 'bpf_raw')
            self.assertNotIn('kernel-bpf-read', [call.args[1] for call in read.call_args_list])

    def test_actual_bpf_loss_preserves_whole_sources_before_rejection(self):
        raws, metadata = self.fixture()
        metadata.update(qualified_acquisition=False, errors=['actual reserve loss'])
        metadata['native_bpf'].update(qualified_acquisition=False, errors=['actual reserve loss'])
        metadata['native_bpf']['programs_after'][0]['reserve_failed'] = 1
        with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)):
            out = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'reports actual loss'):
                host.retain_kernel_trace(None, out, metadata)
            self.assert_whole(out, raws)
            self.assertFalse((out/'kernel-vblank-artifact-errors.json').exists())

    def test_missing_unsupported_or_incomplete_receipt_never_qualifies(self):
        mutations = [
            lambda m:m.pop('acquisition_backend'),
            lambda m:m.update(acquisition_backend='kprobe'),
            lambda m:m.update(bpf_schema=2),
            lambda m:m.update(bpf_schema=True),
            lambda m:m.pop('discovery_bpf'),
            lambda m:m['discovery_bpf'].update(bytes=True),
            lambda m:m['discovery_bpf'].update(bytes=2*1024*1024+1),
            lambda m:m['discovery_bpf'].update(bytes=m['bpf_raw']['bytes']+208),
            lambda m:m['discovery_bpf'].update(records=1),
            lambda m:m['discovery_bpf'].update(sha256='0'*64),
            lambda m:m.pop('native_bpf'),
            lambda m:m.pop('native_exit'),
            lambda m:m['native_exit'].update(schema=2),
            lambda m:m['native_exit'].update(terminal_observed=False),
            lambda m:m['native_exit'].pop('terminal_observed'),
            lambda m:m['native_exit'].update(exitcode=1),
            lambda m:m['native_exit'].update(exitcode=-9),
            lambda m:m['native_exit'].update(exitcode=False),
            lambda m:m['native_exit'].pop('exitcode'),
            lambda m:m['native_exit'].update(pid=43),
            lambda m:m['native_exit'].update(pid=True),
            lambda m:m['native_exit'].update(group='another-acquisition'),
            lambda m:m['native_exit'].pop('observed_exit_monotonic_ns'),
            lambda m:m['native_exit'].update(observed_exit_monotonic_ns=199),
            lambda m:m.pop('native_pid'),
            lambda m:m.update(native_pid=True),
            lambda m:m.pop('group'),
            lambda m:m['native_bpf'].update(schema=2),
            lambda m:m['native_bpf'].update(qualified_acquisition=False),
            lambda m:m['native_bpf'].pop('errors'),
            lambda m:m['native_bpf'].update(errors=['actual failure']),
            lambda m:m['native_bpf'].update(cleanup_errors=['detach failure']),
            lambda m:m['native_bpf'].pop('cleanup_errors'),
            lambda m:m['native_bpf'].pop('capture_stopped_monotonic_ns'),
            lambda m:m.update(cpus=True),
            lambda m:m.update(online_cpu_ids=[1]),
            lambda m:m.pop('buffer_budget'),
            lambda m:m.pop('native_trace_clock'),
            lambda m:m.update(native_trace_clock_after='[local] mono'),
            lambda m:m.pop('cpu_stats'),
            lambda m:m['cpu_stats']['cpu0'].pop('overrun'),
            lambda m:m['cpu_stats']['cpu0'].update(overrun='1'),
            lambda m:m['cpu_stats']['cpu0'].update(overrun=0),
            lambda m:m.pop('capture_filter'),
            lambda m:m.pop('native_trace_started_monotonic_ns'),
            lambda m:m.update(stopped_monotonic_ns=180),
            lambda m:m['buffer_budget'].pop('actual_cpus'),
            lambda m:m['buffer_budget'].update(maximum_accounted_ring_bytes=4*1024*1024),
            lambda m:m['native_bpf'].update(possible_cpu_count=0),
            lambda m:m['native_bpf'].update(quiescence_completed=False),
            lambda m:m['native_bpf'].pop('quiescence_backend'),
            lambda m:m['native_bpf'].pop('quiescence_begin_monotonic_ns'),
            lambda m:m['native_bpf']['quiescence_registration_before'].pop('raw_line'),
            lambda m:m['native_bpf']['quiescence_registration_active'].update(count=2),
            lambda m:m['native_bpf']['programs_after'][0].update(run_cnt=0),
            lambda m:m['native_bpf']['programs_after'][0].update(calls=0),
            lambda m:m['native_bpf']['programs_after'][0].update(retained_records=0),
            lambda m:m.pop('actual_record_counts'),
            lambda m:m['actual_record_counts'].update(timer=79),
            lambda m:m.pop('native_timer_reconciliation'),
            lambda m:m.pop('function_pair_reconciliation'),
            lambda m:m['function_pair_reconciliation']['complete_pairs_by_entry_inside'].update(timer=0),
            lambda m:m['function_pair_reconciliation']['complete_pairs_by_entry_inside'].update(timer=True),
            lambda m:m['native_bpf'].pop('capture_enable_begin_monotonic_ns'),
            lambda m:m['native_bpf'].pop('capture_stop_begin_monotonic_ns'),
            lambda m:m['native_bpf'].update(capture_stop_begin_monotonic_ns=100),
            lambda m:m['native_bpf']['raw'].update(bytes=1),
            lambda m:m['native_bpf'].pop('programs_before'),
            lambda m:m['native_bpf']['programs_before'].pop(),
            lambda m:m['native_bpf']['programs_after'].pop(),
            lambda m:m['native_bpf']['programs_after'][0].pop('calls'),
            lambda m:m['native_bpf']['programs_after'][0].update(recursion_misses=1),
            lambda m:m['native_bpf']['programs_after'][0].update(reserve_failed=1),
            lambda m:m['native_bpf']['programs_after'][0].update(read_failed=1),
            lambda m:m['native_bpf']['programs_after'][0].update(name='unsupported-program'),
            lambda m:m['native_bpf']['programs_after'][0].update(id=99),
            lambda m:m['native_bpf']['programs_after'][0].update(attach_owner='vmlinux'),
            lambda m:m['native_bpf']['programs_after'][0].update(event=True),
            lambda m:m.pop('cleanup_errors'),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(mutation=index):
                raws, metadata = self.fixture()
                mutate(metadata)
                with tempfile.TemporaryDirectory() as tmp, patch.object(host, 'guest_probe', side_effect=self.reader(raws)):
                    out = Path(tmp)
                    with self.assertRaises(ValueError):
                        host.retain_kernel_trace(None, out, metadata)
                    self.assert_whole(out, raws)

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


class NativeQualification(unittest.TestCase):
    def fixture(self):
        btf = observer.Btf(fixture().raw())
        layout = observer.build_layout(btf,btf,btf,drm_module=False)
        before = [dict(event=i,name=row['name'],id=100+i,attach_btf_obj_id=21 if i==0 else 1,
                       attach_btf_id=row['attach_btf_id'],attach_owner=row['attach_owner'],
                       run_cnt=0,run_time_ns=0,recursion_misses=0)
                  for i,row in enumerate(layout['functions'])]
        after = [dict(row,run_cnt=1,run_time_ns=1,calls=1,disabled=0,emitted=1,
                      reserve_failed=0,read_failed=0,retained_records=1) for row in before]
        raw = b''.join(binary_record(name,monotonic_ns=1_000_000_000+i)
                       for i,name in enumerate(observer.EVENT_NAMES))
        records = observer.bpf_records(raw,[0])
        native = dict(schema=1,qualified_acquisition=True,errors=[],cleanup_errors=[],
                      programs_before=before,programs_after=after,raw={'bytes':len(raw)},
                      online_cpu_ids=[0],possible_cpu_count=16,
                      quiescence_completed=True,quiescence_backend='tracefs-function-ops-tasks-rcu-v1',
                      quiescence_registration_before={'count':0,'raw_line':None},
                      quiescence_registration_active={'count':1,'raw_line':'drm_crtc_arm_vblank_event (1)\n'},
                      quiescence_registration_after={'count':0,'raw_line':None},
                      capture_enable_begin_monotonic_ns=999_999_900,
                      capture_started_monotonic_ns=999_999_901,
                      capture_stop_begin_monotonic_ns=1_000_000_100,
                      capture_stopped_monotonic_ns=1_000_000_101,
                      quiescence_begin_monotonic_ns=1_000_000_102,
                      quiescence_end_monotonic_ns=1_000_000_103,detached_monotonic_ns=1_000_000_104)
        marker = {'layout':layout,'online_cpu_ids':[0],
                  'native_ready':{'programs_before':before,'possible_cpu_count':16},'native_exit':{'exitcode':0}}
        return native,marker,records,raw
    def test_complete_source_counters_and_whole_bytes_reconcile(self):
        native,marker,records,raw = self.fixture()
        self.assertEqual(observer.native_qualification(native,marker,records,len(raw)),
                         dict.fromkeys(observer.EVENT_NAMES,1))
    def test_stats_loss_identity_and_extent_fail_closed(self):
        native,marker,records,raw = self.fixture()
        changes = [lambda m:m['programs_after'][5].update(recursion_misses=1),
                   lambda m:m['programs_after'][0].update(run_cnt=0),
                   lambda m:m['programs_after'][1].update(retained_records=0),
                   lambda m:m['programs_after'][2].update(read_failed=1),
                   lambda m:m['programs_after'][3].update(reserve_failed=1),
                   lambda m:m['programs_after'][4].update(attach_owner='another_module'),
                   lambda m:m['programs_after'].pop(),
                   lambda m:m['raw'].update(bytes=len(raw)-1),
                   lambda m:m.update(detached_monotonic_ns=1),
                   lambda m:m.update(schema=True),
                   lambda m:m.update(online_cpu_ids=[1]),
                   lambda m:m.update(possible_cpu_count=0),
                   lambda m:m.update(quiescence_completed=False),
                   lambda m:m.pop('quiescence_backend'),
                   lambda m:m['quiescence_registration_before'].pop('raw_line'),
                   lambda m:m['quiescence_registration_active'].update(count=2)]
        for change in changes:
            mutated=json.loads(json.dumps(native));change(mutated)
            with self.assertRaises(ValueError):
                observer.native_qualification(mutated,marker,records,len(raw))
    def test_post_serialization_native_exit_failure_cannot_qualify(self):
        native,marker,records,raw=self.fixture()
        marker['native_exit']['exitcode']=1
        with self.assertRaisesRegex(ValueError,'exit failure'):
            observer.native_qualification(native,marker,records,len(raw))
    def test_native_callbacks_and_complete_pairs_are_independent(self):
        records=observer.bpf_records(binary_record(monotonic_ns=1_000_000_100)+
                    binary_record('timer_return',monotonic_ns=1_000_000_200),[0])
        source=b'<idle>-0 [000] d.h1. 1.000000: hrtimer_expire_entry: hrtimer=hashed function=drm_vblank_timer_function now=1000000000\n'
        marker={'cpus':1,'online_cpu_ids':[0],'filters_ready_monotonic_ns':999_999_900}
        native={'capture_started_monotonic_ns':999_999_900,
                'capture_stop_begin_monotonic_ns':1_000_002_000}
        proof=observer.native_timer_reconciliation(source,records,marker,native,1_000_003_000)
        self.assertEqual(proof['native_callbacks_definitely_inside'],1)
        self.assertEqual(proof['complete_timer_pairs_inside'],1)
        for bad in (b'',source+source.replace(b'1.000000:',b'1.000001:'),
                    source.replace(b'drm_vblank_timer_function',b'another_callback')):
            with self.assertRaises(ValueError):
                observer.native_timer_reconciliation(bad,records,marker,native,1_000_003_000)
    def test_every_original_function_pair_requires_actual_return(self):
        for name in ('arm','accurate','timeout','timer'):
            records=observer.bpf_records(binary_record(name,monotonic_ns=100),[0])
            with self.assertRaisesRegex(ValueError,'entry lacks retained return'):
                observer.function_pair_reconciliation(records,50,200)
            records+=observer.bpf_records(binary_record(name+'_return',monotonic_ns=300),[0])
            proof=observer.function_pair_reconciliation(records,50,200)
            self.assertEqual(proof['complete_pairs_by_entry_inside'][name],1)
            self.assertEqual(len(proof['boundary']['pairs_crossing_stop']),1)
    def test_start_stop_boundaries_are_explicit_not_fabricated_pairs(self):
        raw=binary_record('arm',monotonic_ns=10)+binary_record('arm_return',monotonic_ns=100)
        proof=observer.function_pair_reconciliation(observer.bpf_records(raw,[0]),50,200)
        self.assertEqual(proof['entries_inside']['arm'],0)
        self.assertEqual(len(proof['boundary']['pairs_crossing_start']),1)
        raw=binary_record('arm_return',monotonic_ns=10)+binary_record('arm',monotonic_ns=300)
        proof=observer.function_pair_reconciliation(observer.bpf_records(raw,[0]),50,200)
        self.assertEqual(len(proof['boundary']['returns_without_entry']),1)
        self.assertEqual(len(proof['boundary']['entries_without_return']),1)
        with self.assertRaisesRegex(ValueError,'return lacks retained entry'):
            observer.function_pair_reconciliation(observer.bpf_records(binary_record('arm_return',monotonic_ns=100),[0]),50,200)
    def test_task_migration_and_nested_argument_identity_are_checked(self):
        raw=binary_record('accurate',cpu=0,monotonic_ns=100)+binary_record('accurate_return',cpu=9,monotonic_ns=200)
        proof=observer.function_pair_reconciliation(observer.bpf_records(raw,[0,9]),50,300)
        self.assertEqual(proof['complete_pairs_by_entry_inside']['accurate'],1)
        raw=binary_record('accurate',cpu=0,monotonic_ns=100,pid_tgid=0)+binary_record('accurate_return',cpu=9,monotonic_ns=200,pid_tgid=0)
        with self.assertRaises(ValueError):observer.function_pair_reconciliation(observer.bpf_records(raw,[0,9]),50,300)
        raw=binary_record('timeout',monotonic_ns=100)+binary_record('timeout_return',monotonic_ns=200,fields={'output':0xffff7001})
        with self.assertRaisesRegex(ValueError,'identity changed'):
            observer.function_pair_reconciliation(observer.bpf_records(raw,[0]),50,300)


class BufferAllocation(unittest.TestCase):
    def header(self, offset=16, size=4080):
        return f'field: char data; offset:{offset}; size:{size}; signed:1;\n'

    def test_shared_bpf_and_native_pages_fit_total_budget_for_each_cpu_count(self):
        btf=observer.Btf(fixture().raw())
        for cpus in (1,2,4,8):
            budget=observer.buffer_budget(btf,cpus,4096)
            layout=observer.buffer_allocation(self.header(),'4',4096,budget['native_bound_bytes_per_cpu'])
            self.assertEqual(budget['bpf_ring_allocation_bytes'],1024*1024+12288+8192+4096)
            self.assertLessEqual(budget['bpf_ring_allocation_bytes']+cpus*(
                layout['data_page_bytes_per_cpu']+budget['native_inactive_snapshot_bytes_per_cpu']
                +budget['barrier_ring_reader_snapshot_bytes_per_cpu']),2*1024*1024*cpus)
            self.assertEqual(budget['native_bound_bytes_per_cpu']%4096,0)
        with self.assertRaises(ValueError):observer.buffer_budget(btf,0,4096)
        with self.assertRaises(ValueError):observer.buffer_budget(btf,9,4096)
        with self.assertRaises(ValueError):observer.buffer_budget(btf,4,8192)

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
    def test_unretired_native_never_reenters_potentially_blocked_tracefs(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace=Path(tmp);instance=trace/'instances/tuna_test';instance.mkdir(parents=True)
            marker={'group':'tuna_test','instance_created':True,'native_launched':True}
            with patch.object(observer,'TRACE',trace),patch.object(observer,'write',side_effect=OSError('disable failed')) as write,patch.object(observer,'stop_native',side_effect=RuntimeError('physical exit missing')) as native:
                errors=observer.cleanup(marker)
            self.assertEqual(len(errors),2);native.assert_called_once_with(marker);write.assert_not_called()
            self.assertTrue(instance.exists())
    def test_disable_failure_after_terminal_keeps_error_and_removes_owned_instances(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace=Path(tmp);instance=trace/'instances/tuna_test';instance.mkdir(parents=True)
            barrier=trace/'instances/tuna_test_barrier';barrier.mkdir()
            marker={'group':'tuna_test','instance_created':True,'barrier_instance_created':True,'native_launched':True}
            with patch.object(observer,'TRACE',trace),patch.object(observer,'write',side_effect=OSError('disable failed')),patch.object(observer,'stop_native',side_effect=lambda value:value.update(native_terminal_observed=True)) as native:
                errors=observer.cleanup(marker)
            self.assertEqual(len(errors),2);native.assert_called_once_with(marker)
            self.assertFalse(instance.exists());self.assertFalse(barrier.exists())
    def test_atomic_receipt_never_publishes_partial_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            target=Path(tmp)/'native-exit.json'
            with patch.object(observer.json,'dump',side_effect=OSError('short receipt')):
                with self.assertRaises(OSError):observer.save_atomic(target,{'exitcode':0})
            self.assertFalse(target.exists())
            self.assertFalse(target.with_name(target.name+'.pending').exists())
            observer.save_atomic(target,{'exitcode':0})
            self.assertEqual(json.loads(target.read_text()),{'exitcode':0})
            self.assertEqual(target.stat().st_mode&0o777,0o600)
    def test_stop_does_not_signal_reused_native_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            marker={'group':'tuna_test','native_pid':123,'native_process_identity':{'start_ticks':1}}
            with patch.object(observer,'ROOT',Path(tmp)),patch.object(observer,'process_identity',return_value={'start_ticks':2}),patch.object(observer.os,'kill') as kill:
                with self.assertRaisesRegex(RuntimeError,'ownership changed'):observer.stop_native(marker)
            kill.assert_not_called()
    def test_unobserved_branch_is_coverage_not_fabricated_records(self):
        counts=observer.record_counts(observer.bpf_records(binary_record('arm'),[0]))
        self.assertEqual(counts['arm'],1)
        self.assertEqual(counts['timeout'],0)


if __name__=='__main__': unittest.main()
