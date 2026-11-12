#!/usr/bin/env python3
"""Unit tests for the GNOME-relative performance budget gate (#315).

Synthetic artifact trees only; the real paired measurement stays in
performance-baseline.yml.
"""
import contextlib
import copy
import io
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import gnome_reference  # noqa: E402
import perf_budget as pb  # noqa: E402

loader = importlib.machinery.SourceFileLoader("gate", str(ROOT / "scripts/tuna-perf-gate"))
spec = importlib.util.spec_from_loader("gate", loader)
gate = importlib.util.module_from_spec(spec)
loader.exec_module(gate)

RUN = "https://github.com/tuna-os/tuna-desktop/actions/runs/{}"
PHASES = {"idle": (10, 40), "overview": (50, 70), "settle": (80, 100)}


def samples(load, names=True, start=0, end=101):
    """One sample per second; load maps (phase or None) to {name: percent}."""
    rows, ticks = [], {}
    for second in range(start, end):
        phase = next((p for p, (a, b) in PHASES.items() if a <= second - 1 and second <= b), None)
        per = load.get(phase, load.get(None, {}))
        processes = []
        for name, percent in sorted(per.items()):
            ticks[name] = ticks.get(name, 0) + percent  # hz=100, one second
            pid = 100 + list(ticks).index(name)
            row = {"pid": pid, "start_ticks": pid, "cpu_ticks": round(ticks[name])}
            if names:
                row.update(comm=name[:15], cmd=name)
            processes.append(row)
        rows.append({"monotonic_s": float(second), "boottime_s": float(second), "hz": 100,
                     "cpu_interval_start_boottime_s": float(second - 1) if second > start else None,
                     "cpu_observed_percent": sum(per.values()) if second > start else None,
                     "processes": processes})
    return rows


def memory_samples(memory, start=0, end=101):
    """One sample per second; memory(second) is {process name: PSS MiB} of the processes alive then."""
    rows, pids = [], {}
    for second in range(start, end):
        processes = []
        for name, mib in sorted(memory(second).items()):
            pid = pids.setdefault(name, 100 + len(pids))
            processes.append({"pid": pid, "start_ticks": pid, "cpu_ticks": 0, "comm": name[:15], "cmd": name,
                              "pss_kib": round(mib * 1024), "rss_kib": round(mib * 2048)})
        rows.append({"monotonic_s": float(second), "boottime_s": float(second), "hz": 100,
                     "cpu_interval_start_boottime_s": float(second - 1) if second > start else None,
                     "cpu_observed_percent": 0.0 if second > start else None, "unreadable": [],
                     "processes": processes, "pss_kib": sum(p["pss_kib"] for p in processes)})
    return rows


def write_session(directory, load, overview_ms=200, pacing=16.7, names=True, rows=None):
    directory.mkdir(parents=True)
    phases = {f"{p}_{edge}": float(v) for p, (a, b) in PHASES.items() for edge, v in (("start", a), ("end", b))}
    (directory / "report.json").write_text(json.dumps(
        {"presentation_cadence": {"interval_ms": {"p95": pacing, "p99": pacing * 2}}}))
    (directory / "samples.json").write_text(json.dumps(samples(load, names) if rows is None else rows))
    (directory / "guest-phases.json").write_text(json.dumps(phases))
    (directory / "observations.json").write_text(json.dumps(
        [{"index": i, "upper_s": overview_ms / 1000} for i in range(20)]))


GNOME = {None: {"gnome-shell": 2}, "overview": {"gnome-shell": 20, "nautilus": 5}}
TUNA = {None: {"tuna-compositor": 2, "tuna-shell-gtk": 1},
        "overview": {"tuna-compositor": 30, "tuna-shell-gtk": 6, "Xwayland": 2, "nautilus": 5}}


def tree(root, tuna=TUNA, outlier=None, pairs=3, **kwargs):
    for index in range(pairs):
        base = Path(root) / ("" if index == 0 else f"repeat-{index + 1}")
        write_session(base / "gnome", GNOME)
        write_session(base / "tuna", outlier if (outlier and index == 2) else tuna, **kwargs)


def budgets(**metrics):
    return {"schema": 1, "target_ratio": 1.0, "margin": 0.15, "metrics": metrics}


def budget(max_ratio, ceiling=None, floor=1.0, enforced=True, evidence=RUN.format(1), **extra):
    return dict(max_ratio=max_ratio, ceiling=ceiling, floor=floor, enforced=enforced, evidence=evidence, **extra)


