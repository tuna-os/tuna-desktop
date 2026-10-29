"""Per-phase, per-process metrics and the GNOME-relative budget gate (#315, #503).

Every metric is measured on Tuna Desktop and on the GNOME 51 session of the
same pair (same job, image and VM profile), so a budget is a ratio to GNOME
plus an optional absolute ceiling. Budgets live in docs/perf/budgets.json and
only ratchet tighter; see docs/perf/README.md.
"""
import json
import math
from pathlib import Path
import random
import re
import statistics

# Guest CLOCK_BOOTTIME markers written by scripts/tuna-vm-perf, in workload
# order. A phase is reported only when both of its markers exist.
PHASES = ("idle", "frames", "typing", "overview", "workspaces", "window_cycling",
          "notifications", "settle")
ROLES = ("compositor", "shell", "xwayland", "portals", "services", "apps")
# Desktop is what Tuna Desktop owns: compositor plus shell. On GNOME both
# live in gnome-shell, so the comparison is the same set of work.
AGGREGATES = {"desktop": ("compositor", "shell"), "total": ROLES}
MIN_PHASE_INTERVALS = 3
DESKTOPS = ("gnome", "tuna")
# The notifications phase: one unmeasured warm-up round (the first banner,
# notification source and lazily built widgets), then this many measured
# rounds of ten notifications one second apart. A single round left the phase
# decided by about a dozen 1 s intervals of bursty banner work (#782).
NOTIFICATIONS_PER_ROUND = 10
NOTIFICATION_ROUNDS = 3

_COMPOSITOR = {"tuna-compositor", "gnome-shell", "mutter"}
_SHELL_PREFIXES = ("tuna-shell-", "gnome-shell-calendar-server", "gnome-shell-portal-helper")
_SERVICE_PREFIXES = ("tuna-session", "tuna-ibus-bridge", "gnome-session", "gsd-", "ibus", "dbus",
                     "pipewire", "wireplumber", "at-spi", "gvfs", "systemd", "evolution-", "goa-",
                     "gnome-keyring", "localsearch", "tracker", "dconf", "xdg-document-portal",
                     "xdg-permission-store", "gcr-", "colord", "geoclue", "obexd", "mpris")


def process_name(row):
    """argv[0] basename when recorded, else the kernel's 15-byte comm."""
    for key in ("cmd", "comm"):
        value = row.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def classify(name):
    if name is None:
        return None
    if name in _COMPOSITOR:
        return "compositor"
    if name.startswith(_SHELL_PREFIXES) or name == "gjs":
        # GNOME Shell runs its notification and screencast services under gjs.
        return "shell"
    if name == "Xwayland":
        return "xwayland"
    if name.startswith("xdg-desktop-portal"):
        return "portals"
    if name.startswith(_SERVICE_PREFIXES):
        return "services"
    return "apps"


def percentile(values, p):
    """Nearest-rank percentile of already sorted values."""
    return values[min(len(values) - 1, max(0, math.ceil(p * len(values)) - 1))]


def distribution(values):
    values = sorted(values)
    if not values:
        return {"count": 0}
    return {"count": len(values), "p50": percentile(values, .50), "p95": percentile(values, .95),
            "p99": percentile(values, .99), "min": values[0], "max": values[-1],
            "mean": statistics.mean(values)}


def cpu_intervals(samples):
    """Per-interval CPU percent of one core by role, from per-process ticks.

    Only processes present in both samples count, like the observer's own
    total, so a process start is never charged its lifetime ticks at once.
    Returns (begin_boottime, end_boottime, {role: percent}, named).
    """
    result = []
    for before, after in zip(samples, samples[1:]):
        elapsed = after.get("monotonic_s", 0) - before.get("monotonic_s", 0)
        hz = after.get("hz")
        begin, end = after.get("cpu_interval_start_boottime_s"), after.get("boottime_s")
        if not elapsed > 0 or not hz or begin is None or end is None:
            continue
        previous = {(p["pid"], p["start_ticks"]): p["cpu_ticks"] for p in before["processes"]}
        roles = dict.fromkeys(ROLES, 0.0)
        named = True
        for process in after["processes"]:
            ticks = previous.get((process["pid"], process["start_ticks"]))
            if ticks is None:
                continue
            role = classify(process_name(process))
            if role is None:
                named = False
                role = "apps"
            roles[role] += 100 * max(0, process["cpu_ticks"] - ticks) / hz / elapsed
        for name, members in AGGREGATES.items():
            roles[name] = sum(roles[member] for member in members)
        result.append((begin, end, roles, named))
    return result


