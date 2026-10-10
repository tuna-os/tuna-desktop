"""Bounded ownership recovery and atomic UID-preconditioned Kubernetes cleanup."""
import json,subprocess,time
PATHS={'vm':'/apis/kubevirt.io/v1/namespaces/{}/virtualmachines/{}','dv':'/apis/cdi.kubevirt.io/v1beta1/namespaces/{}/datavolumes/{}','pvc':'/api/v1/namespaces/{}/persistentvolumeclaims/{}','pod':'/api/v1/namespaces/{}/pods/{}'}

def get(context,namespace,kind,name):
 r=subprocess.run(['kubectl','--context',context,'get',kind,name,'-n',namespace,'--ignore-not-found','-o','json'],capture_output=True,check=True,timeout=30)
 return json.loads(r.stdout) if r.stdout.strip() else None

def delete(context,namespace,kind,name,uid):
 body={'apiVersion':'v1','kind':'DeleteOptions','preconditions':{'uid':uid},'propagationPolicy':'Background'}
 r=subprocess.run(['kubectl','--context',context,'delete','--raw',PATHS[kind].format(namespace,name),'-f','-'],input=json.dumps(body).encode(),capture_output=True,timeout=30)
 if r.returncode:
  # Only an independently confirmed absence is success. Conflicts remain fatal.
  if get(context,namespace,kind,name) is not None:raise RuntimeError('UID-preconditioned deletion refused: '+r.stderr.decode(errors='replace')[:1024])

def create(context,namespace,kind,obj,path,nonce,register):
 obj['metadata'].setdefault('labels',{})['tuna-perf-nonce']=nonce
 path.write_text(json.dumps(obj,indent=2));name=obj['metadata']['name']
 try:
  r=subprocess.check_output(['kubectl','--context',context,'create','-f',str(path),'-o','json'],timeout=45)
  result=json.loads(r)
 except BaseException:
  # A timed-out create can already have committed. Recover only our unpredictable
  # ownership label; registering precedes rethrow so caller cleanup can run.
  result=get(context,namespace,kind,name)
  if result is not None:
   if result['metadata'].get('labels',{}).get('tuna-perf-nonce')!=nonce:raise ValueError('ambiguous create resolved to foreign resource')
   register(kind,name,result['metadata']['uid'])
  raise
 if result['metadata'].get('labels',{}).get('tuna-perf-nonce')!=nonce:raise ValueError('created resource ownership nonce differs')
 register(kind,name,result['metadata']['uid']);return result

def cleanup(context,namespace,owned,timeout=180):
 errors=[]
 for kind,name,uid in reversed(owned):
  try:delete(context,namespace,kind,name,uid)
  except Exception as error:errors.append(str(error))
 # Track transitive pod/PVC dependents by actual owner UID, including scratch.
 uids={x[2] for x in owned};deadline=time.monotonic()+timeout
 while time.monotonic()<deadline:
  pending=[]
  for kind in ('vm','dv','pvc','pod'):
   r=subprocess.check_output(['kubectl','--context',context,'get',kind,'-n',namespace,'-o','json'],timeout=30)
   for q in json.loads(r)['items']:
    md=q['metadata']
    if md['uid'] in uids or any(o.get('uid') in uids for o in md.get('ownerReferences',[])):
     uids.add(md['uid']);pending.append((kind,md['name'],md['uid']))
  if not pending:
   if errors:raise RuntimeError('; '.join(errors))
   return {'all_owned_resources_absent':True,'tracked_uids':sorted(uids)}
  time.sleep(1)
 raise TimeoutError('owned cleanup still pending: '+repr(pending)+'; '+ '; '.join(errors))

def placement(context,namespace,claim,node):
 pv=json.loads(subprocess.check_output(['kubectl','--context',context,'get','pv',claim['spec']['volumeName'],'-o','json'],timeout=30))
 ref=pv['spec'].get('claimRef',{})
 if ref.get('uid')!=claim['metadata']['uid'] or ref.get('namespace')!=namespace:raise ValueError('target PV claim identity differs')
 terms=pv['spec'].get('nodeAffinity',{}).get('required',{}).get('nodeSelectorTerms',[])
 if not terms or any(not any(e.get('key')=='kubernetes.io/hostname' and e.get('operator')=='In' and e.get('values')==[node] for e in t.get('matchExpressions',[])) for t in terms):raise ValueError('target PV does not exclusively select requested node')
 return pv
