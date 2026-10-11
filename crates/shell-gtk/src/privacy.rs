//! Privacy device enforcement on the PipeWire graph.
//!
//! GTK-free: the [`Tracker`] plans mute/destroy steps from graph
//! scans ([`AudioState::privacy_scan`](crate::audio_state::AudioState::privacy_scan))
//! and the caller executes them — the GLib loop through the existing
//! async `wpctl`/`pw-cli` helpers, blocking test drivers through
//! [`run_blocking`].
//!
//! Enforcement rules (see `docs/privacy-enforcement.md` for the
//! GNOME comparison):
//!
//! - Microphone disabled: every `Audio/Source` reads muted. Capture
//!   streams keep running but capture silence; an unmute attempt is
//!   re-muted on the next graph event. Nothing is destroyed, so
//!   explicit restore just unmutes.
//! - Sound output disabled: every `Audio/Sink` reads muted, same
//!   re-mute and restore shape.
//! - Camera disabled: `Video/Source` nodes and video-capture streams
//!   linked to them are destroyed, existing and newly appearing.
//!   Unlinked capture streams capture nothing and are left alone, so
//!   a screen-share consumer linking up later is never broken by a
//!   camera kill-switch.
//!
//! Restore is explicit and observable: flipping a key back unmutes
//! exactly the nodes this tracker muted (a node the user muted by
//! hand stays muted), and video streams created after restore are
//! no longer destroyed.

use std::collections::BTreeSet;
#[cfg(test)]
use std::time::{Duration, Instant};

/// One enforcement step against the PipeWire graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Set a node's mute flag by id (`wpctl set-mute`).
    SetMute {
        /// Node id from `pw-dump`.
        node: u64,
        /// Target mute state (`false` only on restore).
        muted: bool,
    },
    /// Destroy a node by id (`pw-cli destroy`): camera devices and
    /// their capture streams while the camera is disabled.
    DestroyNode {
        /// Node id from `pw-dump`.
        node: u64,
    },
}

/// One graph scan folded against the policy: what must be muted,
/// what must not exist, and what is muted right now. Built by
/// [`AudioState::privacy_scan`](crate::audio_state::AudioState::privacy_scan).
#[derive(Debug, Default)]
pub struct PrivacyScan {
    /// Nodes that must read muted right now.
    pub mute_required: BTreeSet<u64>,
    /// Camera nodes/streams that must not exist right now.
    pub destroy: Vec<u64>,
    /// Nodes currently muted.
    pub muted_now: BTreeSet<u64>,
}

/// `wpctl` argv for an action (`None` for non-wpctl actions).
pub fn wpctl_args(action: &Action) -> Option<[String; 3]> {
    match action {
        Action::SetMute { node, muted } => Some([
            "set-mute".to_owned(),
            node.to_string(),
            if *muted {
                "1".to_owned()
            } else {
                "0".to_owned()
            },
        ]),
        Action::DestroyNode { .. } => None,
    }
}

/// `pw-cli` argv for an action (`None` for non-pw-cli actions).
pub fn pw_cli_args(action: &Action) -> Option<[String; 2]> {
    match action {
        Action::DestroyNode { node } => Some(["destroy".to_owned(), node.to_string()]),
        Action::SetMute { .. } => None,
    }
}

/// Enforcement state across scans: which nodes this tracker muted,
/// so explicit restore unmutes exactly those.
#[derive(Debug, Default)]
pub struct Tracker {
    muted_by_policy: BTreeSet<u64>,
}

