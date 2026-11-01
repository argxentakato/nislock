// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Model: locker-specific session state shown on screen.
//!
//! The text-entry state machine (buffer, focus, layers) is the reusable
//! `nislock_kbd::Keyboard` component; this module holds only the state that
//! belongs to the locker around it: who is being authenticated and what
//! feedback the screen shows.

use nislock_kbd::StatusKind;

/// Screen-visible model state (formerly `ScreenTexts`).
pub struct LockModel {
    /// The account being authenticated (also rendered on the lock screen).
    pub user: String,
    /// Status line shown under the password field.
    pub status: String,
    /// Severity of the status line, used for color by the View.
    pub status_kind: StatusKind,
}

impl LockModel {
    pub fn new(user: String) -> Self {
        Self {
            user,
            status: String::new(),
            status_kind: StatusKind::Idle,
        }
    }

    pub fn set_status(&mut self, status: impl Into<String>, kind: StatusKind) {
        self.status = status.into();
        self.status_kind = kind;
    }
}
