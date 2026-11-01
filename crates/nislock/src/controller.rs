// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Controller: turns raw widget effects into Model changes and commands for
//! the application shell. It knows neither Wayland nor PAM concretely — the
//! authenticator is injected, and outputs are expressed as [`ControlEffect`]s
//! the shell acts on.

use nislock_kbd::{Effect, Event as KbdEvent, Keyboard, StatusKind};

use crate::auth::Authenticator;
use crate::model::LockModel;

/// Commands the shell (Wayland app) must carry out after a controller call.
pub enum ControlEffect {
    /// Nothing to do.
    None,
    /// The lock surfaces must be repainted.
    Redraw,
    /// Authentication succeeded: send unlock and exit with this status code.
    /// (Exiting while locked without unlock makes the compositor report a crash.)
    UnlockExit(i32),
}

pub struct LockController {
    auth: Box<dyn Authenticator>,
}

impl LockController {
    pub fn new(auth: Box<dyn Authenticator>) -> Self {
        Self { auth }
    }

    /// Feed one widget event through the keyboard model, then apply the
    /// resulting policy (auth on submit, buffer reset on cancel).
    pub fn handle_event(
        &self,
        ev: KbdEvent,
        keyboard: &mut Keyboard,
        model: &mut LockModel,
    ) -> ControlEffect {
        match keyboard.handle(ev) {
            Effect::Noop => ControlEffect::None,
            Effect::Redraw => ControlEffect::Redraw,
            // Exiting while locked without unlock makes the compositor report a crash.
            Effect::Cancel => {
                keyboard.clear();
                ControlEffect::Redraw
            }
            Effect::Submit(password) => match self.auth.verify(&model.user, &password) {
                Ok(true) => {
                    model.set_status("welcome!", StatusKind::Info);
                    ControlEffect::UnlockExit(0)
                }
                // Backend errors (already logged) and rejections look the same
                // to the user: clear the field and stay locked.
                _ => {
                    model.set_status("wrong password", StatusKind::Error);
                    keyboard.clear();
                    ControlEffect::Redraw
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use zeroize::Zeroizing;

    use super::*;
    use crate::auth::AuthError;

    /// Records every password it was asked to verify, so tests can observe
    /// (indirectly) what the keyboard buffer contained at submit time.
    struct RecordingAuth {
        accepts: bool,
        seen: Rc<RefCell<Vec<String>>>,
    }

    impl RecordingAuth {
        fn new(accepts: bool) -> Self {
            Self {
                accepts,
                seen: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl Authenticator for RecordingAuth {
        fn verify(&self, _user: &str, password: &Zeroizing<String>) -> Result<bool, AuthError> {
            self.seen.borrow_mut().push(password.to_string());
            Ok(self.accepts)
        }
    }

    #[test]
    fn accepted_password_requests_unlock_exit() {
        let c = LockController::new(Box::new(RecordingAuth::new(true)));
        let mut kb = Keyboard::new();
        let mut model = LockModel::new("deck".into());
        kb.handle(KbdEvent::TextInput('a'));
        let effect = c.handle_event(KbdEvent::Submit, &mut kb, &mut model);
        assert!(matches!(effect, ControlEffect::UnlockExit(0)));
        assert_eq!(model.status, "welcome!");
        assert_eq!(model.status_kind, StatusKind::Info);
    }

    #[test]
    fn rejected_password_receives_submitted_text_then_field_is_cleared() {
        let auth = RecordingAuth::new(false);
        let seen = auth.seen.clone();
        let c = LockController::new(Box::new(auth));
        let mut kb = Keyboard::new();
        let mut model = LockModel::new("deck".into());
        kb.handle(KbdEvent::TextInput('a'));
        let effect = c.handle_event(KbdEvent::Submit, &mut kb, &mut model);
        assert!(matches!(effect, ControlEffect::Redraw));
        assert_eq!(model.status, "wrong password");
        assert_eq!(model.status_kind, StatusKind::Error);
        assert!(matches!(
            c.handle_event(KbdEvent::Submit, &mut kb, &mut model),
            ControlEffect::None
        ));
        assert_eq!(*seen.borrow(), vec!["a".to_string()]);
    }

    #[test]
    fn cancel_clears_the_field() {
        let auth = RecordingAuth::new(false);
        let seen = auth.seen.clone();
        let c = LockController::new(Box::new(auth));
        let mut kb = Keyboard::new();
        let mut model = LockModel::new("deck".into());
        kb.handle(KbdEvent::TextInput('a'));
        let effect = c.handle_event(KbdEvent::Cancel, &mut kb, &mut model);
        assert!(matches!(effect, ControlEffect::Redraw));
        assert!(matches!(
            c.handle_event(KbdEvent::Submit, &mut kb, &mut model),
            ControlEffect::None
        ));
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn press_maps_to_redraw() {
        let c = LockController::new(Box::new(RecordingAuth::new(false)));
        let mut kb = Keyboard::new();
        let mut model = LockModel::new("deck".into());
        let effect = c.handle_event(KbdEvent::Press, &mut kb, &mut model);
        assert!(matches!(effect, ControlEffect::Redraw));
        let effect = c.handle_event(KbdEvent::TextInput('x'), &mut kb, &mut model);
        assert!(matches!(effect, ControlEffect::Redraw));
    }
}
