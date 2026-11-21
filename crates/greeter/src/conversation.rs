//! GDM's UserVerifier as the [`Host`] of the shared AuthPrompt machine.
//!
//! The lock screen's machine (`tuna-auth-prompt`) speaks in requests,
//! sequenced queries and one result per request; GDM speaks in service
//! names. This adapter is what gnome-shell 51's `userVerifier.js`
//! (`ShellUserVerifier`) and its auth services do between `authPrompt.js`
//! and libgdm's `GdmUserVerifier`:
//!
//! - `authServicesSSSDSwitchable.js`, first in priority while
//!   `enable-switchable-authentication` is set: every begin starts
//!   `gdm-switchable-auth`; SSSD answers with a CustomJSON
//!   `auth-mechanisms` request listing the user's mechanisms (password,
//!   smartcard certificates, passkey, web login `eidp`), the prompt is
//!   drawn here from it and the answer goes back as a JSON reply. When
//!   the service is missing, fails to start, or speaks plain PAM (the
//!   user is not SSSD's), its mechanisms are `NOT_FOUND` and the roles
//!   cascade to the legacy services.
//! - `authServicesLegacy.js`: `gdm-password` and `gdm-smartcard`, as
//!   `enable-password-authentication` and
//!   `enable-smartcard-authentication` allow; only the selected
//!   mechanism's service runs. Fingerprint is not offered: the login
//!   dialog's verifier is not re-authentication only, so it never makes a
//!   fingerprint manager (GDM 51.0: fingerprint no longer logs in).
//! - The selected mechanism (Login Options' first section) starts with
//!   the first selectable one; choosing another resets softly.
//! - A begin's result is the first of `VerificationComplete` (applied),
//!   `VerificationFailed` or `ConversationStopped` (denied) of its
//!   service; `SecretInfoQuery`/`InfoQuery` become sequenced queries,
//!   `ChoiceQuery` a choice list, and only the outstanding step may be
//!   answered; `Info`/`Problem` become messages.
//! - A cancel is `Cancel()`, after which GDM resets and stops every
//!   conversation: a cancelled begin's result is its
//!   `ConversationStopped` (at once when nothing ran).
//! - `_canRetry`: with a user, `allowed-failures` times, then give up.
//!
//! Pure: GDM calls are queued as [`Call`]s for the D-Bus layer, and GDM
//! signals come back as machine [`Input`]s.

use std::cell::RefCell;

use serde_json::{json, Value};
use tuna_auth_prompt::{Ask, Choice, Host};
use tuna_shell_control::{AuthEvent, AuthService};

use crate::gdm::{Call, Signal};

/// authServicesLegacy.js's services.
pub const PASSWORD_SERVICE: &str = "gdm-password";
pub const SMARTCARD_SERVICE: &str = "gdm-smartcard";
/// authServicesSSSDSwitchable.js's service.
pub const SWITCHABLE_SERVICE: &str = "gdm-switchable-auth";

/// authServicesSSSDSwitchable.js: SSSD warns before a passkey locks.
const PASSKEY_ATTEMPTS: &str =
    "You have %d attempts left. If the passkey gets locked, you may not able to access your account.";
/// SSSD reports an expired password only as PAM text.
const PASSWORD_EXPIRED: &str = "Password expired. Change your password now";

/// authServices.js `Role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Password,
    Smartcard,
    Passkey,
    /// `eidp`: web login through an identity provider.
    WebLogin,
}

impl Role {
    fn parse(role: &str) -> Option<Self> {
        match role {
            "password" => Some(Self::Password),
            "smartcard" => Some(Self::Smartcard),
            "passkey" => Some(Self::Passkey),
            "eidp" => Some(Self::WebLogin),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Smartcard => "smartcard",
            Self::Passkey => "passkey",
            Self::WebLogin => "eidp",
        }
    }
}

/// `org.gnome.login-screen`'s authentication keys (GDM 51.1.1
/// `org.gnome.login-screen.gschema.xml`, all true by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthPolicy {
    pub allowed_failures: u32,
    pub password: bool,
    pub smartcard: bool,
    pub switchable: bool,
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self {
            allowed_failures: 3,
            password: true,
            smartcard: true,
            switchable: true,
        }
    }
}

/// An item of Login Options' mechanism section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mechanism {
    pub service: &'static str,
    pub role: Role,
    pub name: String,
    /// The switchable mechanism's id in SSSD's JSON.
    pub id: Option<String>,
}

/// What the machine (or the login dialog) must hear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Event {
        request: u64,
        service: AuthService,
        event: AuthEvent,
    },
    Result {
        request: u64,
        applied: bool,
    },
    /// A step shown instead of the entry.
    Ask {
        request: u64,
        sequence: u32,
        ask: Ask,
    },
    /// authServices `reset({softReset: true})`.
    Restart,
    /// `mechanisms-changed`: Login Options' mechanisms or the selected one
    /// changed.
    Mechanisms,
    /// Call [`Conversations::timeout`] with `token` after `secs`.
    Timer {
        token: u64,
        secs: u64,
    },
}

/// How the outstanding step is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// A PAM query: `AnswerQuery`.
    Query,
    /// A ChoiceList query: `SelectChoice`.
    Choice,
    /// A switchable mechanism's entry: a JSON reply.
    Json,
    /// Switchable's certificate list: the chosen one's PIN is asked.
    Certificate,
    /// Web login's intro button: it opens the dialog.
    WebIntro,
    /// Web login's Done: an empty JSON reply.
    WebDone,
    /// An instruction nothing answers (insert the key, touch it).
    Instruction,
}

#[derive(Debug)]
struct Begun {
    request: u64,
    /// The GDM service this begin started; `None` once it stopped on its
    /// own, or when nothing could start.
    service: Option<&'static str>,
    outstanding: Option<(u32, Step)>,
    /// The channel the begin went out on (0: not yet known).
    channel: u64,
}

/// authServicesSSSDSwitchable.js `MechanismsStatus` (GNOME's names).
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    Waiting,
    NotFound,
    Found,
}

