# Tuna greeter: the login screen on GDM's daemon

Owner decision 2026-11-16 ([ADR 0008](adr/0008-v1-beta-track-and-release-scope.md)
item 6, epic [#1164](https://github.com/tuna-os/tuna-desktop/issues/1164)):
v1 replaces GDM's GNOME Shell greeter (`gnome-shell --mode=gdm`) with a Tuna
greeter and keeps GDM's daemon. GDM keeps PAM, seats, VT handoff, user
switching and session start. The greeter is `tuna-compositor --greeter`
running `tuna-greeter`, a GTK4/libadwaita client of GDM's greeter D-Bus API.
Parity bar: GDM 51's login screen. Rollout: replace
`org.gnome.Shell@gdm.service` on every TunaOS variant.

Sources read for this page (tags, not branches): GDM
[51.1.1](https://gitlab.gnome.org/GNOME/gdm/-/tree/51.1.1), gnome-shell
[51.1](https://gitlab.gnome.org/GNOME/gnome-shell/-/tree/51.1), gnome-session
main, and the installed files of `ghcr.io/tuna-os/marlin` (image
`44bcc7837e69`, built 2026-11-14: `gdm 51.1.1-1`, `gnome-shell 1:51.1-1`).

## How GDM 51 starts its greeter

1. `GdmLocalDisplayFactory` makes a display; `GdmLaunchEnvironment`
   (`daemon/gdm-launch-environment.c`) allocates a *dynamic* user
   (`gdm-greeter`, "GDM Greeter", from `GdmDynamicUserStore`; GDM 49 moved
   off the static `gdm` user) and opens a `GdmSession` for it with the
   daemon-only PAM service `gdm-launch-environment`.
2. It selects the session named `gnome-greeter`
   (`GDM_GREETER_SESSION`, hard-coded) of type `wayland`. GDM looks for
   `gnome-greeter.desktop` in `/usr/share/gdm/greeter/wayland-sessions/`
   first (`get_system_session_dirs`). Its `Exec=gnome-session --session
   gnome-login`.
3. The environment (`build_launch_environment`) is written to the greeter
   user's `~/.config/environment.d/gdm.conf`: `USER`/`LOGNAME`/`HOME`
   (`/run/gdm/home/…`), `SHELL=/sbin/nologin`, `GDM_VERSION`, `GDM_SEAT_ID`,
   `XDG_CONFIG_HOME`/`XDG_STATE_HOME` under `/var/lib/gdm/<seat>/` (kept
   across boots so a11y settings persist), `DCONF_PROFILE=gdm`,
   `GVFS_DISABLE_FUSE=1`, `GIO_USE_VFS=local`, `RUNNING_UNDER_GDM=true`,
   `XDG_DATA_DIRS=/usr/share/gdm/greeter:<system dirs>` and
   `GDM_SUPPORTED_SESSION_TYPES` (`wayland:x11`, colon separated).
4. gnome-session's leader starts `gnome-session@gnome-login.target`
   (`gnome-login.session`: `Kiosk=true`). GDM installs the drop-in
   `/usr/lib/systemd/user/gnome-session@gnome-login.target.d/gnome-login.session.conf`:
   `Requires=gnome-session-basic-services.target` and
   `Requires=org.gnome.Shell@gdm.service`. The basic services are the
   settings daemon plugins A11ySettings, Keyboard, MediaKeys, Power, Rfkill,
   Sharing, Smartcard and UsbProtection.
5. `org.gnome.Shell@.service` runs `gnome-shell --mode=%i` as `Type=notify`,
   `Before=gnome-session-initialized.target`, `OnFailure=` the session
   shutdown. GNOME Shell signals `READY=1`, calls GDM's
   `Manager.RegisterSession` at startup and `RegisterDisplay` once the login
   screen is up (`js/ui/main.js`, `js/misc/loginManager.js`).

### Integration point (chosen)

Replace the vendor drop-in by name: ship
`/etc/systemd/user/gnome-session@gnome-login.target.d/gnome-login.session.conf`
with `Requires=gnome-session-basic-services.target` and
`Requires=tuna-greeter.service`. systemd lets a drop-in in `/etc` override the
one of the same name in `/usr/lib` (systemd.unit(5), "Drop-in files … with
the same name"), and dependency lists cannot be emptied by another drop-in,
so this is the only override that removes the shell. It keeps everything
else of GDM's design: the `gnome-greeter` session, gnome-session's leader
and `gnome-login` target, the settings daemon plugins, and GDM's own
restart of a failed greeter. TunaOS already themes the greeter through
`/etc/systemd/user/org.gnome.Shell@gdm.service.d/` (the Marlin image has
`tunaos-gdm-theme.conf` there), so `/etc` is the image's existing seam.

Rejected:

- Overriding `org.gnome.Shell@gdm.service`'s `ExecStart=`: the unit keeps
  GNOME Shell's name, `AssertEnvironment` and `ExecStopPost`, and confuses
  every reader.
- Masking `org.gnome.Shell@gdm.service`: a masked `Requires=` fails the
  target, and the greeter never starts.
- A different `gnome-greeter.desktop`: the file belongs to the `gdm`
  package (package conflicts) and the session name is compiled into GDM.

`tuna-greeter.service` (packaging slice, not in this change):

```ini
[Unit]
Description=Tuna Desktop login screen
OnFailure=gnome-session-shutdown.target
OnFailureJobMode=replace-irreversibly
CollectMode=inactive-or-failed
RefuseManualStart=on
RefuseManualStop=on
After=gnome-session-manager.target
Requisite=gnome-session-initialized.target
PartOf=gnome-session-initialized.target
Before=gnome-session-initialized.target

[Service]
Slice=session.slice
Type=notify
ExecStart=/usr/bin/tuna-compositor --greeter
ExecStopPost=-systemctl --user unset-environment WAYLAND_DISPLAY DISPLAY
Restart=no
TimeoutStopSec=5
```

`Type=notify`: in greeter mode the compositor sends `READY=1` to
`NOTIFY_SOCKET` once, when the greeter's layer first maps (`G-GREETER-READY`),
so the settings daemon plugins start after the login screen is up.

Other greeters: no other greeter in the wild uses GDM's greeter API (LightDM
and greetd greeters speak their own daemons); GDM's only other client of
the same session-file seam is `gnome-initial-setup`
(`gnome-initial-setup.desktop`, its own `gnome-session` session).

## The greeter D-Bus API

### Opening the channel

`org.gnome.DisplayManager` on the **system** bus,
`/org/gnome/DisplayManager/Manager`, interface
`org.gnome.DisplayManager.Manager`:

| Method | |
|---|---|
| `OpenSession() → s address` | Only from a process of the display's greeter session and only from its `allowed_user` uid; refused once a user session runs on the display ("Can only be called before user is logged in"). |
| `OpenReauthenticationChannel(s user) → s address` | The unlock dialog's channel, and the login screen's for user switching: from a login screen GDM grants it only for a user whose session runs (`gdm_session_start_reauthentication` on that session) and answers `AccessDenied` otherwise ("Login screen only allowed to open reauthentication channels for running sessions"). A conversation verified there makes GDM activate the user's session (`switch_to_compatible_user_session`) and close the channel. |
| `RegisterSession()`, `RegisterDisplay()` | The greeter is up; GDM ends plymouth. |

The address is a `GDBusServer` per `GdmSession`
(`gdm_dbus_setup_private_server`): `unix:dir=/run/gdm/dbus` (mode 0711), the
socket made 0666, EXTERNAL authentication, and `allow_user_function`
accepts only the session's `allowed_user`. There is no
`GDM_GREETER_DBUS_ADDRESS` variable: libgdm always asks `OpenSession`. The
client connects peer to peer (`G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT`,
no message bus, no bus names) and uses the object
`/org/gnome/DisplayManager/Session`.

### `org.gnome.DisplayManager.UserVerifier`

| Member | |
|---|---|
| `EnableExtensions(as)` | Export `UserVerifier.ChoiceList` (`SelectChoice(s service, s choice)`, signal `ChoiceQuery(s service, s prompt, a{ss} list)`) and/or `UserVerifier.CustomJSON` (`Reply`, `ReportError`, signal `Request(s service, s protocol, u version, s json)`, used for web login and passkeys). Unknown names are ignored. |
| `BeginVerification(s service)` / `BeginVerificationForUser(s service, s user)` | Start a PAM conversation. GDM 51 refuses services with `/`, not starting with `gdm-`, or daemon-only (`gdm-autologin`, `gdm-launch-environment`). The reply waits until the worker started. |
| `AnswerQuery(s service, s answer)` | Answer the outstanding `InfoQuery`/`SecretInfoQuery`. |
| `Cancel()` | GDM's manager then resets the session: `Reset` is emitted, every conversation stops (`ConversationStopped` each) and the selected session and user are forgotten. |
| signals `ConversationStarted(s)`, `ConversationStopped(s)`, `ReauthenticationStarted(i)` | |
| signals `Info(s service, s text)`, `Problem(s service, s text)` | PAM_TEXT_INFO / PAM_ERROR_MSG and GDM's own failure text. |
| signals `InfoQuery(s service, s query)`, `SecretInfoQuery(s service, s query)` | PAM_PROMPT_ECHO_ON / OFF. |
| signal `Reset()` | |
| signal `ServiceUnavailable(s service, s message)` | Service missing or `PAM_MAXTRIES`; followed by `VerificationFailed`. |
| signals `VerificationFailed(s)`, `VerificationComplete(s)` | Failure is `Problem` (or `ServiceUnavailable`) + `VerificationFailed` + `ConversationStopped`. In login mode `VerificationComplete` comes after the PAM session opened. |

Services gnome-shell 51 uses: `gdm-password`, `gdm-fingerprint`,
`gdm-smartcard` (`authServicesLegacy.js`), or `gdm-switchable-auth` for SSSD
switchable auth (`authServicesSSSDSwitchable.js`, `enable-switchable-authentication`).
GDM 51.0: fingerprint no longer logs in, it only re-authenticates.

### `org.gnome.DisplayManager.Greeter`

| Member | |
|---|---|
| `SelectSession(s id)` | The session desktop file id; GDM 51.beta rejects path traversal. Refused once the session runs. |
| `SelectUser(s)`, `BeginAutoLogin(s)` (only the configured autologin user since 51.beta), `GetTimedLoginDetails() → (b, s, i)`, `StopConflictingSession()` | |
| `StartSessionWhenReady(s service, b start)` | After `SessionOpened`: start the opened session. |
| signals `SelectedUserChanged(s)`, `DefaultLanguageNameChanged(s)`, `DefaultSessionNameChanged(s)` (the user's saved session, else GDM's fallback; TunaOS sets `FallbackSession=tuna`), `TimedLoginRequested(s, i)`, `SessionOpened(s service, s session_id)`, `Reauthenticated(s)` | |

GDM 51 has no `RemoteGreeter` or `Chooser` interface any more (XDMCP is
gone); remote logins use `RemoteDisplayFactory` on the manager.

### How gnome-shell 51 drives it

`loginDialog.js` makes a `Gdm.Client`, enables both extensions, lists users
from `AccountsService.UserManager` (loaded, not system, not locked;
`user-added`/`-removed`/`-changed` keep it current), and on a click begins
`AuthPrompt.begin({userName})`. `authServices.js` starts every enabled
service with `BeginVerificationForUser`; `authPrompt.js` turns queries into
the entry (hidden for secret), `Info`/`Problem` into the queued message
(48 ms per character, at least 2 s, × `GDM_MESSAGE_TIME_MULTIPLIER`), and on
`VerificationFailed` counts failures against `allowed-failures` (default 3):
below it the prompt soft-resets for the same user, at it the dialog returns
to the user list. "Not listed?" asks "Username" itself and then begins for
that name. The session menu ("Login Options" → "Session Type") lists
`Gdm.get_session_ids()` and calls `SelectSession`; `DefaultSessionNameChanged`
moves its active item. `SessionOpened` → (conflicting-session check through
logind) → fade out → `StartSessionWhenReady(service, true)`.

## Login flows (gnome-shell 51.1, GDM 51.1.1)

What `userVerifier.js` (`ShellUserVerifier`), `authServices*.js`,
`authPrompt.js` and `loginDialog.js` do, and what the greeter does (sources
fetched at the tags; gnome-shell 51 has no `util.js` verifier any more, it
moved to `userVerifier.js` and the auth services):

- **Channels.** Every begin for a named user first asks
  `OpenReauthenticationChannel(user)` (`_getUserVerifierProxies`); on
  `AccessDenied` the conversation runs on the greeter's own channel.
  libgdm enables the ChoiceList and CustomJSON extensions on each channel
  it makes a UserVerifier proxy for, and asks `GetTimedLoginDetails` when it
  makes the Greeter proxy (GDM answers with `TimedLoginRequested`).
- **Auth services and mechanisms.** `AuthServicesSSSDSwitchable` comes
  first while `enable-switchable-authentication` is set (GDM's default):
  it begins `gdm-switchable-auth` and waits for SSSD's CustomJSON
  `auth-mechanisms` request (`{"authSelection": {"mechanisms": {…},
  "priority": […]}}`). A failed begin (the PAM service is missing: Arch's GDM
  ships no `gdm-switchable-auth`; GDM answers
  `SessionWorker.Error.ServiceUnavailable`, then `ServiceUnavailable`,
  `VerificationFailed`, `ConversationStopped`), or plain PAM messages before
  any JSON (pam_unix behind it), means `NOT_FOUND`: the roles cascade to
  `AuthServicesLegacy`, whose mechanisms are Password (`gdm-password`) and
  Smartcard (`gdm-smartcard`) as `enable-password-authentication` and
  `enable-smartcard-authentication` allow. Login Options' first section
  ("Login Options") lists the mechanisms; the selected one (else the first)
  is selected, which resets softly (`Cancel`, then the begin of its service).
  Only the selected service runs. A hard reset (back to the list) tries the
  switchable service again; a soft one keeps the selection.
- **Smartcard.** Smartcard is a mechanism like Password. gsd-smartcard's
  token insertion or removal (`smartcardManager.js` on the session bus)
  resets softly while Smartcard is selected, unless a PIN is being checked
  and a card is still in. GNOME 51 does not start `gdm-smartcard` on its own
  when a token appears (gnome-shell ≤ 46's `util.js` did); the greeter
  follows 51. With only Smartcard allowed and switchable auth off, no
  username is needed: the user list is skipped and `BeginVerification
  (gdm-smartcard)` lets the certificate name the user.
- **ChoiceList.** `ChoiceQuery(service, prompt, a{ss})` of the selected
  service shows as a titled list (`authList.js`); choosing answers
  `SelectChoice(service, key)`. It is a prompt step (Back from step 2
  soft-resets).
- **Web login (`eidp`) and SSSD's other mechanisms.** From SSSD's JSON the
  greeter draws GNOME's prompts locally and answers with a CustomJSON
  `Reply` (`{"authSelection": {"status": "Ok", "<id>": {…}}}`): password
  (`{"password"}`), smartcard certificates (one: its PIN; several: "Select
  Identity" then the PIN; none: "Insert Smartcard"), passkey (insert-key
  instruction, PIN with the attempts warning, touch instruction) and web
  login: the `initPrompt` button ("Log In"), then `webLogin.js`'s dialog
  (link prompt, a 150 px QR code of the URL, the URL without its scheme,
  "Login code: " and the code, Cancel and Done); Done replies `{}` and the
  dialog waits; `timeout` refreshes the mechanisms. `cancelRequested`'s
  steps back (dialog → button, PIN → certificate list, touch or waiting →
  soft reset) are the conversation's own; Back at the insert-key
  instruction or at a single PIN resets hard. The passkey reply carries
  the PIN with SSSD's `kerberos` and `cryptoChallenge`. GNOME 51 also
  restarts the passkey prompt when a FIDO2 token is plugged in or out
  (`fido2TokenManager.js`, udev `hidraw` with `ID_FIDO_TOKEN`); the greeter
  does not watch udev yet, so after the insert-key instruction the user
  goes Back and chooses again.
- **Expired SSSD password.** SSSD reports expiry only as PAM text: with
  the password mechanism selected, `Info` "Password expired. Change your
  password now." shows and the following `SecretInfoQuery`s ("New
  Password", "Reenter new Password") are prompt steps answered with
  `AnswerQuery` on `gdm-switchable-auth`, not JSON (`_resettingPassword`).
- **Multi-step.** Any further `InfoQuery`/`SecretInfoQuery` (a 2FA code
  after the password) is the next prompt step of the shared AuthPrompt
  machine: the entry clears, shows the question (visible for `InfoQuery`)
  and Back returns to step 1 with a soft reset.
- **Timed login.** `TimedLoginRequested(user, delay)`: the user's row (or,
  with the user list disabled, the prompt) shows the countdown bar; over
  5 s the bar waits until nothing was touched for 5 s
  (`_TIMED_LOGIN_IDLE_THRESHOLD`), cancels an open prompt, then runs for the
  rest; any key or click starts it over; when it ends the greeter calls
  `BeginAutoLogin(user)` and starts `gdm-autologin`'s session on
  `SessionOpened`. Note GDM 51.1.1's `BeginAutoLogin` only admits the
  configured `AutomaticLogin` user (`gdm-session.c`), so timed login needs
  that user to be the automatic-login user as well.
- **Conflicting session.** On `SessionOpened(service, id)` logind's sessions
  are read; another graphical, active or online session of the same user
  opens "Session Already Running" (`conflictingSessionDialog.js`, with its
  four local/remote texts and the force-stop warning). Cancel resets (GDM's
  `Cancel` closes the opened session) back to the list; Force Stop calls
  `StopConflictingSession` and the session starts when logind removes the
  old one; after 60 s unanswered the dialog closes with "Login Attempt Timed
  Out" (a toast that, like GNOME's critical notification, stays until a user is chosen) and the prompt resets
  (GDM's `Cancel`) back to the list.
- **User switching.** The list rings users with a session (`logged-in`,
  libaccountsservice's count from logind; the row's accessible description
  is "Logged in"); a logged-in user's Login Options has no session type.
  Choosing one runs the conversation on the user's re-authentication
  channel; on success GDM switches to that session, no session opens and the
  greeter stays verified until its own logind session is active again,
  when the prompt resets (`_loginScreenSessionActivated`). Users whose
  session GDM does not manage (remote, or started elsewhere) get
  `AccessDenied`, log in on the greeter's channel and meet the conflicting
  session dialog.
- **Fingerprint: not applicable.** GDM 51.0 stopped logging in with
  fingerprint; it re-authenticates only. gnome-shell 51's login dialog makes
  its verifier with `reauthenticationOnly: false`, so
  `AuthServicesLegacy._connectFingerprintManager` never runs and Fingerprint
  is filtered out of the mechanisms, also on a user-switch re-authentication
  channel. The greeter matches: no `gdm-fingerprint` (the lock screen keeps
  its own fprintd path).
- **Desktop side (P-SY-12).** gnome-shell 51's `activateSwitchUser` locks
  and calls libgdm's `gdm_goto_login_session_sync`; GDM 51.1.1's
  `goto_login_session` (`common/gdm-common.c`) activates an existing
  `greeter`-class, non-closing `gdm-launch-environment` session on the seat
  with logind `ActivateSessionOnSeat`, and otherwise (on `seat0` only) calls
  `LocalDisplayFactory.CreateTransientDisplay`. `crates/shell-gtk/src/
  user_switch.rs` does the same calls in the same order; checked against
  51.1.1, no change needed.

## What GDM 51's login screen shows

- Background `#222226`, text `#fafafb` (`#lockDialogGroup`,
  `_login-lock.scss`), TunaOS tints it with the variant accent.
- Panel (`gdm` session mode): the clock in the middle; at the right the
  dwell-click, keyboard (input source) and quick-settings indicators
  (power, volume, network, a11y toggles); no activities, no overview.
- Centre: the user list (25em, 64px avatars, logged-in users ringed) and
  "Not listed?"; or the prompt (avatar 160px, name 20pt, Back, entry with
  the next arrow, caps-lock warning, message).
- Banner under the panel (`banner-message-enable`, `-text`, `-source`,
  `-path`), logo at the bottom (`logo`), bottom-right buttons: Login
  Options (auth mechanisms and session type) and the accessibility menu.
- Policy keys (`org.gnome.login-screen`): `disable-user-list`,
  `disable-restart-buttons`, `allowed-failures`, `enable-password-/
  fingerprint-/smartcard-/switchable-authentication`, banner and logo keys.
- Timed login, conflicting-session dialog, realmd login hint, web login.

## Architecture (this slice)

- `crates/compositor`: `--greeter` (`NestedSession::greeter`). The
  supervised client is `tuna-greeter` (`TUNA_GREETER_BIN`, sibling, `PATH`)
  with the shell's restart budget; when the budget runs out the compositor
  exits non-zero so GDM's own greeter recovery restarts the login screen.
  No lock (idle, suspend and logind `Lock` are ignored: GDM's greeter dconf
  profile sets `org.gnome.desktop.lockdown disable-lock-screen`, so GNOME
  Shell's shield never locks the login screen), no overview or hot corner
  (`gdm` mode's `hasOverview`), no shell swipes, no startup
  overview, no user-settings upgrade. On DRM it imports `WAYLAND_DISPLAY` to
  systemd and D-Bus activation but starts no `tuna-session.target`, holds
  no leader FIFO and does not register with GDM: gnome-session leads, and
  the greeter registers itself. Outputs, input and the screen reader path
  are those of a user session. `READY=1` goes to `NOTIFY_SOCKET` once the
  greeter first maps.
- `crates/auth-prompt` (`tuna-auth-prompt`): the lock screen's GNOME
  AuthPrompt state machine and `PromptEntry` widget, moved out of
  `tuna-shell-gtk` so both screens share them. `Host::can_retry`
  (`_canRetry`) and `Effect::GiveUp` add the login screen's
  `allowed-failures`; the lock screen keeps the default (always retry).
- `crates/greeter` (`tuna-greeter`): `gdm.rs` (channel, signal parsing,
  calls; the session object's signals are subscribed through GDBus
  directly, because gio-rs 0.22's subscription aborts on the sender-less
  signals of a peer-to-peer channel), `channel.rs` (the greeter's channel
  and per-begin re-authentication channels, calls in order),
  `conversation.rs` (`ShellUserVerifier` and the auth services as the
  machine's `Host`: mechanisms, SSSD's JSON, ChoiceList), `auth_ui.rs`
  (choice list, button, web login dialog with QR code), `timed_login.rs`,
  `conflict.rs`, `logind.rs`, `smartcard.rs` (gsd-smartcard tokens),
  `accounts.rs` (AccountsService list), `sessions.rs` (libgdm's session
  enumeration), `main.rs` (layer-shell window, pages, Login Options).
- The AuthPrompt machine gained GNOME's non-entry steps (`Ask`: choice
  list, button, web login, answered through the same query sequence),
  `cancel_requested`, soft `restart`, and the entry area's visibility
  (`clear()` hides it until a question); the lock screen ignores them.

## Proof

`scripts/tuna-greeter-proof` (gtk-shell CI job) runs the real
`tuna-compositor --greeter` nested on Xvfb with the real `tuna-greeter`;
GDM's daemon, AccountsService, logind and gsd-smartcard are
`scripts/lib/tuna-gdm-mock.py` on a private system bus (gsd-smartcard on
the private session bus), serving the API above with GDM 51.1.1's signal
order and failure text, and a control interface (`org.tuna.Proof.GdmMock`)
the proof uses to insert the card or the passkey (and touch it), install
`gdm-switchable-auth`, set timed login, add and delete users, end a user's
sessions and close GDM's channels. `org.gnome.login-screen` is GDM 51.1.1's
schema (`scripts/lib/org.gnome.login-screen.gschema.xml`), compiled for the
run and set with `gsettings` on the keyfile backend. Users are chosen and
passwords typed with real X11 keys, and the result is read back through
AT-SPI, the mock's call log and the compositor's state; `A-1-user-list.png`,
`C-1-prompt.png` and `P-1-policy.png` are the review shots against GNOME
51's login dialog, with `F-2-code.png`, `G-2-conflict.png`,
`G-3-timeout.png`, `H-1-insert.png`, `H-2-choice.png`, `I-2-web-login.png`,
`P-2-pin.png`, `P-3-touch.png`, `S-1-identity.png`, `X-2-new.png`,
`J-1-policy.png`, `K-1-timed.png` and `L-1-prompt-timed.png` for the flows.
Gates: `G-GREETER-READY`, `-USER-LIST`, `-NO-SHELL`, `-NOT-LISTED`,
`-GIVE-UP`, `-SESSION`, `-WRONG-PASSWORD`, `-PASSWORD`, `-START`,
`-MULTISTEP`, `-SWITCH-RETURN`, `-CONFLICT`, `-CONFLICT-TIMEOUT`,
`-SMARTCARD`, `-CHOICE-LIST`, `-WEB-LOGIN`, `-PASSKEY`, `-SSSD-SMARTCARD`,
`-PASSWORD-EXPIRED`, `-POLICY-AUTH`, `-TIMED-LOGIN`, `-TIMED-LOGIN-PROMPT`,
`-SUPERVISION`, `-HYGIENE`. A second login screen on the same mock then
runs with `TUNA_SESSION_KIND=hardware`: `G-GREETER-POLICY`, `-CAPS-LOCK`,
`-POWER-MENU`, `-LIVE-USERS`, `-RECONNECT`. Nested only: no real GDM, PAM,
SSSD, card reader, passkey, identity provider, gnome-session or DRM.

## Security model

- The greeter runs as GDM's dynamic greeter user and holds no secret beyond
  relaying an answer: the answer (password, code, PIN, or SSSD's JSON reply
  carrying one) goes straight from the entry to `AnswerQuery` or
  `CustomJSON.Reply` on GDM's channel, the entry is cleared, `Call`'s
  `Debug` redacts both and no trace prints them. PAM runs in GDM's session
  worker as root; the greeter never sees a PAM handle or a password hash.
- Only the greeter session's uid may `OpenSession` or connect to the
  channel (GDM's checks); a re-authentication channel is GDM's to grant,
  for a running session only. Only GDM's allowed `gdm-*` services start;
  the greeter asks for `gdm-password`, `gdm-smartcard` and
  `gdm-switchable-auth`, never `gdm-autologin` (GDM runs it after
  `BeginAutoLogin`, which it admits for the configured user only).
- `SelectSession` ids come from the session files GDM itself would accept;
  GDM validates them again (51.beta).
- The compositor admits layer-shell only to its supervised child, so no
  other client can draw over the login screen.

## Lifecycle

- Start: GDM → `gnome-session --session gnome-login` →
  `gnome-session@gnome-login.target` → `tuna-greeter.service` →
  `tuna-compositor --greeter` → supervised `tuna-greeter` → `OpenSession`,
  user list, `RegisterSession`/`RegisterDisplay`.
- Greeter crash: the compositor's overlay covers the screen and restarts
  it with backoff; the new process asks GDM for the channel again. Budget
  spent: the compositor exits non-zero, the unit fails, gnome-session shuts
  down, GDM sees the greeter session end and restarts the display (GDM's
  own limits apply; text consoles remain).
- GDM channel closed: the greeter asks for a new channel at once, then
  every second until it gets one; the begin owed a result is denied and the
  prompt begins again on the new channel (`G-GREETER-RECONNECT`).
- Login: `VerificationComplete` → `SessionOpened` →
  `StartSessionWhenReady`; GDM starts the user session and stops the greeter
  session (SIGTERM to its process group).

## Parity checklist against GDM 51

| Area | GDM 51 | Tuna greeter now | Gate |
|---|---|---|---|
| User list from AccountsService, filtered like `UserList.addUser`, with avatars | yes | yes (`IconFile` avatars, live add/remove) | G-GREETER-USER-LIST, G-GREETER-LIVE-USERS; cargo: `a_readable_avatar_file_is_the_users_image`, `only_added_and_deleted_users_change_the_list` |
| Password through `gdm-password`, GNOME's prompt and messages | yes | yes (shared AuthPrompt machine) | G-GREETER-PASSWORD; cargo: `conversation::tests::*`, `gdm::tests::*` |
| Wrong password: GDM's problem shown, retry, `allowed-failures` back to the list | yes | yes | G-GREETER-WRONG-PASSWORD, G-GREETER-GIVE-UP, G-GREETER-POLICY; cargo: `a_host_done_trying_gives_up_once_the_problem_is_read`, `failure_then_stop_is_one_denied_result_and_counts` |
| "Not listed?" username entry | yes | yes | G-GREETER-NOT-LISTED; cargo: `a_typed_username_is_trimmed_and_matched_to_the_list` |
| Caps-lock warning under a secret question ("Caps lock is on") | yes | yes | G-GREETER-CAPS-LOCK; cargo: `the_caps_lock_warning_belongs_to_secret_questions` |
| Session type in Login Options, `DefaultSessionNameChanged` | yes | yes (re-selected after GDM resets) | G-GREETER-SESSION; cargo: `sessions::tests::*` |
| `StartSessionWhenReady` after `SessionOpened` | yes | yes (at once: GNOME's `AuthPrompt.finish` waits for queued messages first) | G-GREETER-START; cargo: `only_session_opened_starts_a_session_after_verification` |
| GDM's channel closes: reconnect, owed results denied | yes | yes | G-GREETER-RECONNECT; cargo: `wire_failures_and_disconnects_deny_what_is_owed` |
| No overview, lock or panel layers; greeter supervised | yes (`gdm` mode) | yes | G-GREETER-NO-SHELL, G-GREETER-SUPERVISION; cargo: `the_login_screen_never_locks_and_has_no_overview`, `ready_is_sent_once_when_the_greeter_first_maps`, `greeter_flag_makes_a_login_screen_session`, `help_documents_greeter_mode`, `a_spent_greeter_budget_fails_the_unit` |
| Register session/display with GDM | yes | yes (the greeter; the compositor only publishes the display) | G-GREETER-READY (mock GDM); cargo: `the_greeter_publishes_only_its_display`; V-GDM-LOGIN once packaged |
| Banner, logo, `disable-user-list`, `disable-restart-buttons` | yes | yes, read at start (banner text only, not `banner-message-source=file`) | G-GREETER-POLICY, G-GREETER-POWER-MENU; cargo: `login_screen_keys_make_the_policy`, `banner_needs_enable_and_text_and_failures_allow_one_try` |
| Wiggle, fades | yes | no | — |
| Fingerprint | re-auth only in 51; not on the login dialog | not offered (as GNOME) | not applicable; see Login flows |
| Smartcard (`gdm-smartcard`, gsd-smartcard insertion, PIN) | yes | yes | G-GREETER-SMARTCARD; cargo: `smartcard_changes_restart_only_the_smartcard_mechanism`, `tokens_report_insertions_like_smartcard_manager` |
| Authentication policy keys (`enable-password-/smartcard-/switchable-authentication`, `allowed-failures`) | yes | yes (followed live) | G-GREETER-POLICY-AUTH; cargo: `password_policy_leaves_smartcard_without_a_username`, `the_schemas_defaults_are_the_default_policy`, `allowed_failures_leaves_at_least_one_try`, `a_reset_lands_on_the_list_the_question_or_no_user` |
| ChoiceList | yes | yes | G-GREETER-CHOICE-LIST; cargo: `legacy_choice_lists_answer_with_select_choice` |
| Login Options' mechanisms (`_onMechanismsChanged`, `selectMechanism`) | yes | yes | G-GREETER-SMARTCARD, G-GREETER-POLICY-AUTH; cargo: `login_options_select_the_first_mechanism_and_start_it`, `no_mechanism_is_chosen_while_an_answer_is_checked` |
| Switchable auth, CustomJSON web login (`eidp`) | yes | yes | G-GREETER-WEB-LOGIN; cargo: `sssd_web_login_*`, `missing_switchable_service_cascades_to_password`, `plain_pam_on_the_switchable_service_is_not_sssd` |
| SSSD passkey (insert key, PIN with the attempts warning, touch) | yes | yes, but a FIDO2 token plugged in does not restart the prompt (no udev watch: Back and choose again) | G-GREETER-PASSKEY; cargo: `sssd_passkey_without_the_key_asks_for_it`, `sssd_passkey_pin_then_touch_replies_json`, `sssd_passkey_warns_only_of_its_last_three_attempts` |
| SSSD smartcard certificates ("Select Identity", PIN) | yes | yes | G-GREETER-SSSD-SMARTCARD; cargo: `sssd_smartcard_lists_certificates_then_asks_the_pin` |
| SSSD expired password (`_resettingPassword`) | yes | yes | G-GREETER-PASSWORD-EXPIRED; cargo: `an_expired_sssd_password_is_changed_through_pam_queries`, `only_the_password_mechanism_takes_the_expiry_notice` |
| Multi-step (2FA after the password, Back at step 2 soft-resets) | yes | yes | G-GREETER-MULTISTEP; cargo: `a_second_query_after_the_password_is_the_next_step` |
| Accessibility menu, Orca, OSK, input sources before login | yes | AT-SPI tree only | #1164 |
| Quick settings (power, volume, network) | yes | power actions (Suspend, Restart…, Power Off… straight to logind) on hardware only | G-GREETER-POWER-MENU; cargo: `power_actions_need_hardware_and_restart_buttons` |
| Timed login (row countdown, key or click starts over; prompt countdown under `disable-user-list`) | yes | yes | G-GREETER-TIMED-LOGIN, G-GREETER-TIMED-LOGIN-PROMPT; cargo: `long_delays_wait_for_five_idle_seconds` |
| Conflicting session dialog | yes | yes (in-window dialog; the timeout notice is a toast) | G-GREETER-CONFLICT, G-GREETER-CONFLICT-TIMEOUT; cargo: `the_banner_names_who_and_where`, `an_unanswered_dialog_times_out_after_a_minute`, `a_closed_dialogs_timer_does_nothing`, `the_conflicting_sessions_end_starts_the_login_and_stops_the_timer` |
| User switching: logged-in ring, return to the running session | yes | yes | G-GREETER-SWITCH-RETURN; cargo: `access_denied_verifies_on_the_greeters_channel`, `a_replaced_reauthentication_channel_is_closed_but_never_the_greeters`, `a_closed_channel_is_the_greeters_or_a_users`, `begins_for_a_user_ask_for_the_users_channel_and_the_rest_follow`, `calls_go_out_one_at_a_time_in_order`, `begins_on_the_greeters_channel_select_the_session_again`, `access_denied_is_gdms_refusal_only`, `reauthentication_channels_are_opened_for_running_sessions_only`, `users_with_a_session_are_ringed`, `only_a_return_to_an_active_login_screen_starts_over` |
| Realmd login hint | yes | no | — |
| Translations | yes | no | — |
| Greeter on every variant, `org.gnome.Shell@gdm.service` gone | — | no packaging yet | V-GREETER-LIST, V-GDM-LOGIN, V-GDM-FALLBACK |