impl Tracker {
    /// Plan one round: mute what the policy requires, destroy what
    /// must not exist, and unmute tracked nodes the policy no longer
    /// requires muted. A tracked node that is already unmuted (or
    /// gone from the graph) is forgotten silently.
    pub fn plan(&mut self, scan: &PrivacyScan) -> Vec<Action> {
        let mut actions = Vec::new();
        for id in scan.mute_required.difference(&scan.muted_now) {
            actions.push(Action::SetMute {
                node: *id,
                muted: true,
            });
            self.muted_by_policy.insert(*id);
        }
        for id in &scan.destroy {
            actions.push(Action::DestroyNode { node: *id });
        }
        let stale: Vec<u64> = self
            .muted_by_policy
            .difference(&scan.mute_required)
            .copied()
            .collect();
        for id in stale {
            self.muted_by_policy.remove(&id);
            if scan.muted_now.contains(&id) {
                actions.push(Action::SetMute {
                    node: id,
                    muted: false,
                });
            }
        }
        actions
    }

    /// Nodes this tracker muted and has not restored yet (tests).
    #[cfg(test)]
    fn tracked(&self) -> &BTreeSet<u64> {
        &self.muted_by_policy
    }
}

/// Bound on one blocking enforcement command (tests only).
#[cfg(test)]
const RUN_TIMEOUT: Duration = Duration::from_secs(5);

