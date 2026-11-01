// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Library error type for `nislock-input`.
//!
//! Plain `thiserror` type for a library crate; diagnostic rendering is left
//! to the consuming binary.

use std::io;

use thiserror::Error;

/// Failure during device discovery or while streaming Deck input reports.
#[derive(Debug, Error)]
pub enum InputError {
    /// sysfs/devfs access failed (permissions, missing `/sys/class/hidraw`, ...).
    #[error(transparent)]
    Io(#[from] io::Error),
    /// No hidraw node matching the Steam Deck control interface exists.
    #[error("no Steam Deck hidraw control interface found (is the controller on?)")]
    DeviceNotFound,
}
