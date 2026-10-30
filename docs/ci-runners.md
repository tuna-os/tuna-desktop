# Self-hosted CI runners (karnataka)

Most `ci` lanes run on self-hosted GitHub Actions runners on `karnataka`, the
k3s node that also hosts KubeVirt, corral-web and the `punjab` VM. GitHub's
own runners are still used for fork pull requests, for the lanes listed under
[What runs where](#what-runs-where), and for everything when the
[rollback switch](#roll-back-to-github-hosted-runners) is on.

Everything below assumes `kubectl --context karnataka-k3s`, `helm` (v3.8+ or
v4), `gh` logged in as an org admin, and `ssh karnataka` with sudo.

## Components

| Piece | Where | What |
| --- | --- | --- |
| ARC controller | namespace `arc-systems`, Helm release `arc` | `gha-runner-scale-set-controller` chart; watches only `tuna-ci` |
| Scale set `tuna-ci-24.04` | namespace `tuna-ci`, release `tuna-ci-24-04` | [values](../.github/runners/values-24.04.yaml); jobs that use `ubuntu-latest` on GitHub |
| Scale set `tuna-ci-26.04` | namespace `tuna-ci`, release `tuna-ci-26-04` | [values](../.github/runners/values-26.04.yaml); jobs that use `ubuntu-26.04` on GitHub |
| Runner image | node-local containerd, `localhost/tuna-ci-runner:<ubuntu>-<runner>-<stamp>` | [Containerfile](../.github/runners/image/Containerfile), built by [build-image](../.github/runners/build-image) |
| Guard rails | [cluster.yaml](../.github/runners/cluster.yaml) | namespace, admission policy, NetworkPolicy, quota, priority class |
| Scripts | [.github/runners](../.github/runners) | `build-image` (images), `deploy` (guard rails + Helm releases) |
| GitHub side | org `tuna-os` | runner group `tuna-desktop-karnataka` (only repository: `tuna-os/tuna-desktop`), secret `tuna-ci-github` in `tuna-ci` |

Both chart releases use the same chart version (0.15.1 at install).

## What runs where

The `runs-on` of a moved job is a YAML alias (`*runner-24` / `*runner-26`)
of the expressions anchored on `select-lanes` and `orca-speech` in
`.github/workflows/ci.yml`. They pick the self-hosted scale set when the
repository variable `CI_RUNNERS` is not `hosted` **and** the event is a push,
schedule, dispatch or a pull request whose head repository is
`tuna-os/tuna-desktop`; otherwise the original GitHub-hosted label.

| Runner | Lanes |
| --- | --- |
| `tuna-ci-24.04` | select-lanes, check, fprint-phase-proof, app-content, scroll-proof, grid-cancel-comparison, gtk-shell, parity-ledger, docs-shots, ci-gate |
| `tuna-ci-26.04` | orca-speech |
| GitHub-hosted, always | arch-package, drm-smoke, marlin-vm (both matrix legs) |
| GitHub-hosted, not yet proven self-hosted | journey, portal-security, remote-security, niri-parity, marlin-image |

Why the always-hosted lanes stay hosted:

- **arch-package** is a `container:` job; the runner needs a Docker daemon
  to start the job container, and a daemon needs a privileged pod.
- **drm-smoke** loads `vkms` (and may install its kernel modules) into the
  runner's kernel. On karnataka that is the shared host kernel under punjab
  and KubeVirt, a Fedora kernel the Ubuntu module install cannot target, and
  the module would expose a DRM device to the whole node.
- **marlin-vm** needs more than `/dev/kvm`: it runs
  `podman run --privileged --pid=host -v /dev:/dev … bootc install to-disk
  --via-loopback`, i.e. loop devices and a privileged container, which is
  root on the node. KVM through KubeVirt's device plugin would work, the
  disk build would not.

journey's self-hosted attempt failed in "Qualify disposable Debian lifecycle"
(`end of file on stdin at conffile prompt`), but that failure came from #899
changing a tuna-lock conffile, not from the runners; #930 restores it.

The podman lanes (journey, and portal-security, remote-security, niri-parity
and marlin-image, which also wait on hosted `arch-package`); rootless podman, `podman
build` and `docker run` work in the runner pods, but these lanes have not
passed self-hosted yet. Move one by switching its `runs-on` to the alias
and proving it green on a pull request.

Other workflows (`stress`, `fuzz`, `baseline`, …) still use GitHub-hosted
runners; the same aliases can move them.

## Security model

The repository is public, so the controls assume a job can run arbitrary
code as root inside its pod.

1. **Only this repository's own branches reach the runners.**
   - Workflow: the `runs-on` expression above sends fork pull requests to
     GitHub-hosted runners.
   - Runner image: a fork could edit that expression in its own pull
     request, so every job first runs
     [`job-started.sh`](../.github/runners/image/job-started.sh)
     (`ACTIONS_RUNNER_HOOK_JOB_STARTED`, baked into the image, out of reach
     of workflows). It fails the job before checkout unless the repository
     is `tuna-os/tuna-desktop` and the event is `push`,
     `workflow_dispatch`, `schedule`, or `pull_request` whose head
     repository is `tuna-os/tuna-desktop`.
   - GitHub: the runner group `tuna-desktop-karnataka` has a single selected
     repository, and the repository requires maintainer approval before
     workflows run for **all** outside contributors' pull requests
     (Settings → Actions → General → "Require approval for all external
     contributors").
2. **One job per pod, nothing persists.** ARC registers each pod as an
   ephemeral just-in-time runner; the runner exits after one job and the pod,
   with its emptyDir volumes, is deleted. There is no shared cache volume.
3. **The pod cannot become the node.** The admission policy
   `tuna-ci-runner-pods` rejects any pod in `tuna-ci` that is not in its own
   user namespace (`hostUsers: false`), is privileged, shares host
   namespaces, binds host ports, mounts anything but emptyDir, mounts a
   service account token, or adds a capability other than `SYS_ADMIN`. Root
   and `CAP_SYS_ADMIN` in the pod (needed for `sudo apt-get` and rootless
   podman's nested user namespace) map to an unprivileged 65536-uid range on
   the node. No host paths are mounted; `/dev/net/tun` (podman's network)
   comes from KubeVirt's device plugin.
4. **No cluster access.** No service account token is mounted and the
   service accounts have no roles. NetworkPolicy `runners-internet-only`
   denies all ingress and allows egress only to cluster DNS and public
   addresses: the Kubernetes API (Service and `:6443` on every node
   address), the kubelet, other namespaces' pods and Services (corral-web,
   punjab, KubeVirt) and link-local metadata are unreachable.
5. **Headroom.** ResourceQuota `runner-ceiling` caps the namespace at 8 pods,
   32 CPU / 112 GiB limits, 16 CPU / 48 GiB requests. Each runner asks for
   2 CPU / 6 GiB and is limited to 4 CPU / 14 GiB / 45 GiB disk. Runner pods
   use PriorityClass `tuna-ci-runner` (-100, never preempts): new VMs and
   cluster services preempt them, and the kubelet evicts them first under
   memory pressure.

The ARC listener uses the GitHub credential in secret `tuna-ci-github`
(`tuna-ci` namespace, copied by the controller to `arc-systems`). Runner
pods never mount it and cannot reach the API to read it.

### Node prerequisite: NetworkPolicy enforcement

k3s enforces NetworkPolicy with its embedded kube-router, which reads rules
with `iptables-save`. Tailscale in nftables mode writes rules that
`iptables-save` cannot parse, which silently stops **all** NetworkPolicy
enforcement (`Aborting sync. Failed to run iptables-save` in the k3s log).
karnataka therefore runs tailscaled in iptables mode:

```
# /etc/systemd/system/tailscaled.service.d/10-firewall-iptables.conf
[Service]
Environment=TS_DEBUG_FIREWALL_MODE=iptables
```

`k3s.service` depends on `tailscaled.service`, so restarting tailscaled
restarts k3s (workloads keep running; the API is down for ~10 s). Check
enforcement after any tailscale or k3s upgrade:

```
ssh karnataka 'sudo iptables-save -t filter >/dev/null && echo ok'
ssh karnataka "sudo journalctl -u k3s --since -10min | grep -c 'Aborting sync'"   # expect 0
```

## Install from scratch

```
# 1. Guard rails (namespace, admission policy, NetworkPolicy, quota, priority).
kubectl --context karnataka-k3s apply -f .github/runners/cluster.yaml

# 2. GitHub runner group restricted to this repository.
rid=$(gh api repos/tuna-os/tuna-desktop --jq .id)
gh api -X POST orgs/tuna-os/actions/runner-groups -f name=tuna-desktop-karnataka \
  -f visibility=selected -F "selected_repository_ids[]=$rid" -F allows_public_repositories=true

# 3. Credential, from files, never on the command line (see "Credentials").
kubectl --context karnataka-k3s -n tuna-ci create secret generic tuna-ci-github \
  --from-file=github_app_id=app_id --from-file=github_app_installation_id=installation_id \
  --from-file=github_app_private_key=app.pem

# 4. Images: builds both on karnataka, imports them into k3s' containerd and
#    points values-24.04.yaml / values-26.04.yaml at the new tags.
.github/runners/build-image

# 5. Guard rails again (idempotent), ARC controller, both scale sets.
.github/runners/deploy
```

Check: `kubectl -n tuna-ci get autoscalingrunnersets` shows both `Running`,
and `gh api orgs/tuna-os/actions/runner-groups/<id>/runners` lists the idle
`tuna-ci-24.04-runner-*`.

### Credentials

The listener needs a GitHub credential that may create runner registration
tokens for the org. Preferred: a GitHub App owned by `tuna-os` with only
**Organization → Self-hosted runners: Read and write**, installed on the org
(secret keys `github_app_id`, `github_app_installation_id`,
`github_app_private_key`). A classic PAT works too (key `github_token`, scope
`admin:org`), but carries far more power than ARC needs. Replace the secret
in place and restart the listeners:

```
kubectl --context karnataka-k3s -n tuna-ci create secret generic tuna-ci-github \
  --from-file=… --dry-run=client -o yaml | kubectl --context karnataka-k3s apply -f -
kubectl --context karnataka-k3s -n arc-systems delete pod -l app.kubernetes.io/component=runner-scale-set-listener
```

Files holding credentials: create with `umask 077`, delete after use, strip
the trailing newline (`tr -d '\n'`), never echo them.

## Upgrade

**Runner image** (GitHub stops sending jobs to runners more than 30 days
behind the latest `actions/runner` release, so rebuild at least monthly, and
whenever a job needs a new preinstalled tool):

```
.github/runners/build-image             # new tags written into the values files
.github/runners/deploy --sets-only      # new pods use them; running jobs finish on the old
git commit .github/runners/values-*.yaml
# after running jobs finished, drop the old tags:
ssh karnataka 'sudo k3s ctr -n k8s.io images ls -q | grep tuna-ci-runner'
ssh karnataka 'sudo k3s ctr -n k8s.io images rm localhost/tuna-ci-runner:<old tag>'
```

`values-24.04.yaml` and `values-26.04.yaml` differ only in the scale set
name, image and min/max runners; change both together.

Images are node-local and `imagePullPolicy: Never`; if kubelet image garbage
collection ever removes one (disk above 85%), pods fail with
`ErrImageNeverPull`: rebuild.

**ARC chart**: controller and scale sets must run the same chart version.
Bump `ARC_CHART_VERSION`'s default in `deploy` (and the version in this
page), then run `.github/runners/deploy`: it upgrades the controller first.

## Scale

`minRunners` (idle, pre-registered pods) and `maxRunners` live in the values
files: 24.04 is 1–5, 26.04 is 0–3. Keep `maxRunners` of both sets within the
quota's 8 pods; change the quota in `cluster.yaml` only together with a look
at punjab's and KubeVirt's needs (`kubectl describe node karnataka`). Apply
with `.github/runners/deploy --sets-only`. A temporary change without editing
files applies the same `--set` to both sets:

```
.github/runners/deploy --sets-only -- --set maxRunners=2
```

## Drain (maintenance on karnataka)

1. Send new jobs to GitHub: `gh variable set CI_RUNNERS -b hosted -R tuna-os/tuna-desktop`.
2. Stop creating runners:
   `.github/runners/deploy --sets-only -- --set minRunners=0 --set maxRunners=0`.
   Running jobs finish; idle runners are removed.
3. Wait for `kubectl --context karnataka-k3s -n tuna-ci get pods` to be
   empty (or cancel the remaining runs).
4. Undo in reverse order: `.github/runners/deploy --sets-only`, then
   `gh variable delete CI_RUNNERS -R tuna-os/tuna-desktop`.

Jobs already queued for a `tuna-ci-*` label when you flip the variable keep
waiting for that label: re-run them (`gh run rerun <id> --failed` after
cancelling) so they pick the hosted label.

## Roll back to GitHub-hosted runners

One switch:

```
gh variable set CI_RUNNERS -b hosted -R tuna-os/tuna-desktop
```

Every job evaluated afterwards uses its original GitHub-hosted label
(`ubuntu-latest` / `ubuntu-26.04`); assertions are identical on both.
Re-run queued runs as in [Drain](#drain-maintenance-on-karnataka).
`gh variable delete CI_RUNNERS -R tuna-os/tuna-desktop` switches back.

Full removal:

```
helm --kube-context karnataka-k3s uninstall tuna-ci-24-04 tuna-ci-26-04 -n tuna-ci
helm --kube-context karnataka-k3s uninstall arc -n arc-systems
kubectl --context karnataka-k3s delete -f .github/runners/cluster.yaml
kubectl --context karnataka-k3s delete namespace arc-systems
gh api -X DELETE orgs/tuna-os/actions/runner-groups/<id>
ssh karnataka 'sudo k3s ctr -n k8s.io images ls -q | grep tuna-ci-runner | xargs -r sudo k3s ctr -n k8s.io images rm'
```

and replace the `*runner-24` / `*runner-26` aliases and anchors in `ci.yml`
with `ubuntu-latest` / `ubuntu-26.04`.

## Caches

`actions/cache` (and `Swatinem/rust-cache` on top of it) works unchanged on
the self-hosted runners: it talks to GitHub's cache service over HTTPS, and
the cache keys (`ubuntu-24.04`, `ubuntu-26.04` from `/etc/os-release`) match
the hosted runners', so both share entries. As before, only trusted `main`
runs save Rust caches; pull requests restore.

A shared sccache volume or bucket was considered and not deployed. Jobs run
as root in their pod, so any job that could write a shared compiler cache
could plant objects that later `main` builds (and published images) link,
and a shared volume would break "nothing persists between jobs". sccache has
no per-entry integrity check that would make a shared writable cache safe
across branches.

## Troubleshooting

| Symptom | Look at |
| --- | --- |
| Jobs stay queued on a `tuna-ci-*` label | `kubectl -n arc-systems logs -l app.kubernetes.io/component=runner-scale-set-listener`; `kubectl -n tuna-ci get ephemeralrunners`; `kubectl -n tuna-ci describe resourcequota` |
| Pod rejected | `kubectl -n tuna-ci get events` (admission policy message names the rule) |
| `refused this job` in "Set up job" | the job-started hook: fork, foreign repository or disallowed event |
| A step misses a tool GitHub's image has | add it to the Containerfile's package list, rebuild, roll |
| Nested podman/docker errors | needs `/dev/net/tun` (tun resource), the emptyDir storage mounts and `SYS_ADMIN`, all in the values files |
| Controller cannot get a registration token | credential expired or lacks org runner permission; replace the secret |
