//! Linux pseudo-terminal allocation behind [`crate::platform::pty`].
//!
//! `openpty` allocates a master/slave pair from the kernel's pty pool;
//! `ttyname_r` names the slave so a caller can open it by path exactly as it
//! would open an attached board.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

/// Linux always provides pseudo-terminals.
pub const SUPPORTED: bool = true;

/// One pseudo-terminal pair allocated with `openpty`.
pub struct PtyPair {
    master: OwnedFd,
    /// Held open for the pair's lifetime so the terminal stays live while the
    /// caller opens the slave by name; the master read fails once it is gone.
    _slave: OwnedFd,
    name: String,
}

impl PtyPair {
    /// Allocate a pseudo-terminal pair with kernel-default line settings.
    pub fn open() -> io::Result<Self> {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        // SAFETY: openpty initializes both descriptors on success. The
        // termios/winsize pointers are null, which requests kernel defaults.
        let rc = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were initialized above and each is moved
        // into its OwnedFd exactly once.
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        // SAFETY: same descriptor ownership as `master`.
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
        let name = slave_name_of(&slave)?;
        Ok(Self {
            master,
            _slave: slave,
            name,
        })
    }

    pub fn slave_name(&self) -> &str {
        &self.name
    }

    /// Poll the master side for readable input without consuming it.
    pub fn wait_for_master_input(&self, timeout: Duration) -> io::Result<bool> {
        let mut poll_fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: poll_fd points at one initialized pollfd that outlives the
        // call.
        let rc = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(rc > 0 && poll_fd.revents & libc::POLLIN != 0)
    }

    /// Read until the caller's buffer is full, retrying interrupted reads.
    pub fn read_master_exact(&self, buffer: &mut [u8]) -> io::Result<()> {
        let mut filled = 0;
        while filled < buffer.len() {
            let remaining = buffer.len() - filled;
            // SAFETY: the destination pointer is valid for `remaining` bytes.
            let rc = unsafe {
                libc::read(
                    self.master.as_raw_fd(),
                    buffer[filled..].as_mut_ptr().cast(),
                    remaining,
                )
            };
            if rc < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if rc == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "pseudo-terminal master closed before the buffer filled",
                ));
            }
            filled += rc as usize;
        }
        Ok(())
    }
}

/// Name the slave side of a pseudo-terminal pair.
fn slave_name_of(slave: &OwnedFd) -> io::Result<String> {
    let mut buffer = [0 as std::ffi::c_char; 256];
    // SAFETY: ttyname_r writes at most buffer.len() bytes into the array.
    let rc = unsafe { libc::ttyname_r(slave.as_raw_fd(), buffer.as_mut_ptr(), buffer.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    // SAFETY: on success ttyname_r wrote a NUL-terminated device path.
    let name = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    name.to_str()
        .map(str::to_owned)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