def phase_windows(guest_phases):
    windows = {}
    for phase in PHASES:
        start, end = guest_phases.get(f"{phase}_start"), guest_phases.get(f"{phase}_end")
        if start is not None and end is not None and 0 < start < end:
            windows[phase] = (start, end)
    return windows


def phase_cpu(samples, guest_phases):
    """{phase: {role: distribution}} over intervals wholly inside the phase.

    Roles are omitted (only "total" kept) when the samples carry no process
    names, as in observers older than this module.
    """
    intervals = cpu_intervals(samples)
    result = {}
    for phase, (start, end) in phase_windows(guest_phases).items():
        inside = [row for row in intervals if start <= row[0] <= row[1] <= end]
        if len(inside) < MIN_PHASE_INTERVALS:
            continue
        named = all(row[3] for row in inside)
        keys = ROLES + tuple(AGGREGATES) if named else ("total",)
        result[phase] = {key: distribution([row[2][key] for row in inside]) for key in keys}
        result[phase]["named"] = named
        result[phase]["window_boottime_s"] = [start, end]
    return result


def read_session(directory):
    directory = Path(directory)
    report = json.loads((directory / "report.json").read_text())
    samples = json.loads((directory / "samples.json").read_text())
    guest_phases = json.loads((directory / "guest-phases.json").read_text())
    observations = json.loads((directory / "observations.json").read_text())
    return report, samples, guest_phases, observations


STATISTICS = {"mean": statistics.mean,
              "p50": lambda values: percentile(sorted(values), .50),
              "p95": lambda values: percentile(sorted(values), .95),
              "p99": lambda values: percentile(sorted(values), .99)}


def session_series(directory):
    """{metric: (statistic, observations)} behind each value of session_metrics.

    CPU observations are the phase's whole 1 s intervals in order; a fixed
    report value (frame pacing) is one observation with the "p95"/"p99" name.
    """
    report, samples, guest_phases, observations = read_session(directory)
    series = {}
    intervals = cpu_intervals(samples)
    for phase, (start, end) in phase_windows(guest_phases).items():
        inside = [row for row in intervals if start <= row[0] <= row[1] <= end]
        if len(inside) < MIN_PHASE_INTERVALS:
            continue
        keys = ROLES + tuple(AGGREGATES) if all(row[3] for row in inside) else ("total",)
        for key in keys:
            values = [row[2][key] for row in inside]
            series[f"cpu.{phase}.{key}.mean"] = ("mean", values)
            series[f"cpu.{phase}.{key}.p95"] = ("p95", values)
    whole = [s["cpu_observed_percent"] for s in samples if s.get("cpu_observed_percent") is not None]
    if whole:
        series["cpu.whole.total.p50"] = ("p50", whole)
    response = [row["upper_s"] * 1000 for row in observations if "upper_s" in row]
    if response:
        series["overview.response_ms.p50"] = ("p50", response)
        series["overview.response_ms.p95"] = ("p95", response)
    pacing = (report.get("presentation_cadence") or {}).get("interval_ms") or {}
    for key in ("p95", "p99"):
        if key in pacing:
            series[f"frame_pacing.interval_ms.{key}"] = (key, [pacing[key]])
    return series


def summarize(series):
    return {metric: STATISTICS[stat](values) for metric, (stat, values) in series.items()}


def session_metrics(directory):
    """Flat {metric: value} for one measured desktop session."""
    return summarize(session_series(directory))


# One serialized observer record per second is forwarded through journald to
# the serial console. Thread-diagnostic records of ~45 KiB (journald's 48 KiB
# LineMax) stalled every GNOME session from 2026-10-22 (#782); records this
# large are an observer that perturbs what it measures, never a gated sample.
OBSERVER_RECORD_BYTES = 32 * 1024