/// Run one action to completion, blocking (test drivers only; the
/// shell dispatches through the async GLib helpers instead).
/// The child sees `runtime`/`address` as its PipeWire runtime and
/// session bus, so parallel harnesses never share audio state.
/// `false` when the tool is missing, fails, or overruns its bound.
#[cfg(test)]
pub fn run_blocking(action: &Action, runtime: &std::path::Path, address: &str) -> bool {
    let (tool, args): (&str, Vec<String>) = match action {
        Action::SetMute { .. } => {
            let Some(argv) = wpctl_args(action) else {
                return false;
            };
            ("wpctl", argv.into())
        }
        Action::DestroyNode { .. } => {
            let Some(argv) = pw_cli_args(action) else {
                return false;
            };
            ("pw-cli", argv.into())
        }
    };
    let mut child = match std::process::Command::new(tool)
        .args(&args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(mute_required: &[u64], destroy: &[u64], muted_now: &[u64]) -> PrivacyScan {
        PrivacyScan {
            mute_required: mute_required.iter().copied().collect(),
            destroy: destroy.to_vec(),
            muted_now: muted_now.iter().copied().collect(),
        }
    }

    #[test]
    fn command_lines_target_the_node() {
        assert_eq!(
            wpctl_args(&Action::SetMute {
                node: 42,
                muted: true
            }),
            Some(["set-mute".to_owned(), "42".to_owned(), "1".to_owned()])
        );
        assert_eq!(
            wpctl_args(&Action::SetMute {
                node: 42,
                muted: false
            }),
            Some(["set-mute".to_owned(), "42".to_owned(), "0".to_owned()])
        );
        assert_eq!(
            pw_cli_args(&Action::DestroyNode { node: 61 }),
            Some(["destroy".to_owned(), "61".to_owned()])
        );
        assert_eq!(wpctl_args(&Action::DestroyNode { node: 61 }), None);
        assert_eq!(
            pw_cli_args(&Action::SetMute {
                node: 42,
                muted: true
            }),
            None
        );
    }

    #[test]
    fn plan_mutes_unmuted_destroys_cameras_and_ignores_the_rest() {
        let mut tracker = Tracker::default();
        let actions = tracker.plan(&scan(&[48, 49], &[60, 61], &[49]));
        assert_eq!(
            actions,
            vec![
                Action::SetMute {
                    node: 48,
                    muted: true
                },
                Action::DestroyNode { node: 60 },
                Action::DestroyNode { node: 61 },
            ]
        );
        // Only the node this tracker muted is tracked: 49 was
        // already muted (by the user, say) and stays untracked.
        assert_eq!(tracker.tracked(), &BTreeSet::from([48]));
        // A steady scan plans nothing.
        assert!(tracker.plan(&scan(&[48, 49], &[], &[48, 49])).is_empty());
    }

    #[test]
    fn plan_remutes_a_bypass_attempt() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.plan(&scan(&[50], &[], &[])).len(), 1);
        // The node reads muted: steady.
        assert!(tracker.plan(&scan(&[50], &[], &[50])).is_empty());
        // Something unmuted it behind our back: mute it again.
        assert_eq!(
            tracker.plan(&scan(&[50], &[], &[])),
            vec![Action::SetMute {
                node: 50,
                muted: true
            }]
        );
    }

    #[test]
    fn plan_restore_unmutes_only_tracked_nodes() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.plan(&scan(&[48], &[], &[])).len(), 1);
        assert!(tracker.plan(&scan(&[48], &[], &[48])).is_empty());
        // Policy lifted: the tracked node is still muted, so it is
        // unmuted; a node muted by hand (49) is not ours to touch.
        assert_eq!(
            tracker.plan(&scan(&[], &[], &[48, 49])),
            vec![Action::SetMute {
                node: 48,
                muted: false
            }]
        );
        assert!(tracker.tracked().is_empty());
        assert!(tracker.plan(&scan(&[], &[], &[49])).is_empty());
    }

    #[test]
    fn plan_forgets_vanished_nodes_silently() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.plan(&scan(&[48], &[], &[])).len(), 1);
        // The node left the graph (neither required nor muted):
        // no unmute for a node that is gone.
        assert!(tracker.plan(&scan(&[], &[], &[])).is_empty());
        assert!(tracker.tracked().is_empty());
    }

    /// Live enforcer against a real headless PipeWire + WirePlumber:
    /// virtual sink/source devices, real GStreamer app streams, and
    /// the real `wpctl`/`pw-cli` executors. Each run owns its
    /// runtime dir, bus, and daemons, so parallel tests never share
    /// audio state. Skips (loudly) when any daemon or tool is
    /// missing.
    mod live_pw {
        use std::path::{Path, PathBuf};
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        use super::{run_blocking, Action, Tracker};
        use crate::audio_state::AudioState;
        use tuna_shell_host::settings::PrivacyPolicy;

        /// Daemons and tools this harness owns for one run.
        struct Harness {
            _dir: tempfile::TempDir,
            runtime: PathBuf,
            address: String,
            daemons: Vec<Child>,
            gst_children: Vec<Child>,
        }

        impl Harness {
            fn command(&self, tool: &str) -> Command {
                let mut command = Command::new(tool);
                command
                    .env("XDG_RUNTIME_DIR", &self.runtime)
                    .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                command
            }

            /// Current graph as parsed state (the enforcement input).
            fn dump(&self) -> Option<AudioState> {
                let output = self.command("pw-dump").output().ok()?;
                if !output.status.success() {
                    return None;
                }
                let mut state = AudioState::default();
                state.feed(&output.stdout).ok()?;
                Some(state)
            }

            /// Current graph as raw objects (the assertion oracle,
            /// independent of the scan under test).
            fn objects(&self) -> Option<Vec<serde_json::Value>> {
                let output = self.command("pw-dump").output().ok()?;
                if !output.status.success() {
                    return None;
                }
                serde_json::from_slice(&output.stdout).ok()
            }

            /// Mute flag of the node named `name`, when present with
            /// mixer state.
            fn muted(&self, name: &str) -> Option<bool> {
                self.objects()?.iter().find_map(|object| {
                    let props = &object["info"]["props"];
                    if props["node.name"].as_str() != Some(name) {
                        return None;
                    }
                    object["info"]["params"]["Props"]
                        .as_array()?
                        .first()
                        .and_then(|props| props["mute"].as_bool())
                })
            }

            /// Ids of nodes with this media class (and optional name).
            fn nodes(&self, class: &str, name: Option<&str>) -> Vec<u64> {
                self.objects()
                    .unwrap_or_default()
                    .iter()
                    .filter(|object| {
                        object["info"]["props"]["media.class"].as_str() == Some(class)
                            && name.is_none_or(|name| {
                                object["info"]["props"]["node.name"].as_str() == Some(name)
                            })
                    })
                    .filter_map(|object| object["id"].as_u64())
                    .collect()
            }

            /// Drive the tracker to a steady state under `policy`:
            /// dump, scan, plan, execute, until a round plans
            /// nothing. Returns the actions executed.
            fn settle(&self, tracker: &mut Tracker, policy: &PrivacyPolicy) -> Vec<Action> {
                let mut executed = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(30);
                while let Some(state) = self.dump() {
                    let actions = tracker.plan(&state.privacy_scan(policy));
                    if actions.is_empty() || Instant::now() >= deadline {
                        break;
                    }
                    for action in &actions {
                        if run_blocking(action, &self.runtime, &self.address) {
                            executed.push(*action);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                executed
            }

            /// Wait up to `timeout` for `condition` on the live
            /// graph (polls `pw-dump`, never sleeps blind).
            fn wait_for(&self, timeout: Duration, condition: impl Fn() -> bool) -> bool {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
                    if condition() {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                condition()
            }

            /// Start a GStreamer pipeline kept alive for the run.
            fn gst(&mut self, args: &[&str]) -> bool {
                match self
                    .command("gst-launch-1.0")
                    .args(args)
                    .env("GST_DEBUG", "0")
                    .stdout(Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        self.gst_children.push(child);
                        true
                    }
                    Err(_) => false,
                }
            }

            fn kill_all(children: &mut Vec<Child>) {
                for child in children.iter_mut() {
                    let _ = child.kill();
                }
                for child in children.iter_mut() {
                    let _ = child.wait();
                }
                children.clear();
            }
        }

        impl Drop for Harness {
            fn drop(&mut self) {
                Harness::kill_all(&mut self.gst_children);
                Harness::kill_all(&mut self.daemons);
            }
        }

        /// True when `tool` spawns (exit status ignored: presence,
        /// not behavior).
        fn tool_present(tool: &str) -> bool {
            Command::new(tool)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        }

        /// PipeWire configuration: the installed default plus two
        /// virtual null devices, so the harness never depends on
        /// hardware. `None` when no default config is installed.
        fn virtual_config(dir: &Path) -> Option<PathBuf> {
            let system = std::fs::read_to_string("/usr/share/pipewire/pipewire.conf").ok()?;
            let objects = r#"context.objects = [
    { factory = adapter
      args = {
          factory.name     = support.null-audio-sink
          node.name        = "virt-sink"
          node.description = "Virtual Sink"
          media.class      = "Audio/Sink"
          audio.channels   = 2
          audio.position   = [ FL FR ]
      }
    }
    { factory = adapter
      args = {
          factory.name     = support.null-audio-sink
          node.name        = "virt-source"
          node.description = "Virtual Source"
          media.class      = "Audio/Source"
          audio.channels   = 1
          audio.position   = [ MONO ]
      }
    }
]
"#;
            let mut lines: Vec<&str> = system.lines().collect();
            let start = lines
                .iter()
                .position(|line| line.starts_with("context.objects"))?;
            let end = lines
                .iter()
                .enumerate()
                .skip(start)
                .find(|(_, line)| line.trim() == "]")
                .map(|(index, _)| index)?;
            lines.splice(start..=end, objects.lines());
            let path = dir.join("pipewire.conf");
            std::fs::write(&path, lines.join("\n")).ok()?;
            Some(path)
        }

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, Child)> {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::BufRead;
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        /// Spawn the harness daemons: private bus, headless
        /// PipeWire with virtual devices, headless WirePlumber.
        /// `None` sketches which leg is missing (the caller skips).
        fn harness() -> Option<Harness> {
            for tool in [
                "dbus-daemon",
                "pipewire",
                "wireplumber",
                "wpctl",
                "pw-dump",
                "pw-cli",
                "gst-launch-1.0",
            ] {
                if !tool_present(tool) {
                    eprintln!("SKIP live privacy test: {tool} not on PATH");
                    return None;
                }
            }
            let dir = tempfile::tempdir().expect("privacy harness dir");
            let runtime = dir.path().join("run");
            std::fs::create_dir_all(&runtime).expect("harness runtime dir");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
                    .expect("harness runtime dir mode");
            }
            let Some(config) = virtual_config(dir.path()) else {
                eprintln!("SKIP live privacy test: no /usr/share/pipewire/pipewire.conf");
                return None;
            };
            let Some((address, daemon)) = private_bus() else {
                eprintln!("SKIP live privacy test: no usable dbus-daemon on PATH");
                return None;
            };
            let mut children = vec![daemon];
            let spawn = |children: &mut Vec<Child>,
                         tool: &str,
                         args: &[&str],
                         address: &str,
                         runtime: &Path| {
                Command::new(tool)
                    .args(args)
                    .env("XDG_RUNTIME_DIR", runtime)
                    .env("DBUS_SESSION_BUS_ADDRESS", address)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()
                    .map(|child| children.push(child))
                    .is_some()
            };
            if !spawn(
                &mut children,
                "pipewire",
                &["-c", &config.to_string_lossy()],
                &address,
                &runtime,
            ) {
                Harness::kill_all(&mut children);
                eprintln!("SKIP live privacy test: pipewire would not start");
                return None;
            }
            if !spawn(&mut children, "wireplumber", &[], &address, &runtime) {
                Harness::kill_all(&mut children);
                eprintln!("SKIP live privacy test: wireplumber would not start");
                return None;
            }
            let harness = Harness {
                _dir: dir,
                runtime,
                address,
                daemons: children,
                gst_children: Vec::new(),
            };
            if !harness.wait_for(Duration::from_secs(30), || {
                !harness.nodes("Audio/Sink", Some("virt-sink")).is_empty()
                    && !harness
                        .nodes("Audio/Source", Some("virt-source"))
                        .is_empty()
            }) {
                eprintln!("SKIP live privacy test: virtual devices never appeared");
                return None;
            }
            Some(harness)
        }

        #[test]
        fn live_enforcer_mutes_bypass_and_restores_devices() {
            let Some(mut harness) = harness() else {
                return;
            };
            let mut tracker = Tracker::default();
            let allowed = PrivacyPolicy::default();

            // Baseline: an allowed policy plans nothing.
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "allowed policy enforces nothing"
            );
            assert_eq!(harness.muted("virt-sink"), Some(false));
            assert_eq!(harness.muted("virt-source"), Some(false));

            // Real app streams, opened before the policy lands.
            assert!(harness.gst(&[
                "audiotestsrc",
                "wave=silence",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Stream/Output/Audio",
            ]));
            assert!(harness.gst(&[
                "pipewiresrc",
                "stream-properties=props,media.class=Stream/Input/Audio",
                "!",
                "fakesink",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness.nodes("Stream/Output/Audio", None).is_empty()
                    && !harness.nodes("Stream/Input/Audio", None).is_empty()
            }));

            // Disable microphone and sound output: existing streams
            // keep their nodes, but both devices read muted —
            // capture and playback go silent.
            let denied = PrivacyPolicy {
                microphone_disabled: true,
                sound_output_disabled: true,
                ..PrivacyPolicy::default()
            };
            let actions = harness.settle(&mut tracker, &denied);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, Action::SetMute { muted: true, .. })),
                "disabling mutes devices, got {actions:?}"
            );
            assert_eq!(harness.muted("virt-sink"), Some(true));
            assert_eq!(harness.muted("virt-source"), Some(true));
            assert!(
                !harness.nodes("Stream/Output/Audio", None).is_empty(),
                "playback streams are silenced, not destroyed"
            );
            assert!(
                !harness.nodes("Stream/Input/Audio", None).is_empty(),
                "capture streams are silenced, not destroyed"
            );

            // Negative control: a direct unmute behind the
            // enforcer's back is re-muted, not honored.
            let source = harness.nodes("Audio/Source", Some("virt-source"))[0].to_string();
            let unmuted = harness
                .command("wpctl")
                .args(["set-mute", &source, "0"])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            assert!(unmuted, "bypass attempt runs");
            harness.settle(&mut tracker, &denied);
            assert_eq!(
                harness.muted("virt-source"),
                Some(true),
                "unmute attempt while disabled is re-muted"
            );

            // A stream opened after the policy lands is silenced
            // too: the device it would use stays muted.
            assert!(harness.gst(&[
                "pipewiresrc",
                "stream-properties=props,media.class=Stream/Input/Audio",
                "!",
                "fakesink",
            ]));
            harness.settle(&mut tracker, &denied);
            assert_eq!(harness.muted("virt-source"), Some(true));

            // Explicit restore re-exposes the actual capability:
            // both devices unmute, and a fresh capture stream sees
            // an unmuted source.
            harness.settle(&mut tracker, &allowed);
            assert_eq!(harness.muted("virt-sink"), Some(false));
            assert_eq!(harness.muted("virt-source"), Some(false));
            assert!(harness.gst(&[
                "pipewiresrc",
                "stream-properties=props,media.class=Stream/Input/Audio",
                "!",
                "fakesink",
            ]));
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "restored policy enforces nothing"
            );
            assert_eq!(harness.muted("virt-source"), Some(false));
        }

        #[test]
        fn live_enforcer_destroys_cameras_existing_and_new() {
            let Some(mut harness) = harness() else {
                return;
            };
            let mut tracker = Tracker::default();
            let allowed = PrivacyPolicy::default();
            let camera_off = PrivacyPolicy {
                camera_disabled: true,
                ..PrivacyPolicy::default()
            };

            // A fake camera device (a real graph node published as
            // `Video/Source`) survives while allowed.
            assert!(harness.gst(&[
                "videotestsrc",
                "pattern=black",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Video/Source,node.name=fake-camera",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness
                    .nodes("Video/Source", Some("fake-camera"))
                    .is_empty()
            }));
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "allowed policy keeps the camera node"
            );
            assert!(
                !harness
                    .nodes("Video/Source", Some("fake-camera"))
                    .is_empty(),
                "camera node survives while allowed"
            );

            // Disable: the existing camera node is destroyed.
            let actions = harness.settle(&mut tracker, &camera_off);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, Action::DestroyNode { .. })),
                "disabling destroys camera nodes, got {actions:?}"
            );
            assert!(
                harness.wait_for(Duration::from_secs(10), || {
                    harness
                        .nodes("Video/Source", Some("fake-camera"))
                        .is_empty()
                }),
                "existing camera node is destroyed while disabled"
            );

            // A camera opened after the policy lands is destroyed
            // too: newly opened streams cannot bypass it.
            assert!(harness.gst(&[
                "videotestsrc",
                "pattern=black",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Video/Source,node.name=fake-camera-2",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness
                    .nodes("Video/Source", Some("fake-camera-2"))
                    .is_empty()
            }));
            harness.settle(&mut tracker, &camera_off);
            assert!(
                harness.wait_for(Duration::from_secs(10), || {
                    harness
                        .nodes("Video/Source", Some("fake-camera-2"))
                        .is_empty()
                }),
                "newly opened camera node is destroyed while disabled"
            );

            // Explicit restore: a fresh camera node survives.
            harness.settle(&mut tracker, &allowed);
            assert!(harness.gst(&[
                "videotestsrc",
                "pattern=black",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Video/Source,node.name=fake-camera-3",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness
                    .nodes("Video/Source", Some("fake-camera-3"))
                    .is_empty()
            }));
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "restored policy destroys nothing"
            );
            assert!(
                !harness
                    .nodes("Video/Source", Some("fake-camera-3"))
                    .is_empty(),
                "camera node survives after restore"
            );
        }
    }
}
