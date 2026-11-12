//! ext-idle-notify v2 (#1046): clients ask to hear when the seat has been
//! idle for a timeout and when activity resumes (swayidle-style idle
//! daemons, chat presence).
//!
//! Smithay's `IdleNotifierState` arms one calloop timer per notification
//! on a loop whose data is `State`; the runtime's loop belongs to
//! `Runtime`, and its idle clock is the one org.gnome.Mutter.IdleMonitor
//! already keeps ([`crate::idle_monitor`]). So the protocol is served here
//! on that clock instead: the runtime feeds [`IdleNotifier::activity`]
//! wherever it feeds the D-Bus idle monitor (every timestamped input
//! event), calls [`IdleNotifier::tick`] beside `IdleMonitor::tick` with the
//! idle inhibitors the idle lock honours (zwp_idle_inhibit_v1 and
//! org.gnome.SessionManager's idle flag), and sleeps no later than
//! [`IdleNotifier::next_deadline`].
//!
//! As the protocol asks, a notification from `get_idle_notification`
//! never idles while an inhibitor is held (one that is idle resumes when
//! an inhibitor appears, and its timeout restarts when the last one goes),
//! while one from `get_input_idle_notification` counts input alone.
//! There is one seat, so the seat argument selects nothing.

use std::time::{Duration, Instant};

use smithay::reexports::{
    wayland_protocols::ext::idle_notify::v1::server::{
        ext_idle_notification_v1::{self, ExtIdleNotificationV1},
        ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
    },
    wayland_server::{
        backend::{ClientId, GlobalId},
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    },
};

use crate::State;

/// ext_idle_notifier_v1's version: v2 adds `get_input_idle_notification`.
const VERSION: u32 = 2;

/// One live ext_idle_notification_v1.
#[derive(Debug)]
struct Notification {
    resource: ExtIdleNotificationV1,
    timeout: Duration,
    /// Created by `get_input_idle_notification`: idle inhibitors do not
    /// hold it back.
    input_only: bool,
    /// Start of the stretch its timeout counts from: creation, the last
    /// activity, or the end of an inhibition.
    since: Instant,
    /// `idled` was sent and `resumed` has not followed yet.
    idle: bool,
}

impl Notification {
    /// Whether an idle inhibitor holds this notification back.
    fn held(&self, inhibited: bool) -> bool {
        inhibited && !self.input_only
    }

    fn resume(&mut self) {
        if self.idle {
            self.resource.resumed();
            self.idle = false;
        }
    }
}

/// The ext_idle_notifier_v1 global and every notification created on it.
#[derive(Debug)]
pub(crate) struct IdleNotifier {
    _global: GlobalId,
    notifications: Vec<Notification>,
    /// Whether an idle inhibitor was held at the last tick.
    inhibited: bool,
}

impl IdleNotifier {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, ExtIdleNotifierV1, ()>(VERSION, ()),
            notifications: Vec::new(),
            inhibited: false,
        }
    }

    /// Seat activity at `now`: idle notifications resume and every
    /// timeout restarts.
    pub(crate) fn activity(&mut self, now: Instant) {
        for notification in &mut self.notifications {
            notification.resume();
            notification.since = now;
        }
    }

    /// Fire the notifications whose timeout has passed by `now`, with
    /// `inhibited` telling whether an idle inhibitor is held.
    pub(crate) fn tick(&mut self, now: Instant, inhibited: bool) {
        if inhibited != self.inhibited {
            self.inhibited = inhibited;
            for notification in self.notifications.iter_mut().filter(|n| !n.input_only) {
                if inhibited {
                    notification.resume();
                } else {
                    notification.since = now;
                }
            }
        }
        for notification in &mut self.notifications {
            if !notification.idle
                && !notification.held(inhibited)
                && now.saturating_duration_since(notification.since) >= notification.timeout
            {
                notification.resource.idled();
                notification.idle = true;
            }
        }
    }

    /// When [`tick`](Self::tick) next has a notification to fire.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.notifications
            .iter()
            .filter(|n| !n.idle && !n.held(self.inhibited))
            .map(|n| n.since + n.timeout)
            .min()
    }
}

impl GlobalDispatch<ExtIdleNotifierV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtIdleNotifierV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ExtIdleNotifierV1,
        request: ext_idle_notifier_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let (id, timeout, input_only) = match request {
            ext_idle_notifier_v1::Request::GetIdleNotification { id, timeout, .. } => {
                (id, timeout, false)
            }
            ext_idle_notifier_v1::Request::GetInputIdleNotification { id, timeout, .. } => {
                (id, timeout, true)
            }
            _ => return,
        };
        let resource = data_init.init(id, ());
        state
            .protocols
            .idle_notifier
            .notifications
            .push(Notification {
                resource,
                timeout: Duration::from_millis(u64::from(timeout)),
                input_only,
                since: Instant::now(),
                idle: false,
            });
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtIdleNotificationV1,
        _request: ext_idle_notification_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `destroy` is the only request; `destroyed` drops the entry.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ExtIdleNotificationV1,
        _data: &(),
    ) {
        state
            .protocols
            .idle_notifier
            .notifications
            .retain(|n| n.resource != *resource);
    }
}
