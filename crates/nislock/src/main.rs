// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! nislock — ext-session-lock-v1 locker driven by the Steam Deck gamepad,
//! with any physical Wayland keyboard typing directly into the field.
//!
//! Behavior (unchanged across the refactor): the D-pad moves a cursor over an
//! on-screen keyboard, A pushes a key into the password buffer, B pops, Start
//! submits (L4/R4 hold-to-run keys), and a correct password authenticates via
//! PAM then unlock-and-exits with code 0. A connected physical keyboard adds
//! characters directly while a lock surface has focus (see [`app`]'s
//! `physical_key_events`); it never moves the on-screen cursor. If the
//! compositor ends the lock, we exit 1; startup/protocol failures print a
//! rendered report and exit FAILURE.
//! SIGUSR1 requests an unlock-and-exit, like swaylock/hyprlock.
//! The authenticated account comes from the real UID, and the PAM service
//! is fixed; neither can be selected through a configuration file or an
//! environment variable.
//!
//! This binary is only the entry point: bootstrap/run live in [`app`], and
//! the MVC modules ([`controller`], [`model`], [`view`], plus the PAM
//! [`auth`] wrapper) carry the actual implementation.

mod app;
mod auth;
mod config;
mod controller;
mod error;
mod model;
mod view;

use std::process::ExitCode;

use crate::error::NisError;

fn run() -> Result<i32, NisError> {
    let config = config::Config::load()?;
    let mut runtime = app::bootstrap(&config)?;
    runtime.run()
}

fn main() -> ExitCode {
    match run() {
        // 0 = unlocked by the user, 1 = the compositor finished the lock.
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            // The only use of miette in the workspace: the human-readable
            // terminal report, with the cause chain behind the top-level
            // message.
            eprintln!("{:?}", err.into_diagnostic());
            ExitCode::FAILURE
        }
    }
}
