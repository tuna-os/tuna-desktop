---
created_date: "2026-10-20"
document_status: final
closed_date: "2026-10-20"
---

# Manual native qualification: legacy upgrade transition

Status: **not executed; all routes below remain unqualified**. These procedures
cover the manual metrics identified in the approved implementation plan. The
passing package-policy, settings-safety and private lifecycle tests remain in
the automated suites; this document does not relabel them as native desktop
qualification or repeat their assertions as new manual requirements.

## Shared preparation and evidence

A release/QA maintainer runs each applicable route before advertising native
upgrade support or removing either temporary bridge. Use disposable VMs with
synthetic accounts and settings, a working independent recovery login, and a
snapshot plus known-good package/deployment. Never run these procedures on a
real user's active session or host authentication configuration. Session exits
and deployment activation are voluntary actions inside the disposable VM;
there is no forced logout/reboot or release publication.

Record the source commit, original package/image digests, actual package
versions, distribution/GDM/PAM/package-manager versions, VM configuration,
starting session identifier, executable identity and every operation's exit
status. Preserve bounded before/after settings and policy hashes/metadata,
including deliberately absent entries. Never record passwords, real account
information or unrestricted authentication/configuration logs.

Use the exact candidate built from the reviewed source. For Debian packages,
record `sha256sum "$OLD_DEB" "$CANDIDATE_DEB"` and
`dpkg-deb -f "$OLD_DEB" Version` / `dpkg-deb -f "$CANDIDATE_DEB" Version`;
classify the direction with `dpkg --compare-versions` before installing. The
existing CI candidate `0.1.0` is older than beta `1.0.0~beta.1`. A beta-to-newer
release claim requires a genuinely newer, provenance-recorded candidate or an
explicitly test-versioned build from identical source; do not rename an archive
or describe the older development artifact as newer. Test versions are never
published as releases. Install a Debian candidate only inside the VM with
`sudo dpkg -i "$CANDIDATE_DEB"`, preserving prompts and failures. Do not add
`--force-confnew` or `--force-confmiss` to obtain a green result.

For every procedure, a pass means all listed outcomes hold, with **zero lost,
merged or overwritten pre-existing settings attributable to migration**.
A missing required observation, unresolved prompt or recovery failure is not a
pass. Publish only the bounded, reviewed evidence and keep the route unqualified
until independent review accepts it.

<!-- tuna-rename: keep-begin -->

## 1. GDM saved-name login, explicit reselection and remembered login

**Metric:** Every advertised GDM upgrade route has a reproducible native result
or remains explicitly unqualified; no automatic saved-name migration claim.

**Setup:** Create separate VM snapshots for a genuine pre-rename installation
and a published beta installed over it but never launched. Establish the old
remembered selection by logging into the old desktop through GDM and voluntarily
returning to GDM. Do not write AccountsService files to manufacture a result.
Capture the effective saved identifier using the VM's AccountsService/GDM
read-only inspection and verify that it names `roost`. If that starting state
cannot be established, the case is not valid.

**Procedure:**
1. Apply the candidate using that distribution's package/deployment route.
   Verify `stat -c '%F %u:%g %a' /usr/share/wayland-sessions/roost.desktop`
   reports a regular root-owned 0644 file, and inspect it for `NoDisplay=true`,
   `Exec=tuna-session`, `TryExec=tuna-session` and no `Hidden=true`.
2. At GDM, authenticate the synthetic account without selecting a different
   session. Observe a usable Tuna Desktop and record its running executable
   identity. Mere file presence or a nested compositor launch does not pass.
3. Voluntarily log out, explicitly select the visible **Tuna Desktop** entry
   (the Marlin preview label may include “(preview)”), and log in. Inspect the
   saved selection read-only; it must now identify `tuna`.
4. Voluntarily log out and log in again without reselection. Verify the new
   selection remains remembered and the desktop is usable. Record both login
   outcomes and the saved-name observation.
5. Exercise documented recovery in a separate snapshot if login fails, retaining
   the previous package/deployment and independent login. Do not delete a bridge
   from a live session to disguise a failure.

**Pass:** The old saved selection reaches the current desktop, explicit
reselection persists through the subsequent login, recovery remains usable,
and there are zero unexplained settings losses. Complete both starting-state
cases for each advertised GDM distribution. Removing the hidden entry later
requires a separately reviewed candidate and the same saved-name tests after
its removal, or a verified user-controlled reselection prerequisite.

## 2. Actual running pre-rename compositor lock/unlock

**Metric:** Native running-session continuity has actual old-compositor
observations, or remains explicitly unqualified. Administrator policy is never
bypassed to obtain an unlock.

**Setup:** Boot a genuine pre-rename package in a disposable VM, log in using its
real compositor, and keep a separate recovery login available. Record
`readlink /proc/"$OLD_PID"/exe`, `stat -Lc '%d:%i' /proc/"$OLD_PID"/exe`, its
version/artifact hash and the old service name established by source/binary
provenance. `$OLD_PID` must be the observed compositor process, not the private
PAM fixture helper. Prepare separate unchanged, modified, deliberately missing
and explicitly denying old-policy VM snapshots; all policies concern only the
synthetic VM account. Keep those fixtures private.

