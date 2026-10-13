#!/usr/bin/python3
import copy
import importlib.util
import json
from pathlib import Path
import unittest

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location("endpoint",ROOT/"scripts/lib/tuna_strip_endpoint.py")
e=importlib.util.module_from_spec(spec);spec.loader.exec_module(e)

def state():
    return dict(session_mode="scroll",strip_animating=False,rendered_frames=100,
                strip_offset=842.0,strip_view=842.0,focused=3,
                windows=[dict(id=3,rect=[438,48,826,736])])

class Endpoints(unittest.TestCase):
    def test_actual_failure_residual_refused(self):
        s=state();s.update(rendered_frames=99,strip_view=841.809097241329)
        with self.assertRaises(ValueError):e.endpoint(s,826)
    def test_canonical_settled_fresh_expected_client(self):
        self.assertTrue(e.endpoint(state(),826,baseline=99,focused=3))
        self.assertFalse(e.endpoint(state(),826,baseline=100,focused=3))
        self.assertFalse(e.endpoint(state(),826,baseline=99,focused=2))
        self.assertFalse(e.endpoint(state(),616,baseline=99))
    def test_busy_even_when_position_equal_is_not_endpoint(self):
        s=state();s['strip_animating']=True
        self.assertFalse(e.endpoint(s,826))
    def test_missing_or_malformed_state_is_not_readiness(self):
        for key,value in (("strip_animating",None),("strip_animating",0),("rendered_frames",True),
                          ("rendered_frames",-1),("strip_view",float('nan')),("strip_offset",float('inf')),
                          ("strip_view",True),("windows",None),("windows",[dict(id=3,rect=[0,48,826,True])]),
                          ("windows",[dict(id=3,rect=[0,48,826,736]),dict(id=3,rect=[0,48,826,736])])):
            with self.subTest(key=key,value=value):
                s=state();s[key]=value
                with self.assertRaises(ValueError):e.endpoint(s,826)
    def test_fixed_wait_busy_stale_then_terminal_no_new_input(self):
        busy=state();busy.update(strip_animating=True,strip_view=841.809097241329,rendered_frames=99)
        stale=state();stale['rendered_frames']=99
        sequence=iter((busy,stale,state()));clock=[0.0]
        def pause(dt):clock[0]+=dt
        result=e.wait_endpoint(lambda:next(sequence),826,deadline=1,baseline=99,focused=3,now=lambda:clock[0],pause=pause)
        self.assertEqual(result['strip_view'],842.0);self.assertGreater(result['rendered_frames'],99)
        self.assertLess(clock[0],1)
    def test_old_busy_or_frozen_snapshot_never_passes_by_time(self):
        for busy in (False,True):
            s=state();s.update(strip_animating=busy,rendered_frames=99)
            clock=[0.0]
            def pause(dt):clock[0]+=dt
            with self.subTest(busy=busy),self.assertRaises(TimeoutError):
                e.wait_endpoint(lambda:s,826,deadline=.03,baseline=99,now=lambda:clock[0],pause=pause)
    def test_no_animation_endpoint_does_not_require_extra_wait(self):
        result=e.wait_endpoint(state,826,deadline=1,now=lambda:0,pause=lambda _:self.fail('already settled'))
        self.assertFalse(result['strip_animating'])
    def test_producer_wiring_keeps_terminal_publication_after_render(self):
        text=(ROOT/"crates/compositor/src/runtime.rs").read_text()
        start=text.index('        self.publish_overview_previews();\n',text.index('    fn tick('))
        end=text.index('        self.screencast_tick();',start)
        fragment=text[start:end]
        self.assertIn('let strip_was_animating = self.manager.strip_animating();',fragment)
        self.assertLess(fragment.index('self.render()?;'),fragment.index('if (strip_was_animating || strip_lands_immediately) && !self.manager.strip_animating()'))
        self.assertGreater(fragment.index('self.publish_state();'),fragment.index('self.render()?;'))
        self.assertIn('&& !self.input_settings.motion.allows_motion()',fragment)
        self.assertIn('doc["strip_animating"] = serde_json::Value::Bool(self.manager.strip_animating());',text)
    def test_timing_threshold_and_sample_schedule_not_weakened(self):
        capture=(ROOT/"scripts/lib/tuna-scroll-capture.py").read_text()
        verify=(ROOT/"scripts/lib/tuna-scroll-verify.py").read_text()
        self.assertIn('[0, 50, 100, 150, 200, 300, 400, 600, 800]',capture)
        self.assertIn("if not endpoint(observed, width):",capture)
        self.assertIn('settled <= 800',verify)
        self.assertIn("abs(actual['strip_view'] - actual['strip_offset']) < 0.1",verify)

if __name__=="__main__":unittest.main()
