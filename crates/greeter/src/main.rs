// Greeter entry point: builds the login window from the model.
// Every greetd exchange completes on a worker before the next UI step.
use gtk4::prelude::*;
use std::{cell::RefCell, rc::Rc};
use tuna_greeter::{login, session::enumerate_system};

/// Release version stamped at build time: `TUNA_VERSION` (a `vX.Y.Z` tag or
/// plain `X.Y.Z`) wins, otherwise the crate version. Duplicated per binary on
/// purpose — no shared dependency for a few lines.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("TUNA_VERSION"), env!("CARGO_PKG_VERSION"))
}

fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        println!("tuna-greeter {}", release_version());
        return;
    }
    let app = libadwaita::Application::builder()
        .application_id("asia.reilly.tuna.greeter")
        .build();
    let windows = Rc::new(RefCell::new(Vec::<login::LoginWindow>::new()));
    let owner = windows.clone();
    app.connect_activate(move |app| {
        if let Some(window) = owner.borrow().first() {
            window.widgets().window.present();
            return;
        }
        let sessions = enumerate_system();
        let socket = std::env::var("GREETD_SOCK").unwrap_or_default();
        owner
            .borrow_mut()
            .push(login::build_login(app, sessions.entries, socket));
    });
    app.connect_shutdown(move |_| {
        windows.borrow_mut().clear();
    });
    app.run();
}

#[cfg(test)]
mod version_tests {
    use super::normalize_version;

    #[test]
    fn tag_prefix_is_stripped() {
        assert_eq!(normalize_version(Some("v1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn plain_version_is_kept() {
        assert_eq!(normalize_version(Some("1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn missing_or_empty_falls_back() {
        assert_eq!(normalize_version(None, "0.1.0"), "0.1.0");
        assert_eq!(normalize_version(Some(""), "0.1.0"), "0.1.0");
    }
}
