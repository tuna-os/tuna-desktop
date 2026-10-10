"""Bind fixture client keys and measured guest host keys; never predict host keys."""
import hashlib,json,re,subprocess
from pathlib import Path
HOST_KEYS='''import pathlib,json,subprocess
boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
keys=[]
for path in pathlib.Path('/etc/ssh').glob('ssh_host_*_key.pub'):
 if path.is_symlink() or not path.is_file() or path.stat().st_size>4096:raise ValueError('invalid host public key')
 p=subprocess.run(['ssh-keygen','-lf',str(path)],capture_output=True,check=True,timeout=5)
 keys.append({'path':str(path),'fingerprint':p.stdout.decode().split()[1]})
if not keys:raise ValueError('guest host public keys missing')
if pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()!=boot:raise ValueError('guest rebooted during host key acquisition')
print(json.dumps({'boot_id':boot,'keys':keys}))
'''

def export_policy(manifest):
 ssh=manifest.get('ssh',{})
 if ssh.get('user')!='root' or ssh.get('password_authentication') is not False or ssh.get('authorized_keys_path')!='/etc/ssh/authorized_keys/root' or not re.fullmatch('[0-9a-f]{64}',ssh.get('public_key_sha256','')) or not re.fullmatch(r'SHA256:[A-Za-z0-9+/]{43}',ssh.get('public_key_fingerprint','')):raise ValueError('original exported public-key-only SSH contract missing')
 return ssh

def client_key(manifest,private,public,user):
 expected=export_policy(manifest)
 if user!=expected['user']:raise ValueError('SSH user differs from fixture')
 public=Path(public)
 if hashlib.sha256(public.read_bytes()).hexdigest()!=expected['public_key_sha256']:raise ValueError('exact exported public key file differs')
 fingerprint=subprocess.check_output(['ssh-keygen','-lf',str(public)],timeout=10).decode().split()[1]
 derived=subprocess.check_output(['ssh-keygen','-y','-f',str(private)],timeout=10)
 actual=subprocess.run(['ssh-keygen','-lf','-'],input=derived,capture_output=True,check=True,timeout=10).stdout.decode().split()[1]
 if fingerprint!=expected['public_key_fingerprint'] or actual!=fingerprint:raise ValueError('selected private/public key pair differs from exported identity')
 return {'user':user,'public_key_sha256':expected['public_key_sha256'],'public_key_fingerprint':fingerprint,'private_key_recorded':False}

def host_receipt(value,known_hosts,boot_id):
 if value.get('boot_id')!=boot_id or not value.get('keys'):raise ValueError('measured host keys have wrong boot identity')
 keys=value['keys']
 if any(not re.fullmatch(r'SHA256:[A-Za-z0-9+/]{43}',k.get('fingerprint','')) for k in keys):raise ValueError('invalid measured guest host fingerprint')
 path=Path(known_hosts)
 if path.stat().st_size>1024*1024:raise ValueError('owned known_hosts exceeds bound')
 fingerprints={line.split()[1] for line in subprocess.check_output(['ssh-keygen','-lf',str(path)],timeout=10).decode().splitlines()}
 matched=sorted(fingerprints & {k['fingerprint'] for k in keys})
 if not matched:raise ValueError('authenticated SSH known_hosts does not match measured guest host key')
 return {'measured_after_boot':True,'boot_id':boot_id,'guest_public_host_keys':keys,'matched_known_hosts_fingerprints':matched,'owned_known_hosts_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'transport':'Kubernetes API tunnel; initial TOFU accept-new; existing keys strictly checked'}