def session_problems(directory):
    """Reasons a measured session must not enter the gated comparison."""
    directory = Path(directory)
    problems = []
    report = json.loads((directory / "report.json").read_text())
    if "observer_thread_diagnostics=true" in (report.get("meta") or []):
        problems.append("observer per-thread diagnostics were enabled")
    samples = json.loads((directory / "samples.json").read_text())
    largest = max((len(json.dumps(s, separators=(",", ":"))) for s in samples), default=0)
    if largest > OBSERVER_RECORD_BYTES:
        problems.append(f"observer record of {largest} bytes exceeds {OBSERVER_RECORD_BYTES}")
    return problems


def host_cpu(directory):
    """The measuring host's CPU model, or None for reports without host provenance."""
    report = json.loads((Path(directory) / "report.json").read_text())
    match = re.search(r"^model name\s*:\s*(.+)$", (report.get("host") or {}).get("cpu") or "", re.M)
    return match[1].strip() if match else None


def qualify(pairs):
    """(admitted pairs, [{"profile", "repeat", "desktop", "problems"}] refused)."""
    admitted, refused = [], []
    for pair in pairs:
        reasons = [{"profile": pair["profile"], "repeat": pair["repeat"], "desktop": desktop,
                    "problems": problems}
                   for desktop in DESKTOPS if (problems := session_problems(pair[desktop]))]
        (refused.extend(reasons) if reasons else admitted.append(pair))
    return admitted, refused


def discover_pairs(root, profile=None):
    """Completed pairs, optionally restricted to one calibrated image profile."""
    root = Path(root)
    pairs = []
    for candidate, base in (("diagnostic", root), ("stock", root / "stock")):
        if profile is not None and candidate != profile:
            continue
        for repeat in (1, 2, 3):
            directory = base if repeat == 1 else base / f"repeat-{repeat}"
            sessions = {desktop: directory / desktop for desktop in DESKTOPS}
            if all((path / "report.json").is_file() for path in sessions.values()):
                pairs.append({"profile": candidate, "repeat": repeat, **sessions})
    return pairs


def pair_series(pairs):
    """[(pair, gnome series, tuna series)] read once for comparison and confidence."""
    return [(pair, session_series(pair["gnome"]), session_series(pair["tuna"])) for pair in pairs]


def compare(pairs, series=None):
    """{metric: [{"profile", "repeat", "gnome", "tuna"}]} over all pairs."""
    result = {}
    for pair, gnome, tuna in pair_series(pairs) if series is None else series:
        gnome, tuna = summarize(gnome), summarize(tuna)
        for metric in sorted(gnome.keys() & tuna.keys()):
            result.setdefault(metric, []).append({"profile": pair["profile"], "repeat": pair["repeat"],
                                                  "gnome": gnome[metric], "tuna": tuna[metric]})
    return result


BOOTSTRAP_REPLICATES = 1000
BOOTSTRAP_SEED = 782
# Banner, animation and redraw bursts span consecutive 1 s intervals.
BOOTSTRAP_BLOCK = 3
CONFIDENCE = 0.90


def resample(values, rng, block=BOOTSTRAP_BLOCK):
    """Moving-block bootstrap resample of one session's ordered observations."""
    if len(values) <= block:
        return [values[rng.randrange(len(values))] for _ in values]
    result = []
    while len(result) < len(values):
        start = rng.randrange(len(values) - block + 1)
        result.extend(values[start:start + block])
    return result[:len(values)]


