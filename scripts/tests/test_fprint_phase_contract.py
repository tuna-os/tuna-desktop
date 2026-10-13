#!/usr/bin/python3
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import importlib.machinery
from unittest import mock
import unittest

spec = importlib.util.spec_from_file_location("c",Path(__file__).resolve().parents[1]/"lib/fprint_phase_contract.py")
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
NONCE = "a"*32

def vectors():
    measurements = {
        "A1":dict(scope="synthetic_socketpair",capacity=1,outq=2,eagain=True,send_parked=True,shutdown_released_error=True,sender_joined=True),
        "A2":dict(scope="synthetic_zbus_signal",capacity=1,outq=2,inq=3,c2d=4096,d2c=4,send_parked=True,close_parked=True,shutdown_released_error=True,sender_joined=True,closer_joined=True),
        "B":dict(scope="public_verify",branch="pending",inq=2,c2d=256,d2c=5,elapsed_ms=3000,claims=0,peer_live_at_retirement=True,claim_free_before_teardown=True,outcome="TimedOut"),
    }
    return {k:dict(phase=k,nonce=NONCE,pid=123,driver_exit="unobserved",measurements=v) for k,v in measurements.items()}

def output(v):
    return "\n".join("FPRINT-PHASE "+json.dumps(r) for r in v.values())+"\ntest result: ok. 1 passed; 0 failed; 0 ignored; 51 filtered out; finished in 3s\n"

