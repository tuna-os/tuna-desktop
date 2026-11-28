//! GDM's channels and the order of calls on them.
//!
//! The greeter's own channel (`Manager.OpenSession`) carries the Greeter
//! interface and, for users without a running session, the
//! conversations. As userVerifier.js `_getUserVerifierProxies` does, every
//! begin for a named user first asks GDM for that user's
//! re-authentication channel (`OpenReauthenticationChannel`): GDM grants
//! it only when the user's session runs, and then the conversation runs
//! there and its success makes GDM switch to that session (user
//! switching); `AccessDenied` sends the conversation to the greeter's own
//! channel. Calls go out one at a time, in order, each on the channel of
//! the begin it belongs to.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use crate::gdm::{self, Call};
use crate::{trace, Greeter, GREETER};

pub struct Channel {
    pub id: u64,
    pub connection: gio::DBusConnection,
    _signals: gdm::Subscription,
}

#[derive(Default)]
pub struct Channels {
    pub main: RefCell<Option<Rc<Channel>>>,
    /// The channel of the latest begin.
    verifier: RefCell<Option<Rc<Channel>>>,
    queue: RefCell<Queue>,
    next_id: Cell<u64>,
    /// Begins that went to a user's re-authentication channel.
    reauth: RefCell<HashSet<u64>>,
}

impl Channels {
    pub fn is_reauth(&self, request: u64) -> bool {
        self.reauth.borrow().contains(&request)
    }

    fn main_id(&self) -> Option<u64> {
        self.main.borrow().as_ref().map(|m| m.id)
    }
}

/// GDM calls waiting to go out: one at a time, oldest first, each begin
/// with its request.
#[derive(Default)]
struct Queue {
    calls: VecDeque<(Call, Option<u64>)>,
    pumping: bool,
}

impl Queue {
    /// Queue a call; whether a pump must start (none is running).
    fn push(&mut self, call: Call, request: Option<u64>) -> bool {
        self.calls.push_back((call, request));
        !std::mem::replace(&mut self.pumping, true)
    }

    /// The next call to make; `None` ends the pump.
    fn pop(&mut self) -> Option<(Call, Option<u64>)> {
        let next = self.calls.pop_front();
        if next.is_none() {
            self.pumping = false;
        }
        next
    }
}

/// The channel a call runs on (`_getUserVerifierProxies`).
#[derive(Debug, PartialEq, Eq)]
enum Route<'a> {
    /// A begin for a named user: that user's re-authentication channel
    /// when GDM grants one, else the greeter's.
    UserVerifier(&'a str),
    /// The greeter's own channel.
    Greeter,
    /// The latest begin's channel (else the greeter's).
    Verifier,
}

fn route(call: &Call) -> Route<'_> {
    match call {
        Call::BeginVerificationForUser { user, .. } => Route::UserVerifier(user),
        // No user: no session to switch to.
        Call::BeginVerification { .. } => Route::Greeter,
        call if call.is_verifier() => Route::Verifier,
        _ => Route::Greeter,
    }
}

