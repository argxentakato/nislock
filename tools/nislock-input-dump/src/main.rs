// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! Debug tool: print raw Deck input reports and mapped widget events.
//!
//! `cargo run -p nislock-input-dump -- --lizard-off` silences the Deck's
//! keyboard/mouse emulation while running (dev flag, needs write access).

use std::fs::OpenOptions;
use std::io::Read;
use std::time::{Duration, Instant};

use nislock_input::{GamepadState, deckhid, lizard};

const FEED_INTERVAL: Duration = Duration::from_millis(500);

fn main() {
    let lizard_off = std::env::args().skip(1).any(|arg| arg == "--lizard-off");
    let Some(path) = deckhid::find_deck_hidraw().expect("cannot scan sysfs hidraw") else {
        eprintln!("no Steam Deck hidraw control interface found (is the controller on?)");
        std::process::exit(1);
    };
    println!("opening {}", path.display());
    let mut writable = false;
    let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => {
            writable = true;
            file
        }
        Err(_) => std::fs::File::open(&path).expect("cannot open hidraw"),
    };
    if lizard_off {
        if !writable {
            eprintln!("hidraw is not writable; cannot control lizard mode");
            std::process::exit(1);
        }
        if let Err(e) = lizard::disable(&file) {
            eprintln!("failed to disable the Deck's keyboard emulation: {e}");
            std::process::exit(1);
        }
        println!("lizard mode disabled (restored after this tool exits)");
    }
    println!("(evdev gamepad interfaces should disappear now; they return on exit)");
    let mut state = GamepadState::default();
    let mut buf = [0u8; deckhid::REPORT_LEN];
    let mut last_feed = Instant::now();
    loop {
        if lizard_off && last_feed.elapsed() >= FEED_INTERVAL {
            last_feed = Instant::now();
            if let Err(e) = lizard::feed(&file) {
                eprintln!("lizard feed failed: {e}");
            }
        }
        let Ok(n) = file.read(&mut buf) else { continue };
        if n != buf.len() {
            continue;
        }
        let Some(frame) = deckhid::parse_report(&buf) else {
            println!("non-report packet (first 4 bytes: {:02x?})", &buf[..4]);
            continue;
        };
        println!(
            "buttons=0x{0:016x} lthumb=({1}, {2})",
            frame.buttons, frame.lthumb_x, frame.lthumb_y
        );
        for mapped in state.process(&frame) {
            println!("  -> {mapped:?}");
        }
    }
}
