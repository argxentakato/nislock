// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Steam Deck controller → `nislock_kbd::Event` mapping.
//!
//! We read the raw 64-byte input reports from the `hid-steam` control
//! interface's hidraw node (see [`deckhid`]) instead of evdev: while that node
//! is open the kernel forwards reports to us regardless of the `lizard_mode`
//! module parameter, and no other client can consume the events underneath us.

pub mod deckhid;
pub mod error;

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use deckhid::{Frame, buttons};
pub use error::InputError;
pub use nislock_kbd::{Dir, Event};
use nislock_kbd::Dir::{Down, Left, Right, Up};

pub use deckhid::find_deck_hidraw;

const INITIAL_REOPEN_DELAY: Duration = Duration::from_millis(500);
const MAX_REOPEN_DELAY: Duration = Duration::from_secs(5);

/// Button/stick state needed to translate raw reports into widget events.
#[derive(Default)]
pub struct GamepadState {
    prev_buttons: u64, // for rising-edge detection on the button bitmask
    stick_nav: [bool; 4], // edge latch for left/up/right/down from the stick
}

const DEAD: i16 = 5800;

fn nav_slot(dir: Dir) -> usize {
    match dir {
        Left => 0,
        Up => 1,
        Right => 2,
        Down => 3,
    }
}

/// Map one newly-pressed button bit to a widget event, if any.
fn map_button(bit: u64) -> Option<Event> {
    match bit {
        buttons::A => Some(Event::Press),
        buttons::B => Some(Event::Backspace),
        buttons::X => Some(Event::ToggleShift),
        buttons::Y => Some(Event::TextInput(' ')),
        // Bumpers and VIEW switch the keyboard layer; START submits; STEAM cancels.
        buttons::L1 | buttons::VIEW => Some(Event::PrevLayer),
        buttons::R1 => Some(Event::NextLayer),
        buttons::OPTIONS => Some(Event::Submit),
        buttons::STEAM => Some(Event::Cancel),
        // D-pad and the L4/R4 touch buttons drive focus navigation.
        buttons::D_UP | buttons::L4 => Some(Event::Nav(Up)),
        buttons::D_DOWN | buttons::R4 => Some(Event::Nav(Down)),
        buttons::D_LEFT => Some(Event::Nav(Left)),
        buttons::D_RIGHT => Some(Event::Nav(Right)),
        _ => None,
    }
}

impl GamepadState {
    fn set_nav(&mut self, out: &mut Vec<Event>, dir: Dir, active: bool) {
        let slot = nav_slot(dir);
        if active {
            if !self.stick_nav[slot] {
                self.stick_nav[slot] = true;
                out.push(Event::Nav(dir));
            }
        } else {
            self.stick_nav[slot] = false;
        }
    }

    /// Translate one decoded input report; empty vec when nothing to emit.
    pub fn process(&mut self, frame: &Frame) -> Vec<Event> {
        let mut out = Vec::new();
        // Rising edges only: a bit transitions 0 -> 1 between reports.
        let mut pressed = frame.buttons & !self.prev_buttons;
        self.prev_buttons = frame.buttons;
        while pressed != 0 {
            let bit = 1u64 << pressed.trailing_zeros();
            pressed &= !bit;
            if let Some(ev) = map_button(bit) {
                out.push(ev);
            }
        }
        // The left stick drives focus navigation (edge-triggered, dead zone).
        // Raw reports are up-positive; the kernel negates that sign for evdev.
        self.set_nav(&mut out, Left, frame.lthumb_x < -DEAD);
        self.set_nav(&mut out, Right, frame.lthumb_x > DEAD);
        self.set_nav(&mut out, Up, frame.lthumb_y > DEAD);
        self.set_nav(&mut out, Down, frame.lthumb_y < -DEAD);
        out
    }
}

