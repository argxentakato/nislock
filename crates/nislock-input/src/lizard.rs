// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! The Steam Deck firmware emulates a USB keyboard and mouse ("lizard mode")
//! even while we hold the control hidraw open: `hid-steam` deliberately sends
//! no lizard commands to a client device, so a pressed A button also arrives
//! at the compositor as an emulated Enter keypress.
//!
//! The command ids, settings indices, and the 64-byte feature-report layout
//! below follow the `hid-steam` driver and SDL's Steam Deck HIDAPI driver.
//! Like SDL, the firmware watchdog stays enabled: the disable is fed
//! periodically and the firmware restores lizard mode by itself once the
//! feed stops, so a crash cannot leave the Deck without its stock input.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

use crate::deckhid::REPORT_LEN;

/// One control message: report-id byte (0) + the 64-byte Valve feature report.
const FEATURE_LEN: usize = REPORT_LEN + 1;

// Valve feature-report command ids (hid-steam.c `enum` ID_*).
const ID_CLEAR_DIGITAL_MAPPINGS: u8 = 0x81;
const ID_SET_DEFAULT_DIGITAL_MAPPINGS: u8 = 0x85;
const ID_SET_SETTINGS_VALUES: u8 = 0x87;
const ID_LOAD_DEFAULT_SETTINGS: u8 = 0x8E;

// Firmware setting indices (Valve `ControllerSettings` enum).
const SETTING_LEFT_TRACKPAD_MODE: u8 = 7;
const SETTING_RIGHT_TRACKPAD_MODE: u8 = 8;
const SETTING_LIZARD_MODE: u8 = 9;
const SETTING_SMOOTH_ABSOLUTE_MOUSE: u8 = 24;
const SETTING_LEFT_TRACKPAD_CLICK_PRESSURE: u8 = 52;
const SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE: u8 = 53;
const SETTING_STEAM_WATCHDOG_ENABLE: u8 = 71;

const TRACKPAD_NONE: u16 = 7;
const LIZARD_MODE_ON: u16 = 1;
/// Pressure threshold no click can reach, i.e. the clicky pads are inert.
const CLICK_PRESSURE_OFF: u16 = 0xFFFF;

/// A control command with no payload.
fn command_report(cmd: u8) -> [u8; FEATURE_LEN] {
    let mut report = [0u8; FEATURE_LEN];
    report[1] = cmd;
    report[2] = 0; // settings payload length
    report
}

/// An ID_SET_SETTINGS_VALUES payload: (reg, valueLo, valueHi) triples.
fn settings_report(settings: &[(u8, u16)]) -> [u8; FEATURE_LEN] {
    let mut report = command_report(ID_SET_SETTINGS_VALUES);
    for (i, &(reg, value)) in settings.iter().enumerate() {
        report[3 + i * 3] = reg;
        report[4 + i * 3] = (value & 0xff) as u8;
        report[5 + i * 3] = (value >> 8) as u8;
    }
    report[2] = (settings.len() * 3) as u8;
    report
}

fn disable_reports() -> [[u8; FEATURE_LEN]; 2] {
    [
        command_report(ID_CLEAR_DIGITAL_MAPPINGS),
        settings_report(&[
            (SETTING_SMOOTH_ABSOLUTE_MOUSE, 0),
            (SETTING_LEFT_TRACKPAD_MODE, TRACKPAD_NONE),
            (SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_NONE),
            (SETTING_LEFT_TRACKPAD_CLICK_PRESSURE, CLICK_PRESSURE_OFF),
            (SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE, CLICK_PRESSURE_OFF),
        ]),
    ]
}

fn feed_reports() -> [[u8; FEATURE_LEN]; 2] {
    [
        command_report(ID_CLEAR_DIGITAL_MAPPINGS),
        settings_report(&[(SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_NONE)]),
    ]
}