class Contract(unittest.TestCase):
    def test_positive(self):
        v=vectors(); self.assertEqual(c.phase_vectors(output(v),NONCE,123,v),v); c.positive(v)
    def test_nonfill_explicitly_not_qualified(self):
        v=vectors(); v["B"]["measurements"].update(branch="non_fill",inq=0)
        parsed=c.phase_vectors(output(v),NONCE,123,v)
        self.assertEqual(parsed["B"]["measurements"]["branch"],"non_fill")
        with self.assertRaises(ValueError): c.positive(parsed)
    def test_binding_and_records(self):
        base=vectors()
        for phase in base:
            for key,value in (("nonce","b"*32),("pid",124),("driver_exit","retired"),("phase","X")):
                with self.subTest(phase=phase,key=key):
                    v=copy.deepcopy(base);v[phase][key]=value
                    with self.assertRaises(ValueError): c.phase_vectors(output(v),NONCE,123,base)
        for text in (output(base)+"FPRINT-PHASE "+json.dumps(base["B"]),output({"A1":base["A1"]}),"FPRINT-PHASE broken",output(base).replace("1 passed","0 passed"),output(base).replace("0 ignored","1 ignored")):
            with self.subTest(text=text):
                with self.assertRaises(ValueError): c.phase_vectors(text,NONCE,123,base)
    def test_measurement_negatives(self):
        base=vectors()
        for phase in base:
            for key,value in base[phase]["measurements"].items():
                v=copy.deepcopy(base)
                if isinstance(value,bool): v[phase]["measurements"][key]=False
                elif isinstance(value,int):
                    if key in ("elapsed_ms","claims"):v[phase]["measurements"][key]=15000
                    else:v[phase]["measurements"][key]=0
                else:v[phase]["measurements"][key]="wrong"
                with self.subTest(phase=phase,key=key):
                    with self.assertRaises(ValueError):c.positive(v)
    def test_build_roles(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)/"source";target=Path(tmp)/"target";target.mkdir();root.mkdir()
            messages=[]
            for name,kind,test,filename in (("tuna_compositor","lib",True,"lib.rs"),("tuna-compositor","bin",False,"main.rs")):
                exe=target/name;exe.write_text(name)
                messages.append(dict(reason="compiler-artifact",executable=str(exe),target=dict(name=name,kind=[kind],src_path=str(root/"crates/compositor/src"/filename)),profile=dict(test=test),package_id="path+"+(root/"crates/compositor").as_uri()+"#tuna-compositor@0.1.0"))
            self.assertEqual(len(c.select_artifacts(messages,root,target)),2)
            for index in (0,1):
                for field,value in (("kind",["bin" if index==0 else "lib"]),("src_path",str(root/"fake.rs"))):
                    bad=copy.deepcopy(messages);bad[index]["target"][field]=value
                    with self.assertRaises(ValueError):c.select_artifacts(bad,root,target)
                for field,value in (("package_id","fake@0.1.0"),("profile",dict(test=index==1))):
                    bad=copy.deepcopy(messages);bad[index][field]=value
                    with self.assertRaises(ValueError):c.select_artifacts(bad,root,target)
            with self.assertRaises(ValueError):c.select_artifacts(messages[:1],root,target)
    def test_actual_owned_child_wrapper_and_failure_receipts(self):
        loader=importlib.machinery.SourceFileLoader("wrapper",str(Path(__file__).resolve().parents[1]/"tuna-fprint-phase-proof"))
        ws=importlib.util.spec_from_loader(loader.name,loader);w=importlib.util.module_from_spec(ws);loader.exec_module(w)
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); exe=root/"fixture-child"
            # This Python child tests wrapper custody only; never a real FP qualification.
            exe.write_text("#!/usr/bin/python3\nimport os,json\nr="+repr(vectors()["B"])+"\nr['nonce']=os.environ['TUNA_FPRINT_PHASE_NONCE'];r['pid']=os.getpid()\nprint('FPRINT-PHASE '+json.dumps(r))\nprint('test result: ok. 1 passed; 0 failed; 0 ignored;')\n")
            exe.chmod(0o755)
            admission=dict(files={"tuna_compositor":dict(admitted_executable=str(exe))})
            with mock.patch.object(w,"verify_unchanged"):
                result=w.run_filter(root,admission,0,"fixture",("B",))
                self.assertEqual(result["B"]["measurements"]["branch"],"pending")
                receipt=json.loads((root/"child-0.json").read_text());self.assertEqual(receipt["exit"],0);self.assertTrue(receipt["owned_cleanup"])
                exe.write_text("#!/usr/bin/python3\nraise SystemExit(4)\n")
                with self.assertRaises(ValueError):w.run_filter(root,admission,1,"fixture",("B",))
                self.assertEqual(json.loads((root/"child-1.json").read_text())["exit"],4)
                exe.write_text("#!/usr/bin/python3\nimport time\ntime.sleep(10)\n")
                with mock.patch.object(w,"CHILD_TIMEOUT",0.1):
                    with self.assertRaises(ValueError):w.run_filter(root,admission,2,"fixture",("B",))
                receipt=json.loads((root/"child-2.json").read_text());self.assertEqual(receipt["exit"],124);self.assertTrue(receipt["owned_cleanup"])
    def test_hash_drift_refuses_admission(self):
        loader=importlib.machinery.SourceFileLoader("hash_wrapper",str(Path(__file__).resolve().parents[1]/"tuna-fprint-phase-proof"))
        ws=importlib.util.spec_from_loader(loader.name,loader);w=importlib.util.module_from_spec(ws);loader.exec_module(w)
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/"bin";p.write_text("before")
            source={"sha":"fixture"}; admission=dict(source=source,files={"test":dict(cargo_executable=str(p),admitted_executable=str(p),sha256=c.digest(p))})
            with mock.patch.object(w.contract,"source_tuple",return_value=source):
                w.verify_unchanged(admission);p.write_text("changed")
                with self.assertRaises(ValueError):w.verify_unchanged(admission)

    def test_api_identity(self):
        sha="a"*40
        run=dict(id=1,run_attempt=1,repository=dict(full_name=c.REPO),head_sha=sha,path=c.WORKFLOW,event="workflow_dispatch",status="in_progress",conclusion=None)
        jobs=[dict(id=2,name=c.JOB,status="in_progress",conclusion=None,head_sha=sha,run_id=1,run_attempt=1),dict(id=3,name="check",status="completed",conclusion="success",head_sha=sha,run_id=1,run_attempt=1)]
        commit=dict(sha=sha,tree=dict(sha="b"*40))
        c.validate_identity(run,jobs,commit,sha,1,1,"workflow_dispatch")
        for key,value in (("head_sha","wrong"),("path","wrong"),("id",2),("run_attempt",2),("event","push"),("status","completed")):
            bad=copy.deepcopy(run);bad[key]=value
            with self.assertRaises(ValueError):c.validate_identity(bad,jobs,commit,sha,1,1,"workflow_dispatch")
        for index in (0,1):
            for key in ("head_sha","run_id","run_attempt","conclusion"):
                bad=copy.deepcopy(jobs);bad[index][key]="wrong"
                with self.assertRaises(ValueError):c.validate_identity(run,bad,commit,sha,1,1,"workflow_dispatch")

if __name__=="__main__":unittest.main()
