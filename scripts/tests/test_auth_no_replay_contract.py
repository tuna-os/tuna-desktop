#!/usr/bin/env python3
"""Source routing checks only; Rust sockets/GTK callback execution belongs to CI."""
from pathlib import Path
import re
import unittest
ROOT=Path(__file__).resolve().parents[2]
class Contract(unittest.TestCase):
 def test_current_unlock_uses_dedicated_encoder_and_never_reconnects_or_replays(self):
  text=(ROOT/'crates/shell-host/src/control.rs').read_text()
  body=text.split('pub fn unlock(',1)[1].split('\n    ///',1)[0]
  self.assertIn('encode_unlock_frame',body);self.assertIn('Shutdown::Both',body)
  self.assertNotIn('write_message',body);self.assertNotIn('reconnect()',body)
  self.assertIn('auth_needs_snapshot',body)
 def test_proof_imports_load_before_any_ci_or_gui_execution(self):
  path=ROOT/'scripts/tuna-auth-no-replay-gtk-proof'
  prefix=path.read_text().split('root=Path.cwd().resolve()',1)[0]
  namespace={'__file__':str(path)}
  exec(compile(prefix,str(path),'exec'),namespace)
  self.assertTrue(callable(namespace['identity'].current_identity))
  self.assertTrue(callable(namespace['lifetime'].bounded_command))
 def test_initial_snapshot_and_pr_checkout_admission(self):
  text=(ROOT/'crates/shell-host/src/control.rs').read_text()
  self.assertIn('auth_needs_snapshot: true',text)
  proof=(ROOT/'scripts/tuna-auth-no-replay-gtk-proof').read_text()
  self.assertNotIn('GITHUB_SHA',proof)
  self.assertIn('identity.current_identity(root)',proof)
  self.assertIn('owned_cleanup',proof)
  self.assertIn('lifetime.bounded_command',proof)
  workflow=(ROOT/'.github/workflows/ci.yml').read_text()
  for job in ('check','gtk-shell'):
   block=re.search(r'^  '+re.escape(job)+r':\n(.*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:|\Z)',workflow,re.M|re.S).group(1)
   self.assertIn('ref: ${{ github.event.pull_request.head.sha || github.sha }}',block)
 def test_generic_writer_refuses_credentials_before_generic_encoder(self):
  text=(ROOT/'crates/shell-host/src/control.rs').read_text().split('fn write_message(',1)[1]
  self.assertLess(text.index('CommandKind::Unlock'),text.index('encode_frame'))
 def test_actual_consumer_clears_fields_and_waits_for_authoritative_lock(self):
  lock=(ROOT/'crates/shell-gtk/src/lock.rs').read_text()
  self.assertIn('None => reset_prompt(entry, message)',lock)
  self.assertIn('entry.set_text("")',lock)
  main=(ROOT/'crates/shell-gtk/src/main.rs').read_text()
  self.assertIn('sync_connection_generation(auth_generation)',main)
  self.assertIn('filter(|c| !c.needs_snapshot())',main)
  self.assertIn('if let Some(locked) = authoritative_lock',main)
 def test_indexed_ui_routes_real_handlers_and_no_unavailable_entry(self):
  text=(ROOT/'crates/shell-gtk/src/lock_auth.rs').read_text()
  for guard in ('self.context.get() == Some(context)', 'self.serial.get() == serial',
                'self.generation.get() == Some(context.generation)', '!self.stopped.get()'):
   self.assertIn(guard,text)
  submit=text.split('fn submit(',1)[1].split('pub fn cancel(',1)[0]
  self.assertLess(submit.index('self.clear_fields()'),submit.index('send(context, answers)'))
  self.assertIn('entry.set_visibility(prompt.style == Style::Visible)',text)
  self.assertIn('gtk::Label::new(Some(&prompt.text))',text)
  self.assertNotIn('set_markup',text)
  main=(ROOT/'crates/shell-gtk/src/main.rs').read_text()
  for caller in ('c.start_interactive_auth()', 'c.answer_interactive_auth(context, answers)',
                 'c.cancel_interactive_auth(context)', 'Ok(Handled::InteractiveAuth(event))',
                 'c.locked_auth_generation()', 'lock_ui.auth_event(event)'):
   self.assertIn(caller,main)
 def test_both_actual_gtk_filters_have_admitted_no_ignore_controls(self):
  proof=(ROOT/'scripts/tuna-auth-no-replay-gtk-proof').read_text()
  for case in ('actual_gtk_auth_send_failure_and_generation_reset',
               'actual_gtk_indexed_prompts_clear_and_reject_stale_callbacks'):
   self.assertIn(case,proof)
  self.assertIn("'--exact','--ignored','--test-threads=1'",proof)
  self.assertIn('1 passed; 0 failed; 0 ignored;',proof)
  self.assertIn("observed={'case':case",proof)
  self.assertIn('identity.current_identity(root)==run_identity',proof)
  self.assertIn('all_monitor_clear stale_callbacks cancel_ack generation_loss policy_absent',proof)