class Attribution(unittest.TestCase):
    def test_process_names_map_to_roles(self):
        for name, role in (("tuna-compositor", "compositor"), ("gnome-shell", "compositor"),
                           ("tuna-shell-gtk", "shell"), ("tuna-shell-host", "shell"), ("gjs", "shell"),
                           ("Xwayland", "xwayland"), ("xdg-desktop-portal-gnome", "portals"),
                           ("xdg-desktop-portal", "portals"), ("gsd-color", "services"),
                           ("tuna-session", "services"), ("pipewire", "services"),
                           ("nautilus", "apps"), ("tuna-frame-pacer", "apps"), (None, None)):
            self.assertEqual(pb.classify(name), role, name)

    def test_argv_name_wins_over_truncated_comm(self):
        self.assertEqual(pb.process_name({"comm": "xdg-desktop-por", "cmd": "xdg-desktop-portal-gtk"}),
                         "xdg-desktop-portal-gtk")
        self.assertEqual(pb.process_name({"comm": "Xwayland", "cmd": None}), "Xwayland")
        self.assertIsNone(pb.process_name({}))

    def test_intervals_attribute_roles_and_never_charge_a_new_process_its_history(self):
        rows = samples({None: {"tuna-compositor": 10, "tuna-shell-gtk": 4}}, start=0, end=3)
        rows[2]["processes"].append({"pid": 900, "start_ticks": 900, "cpu_ticks": 5000, "cmd": "nautilus"})
        intervals = pb.cpu_intervals(rows)
        self.assertEqual(len(intervals), 2)
        _, _, roles, named = intervals[1]
        self.assertTrue(named)
        self.assertAlmostEqual(roles["compositor"], 10)
        self.assertAlmostEqual(roles["shell"], 4)
        self.assertAlmostEqual(roles["desktop"], 14)
        self.assertAlmostEqual(roles["apps"], 0)
        self.assertAlmostEqual(roles["total"], 14)

    def test_phases_use_only_wholly_contained_intervals_and_need_three(self):
        rows = samples({None: {"tuna-compositor": 1}, "overview": {"tuna-compositor": 50}})
        result = pb.phase_cpu(rows, {"overview_start": 50.0, "overview_end": 70.0,
                                     "idle_start": 10.0, "idle_end": 12.5, "typing_start": 5.0})
        self.assertEqual(set(result), {"overview"})  # idle has two intervals; typing has no end
        self.assertEqual(result["overview"]["compositor"]["count"], 20)
        self.assertAlmostEqual(result["overview"]["compositor"]["mean"], 50)
        self.assertTrue(result["overview"]["named"])

    def test_unnamed_samples_report_only_the_total(self):
        result = pb.phase_cpu(samples({None: {"a": 3}}, names=False), {"idle_start": 10.0, "idle_end": 40.0})
        self.assertEqual(set(result["idle"]) - {"named", "window_boottime_s"}, {"total"})
        self.assertFalse(result["idle"]["named"])


def run_gate(args):
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        return gate.main(args)


def run_gate_output(args):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        status = gate.main(args)
    return status, out.getvalue(), err.getvalue()