**Procedure:**
1. Replace the package while the old compositor remains running. Do not restart
   it. Record the package result, any administrator conffile decision, old PID
   and executable inode. A prompt that cannot be answered safely is a recorded
   incomplete transaction, not unattended success.
2. Verify `/etc/pam.d/roost-lock` matches the intended preserved state (including
   deliberate absence where the manager preserves it). The ordinary packaged
   service includes the VM's `login` auth/account policy. Inspect actual native
   lookup and fallback behavior without transmitting credentials or host data.
3. Use the old desktop's real lock control, then its real unlock UI with the
   synthetic account. In an allowing case, observe a usable unlocked desktop.
   In denying cases, observe that denial cannot be bypassed, then recover
   through the independent login and the documented local policy decision.
   A missing service must be assessed against that VM's `other` policy; absence
   alone is not a universal denial guarantee.
4. Restore the prior package in a separate recovery snapshot and repeat the
   appropriate lock/unlock and policy inspection. Record the actual direction
   and manager decisions; never purge configuration as a shortcut.

**Pass:** The same actual old compositor remains observable across replacement,
allowing policy unlocks, denying policy stays denied, all intended local policy
states and settings are preserved according to the documented manager behavior,
and recovery works. Each changed/deleted-policy limitation must have a verified
safe prerequisite before its route is advertised. The existing synthetic
private-PAM cases cannot satisfy this procedure.

## 3. Native Arch replacement, local policy and downgrade

**Metric:** The Arch route has actual pacman transaction evidence or remains
explicitly unqualified; package staging does not qualify it.

**Setup:** Use a disposable Arch VM with a genuine installed `roost` package and
a second snapshot with the beta `tuna-desktop` package. Build the reviewed source
using `TUNA_LOCAL_SRC="$PWD" makepkg -f --dir packaging/arch` in a suitable
non-root build environment, recording source, PKGBUILD, package digest and
`pacman -Qp "$CANDIDATE_PKG"`. Retain genuine old packages. Capture
`pacman --version`, `pacman -Qii roost` or `pacman -Qii tuna-desktop`, and policy
hashes/metadata. Use separate unchanged, modified and deliberately deleted
old-service states. Record any existing explicit `NoExtract` policy; do not
silently add one during the test.

**Procedure:**
1. Run `sudo pacman -U "$CANDIDATE_PKG"` inside the VM with normal prompts;
   record differently named package replacement/conflict decisions and exit
   status. Do not use `--noconfirm` or force overwrite flags for qualification.
2. Inspect `pacman -Qii tuna-desktop`, `pacman -Ql tuna-desktop`, both protected
   PAM paths, any `.pacnew`/`.pacsave`, and both session files. Record whether a
   deliberately missing policy was reinstated. Do not call that preservation;
   require a documented, reviewed administrative prerequisite when needed.
3. Run the actual GDM and old-compositor procedures above for the Arch route.
   Verify old-only settings remain available through guarded current paths and
   that conflicting old/current data is preserved with recovery guidance.
4. In the recovery snapshot run `sudo pacman -U "$OLD_PKG"`, record all package
   replacement/configuration choices and recheck the older reader, login,
   policy and settings access. Do not delete backup files to simplify results.

**Pass:** All starting states have truthful transaction/policy results, required
native login/unlock outcomes or explicit safe prerequisites, usable downgrade
recovery and zero migration data loss. Observed pacman behavior must match the
published Arch guidance; do not infer it from dpkg or `PKGBUILD package()`.

## 4. Native Marlin deployment activation and rollback

**Metric:** The Marlin route has actual deployment and recovery evidence or
remains explicitly unqualified; image contents and clean-boot CI do not qualify
an upgrade.

**Setup:** Use a disposable VM with a known-good Marlin deployment containing
the genuine old desktop and a separately available recovery console. Retain
that login-capable deployment. Build the candidate with the actual
`packaging/marlin/Containerfile` and reviewed Arch package; record base-image,
candidate-image and package digests, source and `bootc --version` plus
`bootc status --json`. Use a private test image destination already authorized
for that test; do not publish a release or change a production channel.

**Procedure:**
1. Prepare separate unchanged, modified and intentionally removed PAM-policy
   starting deployments. Record `/etc` policy hashes/metadata, saved GDM name,
   settings state and running old-compositor identity.
2. Stage the candidate through Marlin's documented bootc deployment flow, using
   the exact recorded image digest. Record the staged/current deployment and
   observed treatment of `/etc` changes. Do not assume building an image has
   changed the active deployment or running process.
3. Observe the existing old-session lock/unlock behavior while its deployment
   remains active. Activate the candidate only with the VM operator's ordinary
   controlled session exit/boot procedure; no forced live-session termination.
   Record the booted deployment identity and execute the GDM saved-name and
   settings/recovery cases on that deployment.
4. Use the documented rollback flow to return to the retained known-good
   deployment, again under controlled VM operation. Record `bootc status --json`
   and verify login, intended policy and older settings-reader access.

**Pass:** Deployment identities and activation/rollback outcomes are recorded,
local policy behavior is explained for every starting state, actual GDM/session
checks succeed or have verified safe prerequisites, recovery remains available,
and no pre-existing settings are lost. Do not transfer Debian/Arch package-only
results to Marlin's active deployment or claim old-process unlock from a reboot.

<!-- tuna-rename: keep-end -->
