//! GNOME's display configuration (`~/.config/monitors.xml`, #59, #340).
//!
//! Mutter keeps one configuration per set of connected monitors: its
//! logical monitors (position, scale, rotation, primary, and the
//! monitors showing each one with the mode they run) plus the monitors
//! switched off. Tuna Desktop reads the configuration matching the
//! connectors that are lit, so a laptop set to 150 percent in GNOME
//! opens Tuna Desktop at 150 percent too, and writes what GNOME
//! Settings keeps in Mutter 51's exact format
//! (`meta-monitor-config-store.c`). Logical layout mode (GNOME's
//! default) is assumed: positions are logical pixels.

use std::path::PathBuf;

use crate::display_config::ModeSpec;

/// Mutter's `MetaMonitorSpec`: the connector and the monitor's
/// vendor, product and serial.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct MonitorSpec {
    /// Connector name as the kernel names it (`eDP-1`, `HDMI-A-1`).
    pub connector: String,
    pub vendor: String,
    pub product: String,
    pub serial: String,
}

impl MonitorSpec {
    /// A spec known only by its connector.
    pub fn connector(connector: &str) -> Self {
        Self {
            connector: connector.to_owned(),
            ..Self::default()
        }
    }
}

/// One monitor of a logical monitor and the mode it runs (`None` when
/// the file names no mode: the monitor keeps its current one).
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorConfig {
    pub spec: MonitorSpec,
    pub mode: Option<ModeSpec>,
    pub color_mode: crate::display_config::ColorMode,
}

/// One logical monitor; several monitors mirror it.
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalMonitorConfig {
    /// Logical position.
    pub x: i32,
    pub y: i32,
    /// Output scale (1, 1.25, 1.5, 2, …).
    pub scale: f64,
    /// Mutter DisplayConfig transform (0-7) from the `<transform>` block
    /// (#345): 0 normal, 1 left, 2 upside-down, 3 right, 4-7 flipped.
    /// Absent blocks stay 0, as mutter writes normal without a block.
    pub transform: u32,
    /// GNOME's primary monitor (top bar).
    pub primary: bool,
    pub monitors: Vec<MonitorConfig>,
}

/// One configuration: what each monitor of one monitor set shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MonitorsConfig {
    pub logical: Vec<LogicalMonitorConfig>,
    /// Monitors switched off (`<disabled>`).
    pub disabled: Vec<MonitorSpec>,
}

