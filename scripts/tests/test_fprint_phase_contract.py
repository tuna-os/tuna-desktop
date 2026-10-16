#!/usr/bin/python3
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import os
import subprocess
import sys
import time
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

class LiveIdentity(unittest.TestCase):
    def test_queued_run_requires_exact_running_job_and_green_check(self):
        sha = "a" * 40
        run = dict(id=100, run_attempt=1, repository=dict(full_name=c.REPO),
                   head_sha=sha, path=c.WORKFLOW, event="workflow_dispatch",
                   status="queued", conclusion=None)
        jobs = [dict(id=200, name=c.JOB, status="in_progress", conclusion=None,
                     head_sha=sha, run_id=100, run_attempt=1),
                dict(id=201, name="check", status="completed", conclusion="success",
                     head_sha=sha, run_id=100, run_attempt=1)]
        commit = dict(sha=sha, tree=dict(sha="b" * 40))
        def validate(r=run, j=jobs, source=commit):
            return c.validate_identity(r, j, source, sha, 100, 1, "workflow_dispatch")
        self.assertEqual(validate()["job_id"], 200)
        for index, key, value in ((0, "status", "queued"), (0, "status", "completed"),
                                 (0, "conclusion", "success"), (0, "head_sha", "c" * 40),
                                 (0, "run_id", 101), (0, "run_attempt", 2),
                                 (1, "conclusion", "failure"), (1, "conclusion", None),
                                 (1, "status", "queued"), (1, "head_sha", "c" * 40)):
            wrong = copy.deepcopy(jobs)
            wrong[index][key] = value
            with self.subTest(index=index, key=key), self.assertRaises(ValueError):
                validate(j=wrong)
        for wrong in ([], jobs[:1], jobs + [jobs[0]], jobs + [jobs[1]]):
            with self.subTest(jobs=wrong), self.assertRaises(ValueError):
                validate(j=wrong)
        for status in ("completed", "waiting", "pending", "requested", "unknown", None):
            with self.subTest(status=status), self.assertRaises(ValueError):
                validate(r=dict(run, status=status))
        for key, value in (("id", 101), ("run_attempt", 2), ("head_sha", "c" * 40),
                           ("event", "push"), ("path", "wrong"), ("conclusion", "success"),
                           ("repository", dict(full_name="foreign/repo"))):
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate(r=dict(run, **{key: value}))
        with self.assertRaises(ValueError):
            validate(source=dict(commit, sha="c" * 40))


class Contract(unittest.TestCase):
    def test_ci_mismatch_retains_public_fields_and_excludes_unrelated_payload(self):
        sha = "a" * 40
        run = dict(id=100, run_attempt=1, repository=dict(full_name=c.REPO),
                   head_sha=sha, path=c.WORKFLOW, event="workflow_dispatch",
                   status="completed", conclusion=None, private_payload="excluded fixture value")
        with self.assertRaises(ValueError) as error:
            c.validate_identity(run, [], dict(sha=sha), sha, 100, 1, "workflow_dispatch")
        receipt = json.loads(str(error.exception).split(": ", 1)[1])
        self.assertEqual(receipt["expected"]["status"], ["in_progress", "queued"])
        self.assertEqual(receipt["observed"]["status"], "completed")
        self.assertEqual(receipt["observed"]["head_sha"], sha)
        self.assertEqual(receipt["observed"]["commit_sha"], sha)
        self.assertNotIn("private_payload", receipt["observed"])

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
    def test_exited_leader_still_reserves_pid_at_group_kill(self):
        loader=importlib.machinery.SourceFileLoader("held_wrapper",str(Path(__file__).resolve().parents[1]/"tuna-fprint-phase-proof"))
        ws=importlib.util.spec_from_loader(loader.name,loader);w=importlib.util.module_from_spec(ws);loader.exec_module(w)
        real_killpg=os.killpg; observed=[]
        def checked_killpg(pid,sig):
            # WNOWAIT leaves the exited leader as an owned zombie; no PGID reuse.
            state=Path(f"/proc/{pid}/stat").read_text().rsplit(")",1)[1].split()[0]
            self.assertEqual(state,"Z");observed.append(pid);real_killpg(pid,sig)
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(w.os,"killpg",side_effect=checked_killpg):
            receipt=w.bounded_command([sys.executable,"-c","raise SystemExit(0)"],Path(tmp)/"out",time.monotonic()+2)
            self.assertTrue(receipt["owned_cleanup"]);self.assertEqual(receipt["exit"],0)
            self.assertFalse(receipt["adopted_descendants_observed"]);self.assertEqual(observed,[receipt["pid"]])
            self.assertFalse(Path(f"/proc/{receipt['pid']}").exists())
    def test_setsid_cleared_env_lingering_descendant_refused_and_retired(self):
        loader=importlib.machinery.SourceFileLoader("orphan_wrapper",str(Path(__file__).resolve().parents[1]/"tuna-fprint-phase-proof"))
        ws=importlib.util.spec_from_loader(loader.name,loader);w=importlib.util.module_from_spec(ws);loader.exec_module(w)
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);exe=root/"fixture-child";pidfile=root/"escaped.pid"
            # Separate process/session and empty environment defeats token scans.
            descendant="import os,time;os.environ.clear();time.sleep(20)"
            script="#!/usr/bin/python3\nimport os,json,subprocess\np=subprocess.Popen(['/usr/bin/python3','-c',"+repr(descendant)+"],start_new_session=True,env={})\nopen("+repr(str(pidfile))+",'w').write(str(p.pid))\nr="+repr(vectors()["B"])+"\nr['nonce']=os.environ['TUNA_FPRINT_PHASE_NONCE'];r['pid']=os.getpid()\nprint('FPRINT-PHASE '+json.dumps(r))\nprint('test result: ok. 1 passed; 0 failed; 0 ignored;')\n"
            exe.write_text(script);exe.chmod(0o755)
            admission=dict(files={"tuna_compositor":dict(admitted_executable=str(exe))})
            with mock.patch.object(w,"verify_unchanged"):
                with self.assertRaises(ValueError):w.run_filter(root,admission,0,"fixture",("B",))
            receipt=json.loads((root/"child-0.json").read_text())
            self.assertEqual(receipt["exit"],0);self.assertTrue(receipt["owned_cleanup"])
            self.assertTrue(receipt["adopted_descendants_observed"])
            self.assertFalse((Path("/proc")/pidfile.read_text()).exists())
    def test_preexisting_child_refused_without_killing_it(self):
        loader=importlib.machinery.SourceFileLoader("exclusive_wrapper",str(Path(__file__).resolve().parents[1]/"tuna-fprint-phase-proof"))
        ws=importlib.util.spec_from_loader(loader.name,loader);w=importlib.util.module_from_spec(ws);loader.exec_module(w)
        other=subprocess.Popen([sys.executable,"-c","import time;time.sleep(20)"])
        try:
            with tempfile.TemporaryDirectory() as tmp, mock.patch.object(w.os,"killpg") as kill:
                with self.assertRaises(ValueError):w.bounded_command([sys.executable,"-c","pass"],Path(tmp)/"out",time.monotonic()+2)
                kill.assert_not_called();self.assertIsNone(other.poll())
        finally:other.terminate();other.wait(timeout=2)

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
