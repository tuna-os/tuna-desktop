import sys,struct,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
import corral_perf as m
from unittest.mock import patch
import subprocess
class Stream:
 def __init__(self,data):self.data=bytearray(data);self.sent=bytearray()
 def settimeout(self,value):pass
 def recv(self,n):result=bytes(self.data[:n]);del self.data[:n];return result
 def sendall(self,data):self.sent.extend(data)
 def close(self):pass
def update(x,w,raw):return b'\x00\x00\x00\x01'+struct.pack('>HHHHi',x,0,w,1,0)+raw
def stream(data):return Stream(b'RFB 003.008\n\x01\x01'+b'\x00'*4+struct.pack('>HH',2,1)+b'\x00'*20+data)
class Tests(unittest.TestCase):
 def test_api_call_inherits_remaining_deadline(self):
  with patch.object(m.time,'monotonic',return_value=10),patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],0,stdout=b'{}')) as run:
   m.kubectl('ctx','get','pods',deadline=12)
  self.assertEqual(run.call_args.kwargs['timeout'],2)
  with patch.object(m.time,'monotonic',return_value=13),patch.object(m.subprocess,'run') as run:
   with self.assertRaises(TimeoutError):m.kubectl('ctx','get','pods',deadline=12)
  run.assert_not_called()

 def test_quantities(self):
  for value,count in [('1k',1000),('2',2),('0',0),('1.5k',1500)]:self.assertEqual(m.resource_count(value),count)
  for value in ['-1','0.1','1Gi','bad']:
   with self.assertRaises(ValueError):m.resource_count(value)
 def test_names(self):
  m.node_name('ip-10-20-1-12')
  for name in ['--raw','../pods','a/b','']:
   with self.assertRaises(ValueError):m.node_name(name)
 def test_capacity(self):
  node={'metadata':{'name':'node','uid':'uid'},'status':{'conditions':[{'type':'Ready','status':'True'},{'type':'DiskPressure','status':'False'}],'allocatable':{'devices.kubevirt.io/'+k:'1k' for k in ('kvm','tun','vhost-net')}}}
  fs={'capacityBytes':100*m.GIB,'availableBytes':20*m.GIB};stats={'node':{'fs':fs,'runtime':{'imageFs':fs,'containerFs':fs}}}
  r=m.capacity_receipt(node,stats,estimated_peak_bytes=4*m.GIB);self.assertTrue(r['ready_for_disk_admission']);self.assertFalse(r['performance_qualified']);self.assertFalse(r['gpu']['qualified']);self.assertFalse(r['total_power']['qualified'])
  for peak in [0,5*m.GIB]:self.assertFalse(m.capacity_receipt(node,stats,estimated_peak_bytes=peak)['ready_for_disk_admission'])
  node['spec']={'taints':[{'effect':'NoSchedule'}]};self.assertFalse(m.capacity_receipt(node,stats,estimated_peak_bytes=m.GIB)['ready_for_disk_admission'])
  self.assertFalse(m.capacity_receipt(node,{},estimated_peak_bytes=m.GIB)['ready_for_disk_admission'])
 def test_pinned_container_disk_default_preserves_exact_sources(self):
  import copy
  volumes=[{'name':'disk','containerDisk':{'image':'repo@sha256:'+'a'*64}},{'name':'cloud','cloudInitNoCloud':{'userData':'synthetic exact bytes'}}]
  vm={'metadata':{'name':'vm','namespace':'ns','uid':'vm-uid'},'spec':{'template':{'spec':{'volumes':copy.deepcopy(volumes)}}}}
  vmi={'metadata':{'name':'vm','namespace':'ns','uid':'vmi-uid','ownerReferences':[{'kind':'VirtualMachine','uid':'vm-uid','controller':True}]},'status':{'phase':'Running','nodeName':'node'},'spec':{'volumes':copy.deepcopy(volumes)}}
  vmi['spec']['volumes'][0]['containerDisk']['imagePullPolicy']='IfNotPresent'
  identity=m.vm_identity(vm,vmi);self.assertEqual(m.verify_identity(identity,vm,vmi),identity)
  for field,value in [('image','repo@sha256:'+'b'*64),('imagePullPolicy','Always'),('imagePullPolicy','Never')]:
   old=vmi['spec']['volumes'][0]['containerDisk'].copy();vmi['spec']['volumes'][0]['containerDisk'][field]=value
   with self.assertRaises(ValueError):m.vm_identity(vm,vmi)
   vmi['spec']['volumes'][0]['containerDisk']=old
  vmi['spec']['volumes'][1]['cloudInitNoCloud']['userData']='different bytes'
  with self.assertRaises(ValueError):m.vm_identity(vm,vmi)
  vmi['spec']['volumes'][1]=copy.deepcopy(volumes[1]);vm['spec']['template']['spec']['volumes'][0]['containerDisk']['imagePullPolicy']='Never'
  with self.assertRaises(ValueError):m.vm_identity(vm,vmi)
 def test_identity_replacement_and_migration(self):
  vm={'metadata':{'name':'vm','namespace':'ns','uid':'vm-uid'},'spec':{'template':{'spec':{'volumes':[]}}}}
  vmi={'metadata':{'name':'vm','namespace':'ns','uid':'vmi-uid','ownerReferences':[{'kind':'VirtualMachine','uid':'vm-uid','controller':True}]},'status':{'phase':'Running','nodeName':'node'},'spec':{'volumes':[]}}
  identity=m.vm_identity(vm,vmi);self.assertEqual(m.verify_identity(identity,vm,vmi),identity)
  vmi['metadata']['uid']='replacement'
  with self.assertRaises(ValueError):m.verify_identity(identity,vm,vmi)
  vmi['metadata']['uid']='vmi-uid';vmi['status']['migrationState']={}
  with self.assertRaises(ValueError):m.verify_identity(identity,vm,vmi)
  vmi['status'].pop('migrationState');vmi['spec']['volumes']=[{'name':'replaced'}]
  with self.assertRaises(ValueError):m.vm_identity(vm,vmi)
  vmi['metadata']['ownerReferences']=[]
  with self.assertRaises(ValueError):m.vm_identity(vm,vmi)
 def test_pixels_keys(self):
  s=stream(update(0,2,b'abcdefgh'));r=m.Rfb(s);self.assertEqual(r.snapshot(),(2,1,b'abcdefgh'));r.keys(0xffeb,32)
  self.assertEqual(s.sent[-32:],b''.join(struct.pack('>BBHI',4,d,0,k) for d,k in [(1,0xffeb),(1,32),(0,32),(0,0xffeb)]))
 def test_partial(self):
  with self.assertRaises(EOFError):m.Rfb(stream(update(0,1,b'abcd'))).snapshot()
 def test_disjoint(self):self.assertEqual(m.Rfb(stream(update(0,1,b'abcd')+update(1,1,b'efgh'))).snapshot()[2],b'abcdefgh')
 def test_bounds(self):
  with self.assertRaises(ValueError):m.Rfb(stream(update(2,1,b'abcd'))).snapshot()
if __name__=='__main__':unittest.main()
