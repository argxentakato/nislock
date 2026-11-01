// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Authentication port (Controller-side): the Controller depends only on the
//! [`Authenticator`] trait, so lock-policy tests can substitute a fake and
//! the PAM details stay behind one implementation.

use thiserror::Error;
use zeroize::Zeroizing;

/// Failure opening or driving the authentication backend.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The backend itself failed (service misconfiguration, PAM stack error).
    /// Note this is distinct from a password being *rejected*, which is an
    /// ordinary `Ok(false)` result.
    #[error(transparent)]
    Pam(#[from] pam::PamError),
}

/// Verification abstraction used by [`crate::controller::LockController`].
pub trait Authenticator {
    /// `Ok(true)` accepts the password; `Ok(false)` is a normal rejection.
    /// `Err` is a backend failure: callers should treat it as a rejection for
    /// policy purposes.
    fn verify(&self, user: &str, password: &Zeroizing<String>) -> Result<bool, AuthError>;
}

/// The PAM service consulted on submit. Intentionally pinned rather than
/// configurable: the service defines which policy applies to the session
/// owner, so it is part of the locker's security profile, not user setup.
const PAM_SERVICE: &str = "login";

/// PAM-backed authenticator (the production implementation).
#[derive(Default)]
pub struct PamAuthenticator;

impl PamAuthenticator {
    pub fn new() -> Self {
        Self
    }
}

impl Authenticator for PamAuthenticator {
    fn verify(&self, user: &str, password: &Zeroizing<String>) -> Result<bool, AuthError> {
        let mut auth = match pam::Authenticator::with_password(PAM_SERVICE) {
            Ok(auth) => auth,
            Err(e) => {
                // Historical behavior: opening the service fails loudly, then
                // the submit is treated as a rejection by the controller.
                eprintln!("nislock: cannot open PAM service {PAM_SERVICE}: {e}");
                return Err(AuthError::Pam(e));
            }
        };
        auth.get_handler().set_credentials(user, password.as_str());
        // Historical behavior: any authenticate() error (including a wrong
        // password) is a rejection, not an Err.
        Ok(auth.authenticate().is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::PAM_SERVICE;

    #[test]
    fn pam_service_is_pinned_to_the_system_login_policy() {
        assert_eq!(PAM_SERVICE, "login");
    }
}
