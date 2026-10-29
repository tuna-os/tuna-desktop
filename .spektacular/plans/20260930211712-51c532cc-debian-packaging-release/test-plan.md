---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Test plan: debian-packaging-release

Automated coverage (green in CI): 12 `normalize_version` unit tests, full
`cargo test --workspace`, clippy `-D warnings`, fmt check, shellcheck on both
scripts, and the journey job's release-build plus `check-release-package` run
against a real artifact.

Manual release-checklist procedures (run per release on a clean supported system):

1. **Clean install without pre-installed dependencies.** On a clean Debian
   stable / Ubuntu LTS (amd64) VM, `sudo apt install ./roost_X.Y.Z_amd64.deb`.
   Pass: installs with no errors, dependencies resolved automatically.
2. **Login screen lists Roost.** Pass: Roost is selectable and starts the
   desktop session.
3. **Version agreement.** Each installed binary's `--version` prints `X.Y.Z`,
   matching `dpkg-deb -f Version` and the release notes. Pass: all five agree.
4. **Upgrade preserves user data.** Install the previous release, create config
   and app data, install the new package over it. Pass: data intact, session
   starts.
5. **Uninstall is clean.** Remove the package. Pass: no Roost-owned binaries or
   session files remain; user configuration and application data preserved.
