# Sourced by scripts/tuna-gtk-shell-proof.
#
# GNOME's welcome dialog (#1196): gnome-shell js/ui/welcomeDialog.js,
# opened by main.js _handleShowWelcomeScreen at startup when
# org.gnome.shell welcome-dialog-last-shown-version predates 40.beta.
# The dialog only exists at startup, so every case restarts the shell
# under the compositor's supervision, as G-A11Y-MENU-RESTART does.
#
#   G-WELCOME-DIALOG   a fresh session with gnome-tour installed shows
#                      "Welcome to <OS>" with Skip and Take Tour, records
#                      the shell's version, and Escape is Skip
#   G-WELCOME-ONCE     a recorded version is never shown again; a version
#                      before 40.beta is, and 40.0 is not
#   G-WELCOME-TOUR     Take Tour launches gnome-tour and closes the dialog
#   G-WELCOME-NO-TOUR  without gnome-tour no dialog opens, but the version
#                      is still recorded, as in GNOME

WELCOME_KEY="welcome-dialog-last-shown-version"
WELCOME_RESTARTS=0

welcome_key() {
    gsettings get org.gnome.shell "$WELCOME_KEY"
}

# Restart the shell and wait until the new one has mapped its panel.
welcome_restart() {
    old_shell="$(a11y_shell_pid)" || fail "$1" "expected exactly one owned shell"
    kill -TERM "$old_shell"
    end=$(($(date +%s) + 20))
    while :; do
        new_shell="$(a11y_shell_pid || true)"
        [ -n "$new_shell" ] && [ "$new_shell" != "$old_shell" ] && break
        [ "$(date +%s)" -le "$end" ] || fail "$1" "the supervisor did not restart the shell"
        sleep 0.2
    done
    WELCOME_RESTARTS=$((WELCOME_RESTARTS + 1))
    cstate '.layers | any(.namespace == "tuna-panel")' "restarted panel" 20 ||
        fail "$1" "the restarted shell never mapped its panel"
    # The dialog opens from an idle callback right after the panel.
    sleep 3
}

welcome_layer_is() {
    cstate ".layers | any(.namespace == \"tuna-welcome\") == $1" "welcome dialog $1" "${2:-10}"
}

welcome_dump() {
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-welcome-$1.json" 20 ${2:+--press "$2"}
}

welcome_proof() {
    cstate '.active_workspace != null and .locked == false' "welcome desktop ready" ||
        fail G-WELCOME-DIALOG "desktop never became ready"
    tour_marker="$ARTIFACTS/tour-launched"
    # The fresh session: the key's default is the oldest version, gnome-tour
    # is installed (tour-share), and the dialog is already up.
    welcome_layer_is true 20 || fail G-WELCOME-DIALOG "a fresh session did not open the welcome dialog"
    welcome_dump fresh || fail G-WELCOME-DIALOG "accessibility dump failed"
    os_title="Welcome to $(sed -n 's/^PRETTY_NAME=//p' /etc/os-release | head -1 | tr -d "\"'")"
    showing "$ARTIFACTS/a11y-welcome-fresh.json" "$os_title" ||
        fail G-WELCOME-DIALOG "no '$os_title' title"
    showing "$ARTIFACTS/a11y-welcome-fresh.json" "If you want to learn your way around, check out the tour" ||
        fail G-WELCOME-DIALOG "no description"
    showing "$ARTIFACTS/a11y-welcome-fresh.json" "Skip" || fail G-WELCOME-DIALOG "no Skip button"
    showing "$ARTIFACTS/a11y-welcome-fresh.json" "Take Tour" || fail G-WELCOME-DIALOG "no Take Tour button"
    [ "$(welcome_key)" = "'51.0'" ] ||
        fail G-WELCOME-DIALOG "the key is $(welcome_key), not the shell's version '51.0'"
    [ ! -e "$tour_marker" ] || fail G-WELCOME-DIALOG "the tour launched before it was asked for"
    # Escape is the Skip button (addButton key: Clutter.KEY_Escape).
    xdotool key Escape
    welcome_layer_is false || fail G-WELCOME-DIALOG "Escape did not close the dialog"
    [ ! -e "$tour_marker" ] || fail G-WELCOME-DIALOG "Escape launched the tour"
    pass G-WELCOME-DIALOG "a fresh session with gnome-tour installed shows the welcome dialog with the OS name, the tour description, Skip and Take Tour, records version 51.0, and Escape skips it"

    # The recorded version keeps it away across a restart.
    welcome_restart G-WELCOME-ONCE
    welcome_layer_is false 5 || fail G-WELCOME-ONCE "the dialog came back for a recorded version"
    # 40.0 is after 40.beta: not due. Anything before it is.
    gsettings set org.gnome.shell "$WELCOME_KEY" "'40.0'"
    welcome_restart G-WELCOME-ONCE
    welcome_layer_is false 5 || fail G-WELCOME-ONCE "40.0 showed the dialog"
    [ "$(welcome_key)" = "'40.0'" ] || fail G-WELCOME-ONCE "a not-due session rewrote the key to $(welcome_key)"
    gsettings set org.gnome.shell "$WELCOME_KEY" "'3.38'"
    welcome_restart G-WELCOME-ONCE
    welcome_layer_is true 10 || fail G-WELCOME-ONCE "3.38 predates 40.beta but did not show the dialog"
    [ "$(welcome_key)" = "'51.0'" ] || fail G-WELCOME-ONCE "the key is $(welcome_key) after showing"
    pass G-WELCOME-ONCE "a recorded version stays quiet across restarts; 40.0 is not due and is left alone; 3.38 predates 40.beta, shows it and records 51.0"

    # Take Tour (this dialog is still up from the 3.38 session).
    welcome_dump tour "Take Tour" || fail G-WELCOME-TOUR "could not press Take Tour"
    end=$(($(date +%s) + 10))
    while [ ! -e "$tour_marker" ] && [ "$(date +%s)" -le "$end" ]; do sleep 0.2; done
    [ -e "$tour_marker" ] || fail G-WELCOME-TOUR "gnome-tour was not launched"
    welcome_layer_is false || fail G-WELCOME-TOUR "the dialog stayed after Take Tour"
    pass G-WELCOME-TOUR "Take Tour launches org.gnome.Tour.desktop and closes the dialog"

    # No gnome-tour: nothing opens, yet GNOME still records the version.
    rm -f "$tour_marker" "$ARTIFACTS/tour-share/applications/org.gnome.Tour.desktop"
    gsettings reset org.gnome.shell "$WELCOME_KEY"
    welcome_restart G-WELCOME-NO-TOUR
    welcome_layer_is false 5 || fail G-WELCOME-NO-TOUR "the dialog opened without gnome-tour"
    [ "$(welcome_key)" = "'51.0'" ] ||
        fail G-WELCOME-NO-TOUR "the version was not recorded without gnome-tour: $(welcome_key)"
    pass G-WELCOME-NO-TOUR "without gnome-tour no dialog opens and the shell's version is still recorded"

    grep -q panicked "$LOG" && fail G-NOPANIC "panic in nested log"
    welcome_exits="$(grep -c ShellExited "$LOG" || true)"
    [ "$welcome_exits" = "$WELCOME_RESTARTS" ] ||
        fail G-SHELL-STABLE "the shell exited $welcome_exits time(s); the proof restarted it $WELCOME_RESTARTS"
    pass G-SHELL-STABLE "the shell exited only for the $WELCOME_RESTARTS deliberate restarts"
}
