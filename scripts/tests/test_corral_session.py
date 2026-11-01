import contextlib,io,json,os,pathlib,struct,subprocess,sys,tempfile,types,unittest
from unittest.mock import patch
sys.path.insert(0,str(pathlib.Path(__file__).resolve().parents[1]/'lib'))
import corral_session as m

class Tests(unittest.TestCase):
 def test_native_ssh_argv_explicitly_isolates_host_stores(self):
  with tempfile.TemporaryDirectory() as d:
   known=pathlib.Path(d)/'owned_hosts';key=pathlib.Path(d)/'key';args=m.ssh_args('ctx','ns','root',key,known)
   self.assertEqual(args[args.index('--known-hosts')+1],str(known))
   self.assertIn('--local-ssh-opts=-o UserKnownHostsFile='+str(known),args)
   self.assertIn('--local-ssh-opts=-o GlobalKnownHostsFile=/dev/null',args)
   self.assertIn('--local-ssh-opts=-o StrictHostKeyChecking=accept-new',args)
   self.assertNotIn('--local-ssh-opts=-o StrictHostKeyChecking=no',args)
 def test_launcher_matches_actual_vmi_owner_and_node(self):
  s=m.Session.__new__(m.Session);s.context='ctx';s.namespace='ns';s.name='vm';s.identity={'vmi_uid':'instance','node':'node'}
  pod={'metadata':{'name':'launcher','ownerReferences':[{'uid':'instance','kind':'VirtualMachineInstance','controller':True}]},'spec':{'nodeName':'node'}}
  with patch.object(m,'kubectl',return_value={'items':[pod]}) as call:
   self.assertEqual(s.launcher(),pod)
  self.assertIn('kubevirt.io/created-by=instance',call.call_args.args)
  for kind,controller,uid,node in [('VirtualMachine',True,'instance','node'),('VirtualMachineInstance',False,'instance','node'),('VirtualMachineInstance',True,'replacement','node'),('VirtualMachineInstance',True,'instance','other')]:
   foreign={'metadata':{'ownerReferences':[{'uid':uid,'kind':kind,'controller':controller}]},'spec':{'nodeName':node}}
   with patch.object(m,'kubectl',return_value={'items':[foreign]}):
    with self.assertRaises(ValueError):s.launcher()
  with patch.object(m,'kubectl',return_value={'items':[pod,pod]}):
   with self.assertRaises(ValueError):s.launcher()
 def test_boot_attribution(self):
  s=m.Session.__new__(m.Session);s.boot_id=None;s.metadata={}
  boot='01234567-0123-0123-0123-012345678901';s.check_boot(boot);s.check_boot(boot)
  with self.assertRaises(ValueError):s.check_boot('wrong')
  with self.assertRaises(ValueError):s.check_boot('11234567-0123-0123-0123-012345678901')
 def test_fixed_command_rejection(self):
  s=m.Session.__new__(m.Session)
  for args in [dict(path='/bin/sh',arg=['clock'],**{'capture-output':True}),dict(path='/usr/libexec/tuna-perf-phase',arg=['injected'],**{'capture-output':True}),dict(path='/usr/libexec/tuna-perf-phase',arg=['clock',';touch /tmp/x'],**{'capture-output':True})]:
   with self.assertRaises(ValueError):s.command('guest-exec',**args)
 def test_fixed_bpf_read_action_uses_existing_phase_transport(self):
  for index in ('0','1023'):
   with self.subTest(index=index):
    s=m.Session.__new__(m.Session);s.counter=0;s.responses={}
    status={'exited':True,'exitcode':0,'transport_boot_id':'original-boot'}
    args=['kernel-bpf-read','--index',index]
    with patch.object(s,'python',return_value=status.copy()) as execute,patch.object(s,'check_boot') as boot:
     self.assertEqual(s.command('guest-exec',path='/usr/libexec/tuna-perf-phase',arg=args,**{'capture-output':True}),{'pid':1})
     execute.assert_called_once_with(m.PHASE,args);boot.assert_called_once_with('original-boot')
     self.assertEqual(s.command('guest-exec-status',pid=1),{'exited':True,'exitcode':0})
  for action in ('kernel-bpf-read-extra','kernel-arbitrary-read'):
   with self.assertRaises(ValueError):
    s.command('guest-exec',path='/usr/libexec/tuna-perf-phase',arg=[action,'--index','0'],**{'capture-output':True})
 def test_status_token_once(self):
  s=m.Session.__new__(m.Session);s.responses={1:{'exited':True}}
  self.assertTrue(s.command('guest-exec-status',pid=1)['exited'])
  with self.assertRaises(KeyError):s.command('guest-exec-status',pid=1)
 def test_bounded_process_timeout(self):
  with self.assertRaises(TimeoutError):m.bounded_run([sys.executable,'-c','import time;time.sleep(30)'],.04)
 def test_bounded_process_failure(self):
  with self.assertRaises(RuntimeError):m.bounded_run([sys.executable,'-c','raise SystemExit(2)'],2)
 def test_owned_listener(self):
  import socket
  with socket.socket() as sock:
   sock.bind(('127.0.0.1',0));sock.listen();port=sock.getsockname()[1]
   self.assertTrue(m.owned_listener(os.getpid(),port));self.assertFalse(m.owned_listener(os.getppid(),port))
 def test_guest_phase_actual_output_and_exit(self):
  real=subprocess.Popen
  def child(command,**kwargs):
   self.assertEqual(command[:5],['/usr/bin/timeout','--signal=TERM','--kill-after=2s','20s','/usr/libexec/tuna-perf-phase'])
   return real([sys.executable,'-c','import json;print(json.dumps({"boottime_s":10}))'],**kwargs)
  output=io.StringIO()
  with patch.object(subprocess,'Popen',side_effect=child),patch.object(sys,'argv',['phase','["clock"]']),contextlib.redirect_stdout(output):exec(m.PHASE,{})
  result=json.loads(output.getvalue());self.assertEqual(result['exitcode'],0);self.assertFalse(result['out-truncated'])
 def test_independent_guest_timeout(self):
  real=subprocess.Popen
  def child(command,**kwargs):
   self.assertEqual(command[0],'/usr/bin/timeout')
   return real(['/usr/bin/timeout','--signal=TERM','--kill-after=2s','.05s',sys.executable,'-c','import time;time.sleep(30)'],**kwargs)
  output=io.StringIO()
  with patch.object(subprocess,'Popen',side_effect=child),patch.object(sys,'argv',['phase','["clock"]']),contextlib.redirect_stdout(output):exec(m.PHASE,{})
  self.assertEqual(json.loads(output.getvalue())['exitcode'],124)
 def test_journal_unit_and_boot_attribution(self):
  boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
  row={'_BOOT_ID':boot.replace('-',''),'_SYSTEMD_UNIT':'tuna-perf.service','MESSAGE':'sample'}
  def run(row):
   output=io.StringIO()
   with patch.object(subprocess,'run',return_value=types.SimpleNamespace(stdout=json.dumps(row).encode()+b"\n")),contextlib.redirect_stdout(output):exec(m.JOURNAL,{})
   return json.loads(output.getvalue())
  self.assertEqual(run(row)['messages'],['sample'])
  for field,value in [('_BOOT_ID','another-boot'),('_SYSTEMD_UNIT','untrusted.service'),('MESSAGE',[] )]:
   invalid=dict(row);invalid[field]=value
   with self.assertRaises(ValueError):run(invalid)
 def test_import_wrong_bytes_rejected_before_cluster_access(self):
  with tempfile.TemporaryDirectory() as temp:
   root=pathlib.Path(temp);(root/'disk.qcow2').write_bytes(b'actualbytes')
   (root/'manifest.json').write_text(json.dumps({'schema':1,'export_succeeded':True,'paired_desktops':['gnome','tuna'],'disk_sha256':'0'*64}))
   (root/'import.json').write_text('{}')
   with patch.object(m,'kubectl') as cluster:
    with self.assertRaises(ValueError):m.admit_imported_disk(root,root/'import.json','context','ns','vm','missing','missing','missing')
    cluster.assert_not_called()
 def test_session_selection_real_files_and_no_immutable_write(self):
  realpath=pathlib.Path
  with tempfile.TemporaryDirectory() as temp:
   root=realpath(temp)
   for file,text in {'/etc/gdm/custom.conf':'[daemon]\nDefaultSession=gnome.desktop\n', '/usr/lib/tmpfiles.d/tuna-vm-lane.conf':r'f+ /var/lib/AccountsService/users/tuna-test 0600 root root - [User]\nSession=gnome\nSystemAccount=false\n', '/usr/share/wayland-sessions/tuna.desktop':'[Desktop Entry]\n', '/proc/sys/kernel/random/boot_id':'01234567-0123-0123-0123-012345678901'}.items():
    p=root/file.lstrip('/');p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text);p.chmod(0o644)
   original=(root/'usr/lib/tmpfiles.d/tuna-vm-lane.conf').read_bytes()
   class GuestPath:
    def __init__(self,value):self.p=root/str(value).lstrip('/')
    def __getattr__(self,name):return getattr(self.p,name)
    def __fspath__(self):return str(self.p)
    def lstat(self):
     s=self.p.lstat();return types.SimpleNamespace(st_mode=s.st_mode,st_size=s.st_size,st_uid=0)
   out=io.StringIO()
   with patch.object(pathlib,'Path',GuestPath),patch.object(sys,'argv',['select','"tuna"']),patch.object(subprocess,'run') as reboot,contextlib.redirect_stdout(out):exec(m.SELECT,{})
   self.assertIn('DefaultSession=tuna.desktop',(root/'etc/gdm/custom.conf').read_text())
   self.assertEqual((root/'var/lib/AccountsService/users/tuna-test').read_text(),'[User]\nSession=tuna\nSystemAccount=false\n')
   self.assertIn(r'Session=tuna\n',(root/'etc/tmpfiles.d/tuna-vm-lane.conf').read_text())
   self.assertEqual(original,(root/'usr/lib/tmpfiles.d/tuna-vm-lane.conf').read_bytes())
   self.assertTrue(json.loads(out.getvalue())['reboot_requested']);self.assertIn('/usr/bin/systemctl',reboot.call_args.args[0])
if __name__=='__main__':unittest.main()
