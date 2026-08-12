//! Whether this program has a console, and where its output goes.
//!
//! VBNote is two programs wearing one binary. Started from the Start menu it
//! is an appliance: a window, a voice, and nothing else -- and a black
//! terminal appearing beside it is confusing at best, and for someone using a
//! screen reader it is one more window to get lost in. Started from a command
//! line it is a developer's tool and every word it prints matters.
//!
//! So it is built as a windowed program, which is the one that cannot be
//! undone at run time, and then given a console back when there should be one:
//!
//! * **Started from a terminal**, it attaches to the terminal that started it
//!   and prints there, exactly as a console program would.
//! * **Started from the Start menu with `debug = yes`**, it makes a console of
//!   its own to print into.
//! * **Started from the Start menu otherwise**, it has none, and printing goes
//!   nowhere at all.
//!
//! The handles have to be set up before anything is printed, because the
//! standard library looks them up once and keeps them. That is why this is
//! called at the very top of `main` and again, at most once more, after the
//! settings have been read.

/// Attach to the terminal that started this, if one did.
///
/// Cheap and silent when there is not one, so it costs nothing to try on every
/// start.
pub fn attach_to_parent() -> bool {
    platform::attach_to_parent()
}

/// Make a console, for a windowed program that has been asked to be talkative.
pub fn open_new() -> bool {
    platform::open_new()
}

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};

    static HAVE_ONE: AtomicBool = AtomicBool::new(false);

    type Handle = isize;

    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // -10
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // -11
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // -12
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: Handle = -1;

    extern "system" {
        fn AttachConsole(process: u32) -> i32;
        fn AllocConsole() -> i32;
        fn SetStdHandle(which: u32, handle: Handle) -> i32;
        fn GetStdHandle(which: u32) -> Handle;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: usize,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Whether the process already has somewhere to send this stream.
    ///
    /// A windowed program usually has nothing, but a redirected one has
    /// whatever it was pointed at -- a file, a pipe, another program.
    fn already_have(which: u32) -> bool {
        let h = unsafe { GetStdHandle(which) };
        h != 0 && h != INVALID_HANDLE_VALUE
    }

    fn console_handle(name: &str) -> Handle {
        unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                0,
                OPEN_EXISTING,
                0,
                0,
            )
        }
    }

    /// Give the standard streams somewhere to go, if they have nowhere.
    ///
    /// A windowed program starts with no usable standard handles, so attaching
    /// a console is only half the job: without this, printing still goes
    /// nowhere. But only the missing ones are filled in. Replacing a handle
    /// that is already there would undo a redirection the person running this
    /// asked for -- `vbnote --help > file` would write to a console instead of
    /// the file, which is precisely the bug this comment is standing on.
    fn wire_up_handles() {
        unsafe {
            if !already_have(STD_OUTPUT_HANDLE) || !already_have(STD_ERROR_HANDLE) {
                let out = console_handle("CONOUT$");
                if out != INVALID_HANDLE_VALUE {
                    if !already_have(STD_OUTPUT_HANDLE) {
                        SetStdHandle(STD_OUTPUT_HANDLE, out);
                    }
                    if !already_have(STD_ERROR_HANDLE) {
                        SetStdHandle(STD_ERROR_HANDLE, out);
                    }
                }
            }
            if !already_have(STD_INPUT_HANDLE) {
                let input = console_handle("CONIN$");
                if input != INVALID_HANDLE_VALUE {
                    SetStdHandle(STD_INPUT_HANDLE, input);
                }
            }
        }
    }

    pub fn attach_to_parent() -> bool {
        if HAVE_ONE.load(Ordering::Relaxed) {
            return true;
        }
        let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
        if attached {
            wire_up_handles();
            HAVE_ONE.store(true, Ordering::Relaxed);
        }
        attached
    }

    pub fn open_new() -> bool {
        if HAVE_ONE.load(Ordering::Relaxed) {
            return true;
        }
        let made = unsafe { AllocConsole() } != 0;
        if made {
            wire_up_handles();
            HAVE_ONE.store(true, Ordering::Relaxed);
        }
        made
    }
}

#[cfg(not(windows))]
mod platform {
    /// Everywhere else a process simply has the streams it was given.
    pub fn attach_to_parent() -> bool {
        true
    }
    pub fn open_new() -> bool {
        true
    }
}
