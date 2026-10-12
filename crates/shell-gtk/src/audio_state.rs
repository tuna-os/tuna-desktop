//! Bounded state from pw-dump's event stream. No GTK or daemon calls.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use tuna_shell_host::settings::PrivacyPolicy;

pub const MAX_PENDING: usize = 4 * 1024 * 1024;
const MAX_OBJECTS: usize = 4096;
const MAX_STATE: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub struct AudioState {
    objects: BTreeMap<u64, Value>,
    pending: Vec<u8>,
    state_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub sink: u64,
    pub device: Option<u64>,
    pub route: Option<(u64, u64)>,
    pub name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub recording: bool,
    pub source: Option<(f64, bool)>,
    pub sink: Option<(f64, bool)>,
    /// None until the default node reports a recognized playback state.
    pub sink_running: Option<bool>,
    pub outputs: Vec<Output>,
}

impl AudioState {
    /// pw-dump emits consecutive JSON arrays, split at arbitrary pipe reads.
    /// Reject malformed or oversized streams instead of growing indefinitely.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<bool, &'static str> {
        if self.pending.len().saturating_add(bytes.len()) > MAX_PENDING {
            return Err("audio event exceeds the buffer limit");
        }
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut batches = Vec::new();
        let mut stream = serde_json::Deserializer::from_slice(&self.pending).into_iter::<Value>();
        while let Some(value) = stream.next() {
            match value {
                Ok(Value::Array(batch)) => {
                    batches.push(batch);
                    consumed = stream.byte_offset();
                }
                Ok(_) => return Err("audio event is not an array"),
                Err(e) if e.is_eof() => break,
                Err(_) => return Err("invalid audio event JSON"),
            }
        }
        self.pending.drain(..consumed);
        let changed = !batches.is_empty();
        for batch in batches {
            for mut object in batch {
                let Some(id) = object["id"].as_u64() else {
                    continue;
                };
                if object.get("info").is_some_and(Value::is_null) {
                    if let Some(old) = self.objects.remove(&id) {
                        self.state_bytes = self.state_bytes.saturating_sub(old.to_string().len());
                    }
                    continue;
                }
                // Metadata dumps contain changed entries, unlike node/device
                // info, which contains the complete current cached state.
                if object["type"]
                    .as_str()
                    .is_some_and(|s| s.ends_with(":Metadata"))
                {
                    if let Some(old) = self.objects.get(&id) {
                        let mut entries = old["metadata"].as_array().cloned().unwrap_or_default();
                        for entry in object["metadata"].as_array().into_iter().flatten() {
                            entries.retain(|e| {
                                e["subject"] != entry["subject"] || e["key"] != entry["key"]
                            });
                            if !entry["value"].is_null() {
                                entries.push(entry.clone());
                            }
                        }
                        object["metadata"] = Value::Array(entries);
                    }
                }
                if self.objects.len() < MAX_OBJECTS || self.objects.contains_key(&id) {
                    let previous = self.objects.get(&id).map_or(0, |old| old.to_string().len());
                    let total = self
                        .state_bytes
                        .saturating_sub(previous)
                        .saturating_add(object.to_string().len());
                    if total > MAX_STATE {
                        return Err("audio graph exceeds the retained-state limit");
                    }
                    self.state_bytes = total;
                    self.objects.insert(id, object);
                }
            }
        }
        Ok(changed)
    }

    fn default_name(&self, key: &str) -> Option<String> {
        self.objects
            .values()
            .filter(|o| o["props"]["metadata.name"] == "default")
            .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
            .find(|e| e["subject"] == 0 && e["key"] == key)
            .and_then(|e| {
                let value = if let Some(s) = e["value"].as_str() {
                    serde_json::from_str::<Value>(s).ok()?
                } else {
                    e["value"].clone()
                };
                value["name"].as_str().map(str::to_owned)
            })
    }

    /// The owned feedback player must actually be running and linked to
    /// the current default sink; an alive player or a different output
    /// does not establish playback on this sink.
    pub fn default_sink_has_running_process(&self, pid: u32) -> bool {
        let Some(name) = self.default_name("default.audio.sink") else {
            return false;
        };
        let Some((&sink, _)) = self.objects.iter().find(|(_, object)| {
            object["info"]["props"]["media.class"] == "Audio/Sink"
                && object["info"]["props"]["node.name"] == name
        }) else {
            return false;
        };
        self.objects.iter().any(|(&stream, object)| {
            object["info"]["state"] == "running"
                && object["info"]["props"]["media.class"] == "Stream/Output/Audio"
                && number(&object["info"]["props"]["application.process.id"])
                    == Some(u64::from(pid))
                && self.objects.values().any(|link| {
                    number(&link["info"]["output-node-id"]) == Some(stream)
                        && number(&link["info"]["input-node-id"]) == Some(sink)
                        && link["info"]["state"] == "active"
                })
        })
    }

    /// Fold the current graph against the privacy kill-switches:
    /// which nodes must read muted, which camera nodes/streams must
    /// not exist, and which nodes are muted right now. A video
    /// capture stream counts as camera capture only while linked to
    /// a `Video/Source` node — an unlinked stream captures nothing,
    /// and a stream wired to anything else (a screen-share feed) is
    /// never the camera's business.
    pub fn privacy_scan(&self, policy: &PrivacyPolicy) -> crate::privacy::PrivacyScan {
        let mut scan = crate::privacy::PrivacyScan {
            audio_present: BTreeSet::new(),
            mute_required: BTreeSet::new(),
            destroy: Vec::new(),
            muted_now: BTreeSet::new(),
        };
        for (&id, object) in &self.objects {
            let props = &object["info"]["props"];
            let class = props["media.class"].as_str().unwrap_or("");
            let audio_required = if class == "Audio/Source" {
                Some(policy.microphone_disabled)
            } else if class == "Audio/Sink" {
                Some(policy.sound_output_disabled)
            } else {
                None
            };
            if let Some(required) = audio_required {
                scan.audio_present.insert(id);
                if node_muted(object) {
                    scan.muted_now.insert(id);
                }
                if required {
                    scan.mute_required.insert(id);
                }
            } else if policy.camera_disabled
                && (is_camera_node(class)
                    || (class == "Stream/Input/Video" && self.video_stream_captures_camera(id)))
            {
                scan.destroy.push(id);
            }
        }
        scan
    }

    /// A video capture stream captures the camera while a link feeds
    /// it from a `Video/Source` node (data flows output node to
    /// input node, as in
    /// [`AudioState::default_sink_has_running_process`]).
    fn video_stream_captures_camera(&self, stream: u64) -> bool {
        self.objects.values().any(|object| {
            let info = &object["info"];
            let output = number(&info["output-node-id"]);
            let input = number(&info["input-node-id"]);
            input == Some(stream)
                && output.is_some_and(|camera| {
                    self.objects.get(&camera).is_some_and(|node| {
                        is_camera_node(node["info"]["props"]["media.class"].as_str().unwrap_or(""))
                    })
                })
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let default_sink = self.default_name("default.audio.sink");
        let default_source = self.default_name("default.audio.source");
        let mut snapshot = Snapshot::default();
        for (&id, object) in &self.objects {
            let props = &object["info"]["props"];
            let class = props["media.class"].as_str().unwrap_or("");
            if class == "Stream/Input/Audio"
                && object["info"]["state"] == "running"
                && !property_bool(&props["stream.monitor"])
                && !property_bool(&props["node.passive"])
                && !["pavucontrol", "gnome-volume-control"].iter().any(|name| {
                    props["application.id"].as_str() == Some(name)
                        || props["application.name"].as_str() == Some("PulseAudio Volume Control")
                })
            {
                snapshot.recording = true;
            }
            let is_default_sink =
                props["node.name"].as_str() == default_sink.as_deref() && default_sink.is_some();
            if class == "Audio/Source"
                && default_source.is_some()
                && props["node.name"].as_str() == default_source.as_deref()
            {
                snapshot.source = node_level(object);
            }
            if class != "Audio/Sink" {
                continue;
            }
            if is_default_sink {
                snapshot.sink = node_level(object);
                snapshot.sink_running = match object["info"]["state"].as_str() {
                    Some("running") => Some(true),
                    Some("idle" | "suspended") => Some(false),
                    _ => None,
                };
            }
            let name = props["node.description"]
                .as_str()
                .or_else(|| props["node.nick"].as_str())
                .unwrap_or("Audio Output");
            let device_id = number(&props["device.id"]);
            let profile_device = number(&props["card.profile.device"]);
            let mut routes = Vec::new();
            if let (Some(device_id), Some(profile_device)) = (device_id, profile_device) {
                if let Some(device) = self.objects.get(&device_id) {
                    let params = &device["info"]["params"];
                    for route in params["EnumRoute"].as_array().into_iter().flatten() {
                        if route["direction"] != "Output"
                            || route["available"] == "no"
                            || !route["devices"].as_array().is_some_and(|ids| {
                                ids.iter().any(|v| number(v) == Some(profile_device))
                            })
                        {
                            continue;
                        }
                        let Some(index) = number(&route["index"]) else {
                            continue;
                        };
                        let selected = is_default_sink
                            && params["Route"].as_array().is_some_and(|r| {
                                r.iter().any(|r| {
                                    number(&r["index"]) == Some(index)
                                        && number(&r["device"]) == Some(profile_device)
                                })
                            });
                        let port = route["description"]
                            .as_str()
                            .or_else(|| route["name"].as_str())
                            .unwrap_or("Output");
                        routes.push(Output {
                            sink: id,
                            device: Some(device_id),
                            route: Some((index, profile_device)),
                            name: format!("{port} – {name}"),
                            selected,
                        });
                    }
                }
            }
            if routes.is_empty() {
                routes.push(Output {
                    sink: id,
                    device: None,
                    route: None,
                    name: name.to_owned(),
                    selected: is_default_sink,
                });
            }
            snapshot.outputs.extend(routes);
        }
        snapshot
    }
}

fn property_bool(v: &Value) -> bool {
    v.as_bool().unwrap_or(v.as_str() == Some("true"))
}
/// A camera device node (`Video/Source`, plus any namespaced kind a
/// provider may publish beneath it).
fn is_camera_node(class: &str) -> bool {
    class == "Video/Source" || class.starts_with("Video/Source/")
}
/// A node's mixer mute flag. Missing mixer state reads as unmuted,
/// so an unknown node is muted (and re-muted) rather than trusted.
fn node_muted(node: &Value) -> bool {
    node["info"]["params"]["Props"]
        .as_array()
        .and_then(|params| params.first())
        .and_then(|props| props["mute"].as_bool())
        .unwrap_or(false)
}
fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
fn node_level(node: &Value) -> Option<(f64, bool)> {
    let props = node["info"]["params"]["Props"].as_array()?.first()?;
    let volume = props["channelVolumes"]
        .as_array()
        .and_then(|channels| channels.first())
        .and_then(Value::as_f64)
        .or_else(|| props["volume"].as_f64())?;
    // WirePlumber's mixer API exposes the cubic (perceptual) scale.
    Some((
        volume.max(0.0).cbrt().mul_add(100.0, 0.0),
        props["mute"].as_bool().unwrap_or(false),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn amplified_backend_level_is_not_silently_truncated() {
        let node = json!({"info":{"params":{"Props":[{"channelVolumes":[1.728],"mute":false}]}}});
        let (percent, muted) = node_level(&node).unwrap();
        assert!((percent - 120.0).abs() < 0.000001);
        assert!(!muted);
    }

    fn feed(state: &mut AudioState, value: Value) {
        state.feed(value.to_string().as_bytes()).unwrap();
    }
    #[test]
    fn split_events_remove_capture_and_ignore_monitor_streams() {
        let mut s = AudioState::default();
        let event = br#"[{"id":70,"info":{"state":"running","props":{"media.class":"Stream/Input/Audio"}}}]"#;
        assert!(!s.feed(&event[..15]).unwrap());
        assert!(s.feed(&event[15..]).unwrap());
        assert!(s.snapshot().recording);
        feed(&mut s, json!([{"id":70,"info":null}]));
        assert!(!s.snapshot().recording);
        feed(
            &mut s,
            json!([{"id":70,"info":{"state":"running","props":{"media.class":"Stream/Input/Audio","stream.monitor":true}}}]),
        );
        assert!(!s.snapshot().recording);
        assert!(s.feed(b"garbage").is_err());
        assert!(AudioState::default()
            .feed(&vec![b' '; MAX_PENDING + 1])
            .is_err());
    }
    #[test]
    fn playback_state_is_default_sink_scoped_and_unknown_is_conservative() {
        let mut state = AudioState::default();
        feed(
            &mut state,
            json!([
                {"id":20,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"main"}}]},
                {"id":1,"info":{"state":"running","props":{"media.class":"Audio/Sink","node.name":"other"}}},
                {"id":2,"info":{"state":"idle","props":{"media.class":"Audio/Sink","node.name":"main"}}}
            ]),
        );
        assert_eq!(state.snapshot().sink_running, Some(false));
        feed(
            &mut state,
            json!([{"id":2,"info":{"state":"running","props":{"media.class":"Audio/Sink","node.name":"main"}}}]),
        );
        assert_eq!(state.snapshot().sink_running, Some(true));
        feed(
            &mut state,
            json!([{"id":2,"info":{"state":"error","props":{"media.class":"Audio/Sink","node.name":"main"}}}]),
        );
        assert_eq!(state.snapshot().sink_running, None);
        feed(&mut state, json!([{"id":2,"info":null}]));
        assert_eq!(state.snapshot().sink_running, None);
    }

    #[test]
    fn feedback_player_requires_running_stream_on_default_sink() {
        let mut state = AudioState::default();
        feed(
            &mut state,
            json!([
                {"id":20,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"main"}}]},
                {"id":2,"info":{"props":{"media.class":"Audio/Sink","node.name":"main"}}},
                {"id":3,"info":{"props":{"media.class":"Audio/Sink","node.name":"other"}}},
                {"id":4,"info":{"state":"running","props":{"media.class":"Stream/Output/Audio","application.process.id":"42"}}},
                {"id":5,"info":{"state":"active","output-node-id":4,"input-node-id":2}}
            ]),
        );
        assert!(state.default_sink_has_running_process(42));
        assert!(!state.default_sink_has_running_process(43));
        feed(
            &mut state,
            json!([{"id":5,"info":{"state":"active","output-node-id":4,"input-node-id":3}}]),
        );
        assert!(!state.default_sink_has_running_process(42));
        feed(
            &mut state,
            json!([{"id":5,"info":{"state":"paused","output-node-id":4,"input-node-id":2}}]),
        );
        assert!(!state.default_sink_has_running_process(42));
        feed(
            &mut state,
            json!([{"id":5,"info":{"state":"active","output-node-id":4,"input-node-id":2}},
            {"id":4,"info":{"state":"idle","props":{"media.class":"Stream/Output/Audio","application.process.id":42}}}]),
        );
        assert!(!state.default_sink_has_running_process(42));
        feed(&mut state, json!([{"id":4,"info":null}]));
        assert!(!state.default_sink_has_running_process(42));
    }

    #[test]
    fn ports_share_a_sink_but_switch_distinct_routes_and_metadata_is_incremental() {
        let mut s = AudioState::default();
        feed(
            &mut s,
            json!([
                {"id":20,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[
                    {"subject":0,"key":"default.audio.sink","value":{"name":"analog"}},
                    {"subject":0,"key":"default.audio.source","value":{"name":"mic"}}]},
                {"id":48,"info":{"props":{"media.class":"Audio/Sink","node.name":"analog","node.description":"Built-in Audio","device.id":"40","card.profile.device":0},"params":{"Props":[{"channelVolumes":[0.512,0.125],"mute":false}]}}},
                {"id":49,"info":{"props":{"media.class":"Audio/Source","node.name":"mic"},"params":{"Props":[{"channelVolumes":[0.125],"mute":true}]}}},
                {"id":40,"info":{"params":{"EnumRoute":[
                    {"index":1,"direction":"Output","description":"Speakers","available":"yes","devices":[0]},
                    {"index":2,"direction":"Output","description":"Headphones","available":"unknown","devices":[0]},
                    {"index":3,"direction":"Input","description":"Microphone","devices":[0]},
                    {"index":4,"direction":"Output","available":"no","devices":[0]}],"Route":[{"index":1,"device":0}]}}}
            ]),
        );
        let a = s.snapshot();
        assert_eq!(a.sink, Some((80.0, false)));
        assert_eq!(a.source, Some((50.0, true)));
        assert_eq!(a.outputs.len(), 2);
        assert!(a.outputs[0].selected);
        assert_eq!(a.outputs[1].route, Some((2, 0)));
        feed(
            &mut s,
            json!([{"id":20,"type":"PipeWire:Interface:Metadata","props":{"metadata.name":"default"},"metadata":[{"subject":0,"key":"default.audio.sink","value":{"name":"other"}}]}]),
        );
        assert_eq!(s.snapshot().source, a.source);
        assert!(s.snapshot().sink.is_none());
    }

    fn privacy_graph() -> AudioState {
        let mut s = AudioState::default();
        feed(
            &mut s,
            json!([
                {"id":48,"info":{"props":{"media.class":"Audio/Sink","node.name":"analog"},"params":{"Props":[{"channelVolumes":[0.512],"mute":false}]}}},
                {"id":49,"info":{"props":{"media.class":"Audio/Sink","node.name":"hdmi"},"params":{"Props":[{"channelVolumes":[0.512],"mute":true}]}}},
                {"id":50,"info":{"props":{"media.class":"Audio/Source","node.name":"mic"},"params":{"Props":[{"channelVolumes":[0.125],"mute":false}]}}},
                {"id":51,"info":{"props":{"media.class":"Audio/Source","node.name":"monitor"},"params":{"Props":[{"mute":true}]}}},
                {"id":60,"info":{"state":"running","props":{"media.class":"Video/Source","node.name":"camera"}}},
                {"id":61,"info":{"state":"running","props":{"media.class":"Stream/Input/Video","node.name":"capture"}}},
                {"id":62,"info":{"state":"running","props":{"media.class":"Stream/Input/Video","node.name":"share-view"}}},
                {"id":63,"info":{"state":"running","props":{"media.class":"Stream/Output/Video","node.name":"producer"}}},
                {"id":64,"info":{"props":{"media.class":"Video/Source/Custom","node.name":"doorbell"}}},
                {"id":70,"info":{"state":"active","output-node-id":60,"input-node-id":61}},
                {"id":71,"info":{"state":"active","output-node-id":63,"input-node-id":62}}
            ]),
        );
        s
    }

    #[test]
    fn privacy_scan_is_empty_when_everything_is_allowed() {
        let s = privacy_graph();
        let scan = s.privacy_scan(&PrivacyPolicy::default());
        assert!(scan.mute_required.is_empty());
        assert!(scan.destroy.is_empty());
        assert_eq!(scan.muted_now, BTreeSet::from([49, 51]));
    }

    #[test]
    fn privacy_scan_requires_mute_on_every_unmuted_device() {
        let s = privacy_graph();
        let scan = s.privacy_scan(&PrivacyPolicy {
            microphone_disabled: true,
            sound_output_disabled: true,
            ..PrivacyPolicy::default()
        });
        assert_eq!(scan.mute_required, BTreeSet::from([48, 49, 50, 51]));
        assert!(scan.destroy.is_empty());
    }

    #[test]
    fn privacy_scan_destroys_cameras_but_not_screen_shares() {
        let s = privacy_graph();
        let scan = s.privacy_scan(&PrivacyPolicy {
            camera_disabled: true,
            ..PrivacyPolicy::default()
        });
        assert!(scan.mute_required.is_empty());
        // Both camera flavors plus the stream fed by the camera; the
        // stream fed by the video producer and the producer itself
        // are not camera capture.
        assert_eq!(scan.destroy, vec![60, 61, 64]);
    }

    #[test]
    fn privacy_scan_ignores_an_unlinked_capture_stream() {
        let mut s = AudioState::default();
        feed(
            &mut s,
            json!([
                {"id":61,"info":{"state":"running","props":{"media.class":"Stream/Input/Video","node.name":"capture"}}}
            ]),
        );
        let scan = s.privacy_scan(&PrivacyPolicy {
            camera_disabled: true,
            ..PrivacyPolicy::default()
        });
        assert!(scan.destroy.is_empty());
        // Linking it to a camera later makes it camera capture.
        feed(
            &mut s,
            json!([
                {"id":60,"info":{"state":"running","props":{"media.class":"Video/Source","node.name":"camera"}}},
                {"id":70,"info":{"state":"active","output-node-id":60,"input-node-id":61}}
            ]),
        );
        let scan = s.privacy_scan(&PrivacyPolicy {
            camera_disabled: true,
            ..PrivacyPolicy::default()
        });
        assert_eq!(scan.destroy, vec![60, 61]);
    }

    #[test]
    fn privacy_scan_treats_missing_mixer_state_as_unmuted() {
        let mut s = AudioState::default();
        feed(
            &mut s,
            json!([
                {"id":50,"info":{"props":{"media.class":"Audio/Source","node.name":"mic"}}}
            ]),
        );
        let scan = s.privacy_scan(&PrivacyPolicy {
            microphone_disabled: true,
            ..PrivacyPolicy::default()
        });
        assert_eq!(scan.mute_required, BTreeSet::from([50]));
        assert!(scan.muted_now.is_empty());
    }
}