def confidence(budgets, series, replicates=BOOTSTRAP_REPLICATES, seed=BOOTSTRAP_SEED, level=CONFIDENCE):
    """{metric: (low, high)}: seeded percentile bootstrap of the gated ratio.

    Each replicate resamples the pairs with replacement and, inside every
    session, blocks of its intervals or observations, then recomputes the
    median over pairs of Tuna / max(GNOME, floor). The interval describes this
    run's measurement noise; it never changes a status, which stays on the
    point estimate, so no budget is loosened by it.
    """
    floors = {}
    for metric, budget in budgets["metrics"].items():
        usable = all(metric in gnome and metric in tuna for _, gnome, tuna in series)
        if usable and series and all(finite_number(value) for _, gnome, tuna in series
                                     for value in gnome[metric][1] + tuna[metric][1]):
            floors[metric] = budget.get("floor", 0)
    rng = random.Random(seed)
    draws = {metric: [] for metric in floors}
    for _ in range(replicates if floors else 0):
        chosen = [series[rng.randrange(len(series))] for _ in series]
        for metric, floor in floors.items():
            ratios = []
            for _, gnome, tuna in chosen:
                (gnome_stat, gnome_values), (tuna_stat, tuna_values) = gnome[metric], tuna[metric]
                ratios.append(ratio(STATISTICS[tuna_stat](resample(tuna_values, rng)),
                                    STATISTICS[gnome_stat](resample(gnome_values, rng)), floor))
            draws[metric].append(statistics.median(ratios))
    tail = (1 - level) / 2
    return {metric: (percentile(sorted(values), tail), percentile(sorted(values), 1 - tail))
            for metric, values in draws.items()}


def ratio(tuna, gnome, floor):
    return tuna / max(gnome, floor, 1e-9)


def finite_number(value, *, positive=False):
    return type(value) in (int, float) and math.isfinite(value) and (value > 0 if positive else value >= 0)


def evaluate(budgets, comparison, min_pairs=3, intervals=None):
    """Check every budget against the median over pairs of the Tuna/GNOME ratio.

    The median absorbs one disturbed pair; an enforced budget that is over its
    ratio or ceiling, or has fewer than min_pairs measurements, fails. With
    bootstrap intervals, a row records whether its decision is stable: the
    whole interval lies on one side of the budget ratio.
    """
    rows = []
    for metric, budget in sorted(budgets["metrics"].items()):
        floor = budget.get("floor", 0)
        values = comparison.get(metric, [])
        row = {"metric": metric, "enforced": bool(budget.get("enforced")), "pairs": len(values),
               "max_ratio": budget["max_ratio"], "ceiling": budget.get("ceiling"),
               "unit": budget.get("unit", "")}
        if any(not finite_number(value.get(desktop)) for value in values for desktop in DESKTOPS):
            row.update(status="missing" if row["enforced"] else "warn",
                       reason="paired measurements must be finite non-negative numbers")
            rows.append(row)
            continue
        if len(values) < min_pairs:
            row.update(status="missing" if row["enforced"] else "warn",
                       reason=f"{len(values)} of {min_pairs} required pairs measured")
            rows.append(row)
            continue
        row["ratio"] = statistics.median(ratio(v["tuna"], v["gnome"], floor) for v in values)
        row["tuna"] = statistics.median(v["tuna"] for v in values)
        row["gnome"] = statistics.median(v["gnome"] for v in values)
        reasons = []
        if row["ratio"] > budget["max_ratio"]:
            reasons.append(f"ratio {row['ratio']:.2f} > budget {budget['max_ratio']:.2f}")
        if row["ceiling"] is not None and row["tuna"] > row["ceiling"]:
            reasons.append(f"Tuna {row['tuna']:.1f} > ceiling {row['ceiling']:.1f}")
        if reasons:
            row.update(status="fail" if row["enforced"] else "warn", reason="; ".join(reasons))
        else:
            row["status"] = "pass"
        # The denominator floor stabilizes the regression budget, not the
        # match-or-beat target. A quiet but slower desktop must not claim parity.
        target_ratios = (v["tuna"] / v["gnome"] if v["gnome"] else
                         (0.0 if v["tuna"] == 0 else math.inf) for v in values)
        row["meets_target"] = statistics.median(target_ratios) <= budgets.get("target_ratio", 1.0)
        if intervals and metric in intervals:
            low, high = intervals[metric]
            row["ci90"] = [low, high]
            row["decision_stable"] = high <= budget["max_ratio"] or low > budget["max_ratio"]
        rows.append(row)
    return rows


def failed(rows):
    return [row for row in rows if row["status"] in ("fail", "missing")]


