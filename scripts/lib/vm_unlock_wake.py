"""V-UNLOCK-WAKE on the marlin-vm lifecycle boot (#1194).

GNOME Settings Daemon's power plugin blanks the screen (DisplayConfig
PowerSaveMode 3) as soon as the screen saver turns active, and wakes it only
on input or on the shell's org.gnome.ScreenSaver WakeUpScreen
(gsd-power-manager.c `handle_screensaver_active`, `handle_wake_up_screen`).
GNOME Shell emits WakeUpScreen on every unlock (screenShield.js
`_continueDeactivate`), so an unlock without input, logind's Unlock from
`loginctl unlock-session`, must not leave the session dark.

The lane locks with Super+L, the gate's last input, and waits for gsd-power
to blank the output. The session owner then runs `loginctl unlock-session`.
With no further input, the compositor's snapshot must show the session
unlocked, PowerSaveMode back at 0, and its rendered frame count advancing
past where the blanked output left it.
"""
import json
import os
import time

GATE = "V-UNLOCK-WAKE"
# gsd-power blanks once the shield's ActiveChanged(true) lands; its 30 s
# SCREENSAVER_TIMEOUT_BLANK idle watch is the latest it may come.
BLANK_TIMEOUT = 90
# Super+L to the landed lock.
LOCK_TIMEOUT = 15
# The unlock, and gsd-power's answer to WakeUpScreen.
WAKE_TIMEOUT = 15
# Frames once the output is on again.
FRAMES_TIMEOUT = 10
# The tour's own recovery afterwards: a password unlock or a key.
RECOVER_TIMEOUT = 20
POLL = 0.25
# The marker sample for the moment `loginctl unlock-session` ran.
UNLOCK = "unlock-session"


def sample(state, step, elapsed):
    """One observation of the compositor snapshot's lock, power and frames."""
    return {"step": step, "elapsed": round(elapsed, 2), "locked": state.get("locked"),
            "power_save_mode": state.get("power_save_mode"), "frames": state.get("rendered_frames")}


def blanked(value):
    return value["locked"] is True and value["power_save_mode"] == 3


def woke(value):
    return value["locked"] is False and value["power_save_mode"] == 0


def advanced(value, base):
    return isinstance(value["frames"], int) and isinstance(base, int) and value["frames"] > base


def problems(samples):
    """What the samples, in the order taken, fail of the gate; [] when it holds.

    The sample whose step is UNLOCK, at most one, marks the unlock request;
    every other sample is a snapshot. Before it the session must have been
    locked and blanked, and still be when the unlock was asked for; after
    it, with no input, a snapshot must show the session unlocked at
    PowerSaveMode 0, and a later (or the same) one more rendered frames
    than the last blanked snapshot.
    """
    marks = [index for index, value in enumerate(samples) if value["step"] == UNLOCK]
    if len(marks) > 1:
        return [f"loginctl unlock-session ran {len(marks)} times, not once"]
    cut = marks[0] if marks else len(samples)
    before, after = samples[:cut], samples[cut + 1:]
    found = []
    if not any(value["locked"] is True for value in before):
        found.append("Super+L never locked the session")
    dark = [value for value in before if blanked(value)]
    if not dark:
        modes = sorted({str(value["power_save_mode"]) for value in before})
        found.append(f"gsd-power never blanked the locked session (PowerSaveMode seen: {', '.join(modes)})")
    elif not blanked(before[-1]):
        found.append(f"the session was no longer locked and blanked when unlock-session ran: {before[-1]}")
    if not marks:
        return found + ["loginctl unlock-session never ran"]
    lit = [index for index, value in enumerate(after) if woke(value)]
    if not lit:
        last = after[-1] if after else None
        found.append("with no input, loginctl unlock-session left the session "
                     + (f"locked={last['locked']} at PowerSaveMode {last['power_save_mode']}" if last
                        else "unobserved"))
    elif dark:
        base = dark[-1]["frames"]
        if not any(advanced(value, base) for value in after[lit[0]:]):
            seen = [value["frames"] for value in after[lit[0]:]]
            found.append(f"rendered frames did not advance past the blanked output's {base} after the wake: {seen}")
    return found


def run(qmp, agent, evidence, record, password_unlock):
    """Drive and record V-UNLOCK-WAKE, then leave the session unlocked and lit.

    `password_unlock` types the fixture password at the lock screen; it is
    used only to recover after the gate is decided. Raises only when that
    recovery fails.
    """
    samples, begun = [], time.monotonic()

    def observe(step):
        state = agent.run("inventory")["state"]
        samples.append(sample(state, step, time.monotonic() - begun))
        return samples[-1]

    def until(step, predicate, timeout):
        deadline = time.monotonic() + timeout
        while True:
            value = observe(step)
            if predicate(value) or time.monotonic() >= deadline:
                return predicate(value)
            time.sleep(POLL)

    error = None
    try:
        # The gate's last input: nothing touches input until it is decided.
        qmp.keys("meta_l", "l")
        locked = until("lock", lambda value: value["locked"] is True, LOCK_TIMEOUT)
        if locked and until("blank", blanked, BLANK_TIMEOUT):
            agent.run(UNLOCK)
            samples.append({"step": UNLOCK, "elapsed": round(time.monotonic() - begun, 2)})
            if until("wake", woke, WAKE_TIMEOUT):
                base = [value for value in samples if value["step"] == "blank"][-1]["frames"]
                until("frames", lambda value: advanced(value, base), FRAMES_TIMEOUT)
    except (OSError, RuntimeError, ValueError, KeyError) as caught:
        error = f"{type(caught).__name__}: {caught}"
    finally:
        with open(os.path.join(evidence, "unlock-wake.json"), "w") as stream:
            json.dump({"samples": samples, "error": error}, stream, indent=2)
    found = problems(samples) + ([error] if error else [])
    if found:
        record(GATE, False, "; ".join(found)[:900])
    else:
        dark = [value for value in samples if value["step"] == "blank"][-1]
        mark = next(value for value in samples if value["step"] == UNLOCK)
        lit = next(value for value in samples if value["step"] in ("wake", "frames") and woke(value))
        record(GATE, True,
               f"Super+L locked and gsd-power blanked the output (PowerSaveMode 3) at {dark['elapsed']} s; "
               f"loginctl unlock-session with no input unlocked it and PowerSaveMode was 0 again "
               f"{round(lit['elapsed'] - mark['elapsed'], 1)} s later, frames advancing past {dark['frames']} "
               f"to {samples[-1]['frames']} (lifecycle/unlock-wake.json)")

    # Whatever the verdict, the tour goes on from an unlocked, lit session.
    state = observe("recover")
    if state["locked"] is True:
        password_unlock()
    elif state["power_save_mode"] != 0:
        qmp.keys("ctrl")
    if not until("recovered", woke, RECOVER_TIMEOUT):
        raise RuntimeError(f"the session did not come back unlocked and lit after {GATE}: {samples[-1]}")
