import copy
import contextlib
import io
import json
from unittest.mock import patch
import importlib.machinery
import importlib.util
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from corral_guest_cpu import COLLECT, FIELDS, delta, memory_capacity, online_ids, parse_sample, stat_counters, summarize, collect_idle

BOOT = '79055206-13fe-4e52-8d9e-4764ee4c96fd'


def sample(index=0):
    # Two cores: 20user+10systembusy, 10steal,20iowait,140idle persecond.
    # guest10 is part of user20, not additional CPU.
    values = [100+20*index, 0, 50+10*index, 1000+140*index, 20+20*index, 0, 0, 10+10*index, 50+10*index, 0]
    row = ' '.join(map(str, values));core = ' '.join(str(v//2) for v in values)
    return {'boot_id': BOOT, 'boot_after': BOOT, 'begin_monotonic_ns': 1_000_000_000+index*1_000_000_000,
            'cpu_monotonic_ns': 1_001_000_000+index*1_000_000_000, 'end_monotonic_ns': 1_002_000_000+index*1_000_000_000,
            'online_before': '0-1\n', 'online_after': '0,1\n', 'hz': 100,
            'proc_stat': 'cpu '+row+'\ncpu0 '+core+'\ncpu1 '+core+'\n',
            'meminfo': 'MemTotal: 4096 kB\nMemAvailable: 3000 kB\n'}


def change_counter(value, field, amount):
    lines = []
    for line in value['proc_stat'].splitlines():
        parts = line.split();parts[1+FIELDS.index(field)] = str(int(parts[1+FIELDS.index(field)])+amount);lines.append(' '.join(parts))
    value['proc_stat'] = '\n'.join(lines)


class GuestCpuTests(unittest.TestCase):
    def test_guest_columns_not_counted_twice_and_cores_explicit(self):
        row = delta(parse_sample(sample()), parse_sample(sample(1)))
        self.assertEqual(row['busy_one_core_percent'], 30)
        self.assertEqual(row['busy_guest_capacity_percent'], 15)
        self.assertEqual(row['steal_guest_capacity_percent'], 5)
        self.assertEqual(row['iowait']['guest_capacity_percent'], 10)
        self.assertEqual(row['busy_accounted_time_fraction']['percent'], 15)
        self.assertEqual(row['counter_deltas']['cpu']['guest'], 10)
        self.assertEqual(row['core_ids'], [0, 1])

    def test_invalid_cpu_schema_missing_duplicate_counter_and_guest_subset(self):
        raw = sample()
        for text in ['', 'cpu 1 2\n', raw['proc_stat']+'cpu 1 2\n', raw['proc_stat'].replace('100 ', '-1 ', 1), raw['proc_stat'].replace('100 ', 'true ', 1)]:
            with self.assertRaises(ValueError):stat_counters(text, [0, 1])
        bad=sample();change_counter(bad, 'guest', 999)
        with self.assertRaises(ValueError):parse_sample(bad)

    def test_online_ranges_duplicate_and_topology_drift(self):
        self.assertEqual(online_ids('0-2,4'), [0, 1, 2, 4])
        for text in ['', '3-1', '0,0', '4096', '1.0', '0-4096']:
            with self.assertRaises(ValueError):online_ids(text)
        for field in ['online_before', 'online_after']:
            bad=sample(1);bad[field]='0'
            with self.assertRaises(ValueError):parse_sample(bad)
        before=parse_sample(sample());after=parse_sample(sample(1));after['core_ids']=[0]
        with self.assertRaises(ValueError):delta(before, after)

    def test_clock_boot_hz_drift_and_invalid_memory_refused(self):
        for field,value in [('boot_id','bad'), ('boot_after','changed'), ('hz',0), ('hz',True), ('cpu_monotonic_ns',0), ('cpu_monotonic_ns',float('nan')), ('end_monotonic_ns',1)]:
            bad=sample();bad[field]=value
            with self.assertRaises(ValueError):parse_sample(bad)
        for field,value in [('boot_id','a'), ('hz',250), ('cpu_monotonic_ns',1_001_000_000)]:
            before=parse_sample(sample());after=parse_sample(sample(1));after[field]=value
            with self.assertRaises(ValueError):delta(before,after)
        for text in ['', 'MemTotal: 1 kB\n', 'MemTotal: 1 kB\nMemAvailable: 2 kB', 'MemTotal: 1 kB\nMemAvailable: 0 MB', 'MemTotal: 1 kB\nMemTotal: 1 kB\nMemAvailable: 0 kB']:
            with self.assertRaises(ValueError):memory_capacity(text)

    def test_reset_refused_but_iowait_regression_unavailable_not_zero(self):
        for field in FIELDS:
            if field=='iowait':continue
            before=sample();after=sample(1);change_counter(after,field,-10000)
            with self.assertRaises(ValueError):delta(parse_sample(before),parse_sample(after))
        after=sample(1);change_counter(after,'iowait',-11)
        row=delta(parse_sample(sample()),parse_sample(after))
        self.assertEqual(row['busy_guest_capacity_percent'],15)
        self.assertEqual(row['iowait']['status'],'unavailable')
        self.assertNotIn('guest_capacity_percent',row['iowait'])
        self.assertEqual(row['busy_accounted_time_fraction']['status'],'unavailable')
        self.assertLess(row['counter_deltas']['cpu0']['iowait'],0)

    def test_summary_keeps_raw_intervals_memory_estimate_and_scope(self):
        report=summarize([sample(i) for i in range(31)],BOOT)
        self.assertEqual(report['busy_guest_capacity_percent']['samples'],30)
        self.assertEqual(report['busy_guest_capacity_percent']['median'],15)
        self.assertEqual(report['memory_capacity_samples'][0]['used_estimate_kib'],1096)
        self.assertIn('collector/SSH',report['scope'])
        self.assertTrue(report['existing_uid_cpu_and_pss_unchanged'])
        for rows,boot in [([sample(i) for i in range(20)],BOOT),([sample(i) for i in range(31)],'changed')]:
            with self.assertRaises(ValueError):summarize(rows,boot)
        rows=[sample(i) for i in range(31)];rows[15]['boot_after']='changed'
        with self.assertRaises(ValueError):summarize(rows,BOOT)

    def test_guest_source_has_owned_deadline_and_no_mutating_commands(self):
        compile(COLLECT,'guest-collector','exec')
        for token in ('time.monotonic_ns()',"read('/proc/stat')","read('/proc/meminfo')", "seconds!=30", 'guest collection deadline', 'limit+1'):
            self.assertIn(token,COLLECT)
        for token in ('subprocess','systemctl','gsettings','write_text','open(path,\'w\')'):
            self.assertNotIn(token,COLLECT)

    def test_actual_inline_collector_reads_raw_kernel_files_with_absolute_deadline(self):
        clock=[1.0];calls=[0]
        def monotonic_ns():calls[0]+=1;return int(clock[0]*1e9)+calls[0]
        def read(path):
            index=int(clock[0]-1)
            files={'/proc/sys/kernel/random/boot_id':BOOT,'/sys/devices/system/cpu/online':'0-1',
                   '/proc/stat':sample(index)['proc_stat'],'/proc/meminfo':sample()['meminfo']}
            return io.StringIO(files[path])
        def sleep(seconds):clock[0]+=seconds
        output=io.StringIO()
        with patch('builtins.open',side_effect=read),patch('time.monotonic',side_effect=lambda:clock[0]),patch('time.monotonic_ns',side_effect=monotonic_ns),patch('time.sleep',side_effect=sleep),patch('os.sysconf',return_value=100),patch('sys.argv',['collector','{"seconds":30}']),contextlib.redirect_stdout(output):
            exec(COLLECT,{})
        raw=json.loads(output.getvalue());self.assertEqual(len(raw),31)
        self.assertAlmostEqual(summarize(raw,BOOT)['busy_guest_capacity_percent']['median'],15)
        def stalled(path):
            if path=='/proc/stat':clock[0]+=35
            return read(path)
        clock[0]=1.0
        with patch('builtins.open',side_effect=stalled),patch('time.monotonic',side_effect=lambda:clock[0]),patch('time.monotonic_ns',side_effect=monotonic_ns),patch('os.sysconf',return_value=100),patch('sys.argv',['collector','{"seconds":30}']):
            with self.assertRaises(TimeoutError):exec(COLLECT,{})

    def test_host_hook_retains_invalid_raw_and_uses_bounded_transport(self):
        import tempfile
        class FakeSession:
            boot_id=BOOT
            def remote(self,command,timeout):
                self.command=command;self.timeout=timeout
                return json.dumps(self.raw).encode()
        session=FakeSession();session.raw=[sample(i) for i in range(31)]
        with tempfile.TemporaryDirectory() as out:
            result=collect_idle(session,out)
            self.assertEqual(session.timeout,42)
            self.assertIn('--kill-after=2s 35s',session.command)
            self.assertEqual(result['busy_guest_capacity_percent']['median'],15)
            session.raw[4].pop('proc_stat')
            with self.assertRaises(ValueError):collect_idle(session,out)
            retained=json.loads(Path(out,'guest-aggregate-raw.json').read_text())
            self.assertNotIn('proc_stat',retained[4])

    def test_cli_selected_process_provenance_and_admission_guards(self):
        path=Path(__file__).resolve().parents[1]/'tuna-corral-guest-cpu'
        loader=importlib.machinery.SourceFileLoader('guest_cpu_cli',str(path));spec=importlib.util.spec_from_loader(loader.name,loader);module=importlib.util.module_from_spec(spec);loader.exec_module(module)
        selected={'desktop':'tuna','compositor':{'exe':'/usr/bin/tuna-compositor','sha256':'a'*64}}
        manifest={'candidate_required_members':{'usr/bin/tuna-compositor':{'sha256':'a'*64}}}
        module.selected_identity('tuna',selected,manifest)
        for field,value in [('exe','/usr/bin/gnome-shell'),('sha256','b'*64)]:
            bad=copy.deepcopy(selected);bad['compositor'][field]=value
            with self.assertRaises(ValueError):module.selected_identity('tuna',bad,manifest)
        source=path.read_text()
        for token in ('candidate_intent(', 'admit_imported_disk(', 'client_key(', "gnome_profile') != 'stock'", 'same_reference(', 'session.verify()', "(p/'exe').open('rb')"):
            self.assertIn(token,source)
        compile(module.PROCESS_BYTES,'actual-exe-probe','exec')
        reference={'image':{'schema':1,'profile':'stock','mutter_package':'mutter 51.0-1','shell_package':'gnome-shell 51.0-1','packages_sha256':'c'*64,'critical_sha256':'d'*64},
                   'principal':{'uid':1000,'pid':12,'start':123,'owner':':1.2'},'shell_version':'GNOME Shell 51.0','shell_sha256':'e'*64,
                   'modules':{name:{'path':'/usr/lib/'+name+'.so','device':1,'inode':2,'uid':0,'bytes':3,'sha256':'f'*64,'package':'mutter 51.0-1'} for name in ('core','clutter','cogl')}}
        selected={'desktop':'gnome','compositor':{'exe':'/usr/bin/gnome-shell','pid':12,'start_ticks':123,'sha256':'e'*64}}
        module.selected_identity('gnome',selected,{},reference)
        for field,value in [('exe','/usr/bin/tuna-compositor'),('pid',13),('start_ticks',124),('sha256','a'*64)]:
            bad=copy.deepcopy(selected);bad['compositor'][field]=value
            with self.assertRaises(ValueError):module.selected_identity('gnome',bad,{},reference)


if __name__ == '__main__':unittest.main()
