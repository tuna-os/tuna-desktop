import copy
import hashlib
import json
from pathlib import Path
import sys
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
sys.path.insert(0,str(Path(__file__).resolve().parent))
from corral_guest_cpu import PairCpuReceipts,PAIRED_CASES,bind_idle_phase,collect_idle,idle_receipt,summarize
from test_corral_guest_cpu import sample,BOOT

PHASE={'idle_start':5.99,'idle_end':36.01}
SOURCE={'commit':'a'*40,'sha256':{'helper':'b'*64}}


def raw_for(boot):
    rows=[sample(i) for i in range(31)]
    for row in rows:row['boot_id']=row['boot_after']=boot
    return rows


def write_case(directory,raw,phase=PHASE,source=SOURCE):
    payload=json.dumps(raw).encode();Path(directory,'guest-aggregate-raw.json').write_bytes(payload)
    receipt=idle_receipt(raw,raw[0]['boot_id'],phase);receipt['raw_sha256']=hashlib.sha256(payload).hexdigest()
    return {'guest_aggregate_idle':receipt,'guest_aggregate_admission':{'completed':True,'source':source},'guest_phases_boottime_s':phase}


class ClockAndPairTests(unittest.TestCase):
    def test_boottime_phase_is_explicit_and_every_window_is_bounded(self):
        rows=raw_for(BOOT);summary=summarize(rows,BOOT,PHASE)
        self.assertEqual(summary['guest_idle_boottime_phase'],PHASE)
        for phase in [{'idle_start':6.1,'idle_end':36.01},{'idle_start':5.99,'idle_end':35.9},{'idle_start':True,'idle_end':36.01},{'idle_start':float('nan'),'idle_end':36.01}]:
            with self.assertRaises(ValueError):summarize(rows,BOOT,phase)
        for position in [0,15,30]:
            bad=copy.deepcopy(rows)
            for key in ['begin_boottime_ns','cpu_boottime_ns','end_boottime_ns']:bad[position][key]+=1_000_000_000
            with self.assertRaises(ValueError):summarize(bad,BOOT,PHASE)
        bad=copy.deepcopy(rows);bad[7]['cpu_boottime_ns']=1
        with self.assertRaises(ValueError):summarize(bad,BOOT,PHASE)
        bad=copy.deepcopy(rows);bad[7]['unknown_clock']=0
        with self.assertRaises(ValueError):summarize(bad,BOOT,PHASE)

    def test_all_six_cases_same_source_core_hz_and_unique_boots_required(self):
        pairs=PairCpuReceipts(SOURCE)
        with tempfile.TemporaryDirectory() as directory:
            for i,case in enumerate(PAIRED_CASES):
                boot='79055206-13fe-4e52-8d9e-'+f'{i+1:012d}'
                report=write_case(directory,raw_for(boot));pairs.add(case,report,directory,boot)
                if i<5:
                    with self.assertRaises(ValueError):pairs.finish()
            receipt=pairs.finish();self.assertTrue(receipt['completed']);self.assertEqual(receipt['core_ids'],[0,1]);self.assertEqual(receipt['hz'],100)
            with self.assertRaises(ValueError):pairs.add('1-gnome',report,directory,boot)

    def test_pair_rejects_counterfeit_summary_raw_phase_source_and_boot(self):
        with tempfile.TemporaryDirectory() as directory:
            raw=raw_for(BOOT);good=write_case(directory,raw)
            for name in ('missing','summary','raw-sha','source','admission','phase','boot'):
                report=copy.deepcopy(good)
                if name=='missing':report.pop('guest_aggregate_idle')
                if name=='summary':report['guest_aggregate_idle']['busy_guest_capacity_percent']['median']=0
                if name=='raw-sha':report['guest_aggregate_idle']['raw_sha256']='0'*64
                if name=='source':report['guest_aggregate_admission']['source']['commit']='c'*40
                if name=='admission':report['guest_aggregate_admission']['completed']=False
                if name=='phase':report['guest_phases_boottime_s']={'idle_start':6.1,'idle_end':36.01}
                with self.subTest(name=name):
                    with self.assertRaises(ValueError):PairCpuReceipts(SOURCE).add('1-gnome',report,directory,'changed' if name=='boot' else BOOT)
            Path(directory,'guest-aggregate-raw.json').write_bytes(b'[]')
            with self.assertRaises(ValueError):PairCpuReceipts(SOURCE).add('1-gnome',good,directory,BOOT)

    def test_second_case_core_hz_change_is_refused_even_with_valid_raw(self):
        for change in ('cores','hz','reused-boot'):
            pairs=PairCpuReceipts(SOURCE)
            with tempfile.TemporaryDirectory() as directory:
                report=write_case(directory,raw_for(BOOT));pairs.add('1-gnome',report,directory,BOOT)
                boot=BOOT if change=='reused-boot' else '79055206-13fe-4e52-8d9e-4764ee4c96fe';raw=raw_for(boot)
                for row in raw:
                    if change=='cores':row['online_before']=row['online_after']='0';row['proc_stat']='\n'.join(line for line in row['proc_stat'].splitlines() if not line.startswith('cpu1 '))
                    if change=='hz':row['hz']=250
                report=write_case(directory,raw)
                with self.assertRaises(ValueError):pairs.add('1-tuna',report,directory,boot)

    def test_actual_runner_same_idle_hook_order_for_both_desktops_and_qemu_control(self):
        script=Path(__file__).resolve().parents[1].joinpath('tuna-vm-perf').read_text()
        start=script.index('        time.sleep(30)  # Equal post-panel settle')
        end=script.index('        presentation = guest_probe',start)
        code='\n'.join(line[8:] if line.startswith('        ') else line for line in script[start:end].splitlines())
        for desktop in ('gnome','tuna'):
            events=[]
            class Fake:
                boot_id=BOOT
                def remote(self,command,timeout):return json.dumps(raw_for(BOOT)).encode()
            with tempfile.TemporaryDirectory() as out:
                def clock_probe(agent,action):
                    events.append('marker');return {'boottime_s':PHASE['idle_start'] if events.count('marker')==1 else PHASE['idle_end']}
                def collect(session,path):events.append('collect');return collect_idle(session,path)
                def bind(*args):events.append('bind');return bind_idle_phase(*args)
                admission=SimpleNamespace(finish=lambda:events.append('finish'))
                env={'time':SimpleNamespace(sleep=lambda seconds:events.append('settle'),clock_gettime=lambda clock:1,CLOCK_BOOTTIME=7),
                     'phases':{},'guest_phases':{},'guest_probe':clock_probe,'agent':object(),'args':SimpleNamespace(guest_aggregate_cpu=True,desktop=desktop),
                     'corral':Fake(),'out':Path(out),'collect_idle':collect,'bind_idle_phase':bind,'aggregate_admission':admission}
                exec(compile(code,'actual-idle-phase','exec'),env)
                self.assertEqual(events,['settle','marker','collect','marker','bind','finish'])
                self.assertEqual(env['guest_aggregate']['guest_idle_boottime_phase'],PHASE)
        events=[]
        env={'time':SimpleNamespace(sleep=lambda seconds:events.append(seconds),clock_gettime=lambda clock:1,CLOCK_BOOTTIME=7),
             'phases':{},'guest_phases':{},'guest_probe':lambda *args:{'boottime_s':1},'agent':object(),'args':SimpleNamespace(guest_aggregate_cpu=False)}
        exec(compile(code,'actual-qemu-control','exec'),env);self.assertEqual(events,[30,30])

    def test_actual_wrapper_fresh_boot_loop_owns_every_constructed_candidate(self):
        source=Path(__file__).resolve().parents[1].joinpath('tuna-corral-perf-pairs').read_text()
        start=source.index("   stage='wait-fresh-boot'");end=source.index('   if active.boot_id in boots:',start)
        code='\n'.join(line[3:] for line in source[start:end].splitlines())
        for fault in ('identity','domain','node','metadata','cleanup','mismatch-and-cleanup','transient','stale','success'):
            with self.subTest(fault=fault):
                candidates=[];calls=[];closed=[];clock=[0]
                def monotonic():
                    clock[0]+=1;return clock[0]
                class Candidate:
                    identity={'vm_uid':'owned'}
                    metadata={'guest_domain':'domain','node_uid':'node'}
                    boot_id='new'
                    def close(self):
                        closed.append(self)
                        if fault in ('cleanup','mismatch-and-cleanup'):raise OSError('cleanup failure')
                def construct(*args):
                    calls.append(1)
                    if fault=='transient' and len(calls)==1:raise RuntimeError('not ready')
                    item=Candidate();candidates.append(item)
                    if fault in ('identity','mismatch-and-cleanup'):item.identity={'vm_uid':'replacement'}
                    if fault in ('domain','node'):item.metadata={**item.metadata,('guest_domain' if fault=='domain' else 'node_uid'):'replacement'}
                    if fault=='metadata':item.metadata={}
                    if fault=='cleanup' or (fault=='stale' and len(calls)==1):item.boot_id='old'
                    return item
                env={'Session':construct,'time':SimpleNamespace(monotonic=monotonic,sleep=lambda _:None),
                     'a':SimpleNamespace(context='exact',namespace='owned',vm='owned',user='root',key='key',known_hosts='owned'),
                     'setup':None,'original':{'vm_uid':'owned'},'domain':'domain','node_uid':'node',
                     'selection':{'previous_boot_id':'old'},'active':None,'subprocess':subprocess,'sys':sys}
                if fault in ('success','transient','stale'):
                    exec(compile(code,'actual-paired-fresh-boot','exec'),env)
                    self.assertIs(env['active'],candidates[-1]);self.assertEqual(closed,candidates[:-1])
                    env['active'].close();self.assertEqual(closed,candidates)
                else:
                    error=OSError if fault=='cleanup' else KeyError if fault=='metadata' else ValueError
                    with self.assertRaises(error) as raised:exec(compile(code,'actual-paired-fresh-boot','exec'),env)
                    self.assertEqual(len(calls),1);self.assertEqual(closed,candidates)
                    self.assertIsNone(env['active'])
                    if fault=='mismatch-and-cleanup':
                        self.assertIn('replaced or migrated',str(raised.exception))
                        self.assertEqual(raised.exception.__notes__,['Candidate session cleanup failed: OSError'])

    def test_pair_entrypoint_forwards_original_intent_and_requires_finish(self):
        source=Path(__file__).resolve().parents[1].joinpath('tuna-corral-perf-pairs').read_text();compile(source,'paired-runner','exec')
        for token in ('--corral-export-dir','--corral-import-receipt','--corral-candidate-sha','--corral-package-run','--corral-public-key','aggregate_pairs.add(', 'aggregate_pairs.finish()', "a.profile!='stock'", "completed_cases+=1"):
            self.assertIn(token,source)
        self.assertNotIn('case if stage',source)


if __name__=='__main__':unittest.main()