// The explicit settings also repair a Deck left with lizard mode and the
// watchdog switched off (the hid-steam disable sequence).
fn enable_reports() -> [[u8; FEATURE_LEN]; 3] {
    [
        command_report(ID_SET_DEFAULT_DIGITAL_MAPPINGS),
        command_report(ID_LOAD_DEFAULT_SETTINGS),
        settings_report(&[
            (SETTING_LIZARD_MODE, LIZARD_MODE_ON),
            (SETTING_STEAM_WATCHDOG_ENABLE, 1),
        ]),
    ]
}

/// Hide the Deck's emulated keyboard/mouse; must be followed by [`feed`].
pub fn disable(file: &File) -> Result<(), io::Error> {
    send_reports(file, &disable_reports())
}

/// Keep the firmware watchdog from restoring lizard mode; call at least
/// every few hundred milliseconds while lizard mode must stay off.
pub fn feed(file: &File) -> Result<(), io::Error> {
    send_reports(file, &feed_reports())
}

/// Restore the stock button→key mappings and settings.
pub fn enable(file: &File) -> Result<(), io::Error> {
    send_reports(file, &enable_reports())
}

fn send_reports(file: &File, reports: &[[u8; FEATURE_LEN]]) -> Result<(), io::Error> {
    for report in reports {
        ctl::set_feature(file.as_raw_fd(), report)?;
    }
    Ok(())
}

/// All `unsafe` for this module lives in this tiny ioctl shim.
mod ctl {
    use std::io;
    use std::os::unix::io::RawFd;

    use super::FEATURE_LEN;

    // _IOC encoding of `HIDIOCSFEATURE(FEATURE_LEN)` from <linux/hidraw.h>.
    const SET_FEATURE: libc::Ioctl =
        ((3u32 << 30) | ((b'H' as u32) << 8) | 0x06 | ((FEATURE_LEN as u32) << 16)) as libc::Ioctl;

    pub(super) fn set_feature(fd: RawFd, report: &[u8; FEATURE_LEN]) -> Result<(), io::Error> {
        // SAFETY: fd is valid while the borrowed `File` lives, and the request
        // length exactly describes the 65-byte buffer being passed.
        let ret = unsafe { libc::ioctl(fd, SET_FEATURE, report.as_ptr()) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_reports_are_zero_padded_after_header() {
        let clear = command_report(ID_CLEAR_DIGITAL_MAPPINGS);
        assert_eq!(&clear[..3], &[0x00, 0x81, 0x00]);
        assert!(clear[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn disable_reports_match_sdl_layout() {
        let [clear, settings] = disable_reports();
        assert_eq!(&clear[..3], &[0x00, 0x81, 0x00]);
        // 0x87, payload length, then (reg, lo, hi)*.
        assert_eq!(
            &settings[..18],
            &[
                0x00, 0x87, 15, 24, 0, 0, 7, 7, 0, 8, 7, 0, 52, 0xFF, 0xFF, 53, 0xFF, 0xFF
            ]
        );
        assert!(settings[18..].iter().all(|&b| b == 0));
    }

    #[test]
    fn disable_leaves_lizard_and_watchdog_settings_alone() {
        let [_, settings] = disable_reports();
        let regs: Vec<u8> = settings[3..18].chunks(3).map(|t| t[0]).collect();
        assert!(!regs.contains(&SETTING_LIZARD_MODE));
        assert!(!regs.contains(&SETTING_STEAM_WATCHDOG_ENABLE));
    }

    #[test]
    fn feed_reports_repeat_clear_and_right_trackpad() {
        let [clear, settings] = feed_reports();
        assert_eq!(&clear[..3], &[0x00, 0x81, 0x00]);
        assert_eq!(&settings[..6], &[0x00, 0x87, 3, 8, 7, 0]);
        assert!(settings[6..].iter().all(|&b| b == 0));
    }

    #[test]
    fn enable_reports_restore_defaults_and_lizard_settings() {
        let [defaults, reset, settings] = enable_reports();
        assert_eq!(&defaults[..3], &[0x00, 0x85, 0x00]);
        assert_eq!(&reset[..3], &[0x00, 0x8E, 0x00]);
        assert_eq!(&settings[..9], &[0x00, 0x87, 6, 9, 1, 0, 71, 1, 0]);
    }
}
