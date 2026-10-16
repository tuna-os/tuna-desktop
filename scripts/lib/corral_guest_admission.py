"""Shared original-image/process admission for guest aggregate observations."""
import base64
import hashlib
import json
from pathlib import Path
import subprocess
from corral_session import PHASE, admit_imported_disk
from corral_ssh import client_key
from corral_visual import candidate_intent
from gnome_reference import validate_image, validate_reference, same_reference

PROCESS_BYTES = r"""import hashlib,json,pathlib,pwd,os,sys
selected=json.loads(sys.argv[1]);row=selected['compositor'];p=pathlib.Path('/proc')/str(row['pid'])
boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
def check():
 if p.stat().st_uid!=pwd.getpwnam('tuna-test').pw_uid or os.readlink(p/'exe')!=row['exe'] or int((p/'stat').read_text().rsplit(')',1)[1].split()[19])!=row['start_ticks']:raise ValueError('selected running process changed')
check()
with (p/'exe').open('rb') as stream:actual=hashlib.file_digest(stream,'sha256').hexdigest()
check()
if actual!=row['sha256'] or boot!=selected['boot_id'] or pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()!=boot:raise ValueError('actual running executable or boot differs')
print(json.dumps({'boot_id':boot,'pid':row['pid'],'running_exe_sha256':actual}))
"""


def fingerprint(path):
    with Path(path).open('rb') as stream:return hashlib.file_digest(stream, 'sha256').hexdigest()


def stock_reference(session):
    result = session.python(PHASE, ['stock-reference'])
    session.check_boot(result['transport_boot_id'])
    if result.get('exitcode') != 0 or result.get('exited') is not True or result.get('out-truncated') is not False or result.get('err-truncated') is not False:
        raise ValueError('stock GNOME reference probe failed or incomplete')
    return validate_reference(json.loads(base64.b64decode(result['out-data'], validate=True))['stock_reference'], 'stock')


def image_reference(session):
    result = session.python(PHASE, ['reference-image'])
    session.check_boot(result['transport_boot_id'])
    if result.get('exitcode') != 0 or result.get('exited') is not True or result.get('out-truncated') is not False or result.get('err-truncated') is not False:
        raise ValueError('guest image reference probe failed or incomplete')
    return validate_image(json.loads(base64.b64decode(result['out-data'], validate=True))['reference_image'], 'stock')


def selected_identity(desktop, selected, manifest, reference=None):
    if selected.get('desktop') != desktop:raise ValueError('selected desktop mismatch')
    compositor = selected['compositor']
    if desktop == 'tuna':
        expected = manifest['candidate_required_members']['usr/bin/tuna-compositor']['sha256']
        if compositor['exe'] != '/usr/bin/tuna-compositor' or compositor['sha256'] != expected:
            raise ValueError('selected Tuna bytes differ from original package')
    else:
        validate_reference(reference, 'stock')
        principal = reference['principal']
        if (compositor['exe'], compositor['pid'], compositor['start_ticks'], compositor['sha256']) != ('/usr/bin/gnome-shell', principal['pid'], principal['start'], reference['shell_sha256']):
            raise ValueError('selected stock GNOME 51 process differs from admitted reference')



def source_identity():
    root=Path(__file__).resolve().parents[2]
    commit=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],timeout=10).decode().strip()
    paths=('scripts/tuna-corral-guest-cpu','scripts/tuna-vm-perf','scripts/tuna-corral-perf-pairs',
           'scripts/lib/corral_guest_cpu.py','scripts/lib/corral_guest_admission.py',
           'scripts/lib/corral_session.py','scripts/lib/corral_perf.py',
           'scripts/lib/corral_ssh.py','scripts/lib/corral_visual.py','scripts/lib/gnome_reference.py',
           'scripts/lib/corral_artifact.py','scripts/lib/corral_raw.py','scripts/lib/corral_owned.py',
           'scripts/tuna-corral-benchmark-import')
    hashes={}
    for name in paths:
        blob=subprocess.check_output(['git','-C',str(root),'show','HEAD:'+name],timeout=10)
        hashes[name]=fingerprint(root/name)
        if hashlib.sha256(blob).hexdigest()!=hashes[name]:raise ValueError('observer source differs from committed bytes')
    return {'commit':commit,'sha256':hashes}


class AggregateAdmission:
    def __init__(self,session,options,reference,selected,stock=None):
        self.session=session;self.options=options;self.selected=selected;self.stock=stock
        self.manifest=json.loads((Path(options['export_dir'])/'manifest.json').read_text())
        candidate_intent(self.manifest,options['candidate_sha'],options['package_run'])
        if self.manifest.get('gnome_profile')!='stock':raise ValueError('aggregate comparison requires stock GNOME 51 export')
        self.reference=validate_image(json.loads(Path(options['reference_image']).read_text()),'stock')
        if self.reference!=reference or image_reference(session)!=reference:raise ValueError('guest original image reference differs')
        self.imported=self.admit_disk()
        key=client_key(self.manifest,options['key'],options.get('public_key') or options['key']+'.pub','root')
        if stock is not None and stock['image']!=reference:raise ValueError('stock process original image differs')
        selected_identity(options['desktop'],selected,self.manifest,stock)
        self.running=session.python(PROCESS_BYTES,selected);session.check_boot(self.running['boot_id'])
        self.source=source_identity()
        self.evidence={'source':self.source,'candidate_source_commit':options['candidate_sha'],
                       'package_run':options['package_run'],'package_sha256':self.manifest['package_sha256'],
                       'fixture_image_id':self.manifest['fixture_image_id'],'export_observer_source_commit':self.manifest['observer_source_commit'],
                       'export_manifest_sha256':fingerprint(Path(options['export_dir'])/'manifest.json'),
                       'original_import':self.imported,'client_key':key,'reference':reference,
                       'selected_before':selected,'running_bytes_before':self.running,'session':session.metadata,
                       'completed':False}

    def admit_disk(self):
        o=self.options
        return admit_imported_disk(o['export_dir'],o['import_receipt'],o['context'],o['namespace'],o['vm'],
                                   o['reference_image'],o['tuna_payload'],o['tuna_selection'])

    def finish(self):
        session=self.session;session.verify();final=session.desktop(self.options['desktop'])
        if final!=self.selected:raise ValueError('selected compositor changed during idle window')
        running=session.python(PROCESS_BYTES,final);session.check_boot(running['boot_id'])
        if running!=self.running:raise ValueError('actual running bytes changed during idle window')
        if self.stock is not None:same_reference(self.stock,stock_reference(session),'stock')
        if image_reference(session)!=self.reference or self.admit_disk()!=self.imported:raise ValueError('original image/import changed')
        if source_identity()!=self.source:raise ValueError('observer source changed during idle window')
        self.evidence.update(selected_after=final,running_bytes_after=running,completed=True)
        return self.evidence
