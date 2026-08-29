//! PID 1 duties.
//!
//! There is no init system, no shell and no userland in the guest rootfs — this
//! binary is `/init` and the only process in the VM. That is deliberate: there
//! is nothing for a compromised web server to exec.

use std::ffi::CString;
use std::os::fd::{FromRawFd, OwnedFd};

/// Mount `/proc`. Needed to read the kernel command line, which is how runtime
/// configuration reaches the guest (there is no shell to set env vars).
pub fn mount_proc() {
    let src = CString::new("proc").unwrap();
    let target = CString::new("/proc").unwrap();
    let fstype = CString::new("proc").unwrap();
    // MS_NOSUID | MS_NODEV | MS_NOEXEC
    let flags = 0x2 | 0x4 | 0x8;
    // SAFETY: all pointers are valid NUL-terminated C strings that outlive the
    // call, and the data argument is legitimately NULL for procfs.
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        eprintln!("warning: could not mount /proc: {}", std::io::Error::last_os_error());
    }
}

/// Read a `key=value` parameter out of `/proc/cmdline`.
///
/// Firecracker's `boot_args` is the only configuration channel into the guest,
/// so this is how the allowed CORS origin and vsock ports get in without
/// rebuilding the rootfs.
pub fn kernel_param(key: &str) -> Option<String> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").ok()?;
    let prefix = format!("{key}=");
    cmdline
        .split_ascii_whitespace()
        .find_map(|tok| tok.strip_prefix(&prefix))
        .map(str::to_owned)
}

/// Create a pipe and splice its read end onto our own stdin.
///
/// `rs162` can only take I/Q from a file, a TCP socket, or stdin — there is no
/// constructor accepting an arbitrary reader. Since we are PID 1 we simply
/// rewrite fd 0 to be the read end of a pipe that the vsock feeder fills. This
/// keeps the guest free of any network stack while still satisfying the crate's
/// `from_stdin` API.
///
/// Returns the write end, to be handed to the feeder task.
pub fn redirect_stdin_from_pipe() -> std::io::Result<OwnedFd> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid two-element array for pipe2 to fill.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: read_fd is a freshly created, owned fd; dup2 closes fd 0 first.
    if unsafe { libc::dup2(read_fd, libc::STDIN_FILENO) } < 0 {
        let err = std::io::Error::last_os_error();
        // SAFETY: both fds are owned by us and not yet handed out.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(err);
    }
    // SAFETY: read_fd has been duplicated onto fd 0; the original is redundant.
    unsafe { libc::close(read_fd) };

    // The feeder writes from async code, so the write end must not block.
    // SAFETY: write_fd is a valid open fd we exclusively own.
    unsafe {
        let flags = libc::fcntl(write_fd, libc::F_GETFL);
        libc::fcntl(write_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    // SAFETY: write_fd is open, owned, and not duplicated elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(write_fd) })
}
