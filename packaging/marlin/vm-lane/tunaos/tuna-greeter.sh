#!/usr/bin/env bash

# Tuna's login screen (tuna-os/tuna-desktop#1164). GDM keeps its daemon, PAM
# and session start. Its greeter session runs `tuna-compositor --greeter`
# instead of GNOME Shell. From GDM 49, gnome-session starts the greeter shell
# through GDM's drop-in
# /usr/lib/systemd/user/gnome-session@gnome-login.target.d/gnome-login.session.conf.
# A drop-in of the same name in /etc replaces it. tuna-desktop ships that
# replacement inactive, so the image installs it here.
#
# The replacement must keep every other requirement of GDM's drop-in. If GDM
# adds one, this hook fails, and the replacement must be updated first.
#
# GDM 48 and older (Debian trixie) start the shell through session.conf
# instead. There the login screen stays GNOME Shell's.
_tuna_greeter_install() {
	local test_root="${TUNAOS_TUNA_GREETER_ROOT:-}"
	local drop_in="gnome-session@gnome-login.target.d/gnome-login.session.conf"
	local vendor="${test_root}/usr/lib/systemd/user/${drop_in}"
	local older="${test_root}/usr/lib/systemd/user/gnome-session@gnome-login.target.d/session.conf"
	local packaged="${test_root}/usr/share/tuna-desktop/greeter/gnome-login.session.conf"
	local unit="${test_root}/usr/lib/systemd/user/tuna-greeter.service"
	local override="${test_root}/etc/systemd/user/${drop_in}"
	local expected actual

	if [[ ! -f "$vendor" ]]; then
		if [[ -f "$older" ]]; then
			echo "GDM has no gnome-login.session.conf (GDM 48 or older): the login screen stays GNOME Shell's"
			return 0
		fi
		echo "GDM's greeter session drop-in is missing: $vendor" >&2
		return 1
	fi
	grep -qx 'Requires=org.gnome.Shell@gdm.service' "$vendor" || {
		echo "GDM's greeter session does not require org.gnome.Shell@gdm.service: $vendor" >&2
		return 1
	}
	[[ -f "$packaged" && -f "$unit" && -x "${test_root}/usr/bin/tuna-greeter" ]] || {
		echo "the installed tuna-desktop does not ship the Tuna login screen: $packaged" >&2
		return 1
	}
	expected="$(grep -v -e '^#' -e '^[[:space:]]*$' "$vendor" |
		sed 's/^Requires=org\.gnome\.Shell@gdm\.service$/Requires=tuna-greeter.service/')"
	actual="$(grep -v -e '^#' -e '^[[:space:]]*$' "$packaged")"
	[[ "$actual" == "$expected" ]] || {
		echo "tuna-desktop's gnome-login.session.conf does not match GDM's drop-in with the shell replaced" >&2
		diff <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
		return 1
	}
	install -Dm0644 "$packaged" "$override"
}

_tuna_greeter_install