class Gate(unittest.TestCase):
    def evaluate(self, budget_file, **kwargs):
        with tempfile.TemporaryDirectory() as directory:
            tree(directory, **kwargs)
            pairs = pb.discover_pairs(directory)
            self.assertEqual(len(pairs), kwargs.get("pairs", 3))
            return {row["metric"]: row for row in pb.evaluate(budget_file, pb.compare(pairs))}

    def test_pairs_are_discovered_for_both_profiles(self):
        with tempfile.TemporaryDirectory() as directory:
            tree(directory)
            tree(Path(directory) / "stock", pairs=2)
            pairs = pb.discover_pairs(directory)
            self.assertEqual([(p["profile"], p["repeat"]) for p in pairs],
                             [("diagnostic", 1), ("diagnostic", 2), ("diagnostic", 3), ("stock", 1), ("stock", 2)])

    def test_diagnostic_pairs_cannot_supply_missing_stock_repeats(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree(root / "artifacts")
            tree(root / "artifacts" / "stock", pairs=2)
            path, out = root / "budgets.json", root / "gate.json"
            path.write_text(json.dumps(budgets(**{"cpu.overview.total.mean": budget(99)})))
            self.assertEqual(run_gate(["check", "--artifacts", str(root / "artifacts"),
                                       "--budgets", str(path), "--out", str(out)]), 1)
            result = json.loads(out.read_text())
            self.assertEqual(result["profile"], "stock")
            self.assertEqual(len(result["pairs"]), 2)
            self.assertEqual(result["results"][0]["status"], "missing")

    def test_diagnostic_values_cannot_hide_stock_regression(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree(root / "artifacts", overview_ms=200)
            tree(root / "artifacts" / "stock", overview_ms=600)
            path, out = root / "budgets.json", root / "gate.json"
            path.write_text(json.dumps(budgets(**{"overview.response_ms.p50": budget(2.5)})))
            self.assertEqual(run_gate(["check", "--artifacts", str(root / "artifacts"),
                                       "--budgets", str(path), "--out", str(out)]), 1)
            result = json.loads(out.read_text())
            self.assertEqual({p["profile"] for p in result["pairs"]}, {"stock"})
            self.assertEqual(result["results"][0]["status"], "fail")
            self.assertEqual(result["results"][0]["ratio"], 3.0)

    def test_ratio_and_ceiling_pass_and_fail(self):
        rows = self.evaluate(budgets(**{
            "cpu.overview.desktop.mean": budget(1.9),            # 36 / 20 = 1.8
            "cpu.overview.total.mean": budget(1.5),              # 43 / 25 = 1.72
            "cpu.overview.compositor.mean": budget(2.0, ceiling=25),
            "overview.response_ms.p50": budget(1.0, floor=0)}), overview_ms=200)
        self.assertEqual(rows["cpu.overview.desktop.mean"]["status"], "pass")
        self.assertAlmostEqual(rows["cpu.overview.desktop.mean"]["ratio"], 1.8)
        self.assertEqual(rows["cpu.overview.total.mean"]["status"], "fail")
        self.assertIn("ratio 1.72 > budget 1.50", rows["cpu.overview.total.mean"]["reason"])
        self.assertEqual(rows["cpu.overview.compositor.mean"]["status"], "fail")
        self.assertIn("ceiling", rows["cpu.overview.compositor.mean"]["reason"])
        self.assertTrue(rows["overview.response_ms.p50"]["meets_target"])

    def test_floor_keeps_a_near_zero_gnome_value_from_exploding_the_ratio(self):
        quiet = copy.deepcopy(TUNA)
        quiet[None] = {"tuna-compositor": 0.5}
        gnome_idle = {"cpu.idle.total.mean": budget(1.0, floor=1.0)}
        rows = self.evaluate(budgets(**gnome_idle), tuna=quiet)
        # GNOME idles at 2% here; with a 1% floor 0.5% is comfortably within.
        self.assertEqual(rows["cpu.idle.total.mean"]["status"], "pass")
        self.assertAlmostEqual(pb.ratio(0.5, 0.0, 1.0), 0.5)

    def test_median_absorbs_one_disturbed_pair(self):
        noisy = copy.deepcopy(TUNA)
        noisy["overview"] = {"tuna-compositor": 90}
        rows = self.evaluate(budgets(**{"cpu.overview.desktop.mean": budget(1.9)}), outlier=noisy)
        self.assertEqual(rows["cpu.overview.desktop.mean"]["status"], "pass")

    def test_budget_floor_does_not_claim_the_match_or_beat_target(self):
        metric = "cpu.idle.desktop.mean"
        config = budgets(**{metric: budget(1.0, floor=1.0)})
        for gnome, tuna, meets in ((0.1, 0.5, False), (0.1, 0.1, True),
                                   (0.0, 0.5, False), (0.0, 0.0, True),
                                   (0.0, 1e-12, False)):
            with self.subTest(gnome=gnome, tuna=tuna):
                values = [{"gnome": gnome, "tuna": tuna}] * 3
                row = pb.evaluate(config, {metric: values})[0]
                self.assertEqual(row["status"], "pass")
                self.assertEqual(row["meets_target"], meets)
                # Infinite zero-baseline ratios are internal only, never JSON.
                json.dumps(row, allow_nan=False)

    def test_match_or_beat_target_retains_the_paired_median(self):
        metric = "cpu.idle.desktop.mean"
        config = budgets(**{metric: budget(1.0, floor=1.0)})
        values = [{"gnome": 0.1, "tuna": 0.05},
                  {"gnome": 0.2, "tuna": 0.2},
                  {"gnome": 0.0, "tuna": 0.5}]
        self.assertTrue(pb.evaluate(config, {metric: values})[0]["meets_target"])
        config["target_ratio"] = 0.9
        self.assertFalse(pb.evaluate(config, {metric: values})[0]["meets_target"])

    def test_provisional_budgets_warn_and_missing_enforced_metrics_fail(self):
        rows = self.evaluate(budgets(**{
            "cpu.overview.total.mean": budget(1.0, enforced=False),
            "cpu.window_cycling.total.mean": budget(2.0)}))
        self.assertEqual(rows["cpu.overview.total.mean"]["status"], "warn")
        self.assertEqual(rows["cpu.window_cycling.total.mean"]["status"], "missing")
        self.assertEqual([r["metric"] for r in pb.failed(list(rows.values()))], ["cpu.window_cycling.total.mean"])

    def test_too_few_pairs_fail_an_enforced_budget(self):
        rows = self.evaluate(budgets(**{"cpu.overview.total.mean": budget(5.0)}), pairs=2)
        self.assertEqual(rows["cpu.overview.total.mean"]["status"], "missing")

    def test_invalid_measurement_cannot_pass_by_nan_comparison(self):
        metric = "cpu.overview.total.mean"
        for value in (float("nan"), float("inf"), -1, True, "1"):
            for desktop in pb.DESKTOPS:
                values = [{"gnome": 1, "tuna": 1} for _ in range(3)]
                values[0][desktop] = value
                rows = pb.evaluate(budgets(**{metric: budget(2)}), {metric: values})
                self.assertEqual(rows[0]["status"], "missing", (desktop, value))

    def test_cli_exit_status_summary_and_report_only(self):
        with tempfile.TemporaryDirectory() as directory:
            tree(Path(directory) / "artifacts" / "stock")
            path = Path(directory) / "budgets.json"
            path.write_text(json.dumps(budgets(**{"cpu.overview.total.mean": budget(1.5)})))
            summary, out = Path(directory) / "summary.md", Path(directory) / "gate.json"
            args = ["check", "--artifacts", str(Path(directory) / "artifacts"), "--budgets", str(path),
                    "--markdown", str(summary), "--out", str(out)]
            self.assertEqual(run_gate(args), 1)
            self.assertIn("`cpu.overview.total.mean` | fail", summary.read_text())
            self.assertIn("| overview | 30.0 | 6.0 | 1.9 | 0.0 | 0.0 | 4.8 |", summary.read_text())
            # Xwayland and the app start inside the phase: their first interval is not charged.
            self.assertEqual(json.loads(out.read_text())["results"][0]["status"], "fail")
            self.assertEqual(run_gate(args + ["--report-only"]), 0)
            path.write_text(json.dumps(budgets(**{"cpu.overview.total.mean": budget(2.0)})))
            self.assertEqual(run_gate(args), 0)

    def test_propose_only_tightens_never_below_target_and_records_evidence(self):
        current = budgets(**{"cpu.overview.total.mean": budget(3.0),
                             "cpu.overview.desktop.mean": budget(1.2),
                             "overview.response_ms.p50": budget(4.0, floor=0)})
        rows = self.evaluate(current, overview_ms=100)
        result = pb.propose(current, list(rows.values()), evidence=RUN.format(7))["metrics"]
        self.assertEqual(result["cpu.overview.total.mean"]["max_ratio"], 2.0)   # 1.72 x 1.15 -> 1.98 -> 2.00
        self.assertEqual(result["cpu.overview.total.mean"]["evidence"], RUN.format(7))
        self.assertEqual(result["cpu.overview.desktop.mean"]["max_ratio"], 1.2)  # never loosened
        self.assertEqual(result["cpu.overview.desktop.mean"]["evidence"], RUN.format(1))
        self.assertEqual(result["overview.response_ms.p50"]["max_ratio"], 1.0)   # 0.5 is past the target

    def test_propose_never_tightens_inside_the_bootstrap_interval(self):
        # #782: budgets taken from one quiet run's point ratio sat inside the
        # run-to-run noise. The interval's upper bound replaces a lower point.
        current = budgets(**{"cpu.overview.desktop.mean": budget(3.0)})
        row = {"metric": "cpu.overview.desktop.mean", "ratio": 1.8, "ci90": [1.5, 2.4]}
        result = pb.propose(current, [row])["metrics"]["cpu.overview.desktop.mean"]
        self.assertEqual(result["max_ratio"], 2.8)   # 2.4 x 1.15 = 2.76 -> 2.80, not 2.10
        row["ci90"] = [1.7, 1.9]
        self.assertEqual(pb.propose(current, [row])["metrics"]["cpu.overview.desktop.mean"]["max_ratio"], 2.2)


def noisy_tree(root, gnome, tuna, pairs=3):
    """Pairs whose overview phase follows explicit per-second gnome-shell/compositor loads."""
    for index in range(pairs):
        base = Path(root) / ("" if index == 0 else f"repeat-{index + 1}")
        for desktop, name, series in (("gnome", "gnome-shell", gnome[index]), ("tuna", "tuna-compositor", tuna[index])):
            write_session(base / desktop, {None: {name: 1}})
            rows = json.loads((base / desktop / "samples.json").read_text())
            total = 0
            for second, row in enumerate(rows):
                start, end = PHASES["overview"]
                if start < second <= end:
                    total += series[(second - start - 1) % len(series)] - 1
                row["processes"][0]["cpu_ticks"] += total
            (base / desktop / "samples.json").write_text(json.dumps(rows))


class Confidence(unittest.TestCase):
    metric = "cpu.overview.desktop.mean"

    def rows(self, directory, max_ratio):
        config = budgets(**{self.metric: budget(max_ratio)})
        series = pb.pair_series(pb.discover_pairs(directory))
        intervals = pb.confidence(config, series)
        return pb.evaluate(config, pb.compare(None, series), 3, intervals)[0], intervals

    def test_series_reproduce_the_reported_phase_distribution(self):
        with tempfile.TemporaryDirectory() as directory:
            noisy_tree(directory, [[10, 30]] * 3, [[20, 60, 40]] * 3)
            session = Path(directory) / "tuna"
            report = pb.phase_cpu(*[json.loads((session / name).read_text())
                                    for name in ("samples.json", "guest-phases.json")])
            metrics = pb.session_metrics(session)
            for role in ("compositor", "desktop", "total"):
                self.assertEqual(metrics[f"cpu.overview.{role}.mean"], report["overview"][role]["mean"])
                self.assertEqual(metrics[f"cpu.overview.{role}.p95"], report["overview"][role]["p95"])

    def test_interval_is_seeded_and_flags_a_decision_inside_the_noise(self):
        with tempfile.TemporaryDirectory() as directory:
            noisy_tree(directory, [[5, 35], [10, 30], [2, 38]], [[20, 60], [30, 50], [10, 70]])
            row, intervals = self.rows(directory, 2.0)
            again, _ = self.rows(directory, 2.0)
            self.assertEqual(row["ci90"], again["ci90"])         # same artifact, same interval
            low, high = row["ci90"]
            self.assertLess(low, 2.0)
            self.assertGreater(high, 2.0)
            self.assertFalse(row["decision_stable"])
            self.assertEqual(row["status"], "pass")              # the point estimate still decides
            self.assertAlmostEqual(row["ratio"], 2.0)
            self.assertTrue(self.rows(directory, 10.0)[0]["decision_stable"])

    def test_steady_measurements_give_a_point_interval(self):
        with tempfile.TemporaryDirectory() as directory:
            noisy_tree(directory, [[20]] * 3, [[30]] * 3)
            row, _ = self.rows(directory, 1.4)
            self.assertEqual(row["ci90"], [1.5, 1.5])
            self.assertEqual(row["status"], "fail")
            self.assertTrue(row["decision_stable"])


class Observer(unittest.TestCase):
    """#782: a perturbing observer's sessions never reach a budget decision."""

    def check(self, mutate):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree(root / "artifacts" / "stock")
            mutate(root / "artifacts" / "stock" / "repeat-2" / "gnome")
            path, out = root / "budgets.json", root / "gate.json"
            path.write_text(json.dumps(budgets(**{"cpu.overview.total.mean": budget(99)})))
            status = run_gate(["check", "--artifacts", str(root / "artifacts"), "--budgets", str(path),
                               "--out", str(out)])
            return status, json.loads(out.read_text())

    def test_clean_sessions_are_admitted(self):
        status, result = self.check(lambda session: None)
        self.assertEqual(status, 0)
        self.assertEqual(result["refused_sessions"], [])
        self.assertEqual(result["results"][0]["pairs"], 3)

    def test_oversized_observer_records_refuse_the_pair(self):
        def bloat(session):
            rows = json.loads((session / "samples.json").read_text())
            rows[5]["processes"][0]["thread_diagnostics"] = {"threads": [{"tid": 1}] * 4000}
            (session / "samples.json").write_text(json.dumps(rows))
        status, result = self.check(bloat)
        self.assertEqual(status, 1)
        self.assertEqual(result["results"][0]["status"], "missing")
        self.assertEqual([(r["repeat"], r["desktop"]) for r in result["refused_sessions"]], [(2, "gnome")])
        self.assertIn("exceeds", result["refused_sessions"][0]["problems"][0])

    def test_thread_diagnostics_provenance_refuses_the_pair(self):
        def flag(session):
            report = json.loads((session / "report.json").read_text())
            report["meta"] = ["observer_thread_diagnostics=true"]
            (session / "report.json").write_text(json.dumps(report))
        status, result = self.check(flag)
        self.assertEqual(status, 1)
        self.assertEqual(result["results"][0]["status"], "missing")

    def test_host_cpu_model_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            session = Path(directory)
            (session / "report.json").write_text(json.dumps(
                {"host": {"cpu": "processor\t: 0\nmodel name\t: AMD EPYC 7763 64-Core Processor\n"}}))
            self.assertEqual(pb.host_cpu(session), "AMD EPYC 7763 64-Core Processor")
            (session / "report.json").write_text("{}")
            self.assertIsNone(pb.host_cpu(session))


GNOME_MEMORY = {"gnome-shell": 180, "gjs": 20, "gsd-color": 100, "nautilus": 200}   # desktop 200, total 500 MiB


def steady(processes):
    return lambda second: processes


def memory_tree(root, compositors=(120, 160, 340), spoil=None):
    """One pair per Tuna compositor size: desktop is 60 MiB and the session 290 MiB above it."""
    for index, compositor in enumerate(compositors):
        base = Path(root) / ("" if index == 0 else f"repeat-{index + 1}")
        write_session(base / "gnome", {}, rows=memory_samples(steady(GNOME_MEMORY)))
        rows = memory_samples(steady({"tuna-compositor": compositor, "tuna-shell-gtk": 60, "pipewire": 30,
                                      "nautilus": 200}))
        if spoil == index:
            rows[20]["unreadable"] = [{"pid": 100, "error": "PermissionError"}]
        write_session(base / "tuna", {}, rows=rows)


def memory_budgets(enforced):
    return budgets(**{metric: budget(1.0, floor=0, enforced=enforced) for metric in pb.MEMORY_CRITERION})


class Memory(unittest.TestCase):
    """v1's "memory no worse than GNOME 51": PSS by role per pair, and never a silent pass."""

    def rows(self, config, **kwargs):
        with tempfile.TemporaryDirectory() as directory:
            memory_tree(directory, **kwargs)
            series = pb.pair_series(pb.discover_pairs(directory))
            return {row["metric"]: row for row in
                    pb.evaluate(config, pb.compare(None, series), 3, pb.confidence(config, series))}

    def test_each_sample_sums_the_processes_alive_by_role(self):
        def memory(second):
            alive = {"tuna-compositor": 100, "tuna-shell-gtk": 40, "tuna-shell-host": 10, "Xwayland": 8,
                     "xdg-desktop-portal-gtk": 4, "pipewire": 6}
            if 3 <= second <= 5:
                alive["nautilus"] = 50      # starts, then exits
            return alive
        raw = memory_samples(memory, end=8)
        raw[6]["processes"].append({"pid": 900, "start_ticks": 900, "cmd": "tuna-shell-gtk", "pss_kib": 2048})
        rows = pb.pss_samples(raw)
        self.assertEqual([row[:2] for row in rows[:2]], [(None, 0.0), (0.0, 1.0)])
        before, during, after, second_shell = rows[2], rows[4], rows[7], rows[6]
        self.assertEqual(before[2], {"compositor": 100, "shell": 50, "xwayland": 8, "portals": 4, "services": 6,
                                     "apps": 0, "desktop": 150, "total": 168})
        self.assertEqual((during[2]["apps"], during[2]["desktop"], during[2]["total"]), (50, 150, 218))
        self.assertEqual(during[4]["nautilus"], 50)
        self.assertEqual(after[2], before[2])
        self.assertNotIn("nautilus", after[4])
        self.assertEqual((second_shell[2]["shell"], second_shell[4]["tuna-shell-gtk"]), (52, 42))
        self.assertTrue(all(row[3] for row in rows))

    def test_a_phase_takes_wholly_contained_collections_and_the_whole_run_takes_all(self):
        # The collections ending at 10 s and 41 s straddle the idle markers.
        def memory(second):
            return {"tuna-compositor": 200 if 11 <= second <= 40 else (300 if second >= 50 else 999)}
        raw = memory_samples(memory)
        series = pb.pss_series(raw, {"idle_start": 10.0, "idle_end": 40.0, "overview_start": 50.0,
                                     "overview_end": 52.5, "typing_start": 5.0})
        # Overview holds two collections; typing has no end marker.
        self.assertEqual({metric.split(".")[1] for metric in series}, {"idle", "whole"})
        self.assertEqual(series["pss.idle.compositor.p50"], ("p50", [200.0] * 30))
        self.assertEqual(series["pss.idle.total.p95"], ("p95", [200.0] * 30))
        # The session report takes the same collections for its idle PSS.
        self.assertEqual(gnome_reference.idle_pss(raw, 10.0, 40.0), [200 * 1024] * 30)
        metrics = pb.summarize(series)
        self.assertEqual(len(series["pss.whole.total.p50"][1]), 101)
        self.assertEqual((metrics["pss.whole.total.p50"], metrics["pss.whole.total.p95"]), (300, 999))
        self.assertEqual((metrics["pss.whole.desktop.p50"], metrics["pss.whole.apps.p50"]), (300, 0))

    def test_a_process_that_exits_stops_counting_from_its_last_sample(self):
        def memory(second):
            alive = {"gnome-shell": 200}
            if second <= 20:
                alive["nautilus"] = 50      # alive for the first ten of thirty idle samples
            return alive
        raw, phases = memory_samples(memory), {"idle_start": 10.0, "idle_end": 40.0}
        metrics = pb.summarize(pb.pss_series(raw, phases))
        self.assertEqual((metrics["pss.idle.apps.p50"], metrics["pss.idle.apps.p95"]), (0, 50))
        self.assertEqual((metrics["pss.idle.total.p50"], metrics["pss.idle.total.p95"]), (200, 250))
        self.assertEqual((metrics["pss.idle.desktop.p50"], metrics["pss.idle.desktop.p95"]), (200, 200))
        processes = pb.process_pss(raw, phases)
        self.assertEqual(processes["idle"], {"gnome-shell": {"p50": 200, "p95": 200},
                                             "nautilus": {"p50": 0, "p95": 50}})
        self.assertEqual(processes["whole"]["nautilus"], {"p50": 0, "p95": 50})

    def test_too_few_or_failed_samples_leave_a_scope_unmeasured_never_smaller(self):
        phases = {"idle_start": 10.0, "idle_end": 40.0, "settle_start": 80.0, "settle_end": 100.0}
        complete = memory_samples(steady(GNOME_MEMORY))

        def scopes(raw):
            return {metric.split(".")[1] for metric in pb.pss_series(raw, phases)}
        self.assertEqual(scopes(complete), {"idle", "settle", "whole"})
        # An observer gap leaves two idle collections: too few, while the rest stands.
        self.assertEqual(scopes([row for row in complete if not 13 <= row["boottime_s"] <= 40]), {"settle", "whole"})
        # A sample the observer could not complete would understate memory.
        for spoil in (lambda row: row.update(unreadable=[{"pid": 100, "error": "PermissionError"}]),
                      lambda row: row["processes"][0].pop("pss_kib"),
                      lambda row: row["processes"][0].update(pss_kib=None),
                      lambda row: row["processes"][0].update(pss_kib=True),
                      lambda row: row["processes"][0].update(pss_kib=-1),
                      lambda row: row["processes"][0].update(pss_kib=1.5),
                      lambda row: row.update(processes=None)):
            raw = copy.deepcopy(complete)
            spoil(raw[20])
            self.assertEqual(scopes(raw), {"settle"})
            self.assertIsNone(pb.pss_samples(raw)[20][2])

    def test_unnamed_processes_report_only_the_total(self):
        raw = memory_samples(steady(GNOME_MEMORY))
        for row in raw:
            for process in row["processes"]:
                del process["comm"], process["cmd"]
        series = pb.pss_series(raw, {"idle_start": 10.0, "idle_end": 40.0})
        self.assertEqual(set(series), {f"pss.{scope}.total.{stat}" for scope in ("idle", "whole")
                                       for stat in ("p50", "p95")})
        self.assertEqual(pb.summarize(series)["pss.idle.total.p50"], 500)

    def test_pair_ratios_take_the_median_and_an_unenforced_budget_neither_passes_nor_fails(self):
        rows = self.rows(memory_budgets(enforced=False))
        desktop, total = rows["pss.idle.desktop.p50"], rows["pss.idle.total.p50"]
        # Desktop 180, 220 and 400 MiB against 200: ratios 0.9, 1.1 and one disturbed 2.0.
        self.assertEqual((desktop["gnome"], desktop["tuna"]), (200, 220))
        self.assertAlmostEqual(desktop["ratio"], 1.1)
        self.assertEqual((desktop["status"], desktop["reason"]), ("warn", "ratio 1.10 > budget 1.00"))
        self.assertFalse(desktop["meets_target"])
        self.assertEqual(desktop["ci90"], [0.9, 2.0])
        self.assertFalse(desktop["decision_stable"])
        # Session 410, 450 and 630 MiB against 500.
        self.assertEqual((total["gnome"], total["tuna"]), (500, 450))
        self.assertAlmostEqual(total["ratio"], 0.9)
        self.assertEqual(total["status"], "unenforced")
        self.assertTrue(total["meets_target"])
        self.assertEqual({row["status"] for row in rows.values()}, {"warn", "unenforced"})
        self.assertEqual(pb.failed(list(rows.values())), [])
        self.assertEqual([row["metric"] for row in pb.unenforced(list(rows.values()))], sorted(pb.MEMORY_CRITERION))

    def test_an_enforced_memory_budget_fails_over_its_ratio_and_when_a_pair_is_unmeasured(self):
        rows = self.rows(memory_budgets(enforced=True))
        self.assertEqual(rows["pss.idle.desktop.p50"]["status"], "fail")
        self.assertEqual(rows["pss.idle.total.p50"]["status"], "pass")
        # One Tuna session has an incomplete sample: its pair is unmeasured, not lighter.
        rows = self.rows(memory_budgets(enforced=True), spoil=0)
        self.assertEqual({row["status"] for row in rows.values()}, {"missing"})
        self.assertEqual(rows["pss.whole.total.p50"]["reason"], "2 of 3 required pairs measured")
        rows = self.rows(memory_budgets(enforced=False), spoil=0)
        self.assertEqual({row["status"] for row in rows.values()}, {"warn"})
        self.assertEqual(pb.failed(list(rows.values())), [])

    def test_report_says_memory_is_not_enforced_and_attributes_it_by_role_and_process(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            memory_tree(root / "artifacts" / "stock")
            config = memory_budgets(enforced=False)
            config["metrics"]["cpu.idle.total.mean"] = budget(1.0)
            path, out = root / "budgets.json", root / "gate.json"
            path.write_text(json.dumps(config))
            status, summary, errors = run_gate_output(["check", "--artifacts", str(root / "artifacts"),
                                                       "--budgets", str(path), "--out", str(out)])
            result = json.loads(out.read_text())
        self.assertEqual(status, 0)     # the desktop is over the target, yet it cannot fail the gate
        for line in ("pss.idle.desktop.p50: not enforced (ratio 1.10 > budget 1.00)",
                     "pss.idle.total.p50: not enforced (within budget)",
                     "memory: the v1 criterion (PSS ratio <= 1.00) is not enforced; "
                     "this result neither passes nor fails it"):
            self.assertIn(line, errors.splitlines())
        for text in ("4 of 5 budgets are not enforced: `pss.idle.desktop.p50`, `pss.idle.total.p50`, "
                     "`pss.whole.desktop.p50`, `pss.whole.total.p50`. They are reported, cannot fail this gate "
                     "and are not passes.",
                     "| `cpu.idle.total.mean` | pass |",
                     "| `pss.idle.desktop.p50` | warn (ratio 1.10 > budget 1.00), not enforced, unstable | "
                     "200.0 | 220.0 | 1.10 |",
                     "| `pss.idle.total.p50` | not enforced (within budget), unstable | 500.0 | 450.0 | 0.90 |",
                     "**The memory criterion is not enforced by this gate.** v1 requires memory no worse than "
                     "GNOME 51: a Tuna / GNOME PSS ratio <= 1.00",
                     "| `pss.whole.desktop.p50` | not enforced | 200.0 | 220.0 | 1.10 | not met |",
                     "| `pss.whole.total.p50` | not enforced | 500.0 | 450.0 | 0.90 | met |",
                     "| idle | p50 | GNOME 51 | 180.0 | 20.0 | 0.0 | 0.0 | 100.0 | 200.0 | 200.0 | 500.0 |",
                     "| idle | p50 | Tuna Desktop | 160.0 | 60.0 | 0.0 | 0.0 | 30.0 | 200.0 | 220.0 | 450.0 |",
                     "| idle | p50 | Tuna − GNOME | -20.0 | +40.0 | +0.0 | +0.0 | -70.0 | +0.0 | +20.0 | -50.0 |",
                     "| whole | p95 | Tuna Desktop | 160.0 |",
                     "| `gnome-shell` | compositor | 180.0 | 180.0 |",
                     "| `tuna-compositor` | compositor | 160.0 | 160.0 |"):
            self.assertIn(text, summary)
        self.assertEqual(result["unenforced"], sorted(pb.MEMORY_CRITERION))
        statuses = {row["metric"]: row["status"] for row in result["results"]}
        self.assertEqual(statuses, {"cpu.idle.total.mean": "pass", "pss.idle.desktop.p50": "warn",
                                    "pss.idle.total.p50": "unenforced", "pss.whole.desktop.p50": "warn",
                                    "pss.whole.total.p50": "unenforced"})
        memory = result["memory"]
        self.assertFalse(memory["criterion"]["enforced"])
        self.assertEqual(memory["criterion"]["metrics"]["pss.whole.desktop.p50"],
                         {"enforcement": "not enforced", "gnome": 200, "tuna": 220,
                          "ratio": memory["criterion"]["metrics"]["pss.whole.desktop.p50"]["ratio"],
                          "meets_target": False})
        self.assertEqual(memory["by_role_mib"]["tuna"]["settle"]["p50"],
                         {"compositor": 160, "shell": 60, "xwayland": 0, "portals": 0, "services": 30, "apps": 200,
                          "desktop": 220, "total": 450})
        self.assertEqual(memory["by_process_mib"]["gnome"]["idle"][:2],
                         [{"process": "nautilus", "role": "apps", "p50": 200, "p95": 200},
                          {"process": "gnome-shell", "role": "compositor", "p50": 180, "p95": 180}])
        self.assertEqual(memory["by_process_mib"]["tuna"]["whole"][1],
                         {"process": "tuna-compositor", "role": "compositor", "p50": 160, "p95": 160})

    def test_the_criterion_is_enforced_only_by_enforced_budgets_at_the_target(self):
        def criterion(config):
            rows = list(self.rows(config).values())
            return pb.memory_criterion(rows), pb.markdown(rows, [], config)
        result, summary = criterion(memory_budgets(enforced=True))
        self.assertTrue(result["enforced"])
        self.assertFalse(result["metrics"]["pss.idle.desktop.p50"]["meets_target"])
        self.assertIn("**The memory criterion is enforced.**", summary)
        self.assertNotIn("not enforced", summary)
        # A budget looser than the target is a regression budget, not the criterion.
        loose = memory_budgets(enforced=True)
        loose["metrics"]["pss.whole.desktop.p50"]["max_ratio"] = 1.3
        result, summary = criterion(loose)
        self.assertFalse(result["enforced"])
        self.assertEqual(result["metrics"]["pss.whole.desktop.p50"]["enforcement"],
                         "enforced at 1.30, above the target")
        self.assertIn("**The memory criterion is not enforced by this gate.**", summary)
        # Without any memory budget the report still says so, and shows nothing as met.
        result, summary = criterion(budgets(**{"cpu.idle.total.mean": budget(1.0)}))
        self.assertFalse(result["enforced"])
        self.assertEqual({value["enforcement"] for value in result["metrics"].values()}, {"no budget"})
        self.assertIn("**The memory criterion is not enforced by this gate.**", summary)
        self.assertIn("| `pss.whole.total.p50` | no budget | - | - | - | not measured |", summary)

    def test_enforcing_a_provisional_budget_needs_a_newer_run_and_cannot_raise_its_ratio(self):
        base = memory_budgets(enforced=False)

        def check(**change):
            head = copy.deepcopy(base)
            head["metrics"]["pss.whole.total.p50"].update(change)
            return pb.ratchet_violations(base, head, ROOT)
        self.assertEqual(check(enforced=True, evidence=RUN.format(2)), [])
        self.assertTrue(any("without new run evidence" in p for p in check(enforced=True)))
        self.assertTrue(any("needs a new ADR" in p
                            for p in check(enforced=True, max_ratio=1.3, evidence=RUN.format(2))))

    def test_validate_knows_pss_metrics_by_scope_role_and_statistic(self):
        known = ("pss.idle.desktop.p50", "pss.whole.total.p95", "pss.settle.apps.p50")
        self.assertEqual(pb.validate(budgets(**{metric: budget(1.0, floor=0, enforced=False) for metric in known})),
                         [])
        unknown = ("pss.idle.total.mean", "pss.nap.total.p50", "pss.idle.gpu.p50", "rss.idle.total.p50")
        self.assertEqual(pb.validate(budgets(**{metric: budget(1.0) for metric in unknown})),
                         [f"{metric}: unknown metric" for metric in unknown])


class Ratchet(unittest.TestCase):
    base = budgets(**{"cpu.overview.total.mean": budget(2.0, ceiling=80)})

    def check(self, change, root=ROOT):
        head = copy.deepcopy(self.base)
        change(head["metrics"])
        return pb.ratchet_violations(self.base, head, root)

    def test_unchanged_and_evidenced_tightening_pass(self):
        self.assertEqual(self.check(lambda m: None), [])
        def tighten(m):
            m["cpu.overview.total.mean"].update(max_ratio=1.8, ceiling=70, evidence=RUN.format(2))
        self.assertEqual(self.check(tighten), [])

    def test_change_without_new_run_evidence_is_refused(self):
        problems = self.check(lambda m: m["cpu.overview.total.mean"].update(max_ratio=1.8))
        self.assertTrue(any("without new run evidence" in p for p in problems))
        problems = self.check(lambda m: m["cpu.overview.total.mean"].update(max_ratio=1.8, evidence="trust me"))
        self.assertTrue(any("without new run evidence" in p for p in problems))

    def test_loosening_needs_a_new_existing_adr(self):
        for change in ({"max_ratio": 2.5}, {"ceiling": 90}, {"ceiling": None}, {"floor": 5.0}, {"enforced": False}):
            problems = self.check(lambda m: m["cpu.overview.total.mean"].update(evidence=RUN.format(3), **change))
            self.assertTrue(any("needs a new ADR" in p for p in problems), change)
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "docs/adr").mkdir(parents=True)
            (Path(directory) / "docs/adr/0099-raise.md").write_text("ADR")
            ok = lambda m: m["cpu.overview.total.mean"].update(max_ratio=2.5, evidence=RUN.format(3),
                                                               adr="docs/adr/0099-raise.md")
            self.assertEqual(self.check(ok, directory), [])
            missing = lambda m: m["cpu.overview.total.mean"].update(max_ratio=2.5, evidence=RUN.format(3),
                                                                    adr="docs/adr/0100-absent.md")
            self.assertTrue(self.check(missing, directory))

    def test_removing_a_budget_and_unevidenced_additions_are_refused(self):
        self.assertTrue(any("removing" in p for p in self.check(lambda m: m.clear())))
        added = lambda m: m.update({"cpu.idle.total.mean": budget(1.5, evidence="")})
        self.assertTrue(any("new budget" in p for p in self.check(added)))
        added = lambda m: m.update({"cpu.idle.total.mean": budget(1.5)})
        self.assertEqual(self.check(added), [])

    def test_calibrated_profile_cannot_change(self):
        head = dict(self.base, profile="diagnostic")
        self.assertTrue(any("profile changed" in p for p in pb.ratchet_violations(self.base, head)))
        self.assertEqual(pb.ratchet_violations(self.base, dict(self.base, profile="stock")), [])

    def test_pair_requirement_cannot_be_lowered(self):
        base = copy.deepcopy(self.base)
        base["min_pairs"] = 6
        head = copy.deepcopy(base)
        head["min_pairs"] = 3
        self.assertTrue(any("min_pairs lowered" in p for p in pb.ratchet_violations(base, head)))
        head["min_pairs"] = 6
        self.assertEqual(pb.ratchet_violations(base, head), [])


class CommittedBudgets(unittest.TestCase):
    def test_committed_file_is_valid_and_every_enforced_budget_is_evidenced(self):
        committed = json.loads((ROOT / "docs/perf/budgets.json").read_text())
        self.assertEqual(pb.validate(committed), [])
        self.assertEqual(pb.ratchet_violations(committed, committed, ROOT), [])
        for metric, entry in committed["metrics"].items():
            self.assertRegex(entry["evidence"], pb.RUN_URL, metric)
            self.assertGreaterEqual(entry["max_ratio"], committed["target_ratio"], metric)

    def test_validate_rejects_unknown_metrics_and_bad_values(self):
        problems = pb.validate(budgets(**{"cpu.nap.total.mean": budget(1.0),
                                          "cpu.idle.total.mean": budget(0, enforced="yes")}))
        self.assertEqual(len(problems), 3)

    def test_validate_rejects_nonfinite_and_invalid_gate_configuration(self):
        valid = budgets(**{"cpu.idle.total.mean": budget(1)})
        for key in ("max_ratio", "ceiling", "floor"):
            for value in (float("nan"), float("inf"), -1, True, "1"):
                candidate = copy.deepcopy(valid)
                candidate["metrics"]["cpu.idle.total.mean"][key] = value
                self.assertTrue(pb.validate(candidate), (key, value))
        for key, values in (("min_pairs", (0, 1, 2, 3.0, True, "3")),
                            ("target_ratio", (0, float("inf"), float("nan"))),
                            ("margin", (-1, float("inf"), float("nan"))),
                            ("profile", (None, True, "both", "", ["stock"]))):
            for value in values:
                candidate = copy.deepcopy(valid)
                candidate[key] = value
                self.assertTrue(pb.validate(candidate), (key, value))
        for candidate in ([], {"schema": 1, "metrics": {}}, budgets(**{"cpu.idle.total.mean": None})):
            self.assertTrue(pb.validate(candidate))

    def test_ratchet_cli_rejects_invalid_config_without_comparing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base.json", Path(directory) / "head.json"
            valid = budgets(**{"cpu.idle.total.mean": budget(1)})
            base.write_text(json.dumps(valid))
            invalid = copy.deepcopy(valid)
            invalid["metrics"]["cpu.idle.total.mean"] = None
            head.write_text(json.dumps(invalid))
            self.assertEqual(run_gate(["ratchet", "--base", str(base), "--head", str(head)]), 1)


if __name__ == "__main__":
    unittest.main()