impl MonitorsConfig {
    /// The connectors this configuration covers, sorted: the key it is
    /// stored under (Mutter keys on the full monitor specs; Tuna Desktop
    /// cannot read EDIDs, so the connector stands for the monitor).
    pub fn connectors(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .logical
            .iter()
            .flat_map(|l| l.monitors.iter().map(|m| m.spec.connector.as_str()))
            .chain(self.disabled.iter().map(|s| s.connector.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The logical monitor showing this connector, and its monitor.
    pub fn find(&self, connector: &str) -> Option<(&LogicalMonitorConfig, &MonitorConfig)> {
        self.logical.iter().find_map(|l| {
            l.monitors
                .iter()
                .find(|m| m.spec.connector == connector)
                .map(|m| (l, m))
        })
    }
}

/// Mutter 51.0 rotation names (`meta-monitor-config-store.c`):
/// `normal`, `left`, `upside_down`, `right`, plus a `flipped` flag.
fn parse_transform(logical: roxmltree::Node<'_, '_>) -> u32 {
    let Some(block) = logical.children().find(|c| c.has_tag_name("transform")) else {
        return 0;
    };
    let text = |name: &str| {
        block
            .children()
            .find(|c| c.has_tag_name(name))
            .and_then(|c| c.text())
            .map(str::trim)
            .unwrap_or_default()
    };
    let base = match text("rotation") {
        "left" => 1,
        // Mutter writes `upside_down`; accept the hyphenated twin too.
        "upside_down" | "upside-down" => 2,
        "right" => 3,
        _ => 0,
    };
    // Unknown rotations fall back to normal rather than failing the
    // whole arrangement; flipped composes as transforms 4-7.
    let flipped = text("flipped") == "yes";
    if flipped {
        base + 4
    } else {
        base
    }
}

/// A `<transform>` block in mutter's exact format (normal omits it).
fn transform_xml(transform: u32) -> String {
    if transform == 0 {
        return String::new();
    }
    let (rotation, flipped) = match transform {
        1 | 5 => ("left", transform >= 4),
        2 | 6 => ("upside_down", transform >= 4),
        3 | 7 => ("right", transform >= 4),
        4 => ("normal", true),
        _ => return String::new(),
    };
    format!(
        "      <transform>\n        <rotation>{}</rotation>\n        <flipped>{}</flipped>\n      </transform>\n",
        rotation,
        if flipped { "yes" } else { "no" },
    )
}

fn child_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.children()
        .find(|c| c.has_tag_name(name))
        .and_then(|c| c.text())
        .map(|t| t.trim().to_owned())
}

fn parse_spec(spec: roxmltree::Node<'_, '_>) -> Option<MonitorSpec> {
    Some(MonitorSpec {
        connector: child_text(spec, "connector").filter(|c| !c.is_empty())?,
        vendor: child_text(spec, "vendor").unwrap_or_default(),
        product: child_text(spec, "product").unwrap_or_default(),
        serial: child_text(spec, "serial").unwrap_or_default(),
    })
}

/// A `<mode>` block: width, height, rate and the interlace flag.
fn parse_mode(mode: roxmltree::Node<'_, '_>) -> Option<ModeSpec> {
    let width: i32 = child_text(mode, "width")?.parse().ok()?;
    let height: i32 = child_text(mode, "height")?.parse().ok()?;
    let refresh: f64 = child_text(mode, "rate")?.parse().ok()?;
    let interlaced = mode
        .children()
        .any(|c| c.has_tag_name("flag") && c.text().map(str::trim) == Some("interlace"));
    (width > 0 && height > 0 && refresh.is_finite() && refresh > 0.0).then_some(ModeSpec {
        width,
        height,
        refresh: refresh as f32,
        interlaced,
    })
}

/// Every configuration in a monitors.xml.
pub fn parse(xml: &str) -> Vec<MonitorsConfig> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    doc.root_element()
        .children()
        .filter(|n| n.has_tag_name("configuration"))
        .filter_map(|configuration| {
            // Mutter rejects an unknown colormode, rather than silently
            // dropping that monitor and applying the rest of the layout.
            if configuration
                .descendants()
                .filter(|node| node.has_tag_name("colormode"))
                .any(|node| {
                    crate::display_config::ColorMode::from_xml(node.text().unwrap_or("").trim())
                        .is_none()
                })
            {
                return None;
            }
            let logical = configuration
                .children()
                .filter(|n| n.has_tag_name("logicalmonitor"))
                .filter_map(|logical| {
                    let scale = child_text(logical, "scale")
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|s| s.is_finite() && *s > 0.0)
                        .unwrap_or(1.0);
                    let monitors: Vec<MonitorConfig> = logical
                        .children()
                        .filter(|n| n.has_tag_name("monitor"))
                        .filter_map(|monitor| {
                            let spec = monitor
                                .children()
                                .find(|c| c.has_tag_name("monitorspec"))
                                .and_then(parse_spec)?;
                            let mode = monitor
                                .children()
                                .find(|c| c.has_tag_name("mode"))
                                .and_then(parse_mode);
                            let color_mode = match child_text(monitor, "colormode") {
                                None => crate::display_config::ColorMode::Default,
                                Some(value) => crate::display_config::ColorMode::from_xml(&value)?,
                            };
                            Some(MonitorConfig {
                                spec,
                                mode,
                                color_mode,
                            })
                        })
                        .collect();
                    (!monitors.is_empty()).then(|| LogicalMonitorConfig {
                        x: child_text(logical, "x")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0),
                        y: child_text(logical, "y")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0),
                        scale,
                        transform: parse_transform(logical),
                        primary: child_text(logical, "primary").is_some_and(|p| p == "yes"),
                        monitors,
                    })
                })
                .collect();
            let disabled = configuration
                .children()
                .filter(|n| n.has_tag_name("disabled"))
                .flat_map(|d| d.children().filter(|c| c.has_tag_name("monitorspec")))
                .filter_map(parse_spec)
                .collect();
            Some(MonitorsConfig { logical, disabled })
        })
        .filter(|c| !c.logical.is_empty())
        .collect()
}