/// `OpenReauthenticationChannel`'s answer for a begin: the address of
/// the user's channel; `None` to verify on the greeter's own (GDM's
/// `AccessDenied`: the user has no running session); any other error
/// fails the begin.
fn reauthentication(answer: Result<String, glib::Error>) -> Result<Option<String>, glib::Error> {
    match answer {
        Ok(address) => Ok(Some(address)),
        Err(error) if gdm::access_denied(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The channel a begin on `new` leaves behind when it replaces the `old`
/// verifier: a re-authentication channel no begin uses any more (libgdm
/// drops its proxy and closes it), never the greeter's own.
fn left_behind(old: Option<u64>, new: u64, main: Option<u64>) -> Option<u64> {
    old.filter(|&old| old != new && Some(old) != main)
}

/// What follows a call's reply.
#[derive(Debug, PartialEq, Eq)]
enum After {
    Nothing,
    /// GDM forgets the selected session on every reset: a begin on the
    /// greeter's channel selects the chosen one again.
    SelectSession(String),
    /// The begin of this request failed.
    FailBegin(u64),
}

fn after_reply(ok: bool, begin: Option<u64>, on_main: bool, chosen: Option<String>) -> After {
    match (ok, begin) {
        (true, Some(_)) if on_main => chosen.map_or(After::Nothing, After::SelectSession),
        (false, Some(request)) => After::FailBegin(request),
        _ => After::Nothing,
    }
}

/// What a closed channel was to the greeter.
#[derive(Debug, PartialEq, Eq)]
struct Closed {
    /// The greeter's own channel: asked for again.
    main: bool,
    /// The latest begin's: forgotten.
    verifier: bool,
}

fn closed(id: u64, main: Option<u64>, verifier: Option<u64>) -> Closed {
    Closed {
        main: main == Some(id),
        verifier: verifier == Some(id),
    }
}

impl Greeter {
    /// Subscribe to a new channel's signals and hear when it closes.
    pub(crate) fn attach(self: &Rc<Self>, connection: gio::DBusConnection) -> Rc<Channel> {
        let id = self.channels.next_id.get() + 1;
        self.channels.next_id.set(id);
        let weak = Rc::downgrade(self);
        let signals = gdm::subscribe(&connection, move |signal| {
            if let Some(greeter) = weak.upgrade() {
                greeter.on_signal(signal);
            }
        });
        // `closed` may be emitted, and its handler dropped, off this
        // thread: the handler only hands the news to the main context.
        connection.connect_closed(move |_, _, _| {
            glib::MainContext::default().invoke(move || {
                if let Some(greeter) = GREETER.with(|g| g.borrow().clone()) {
                    greeter.on_channel_closed(id);
                }
            });
        });
        Rc::new(Channel {
            id,
            connection,
            _signals: signals,
        })
    }

    /// Queue one call; calls go out in order.
    pub(crate) fn send(self: &Rc<Self>, call: Call) {
        self.send_for(call, None);
    }

    pub(crate) fn send_for(self: &Rc<Self>, call: Call, request: Option<u64>) {
        match &call {
            Call::AnswerQuery { service, .. } => {
                trace(&format!("greeter answer service={service}"))
            }
            Call::JsonReply { service, .. } => trace(&format!("greeter reply service={service}")),
            other => trace(&format!("greeter call {other:?}")),
        }
        if !self.channels.queue.borrow_mut().push(call, request) {
            return;
        }
        let greeter = self.clone();
        glib::spawn_future_local(async move {
            loop {
                let next = greeter.channels.queue.borrow_mut().pop();
                let Some((call, request)) = next else {
                    break;
                };
                greeter.run(call, request).await;
            }
        });
    }

    async fn run(self: &Rc<Self>, call: Call, request: Option<u64>) {
        let main = self.channels.main.borrow().clone();
        let channel = match route(&call) {
            Route::UserVerifier(user) => match self.verifier_for(user).await {
                Ok(channel) => Some(channel),
                Err(error) => {
                    eprintln!(
                        "tuna-greeter: OpenReauthenticationChannel: {}",
                        error.message()
                    );
                    None
                }
            },
            Route::Greeter => main,
            Route::Verifier => self.channels.verifier.borrow().clone().or(main),
        };
        let is_begin = matches!(
            call,
            Call::BeginVerificationForUser { .. } | Call::BeginVerification { .. }
        );
        let begin = request.filter(|_| is_begin);
        let Some(channel) = channel else {
            if let Some(request) = begin {
                self.fail_begin(request);
            }
            return;
        };
        let on_main = self.channels.main_id() == Some(channel.id);
        if is_begin {
            self.use_verifier(channel.clone());
        }
        if let Some(request) = begin {
            self.host.set_channel(request, channel.id);
            if !on_main {
                self.channels.reauth.borrow_mut().insert(request);
            }
        }
        let (interface, method, args) = call.wire();
        let result = channel
            .connection
            .call_future(
                None,
                gdm::SESSION_PATH,
                interface,
                method,
                Some(&args),
                None,
                gio::DBusCallFlags::NONE,
                -1,
            )
            .await;
        if let Err(error) = &result {
            eprintln!("tuna-greeter: {method}: {}", error.message());
        }
        let chosen = self.chosen_session.borrow().clone();
        match after_reply(result.is_ok(), begin, on_main, chosen) {
            After::Nothing => {}
            After::SelectSession(session) => self.send(Call::SelectSession { session }),
            After::FailBegin(request) => self.fail_begin(request),
        }
    }

    /// The channel a begin for `user` runs on (`_getUserVerifierProxies`).
    async fn verifier_for(self: &Rc<Self>, user: &str) -> Result<Rc<Channel>, glib::Error> {
        let main = self.channels.main.borrow().clone();
        let system = self.system.borrow().clone();
        let (Some(main), Some(system)) = (main, system) else {
            return Err(glib::Error::new(
                gio::IOErrorEnum::NotConnected,
                "no channel to GDM",
            ));
        };
        let Some(address) =
            reauthentication(gdm::open_reauthentication_channel(&system, user).await)?
        else {
            trace(&format!("greeter verifier user={user} channel=greeter"));
            return Ok(main);
        };
        let connection = gdm::connect(&address).await?;
        let channel = self.attach(connection);
        // libgdm enables the extensions when it makes the channel's
        // UserVerifier proxy.
        let (interface, method, args) = Call::EnableExtensions.wire();
        if let Err(error) = channel
            .connection
            .call_future(
                None,
                gdm::SESSION_PATH,
                interface,
                method,
                Some(&args),
                None,
                gio::DBusCallFlags::NONE,
                -1,
            )
            .await
        {
            eprintln!("tuna-greeter: EnableExtensions: {}", error.message());
        }
        trace(&format!(
            "greeter verifier user={user} channel=reauthentication"
        ));
        Ok(channel)
    }

    /// The latest begin's channel; a replaced re-authentication channel
    /// is closed (libgdm drops its proxy).
    fn use_verifier(self: &Rc<Self>, channel: Rc<Channel>) {
        let main = self.channels.main_id();
        let old = self.channels.verifier.replace(Some(channel.clone()));
        if let Some(old) = old.filter(|o| left_behind(Some(o.id), channel.id, main).is_some()) {
            old.connection.close(None::<&gio::Cancellable>, |_| {});
        }
    }

    pub(crate) fn fail_begin(self: &Rc<Self>, request: u64) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(greeter) = weak.upgrade() {
                let inputs = greeter.host.begin_failed(request);
                greeter.feed(inputs);
            }
        });
    }

    /// A channel closed: its owed begins are denied; the greeter's own
    /// channel is asked for again.
    pub(crate) fn on_channel_closed(self: &Rc<Self>, id: u64) {
        let verifier = self.channels.verifier.borrow().as_ref().map(|v| v.id);
        let was = closed(id, self.channels.main_id(), verifier);
        if was.verifier {
            self.channels.verifier.borrow_mut().take();
        }
        if !was.main {
            trace("greeter reauthentication-channel-closed");
            let inputs = self.host.channel_closed(id, false);
            self.feed(inputs);
            return;
        }
        trace("greeter gdm-closed");
        self.channels.main.borrow_mut().take();
        let inputs = self.host.channel_closed(id, true);
        self.feed(inputs);
        self.on_gdm_closed();
        self.connect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(user: &str) -> Call {
        Call::BeginVerificationForUser {
            service: "gdm-password".into(),
            user: user.into(),
        }
    }

    #[test]
    fn calls_go_out_one_at_a_time_in_order() {
        let mut queue = Queue::default();
        assert!(queue.push(begin("mary"), Some(1)), "the first call pumps");
        assert!(!queue.push(Call::Cancel, None), "one pump at a time");
        assert!(!queue.push(begin("ada"), Some(2)));
        assert_eq!(queue.pop(), Some((begin("mary"), Some(1))));
        assert!(
            !queue.push(Call::GetTimedLoginDetails, None),
            "queued while the pump runs"
        );
        assert_eq!(queue.pop(), Some((Call::Cancel, None)));
        assert_eq!(queue.pop(), Some((begin("ada"), Some(2))));
        assert_eq!(queue.pop(), Some((Call::GetTimedLoginDetails, None)));
        assert_eq!(queue.pop(), None);
        assert!(queue.push(Call::Cancel, None), "an ended pump starts again");
    }

    #[test]
    fn begins_for_a_user_ask_for_the_users_channel_and_the_rest_follow() {
        assert_eq!(route(&begin("mary")), Route::UserVerifier("mary"));
        assert_eq!(
            route(&Call::BeginVerification {
                service: "gdm-smartcard".into()
            }),
            Route::Greeter
        );
        for call in [
            Call::AnswerQuery {
                service: "gdm-password".into(),
                answer: "x".into(),
            },
            Call::SelectChoice {
                service: "gdm-smartcard".into(),
                choice: "k".into(),
            },
            Call::JsonReply {
                service: "gdm-switchable-auth".into(),
                json: "{}".into(),
            },
            Call::Cancel,
        ] {
            assert_eq!(route(&call), Route::Verifier, "{call:?}");
        }
        for call in [
            Call::SelectSession {
                session: "tuna".into(),
            },
            Call::StartSessionWhenReady {
                service: "gdm-password".into(),
            },
            Call::GetTimedLoginDetails,
            Call::BeginAutoLogin { user: "ada".into() },
            Call::StopConflictingSession,
        ] {
            assert_eq!(route(&call), Route::Greeter, "{call:?}");
        }
    }

    #[test]
    fn access_denied_verifies_on_the_greeters_channel() {
        assert_eq!(
            reauthentication(Ok("unix:path=/run/gdm/dbus/x".into())).unwrap(),
            Some("unix:path=/run/gdm/dbus/x".into())
        );
        let denied = gio::DBusError::new_for_dbus_error(
            "org.freedesktop.DBus.Error.AccessDenied",
            "Login screen only allowed to open reauthentication channels for running sessions",
        );
        assert_eq!(reauthentication(Err(denied)).unwrap(), None);
        let broken = gio::DBusError::new_for_dbus_error(
            "org.freedesktop.DBus.Error.NoReply",
            "Did not receive a reply",
        );
        assert!(
            reauthentication(Err(broken)).is_err(),
            "other errors fail the begin"
        );
    }

    #[test]
    fn a_replaced_reauthentication_channel_is_closed_but_never_the_greeters() {
        let main = Some(1);
        assert_eq!(left_behind(None, 1, main), None);
        assert_eq!(left_behind(Some(1), 5, main), None, "the greeter's own");
        assert_eq!(left_behind(Some(5), 5, main), None, "still in use");
        assert_eq!(left_behind(Some(5), 1, main), Some(5));
        assert_eq!(left_behind(Some(5), 6, main), Some(5));
        assert_eq!(left_behind(Some(5), 6, None), Some(5));
    }

    #[test]
    fn begins_on_the_greeters_channel_select_the_session_again() {
        let chosen = || Some("gnome".to_owned());
        assert_eq!(
            after_reply(true, Some(3), true, chosen()),
            After::SelectSession("gnome".into())
        );
        assert_eq!(after_reply(true, Some(3), true, None), After::Nothing);
        assert_eq!(
            after_reply(true, Some(3), false, chosen()),
            After::Nothing,
            "a re-authentication channel has no Greeter interface"
        );
        assert_eq!(
            after_reply(true, None, true, chosen()),
            After::Nothing,
            "only begins"
        );
        assert_eq!(
            after_reply(false, Some(3), false, chosen()),
            After::FailBegin(3)
        );
        assert_eq!(after_reply(false, None, true, chosen()), After::Nothing);
    }

    #[test]
    fn a_closed_channel_is_the_greeters_or_a_users() {
        assert_eq!(
            closed(1, Some(1), Some(1)),
            Closed {
                main: true,
                verifier: true
            }
        );
        assert_eq!(
            closed(4, Some(1), Some(4)),
            Closed {
                main: false,
                verifier: true
            }
        );
        assert_eq!(
            closed(3, Some(1), Some(4)),
            Closed {
                main: false,
                verifier: false
            },
            "an older user's channel"
        );
        assert_eq!(
            closed(1, None, None),
            Closed {
                main: false,
                verifier: false
            },
            "already forgotten"
        );
    }
}
