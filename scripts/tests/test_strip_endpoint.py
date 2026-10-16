#!/usr/bin/python3
import copy
import importlib.util
import json
import hashlib
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
    def test_slow_reader_crosses_deadline_cannot_be_ready(self):
        clock=[0.0]
        def read():clock[0]=1.01;return state()
        with self.assertRaises(TimeoutError):
            e.wait_endpoint(read,826,deadline=1,now=lambda:clock[0],pause=lambda _:None)
    def test_boolean_or_nonpositive_ids_and_invalid_baselines(self):
        for key,value in (("focused",True),("focused",0),("focused",-1),("focused","3"),
                          ("windows",[dict(id=True,rect=[438,48,826,736])]),
                          ("windows",[dict(id=0,rect=[438,48,826,736])])):
            s=state();s[key]=value
            with self.subTest(key=key,value=value),self.assertRaises(ValueError):e.endpoint(s,826)
        for baseline in (True,-1,1.0,"99"):
            with self.subTest(baseline=baseline),self.assertRaises(ValueError):e.endpoint(state(),826,baseline=baseline)
        for expected in (True,0,-1,"3"):
            with self.subTest(expected=expected),self.assertRaises(ValueError):e.endpoint(state(),826,focused=expected)
    def test_image_bracket_rejects_busy_before_later_settled_state(self):
        before=state();before['strip_animating']=True
        with self.assertRaises(ValueError):e.capture_bracket(before,state(),826)
        after=state();after['strip_animating']=True
        with self.assertRaises(ValueError):e.capture_bracket(state(),after,826)
    def test_bracket_preserves_exact_client_geometry_and_frame_order(self):
        before=state();after=state();after['rendered_frames']=101
        e.capture_bracket(before,after,826)
        for key,value in (("rendered_frames",99),("focused",4),("strip_view",842.05),("strip_offset",842.05)):
            bad=copy.deepcopy(after);bad[key]=value
            if key=='focused':bad['windows'][0]['id']=4
            with self.subTest(key=key),self.assertRaises(ValueError):e.capture_bracket(before,bad,826)
        bad=copy.deepcopy(after);bad['windows'][0]['rect'][0]+=1
        with self.assertRaises(ValueError):e.capture_bracket(before,bad,826)
    def test_capture_source_brackets_before_png_encoding(self):
        text=(ROOT/"scripts/lib/tuna-scroll-capture.py").read_text()
        section=text[text.index('def capture(name):'):text.index("capture('01-strip-rest')")]
        self.assertLess(section.index('before = state()'),section.index('ImageGrab.grab'))
        self.assertLess(section.index('ImageGrab.grab'),section.index('observed = state()'))
        self.assertLess(section.index('capture_bracket(before, observed, width)'),section.index('image.save'))
        self.assertEqual(section.count('observed = state()'),1)

    def test_capture_receipt_binds_original_state_image_and_time_order(self):
        image=b"fixture image only";after=state()
        record=dict(before=state(),after=after,width=826,stage="02-strip-preset",
                    png_sha256=hashlib.sha256(image).hexdigest(),before_started=1,
                    capture_started=2,grab_finished=3,after_sampled=4)
        e.capture_receipt(record,after,826,image,"02-strip-preset")
        for key,value in (("stage","view-800"),("png_sha256","0"*64),("grab_finished",5),("capture_started",True),("after_sampled",float('nan'))):
            bad=copy.deepcopy(record);bad[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):e.capture_receipt(bad,after,826,image,"02-strip-preset")
        stale=state();stale['rendered_frames']=101
        with self.assertRaises(ValueError):e.capture_receipt(record,stale,826,image,"02-strip-preset")
        with self.assertRaises(ValueError):e.capture_receipt(record,after,826,b"replaced image","02-strip-preset")

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
        self.assertIn("capture_bracket(before, observed, width)",capture)
        self.assertIn('settled <= 800',verify)
        self.assertIn("abs(actual['strip_view'] - actual['strip_offset']) < 0.1",verify)

if __name__=="__main__":unittest.main()