class ActualGtkFilterAdmission(unittest.TestCase):
 """Functional production-loop controls with explicitly synthetic child output."""
 def execute(self, *, bad_index=None, ignored=False, missing_marker=False, cleanup=True, drift=False):
  import ast, contextlib, hashlib, io, tempfile
  from types import SimpleNamespace
  tree=ast.parse((ROOT/'scripts/tuna-auth-no-replay-gtk-proof').read_text())
  with_node=next(node for node in tree.body if isinstance(node,ast.With))
  loop=next(node for node in with_node.body if isinstance(node,ast.For)
            and isinstance(node.target,ast.Tuple) and isinstance(node.target.elts[0],ast.Name)
            and node.target.elts[0].id=='index')
  calls=[]
  with tempfile.TemporaryDirectory() as directory:
   out=Path(directory)
   def run(argv,path,deadline,env):
    index=len(calls);calls.append(argv)
    output='running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'
    if index==bad_index and ignored:output='running 1 test\ntest result: ok. 0 passed; 0 failed; 1 ignored;\n'
    if index==1 and not missing_marker:output+='interactive GTK controls: mixed_styles fresh_password_otp all_monitor_clear stale_callbacks cancel_ack generation_loss policy_absent\n'
    path.write_text(output)
    return {'owned_cleanup':cleanup,'command_error':None,'exit':0}
   def sha(path):
    return 'synthetic-bin-hash' if Path(path)==out/'bin' else hashlib.sha256(Path(path).read_bytes()).hexdigest()
   identity=SimpleNamespace(source_tuple=lambda root: 'changed' if drift else 'synthetic-source',
                            current_identity=lambda root: 'synthetic-run')
   namespace={'cases':['legacy-filter','indexed-filter'],'binary':out/'bin','out':out,
              'receipt':{'cases':[],'synthetic':True},'lifetime':SimpleNamespace(bounded_command=run),
              'identity':identity,'root':out,'source_identity':'synthetic-source','run_identity':'synthetic-run',
              'admission':{'executable_sha256':'synthetic-bin-hash'},'sha':sha,'env':{},
              'time':SimpleNamespace(monotonic=lambda: 0), 'json':__import__('json'),'re':re}
   with contextlib.redirect_stdout(io.StringIO()):
    exec(compile(ast.Module(body=[loop],type_ignores=[]),'actual GTK admission loop','exec'),namespace)
   return len(calls),namespace['receipt']
 def test_actual_loop_requires_both_one_pass_filters(self):
  calls,receipt=self.execute()
  self.assertEqual(calls,2);self.assertEqual(len(receipt['cases']),2)
 def test_ignored_first_case_cannot_qualify_or_run_next(self):
  with self.assertRaises(AssertionError):self.execute(bad_index=0,ignored=True)
 def test_ignored_second_case_cannot_qualify(self):
  with self.assertRaises(AssertionError):self.execute(bad_index=1,ignored=True)
 def test_second_case_missing_actual_marker_cannot_qualify(self):
  with self.assertRaises(AssertionError):self.execute(missing_marker=True)
 def test_unconfirmed_child_cleanup_cannot_qualify(self):
  with self.assertRaises(AssertionError):self.execute(cleanup=False)
 def test_source_drift_cannot_qualify(self):
  with self.assertRaises(AssertionError):self.execute(drift=True)
if __name__=='__main__':unittest.main()
