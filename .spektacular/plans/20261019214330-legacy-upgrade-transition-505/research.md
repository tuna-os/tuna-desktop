---
created_date: "2026-10-19"
document_status: final
closed_date: "2026-10-19"
---

# Research: 20261019214330-legacy-upgrade-transition-505

## Alternatives considered and rejected
- Revert #775 broadly: rejected because binaries/environment aliases are unnecessary for the two live consumers. Only saved desktop-file identity and compiled PAM-service identity require the transition paths.
- Rewrite AccountsService automatically: rejected because launching a new Exec does not change GDM's saved session name and cross-user/global updates exceed the approved scope.
- Move, merge or delete settings: rejected because it loses conflict/downgrade guarantees. Relative new-to-old links keep original bytes available.
- Force reinstatement of administrator-deleted PAM configuration: rejected because package-manager protections and local policy take precedence; absence remains documented recovery/qualification state.
## Chosen approach — evidence
- scripts/tuna-release and packaging/arch/PKGBUILD own artifact installation. Add precisely the two bridge files with existing configuration protection mechanisms.
- crates/compositor/src/main.rs separates argument-only exits and internal PAM handling from desktop startup, providing a narrow pre-writer adoption integration point.
- The previous adoption implementation linked five sibling pairs but skipped conflicts and followed symlinks. Reimplement safety checks in a small compositor module using existing rustix.
- scripts/check-release-package lacks a forbidden-path allowlist and validates extracted files after archive processing. Inspect bounded original tar members first and exercise actual checker negatives.
## Files examined
- rust-wayland-desktop:scripts/tuna-release — current Debian payload and conffile production.
- rust-wayland-desktop:scripts/check-release-package — genuine artifact/ELF/dependency checks and missing negative policy.
- rust-wayland-desktop:scripts/lib/release-deb-ownership.py — bounded original-archive reader.
- rust-wayland-desktop:scripts/lib/installed-deb-proof-tests.py — existing mocked tool fixtures need compatibility updates and genuine tests alongside them.
- rust-wayland-desktop:scripts/check-deb-clean-install — forbidden-path checks currently miss dangling symlinks.
- rust-wayland-desktop:packaging/arch/PKGBUILD — pacman backup/install policy.
- rust-wayland-desktop:packaging/marlin/Containerfile — image assertions only, not native activation evidence.
- rust-wayland-desktop:crates/compositor/src/main.rs — startup boundary.
- rust-wayland-desktop:crates/compositor/src/wallpaper.rs — relative cache/HOME environment accepted by some readers; migration must not pretend all resolver behavior matches.
- rust-wayland-desktop:crates/compositor/src/monitors.rs — config/HOME resolver behavior.
- rust-wayland-desktop:crates/shell-host/src/xdg.rs — absolute XDG data/state fallbacks.
- rust-wayland-desktop:scripts/rename-to-tuna — explicit keep markers need narrowly scoped retained filenames.
## External references
- https://raw.githubusercontent.com/GNOME/gdm/51.0/daemon/gdm-session.c — saved names and worker saved-session validation are desktop-file names, independent of Exec.
- https://www.debian.org/doc/debian-policy/ap-pkg-conffiles.html — conffile preservation rules; actual dpkg tests establish route details.
- https://man.archlinux.org/man/pacman.8.en#HANDLING_CONFIG_FILES — configuration backup behavior, separate from dpkg.
- https://raw.githubusercontent.com/linux-pam/linux-pam/v1.7.1/libpam/pam_handlers.c — documented private configuration lookup; tests additionally require a verified disposable filesystem boundary and an explicit private deny fallback.
## Open assumptions
- No native GDM, actual old compositor, pacman, container engine or VM runner is available here. Those routes remain unqualified; support claims depend on later native evidence, not assumed success.
- Available dpkg, compiler and libpam pam_start_confdir allow disposable mechanism tests. A failed environment prerequisite is reported rather than replaced by a passing mock.
## Resolver safety decision

### Relative current-only configuration
- **Decision**: automatic adoption requires absolute anchored owned paths; current-only relative configuration remains usable when no adoption occurs. Reject ancestry replaceable by another unprivileged user, honoring sticky-directory ownership rules.
- **Rationale**: current readers accept relative paths in selected environments; hardening automatic adoption must preserve safe fresh and current-only behavior.
- **Rejected**: changing all runtime resolvers or blanket rejection of relative current-only state would expand the repair unnecessarily.
