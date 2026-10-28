//! Debug tool: print raw Deck input reports and mapped widget events.

use std::io::Read;

use nislock_input::{GamepadState, deckhid};

fn main() {
    let Some(path) = deckhid::find_deck_hidraw().expect("cannot scan sysfs hidraw") else {
        eprintln!("no Steam Deck hidraw control interface found (is the controller on?)");
        std::process::exit(1);
    };
    println!("opening {}", path.display());
    let mut file = std::fs::File::open(&path).expect("cannot open hidraw");
    println!("(evdev gamepad interfaces should disappear now; they return on exit)");
    let mut state = GamepadState::default();
    let mut buf = [0u8; deckhid::REPORT_LEN];
    loop {
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
