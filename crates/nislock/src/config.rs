// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Runtime configuration for the locker (Model-independent inputs).
//!
//! The authenticated account is the one the real UID of this process
//! resolves to (via [`uzers`]) — no environment variables, no overrides:
//! authenticating a different account must not hand someone else's session
//! to whoever knows that other password. The PAM service is likewise
//! pinned by [`crate::auth`]. If the real UID has no account, startup
//! fails — there is no silent fallback.

use crate::error::NisError;

/// Immutable settings resolved once at startup.
pub struct Config {
    /// The account the on-screen keyboard authenticates against.
    pub user: String,
}

impl Config {
    pub fn load() -> Result<Self, NisError> {
        Self::from_user(real_user_name())
    }

    fn from_user(user: Option<String>) -> Result<Self, NisError> {
        Ok(Self {
            user: user.ok_or(NisError::UserUnresolvable)?,
        })
    }
}

/// Login name of the account owning this process, resolved from the real
/// UID instead of any environment variable (`$USER` is a shell convention,
/// not kernel-guaranteed). `None` when the real UID has no passwd entry.
fn real_user_name() -> Option<String> {
    uzers::get_current_username().map(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve() -> Result<String, String> {
        Config::load()
            .map(|config| config.user)
            .map_err(|err| err.to_string())
    }

    fn current_username() -> Option<String> {
        uzers::get_current_username().map(|name| name.to_string_lossy().into_owned())
    }

    #[test]
    fn authenticates_the_real_uid_account() {
        match current_username() {
            Some(name) => assert_eq!(resolve().unwrap(), name),
            None => assert!(resolve().is_err()),
        }
    }

    #[test]
    fn fails_when_real_uid_cannot_be_resolved() {
        assert!(matches!(
            Config::from_user(None),
            Err(NisError::UserUnresolvable)
        ));
    }

    #[test]
    fn historical_environment_variables_stay_ignored() {
        const CHILD_MARKER: &str = "NISLOCK_TEST_ENV_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            match current_username() {
                Some(name) => assert_eq!(resolve().unwrap(), name),
                None => assert!(resolve().is_err()),
            }
            return;
        }

        let invalid_user = format!("{}-nislock-invalid", current_username().unwrap_or_default());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::tests::historical_environment_variables_stay_ignored",
            ])
            .env(CHILD_MARKER, "1")
            .env("NISLOCK_USER", invalid_user)
            .env("NISLOCK_PAM_SERVICE", "not-the-fixed-service")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