def propose(budgets, rows, margin=None, evidence=None):
    """Tighten each measured budget to ratio x (1 + margin), never loosening.

    With a bootstrap interval the upper bound replaces a lower point ratio, so
    one quiet run cannot set a budget inside this run's own noise (#782).
    """
    margin = budgets.get("margin", 0.15) if margin is None else margin
    proposed = json.loads(json.dumps(budgets))
    for row in rows:
        if "ratio" not in row:
            continue
        budget = proposed["metrics"][row["metric"]]
        measured = max(row["ratio"], (row.get("ci90") or (0, 0))[1])
        candidate = math.ceil(measured * (1 + margin) * 20) / 20
        candidate = max(candidate, budgets.get("target_ratio", 1.0))
        if candidate < budget["max_ratio"]:
            budget["max_ratio"] = candidate
            if evidence:
                budget["evidence"] = evidence
    return proposed


RUN_URL = re.compile(r"^https://github\.com/tuna-os/tuna-desktop/actions/runs/\d+$")


def ratchet_violations(base, head, repo_root=None):
    """Budgets may only tighten. Every change cites a run; loosening needs an ADR."""
    problems = []
    for metric, old in sorted(base["metrics"].items()):
        new = head["metrics"].get(metric)
        if new is None:
            problems.append(f"{metric}: removing a budget loosens the gate (needs an ADR, keep it with adr set)")
            continue
        changed = any(old.get(key) != new.get(key) for key in ("max_ratio", "ceiling", "floor", "enforced"))
        if not changed:
            continue
        loosened = []
        if new["max_ratio"] > old["max_ratio"]:
            loosened.append("max_ratio raised")
        if old.get("ceiling") is not None and (new.get("ceiling") is None or new["ceiling"] > old["ceiling"]):
            loosened.append("ceiling raised or removed")
        if new.get("floor", 0) > old.get("floor", 0):
            loosened.append("floor raised")
        if old.get("enforced") and not new.get("enforced"):
            loosened.append("enforcement dropped")
        if new.get("evidence") == old.get("evidence") or not RUN_URL.match(str(new.get("evidence", ""))):
            problems.append(f"{metric}: changed without new run evidence (set evidence to the "
                            "performance-baseline run URL that justifies it)")
        if loosened:
            adr = new.get("adr")
            exists = adr and repo_root is not None and (Path(repo_root) / adr).is_file()
            if not adr or adr == old.get("adr") or not str(adr).startswith("docs/adr/") or not exists:
                problems.append(f"{metric}: {', '.join(loosened)}; loosening needs a new ADR under "
                                "docs/adr/ referenced by adr (roadmap change control)")
    for metric, new in sorted(head["metrics"].items()):
        if metric not in base["metrics"] and not RUN_URL.match(str(new.get("evidence", ""))):
            problems.append(f"{metric}: a new budget needs run evidence")
    if head.get("profile", "stock") != base.get("profile", "stock"):
        problems.append("profile changed: calibrate a separate budget instead of replacing its comparison population")
    if head.get("target_ratio", 1.0) > base.get("target_ratio", 1.0):
        problems.append("target_ratio raised")
    if head.get("min_pairs", 3) < base.get("min_pairs", 3):
        problems.append("min_pairs lowered: the repeatability requirement must not be weakened")
    return problems


