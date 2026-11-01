// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Library error type for `nislock-kbd`.
//!
//! This crate is a library, so it reports failures with plain `thiserror`
//! types only; human-ready diagnostic rendering (miette etc.) is left to
//! the consuming binary.

use thiserror::Error;

/// Failure producing a usable renderer or widget instance.
#[derive(Debug, Error)]
pub enum KbdError {
    /// Font data could not be parsed (invalid TTF/OTF bytes or unsupported
    /// features). `message` is the backend's diagnostic text.
    #[error("failed to load font data: {message}")]
    FontLoad { message: String },
}
