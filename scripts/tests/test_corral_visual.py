import copy
import json
from pathlib import Path
import struct
import sys
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
from corral_visual import USER_OBSERVE,candidate_intent,guest_source,native_identity,observation_pair,png,validate_state,wait_state,STATES


class VisualTests(unittest.TestCase):
    def observation(self,state='desktop'):
        names=['Activities','System']
        names+=['Search'] if state=='overview' else []
        names+=['Dark Style','Volume'] if state=='quick-settings' else []
        names+=['Previous month','Next month'] if state=='calendar' else []
        names+=['Search','App Grid','Real App'] if state=='app-grid' else []
        return {'nodes':[{'name':n,'showing':True,'role':'button','ancestors':['App Grid'] if n=='Real App' else []} for n in names],
                'installed_apps':[{'id':'org.real.App.desktop','name':'Real App'}],
                'locked':False,'wayland_session':True,'boot_id':'boot','shell_pid':12,
                'shell_start_ticks':'123','shell_sha256':'abc','session_id':'2','native_backend':{'pid':1}}

    def test_explicit_candidate_intent(self):
        manifest={'candidate_source_commit':'a'*40,'package_selection':{'head_sha':'a'*40,'run_id':42}}
        candidate_intent(manifest,'a'*40,42)
        for candidate,run in [('b'*40,42),('a'*40,43),('a',42),('a'*40,0)]:
            with self.assertRaises(ValueError):candidate_intent(manifest,candidate,run)

    def test_generic_png_dimensions_and_bgr_order(self):
        import zlib
        data=png(2,1,bytes([1,2,3,0,4,5,6,0]))
        self.assertEqual(struct.unpack('>II',data[16:24]),(2,1))
        length=struct.unpack('>I',data[33:37])[0]
        self.assertEqual(zlib.decompress(data[41:41+length]),bytes([0,3,2,1,6,5,4]))
        for width,height,pixels in [(0,1,b''),(1,1,b''),(4097,1,b'')]:
            with self.assertRaises(ValueError):png(width,height,pixels)

    def test_each_state_requires_correct_consumer(self):
        for state in STATES:
            validate_state(state,self.observation(state))
            for other in STATES:
                if other!=state and {state,other}!={'desktop','desktop-return'}:
                    with self.assertRaises(ValueError):validate_state(state,self.observation(other))

    def test_lock_missing_tree_and_wrong_session_refused(self):
        for key,value in [('locked',True),('wayland_session',False),('nodes',[])]:
            observed=self.observation();observed[key]=value
            with self.assertRaises(ValueError):validate_state('desktop',observed)

    def test_pair_rejects_each_identity_change(self):
        before=self.observation()
        for key in ('boot_id','shell_pid','shell_start_ticks','shell_sha256','session_id','native_backend'):
            after=copy.deepcopy(before);after[key]='changed'
            with self.assertRaises(ValueError):observation_pair('desktop',before,after)

    def test_each_partial_menu_pair_is_never_any_named_state(self):
        for menu, members in [('quick-settings', ('Dark Style', 'Volume')),
                              ('calendar', ('Previous month', 'Next month'))]:
            for missing in members:
                partial=self.observation(menu)
                partial['nodes']=[n for n in partial['nodes'] if n['name']!=missing]
                for state in STATES:
                    with self.subTest(menu=menu,missing=missing,state=state):
                        with self.assertRaises(ValueError):validate_state(state,partial)

    def test_partial_tree_cannot_bracket_a_frame(self):
        for menu, members in [('quick-settings', ('Dark Style', 'Volume')),
                              ('calendar', ('Previous month', 'Next month'))]:
            for missing in members:
                partial=self.observation(menu)
                partial['nodes']=[n for n in partial['nodes'] if n['name']!=missing]
                for state in STATES:
                    complete=self.observation(state)
                    with self.assertRaises(ValueError):observation_pair(state,partial,complete)
                    with self.assertRaises(ValueError):observation_pair(state,complete,partial)

    def test_partial_menu_poll_retries_before_complete_or_desktop(self):
        for menu, members in [('quick-settings', ('Dark Style', 'Volume')),
                              ('calendar', ('Previous month', 'Next month'))]:
            for missing in members:
                for requested in (menu,'desktop','desktop-return','overview'):
                    ticks=[0.0];partial=self.observation(menu)
                    partial['nodes']=[n for n in partial['nodes'] if n['name']!=missing]
                    values=[partial,self.observation(requested),self.observation(requested)]
                    calls=[]
                    def observe(remaining):calls.append(remaining);return values.pop(0)
                    result=wait_state(requested,observe,self.observation(),timeout=1,
                                      clock=lambda:ticks[0],sleep=lambda n:ticks.__setitem__(0,ticks[0]+n))
                    self.assertEqual(len(calls),3)
                    self.assertEqual(result['nodes'],self.observation(requested)['nodes'])

    def test_grid_requires_real_installed_app_descendant(self):
        for key,value in [('installed_apps',[]),('installed_apps',[{'id':'other.desktop','name':'Other'}])]:
            bad=self.observation('app-grid');bad[key]=value
            with self.assertRaises(ValueError):validate_state('app-grid',bad)
        bad=self.observation('app-grid')
        for node in bad['nodes']:node['ancestors']=[]
        with self.assertRaises(ValueError):validate_state('app-grid',bad)

    def test_poll_requires_two_fresh_correct_consumer_observations(self):
        ticks=[0.0];values=[self.observation(),self.observation('calendar'),self.observation('calendar')];calls=[]
        def observe(remaining):calls.append(remaining);return values.pop(0)
        result=wait_state('calendar',observe,self.observation(),timeout=1,clock=lambda:ticks[0],sleep=lambda n:ticks.__setitem__(0,ticks[0]+n))
        self.assertEqual(result['nodes'],self.observation('calendar')['nodes'])
        self.assertEqual(len(calls),3);self.assertGreater(calls[0],calls[-1])

    def test_poll_identity_and_missing_consumer_failure_is_immediate(self):
        for key,value in [('shell_pid',99),('shell_start_ticks','new'),('boot_id','reboot'),('nodes',[])]:
            bad=self.observation('calendar');bad[key]=value
            with self.assertRaises(ValueError):wait_state('calendar',lambda remaining:bad,self.observation())

    def test_poll_wrong_or_stalled_state_cannot_be_current(self):
        ticks=[0.0]
        def sleep(n):ticks[0]+=n
        with self.assertRaises(TimeoutError):
            wait_state('app-grid',lambda remaining:self.observation('overview'),self.observation(),timeout=.3,clock=lambda:ticks[0],sleep=sleep)
        def slow(remaining):ticks[0]+=remaining;return self.observation('calendar')
        with self.assertRaises(TimeoutError):
            wait_state('calendar',slow,self.observation(),timeout=.3,clock=lambda:ticks[0],sleep=sleep)

    def test_capture_driver_keeps_return_prerequisite_and_transport_deadline(self):
        source=Path(__file__).resolve().parents[1].joinpath('tuna-corral-visual-capture').read_text()
        compile(source,'capture-driver','exec')
        self.assertIn("receipt['states']['app-grid']['status']!='observed'",source)
        self.assertIn("if name!='desktop-return'",source)
        self.assertIn('timeout=min(remaining,30)',source)
        self.assertIn('/usr/bin/timeout --signal=TERM --kill-after=2s',source)
        self.assertNotIn('time.sleep(.5)',source)

    def test_native_backend_accepts_live_session_receipts(self):
        receipt={'gnome_shell_pids':[],'backend_request':'auto','same_session_scope':True,
                 'shell_descends_from_compositor':True,'primary_drm_fds':[{'path':'/dev/dri/card0'}],
                 'drm_output_journal':[{'message':'tuna-compositor: drm: output VGA-1'}],
                 'compositor_pid':123,'compositor_start_ticks':'456'}
        native_identity(receipt)
        for key,value in [('gnome_shell_pids',[99]),('backend_request','winit'),
                          ('same_session_scope',False),('shell_descends_from_compositor',False),
                          ('primary_drm_fds',[]),('drm_output_journal',[])]:
            bad=copy.deepcopy(receipt);bad[key]=value
            with self.assertRaises(ValueError):native_identity(bad)

    def test_embedded_observer_compiles_and_binds_member_bytes(self):
        source=guest_source({'usr/bin/tuna-shell-gtk':'abc'})
        compile(source,'guest','exec')
        self.assertIn("'usr/bin/tuna-shell-gtk': 'abc'",source)

    def test_observer_uses_installed_gi_atspi_contract(self):
        compile(USER_OBSERVE,'user-observer','exec')
        self.assertNotIn('pyatspi',USER_OBSERVE)
        for api in ("gi.require_version('Atspi','2.0')",'Atspi.get_desktop(0)',
                    'get_process_id()','get_name()','get_role_name()',
                    'get_state_set().contains(Atspi.StateType.SHOWING)',
                    'get_child_count()','get_child_at_index(i)','clear_cache()',
                    'get_action_iface()','get_n_actions()','do_action(0)'):
            self.assertIn(api,USER_OBSERVE)


if __name__=='__main__':unittest.main()