def validate(budgets):
    problems = []
    if not isinstance(budgets, dict) or budgets.get("schema") != 1 or not isinstance(budgets.get("metrics"), dict):
        return ["budgets schema 1 with a metrics object required"]
    if budgets.get("profile", "stock") not in ("stock", "diagnostic"):
        problems.append("profile must name exactly stock or diagnostic")
    if not budgets["metrics"]:
        problems.append("at least one performance budget required")
    if not finite_number(budgets.get("target_ratio", 1.0), positive=True):
        problems.append("target_ratio must be finite and positive")
    if not finite_number(budgets.get("margin", 0.15)):
        problems.append("margin must be finite and non-negative")
    min_pairs = budgets.get("min_pairs", 3)
    if type(min_pairs) is not int or min_pairs < 3:
        problems.append("min_pairs must be an integer of at least 3")
    for metric, budget in budgets["metrics"].items():
        if not isinstance(metric, str) or not isinstance(budget, dict):
            problems.append(f"{metric}: metric name and budget object required")
            continue
        parts = metric.split(".")
        known = (len(parts) == 4 and parts[0] == "cpu" and (parts[1] in PHASES or parts[1] == "whole")
                 and parts[2] in ROLES + tuple(AGGREGATES) and parts[3] in ("mean", "p95", "p50")) or \
            metric in ("overview.response_ms.p50", "overview.response_ms.p95",
                       "frame_pacing.interval_ms.p95", "frame_pacing.interval_ms.p99")
        if not known:
            problems.append(f"{metric}: unknown metric")
        value = budget.get("max_ratio")
        if not finite_number(value, positive=True):
            problems.append(f"{metric}: max_ratio must be finite and positive")
        ceiling = budget.get("ceiling")
        if ceiling is not None and not finite_number(ceiling, positive=True):
            problems.append(f"{metric}: ceiling must be finite and positive or null")
        if not finite_number(budget.get("floor", 0)):
            problems.append(f"{metric}: floor must be finite and non-negative")
        if type(budget.get("enforced")) is not bool:
            problems.append(f"{metric}: enforced must be true or false")
    return problems


def attribution(comparison):
    """{desktop: {phase: {role: median over pairs of the phase mean}}}."""
    result = {desktop: {} for desktop in DESKTOPS}
    for phase in PHASES:
        for role in ROLES:
            values = comparison.get(f"cpu.{phase}.{role}.mean")
            if not values:
                continue
            for desktop in DESKTOPS:
                result[desktop].setdefault(phase, {})[role] = statistics.median(v[desktop] for v in values)
    return result


def markdown(rows, pairs, budgets, comparison=None, refused=(), hosts=()):
    lines = ["## Performance budgets: Tuna Desktop vs GNOME 51", "",
             f"{len(pairs)} {budgets.get('profile', 'stock')} paired sessions. Ratio is the median over pairs of Tuna / GNOME "
             f"(target <= {budgets.get('target_ratio', 1.0):.2f}). Budgets: `docs/perf/budgets.json`. "
             f"The {CONFIDENCE:.0%} interval is a seeded block bootstrap over pairs and intervals; "
             "`unstable` marks a decision whose interval straddles the budget.", ""]
    if hosts:
        lines += ["Host CPU: " + ", ".join(f"`{host}`" for host in hosts) + ". Ratios differ by host class.", ""]
    for item in refused:
        lines.append(f"Refused {item['profile']} repeat {item['repeat']} {item['desktop']}: "
                     + "; ".join(item["problems"]))
    if refused:
        lines.append("")
    lines += ["| Metric | Status | GNOME | Tuna | Ratio | 90% CI | Budget | Ceiling |",
              "|---|---|---:|---:|---:|---:|---:|---:|"]
    for row in rows:
        def number(key, fmt):
            return format(row[key], fmt) if row.get(key) is not None else "-"
        status = row["status"] + ("" if row["status"] == "pass" else f" ({row.get('reason', '')})")
        if row.get("decision_stable") is False:
            status += ", unstable"
        interval = "{:.2f}–{:.2f}".format(*row["ci90"]) if row.get("ci90") else "-"
        lines.append(f"| `{row['metric']}` | {status} | {number('gnome', '.1f')} | {number('tuna', '.1f')} | "
                     f"{number('ratio', '.2f')} | {interval} | {row['max_ratio']:.2f} | {number('ceiling', '.1f')} |")
    for desktop, phases in attribution(comparison or {}).items():
        if not phases:
            continue
        lines += ["", f"### {'GNOME 51' if desktop == 'gnome' else 'Tuna Desktop'}: mean CPU by process role "
                  "(% of one core, median over pairs)", "",
                  "| Phase | " + " | ".join(ROLES) + " |", "|---|" + "---:|" * len(ROLES)]
        for phase, roles in phases.items():
            lines.append(f"| {phase} | " + " | ".join(format(roles.get(role, 0), ".1f") for role in ROLES) + " |")
    return "\n".join(lines) + "\n"
