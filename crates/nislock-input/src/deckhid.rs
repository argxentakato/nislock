// Portions of this file are derived from the `deckhidraw` crate v0.0.0
// (upstream 0BSD, Copyright (c) 2026 R <rqou@berkeley.edu>,
// https://github.com/ArcaneNibble/deckhidraw). Used here as a subset with
// modifications: frame-layout constants, button bits, and sysfs matching.
//! Steam Deck control-interface hidraw: device discovery + raw input reports.
//!
//! The kernel `hid-steam` driver exposes a virtual HID client device
//! (uevent MODALIAS `hid:b0003g0103v000028DEp00001205`) with a
//! `/dev/hidraw<N>` node. While that node is open, the driver forwards the
//! raw 64-byte Deck input reports to the reader and unregisters the evdev
//! gamepad interfaces (they come back when it is closed). This works
//! regardless of the `lizard_mode` module parameter, unlike reading evdev.

use std::io;
use std::path::{Path, PathBuf};

/// Length of one raw input report from the control interface.
pub const REPORT_LEN: usize = 64;

/// Magic value at the start of every input report packet.
const REPORT_MAGIC: u32 = 0x4009_0001;

/// Deck control client interface MODALIAS (as found in sysfs `device/uevent`).
const DECK_MODALIAS: &[u8] = b"hid:b0003g0103v000028DEp00001205";

/// Button bit masks inside `Frame::buttons` (1 = pressed).
pub mod buttons {
    pub const A: u64 = 1 << 7;
    pub const B: u64 = 1 << 5;
    pub const X: u64 = 1 << 6;
    pub const Y: u64 = 1 << 4;

    pub const L1: u64 = 1 << 3;
    pub const R1: u64 = 1 << 2;

    pub const D_UP: u64 = 1 << 8;
    pub const D_RIGHT: u64 = 1 << 9;
    pub const D_LEFT: u64 = 1 << 10;
    pub const D_DOWN: u64 = 1 << 11;

    pub const L4: u64 = 1 << 41;
    pub const R4: u64 = 1 << 42;

    pub const VIEW: u64 = 1 << 12;
    pub const STEAM: u64 = 1 << 13;
    pub const OPTIONS: u64 = 1 << 14;
}

/// The fields we actually need decoded from one input report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Frame {
    /// Digital buttons, see [`buttons`].
    pub buttons: u64,
    /// Left thumb stick position in [-32768, 32767].
    /// x is right-positive, y is **up**-positive (the kernel negates this
    /// sign when re-expressing it through evdev, but on the raw report
    /// positive y is up).
    pub lthumb_x: i16,
    pub lthumb_y: i16,
}

/// Decode one raw report; `None` if too short or not an input report packet.
pub fn parse_report(buf: &[u8]) -> Option<Frame> {
    if buf.len() < REPORT_LEN {
        return None;
    }
    if u32::from_le_bytes(buf[0..4].try_into().ok()?) != REPORT_MAGIC {
        return None;
    }
    Some(Frame {
        buttons: u64::from_le_bytes(buf[8..16].try_into().ok()?),
        lthumb_x: i16::from_le_bytes(buf[48..50].try_into().ok()?),
        lthumb_y: i16::from_le_bytes(buf[50..52].try_into().ok()?),
    })
}

/// Build a synthetic input report (for tests); fields outside `Frame` are zero.
pub fn make_report(buttons: u64, lthumb_x: i16, lthumb_y: i16) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    buf[0..4].copy_from_slice(&REPORT_MAGIC.to_le_bytes());
    buf[8..16].copy_from_slice(&buttons.to_le_bytes());
    buf[48..50].copy_from_slice(&lthumb_x.to_le_bytes());
    buf[50..52].copy_from_slice(&lthumb_y.to_le_bytes());
    buf
}

/// Pure matcher for the contents of a sysfs hidraw `device/uevent` file.
fn uevent_matches_deck(uevent: &[u8]) -> bool {
    for line in uevent.split(|&c| c == b'\n') {
        let line = line.trim_ascii();
        if let Some(value) = line.strip_prefix(b"MODALIAS=")
            && value == DECK_MODALIAS
        {
            return true;
        }
    }
    false
}

/// Find the `/dev/hidraw<N>` node of the Deck control interface, or `None`
/// when the Deck controller is not present.
pub fn find_deck_hidraw() -> io::Result<Option<PathBuf>> {
    let mut found = None;
    for entry in std::fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let uevent_path = entry.path().join("device/uevent");
        let Ok(uevent) = std::fs::read(&uevent_path) else {
            continue;
        };
        if uevent_matches_deck(&uevent) {
            found = Some(Path::new("/dev").join(entry.file_name()));
            break;
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offsets_and_rejects_bad_magic() {
        let buf = make_report(buttons::A | buttons::L4, -1234, 5678);
        let f = parse_report(&buf).expect("valid report parses");
        assert_eq!(f.buttons, buttons::A | buttons::L4);
        assert_eq!((f.lthumb_x, f.lthumb_y), (-1234, 5678));

        let mut bad = buf;
        bad[3] ^= 0xff;
        assert!(parse_report(&bad).is_none());
        assert!(parse_report(&buf[..REPORT_LEN - 1]).is_none());
    }

    #[test]
    fn uevent_matcher_accepts_deck_modalias() {
        let uevent = b"DRIVER=hid-steam\n\
MODALIAS=hid:b0003g0103v000028DEp00001205\n\
HID_ID=0003:000028DE:00001205\n";
        assert!(uevent_matches_deck(uevent));
        // no trailing newline
        assert!(uevent_matches_deck(b"MODALIAS=hid:b0003g0103v000028DEp00001205"));
    }

    #[test]
    fn uevent_matcher_rejects_other_devices() {
        // vendor/product differ
        assert!(!uevent_matches_deck(
            b"MODALIAS=hid:b0003g0000v000028DEp00001142\n"
        ));
        assert!(!uevent_matches_deck(b"MODALIAS=usb:v28DEp1205d0001dc00\n"));
        assert!(!uevent_matches_deck(b"MAJOR=242\nMINOR=1\nDEVNAME=hidraw0\n"));
    }
}
