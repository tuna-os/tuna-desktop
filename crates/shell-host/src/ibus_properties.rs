//! Bounded IBus property values and context-scoped actions. No composition path.
//! Wire: ibus 1.5.32 ibusproperty.c/ibusproplist.c; menu semantics: Shell51 keyboard.js.
use gio::glib;
use std::collections::HashSet;

pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_NODES: usize = 128;
pub const MAX_DEPTH: usize = 8;
pub const MAX_TEXT: usize = 512;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Action,
    Toggle,
    Radio,
    Menu,
    Separator,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub kind: Kind,
    pub label: String,
    pub symbol: Option<String>,
    pub tooltip: String,
    pub sensitive: bool,
    pub visible: bool,
    pub state: u32,
    pub children: Vec<Property>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidProperty;
fn unbox(value: &glib::Variant) -> Result<glib::Variant, InvalidProperty> {
    let mut value = value.clone();
    for _ in 0..4 {
        if !value.is_type(glib::VariantTy::VARIANT) {
            return Ok(value);
        }
        value = value.as_variant().ok_or(InvalidProperty)?;
    }
    Err(InvalidProperty)
}
fn string(value: &glib::Variant) -> Result<String, InvalidProperty> {
    let text = value.str().ok_or(InvalidProperty)?;
    if text.len() > MAX_TEXT {
        return Err(InvalidProperty);
    }
    Ok(text.to_owned())
}
fn text(value: &glib::Variant) -> Result<String, InvalidProperty> {
    let value = unbox(value)?;
    if value.type_().as_str() != "(sa{sv}sv)" || value.child_value(0).str() != Some("IBusText") {
        return Err(InvalidProperty);
    }
    let attrs = unbox(&value.child_value(3))?;
    if attrs.type_().as_str() != "(sa{sv}av)"
        || attrs.child_value(0).str() != Some("IBusAttrList")
        || attrs.child_value(2).n_children() > MAX_NODES
    {
        return Err(InvalidProperty);
    }
    for i in 0..attrs.child_value(2).n_children() {
        let attr = unbox(&attrs.child_value(2).child_value(i))?;
        if attr.type_().as_str() != "(sa{sv}uuuu)"
            || attr.child_value(0).str() != Some("IBusAttribute")
        {
            return Err(InvalidProperty);
        }
    }
    string(&value.child_value(2))
}
struct Budget {
    remaining: usize,
    keys: HashSet<String>,
}
fn list(
    value: &glib::Variant,
    depth: usize,
    budget: &mut Budget,
) -> Result<Vec<Property>, InvalidProperty> {
    let value = unbox(value)?;
    if value.type_().as_str() != "(sa{sv}av)" || value.child_value(0).str() != Some("IBusPropList")
    {
        return Err(InvalidProperty);
    }
    let items = value.child_value(2);
    if items.n_children() > budget.remaining {
        return Err(InvalidProperty);
    }
    (0..items.n_children())
        .map(|i| property(&items.child_value(i), depth, budget))
        .collect()
}
fn property(
    value: &glib::Variant,
    depth: usize,
    budget: &mut Budget,
) -> Result<Property, InvalidProperty> {
    if depth >= MAX_DEPTH {
        return Err(InvalidProperty);
    }
    budget.remaining = budget.remaining.checked_sub(1).ok_or(InvalidProperty)?;
    let value = unbox(value)?;
    // Old libibus omitted the last symbol field; no permissive trailing fields.
    if !matches!(
        value.type_().as_str(),
        "(sa{sv}suvsvbbuvv)" | "(sa{sv}suvsvbbuv)"
    ) || value.child_value(0).str() != Some("IBusProperty")
    {
        return Err(InvalidProperty);
    }
    let kind = match value.child_value(3).get::<u32>().ok_or(InvalidProperty)? {
        0 => Kind::Action,
        1 => Kind::Toggle,
        2 => Kind::Radio,
        3 => Kind::Menu,
        4 => Kind::Separator,
        _ => return Err(InvalidProperty),
    };
    let key = string(&value.child_value(2))?;
    if (key.is_empty() && kind != Kind::Separator)
        || (!key.is_empty() && !budget.keys.insert(key.clone()))
    {
        return Err(InvalidProperty);
    }
    let state = value.child_value(9).get::<u32>().ok_or(InvalidProperty)?;
    if state > 2 {
        return Err(InvalidProperty);
    }
    // Icon is not loaded from an engine-supplied path. Still bound/validate it.
    let _icon = string(&value.child_value(5))?;
    let children = list(&value.child_value(10), depth + 1, budget)?;
    if kind != Kind::Menu && !children.is_empty() {
        return Err(InvalidProperty);
    }
    Ok(Property {
        key,
        kind,
        label: text(&value.child_value(4))?,
        symbol: (value.n_children() == 12)
            .then(|| text(&value.child_value(11)))
            .transpose()?,
        tooltip: text(&value.child_value(6))?,
        sensitive: value.child_value(7).get().ok_or(InvalidProperty)?,
        visible: value.child_value(8).get().ok_or(InvalidProperty)?,
        state,
        children,
    })
}
pub fn parse_list(value: &glib::Variant) -> Result<Vec<Property>, InvalidProperty> {
    if value.size() > MAX_BYTES {
        return Err(InvalidProperty);
    }
    list(
        value,
        0,
        &mut Budget {
            remaining: MAX_NODES,
            keys: HashSet::new(),
        },
    )
}
pub fn parse_property(value: &glib::Variant) -> Result<Property, InvalidProperty> {
    if value.size() > MAX_BYTES {
        return Err(InvalidProperty);
    }
    property(
        value,
        0,
        &mut Budget {
            remaining: MAX_NODES,
            keys: HashSet::new(),
        },
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub epoch: u64,
    pub key: String,
}
#[derive(Default, Debug)]
pub struct Properties {
    pub engine: Option<String>,
    pub context: Option<String>,
    pub items: Vec<Property>,
    epoch: u64,
    exhausted: bool,
}
impl Properties {
    fn invalidate(&mut self) {
        self.items.clear();
        match self.epoch.checked_add(1) {
            Some(epoch) => self.epoch = epoch,
            None => self.exhausted = true,
        }
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn reset(&mut self) {
        self.invalidate();
        self.engine = None;
        self.context = None;
    }
    pub fn engine(&mut self, engine: Option<String>) {
        if self.engine != engine {
            self.invalidate();
            self.engine = engine;
        }
    }
    pub fn initial_engine(&mut self, engine: String) {
        if self.engine.is_none() && !self.exhausted {
            self.engine = Some(engine);
            // No callback from an engine-less graph could act. Still invalidate
            // captured widgets before publishing the actionable graph.
            match self.epoch.checked_add(1) {
                Some(epoch) => self.epoch = epoch,
                None => {
                    self.exhausted = true;
                    self.items.clear();
                }
            }
        }
    }
    pub fn focus_in(&mut self, context: String) {
        if self.context.as_ref() != Some(&context) {
            self.invalidate();
            self.context = Some(context);
        }
    }
    pub fn focus_out(&mut self, context: &str) {
        if self.context.as_deref() == Some(context) {
            self.invalidate();
            self.context = None;
        }
    }
    pub fn register(&mut self, items: Vec<Property>) {
        self.invalidate();
        if !self.exhausted && self.context.is_some() {
            self.items = items;
        }
    }
    pub fn update(&mut self, item: Property) -> bool {
        fn replace(items: &mut [Property], item: &Property) -> bool {
            for current in items {
                if current.key == item.key {
                    // IBus update keeps the identity/type/sub-list intact.
                    if current.kind != item.kind {
                        return false;
                    }
                    current.label = item.label.clone();
                    current.symbol = item.symbol.clone();
                    current.tooltip = item.tooltip.clone();
                    current.visible = item.visible;
                    current.sensitive = item.sensitive;
                    current.state = item.state;
                    return true;
                }
                if replace(&mut current.children, item) {
                    return true;
                }
            }
            false
        }
        let updated = replace(&mut self.items, &item);
        if updated {
            self.epoch = match self.epoch.checked_add(1) {
                Some(epoch) => epoch,
                None => {
                    self.exhausted = true;
                    self.items.clear();
                    self.epoch
                }
            };
        }
        updated
    }
    pub fn token(&self, key: &str) -> Token {
        Token {
            epoch: self.epoch,
            key: key.into(),
        }
    }
    pub fn activate(&mut self, token: &Token) -> Vec<(String, i32)> {
        if self.exhausted
            || token.epoch != self.epoch
            || self.engine.is_none()
            || self.context.is_none()
        {
            return vec![];
        }
        fn activate(items: &mut [Property], key: &str) -> Vec<(String, i32)> {
            if let Some(index) = items.iter().position(|p| p.key == key) {
                let p = &mut items[index];
                if !p.visible || !p.sensitive {
                    return vec![];
                }
                match p.kind {
                    Kind::Action => return vec![(p.key.clone(), p.state as i32)],
                    Kind::Toggle => {
                        p.state = u32::from(p.state != 1);
                        return vec![(p.key.clone(), p.state as i32)];
                    }
                    Kind::Radio if p.state != 1 => {
                        return items
                            .iter_mut()
                            .enumerate()
                            .filter(|(_, p)| p.kind == Kind::Radio && p.visible)
                            .map(|(i, p)| {
                                p.state = u32::from(i == index);
                                (p.key.clone(), p.state as i32)
                            })
                            .collect();
                    }
                    _ => return vec![],
                }
            }
            for p in items {
                if p.kind == Kind::Menu && p.visible && p.sensitive {
                    let signals = activate(&mut p.children, key);
                    if !signals.is_empty() {
                        return signals;
                    }
                }
            }
            vec![]
        }
        activate(&mut self.items, &token.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ibus::text_variant;
    use glib::variant::ToVariant;

    fn wire_list(items: Vec<glib::Variant>) -> glib::Variant {
        glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
            "IBusPropList".to_variant(),
            glib::VariantDict::new(None).end(),
            glib::Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, items),
        ]))
    }
    fn wire(key: &str, kind: u32, state: u32, children: Vec<glib::Variant>) -> glib::Variant {
        glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
            "IBusProperty".to_variant(),
            glib::VariantDict::new(None).end(),
            key.to_variant(),
            kind.to_variant(),
            text_variant(key),
            "".to_variant(),
            text_variant("tip"),
            true.to_variant(),
            true.to_variant(),
            state.to_variant(),
            wire_list(children),
            text_variant("中"),
        ]))
    }
    fn model(items: Vec<glib::Variant>) -> Properties {
        let mut state = Properties::default();
        state.engine(Some("engine".into()));
        state.focus_in("/context/1".into());
        state.register(parse_list(&wire_list(items)).unwrap());
        state
    }
    #[test]
    fn initial_engine_reply_preserves_only_current_context_graph() {
        let mut model = Properties::default();
        model.focus_in("/context/initial".into());
        model.register(parse_list(&wire_list(vec![wire("x", 0, 0, vec![])])).unwrap());
        let before = model.token("x");
        assert!(model.activate(&before).is_empty());
        model.initial_engine("engine".into());
        assert!(model.activate(&before).is_empty());
        assert_eq!(model.activate(&model.token("x")), [("x".into(), 0)]);
        model.initial_engine("late-other".into());
        assert_eq!(model.engine.as_deref(), Some("engine"));
        model.focus_out("/context/initial");
        let old = model.token("x");
        assert!(model.activate(&old).is_empty());
    }
    #[test]
    fn typed_modern_and_explicit_legacy_properties() {
        let value = wire("InputMode", 1, 2, vec![]);
        let parsed = parse_property(&value).unwrap();
        assert_eq!(parsed.symbol.as_deref(), Some("中"));
        assert_eq!(parsed.state, 2);
        let raw = value.as_variant().unwrap();
        let legacy = glib::Variant::tuple_from_iter((0..11).map(|i| raw.child_value(i)));
        assert_eq!(parse_property(&legacy).unwrap().symbol, None);
        assert!(
            parse_property(&glib::Variant::tuple_from_iter((0..12).map(|i| {
                if i == 3 {
                    99u32.to_variant()
                } else {
                    raw.child_value(i)
                }
            })))
            .is_err()
        );
        assert!(parse_property(&wire("x", 0, 3, vec![])).is_err());
        assert!(parse_property(&text_variant("not a property")).is_err());
    }
    #[test]
    fn rejects_duplicate_oversized_deep_and_malformed_values() {
        assert!(parse_list(&wire_list(vec![
            wire("x", 0, 0, vec![]),
            wire("x", 4, 0, vec![])
        ]))
        .is_err());
        assert!(parse_property(&wire(&"x".repeat(MAX_TEXT + 1), 0, 0, vec![])).is_err());
        assert!(parse_list(&wire_list(
            (0..=MAX_NODES)
                .map(|i| wire(&i.to_string(), 0, 0, vec![]))
                .collect()
        ))
        .is_err());
        let mut nested = wire("leaf", 0, 0, vec![]);
        for i in 0..MAX_DEPTH - 1 {
            nested = wire(&format!("menu{i}"), 3, 0, vec![nested]);
        }
        assert!(parse_property(&nested).is_ok());
        assert!(parse_property(&wire("too-deep", 3, 0, vec![nested])).is_err());
        let raw = wire("a", 0, 0, vec![]).as_variant().unwrap();
        let malformed = glib::Variant::tuple_from_iter((0..12).map(|i| {
            if i == 4 {
                glib::Variant::from_variant(&"not-text".to_variant())
            } else {
                raw.child_value(i)
            }
        }));
        assert!(parse_property(&malformed).is_err());
        assert!(parse_list(&"x".repeat(MAX_BYTES + 1).to_variant()).is_err());
        assert!(parse_property(&wire("leaf", 0, 0, vec![wire("illegal", 0, 0, vec![])])).is_err());
    }
    #[test]
    fn radio_group_matches_visible_siblings_not_submenus_or_separators() {
        let mut state = model(vec![
            wire("a", 2, 1, vec![]),
            wire("sep", 4, 0, vec![]),
            wire("b", 2, 0, vec![]),
            wire("menu", 3, 0, vec![wire("nested", 2, 1, vec![])]),
        ]);
        let token = state.token("b");
        assert_eq!(state.activate(&token), [("a".into(), 0), ("b".into(), 1)]);
        assert!(state.activate(&token).is_empty());
        assert_eq!(state.items[3].children[0].state, 1);
        let a = state.token("a");
        state.items[0].sensitive = false;
        assert!(state.activate(&a).is_empty());
        state.items[0].sensitive = true;
        state.items[0].visible = false;
        assert!(state.activate(&a).is_empty());
    }
    #[test]
    fn toggle_action_and_epoch_edges_refuse_old_callbacks() {
        let mut state = model(vec![
            wire("toggle", 1, 0, vec![]),
            wire("action", 0, 2, vec![]),
        ]);
        let toggle = state.token("toggle");
        assert_eq!(state.activate(&toggle), [("toggle".into(), 1)]);
        assert_eq!(state.activate(&toggle), [("toggle".into(), 0)]);
        assert_eq!(
            state.activate(&state.token("action")),
            [("action".into(), 2)]
        );
        state.update(parse_property(&wire("toggle", 1, 1, vec![])).unwrap());
        assert!(state.activate(&toggle).is_empty());
        let latest = state.token("toggle");
        state.focus_in("/context/2".into());
        assert!(state.activate(&latest).is_empty());
        state.focus_out("/context/1");
        assert_eq!(state.context.as_deref(), Some("/context/2"));
        state.register(parse_list(&wire_list(vec![wire("toggle", 1, 0, vec![])])).unwrap());
        let latest = state.token("toggle");
        state.engine(Some("other".into()));
        assert!(state.activate(&latest).is_empty());
        state.reset();
        assert!(state.activate(&latest).is_empty());
    }
    #[test]
    fn updates_preserve_submenu_identity_and_counter_exhaustion_denies() {
        let mut state = model(vec![wire("menu", 3, 0, vec![wire("child", 0, 0, vec![])])]);
        assert!(state.update(parse_property(&wire("menu", 3, 0, vec![])).unwrap()));
        assert_eq!(state.items[0].children[0].key, "child");
        assert!(!state.update(parse_property(&wire("menu", 1, 0, vec![])).unwrap()));
        state.epoch = u64::MAX;
        let token = state.token("child");
        state.register(vec![]);
        assert!(state.activate(&token).is_empty());
        assert!(state.exhausted);
    }
}
