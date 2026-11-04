// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Steam Deck controller → `nislock_kbd::Event` mapping.
//!
//! We read the raw 64-byte input reports from the `hid-steam` control
//! interface's hidraw node (see [`deckhid`]) instead of evdev: while that node
//! is open the kernel forwards reports to us regardless of the `lizard_mode`
//! module parameter, and no other client can consume the events underneath us.
//! While the node is writable we also silence the Deck's built-in
//! keyboard/mouse emulation (see [`lizard`]) so pressed buttons do not also
//! reach the compositor as keystrokes.

pub mod deckhid;
pub mod error;
pub mod lizard;
mod poll;

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use deckhid::{Frame, buttons};
pub use error::InputError;
use nislock_kbd::Dir::{Down, Left, Right, Up};
pub use nislock_kbd::{Dir, Event};
use poll::{Ready, wait_ready};

pub use deckhid::find_deck_hidraw;

const INITIAL_REOPEN_DELAY: Duration = Duration::from_millis(500);
const MAX_REOPEN_DELAY: Duration = Duration::from_secs(5);
/// The firmware watchdog restores lizard mode once the disable stops being fed.
const LIZARD_FEED_INTERVAL: Duration = Duration::from_millis(500);

/// Button/stick state needed to translate raw reports into widget events.
#[derive(Default)]
pub struct GamepadState {
    prev_buttons: u64,    // for rising-edge detection on the button bitmask
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
/// With a writable node the Deck's keyboard emulation is disabled and kept
/// off by periodic feeds; dropping the returned handle stops the thread,
/// which re-enables the emulation before it exits. Any exit that skips the
/// drop (signal, abort) leaves that to the firmware watchdog.
pub fn spawn_deck_reader(tx: Sender<Event>) -> Result<ReaderHandle, InputError> {
    let path = find_deck_hidraw()?.ok_or(InputError::DeviceNotFound)?;
    let (mut file, control) = open_deck_hidraw(&path)?;
    // Closing stop_tx wakes the reader from poll(); nothing is ever written.
    let (stop_tx, stop_rx) = UnixStream::pair()?;
    let thread = std::thread::Builder::new()
        .name("nislock-gamepad".into())
        .spawn(move || {
            let mut state = GamepadState::default();
            let mut buf = [0u8; deckhid::REPORT_LEN];
            let mut controlled = control && try_disable_lizard(&file);
            let mut last_feed = Instant::now();
            'reader: loop {
                if controlled && last_feed.elapsed() >= LIZARD_FEED_INTERVAL {
                    last_feed = Instant::now();
                    if let Err(e) = lizard::feed(&file) {
                        eprintln!("nislock-input: lizard feed failed ({e}); stopping feeds");
                        controlled = false;
                    }
                }
                // Feeding needs the poll timeout: reports stop if the pad input
                // is idle, and the watchdog would restore lizard mode then.
                let timeout = if controlled {
                    Some(LIZARD_FEED_INTERVAL.saturating_sub(last_feed.elapsed()))
                } else {
                    None
                };
                match wait_ready(Some(&file), &stop_rx, timeout) {
                    Err(e) => {
                        eprintln!("nislock-input: deck hidraw poll failed ({e}); stopping");
                        break 'reader;
                    }
                    Ok(Ready::Stop) => break 'reader,
                    Ok(Ready::TimedOut) => {}
                    Ok(Ready::Device) => match file.read(&mut buf) {
                        Ok(n) if n == buf.len() => {
                            let Some(frame) = deckhid::parse_report(&buf) else {
                                continue; // not an input report packet
                            };
                            for mapped in state.process(&frame) {
                                if tx.send(mapped).is_err() {
                                    break 'reader;
                                }
                            }
                        }
                        Ok(_) => {} // short read: discard, alignment is per-read
                        Err(e) => {
                            eprintln!("nislock-input: deck hidraw read failed ({e}); reopening");
                            state = GamepadState::default();
                            let Some((reopened, control)) = reopen_deck_hidraw(&stop_rx) else {
                                break 'reader;
                            };
                            file = reopened;
                            controlled = control && try_disable_lizard(&file);
                            last_feed = Instant::now();
                        }
                    },
                }
            }
            if controlled && let Err(e) = lizard::enable(&file) {
                eprintln!(
                    "nislock-input: failed to re-enable the Deck's keyboard \
                     emulation ({e}); the firmware watchdog will restore it"
                );
            }
        })?;
    Ok(ReaderHandle {
        stop_tx: Some(stop_tx),
        thread: Some(thread),
    })
}