/// Open the Deck control hidraw and stream mapped events to `tx` from a
/// dedicated reader thread. Reports keep flowing while the node is open, and
/// close/reopen by the kernel around us is survived with a bounded backoff.
pub fn spawn_deck_reader(tx: Sender<Event>) -> Result<ReaderHandle, InputError> {
    let path: PathBuf = find_deck_hidraw()?.ok_or(InputError::DeviceNotFound)?;
    let mut file = File::open(&path)?;
    std::thread::Builder::new()
        .name("nislock-gamepad".into())
        .spawn(move || {
            let mut state = GamepadState::default();
            let mut buf = [0u8; deckhid::REPORT_LEN];
            loop {
                match file.read(&mut buf) {
                    Ok(n) if n == buf.len() => {
                        let Some(frame) = deckhid::parse_report(&buf) else {
                            continue; // not an input report packet
                        };
                        for mapped in state.process(&frame) {
                            if tx.send(mapped).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(_) => continue, // short read: discard, alignment is per-read
                    Err(e) => {
                        eprintln!("nislock-input: deck hidraw read failed ({e}); reopening");
                        state = GamepadState::default();
                        file = {
                            let mut delay = INITIAL_REOPEN_DELAY;
                            loop {
                                std::thread::sleep(delay);
                                if let Some(f) = find_deck_hidraw()
                                    .ok()
                                    .flatten()
                                    .and_then(|p| File::open(p).ok())
                                {
                                    break f;
                                }
                                delay = (delay * 2).min(MAX_REOPEN_DELAY);
                            }
                        };
                    }
                }
            }
        })
        .map_err(InputError::Io)
        .map(|_| ReaderHandle)
}

/// Marker for the running reader thread; the thread (and its open hidraw fd)
/// lives until the process exits.
pub struct ReaderHandle;

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(buttons: u64, lx: i16, ly: i16) -> Frame {
        Frame { buttons, lthumb_x: lx, lthumb_y: ly }
    }

    #[test]
    fn face_buttons_fire_on_rising_edge_only() {
        let mut s = GamepadState::default();
        assert_eq!(s.process(&frame(buttons::A, 0, 0)), vec![Event::Press]);
        // held across the next report: no repeat, no release event
        assert!(s.process(&frame(buttons::A, 0, 0)).is_empty());
        assert!(s.process(&frame(0, 0, 0)).is_empty());
        assert_eq!(s.process(&frame(buttons::B, 0, 0)), vec![Event::Backspace]);
    }

    #[test]
    fn face_button_mapping_matches_plan() {
        let mut s = GamepadState::default();
        assert_eq!(s.process(&frame(buttons::X, 0, 0)), vec![Event::ToggleShift]);
        assert_eq!(s.process(&frame(buttons::Y, 0, 0)), vec![Event::TextInput(' ')]);
        assert_eq!(s.process(&frame(buttons::L1, 0, 0)), vec![Event::PrevLayer]);
        assert_eq!(s.process(&frame(buttons::R1, 0, 0)), vec![Event::NextLayer]);
        assert_eq!(s.process(&frame(buttons::VIEW, 0, 0)), vec![Event::PrevLayer]);
        assert_eq!(s.process(&frame(buttons::OPTIONS, 0, 0)), vec![Event::Submit]);
        assert_eq!(s.process(&frame(buttons::STEAM, 0, 0)), vec![Event::Cancel]);
    }

    #[test]
    fn dpad_and_touch_l4_r4_navigate() {
        let mut s = GamepadState::default();
        assert_eq!(s.process(&frame(buttons::D_UP, 0, 0)), vec![Event::Nav(Up)]);
        assert_eq!(s.process(&frame(buttons::D_DOWN, 0, 0)), vec![Event::Nav(Down)]);
        assert_eq!(s.process(&frame(buttons::D_LEFT, 0, 0)), vec![Event::Nav(Left)]);
        assert_eq!(s.process(&frame(buttons::D_RIGHT, 0, 0)), vec![Event::Nav(Right)]);
        assert_eq!(s.process(&frame(buttons::L4, 0, 0)), vec![Event::Nav(Up)]);
        assert_eq!(s.process(&frame(buttons::R4, 0, 0)), vec![Event::Nav(Down)]);
    }

    #[test]
    fn stick_respects_deadzone_and_fires_once() {
        let mut s = GamepadState::default();
        // small values stay inside the deadzone
        assert!(s.process(&frame(0, 1000, 1000)).is_empty());
        // pushing right fires once, holding does not repeat
        assert_eq!(s.process(&frame(0, 30000, 0)), vec![Event::Nav(Right)]);
        assert!(s.process(&frame(0, 32000, 0)).is_empty());
        // returning through the deadzone re-arms the edge
        assert!(s.process(&frame(0, 0, 0)).is_empty());
        // raw reports are up-positive: positive y means up
        assert_eq!(s.process(&frame(0, 0, 30000)), vec![Event::Nav(Up)]);
        assert_eq!(s.process(&frame(0, 0, -30000)), vec![Event::Nav(Down)]);
    }

    #[test]
    fn synthetic_report_round_trips_through_parser() {
        let bytes = deckhid::make_report(buttons::A, -100, 200);
        let parsed = deckhid::parse_report(&bytes).expect("round trip");
        assert_eq!(parsed, frame(buttons::A, -100, 200));
    }
}
