//! Input-source names as GNOME reads them from xkeyboard-config
//! (`GnomeXkbInfo`), for keyboards' language menus and input-source
//! indicators (the shell's and the login screen's).

use std::collections::HashMap;

/// Names for the switcher, as `GnomeXkbInfo.get_layout_info` reads them
/// from xkeyboard-config's rules (`evdev.xml` and `evdev.extras.xml`):
/// the description, and the short description (a variant without one
/// takes its layout's).
#[derive(Debug, Default)]
pub struct XkbNames {
    names: HashMap<String, (String, String)>,
}

impl XkbNames {
    pub fn system() -> Self {
        let mut names = Self::default();
        for file in ["evdev.xml", "evdev.extras.xml"] {
            let path = std::path::Path::new("/usr/share/X11/xkb/rules").join(file);
            if let Ok(text) = std::fs::read_to_string(path) {
                names.add_rules(&text);
            }
        }
        names
    }

    pub fn add_rules(&mut self, text: &str) {
        // xkeyboard-config's registry starts with
        // `<!DOCTYPE xkbConfigRegistry SYSTEM "xkb.dtd">`; it is a system
        // file and declares no entities.
        let Ok(doc) = roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..roxmltree::ParsingOptions::default()
            },
        ) else {
            return;
        };
        let item = |node: roxmltree::Node| {
            let config = node.children().find(|c| c.has_tag_name("configItem"))?;
            let field = |tag: &str| {
                config
                    .children()
                    .find(|c| c.has_tag_name(tag))
                    .and_then(|c| c.text())
                    .map(|t| t.trim().to_owned())
            };
            Some((
                field("name")?,
                field("shortDescription"),
                field("description"),
            ))
        };
        for layout in doc.descendants().filter(|n| n.has_tag_name("layout")) {
            let Some((name, short, description)) = item(layout) else {
                continue;
            };
            let short = short.unwrap_or_else(|| name.clone());
            let description = description.unwrap_or_else(|| name.clone());
            for variant in layout.descendants().filter(|n| n.has_tag_name("variant")) {
                let Some((vname, vshort, vdescription)) = item(variant) else {
                    continue;
                };
                self.names.entry(format!("{name}+{vname}")).or_insert((
                    vdescription.unwrap_or_else(|| format!("{name}+{vname}")),
                    vshort.unwrap_or_else(|| short.clone()),
                ));
            }
            self.names.entry(name).or_insert((description, short));
        }
    }

    /// `(display name, short name)` for an XKB source id.
    pub fn lookup(&self, id: &str) -> (String, String) {
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| (id.to_owned(), id.split('+').next().unwrap_or(id).to_owned()))
    }
}

/// Short names made unique as `_inputSourcesChanged` does: a name two
/// sources share gets a subscript digit per source (`de₁`, `de₂`).
pub fn unique_short_names(shorts: &[String]) -> Vec<String> {
    shorts
        .iter()
        .enumerate()
        .map(|(i, short)| {
            let same: Vec<usize> = shorts
                .iter()
                .enumerate()
                .filter(|(_, s)| *s == short)
                .map(|(j, _)| j)
                .collect();
            if same.len() > 1 {
                let sub = same.iter().position(|j| *j == i).unwrap_or(0) + 1;
                format!(
                    "{short}{}",
                    char::from_u32(0x2080 + sub as u32).unwrap_or('?')
                )
            } else {
                short.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_xkeyboard_config_rules() {
        let mut names = XkbNames::default();
        names.add_rules(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE xkbConfigRegistry SYSTEM "xkb.dtd">
            <xkbConfigRegistry><layoutList>
            <layout><configItem><name>de</name><shortDescription>de</shortDescription>
            <description>German</description></configItem>
            <variantList><variant><configItem><name>nodeadkeys</name>
            <description>German (no dead keys)</description></configItem></variant></variantList>
            </layout></layoutList></xkbConfigRegistry>"#,
        );
        assert_eq!(names.lookup("de"), ("German".into(), "de".into()));
        assert_eq!(
            names.lookup("de+nodeadkeys"),
            ("German (no dead keys)".into(), "de".into())
        );
        assert_eq!(names.lookup("zz+q"), ("zz+q".into(), "zz".into()));
        assert_eq!(
            unique_short_names(&["de".into(), "us".into(), "de".into()]),
            vec!["de₁".to_owned(), "us".into(), "de₂".into()]
        );
    }
}