/// Open the control interface; lizard control needs feature reports, so a
/// node without write access falls back to read-only (no lizard control).
fn open_deck_hidraw(path: &Path) -> Result<(File, bool), InputError> {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => Ok((file, true)),
        Err(write_err) => {
            let file = File::open(path)?;
            eprintln!(
                "nislock-input: {write_err}; opened read-only, \
                 cannot disable the Deck's keyboard emulation"
            );
            Ok((file, false))
        }
    }
}

/// Best-effort lizard disable on a fresh fd; false stops sending for it.
fn try_disable_lizard(file: &File) -> bool {
    match lizard::disable(file) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("nislock-input: failed to disable the Deck's keyboard emulation ({e})");
            false
        }
    }
}

/// Block until the Deck hidraw node can be opened again, backing off between
/// attempts; `None` when the reader should stop first.
fn reopen_deck_hidraw(stop_rx: &UnixStream) -> Option<(File, bool)> {
    let mut delay = INITIAL_REOPEN_DELAY;
    loop {
        if let Ok(Ready::Stop) = wait_ready(None, stop_rx, Some(delay)) {
            return None;
        }
        if let Some(path) = find_deck_hidraw().ok().flatten()
            && let Ok(opened) = open_deck_hidraw(&path)
        {
            return Some(opened);
        }
        delay = (delay * 2).min(MAX_REOPEN_DELAY);
    }
}

/// Keeps the reader thread and its lizard-mode disable alive; dropping it
/// stops the thread, which re-enables the Deck's keyboard emulation first.
pub struct ReaderHandle {
    stop_tx: Option<UnixStream>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ReaderHandle {
    fn drop(&mut self) {
        // closing the write half gives the reader EOF on its stop fd
        drop(self.stop_tx.take());
        // join only errs if the reader panicked; the watchdog restores lizard mode then
        let _ = self.thread.take().map(|thread| thread.join());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(buttons: u64, lx: i16, ly: i16) -> Frame {
        Frame {
            buttons,
            lthumb_x: lx,
            lthumb_y: ly,
        }
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
        assert_eq!(
            s.process(&frame(buttons::X, 0, 0)),
            vec![Event::ToggleShift]
        );
        assert_eq!(
            s.process(&frame(buttons::Y, 0, 0)),
            vec![Event::TextInput(' ')]
        );
        assert_eq!(s.process(&frame(buttons::L1, 0, 0)), vec![Event::PrevLayer]);
        assert_eq!(s.process(&frame(buttons::R1, 0, 0)), vec![Event::NextLayer]);
        assert_eq!(
            s.process(&frame(buttons::VIEW, 0, 0)),
            vec![Event::PrevLayer]
        );
        assert_eq!(
            s.process(&frame(buttons::OPTIONS, 0, 0)),
            vec![Event::Submit]
        );
        assert_eq!(s.process(&frame(buttons::STEAM, 0, 0)), vec![Event::Cancel]);
    }

    #[test]
    fn dpad_and_touch_l4_r4_navigate() {
        let mut s = GamepadState::default();
        assert_eq!(s.process(&frame(buttons::D_UP, 0, 0)), vec![Event::Nav(Up)]);
        assert_eq!(
            s.process(&frame(buttons::D_DOWN, 0, 0)),
            vec![Event::Nav(Down)]
        );
        assert_eq!(
            s.process(&frame(buttons::D_LEFT, 0, 0)),
            vec![Event::Nav(Left)]
        );
        assert_eq!(
            s.process(&frame(buttons::D_RIGHT, 0, 0)),
            vec![Event::Nav(Right)]
        );
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