/// The configuration for exactly these connectors, as GNOME picks it;
/// else the first one that covers them all.
pub fn choose(configurations: &[MonitorsConfig], connected: &[String]) -> Option<MonitorsConfig> {
    let mut wanted: Vec<&str> = connected.iter().map(String::as_str).collect();
    wanted.sort_unstable();
    wanted.dedup();
    configurations
        .iter()
        .find(|c| c.connectors() == wanted)
        .or_else(|| {
            configurations
                .iter()
                .find(|c| wanted.iter().all(|w| c.connectors().contains(w)))
        })
        .cloned()
}

/// `$XDG_CONFIG_HOME/monitors.xml` (default `~/.config/monitors.xml`).
pub fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|dir| dir.join("monitors.xml"))
}

/// Tuna Desktop's own saved configurations
/// (`$XDG_CONFIG_HOME/tuna/monitors.xml`): what GNOME Settings kept in a
/// Tuna Desktop session, in Mutter's format. Kept apart from GNOME's
/// file because Tuna Desktop's monitor specs carry no EDID vendor,
/// product or serial, so a GNOME session's configurations stay intact.
pub fn tuna_path() -> Option<PathBuf> {
    path().and_then(|p| p.parent().map(|dir| dir.join("tuna").join("monitors.xml")))
}

