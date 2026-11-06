//! Keeps the real stdout for the single outcome line.
//!
//! Servo writes to stdout, and some of what it writes is chosen by the page. For example,
//! `servo-script`'s form validation `println!`s an element's custom validity message, newlines
//! included. A page could therefore print a forged `FetchOutcome` before the real one. So before
//! Servo starts, the real stdout is duplicated for the helper's own use, and the process's
//! stdout (the OS handle or fd 1, the C runtime's fd 1, and with them Rust's `std::io::stdout()`)
//! is pointed at stderr.

use std::fs::File;
use std::io;

/// Takes the real stdout for the caller and sends everything else written to stdout to stderr.
pub fn take() -> io::Result<File> {
    imp::take()
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::mem::ManuallyDrop;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};

    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;

    unsafe extern "system" {
        fn SetStdHandle(std_handle: u32, handle: RawHandle) -> i32;
    }

    pub fn take() -> io::Result<File> {
        // Duplicate the handle (try_clone is DuplicateHandle) without taking ownership of the original.
        let original = ManuallyDrop::new(unsafe { File::from_raw_handle(io::stdout().as_raw_handle()) });
        let saved = original.try_clone()?;
        // The C runtime's fd 1, used by C and C++ code in the engine.
        if unsafe { libc::dup2(2, 1) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // Rust's stdout asks the OS for STD_OUTPUT_HANDLE on every write.
        if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, io::stderr().as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(saved)
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::FromRawFd;

    pub fn take() -> io::Result<File> {
        let saved = unsafe { libc::dup(1) };
        if saved == -1 {
            return Err(io::Error::last_os_error());
        }
        // Rust's stdout, C stdio and everything else write to fd 1, which is now stderr.
        if unsafe { libc::dup2(2, 1) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(saved) })
    }
}