/// authServicesSSSDSwitchable.js `PromptStatus` (GNOME's names).
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prompt {
    None,
    PasswordPrompt,
    PasswordWaiting,
    CertList,
    PinPrompt,
    PinWaiting,
    InsertKey,
    Touch,
    WebIntro,
    WebDialog,
    WebWaiting,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Certificate {
    token_name: String,
    module_name: String,
    key_id: String,
    label: String,
    instruction: String,
    pin_prompt: String,
}

/// One mechanism of SSSD's `authSelection`.
#[derive(Clone, Debug, PartialEq)]
struct Switchable {
    id: String,
    role: Role,
    name: String,
    prompt: String,
    certificates: Vec<Certificate>,
    init_prompt: Option<String>,
    link_prompt: Option<String>,
    uri: Option<String>,
    code: Option<String>,
    timeout: u64,
    needs_refresh: bool,
    key_connected: bool,
    init_instruction: String,
    pin_prompt: String,
    pin_attempts: i64,
    touch_instruction: String,
    kerberos: Value,
    crypto_challenge: Value,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// SSSD's request, its mechanisms in the order SSSD sent them (the
/// order `Object.keys` gives gnome-shell, and Login Options' order).
#[derive(serde::Deserialize)]
struct Request {
    #[serde(rename = "authSelection")]
    auth_selection: Option<Selection>,
}

#[derive(serde::Deserialize)]
struct Selection {
    mechanisms: Option<Ordered>,
    priority: Option<Vec<String>>,
}

struct Ordered(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for Ordered {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object of mechanisms")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Ordered, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    entries.push(entry);
                }
                Ok(Ordered(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

/// SSSD's `authSelection` (`{"authSelection": {"mechanisms": {id: {...}},
/// "priority": [id, ...]}}`); `None` without mechanisms.
fn parse_selection(json: &str) -> Option<(Vec<Switchable>, Vec<String>)> {
    let request: Request = serde_json::from_str(json).ok()?;
    let selection = request.auth_selection?;
    let mechanisms = selection.mechanisms?.0;
    let priority = selection.priority.unwrap_or_default();
    let list = mechanisms
        .iter()
        .filter_map(|(id, m)| {
            let role = Role::parse(m.get("role")?.as_str()?)?;
            let certificates = m
                .get("certificates")
                .and_then(Value::as_array)
                .map(|certs| {
                    certs
                        .iter()
                        .map(|c| Certificate {
                            token_name: text(c, "tokenName").unwrap_or_default(),
                            module_name: text(c, "moduleName").unwrap_or_default(),
                            key_id: text(c, "keyId").unwrap_or_default(),
                            label: text(c, "label").unwrap_or_default(),
                            instruction: text(c, "certInstruction").unwrap_or_default(),
                            pin_prompt: text(c, "pinPrompt").unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Switchable {
                id: id.clone(),
                role,
                name: text(m, "name").unwrap_or_else(|| id.clone()),
                prompt: text(m, "prompt").unwrap_or_default(),
                certificates,
                init_prompt: text(m, "initPrompt").filter(|p| !p.is_empty()),
                link_prompt: text(m, "linkPrompt").filter(|p| !p.is_empty()),
                uri: text(m, "uri").filter(|u| !u.is_empty()),
                code: text(m, "code").filter(|c| !c.is_empty()),
                timeout: m.get("timeout").and_then(Value::as_u64).unwrap_or(0),
                needs_refresh: false,
                key_connected: m
                    .get("keyConnected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                init_instruction: text(m, "initInstruction").unwrap_or_default(),
                pin_prompt: text(m, "pinPrompt").unwrap_or_default(),
                pin_attempts: m.get("pinAttempts").and_then(Value::as_i64).unwrap_or(0),
                touch_instruction: text(m, "touchInstruction").unwrap_or_default(),
                kerberos: m.get("kerberos").cloned().unwrap_or(Value::Null),
                crypto_challenge: m.get("cryptoChallenge").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Some((list, priority))
}

/// `_parseCertInstruction`: the first line describes, the second is the
/// subject; the list shows its CN (and O).
fn certificate_choice(cert: &Certificate) -> Choice {
    let mut lines = cert.instruction.splitn(2, '\n');
    let description = lines.next().unwrap_or_default().to_owned();
    let subject = lines.next().unwrap_or_default();
    let field = |prefix: &str| {
        subject
            .split(',')
            .map(str::trim)
            .find_map(|f| f.strip_prefix(prefix))
            .map(str::to_owned)
    };
    let title = field("CN=").unwrap_or_else(|| cert.label.clone());
    let subtitle = match field("O=") {
        Some(org) if !description.is_empty() => Some(format!("{description} — {org}")),
        Some(org) => Some(org),
        None => Some(description).filter(|d| !d.is_empty()),
    };
    Choice {
        key: cert.key_id.clone(),
        title,
        subtitle,
    }
}

struct State {
    policy: AuthPolicy,
    user: Option<String>,
    next_request: u64,
    next_sequence: u32,
    next_token: u64,
    current: Option<Begun>,
    /// Cancelled begins whose `ConversationStopped` is still owed.
    retiring: Vec<Begun>,
    /// GDM calls to make, each begin with its request.
    calls: Vec<(Call, Option<u64>)>,
    inputs: Vec<Input>,
    legacy_failures: u32,
    switch_failures: u32,
    legacy_selected: Option<Role>,
    smartcard_in_progress: bool,
    found: Found,
    switchables: Vec<Switchable>,
    priority: Vec<String>,
    selected: Option<usize>,
    saved_role: Option<Role>,
    certificate: Option<usize>,
    prompt: Prompt,
    resetting_password: bool,
    web_timeout: Option<u64>,
}

/// The GDM side of one login prompt.
pub struct Conversations {
    state: RefCell<State>,
}

impl Conversations {
    pub fn new(policy: AuthPolicy) -> Self {
        Self {
            state: RefCell::new(State {
                policy,
                user: None,
                next_request: 0,
                next_sequence: 0,
                next_token: 0,
                current: None,
                retiring: Vec::new(),
                calls: Vec::new(),
                inputs: Vec::new(),
                legacy_failures: 0,
                switch_failures: 0,
                legacy_selected: None,
                smartcard_in_progress: false,
                found: if policy.switchable {
                    Found::Waiting
                } else {
                    Found::NotFound
                },
                switchables: Vec::new(),
                priority: Vec::new(),
                selected: None,
                saved_role: None,
                certificate: None,
                prompt: Prompt::None,
                resetting_password: false,
                web_timeout: None,
            }),
        }
    }

    /// The authentication keys changed (`_onSettingsChanged`).
    pub fn set_policy(&self, policy: AuthPolicy) {
        let mut state = self.state.borrow_mut();
        if state.policy.switchable != policy.switchable {
            state.found = if policy.switchable {
                Found::Waiting
            } else {
                Found::NotFound
            };
        }
        state.policy = policy;
        if state
            .legacy_selected
            .is_some_and(|role| !Self::legacy_allows(&policy, role))
        {
            state.legacy_selected = None;
        }
        state.inputs.push(Input::Mechanisms);
    }

    /// The user the next begins verify (the selected list item, or what
    /// was typed after "Not listed?"); a new user starts a new fail count,
    /// as a new `AuthPrompt.begin` does.
    pub fn set_user(&self, user: Option<String>) {
        let mut state = self.state.borrow_mut();
        if state.user != user {
            state.legacy_failures = 0;
            state.switch_failures = 0;
        }
        state.user = user;
    }

    /// `userVerifier.needsUsername()`: every service needs one, except
    /// the legacy services when smartcard is their only mechanism.
    pub fn needs_username(&self) -> bool {
        let state = self.state.borrow();
        state.policy.switchable
            || Self::legacy_mechanisms(&state.policy)
                .iter()
                .map(|m| m.role)
                .ne([Role::Smartcard])
    }

    /// The begin for `request` went out on `channel`.
    pub fn set_channel(&self, request: u64, channel: u64) {
        let mut state = self.state.borrow_mut();
        let State {
            current, retiring, ..
        } = &mut *state;
        for begun in current.iter_mut().chain(retiring.iter_mut()) {
            if begun.request == request {
                begun.channel = channel;
            }
        }
    }

    #[cfg(test)]
    pub fn take_calls(&self) -> Vec<Call> {
        self.take_outgoing()
            .into_iter()
            .map(|(call, _)| call)
            .collect()
    }

    /// GDM calls to make, oldest first, each begin with its request.
    pub fn take_outgoing(&self) -> Vec<(Call, Option<u64>)> {
        std::mem::take(&mut self.state.borrow_mut().calls)
    }

    /// Inputs the adapter produced on its own (a local step, a soft
    /// reset, a mechanism change), oldest first.
    pub fn take_inputs(&self) -> Vec<Input> {
        std::mem::take(&mut self.state.borrow_mut().inputs)
    }

    /// Login Options' mechanisms and the selected one
    /// (`_onMechanismsChanged`: per role, the first service's).
    pub fn mechanisms(&self) -> (Vec<Mechanism>, Option<Mechanism>) {
        let state = self.state.borrow();
        let list = Self::mechanism_list(&state);
        let selected = Self::selected_mechanism(&state);
        (list, selected)
    }

    /// The mechanism to select when none is (`selectedMechanism ??
    /// mechanisms.find(m => m.selectable)`).
    pub fn default_mechanism(&self) -> Option<Mechanism> {
        let state = self.state.borrow();
        if Self::selected_mechanism(&state).is_some() {
            return None;
        }
        Self::mechanism_list(&state).into_iter().next()
    }

    /// `userVerifier.selectMechanism`: whether anything changed. A legacy
    /// mechanism resets softly to start its service; a switchable one
    /// starts its prompt on the running conversation.
    pub fn select(&self, mechanism: &Mechanism) -> bool {
        let mut state = self.state.borrow_mut();
        if mechanism.service == SWITCHABLE_SERVICE {
            let Some(index) = state
                .switchables
                .iter()
                .position(|m| Some(&m.id) == mechanism.id.as_ref())
            else {
                return false;
            };
            if state.selected == Some(index) {
                return false;
            }
            state.selected = Some(index);
            Self::start_switchable(&mut state);
            state.inputs.push(Input::Mechanisms);
            return true;
        }
        if state.legacy_selected == Some(mechanism.role)
            || !Self::legacy_mechanisms(&state.policy).contains(mechanism)
        {
            return false;
        }
        state.legacy_selected = Some(mechanism.role);
        state.inputs.push(Input::Restart);
        state.inputs.push(Input::Mechanisms);
        true
    }

    /// Verification starts over for good (`userVerifier.reset()`): every
    /// service forgets its mechanism and failures.
    pub fn reset(&self) {
        let mut state = self.state.borrow_mut();
        state.legacy_failures = 0;
        state.switch_failures = 0;
        state.legacy_selected = None;
        state.saved_role = None;
        if state.policy.switchable {
            state.found = Found::Waiting;
        }
        Self::clear(&mut state);
    }

    /// gsd-smartcard saw a token inserted or removed
    /// (`_onSmartcardChanged`).
    pub fn smartcard_changed(&self, inserted: bool) {
        let mut state = self.state.borrow_mut();
        let restart = if state.found == Found::Found {
            state.selected.is_some() && state.switchables.iter().any(|m| m.role == Role::Smartcard)
        } else {
            state.legacy_selected == Some(Role::Smartcard)
                && !(state.smartcard_in_progress && inserted)
        };
        if restart {
            state.inputs.push(Input::Restart);
        }
    }

    /// A timer from [`Input::Timer`] fired (web login's `timeout`).
    pub fn timeout(&self, token: u64) {
        let mut state = self.state.borrow_mut();
        if state.web_timeout != Some(token) {
            return;
        }
        state.web_timeout = None;
        let selected_web =
            Self::selected_switchable(&state).map(|m| m.role) == Some(Role::WebLogin);
        if selected_web {
            state.inputs.push(Input::Restart);
        } else if let Some(m) = state
            .switchables
            .iter_mut()
            .find(|m| m.role == Role::WebLogin)
        {
            m.needs_refresh = true;
        }
    }

    /// The begin for `request` failed on the wire. The switchable service
    /// failing to start only means it has no mechanisms (the PAM service
    /// is missing): the roles cascade. Otherwise userVerifier.js
    /// `_reportInitError`, counted as a failure.
    pub fn begin_failed(&self, request: u64) -> Vec<Input> {
        let mut state = self.state.borrow_mut();
        let Some(begun) = state.current.as_mut().filter(|b| b.request == request) else {
            return Vec::new();
        };
        if begun.service == Some(SWITCHABLE_SERVICE) {
            begun.service = None;
            begun.outstanding = None;
            state.switch_failures += 1;
            Self::not_found(&mut state);
            return Vec::new();
        }
        state.current = None;
        state.legacy_failures += 1;
        vec![
            Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Problem {
                    text: tuna_auth_prompt::INIT_ERROR.to_owned(),
                },
            },
            Input::Result {
                request,
                applied: false,
            },
        ]
    }

    /// A channel to GDM closed: every begin on it (or not yet on one when
    /// `unassigned`) owed a result is denied.
    pub fn channel_closed(&self, channel: u64, unassigned: bool) -> Vec<Input> {
        let mut state = self.state.borrow_mut();
        let on = |b: &Begun| b.channel == channel || (unassigned && b.channel == 0);
        let mut owed: Vec<Begun> = Vec::new();
        if state.current.as_ref().is_some_and(on) {
            owed.extend(state.current.take());
        }
        let (gone, kept): (Vec<Begun>, Vec<Begun>) = std::mem::take(&mut state.retiring)
            .into_iter()
            .partition(on);
        state.retiring = kept;
        owed.extend(gone);
        owed.into_iter()
            .map(|begun| Input::Result {
                request: begun.request,
                applied: false,
            })
            .collect()
    }

    /// Translate one GDM signal.
    pub fn on_signal(&self, signal: &Signal) -> Vec<Input> {
        let mut state = self.state.borrow_mut();
        if let Signal::ConversationStopped { service } = signal {
            if let Some(index) = state
                .retiring
                .iter()
                .position(|b| b.service == Some(service.as_str()))
            {
                let begun = state.retiring.remove(index);
                return vec![Input::Result {
                    request: begun.request,
                    applied: false,
                }];
            }
        }
        let Some(service) = signal_service(signal) else {
            return Vec::new();
        };
        let running = state
            .current
            .as_ref()
            .and_then(|b| b.service)
            .filter(|s| *s == service);
        let Some(running) = running else {
            return Vec::new();
        };
        if running == SWITCHABLE_SERVICE && state.found != Found::Found {
            return Self::waiting_signal(&mut state, signal);
        }
        if running == SWITCHABLE_SERVICE {
            return Self::switchable_signal(&mut state, signal);
        }
        Self::legacy_signal(&mut state, signal)
    }

    // --- internals ---------------------------------------------------

    fn legacy_allows(policy: &AuthPolicy, role: Role) -> bool {
        match role {
            Role::Password => policy.password,
            Role::Smartcard => policy.smartcard,
            Role::Passkey | Role::WebLogin => false,
        }
    }

    /// authServicesLegacy.js `Mechanisms`, as the settings allow.
    fn legacy_mechanisms(policy: &AuthPolicy) -> Vec<Mechanism> {
        let mut list = Vec::new();
        if policy.password {
            list.push(Mechanism {
                service: PASSWORD_SERVICE,
                role: Role::Password,
                name: "Password".to_owned(),
                id: None,
            });
        }
        if policy.smartcard {
            list.push(Mechanism {
                service: SMARTCARD_SERVICE,
                role: Role::Smartcard,
                name: "Smartcard".to_owned(),
                id: None,
            });
        }
        list
    }

    fn switchable_mechanism(m: &Switchable) -> Mechanism {
        Mechanism {
            service: SWITCHABLE_SERVICE,
            role: m.role,
            name: m.name.clone(),
            id: Some(m.id.clone()),
        }
    }

    /// The legacy services hold their roles only while the switchable
    /// one found no mechanisms (`_redistributeRoles`).
    fn legacy_active(state: &State) -> bool {
        state.found == Found::NotFound
    }

    fn mechanism_list(state: &State) -> Vec<Mechanism> {
        match state.found {
            Found::Found => state
                .switchables
                .iter()
                .map(Self::switchable_mechanism)
                .collect(),
            Found::Waiting => Vec::new(),
            Found::NotFound => Self::legacy_mechanisms(&state.policy),
        }
    }

    fn selected_switchable(state: &State) -> Option<&Switchable> {
        (state.found == Found::Found)
            .then(|| state.selected.and_then(|i| state.switchables.get(i)))
            .flatten()
    }

    fn selected_mechanism(state: &State) -> Option<Mechanism> {
        if state.found == Found::Found {
            return Self::selected_switchable(state).map(Self::switchable_mechanism);
        }
        if !Self::legacy_active(state) {
            return None;
        }
        let role = state.legacy_selected?;
        Self::legacy_mechanisms(&state.policy)
            .into_iter()
            .find(|m| m.role == role)
    }

    fn sequence(state: &mut State) -> u32 {
        state.next_sequence += 1;
        state.next_sequence
    }

    /// Ask on the current begin: a query (entry) or a step (`ask`).
    fn query(state: &mut State, step: Step, question: &str, secret: bool) {
        let sequence = Self::sequence(state);
        let Some(begun) = state.current.as_mut() else {
            return;
        };
        begun.outstanding = Some((sequence, step));
        let request = begun.request;
        state.inputs.push(Input::Event {
            request,
            service: AuthService::Password,
            event: AuthEvent::Query {
                sequence,
                question: question.to_owned(),
                secret,
            },
        });
    }

    fn ask(state: &mut State, step: Step, ask: Ask) {
        let sequence = Self::sequence(state);
        let Some(begun) = state.current.as_mut() else {
            return;
        };
        begun.outstanding = Some((sequence, step));
        let request = begun.request;
        state.inputs.push(Input::Ask {
            request,
            sequence,
            ask,
        });
    }

    fn message(state: &mut State, event: AuthEvent) {
        if let Some(request) = state.current.as_ref().map(|b| b.request) {
            state.inputs.push(Input::Event {
                request,
                service: AuthService::Password,
                event,
            });
        }
    }

    /// `_handleSelectMechanism` of the switchable service.
    fn start_switchable(state: &mut State) {
        state.prompt = Prompt::None;
        let Some(m) = Self::selected_switchable(state).cloned() else {
            return;
        };
        match m.role {
            Role::Password => {
                state.prompt = Prompt::PasswordPrompt;
                Self::query(state, Step::Json, &m.prompt, true);
            }
            Role::Smartcard => Self::start_smartcard(state, &m),
            Role::Passkey => {
                if !m.key_connected {
                    state.prompt = Prompt::InsertKey;
                    Self::ask(
                        state,
                        Step::Instruction,
                        Ask::Choices {
                            prompt: m.init_instruction.clone(),
                            choices: Vec::new(),
                        },
                    );
                    return;
                }
                state.prompt = Prompt::PinPrompt;
                Self::query(state, Step::Json, &m.pin_prompt, true);
                if (1..=3).contains(&m.pin_attempts) {
                    let text = PASSKEY_ATTEMPTS.replace("%d", &m.pin_attempts.to_string());
                    Self::message(state, AuthEvent::Info { text });
                }
            }
            Role::WebLogin => Self::start_web_login(state, &m),
        }
    }

    fn start_smartcard(state: &mut State, m: &Switchable) {
        if let [cert] = m.certificates.as_slice() {
            state.certificate = Some(0);
            state.prompt = Prompt::PinPrompt;
            Self::query(state, Step::Json, &cert.pin_prompt, true);
            return;
        }
        let prompt = if m.certificates.is_empty() {
            "Insert Smartcard"
        } else {
            "Select Identity"
        };
        state.prompt = Prompt::CertList;
        Self::ask(
            state,
            Step::Certificate,
            Ask::Choices {
                prompt: prompt.to_owned(),
                choices: m.certificates.iter().map(certificate_choice).collect(),
            },
        );
    }

    fn start_web_login(state: &mut State, m: &Switchable) {
        let (Some(link), Some(uri)) = (m.link_prompt.clone(), m.uri.clone()) else {
            return;
        };
        if m.needs_refresh {
            state.inputs.push(Input::Restart);
            return;
        }
        if let Some(label) = m.init_prompt.clone() {
            state.prompt = Prompt::WebIntro;
            Self::ask(state, Step::WebIntro, Ask::Button { label });
        } else {
            Self::show_web_login(state, link, uri, m.code.clone());
        }
    }

    fn show_web_login(state: &mut State, message: String, url: String, code: Option<String>) {
        state.prompt = Prompt::WebDialog;
        Self::ask(state, Step::WebDone, Ask::WebLogin { message, url, code });
    }

    /// `_formatResponse` + `_sendResponse`.
    fn reply(state: &mut State, answer: Option<&str>) {
        let Some(m) = Self::selected_switchable(state).cloned() else {
            return;
        };
        let response = match m.role {
            Role::Password => json!({ "password": answer.unwrap_or_default() }),
            Role::Smartcard => {
                let cert = state
                    .certificate
                    .and_then(|i| m.certificates.get(i))
                    .cloned()
                    .unwrap_or_default();
                json!({
                    "pin": answer.unwrap_or_default(),
                    "tokenName": cert.token_name,
                    "moduleName": cert.module_name,
                    "keyId": cert.key_id,
                    "label": cert.label,
                })
            }
            Role::Passkey => json!({
                "pin": answer.unwrap_or_default(),
                "kerberos": m.kerberos,
                "cryptoChallenge": m.crypto_challenge,
            }),
            Role::WebLogin => json!({}),
        };
        let mut selection = serde_json::Map::new();
        selection.insert("status".to_owned(), json!("Ok"));
        selection.insert(m.id.clone(), response);
        let json = json!({ "authSelection": selection }).to_string();
        state.calls.push((
            Call::JsonReply {
                service: SWITCHABLE_SERVICE.to_owned(),
                json,
            },
            None,
        ));
    }

    /// The switchable service has no mechanisms for this user: its roles
    /// cascade to the legacy services (`mechanisms-changed`).
    fn not_found(state: &mut State) {
        state.found = Found::NotFound;
        state.switchables.clear();
        state.selected = None;
        state.inputs.push(Input::Mechanisms);
    }

    /// The switchable service is waiting for SSSD's mechanisms.
    fn waiting_signal(state: &mut State, signal: &Signal) -> Vec<Input> {
        match signal {
            Signal::JsonRequest { json, .. } => {
                let Some((list, priority)) = parse_selection(json) else {
                    Self::not_found(state);
                    return Vec::new();
                };
                let enabled: Vec<Switchable> = list
                    .into_iter()
                    .filter(|m| match m.role {
                        Role::Password => state.policy.password,
                        Role::Smartcard => state.policy.smartcard,
                        Role::Passkey | Role::WebLogin => true,
                    })
                    .collect();
                state.found = Found::Found;
                state.switchables = enabled;
                state.priority = priority;
                Self::track_web_timeout(state);
                // `_handleUpdateEnabledMechanisms`: the saved role, else
                // SSSD's priority, else the first.
                let saved = state.saved_role.take();
                let selected = saved
                    .and_then(|role| state.switchables.iter().position(|m| m.role == role))
                    .or_else(|| {
                        state
                            .priority
                            .first()
                            .and_then(|id| state.switchables.iter().position(|m| &m.id == id))
                    })
                    .or((!state.switchables.is_empty()).then_some(0));
                state.selected = selected;
                Self::start_switchable(state);
                state.inputs.push(Input::Mechanisms);
            }
            // `_eventExpected`: plain PAM before any JSON means pam_unix,
            // not SSSD: fall back to the legacy services.
            Signal::Info { .. }
            | Signal::Problem { .. }
            | Signal::InfoQuery { .. }
            | Signal::SecretInfoQuery { .. }
            | Signal::ServiceUnavailable { .. } => Self::not_found(state),
            Signal::ConversationStopped { .. } => {
                if let Some(begun) = state.current.as_mut() {
                    begun.service = None;
                    begun.outstanding = None;
                }
                if state.found == Found::Waiting {
                    Self::not_found(state);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn track_web_timeout(state: &mut State) {
        state.web_timeout = None;
        let timeout = state
            .switchables
            .iter()
            .find(|m| m.role == Role::WebLogin)
            .map(|m| m.timeout)
            .filter(|t| *t > 0);
        if let Some(secs) = timeout {
            state.next_token += 1;
            let token = state.next_token;
            state.web_timeout = Some(token);
            state.inputs.push(Input::Timer { token, secs });
        }
    }

    fn switchable_signal(state: &mut State, signal: &Signal) -> Vec<Input> {
        let selected = Self::selected_switchable(state).map(|m| m.role);
        match signal {
            Signal::Info { text, .. } => {
                if selected == Some(Role::Password) && text.contains(PASSWORD_EXPIRED) {
                    state.resetting_password = true;
                }
                if selected.is_some() {
                    Self::message(state, AuthEvent::Info { text: text.clone() });
                }
            }
            Signal::Problem { text, .. } if selected.is_some() => {
                Self::message(state, AuthEvent::Problem { text: text.clone() });
            }
            Signal::SecretInfoQuery { query, .. }
                if selected == Some(Role::Password) && state.resetting_password =>
            {
                Self::query(state, Step::Query, query, true);
            }
            Signal::ServiceUnavailable { message, .. } if !message.is_empty() => {
                Self::message(
                    state,
                    AuthEvent::Problem {
                        text: message.clone(),
                    },
                );
            }
            Signal::VerificationFailed { .. } | Signal::ConversationStopped { .. } => {
                if let Some(begun) = state.current.take() {
                    state.switch_failures += 1;
                    state.inputs.push(Input::Result {
                        request: begun.request,
                        applied: false,
                    });
                }
            }
            Signal::VerificationComplete { .. } => {
                if let Some(begun) = state.current.take() {
                    state.inputs.push(Input::Result {
                        request: begun.request,
                        applied: true,
                    });
                }
            }
            _ => {}
        }
        std::mem::take(&mut state.inputs)
    }

    fn legacy_signal(state: &mut State, signal: &Signal) -> Vec<Input> {
        match signal {
            Signal::ConversationStopped { .. } | Signal::VerificationFailed { .. } => {
                if let Some(begun) = state.current.take() {
                    if matches!(signal, Signal::VerificationFailed { .. }) {
                        state.legacy_failures += 1;
                    }
                    return vec![Input::Result {
                        request: begun.request,
                        applied: false,
                    }];
                }
            }
            Signal::VerificationComplete { .. } => {
                if let Some(begun) = state.current.take() {
                    return vec![Input::Result {
                        request: begun.request,
                        applied: true,
                    }];
                }
            }
            Signal::Info { text, .. } => {
                Self::message(state, AuthEvent::Info { text: text.clone() });
            }
            Signal::Problem { text, .. } => {
                Self::message(state, AuthEvent::Problem { text: text.clone() });
            }
            Signal::ServiceUnavailable { message, .. } if !message.is_empty() => {
                Self::message(
                    state,
                    AuthEvent::Problem {
                        text: message.clone(),
                    },
                );
            }
            Signal::InfoQuery { query, .. } => Self::query(state, Step::Query, query, false),
            Signal::SecretInfoQuery { query, .. } => Self::query(state, Step::Query, query, true),
            Signal::ChoiceQuery {
                prompt, choices, ..
            } => {
                // `_handleOnChoiceListQuery`.
                let choices = choices
                    .iter()
                    .map(|(key, title)| Choice {
                        key: key.clone(),
                        title: title.clone(),
                        subtitle: None,
                    })
                    .collect();
                Self::ask(
                    state,
                    Step::Choice,
                    Ask::Choices {
                        prompt: prompt.clone(),
                        choices,
                    },
                );
            }
            _ => {}
        }
        std::mem::take(&mut state.inputs)
    }

    /// `clear()` of every service.
    fn clear(state: &mut State) {
        state.switchables.clear();
        state.priority.clear();
        state.selected = None;
        state.certificate = None;
        state.prompt = Prompt::None;
        state.resetting_password = false;
        state.web_timeout = None;
        state.smartcard_in_progress = false;
    }

    fn start_service(state: &mut State, service: &'static str) {
        let call = match state.user.clone() {
            Some(user) => Call::BeginVerificationForUser {
                service: service.to_owned(),
                user,
            },
            None => Call::BeginVerification {
                service: service.to_owned(),
            },
        };
        let request = state.current.as_ref().map(|b| b.request);
        state.calls.push((call, request));
    }
}

fn signal_service(signal: &Signal) -> Option<&str> {
    match signal {
        Signal::ConversationStarted { service }
        | Signal::ConversationStopped { service }
        | Signal::Info { service, .. }
        | Signal::Problem { service, .. }
        | Signal::InfoQuery { service, .. }
        | Signal::SecretInfoQuery { service, .. }
        | Signal::ServiceUnavailable { service, .. }
        | Signal::VerificationFailed { service }
        | Signal::VerificationComplete { service }
        | Signal::ChoiceQuery { service, .. }
        | Signal::JsonRequest { service, .. } => Some(service),
        _ => None,
    }
}

impl Host for Conversations {
    /// `userVerifier.begin`: the switchable service while it waits for
    /// mechanisms, else the selected legacy mechanism's service; with
    /// nothing selected nothing starts and the mechanisms are announced,
    /// so the login dialog selects the first (which resets softly).
    fn begin(&self, service: AuthService) -> Option<u64> {
        if service != AuthService::Password {
            return None;
        }
        let needs_username = self.needs_username();
        let mut state = self.state.borrow_mut();
        if needs_username && state.user.is_none() {
            return None;
        }
        if let Some(previous) = state.current.take() {
            state.retiring.push(previous);
        }
        state.next_request += 1;
        let request = state.next_request;
        let mut started = None;
        if state.policy.switchable && state.found == Found::Waiting {
            started = Some(SWITCHABLE_SERVICE);
        } else if Self::legacy_active(&state) {
            started = Self::selected_mechanism(&state).map(|m| m.service);
        }
        state.current = Some(Begun {
            request,
            service: started,
            outstanding: None,
            channel: 0,
        });
        if let Some(service) = started {
            Self::start_service(&mut state, service);
        }
        if Self::legacy_active(&state) {
            // authServicesLegacy.js `_handleBeginVerification`.
            state.inputs.push(Input::Mechanisms);
        }
        Some(request)
    }

    fn answer(&self, service: AuthService, sequence: u32, answer: String) -> Option<u64> {
        let mut state = self.state.borrow_mut();
        let begun = state
            .current
            .as_mut()
            .filter(|_| service == AuthService::Password)?;
        let step = match begun.outstanding {
            Some((outstanding, step)) if outstanding == sequence => step,
            _ => return None,
        };
        begun.outstanding = None;
        let request = begun.request;
        let gdm_service = begun.service?;
        match step {
            Step::Query => {
                if state.legacy_selected == Some(Role::Smartcard) && Self::legacy_active(&state) {
                    state.smartcard_in_progress = true;
                }
                state.calls.push((
                    Call::AnswerQuery {
                        service: gdm_service.to_owned(),
                        answer,
                    },
                    None,
                ));
            }
            Step::Choice => state.calls.push((
                Call::SelectChoice {
                    service: gdm_service.to_owned(),
                    choice: answer,
                },
                None,
            )),
            Step::Json => {
                let role = Self::selected_switchable(&state).map(|m| m.role)?;
                Self::reply(&mut state, Some(&answer));
                match role {
                    Role::Password => state.prompt = Prompt::PasswordWaiting,
                    Role::Smartcard => state.prompt = Prompt::PinWaiting,
                    Role::Passkey => {
                        state.prompt = Prompt::Touch;
                        let touch = Self::selected_switchable(&state)
                            .map(|m| m.touch_instruction.clone())
                            .unwrap_or_default();
                        Self::ask(
                            &mut state,
                            Step::Instruction,
                            Ask::Choices {
                                prompt: touch,
                                choices: Vec::new(),
                            },
                        );
                    }
                    Role::WebLogin => {}
                }
            }
            Step::Certificate => {
                let m = Self::selected_switchable(&state)?.clone();
                let index = m.certificates.iter().position(|c| c.key_id == answer)?;
                state.certificate = Some(index);
                state.prompt = Prompt::PinPrompt;
                Self::query(
                    &mut state,
                    Step::Json,
                    &m.certificates[index].pin_prompt,
                    true,
                );
            }
            Step::WebIntro => {
                let m = Self::selected_switchable(&state)?.clone();
                Self::show_web_login(
                    &mut state,
                    m.link_prompt.unwrap_or_default(),
                    m.uri.unwrap_or_default(),
                    m.code,
                );
            }
            Step::WebDone => {
                state.prompt = Prompt::WebWaiting;
                Self::reply(&mut state, None);
                state.web_timeout = None;
            }
            Step::Instruction => return None,
        }
        Some(request)
    }

    /// `userVerifier.cancel()`: the switchable service keeps its mechanism
    /// for the next begin, every service clears, and GDM cancels.
    fn cancel(&self) -> Option<u64> {
        let mut state = self.state.borrow_mut();
        let begun = state.current.take()?;
        if state.found == Found::Found {
            state.saved_role = Self::selected_switchable(&state).map(|m| m.role);
            state.found = Found::Waiting;
        }
        Self::clear(&mut state);
        let request = begun.request;
        if begun.service.is_some() {
            state.retiring.push(begun);
        } else {
            // Nothing ran: nothing will stop.
            state.inputs.push(Input::Result {
                request,
                applied: false,
            });
        }
        state.calls.push((Call::Cancel, None));
        Some(request)
    }

    /// The single-password `Unlock` is the lock screen's own fallback;
    /// GDM has none.
    fn unlock(&self, _password: String) -> Option<u64> {
        None
    }

    fn fingerprint_enabled(&self) -> bool {
        false
    }

    /// The greeter draws no wiggle yet.
    fn wiggle_ms(&self) -> u64 {
        0
    }

    fn can_retry(&self) -> bool {
        let state = self.state.borrow();
        let failures = if state.found == Found::Found {
            state.switch_failures
        } else {
            state.legacy_failures
        };
        state.user.is_some() && failures < state.policy.allowed_failures
    }

    /// authServicesSSSDSwitchable.js `cancelRequested`.
    fn cancel_requested(&self) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(m) = Self::selected_switchable(&state).cloned() else {
            return false;
        };
        match (m.role, state.prompt) {
            (Role::Password, Prompt::PasswordWaiting)
            | (Role::Smartcard, Prompt::PinWaiting)
            | (Role::Passkey, Prompt::Touch)
            | (Role::WebLogin, Prompt::WebWaiting) => {
                state.inputs.push(Input::Restart);
                true
            }
            (Role::Smartcard, Prompt::PinPrompt) if m.certificates.len() > 1 => {
                state.certificate = None;
                Self::start_smartcard(&mut state, &m);
                true
            }
            (Role::WebLogin, Prompt::WebDialog) if m.init_prompt.is_some() => {
                Self::start_web_login(&mut state, &m);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "alice";

    fn s(service: &str) -> String {
        service.to_owned()
    }

    fn legacy() -> AuthPolicy {
        AuthPolicy {
            switchable: false,
            ..AuthPolicy::default()
        }
    }

    /// A password begin with gdm-password selected, as the login dialog
    /// gets there: nothing selected starts nothing, the first mechanism
    /// is selected, the soft reset cancels and begins gdm-password.
    fn begun(c: &Conversations) -> u64 {
        c.set_user(Some(USER.into()));
        let first = c.begin(AuthService::Password).unwrap();
        assert!(
            c.take_calls().is_empty(),
            "nothing selected: nothing starts"
        );
        assert_eq!(c.take_inputs(), vec![Input::Mechanisms]);
        let default = c.default_mechanism().unwrap();
        assert_eq!(default.role, Role::Password);
        assert!(c.select(&default));
        assert_eq!(c.take_inputs(), vec![Input::Restart, Input::Mechanisms]);
        assert_eq!(c.cancel(), Some(first));
        assert_eq!(
            c.take_inputs(),
            vec![Input::Result {
                request: first,
                applied: false
            }]
        );
        let request = c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls(),
            vec![
                Call::Cancel,
                Call::BeginVerificationForUser {
                    service: s(PASSWORD_SERVICE),
                    user: s(USER)
                }
            ]
        );
        c.take_inputs();
        request
    }

    fn query_sequence(inputs: &[Input]) -> u32 {
        inputs
            .iter()
            .find_map(|i| match i {
                Input::Event {
                    event: AuthEvent::Query { sequence, .. },
                    ..
                } => Some(*sequence),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no query in {inputs:?}"))
    }

    #[test]
    fn begin_needs_a_user_and_logs_in_with_the_password_service_only() {
        let c = Conversations::new(legacy());
        assert_eq!(c.begin(AuthService::Password), None);
        c.set_user(Some(USER.into()));
        assert_eq!(c.begin(AuthService::Fingerprint), None);
        assert!(!c.fingerprint_enabled());
        assert_eq!(c.unlock("x".into()), None);
        assert!(c.begin(AuthService::Password).is_some());
        let (list, _) = c.mechanisms();
        assert_eq!(
            list.iter().map(|m| m.service).collect::<Vec<_>>(),
            vec![PASSWORD_SERVICE, SMARTCARD_SERVICE],
            "no fingerprint on the login screen"
        );
    }

    #[test]
    fn secret_query_is_answered_once_with_its_sequence() {
        let c = Conversations::new(legacy());
        let request = begun(&c);
        let inputs = c.on_signal(&Signal::SecretInfoQuery {
            service: s(PASSWORD_SERVICE),
            query: s("Password:"),
        });
        let sequence = query_sequence(&inputs);
        assert_eq!(
            c.answer(AuthService::Password, sequence + 1, "x".into()),
            None
        );
        assert_eq!(
            c.answer(AuthService::Password, sequence, "pw".into()),
            Some(request)
        );
        assert_eq!(
            c.take_calls(),
            vec![Call::AnswerQuery {
                service: s(PASSWORD_SERVICE),
                answer: s("pw")
            }]
        );
        assert_eq!(c.answer(AuthService::Password, sequence, "pw".into()), None);
        assert!(c.take_calls().is_empty());
    }

    #[test]
    fn a_second_query_after_the_password_is_the_next_step() {
        let c = Conversations::new(legacy());
        let request = begun(&c);
        let first = query_sequence(&c.on_signal(&Signal::SecretInfoQuery {
            service: s(PASSWORD_SERVICE),
            query: s("Password:"),
        }));
        c.answer(AuthService::Password, first, "pw".into());
        let inputs = c.on_signal(&Signal::InfoQuery {
            service: s(PASSWORD_SERVICE),
            query: s("Verification code: "),
        });
        assert!(matches!(
            inputs[0],
            Input::Event {
                event: AuthEvent::Query { secret: false, .. },
                ..
            }
        ));
        let second = query_sequence(&inputs);
        assert_ne!(first, second);
        assert_eq!(
            c.answer(AuthService::Password, second, "123456".into()),
            Some(request)
        );
        assert_eq!(c.take_calls().len(), 2);
    }

    #[test]
    fn visible_query_messages_and_unavailable_reach_the_machine() {
        let c = Conversations::new(legacy());
        let request = begun(&c);
        let ev = |signal| c.on_signal(&signal);
        assert_eq!(
            ev(Signal::Info {
                service: s(PASSWORD_SERVICE),
                text: s("Sent")
            }),
            vec![Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Info { text: s("Sent") }
            }]
        );
        assert_eq!(
            ev(Signal::ServiceUnavailable {
                service: s(PASSWORD_SERVICE),
                message: s("Gone")
            }),
            vec![Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Problem { text: s("Gone") }
            }]
        );
        assert!(ev(Signal::Problem {
            service: s(SMARTCARD_SERVICE),
            text: s("x")
        })
        .is_empty());
        assert!(ev(Signal::SecretInfoQuery {
            service: s("gdm-fingerprint"),
            query: s("x")
        })
        .is_empty());
    }

    #[test]
    fn failure_then_stop_is_one_denied_result_and_counts() {
        let c = Conversations::new(AuthPolicy {
            allowed_failures: 2,
            ..legacy()
        });
        let request = begun(&c);
        assert_eq!(
            c.on_signal(&Signal::VerificationFailed {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request,
                applied: false
            }]
        );
        assert!(c
            .on_signal(&Signal::ConversationStopped {
                service: s(PASSWORD_SERVICE)
            })
            .is_empty());
        assert!(c.can_retry());
        // The soft reset keeps the selected mechanism: gdm-password again.
        let second = c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls(),
            vec![Call::BeginVerificationForUser {
                service: s(PASSWORD_SERVICE),
                user: s(USER)
            }]
        );
        assert_ne!(second, request);
        c.on_signal(&Signal::VerificationFailed {
            service: s(PASSWORD_SERVICE),
        });
        assert!(!c.can_retry(), "allowed-failures reached");
        c.set_user(Some("bob".into()));
        assert!(c.can_retry());
        c.set_user(None);
        assert!(!c.can_retry(), "no user, no retry (`_canRetry`)");
    }

    #[test]
    fn a_cancelled_begin_retires_on_conversation_stopped() {
        let c = Conversations::new(legacy());
        let first = begun(&c);
        assert_eq!(c.cancel(), Some(first));
        assert_eq!(c.take_calls(), vec![Call::Cancel]);
        assert!(c.take_inputs().is_empty(), "it ran: its stop is owed");
        assert_eq!(c.cancel(), None);
        let second = c.begin(AuthService::Password).unwrap();
        c.take_calls();
        assert_eq!(
            c.on_signal(&Signal::ConversationStopped {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request: first,
                applied: false
            }]
        );
        assert_eq!(
            c.on_signal(&Signal::VerificationComplete {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request: second,
                applied: true
            }]
        );
    }

    #[test]
    fn wire_failures_and_closed_channels_deny_what_is_owed() {
        let c = Conversations::new(legacy());
        let request = begun(&c);
        let inputs = c.begin_failed(request);
        assert_eq!(inputs.len(), 2);
        assert!(c.begin_failed(request).is_empty());
        let a = c.begin(AuthService::Password).unwrap();
        c.set_channel(a, 7);
        c.cancel();
        let b = c.begin(AuthService::Password).unwrap();
        c.set_channel(b, 1);
        assert_eq!(
            c.channel_closed(7, false),
            vec![Input::Result {
                request: a,
                applied: false
            }]
        );
        assert_eq!(
            c.channel_closed(1, true),
            vec![Input::Result {
                request: b,
                applied: false
            }]
        );
        assert!(c.channel_closed(1, true).is_empty());
    }

    #[test]
    fn missing_switchable_service_cascades_to_password() {
        let c = Conversations::new(AuthPolicy::default());
        c.set_user(Some(USER.into()));
        assert!(c.mechanisms().0.is_empty(), "waiting for SSSD");
        let first = c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls(),
            vec![Call::BeginVerificationForUser {
                service: s(SWITCHABLE_SERVICE),
                user: s(USER)
            }]
        );
        assert!(c.take_inputs().is_empty());
        // GDM: the PAM service is missing.
        assert!(c.begin_failed(first).is_empty());
        assert_eq!(c.take_inputs(), vec![Input::Mechanisms]);
        // Its stop signals do not fail the prompt.
        assert!(c
            .on_signal(&Signal::VerificationFailed {
                service: s(SWITCHABLE_SERVICE)
            })
            .is_empty());
        let default = c.default_mechanism().unwrap();
        assert_eq!(default.service, PASSWORD_SERVICE);
        assert!(c.select(&default));
        assert_eq!(c.take_inputs()[0], Input::Restart);
        assert_eq!(c.cancel(), Some(first));
        assert_eq!(
            c.take_inputs(),
            vec![Input::Result {
                request: first,
                applied: false
            }]
        );
        c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls(),
            vec![
                Call::Cancel,
                Call::BeginVerificationForUser {
                    service: s(PASSWORD_SERVICE),
                    user: s(USER)
                }
            ]
        );
        // A hard reset tries the switchable service again.
        c.reset();
        c.cancel();
        c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls().last(),
            Some(&Call::BeginVerificationForUser {
                service: s(SWITCHABLE_SERVICE),
                user: s(USER)
            })
        );
    }

    #[test]
    fn plain_pam_on_the_switchable_service_is_not_sssd() {
        let c = Conversations::new(AuthPolicy::default());
        c.set_user(Some(USER.into()));
        let first = c.begin(AuthService::Password).unwrap();
        c.take_calls();
        assert!(c
            .on_signal(&Signal::SecretInfoQuery {
                service: s(SWITCHABLE_SERVICE),
                query: s("Password:")
            })
            .is_empty());
        assert_eq!(c.take_inputs(), vec![Input::Mechanisms]);
        assert!(c.select(&c.default_mechanism().unwrap()));
        c.take_inputs();
        assert_eq!(c.cancel(), Some(first));
        assert!(c.take_inputs().is_empty(), "the running one must stop");
        assert_eq!(
            c.on_signal(&Signal::ConversationStopped {
                service: s(SWITCHABLE_SERVICE)
            }),
            vec![Input::Result {
                request: first,
                applied: false
            }]
        );
    }

    const SELECTION: &str = r#"{"authSelection": {"mechanisms": {
        "password": {"name": "Password", "role": "password", "selectable": true, "prompt": "Password"},
        "eidp": {"name": "Web Login", "role": "eidp", "selectable": true,
                 "initPrompt": "Log In", "linkPrompt": "Log in online with another device",
                 "uri": "https://idp.example/device", "code": "1234-5678", "timeout": 300},
        "smartcard:1": {"name": "Smartcard", "role": "smartcard", "selectable": true,
                 "certificates": [
                   {"tokenName": "T1", "moduleName": "m.so", "keyId": "k1", "label": "L1",
                    "certInstruction": "Certificate for PIV\nCN=Ada Lovelace,O=Example", "pinPrompt": "PIN"},
                   {"tokenName": "T2", "moduleName": "m.so", "keyId": "k2", "label": "L2",
                    "certInstruction": "Signing\nCN=Ada L", "pinPrompt": "PIN"}]}},
        "priority": ["eidp", "password"]}}"#;

    fn found(c: &Conversations) -> (u64, Vec<Input>) {
        found_json(c, SELECTION)
    }

    /// A begin whose switchable conversation got SSSD's `json`.
    fn found_json(c: &Conversations, json: &str) -> (u64, Vec<Input>) {
        c.set_user(Some(USER.into()));
        let request = c.begin(AuthService::Password).unwrap();
        c.take_calls();
        let inputs = c.on_signal(&Signal::JsonRequest {
            service: s(SWITCHABLE_SERVICE),
            protocol: s("auth-mechanisms"),
            version: 1,
            json: s(json),
        });
        assert!(inputs.is_empty());
        (request, c.take_inputs())
    }

    /// SSSD's passkey mechanism (pam_sss's JSON).
    fn passkey(connected: bool, attempts: i64) -> String {
        json!({"authSelection": {"mechanisms": {"passkey": {
            "name": "Passkey", "role": "passkey", "selectable": true,
            "initInstruction": "Insert your passkey device, then press ENTER.",
            "keyConnected": connected, "pinRequest": true, "pinAttempts": attempts,
            "pinPrompt": "Security key PIN", "touchInstruction": "Touch security key",
            "kerberos": true, "cryptoChallenge": "6uDMvRKj3W5xJV3HaQjZrtXMNmUUAjRGklFG2MIhN5s="}},
            "priority": ["passkey"]}})
        .to_string()
    }

    fn instruction(inputs: &[Input]) -> (u32, String) {
        inputs
            .iter()
            .find_map(|i| match i {
                Input::Ask {
                    sequence,
                    ask: Ask::Choices { prompt, choices },
                    ..
                } if choices.is_empty() => Some((*sequence, prompt.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no instruction in {inputs:?}"))
    }

    #[test]
    fn sssd_passkey_without_the_key_asks_for_it() {
        let c = Conversations::new(AuthPolicy::default());
        let (_, inputs) = found_json(&c, &passkey(false, 8));
        let (list, selected) = c.mechanisms();
        assert_eq!(list.len(), 1);
        assert_eq!(selected.unwrap().role, Role::Passkey);
        let (sequence, prompt) = instruction(&inputs);
        assert_eq!(prompt, "Insert your passkey device, then press ENTER.");
        assert_eq!(
            c.answer(AuthService::Password, sequence, String::new()),
            None,
            "nothing answers an instruction"
        );
        assert!(c.take_calls().is_empty());
        assert!(!c.cancel_requested(), "Back at the key prompt resets hard");
        assert!(c.take_inputs().is_empty());
    }

    #[test]
    fn sssd_passkey_pin_then_touch_replies_json() {
        let c = Conversations::new(AuthPolicy::default());
        let (request, inputs) = found_json(&c, &passkey(true, 2));
        let pin = query_sequence(&inputs);
        assert!(inputs.contains(&Input::Event {
            request,
            service: AuthService::Password,
            event: AuthEvent::Query {
                sequence: pin,
                question: s("Security key PIN"),
                secret: true
            }
        }));
        assert!(
            inputs.contains(&Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Info {
                    text: s("You have 2 attempts left. If the passkey gets locked, you may not able to access your account.")
                }
            }),
            "{inputs:?}"
        );
        assert!(!c.cancel_requested(), "Back at the PIN resets hard");
        assert_eq!(
            c.answer(AuthService::Password, pin, "4711".into()),
            Some(request)
        );
        let calls = c.take_calls();
        let [Call::JsonReply { service, json }] = calls.as_slice() else {
            panic!("{calls:?}")
        };
        assert_eq!(service, SWITCHABLE_SERVICE);
        assert_eq!(
            serde_json::from_str::<Value>(json).unwrap(),
            json!({"authSelection": {"status": "Ok", "passkey": {
                "pin": "4711", "kerberos": true,
                "cryptoChallenge": "6uDMvRKj3W5xJV3HaQjZrtXMNmUUAjRGklFG2MIhN5s="}}})
        );
        assert!(!format!("{calls:?}").contains("4711"));
        let (touch, prompt) = instruction(&c.take_inputs());
        assert_eq!(prompt, "Touch security key");
        assert_eq!(c.answer(AuthService::Password, touch, String::new()), None);
        // Back at the touch: the PIN went out, a soft reset asks again.
        assert!(c.cancel_requested());
        assert_eq!(c.take_inputs(), vec![Input::Restart]);
        assert_eq!(
            c.on_signal(&Signal::VerificationComplete {
                service: s(SWITCHABLE_SERVICE)
            }),
            vec![Input::Result {
                request,
                applied: true
            }]
        );
    }

    #[test]
    fn sssd_passkey_warns_only_of_its_last_three_attempts() {
        for (attempts, warned) in [(0, false), (1, true), (3, true), (4, false), (8, false)] {
            let c = Conversations::new(AuthPolicy::default());
            let (_, inputs) = found_json(&c, &passkey(true, attempts));
            let warning = inputs.iter().any(|i| {
                matches!(i, Input::Event { event: AuthEvent::Info { text }, .. }
                    if text.contains("attempts left"))
            });
            assert_eq!(warning, warned, "pinAttempts {attempts}");
        }
    }

    const PASSWORD_ONLY: &str = r#"{"authSelection": {"mechanisms": {
        "password": {"name": "Password", "role": "password", "selectable": true, "prompt": "Password"}},
        "priority": ["password"]}}"#;

    #[test]
    fn an_expired_sssd_password_is_changed_through_pam_queries() {
        let c = Conversations::new(AuthPolicy::default());
        let (request, inputs) = found_json(&c, PASSWORD_ONLY);
        let password = query_sequence(&inputs);
        c.answer(AuthService::Password, password, "old".into());
        assert!(matches!(
            c.take_calls().as_slice(),
            [Call::JsonReply { .. }]
        ));
        // Before SSSD says the password expired, PAM queries are not its.
        let query = |question: &str| {
            c.on_signal(&Signal::SecretInfoQuery {
                service: s(SWITCHABLE_SERVICE),
                query: s(question),
            })
        };
        assert!(query("New Password: ").is_empty());
        assert_eq!(
            c.on_signal(&Signal::Info {
                service: s(SWITCHABLE_SERVICE),
                text: s("Password expired. Change your password now.")
            }),
            vec![Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Info {
                    text: s("Password expired. Change your password now.")
                }
            }]
        );
        // The new password, twice, as PAM answers (not JSON).
        for (question, answer) in [("New Password: ", "new"), ("Reenter new Password: ", "new")] {
            let inputs = query(question);
            let sequence = query_sequence(&inputs);
            assert!(matches!(
                &inputs[0],
                Input::Event { event: AuthEvent::Query { question: q, secret: true, .. }, .. }
                    if q == question
            ));
            assert_eq!(
                c.answer(AuthService::Password, sequence, answer.into()),
                Some(request)
            );
            assert_eq!(
                c.take_calls(),
                vec![Call::AnswerQuery {
                    service: s(SWITCHABLE_SERVICE),
                    answer: s(answer)
                }]
            );
        }
        assert!(
            c.on_signal(&Signal::InfoQuery {
                service: s(SWITCHABLE_SERVICE),
                query: s("x")
            })
            .is_empty(),
            "`_handleOnInfoQuery` shows nothing"
        );
        assert_eq!(
            c.on_signal(&Signal::VerificationComplete {
                service: s(SWITCHABLE_SERVICE)
            }),
            vec![Input::Result {
                request,
                applied: true
            }]
        );
        // A new begin forgets the reset.
        c.reset();
        let (_, inputs) = found_json(&c, PASSWORD_ONLY);
        c.answer(AuthService::Password, query_sequence(&inputs), "old".into());
        assert!(query("New Password: ").is_empty());
    }

    #[test]
    fn only_the_password_mechanism_takes_the_expiry_notice() {
        let c = Conversations::new(AuthPolicy::default());
        found_json(&c, &passkey(true, 8));
        c.on_signal(&Signal::Info {
            service: s(SWITCHABLE_SERVICE),
            text: s("Password expired. Change your password now."),
        });
        assert!(c
            .on_signal(&Signal::SecretInfoQuery {
                service: s(SWITCHABLE_SERVICE),
                query: s("New Password: ")
            })
            .is_empty());
    }

    #[test]
    fn sssd_web_login_follows_priority_and_replies_json() {
        let c = Conversations::new(AuthPolicy::default());
        let (request, inputs) = found(&c);
        let (list, selected) = c.mechanisms();
        assert_eq!(list.len(), 3);
        assert_eq!(selected.unwrap().role, Role::WebLogin, "SSSD's priority");
        assert!(matches!(&inputs[0], Input::Timer { secs: 300, .. }));
        let Input::Ask {
            sequence,
            ask: Ask::Button { label },
            ..
        } = &inputs[1]
        else {
            panic!("{inputs:?}")
        };
        assert_eq!(label, "Log In");
        assert_eq!(
            c.answer(AuthService::Password, *sequence, String::new()),
            Some(request)
        );
        assert!(c.take_calls().is_empty(), "the button is local");
        let inputs = c.take_inputs();
        let Input::Ask {
            sequence,
            ask: Ask::WebLogin { message, url, code },
            ..
        } = &inputs[0]
        else {
            panic!("{inputs:?}")
        };
        assert_eq!(
            (message.as_str(), url.as_str(), code.as_deref()),
            (
                "Log in online with another device",
                "https://idp.example/device",
                Some("1234-5678")
            )
        );
        // Back from the dialog returns to the button.
        assert!(c.cancel_requested());
        assert!(matches!(
            c.take_inputs()[0],
            Input::Ask {
                ask: Ask::Button { .. },
                ..
            }
        ));
        assert_eq!(
            c.answer(AuthService::Password, *sequence, String::new()),
            None,
            "the old dialog's step is gone"
        );
    }

    #[test]
    fn sssd_web_login_done_and_timeout() {
        let c = Conversations::new(AuthPolicy::default());
        let (request, inputs) = found(&c);
        let Input::Timer { token, .. } = inputs[0] else {
            panic!()
        };
        let Input::Ask { sequence, .. } = inputs[1] else {
            panic!()
        };
        c.answer(AuthService::Password, sequence, String::new());
        let Input::Ask { sequence, .. } = c.take_inputs()[0].clone() else {
            panic!()
        };
        assert_eq!(
            c.answer(AuthService::Password, sequence, String::new()),
            Some(request)
        );
        let calls = c.take_calls();
        let [Call::JsonReply { service, json }] = calls.as_slice() else {
            panic!("{calls:?}")
        };
        assert_eq!(service, SWITCHABLE_SERVICE);
        let reply: Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            reply,
            json!({"authSelection": {"status": "Ok", "eidp": {}}})
        );
        // Waiting for the outcome, Back resets softly.
        assert!(c.cancel_requested());
        assert_eq!(c.take_inputs(), vec![Input::Restart]);
        // The timeout fired after Done: it was cleared.
        c.timeout(token);
        assert!(c.take_inputs().is_empty());
        assert_eq!(
            c.on_signal(&Signal::VerificationComplete {
                service: s(SWITCHABLE_SERVICE)
            }),
            vec![Input::Result {
                request,
                applied: true
            }]
        );
    }

    #[test]
    fn sssd_smartcard_lists_certificates_then_asks_the_pin() {
        let c = Conversations::new(AuthPolicy::default());
        found(&c);
        let (list, _) = c.mechanisms();
        let smartcard = list.iter().find(|m| m.role == Role::Smartcard).unwrap();
        assert!(c.select(smartcard));
        let inputs = c.take_inputs();
        let Input::Ask {
            sequence,
            ask: Ask::Choices { prompt, choices },
            ..
        } = &inputs[0]
        else {
            panic!("{inputs:?}")
        };
        assert_eq!(prompt, "Select Identity");
        assert_eq!(choices[0].title, "Ada Lovelace");
        assert_eq!(
            choices[0].subtitle.as_deref(),
            Some("Certificate for PIV — Example")
        );
        c.answer(AuthService::Password, *sequence, "k2".into());
        let pin = query_sequence(&c.take_inputs());
        c.answer(AuthService::Password, pin, "0000".into());
        let calls = c.take_calls();
        let [Call::JsonReply { json, .. }] = calls.as_slice() else {
            panic!("{calls:?}")
        };
        let reply: Value = serde_json::from_str(json).unwrap();
        assert_eq!(reply["authSelection"]["smartcard:1"]["keyId"], "k2");
        assert_eq!(reply["authSelection"]["smartcard:1"]["pin"], "0000");
        assert!(!format!("{:?}", calls[0]).contains("0000"));
    }

    #[test]
    fn a_soft_reset_keeps_the_switchable_mechanism() {
        let c = Conversations::new(AuthPolicy::default());
        let (request, _) = found(&c);
        let (list, _) = c.mechanisms();
        let password = list.iter().find(|m| m.role == Role::Password).unwrap();
        assert!(c.select(password));
        c.take_inputs();
        assert_eq!(c.cancel(), Some(request));
        c.on_signal(&Signal::ConversationStopped {
            service: s(SWITCHABLE_SERVICE),
        });
        c.begin(AuthService::Password).unwrap();
        c.take_calls();
        c.on_signal(&Signal::JsonRequest {
            service: s(SWITCHABLE_SERVICE),
            protocol: s("auth-mechanisms"),
            version: 1,
            json: s(SELECTION),
        });
        assert_eq!(c.mechanisms().1.unwrap().role, Role::Password);
    }

    #[test]
    fn legacy_choice_lists_answer_with_select_choice() {
        let c = Conversations::new(legacy());
        begun(&c);
        let inputs = c.on_signal(&Signal::ChoiceQuery {
            service: s(PASSWORD_SERVICE),
            prompt: s("Pick"),
            choices: vec![(s("a"), s("Alpha")), (s("b"), s("Beta"))],
        });
        let Input::Ask {
            sequence,
            ask: Ask::Choices { choices, .. },
            ..
        } = &inputs[0]
        else {
            panic!("{inputs:?}")
        };
        assert_eq!(choices.len(), 2);
        c.answer(AuthService::Password, *sequence, "b".into());
        assert_eq!(
            c.take_calls(),
            vec![Call::SelectChoice {
                service: s(PASSWORD_SERVICE),
                choice: s("b")
            }]
        );
    }

    #[test]
    fn password_policy_leaves_smartcard_without_a_username() {
        let c = Conversations::new(AuthPolicy {
            password: false,
            ..legacy()
        });
        assert!(!c.needs_username());
        let (list, _) = c.mechanisms();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].role, Role::Smartcard);
        let first = c.begin(AuthService::Password).unwrap();
        c.take_inputs();
        assert!(c.select(&c.default_mechanism().unwrap()));
        c.take_inputs();
        assert_eq!(c.cancel(), Some(first));
        c.take_inputs();
        c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls().last(),
            Some(&Call::BeginVerification {
                service: s(SMARTCARD_SERVICE)
            })
        );
        assert!(!c.can_retry(), "no user: done after one failure");
        // With switchable on, a user is needed again.
        c.set_policy(AuthPolicy {
            password: false,
            ..AuthPolicy::default()
        });
        assert!(c.needs_username());
    }

    #[test]
    fn smartcard_changes_restart_only_the_smartcard_mechanism() {
        let c = Conversations::new(legacy());
        begun(&c);
        c.smartcard_changed(true);
        assert!(c.take_inputs().is_empty(), "password is selected");
        let smartcard = c
            .mechanisms()
            .0
            .into_iter()
            .find(|m| m.role == Role::Smartcard)
            .unwrap();
        assert!(c.select(&smartcard));
        c.take_inputs();
        c.smartcard_changed(true);
        assert_eq!(c.take_inputs(), vec![Input::Restart]);
        // A PIN being checked keeps a card that is still in.
        c.cancel();
        c.on_signal(&Signal::ConversationStopped {
            service: s(PASSWORD_SERVICE),
        });
        c.begin(AuthService::Password).unwrap();
        c.take_inputs();
        let pin = query_sequence(&c.on_signal(&Signal::SecretInfoQuery {
            service: s(SMARTCARD_SERVICE),
            query: s("PIN for card: "),
        }));
        c.answer(AuthService::Password, pin, "1234".into());
        c.smartcard_changed(true);
        assert!(c.take_inputs().is_empty());
        c.smartcard_changed(false);
        assert_eq!(c.take_inputs(), vec![Input::Restart]);
    }
}
