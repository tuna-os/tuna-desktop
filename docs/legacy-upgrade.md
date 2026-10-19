# Legacy upgrade and recovery

This is a bounded transition repair after [#775](https://github.com/tuna-os/tuna-desktop/pull/775),
not a declaration that every existing installation can safely upgrade.
The qualification table below distinguishes observed mechanisms from native
desktop routes that still require testing. Use disposable VMs for qualification,
keep another login-capable session or TTY available, and retain a known-good
package/deployment and a backup of the settings involved.

<!-- tuna-rename: keep-begin -->

## Exactly two temporary package paths

Fresh installs **also** contain these regular root-owned 0644 files:

| Path | Purpose | Protection |
|---|---|---|
| `/usr/share/wayland-sessions/roost.desktop` | Resolve the old saved session identifier; `NoDisplay=true`, current `Exec`/`TryExec=tuna-session`, no `Hidden=true` | Ordinary package file |
| `/etc/pam.d/roost-lock` | Keep the service name compiled into already-running old compositors available | Debian conffile; Arch `backup` entry |

The current `tuna.desktop` and `tuna-lock` remain the normal identities.
No old binaries, broad command symlinks or `ROOST_*` environment aliases are
restored. Package relations naming `roost` allow replacement of the old package;
they are not migration logic. There are no maintainer scripts that enumerate
homes, edit AccountsService, restore deleted authentication policy or terminate
sessions. The legacy PAM file is byte-for-byte identical to the published
beta file: changing only its comments would needlessly change its conffile
digest. Its historical “one-release” comment is retained for that reason; the
removal gates in this guide govern the current transition. The package cannot
safely infer that every remembered selection and
running process has moved on, so the narrow bridge is explicitly retained even
on fresh installations until the removal gates below are met.

### Remembered GDM sessions

GDM's saved session is a desktop-file identifier, not its `Exec` command. The
beta's hidden entry did not automatically turn a remembered `roost` selection
into `tuna`. The bridge supplies the old filename while using the current
launcher. At a later login, explicitly choose the visible **Tuna Desktop**
session; then verify that another login remembers the new selection. Do not
rewrite AccountsService records globally or infer success merely because the
new compositor ran. This follows GDM 51's `get_session_name`, saved-session
validation and worker session-name flow, but actual login/reselection evidence
is still required. [GDM 51 source](https://raw.githubusercontent.com/GNOME/gdm/51.0/daemon/gdm-session.c)

### A compositor that was already running

Replacing its executable on disk does not rewrite a running process's compiled
`roost-lock` service name. Keep the narrow service available, with the
administrator's intended policy, while such processes may still request it.
The packaged rules use `auth include login` and `account include login`, just
like the current service. Retaining the file is not proof that every local
`login` stack or old compositor can unlock: that requires an actual old-session
lock/unlock test after replacement. No forced logout, reboot or indiscriminate
PAM deletion is part of this repair.

An administrator who intentionally removed or denied the old service has made
a policy choice. No repair hook restores it; the package manager's handling of
an absent file still differs as described below. Before upgrading that
configuration, establish an independently usable recovery login and a reviewed
local unlock/upgrade precondition. A missing PAM service can fall back to the
machine's `other` policy; never assume a universal outcome from its absence.

## Settings adoption at session startup

The guard runs in `tuna-compositor` before its settings writers or supervised
shell start. `tuna-session` reaches that guard through the compositor; help,
version and the internal PAM helper exit before it. Standalone shell tools are
not a separate migration entry point.

These five sibling pairs are inspected together:

| Base | Old directory | Current directory |
|---|---|---|
| Config | `roost` | `tuna` |
| Cache | `roost` | `tuna` |
| Data | `roost` | `tuna` |
| Data | `roost-shell` | `tuna-shell` |
| State | `roost-shell` | `tuna-shell` |

This also covers an old-only home whose beta package was installed but never
launched. The later `state/tuna/session.json` feature was introduced with the
current name and is not an invented sixth migration pair.

| Observed state | Disposition |
|---|---|
| Neither entry exists | No change; no directories created |
| Current-only owned real directory, without group/other write | Preserve it; no adoption |
| Old-only owned real directory under a safe absolute base | Atomically create only `current -> old`, with a relative sibling target |
| Exact existing `current -> old` sibling link, caller-owned, with safe old directory and ancestry | Preserve it; idempotent |
| Distinct old/current entries, even two directories or an empty current directory | Stop with both paths and recovery guidance; never choose, merge or overwrite |
| Unexpected/dangling leaf link, wrong type/owner, unsafe writers or changed identity | Stop with recovery guidance; preserve entries |

Automatic adoption opens each ancestor and old directory without following
symlinks, checks ownership and replacement permissions, and pins descriptors.
Ancestors must be owned by root or the current user; writable shared ancestors
are allowed only when sticky-directory semantics protect the trusted-owned
next entry. The final base and old directory must belong to the current user
and have no group/other write bits, even if sticky. Root, set-id and secure
execution do not perform adoption. Symlinked home/XDG ancestry and parent
traversal are unsupported for automatic adoption; inspect and choose safe
physical absolute bases explicitly instead of following them silently.

All pairs are inspected before any link is created, then revalidated before
creation. Creation cannot replace an existing destination, including a dangling
link. If a later operation fails, completed links remain for inspection/retry;
rollback does not delete them. No pre-existing data, inode or entry is changed.
Directory timestamps naturally change when a link is added. The guard does not
walk or certify every file inside a settings tree, change data formats, or
establish a new boundary against subsequent mutations by the same owner/root.
It does not promise that links are immutable after validation.

### Relative and missing environment values

Resolution matches the existing readers rather than moving data to a different
base. Config uses an absolute `XDG_CONFIG_HOME`, otherwise `HOME/.config`;
cache uses `XDG_CACHE_HOME` as supplied (even relative/empty), otherwise
`HOME/.cache`. Data/state use absolute XDG values, otherwise an absolute HOME
with `.local/share`/`.local/state`; without either, those roots are absent.
Safe current-only relative config/cache directories and fresh states remain
usable. When an old entry at a relative config/cache base needs adoption, startup
stops with an unsupported-automatic-migration diagnostic. It does not silently
fall back to another directory or import old environment aliases.

## Package-manager distinctions

### Debian-format packages

Both PAM files are conffiles. Local modifications and deliberate removal are
handled by dpkg, and changed shipped content may require an administrator's
configuration-file decision. Disposable dpkg tests show that a deliberately
deleted PAM conffile can prompt when the shipped bytes change; noninteractive
environment alone does not answer that prompt. With no answer, installation
can leave the package unconfigured. Review and explicitly retain the intended
local policy or absence before proceeding; do not assume an unattended upgrade
will finish. Preserve a deliberate local policy rather than blindly accepting
the package copy. A removed-from-package conffile can remain
as obsolete configuration; absence from the new payload does not establish
absence on disk. This repair adds no cleanup hooks and does not pass
`--force-confnew` or `--force-confmiss`. [Debian configuration-file policy](https://www.debian.org/doc/debian-policy/ch-files.html#configuration-files)

### Arch replacement

Both services are listed in `backup`. Pacman's old/current/new comparisons may
preserve a modified file and produce `.pacnew`; replacement from a differently
named package needs its own test, not an assumption that it behaves like dpkg.
An intentionally missing file is not guaranteed to remain absent. Administrators
who require omission can review pacman's explicit `NoExtract` policy for the
exact path; this repair does not alter that configuration. Avoid automatically
restoring or deleting their policy. [pacman configuration handling](https://man.archlinux.org/man/pacman.8.en#HANDLING_CONFIG_FILES),
[NoExtract](https://man.archlinux.org/man/pacman.conf.5.en#OPTIONS)

### Marlin transactional deployments

The image build asserts the package bridge is present. That does not change an
already-running old deployment or prove activation, `/etc` policy preservation,
unlock or rollback. Qualify the actual deployment switch and recovery separately
with unchanged, modified and intentionally removed policy; retain a prior
login-capable deployment. Neither an image-content assertion nor an Arch staging
test qualifies this route.

## Recovery and downgrade

If the guard stops, preserve both named paths and their metadata before making
a choice. Use another login-capable session or a TTY; inspect ownership, types,
links and every conflicting copy. Choose the intended dataset explicitly, and
keep the other copy backed up. Do not fix a conflict with a blind recursive
delete, merge, ownership change or replacement symlink. A partial adoption is
safe to inspect and retry after the actual problem is resolved.

The relative links preserve the old directory for an older reader and make
new writes visible through both names. This is path-level downgrade access,
not a guarantee of application data-format compatibility or safe simultaneous
old/new writers. A package downgrade follows that manager's conffile/backup
rules and may need an administrator's decision. Recheck both PAM policies and
the login choice; do not purge configuration as a recovery shortcut.

## Qualification and reproducible evidence

| Check | Current evidence boundary |
|---|---|
| Original archive policy and exact bridge | Genuine synthetic `.deb` archives, native fixture ELF binaries and the actual checker; malformed/forbidden/link-path negatives |
| Arch package contents | Actual `PKGBUILD package()` staging, including permissive builder umask; no pacman transaction |
| Settings preservation | 39 production-module real-filesystem tests, including conflicts, pinned identities, races and retry; foreign/root/set-id identities modeled |
| Native compositor startup/readers | Source ordering reviewed; full workspace clippy/tests blocked locally by missing GLib development metadata |
| Debian lifecycle | 12 actual dpkg user-namespace tests pass with the original published beta and production-assets/synthetic-program candidate; exact built-candidate container evidence is still required |
| Private PAM | Revised chroot fixture is blocked locally by ambiguous native-library ownership; strict root provenance is required. Positive/negative native authentication awaits disposable-container CI |
| GDM saved login/reselection/second login | **Unqualified: requires native GDM evidence** |
| Running pre-rename compositor lock/unlock | **Unqualified: requires actual old compositor** |
| Arch replacement/deletion/downgrade | **Unqualified: requires native pacman transactions** |
| Marlin activation, policy preservation and rollback | **Unqualified: requires native deployment tests** |

Source checks, byte-path tests and a compiled old-service fixture cannot replace
the last four rows. Record exact source/package/image hashes, tool/library
versions, starting package state, changed/deleted configuration, command exit
codes, before/after manifests and recovery results. A private PAM test must use
an enforceable disposable filesystem boundary,
`pam_start_confdir`, a private `login` policy and explicit private `other` denial.
Only benign permit/deny modules and a synthetic user are allowed; host PAM
configuration and real credentials must be unreachable. A configuration-directory
argument alone is not evidence of an enforceable filesystem boundary. Start the
old executable before replacement but initialize PAM afterward, so it tests lookup by the
still-compiled name rather than a preloaded stack. Native-library provenance must
be established before loading: an unmapped namespace owner is not proof of root
ownership. Do not relax that requirement or replace native audit behavior just
to obtain a passing result. [Linux-PAM configuration lookup](https://raw.githubusercontent.com/linux-pam/linux-pam/v1.7.1/libpam/pam_handlers.c)

## Evidence required before removing the bridge

Remove each retained path only after its own gate passes on every advertised
upgrade route; elapsed releases, issue closure and beta installation are not
evidence of migration.

1. **Saved session:** native GDM fixtures start with the old saved identifier,
   cover direct and beta-never-launched upgrades, explicitly select Tuna Desktop,
   and demonstrate a subsequent remembered login after the legacy file is absent.
   Otherwise document and verify a safe user-controlled reselection precondition
   before package replacement; do not silently remove access.
2. **Running service:** actual pre-rename compositor fixtures establish how an
   upgrade with unchanged, modified and removed PAM policy remains unlockable,
   or enforce a separately reviewed safe upgrade precondition. Never assume
   executable replacement changed an existing process or force session shutdown.
3. **Data and recovery:** old-only, already-adopted and conflicting settings pass
   preservation/recovery checks, including downgrade access. Package-manager
   removal respects local policy and leaves obsolete configuration decisions to
   the administrator.
4. **Distribution routes:** Debian, Arch and Marlin each have artifact-bound
   upgrade/downgrade/rollback evidence, an independently reviewed removal patch
   and updated exact-allowance negative tests. Unqualified routes remain labeled.

<!-- tuna-rename: keep-end -->
