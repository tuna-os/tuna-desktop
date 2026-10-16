//! Tuna Desktop login greeter: prompt state machine, session enumeration, and
//! the greetd JSON-IPC client. The live GTK login controller
//! renders from [`model::GreeterModel`]; no password-specific flow
//! exists anywhere in this crate.

pub mod client;
pub mod controller;
#[cfg(feature = "gtk-ui")]
pub mod login;
pub mod model;
pub mod session;
pub mod ui;
