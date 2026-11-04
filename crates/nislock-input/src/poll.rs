// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Unuaviro F. Argxentakato <unuaviro@argxentakato.me>

//! The reader thread must wait on the device fd and a stop fd at once, which
//! needs one `poll(2)` shim; the module keeps all its `unsafe` here.

use std::io;
use std::os::unix::io::AsRawFd;
use std::time::Duration;

/// Which fd [`wait_ready`] found actionable.
pub(crate) enum Ready {
    /// The device has data or hung up; the read path turns errors into reopen.
    Device,
    /// The stop socket's write half closed; the reader thread must exit.
    Stop,
    /// The caller's timeout elapsed.
    TimedOut,
}

const DEVICE_FLAGS: libc::c_short = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
const STOP_FLAGS: libc::c_short = libc::POLLIN | libc::POLLHUP;

/// Block until the device or stop fd is ready, or `timeout` elapses (`None` =
/// no timeout). `fd < 0` entries are ignored, so `None` polls for stop only;
/// `EINTR` is retried.
pub(crate) fn wait_ready(
    device: Option<&dyn AsRawFd>,
    stop: &dyn AsRawFd,
    timeout: Option<Duration>,
) -> io::Result<Ready> {
    // round up so a sub-millisecond remainder sleeps instead of spinning
    let timeout_ms = timeout.map_or(-1, |t| {
        t.as_micros().div_ceil(1000).min(libc::c_int::MAX as u128) as libc::c_int
    });
    let mut fds = [
        libc::pollfd {
            fd: device.map_or(-1, |f| f.as_raw_fd()),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stop.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: both fds are owned by borrows alive across the call, and
        // poll only writes the revents fields of this exact array.
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if ready(&fds[1], STOP_FLAGS) {
            return Ok(Ready::Stop);
        }
        if ready(&fds[0], DEVICE_FLAGS) {
            return Ok(Ready::Device);
        }
        return Ok(Ready::TimedOut);
    }
}

fn ready(fd: &libc::pollfd, flags: libc::c_short) -> bool {
    fd.revents & flags != 0
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn times_out_when_nothing_is_ready() {
        let (_tx, rx) = UnixStream::pair().unwrap();
        assert!(matches!(
            wait_ready(None, &rx, Some(Duration::from_millis(10))).unwrap(),
            Ready::TimedOut
        ));
    }

    #[test]
    fn reports_stop_once_the_write_half_closes() {
        let (tx, rx) = UnixStream::pair().unwrap();
        drop(tx);
        assert!(matches!(wait_ready(None, &rx, None).unwrap(), Ready::Stop));
    }

    #[test]
    fn reports_a_written_device() {
        let (mut dev_tx, dev_rx) = UnixStream::pair().unwrap();
        let (_stop_tx, stop_rx) = UnixStream::pair().unwrap();
        dev_tx.write_all(b"x").unwrap();
        assert!(matches!(
            wait_ready(Some(&dev_rx), &stop_rx, None).unwrap(),
            Ready::Device
        ));
    }

    #[test]
    fn stop_outranks_a_ready_device() {
        let (mut dev_tx, dev_rx) = UnixStream::pair().unwrap();
        let (stop_tx, stop_rx) = UnixStream::pair().unwrap();
        dev_tx.write_all(b"x").unwrap();
        drop(stop_tx);
        assert!(matches!(
            wait_ready(Some(&dev_rx), &stop_rx, None).unwrap(),
            Ready::Stop
        ));
    }
}
