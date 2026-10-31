# Cutting a Tuna Desktop release

Target: TunaOS Marlin (Arch-based bootc, x86_64), with GNOME 51 as the comparison baseline ([ADR 0007](adr/0007-gnome51-marlin-baseline.md)). The project remains a developer preview until the roadmap release gates pass. Debian/Ubuntu production support must include the preferred GTK shell and
actual installed-session control qualification. The first package lane targets
Ubuntu 26.04 amd64; Debian 13 still needs a packaged gtk4-layer-shell >=1.1
backport and a separately built/qualified artifact. These package targets do
not provide Marlin or physical hardware qualification.

## Checklist

1. Land everything for the release on `main` with green CI.
2. Fill the visual review: copy `docs/reviews/TEMPLATE.md` to
   `docs/reviews/vX.Y.Z.md`, compare the CI proof frames against the
   GNOME 51 baseline bundle, and commit it with no `TBD` left. The
   release script refuses a tagged build without it.
3. Draft the release notes below (changes, known limitations, supported
   configurations).
4. Tag the release: `git tag vX.Y.Z && git push origin vX.Y.Z`.
5. From a clean checkout of the tag, build the package:
   `scripts/tuna-release` — writes `dist/tuna-desktop_X.Y.Z_amd64.deb`.
   Use `--allow-dirty` only for throwaway dev builds, never for a release.
6. Run `scripts/check-release-package` on the artifact. Require all six
   binaries, complete version agreement and the generated ELF dependency
   closure plus explicit runtime floors. Install it on the selected target
   with apt, retaining package/hash/distro dependency receipts and the
   installed GTK control proof; an extracted/debug binary is insufficient.
   Verify the artifact with `scripts/check-deb-clean-install`: require a
   root-owned archive with the GTK shell, then install on a pristine
   `ubuntu:26.04` container without recommendations. Separately confirm the
   login screen lists Tuna Desktop on a real install and every binary's
   `--version` matches the tag.
7. Capture the demo media from the same tag:
   `scripts/tuna-capture --bin-dir /usr/bin --artifacts walkthrough-media`
   captures the installed release's GTK shell and six-feature core tour
   (clip plus still per segment), and checks the segment/template contract.
   Without `--bin-dir`, it builds debug binaries for development only.
   The capture uses real GTK fixture windows, a private bus and isolated
   runtime/configuration/state; it records binary versions and SHA256 hashes.
   Install `ffmpeg`, `Xvfb`, `xdotool`, `jq`, GLib's `gsettings`/`gdbus`,
   and `/usr/bin/python3` with GTK 4 introspection for this capture.
   The recorded state must reach overview, two managed application windows,
   scroll layout, and an awake locked session; successful file creation alone
   is not feature proof.
   Review every still before uploading. Diff against the prior release's
   stills when available; missing old media is not comparison evidence.
8. Render the release media index:
   `scripts/tuna-capture --artifacts walkthrough-media --render-walkthrough vX.Y.Z`
   writes `walkthrough-media/walkthrough.md` with versioned release-asset
   links. It does not replace the manifest-generated `docs/walkthrough.md`.
9. Attach the `.deb`, the notes, the media index, and the twelve media files
   (`<segment>.mp4` + `<segment>.png` per segment) to the release,
   e.g. `gh release upload vX.Y.Z walkthrough-media/walkthrough.md
   walkthrough-media/*.mp4 walkthrough-media/*.png`.
   Link the media index from the release notes. Refresh the canonical feature
   walkthrough only through its [visual capture workflow](visual-capture-refresh.md).

The package version, the binaries' `--version` output, and the notes must
agree — the release script enforces the first two by construction.

## Prereleases (betas and release candidates)

Tag a prerelease with a SemVer suffix: `vX.Y.Z-beta.N`, then `vX.Y.Z-rc.N`.
Every checklist step above applies unchanged, including the filled
`docs/reviews/vX.Y.Z-beta.N.md`. The binaries report `X.Y.Z-beta.N`; the
package versions are mapped so the final `X.Y.Z` always upgrades over them:

| Tag | `--version` | Debian `Version` | Arch `pkgver` (`_semver` in `packaging/arch/PKGBUILD`) |
|---|---|---|---|
| `v1.0.0-beta.1` | `1.0.0-beta.1` | `1.0.0~beta.1` | `1.0.0beta.1` |
| `v1.0.0-beta.2` | `1.0.0-beta.2` | `1.0.0~beta.2` | `1.0.0beta.2` |
| `v1.0.0-beta.3` | `1.0.0-beta.3` | `1.0.0~beta.3` | `1.0.0beta.3` |
| `v1.0.0` | `1.0.0` | `1.0.0` | `1.0.0` |

Publish it as a GitHub prerelease (`gh release create vX.Y.Z-beta.N
--prerelease`) and list every open release gate under "Known limitations":
a beta does not claim the gates in the [roadmap](roadmap.md#global-decision-gates).

### Limited beta.3 bootstrap authorization

The project owner approved a limited `v1.0.0-beta.3` release from
`release/beta3-bootstrap`, rooted at the main ancestor
`d65e35f27237576c6708978cb8be6c0d9e048768`, instead of waiting for the
latest main to become green. This is a source-explicit exception to the
**branch** in checklist step 1, not to qualification. That ancestor includes
the `/etc/localtime` monitor absent from the published beta.2 desktop.
Necessary fixture/dependency fixes must be recorded as explicit backports;
unrelated newer feature stacks are not part of this limited source.

Before tagging or publishing, the final release head must have new successful
selected CI, `check` and `ci-gate` results, completed visual review, and real
installed-package/control evidence bound to that head and package hash.
Historical successes on an equivalent tree are context, not qualification.
No assertion, threshold, timeout, or dependency floor may be relaxed to
qualify it. A selected fresh-account proof must exercise its required
behavior; absent proof is not a pass. Review and notes must retain the open
release gates: this limited beta is not v1 or roadmap #577 completion.

For the untagged candidate, declare beta.3 in Arch's `_semver` and build the
Debian package with `scripts/tuna-release --candidate-version 1.0.0-beta.3`.
Both installed packages must report the intended beta.3 version rather than
the nearest older tag. Candidate mode refuses an exact release-tagged HEAD;
after tagging, use the ordinary tag-derived command and review gate.

After qualification, use the normal new-tag/prerelease, deterministic vendor,
signed package factory, served-package verification and trusted Marlin image
publication paths. Then qualify the new immutable image digest with the
strict published clock proof; an older preview or an exemption for the old
published package cannot stand in for the new image's result.

## Upgrade and uninstall

Follow the [legacy-upgrade and recovery guide](legacy-upgrade.md) before
qualifying an upgrade. Require the exact two-file transition allowance,
non-destructive settings behavior and manager-specific modified/deleted
configuration evidence. Real GDM remembered-name/reselection, a running old
compositor's unlock, Arch replacement and Marlin activation/rollback need
their own native results; archive, user-namespace dpkg and private-PAM
mechanisms cannot stand in for those tests. Do not remove the bridge based
only on elapsed releases or the fact that the beta was installed.

Uninstalling removes the Tuna Desktop-owned program files and session entries; user configuration and application data
are preserved.

## Recovering from a bad release

If a release already tagged, built, or published turns out to be wrong, see
[docs/release-rollback.md](release-rollback.md) for the recovery steps —
there is no automated rollback, so this is a manual process ordered by how
far the bad release already traveled.

## Release notes template

```markdown
# Tuna Desktop X.Y.Z

## Changes
- ...

## Known limitations
- ...

## Supported configurations
- Debian ... / Ubuntu ... (amd64)
```
