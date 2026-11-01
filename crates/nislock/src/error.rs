// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! The binary's top-level error type.
//!
//! Library crates (`nislock-kbd`, `nislock-input`) report plain `thiserror`
//! types; authentication failures are localized by the Controller and never
//! reach this type. `NisError` collects everything that can stop the locker
//! from starting or serving the lock, and `main` renders it with miette —
//! the one and only place miette is used.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NisError {
    #[error("cannot connect to Wayland compositor")]
    Connect(#[from] wayland_client::ConnectError),

    #[error("registry init failed")]
    Registry(#[from] wayland_client::globals::GlobalError),

    #[error("cannot create event loop")]
    EventLoopStart(#[source] calloop::Error),

    #[error("cannot create signal source")]
    SignalSource(#[source] calloop::Error),

    #[error("cannot register signal source")]
    SignalRegistration(#[source] calloop::Error),

    #[error("no wl_compositor")]
    NoCompositor(#[from] wayland_client::globals::BindError),

    #[error("no wl_shm")]
    NoShm(#[source] wayland_client::globals::BindError),

    #[error("gamepad detection failed")]
    Gamepad(#[from] nislock_input::InputError),

    #[error("cannot register gamepad source")]
    GamepadRegistration(#[source] calloop::Error),

    #[error("cannot register wayland source")]
    WaylandRegistration(#[source] calloop::Error),

    #[error("ext-session-lock-v1 unavailable")]
    SessionLockUnavailable(#[source] smithay_client_toolkit::error::GlobalError),

    /// The user to authenticate against could not be resolved: the real UID
    /// has no passwd entry. There is no environment-variable override and no
    /// silent fallback — the locker refuses to start rather than lock against
    /// a guessed account.
    #[error(
        "cannot determine the user to authenticate: \
         the real UID has no passwd entry"
    )]
    UserUnresolvable,

    /// Kept for backends that construct renderers from custom font data.
    #[error(transparent)]
    Kbd(#[from] nislock_kbd::KbdError),
}

/// miette's `Report` renders a top-level `Diagnostic` and then walks plain
/// `std::error::Error` sources, so only the outermost type needs to be a
/// diagnostic; the wrapped Wayland/calloop/PAM causes stay std-only.
#[derive(Debug)]
struct TopDiagnostic(NisError);

impl std::fmt::Display for TopDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for TopDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl miette::Diagnostic for TopDiagnostic {}

impl NisError {
    /// Adapt for `miette::Report` rendering in `main`.
    pub fn into_diagnostic(self) -> miette::Report {
        miette::Report::new(TopDiagnostic(self))
    }
}