/// The user's configuration for these connectors: Tuna Desktop's own
/// saved one first, then GNOME's.
pub fn load(connected: &[String]) -> Option<MonitorsConfig> {
    [tuna_path(), path()]
        .into_iter()
        .flatten()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .find_map(|xml| choose(&parse(&xml), connected))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Mutter's `append_monitor_spec`.
fn spec_xml(spec: &MonitorSpec, indent: &str) -> String {
    format!(
        "{indent}<monitorspec>\n{indent}  <connector>{}</connector>\n{indent}  <vendor>{}</vendor>\n{indent}  <product>{}</product>\n{indent}  <serial>{}</serial>\n{indent}</monitorspec>\n",
        escape(&spec.connector),
        escape(&spec.vendor),
        escape(&spec.product),
        escape(&spec.serial),
    )
}

/// Every configuration as a monitors.xml document, in the order and
/// layout of Mutter's `generate_config_xml`.
pub fn to_xml(configs: &[MonitorsConfig]) -> String {
    let mut out = String::from("<monitors version=\"2\">\n");
    for config in configs {
        out.push_str("  <configuration>\n    <layoutmode>logical</layoutmode>\n");
        for logical in &config.logical {
            out.push_str(&format!(
                "    <logicalmonitor>\n      <x>{}</x>\n      <y>{}</y>\n      <scale>{}</scale>\n",
                logical.x, logical.y, logical.scale
            ));
            if logical.primary {
                out.push_str("      <primary>yes</primary>\n");
            }
            out.push_str(&transform_xml(logical.transform));
            for monitor in &logical.monitors {
                out.push_str("      <monitor>\n");
                out.push_str(&spec_xml(&monitor.spec, "        "));
                if let Some(mode) = monitor.mode {
                    out.push_str(&format!(
                        "        <mode>\n          <width>{}</width>\n          <height>{}</height>\n          <rate>{:.3}</rate>\n{}        </mode>\n",
                        mode.width,
                        mode.height,
                        mode.refresh,
                        if mode.interlaced {
                            "          <flag>interlace</flag>\n"
                        } else {
                            ""
                        },
                    ));
                }
                if monitor.color_mode != crate::display_config::ColorMode::Default {
                    out.push_str(&format!(
                        "        <colormode>{}</colormode>\n",
                        monitor.color_mode.xml(),
                    ));
                }
                out.push_str("      </monitor>\n");
            }
            out.push_str("    </logicalmonitor>\n");
        }
        if !config.disabled.is_empty() {
            out.push_str("    <disabled>\n");
            for spec in &config.disabled {
                out.push_str(&spec_xml(spec, "      "));
            }
            out.push_str("    </disabled>\n");
        }
        out.push_str("  </configuration>\n");
    }
    out.push_str("</monitors>\n");
    out
}

/// Mutter's `meta_monitor_config_store_add`: the store with `config`
/// replacing the configuration for the same monitors, else added.
pub fn merge(store: &str, config: &MonitorsConfig) -> String {
    let mut configs = parse(store);
    let key = config.connectors();
    match configs.iter().position(|c| c.connectors() == key) {
        Some(index) => configs[index] = config.clone(),
        None => configs.push(config.clone()),
    }
    to_xml(&configs)
}

/// Save a configuration into Tuna Desktop's own monitors.xml, keeping
/// the other monitor sets' (atomic replace).
pub fn save(config: &MonitorsConfig) -> std::io::Result<PathBuf> {
    let file = tuna_path().ok_or_else(|| std::io::Error::other("no config dir"))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let store = std::fs::read_to_string(&file).unwrap_or_default();
    let tmp = file.with_extension("xml.tmp");
    std::fs::write(&tmp, merge(&store, config))?;
    std::fs::rename(&tmp, &file)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<monitors version="2">
  <configuration>
    <logicalmonitor>
      <x>0</x><y>0</y><scale>1.5</scale><primary>yes</primary>
      <monitor>
        <monitorspec><connector>eDP-1</connector><vendor>BOE</vendor><product>0x095f</product><serial>0x00000000</serial></monitorspec>
        <mode><width>2256</width><height>1504</height><rate>59.999</rate></mode>
      </monitor>
    </logicalmonitor>
  </configuration>
  <configuration>
    <logicalmonitor>
      <x>0</x><y>0</y><scale>1.5</scale><primary>yes</primary>
      <monitor><monitorspec><connector>eDP-1</connector></monitorspec></monitor>
    </logicalmonitor>
    <logicalmonitor>
      <x>1504</x><y>0</y><scale>1</scale>
      <monitor><monitorspec><connector>HDMI-A-1</connector></monitorspec></monitor>
    </logicalmonitor>
  </configuration>
</monitors>"#;

    fn spec(connector: &str) -> MonitorSpec {
        MonitorSpec {
            connector: connector.into(),
            vendor: "Tuna Desktop".into(),
            product: connector.into(),
            serial: connector.into(),
        }
    }

    fn mode(width: i32, height: i32, refresh: f32) -> Option<ModeSpec> {
        Some(ModeSpec {
            width,
            height,
            refresh,
            interlaced: false,
        })
    }

    fn logical(connector: &str, x: i32, scale: f64, primary: bool) -> LogicalMonitorConfig {
        LogicalMonitorConfig {
            x,
            y: 0,
            scale,
            transform: 0,
            primary,
            monitors: vec![MonitorConfig {
                spec: spec(connector),
                mode: mode(1920, 1080, 60.0),
                color_mode: crate::display_config::ColorMode::Default,
            }],
        }
    }

    #[test]
    fn parses_gnome_monitors_xml() {
        let configs = parse(XML);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].logical.len(), 1);
        let (laptop, monitor) = configs[0].find("eDP-1").unwrap();
        assert_eq!(laptop.scale, 1.5);
        assert!(laptop.primary);
        assert_eq!(monitor.spec.vendor, "BOE");
        assert_eq!(monitor.mode, mode(2256, 1504, 59.999));
        assert_eq!(monitor.mode.unwrap().id(), "2256x1504@59.999");
        let (hdmi, monitor) = configs[1].find("HDMI-A-1").unwrap();
        assert_eq!((hdmi.x, hdmi.scale), (1504, 1.0));
        // A file without modes keeps the current ones.
        assert_eq!(monitor.mode, None);
        assert!(parse("<not xml").is_empty());
    }

    #[test]
    fn saved_arrangements_read_back() {
        let configs = vec![MonitorsConfig {
            logical: vec![
                logical("eDP-1", 0, 1.25, true),
                logical("HDMI-A-1", 1536, 1.0, false),
            ],
            disabled: Vec::new(),
        }];
        let back = parse(&to_xml(&configs));
        assert_eq!(back, configs);
    }

    #[test]
    fn modes_mirrors_and_disabled_monitors_round_trip_in_mutter_format() {
        let mut mirrored = logical("eDP-1", 0, 1.0, true);
        mirrored.monitors.push(MonitorConfig {
            spec: spec("HDMI-A-1"),
            mode: Some(ModeSpec {
                width: 1920,
                height: 1080,
                refresh: 59.94,
                interlaced: true,
            }),
            color_mode: crate::display_config::ColorMode::Default,
        });
        let config = MonitorsConfig {
            logical: vec![mirrored],
            disabled: vec![spec("DP-1")],
        };
        let xml = to_xml(std::slice::from_ref(&config));
        // Mutter's layout, byte for byte.
        assert!(xml.starts_with(
            "<monitors version=\"2\">\n  <configuration>\n    <layoutmode>logical</layoutmode>\n    <logicalmonitor>\n      <x>0</x>\n      <y>0</y>\n      <scale>1</scale>\n      <primary>yes</primary>\n      <monitor>\n        <monitorspec>\n          <connector>eDP-1</connector>\n          <vendor>Tuna Desktop</vendor>\n          <product>eDP-1</product>\n          <serial>eDP-1</serial>\n        </monitorspec>\n        <mode>\n          <width>1920</width>\n          <height>1080</height>\n          <rate>60.000</rate>\n        </mode>\n      </monitor>\n"
        ), "{xml}");
        assert!(xml.contains("<rate>59.940</rate>\n          <flag>interlace</flag>\n"));
        assert!(xml.contains(
            "    <disabled>\n      <monitorspec>\n        <connector>DP-1</connector>\n"
        ));
        let back = parse(&xml);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].logical[0].monitors.len(), 2);
        assert_eq!(back[0].disabled, vec![spec("DP-1")]);
        assert_eq!(
            back[0].logical[0].monitors[1].mode.unwrap().id(),
            "1920x1080i@59.940"
        );
        assert_eq!(back[0].connectors(), ["DP-1", "HDMI-A-1", "eDP-1"]);
    }

    #[test]
    fn saving_replaces_only_the_same_monitor_set() {
        let laptop = MonitorsConfig {
            logical: vec![logical("eDP-1", 0, 1.25, true)],
            disabled: Vec::new(),
        };
        let docked = MonitorsConfig {
            logical: vec![logical("HDMI-A-1", 0, 1.0, true)],
            disabled: vec![spec("eDP-1")],
        };
        let store = merge("", &laptop);
        let store = merge(&store, &docked);
        assert_eq!(parse(&store), vec![laptop.clone(), docked.clone()]);
        let mut laptop_150 = laptop.clone();
        laptop_150.logical[0].scale = 1.5;
        let store = merge(&store, &laptop_150);
        assert_eq!(parse(&store), vec![laptop_150, docked]);
    }

    #[test]
    fn rotation_round_trips_in_gnome_format() {
        // Mutter's rotation names and flipped flag (#345).
        let xml = r#"<monitors version="2">
  <configuration>
    <logicalmonitor>
      <x>0</x><y>0</y><scale>1</scale><primary>yes</primary>
      <transform><rotation>left</rotation><flipped>no</flipped></transform>
      <monitor><monitorspec><connector>eDP-1</connector></monitorspec></monitor>
    </logicalmonitor>
    <logicalmonitor>
      <x>1080</x><y>0</y><scale>1</scale>
      <transform><rotation>right</rotation><flipped>yes</flipped></transform>
      <monitor><monitorspec><connector>HDMI-A-1</connector></monitorspec></monitor>
    </logicalmonitor>
    <logicalmonitor>
      <x>0</x><y>1920</y><scale>1</scale>
      <transform><rotation>upside_down</rotation><flipped>no</flipped></transform>
      <monitor><monitorspec><connector>DP-1</connector></monitorspec></monitor>
    </logicalmonitor>
  </configuration>
</monitors>"#;
        let configs = parse(xml);
        assert_eq!(configs.len(), 1);
        let transforms: Vec<(String, u32)> = configs[0]
            .logical
            .iter()
            .map(|l| (l.monitors[0].spec.connector.clone(), l.transform))
            .collect();
        assert_eq!(
            transforms,
            [
                ("eDP-1".to_owned(), 1),
                ("HDMI-A-1".to_owned(), 7),
                ("DP-1".to_owned(), 2),
            ]
        );
        // Rotation survives a save/load cycle; normal still omits the block.
        let back = parse(&to_xml(&configs));
        assert_eq!(back, configs);
        assert!(!to_xml(&[]).contains("transform"));
        let normal = MonitorsConfig {
            logical: vec![logical("eDP-1", 0, 1.0, true)],
            disabled: Vec::new(),
        };
        assert!(!to_xml(std::slice::from_ref(&normal)).contains("transform"));
        // Unknown rotations keep the arrangement readable at normal.
        let broken = xml.replace("<rotation>left</rotation>", "<rotation>diagonal</rotation>");
        assert_eq!(parse(&broken)[0].logical[0].transform, 0);
    }

    #[test]
    fn picks_the_arrangement_for_the_lit_connectors() {
        let configs = parse(XML);
        let laptop = choose(&configs, &["eDP-1".to_owned()]).unwrap();
        assert_eq!(laptop.logical.len(), 1);
        assert_eq!(laptop.find("eDP-1").unwrap().0.scale, 1.5);
        let docked = choose(&configs, &["HDMI-A-1".to_owned(), "eDP-1".to_owned()]).unwrap();
        assert_eq!(docked.find("HDMI-A-1").unwrap().0.x, 1504);
        assert!(choose(&configs, &["DP-3".to_owned()]).is_none());
        // A disabled monitor belongs to the configuration's monitor set.
        let lid_closed = MonitorsConfig {
            logical: vec![logical("HDMI-A-1", 0, 1.0, true)],
            disabled: vec![spec("eDP-1")],
        };
        let configs = vec![lid_closed.clone()];
        assert_eq!(
            choose(&configs, &["eDP-1".to_owned(), "HDMI-A-1".to_owned()]),
            Some(lid_closed)
        );
    }
    #[test]
    fn color_modes_round_trip_without_enabling_unspecified_hdr() {
        use crate::display_config::ColorMode;
        let mut hdr = MonitorsConfig {
            logical: vec![logical("DP-1", 0, 1.0, true)],
            disabled: vec![],
        };
        hdr.logical[0].monitors[0].color_mode = ColorMode::Bt2100;
        let xml = to_xml(std::slice::from_ref(&hdr));
        assert!(xml.contains("<colormode>bt2100</colormode>"));
        assert_eq!(parse(&xml), [hdr.clone()]);
        let merged = merge(XML, &hdr);
        let selected = choose(&parse(&merged), &["DP-1".to_owned()]).unwrap();
        assert_eq!(selected, hdr);
        assert_eq!(
            parse(XML)[0].logical[0].monitors[0].color_mode,
            ColorMode::Default
        );
        let invalid = xml.replace("bt2100", "unsupported-hdr");
        assert!(parse(&invalid).is_empty());
        hdr.logical[0].monitors[0].color_mode = ColorMode::Default;
        assert!(!to_xml(&[hdr]).contains("colormode"));
    }
}
