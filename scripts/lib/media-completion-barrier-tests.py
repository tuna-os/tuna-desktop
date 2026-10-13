#!/usr/bin/env python3
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('barrier', Path(__file__).with_name('media-completion-barrier.py'))
barrier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(barrier)
OWNER = {'pid':77,'start_ticks':900}


def rows():
    common = {**OWNER,'actor':'shell'}
    return [{**common,'monotonic_ns':10,'stage':'media-accepted','details':{'instance':1,'token':5,'kind':'key'}},
            {**common,'monotonic_ns':20,'stage':'media-complete','details':{'instance':1,'token':5,'kind':'key','outcome':'success'}},
            {**common,'monotonic_ns':30,'stage':'media-scheduler','details':{'instance':1,'accepted':5,'pending':[], 'busy':False,'clamp_pending':False,'failed':0,'allow_amplified':False,'command_busy':False,'commands':0}}]


class Barrier(unittest.TestCase):
    def validate(self, data, since=1, kind='key', policy=False):
        return barrier.validate(data, OWNER, since, kind, policy)
    def test_actual_completed_then_scheduled_record(self):
        self.assertIsNotNone(self.validate(rows()))
    def test_changed_policy_requires_its_fresh_clamp(self):
        self.assertIsNone(self.validate([rows()[2]], kind='clamp'))
        data=rows()
        for row in data[:2]:row['details']['kind']='clamp'
        self.assertIsNotNone(self.validate(data, kind='clamp'))
        self.assertIsNone(self.validate(data, kind='clamp', since=21))
        self.assertIsNone(self.validate([data[0],data[2]], kind='clamp'))
    def test_policy_change_rejects_unexpected_key_intent(self):
        with self.assertRaises(ValueError):self.validate(rows(), kind='clamp')
    def test_preexisting_completion_requires_typed_kind(self):
        data=rows();data.insert(1,{**copy.deepcopy(data[1]),'monotonic_ns':15,'details':{'instance':1,'token':4,'outcome':'success'}})
        with self.assertRaises(ValueError):self.validate(data)
    def test_acceptance_cannot_follow_its_completion(self):
        data=rows();data[0]['monotonic_ns']=21;data=[data[1],data[0],data[2]]
        with self.assertRaises(ValueError):self.validate(data)
    def test_owner_and_intent_types_are_not_inferred_from_equality(self):
        for field in ['pid','start_ticks']:
            bad=copy.deepcopy(OWNER);bad[field]=True
            with self.assertRaises(ValueError):barrier.validate(rows(),bad,1,'key',False)
        for since,kind,policy in [(True,'key',False),(1,'unknown',False),(1,'key',0)]:
            with self.assertRaises(ValueError):barrier.validate(rows(),OWNER,since,kind,policy)
    def test_child_or_volume_value_without_completion_never_passes(self):
        self.assertIsNone(self.validate([]))
        self.assertIsNone(self.validate([rows()[0],rows()[2]]))
    def test_stale_records_do_not_qualify_press(self):
        self.assertIsNone(self.validate(rows(), since=31))
    def test_completion_alone_is_not_scheduler_barrier(self):
        self.assertIsNone(self.validate(rows()[:2]))
    def test_wrong_owner(self):
        data=rows();data[1]['pid']=78
        with self.assertRaises(ValueError):self.validate(data)
    def test_reused_pid(self):
        data=rows();data[1]['start_ticks']=901
        with self.assertRaises(ValueError):self.validate(data)
    def test_pending_key_clamp_command_or_busy_never_passes(self):
        for key,val in [('pending',[6]),('busy',True),('clamp_pending',True),('command_busy',True),('commands',1)]:
            with self.subTest(key=key):
                data=rows();data[2]['details'][key]=val
                self.assertIsNone(self.validate(data))
    def test_failure(self):
        data=rows();data[1]['details']['outcome']='failure'
        with self.assertRaises(ValueError):self.validate(data)
    def test_later_failure_token(self):
        data=rows();data[2]['details']['failed']=6
        with self.assertRaises(ValueError):self.validate(data)
    def test_older_clamp_failure_after_barrier_start_is_not_ignored(self):
        data=rows();data.insert(1,{**copy.deepcopy(data[1]),'monotonic_ns':15,'details':{'instance':1,'token':4,'kind':'clamp','outcome':'failure'}})
        with self.assertRaises(ValueError):self.validate(data)
    def test_completion_higher_than_scheduler_highwater(self):
        data=rows();data.insert(2,{**copy.deepcopy(data[1]),'monotonic_ns':21,'details':{'instance':1,'token':6,'kind':'clamp','outcome':'success'}})
        with self.assertRaises(ValueError):self.validate(data)
    def test_wrong_completion_token(self):
        data=rows();data[1]['details']['token']=4
        self.assertIsNone(self.validate(data))
    def test_wrong_completion_kind(self):
        data=rows();data[1]['details']['kind']='clamp'
        with self.assertRaises(ValueError):self.validate(data)
    def test_multiple_key_intents(self):
        data=rows();data.insert(1,{**copy.deepcopy(data[0]),'monotonic_ns':11,'details':{'instance':1,'token':6,'kind':'key'}})
        with self.assertRaises(ValueError):self.validate(data)
    def test_instances(self):
        data=rows();data[2]['details']['instance']=2
        with self.assertRaises(ValueError):self.validate(data)
    def test_malformed_bool(self):
        data=rows();data[2]['details']['busy']=0
        with self.assertRaises(ValueError):self.validate(data)
    def test_malformed_pending(self):
        data=rows();data[2]['details']['pending']=[True]
        with self.assertRaises(ValueError):self.validate(data)
    def test_malformed_counts(self):
        for key in ['accepted','failed','commands']:
            data=rows();data[2]['details'][key]=-1
            with self.assertRaises(ValueError):self.validate(data)
    def test_wrong_policy_callback(self):
        self.assertIsNone(self.validate(rows(), policy=True))
    def test_scheduler_before_callback(self):
        data=rows();data[2]['monotonic_ns']=15
        with self.assertRaises(ValueError):self.validate(data)
    def test_stale_scheduler_highwater(self):
        data=rows();data[2]['details']['accepted']=4
        with self.assertRaises(ValueError):self.validate(data)
    def test_superseded_key_is_invalid(self):
        data=rows();data[1]['details']['outcome']='superseded'
        with self.assertRaises(ValueError):self.validate(data)
    def test_coalesced_clamp_is_not_claimed_individually_applied(self):
        data=rows();data[0]['details']['kind']='clamp';data[1]['details'].update(kind='clamp',outcome='superseded')
        self.assertIsNone(self.validate(data, kind='idle'))
        data.insert(1,{**copy.deepcopy(data[0]),'monotonic_ns':11,'details':{'instance':1,'token':6,'kind':'clamp'}})
        data.insert(3,{**copy.deepcopy(data[2]),'monotonic_ns':21,'details':{'instance':1,'token':6,'kind':'clamp','outcome':'success'}})
        data[-1]['details']['accepted']=6
        self.assertIsNotNone(self.validate(data, kind='idle'))
    def test_duplicate_token_or_completion(self):
        for index in [0,1]:
            data=rows();data.insert(index,copy.deepcopy(data[index]))
            with self.assertRaises(ValueError):self.validate(data)
    def test_missing_or_out_of_order_record(self):
        data=rows();del data[0]['monotonic_ns']
        with self.assertRaises(ValueError):self.validate(data)
    def test_malformed_json_log_is_not_a_matcher_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'log';path.write_text('tuna-media-diagnostic: {bad}\n')
            with self.assertRaises(ValueError):barrier.read_rows(path)


if __name__=='__main__':unittest.main()
