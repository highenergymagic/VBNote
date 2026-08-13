//! The host key, and taking the keyboard.
//!
//! The window library cannot see half of this keyboard. Its Windows key-state
//! table has no entry for either Alt, so `READ` and `FUNCTION` read as never
//! held however they are bound, and asking the platform for those two
//! separately only papered over it. The real answer is to stop asking a
//! library for the key stream and take it: a low-level keyboard hook receives
//! every key before any application does, distinguishes left from right, and
//! can swallow what it takes.
//!
//! That also makes the modifiers safe. `READ` is left Alt and `FUNCTION` is
//! right Alt by default, and Alt on its own would normally put Windows into
//! menu-bar mode, where keystrokes go to a menu instead of to the window.
//! While captured the hook eats them before that can happen, so pressing
//! `READ` alone -- which does nothing on the real machine, and is therefore a
//! natural thing to try -- does nothing here either. `FUNCTION` is not stuck
//! on right Alt: some keyboards use that key for characters of their own, so
//! it can be moved, `set_function_key` here or `function_key` in `VBNote.ini`.
//! A shift key can be chosen, at the price of losing `SHIFT` on it -- the
//! other shift has to carry every capital after that.
//!
//! # The host key
//!
//! `F11`, alone, is never sent to the machine. Held with a letter it commands
//! the emulator:
//!
//! | chord | what it does |
//! | --- | --- |
//! | host + G | capture the keyboard, or give it back |
//! | host + R | reset the machine |
//! | host + Q | quit, saving the flash disk |
//!
//! These work whether or not the keyboard is captured, because the one that
//! gives the keyboard back has to work when it is.
//!
//! # What captured means
//!
//! Captured, every key goes to the machine and nothing reaches the host.
//! Released, no key reaches the machine and the host has its keyboard back.
//! Nothing is in between, and neither state is guessable by looking, so both
//! are announced.
//!
//! Both only apply **while the emulator's window is the foreground one**.
//! With anything else in front, this is completely transparent: no key is
//! taken, no host chord fires, and the capture setting is simply remembered
//! until the window comes back. That keeps the emulator from swallowing keys
//! in other applications, and it removes a trap -- if capture survived losing
//! focus but the host key needed focus to be pressed, there would be no way
//! to give the keyboard back.
//!
//! # Giving the keyboard back cannot depend on the emulator
//!
//! While captured, this holds every key on the machine -- not the emulated
//! one, the user's. If the only way to release it were a message to the
//! emulator's loop, then a loop that stopped for any reason would take the
//! keyboard with it: no keys anywhere, no way to reach a task manager, and
//! nothing to be done about it without a second computer. For a user who
//! cannot see the screen and may have no second computer to hand, that is not
//! an inconvenience.
//!
//! So **host + G takes effect in the hook itself**, before anything is sent
//! anywhere. The emulator is told afterwards, and all it does with the news is
//! say it out loud. If it never hears, the keyboard still comes back.

use crate::keys::{Mods, Press};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;

/// Something the user asked of the emulator rather than of the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Capture(bool),
    Reset,
    Quit,
}

/// Windows virtual-key codes for the keys that mean something here.
///
/// The machine's own key codes are Windows virtual-key codes -- the scan
/// tables in `pdikeybd.dll` are full of them -- so a key the host reports
/// mostly *is* the code the matrix wants, and translation is a filter rather
/// than a mapping.
pub mod vk {
    pub const LSHIFT: u32 = 0xA0;
    pub const RSHIFT: u32 = 0xA1;
    /// `CONTROL` on the machine. So is `RCONTROL`, the same way both shifts
    /// are `SHIFT`.
    pub const LCONTROL: u32 = 0xA2;
    pub const RCONTROL: u32 = 0xA3;
    /// `READ` on the machine, the chord key.
    pub const LMENU: u32 = 0xA4;
    /// `FUNCTION` on the machine, and the host key that stands in for it by
    /// default. Some keyboards use right Alt for characters of their own
    /// (AltGr), so `FUNCTION` can be moved to another key.
    pub const RMENU: u32 = 0xA5;

    /// The keys `FUNCTION` can be moved to. None of them is a key the machine
    /// has or the emulator has taken -- the machine's own `HELP`, `RPT` and
    /// `MENU` are `F1`-`F3`, `F11` is the host key, and `READ` and `CONTROL`
    /// are where they always are. The shifts are allowed too, at a price: the
    /// key chosen becomes `FUNCTION` and stops being `SHIFT`, so the other
    /// shift has to do all of that work. See `function_key_named`.
    pub const CAPS_LOCK: u32 = 0x14;
    pub const LEFT_WIN: u32 = 0x5B;
    pub const RIGHT_WIN: u32 = 0x5C;
    /// The application key, between the Windows keys and right control.
    pub const MENU: u32 = 0x5D;
    pub const F4: u32 = 0x73;
    pub const F5: u32 = 0x74;
    pub const F6: u32 = 0x75;
    pub const F7: u32 = 0x76;
    pub const F8: u32 = 0x77;
    pub const F9: u32 = 0x78;
    pub const F10: u32 = 0x79;
    pub const F12: u32 = 0x7B;

    /// The host key. Never reaches the machine.
    ///
    /// It was right control until a bug report pointed out the obvious: not
    /// every keyboard has one. Compact and laptop keyboards routinely drop
    /// it, and a user who cannot press the host key cannot take the keyboard,
    /// which means they cannot use the emulator at all. `F11` is on every
    /// keyboard this runs on and the machine has no use for it -- `HELP`,
    /// `RPT` and `MENU` are `F1` to `F3`.
    pub const F11: u32 = 0x7A;
    /// What the host key is. One name, so it is changed in one place.
    pub const HOST: u32 = F11;

    pub const G: u32 = 0x47;
    pub const R: u32 = 0x52;
    pub const Q: u32 = 0x51;
}

static CAPTURED: AtomicBool = AtomicBool::new(false);
static HOST_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT: AtomicBool = AtomicBool::new(false);
static CONTROL: AtomicBool = AtomicBool::new(false);
static READ: AtomicBool = AtomicBool::new(false);
static FUNCTION: AtomicBool = AtomicBool::new(false);

/// Which host key stands in for the machine's `FUNCTION`.
///
/// Right Alt is the default, and is the machine's own code (`named::FUNCTION`
/// is `0xA5`). But some keyboards use right Alt for characters of their own,
/// so it is settable -- `set_function_key` before the hook is installed, or
/// `function_key` in `VBNote.ini` for the installed machine.
static FUNCTION_KEY: AtomicU32 = AtomicU32::new(vk::RMENU);

/// The host key that is the machine's `FUNCTION`, right now.
pub fn function_key() -> u32 {
    FUNCTION_KEY.load(Ordering::Relaxed)
}

/// Choose the host key that stands in for the machine's `FUNCTION`.
pub fn set_function_key(vk: u32) {
    FUNCTION_KEY.store(vk, Ordering::Relaxed);
}

/// The host keys `FUNCTION` can be, by the name a settings file writes.
///
/// `None` for anything else, so a typo in `VBNote.ini` says so rather than
/// silently keeping right Alt and telling nobody.
///
/// The shifts are choices, and choosing one costs you SHIFT on that key: the
/// key becomes `FUNCTION` and the other shift key has to carry every capital
/// and every shifted chord. That is a real price, so the settings file says so
/// -- but it is also where the machine's `FUNCTION` sits, so for a keyboard
/// with a natural thumb key on the right it is the request. READ and CONTROL
/// are not choices, so they cannot be taken.
pub fn function_key_named(name: &str) -> Option<u32> {
    let code = match name.to_ascii_lowercase().as_str() {
        "right_alt" => vk::RMENU,
        "left_shift" => vk::LSHIFT,
        "right_shift" => vk::RSHIFT,
        "menu" | "application" => vk::MENU,
        "caps_lock" => vk::CAPS_LOCK,
        "left_windows" => vk::LEFT_WIN,
        "right_windows" => vk::RIGHT_WIN,
        "f4" => vk::F4,
        "f5" => vk::F5,
        "f6" => vk::F6,
        "f7" => vk::F7,
        "f8" => vk::F8,
        "f9" => vk::F9,
        "f10" => vk::F10,
        "f12" => vk::F12,
        _ => return None,
    };
    Some(code)
}

/// What the chosen `FUNCTION` key is called, for saying out loud.
pub fn function_key_spoken() -> String {
    match function_key() {
        vk::RMENU => "right Alt".into(),
        vk::LSHIFT => "left shift".into(),
        vk::RSHIFT => "right shift".into(),
        vk::CAPS_LOCK => "Caps Lock".into(),
        vk::LEFT_WIN => "the left Windows key".into(),
        vk::RIGHT_WIN => "the right Windows key".into(),
        vk::F4 => "F4".into(),
        vk::F5 => "F5".into(),
        vk::F6 => "F6".into(),
        vk::F7 => "F7".into(),
        vk::F8 => "F8".into(),
        vk::F9 => "F9".into(),
        vk::F10 => "F10".into(),
        vk::F12 => "F12".into(),
        other => format!("key {other:#04x}"),
    }
}

/// A question is on screen and wants the keyboard to itself.
static DIALOG_UP: AtomicBool = AtomicBool::new(false);
/// Which keys are physically down, one bit per virtual-key code.
///
/// Windows repeats a held key, several times a second, and each repeat is
/// another key-down event. Passing those on would queue dozens of keystrokes
/// behind each other -- and every keystroke here is held until the guest has
/// looked at it, so the queue drains far slower than Windows fills it. A held
/// `READ` alone was enough to bury the letter it was meant to modify.
static DOWN: [std::sync::atomic::AtomicU64; 4] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Record a key going down or up, and say whether this is news.
///
/// A key-down for a key already down is a repeat, and is not.
fn note_down(vk: u32, down: bool) -> bool {
    let (word, bit) = ((vk as usize / 64) & 3, 1u64 << (vk % 64));
    let before = if down {
        DOWN[word].fetch_or(bit, Ordering::Relaxed)
    } else {
        DOWN[word].fetch_and(!bit, Ordering::Relaxed)
    };
    (before & bit != 0) != down
}
/// What the machine's modifiers are, from what this has seen held.
///
/// Tracked here rather than asked of the platform, because while the keyboard
/// is captured these keys are swallowed and the tracking is the only account
/// of them that is certainly right.
fn mods() -> Mods {
    Mods {
        shift: SHIFT.load(Ordering::Relaxed),
        control: CONTROL.load(Ordering::Relaxed),
        read: READ.load(Ordering::Relaxed),
        function: FUNCTION.load(Ordering::Relaxed),
    }
}

/// What a key event means. Pure, so the decision table can be tested without
/// a keyboard, a hook, or Windows.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Let the host have it.
    PassThrough,
    /// Take it and do nothing else.
    Swallow,
    /// Take it and run this command.
    Run(Command),
    /// Take it and send this to the machine.
    Send(u32),
}

/// Decide what to do with one key event.
///
/// `captured`, `host_down` and `function` are passed in rather than read from
/// the statics so that every combination can be exercised.
pub fn decide(
    vk: u32,
    down: bool,
    captured: bool,
    host_down: bool,
    focused: bool,
    function: u32,
) -> Verdict {
    // With another application in front this is not here at all.
    if !focused {
        return Verdict::PassThrough;
    }
    // The host key itself never goes anywhere.
    if vk == vk::HOST {
        return Verdict::Swallow;
    }
    if host_down {
        if !down {
            return Verdict::Swallow;
        }
        return match vk {
            vk::G => Verdict::Run(Command::Capture(!captured)),
            vk::R => Verdict::Run(Command::Reset),
            vk::Q => Verdict::Run(Command::Quit),
            // An unassigned host chord does nothing, and is still swallowed:
            // typing a stray letter into whatever is behind the emulator is
            // not a good way to find out a chord does not exist.
            _ => Verdict::Swallow,
        };
    }
    if !captured {
        return Verdict::PassThrough;
    }
    // A modifier is held, never sent. Sending it as a keystroke of its own is
    // what stopped `READ` and `FUNCTION` working at all: each one became a
    // keystroke in the queue, Windows repeated it while it was held, and the
    // letter it was supposed to modify ended up behind a wall of them. The
    // machine has no use for a lone `READ` anyway -- it is a chord key.
    if modifier_flag(vk, function).is_some() {
        return Verdict::Swallow;
    }
    if down {
        Verdict::Send(vk)
    } else {
        Verdict::Swallow
    }
}

/// Whether a key is one of the machine's modifiers, and which.
///
/// The chosen FUNCTION key wins over everything else, so it can even be a
/// shift key -- a keyboard that only has a natural chord key under the right
/// thumb is a real request, and the other shift keeps SHIFT. READ and CONTROL
/// are not among the choices (`function_key_named`), so they cannot be taken
/// by a setting.
fn modifier_flag(vk: u32, function: u32) -> Option<&'static AtomicBool> {
    if vk == function {
        return Some(&FUNCTION);
    }
    match vk {
        vk::LSHIFT | vk::RSHIFT => Some(&SHIFT),
        vk::LCONTROL | vk::RCONTROL => Some(&CONTROL),
        vk::LMENU => Some(&READ),
        _ => None,
    }
}

#[cfg(windows)]
pub use platform::install;

#[cfg(windows)]
mod platform {
    use super::*;

    type Hook = *mut core::ffi::c_void;

    #[repr(C)]
    struct KbdLlHookStruct {
        vk_code: u32,
        scan_code: u32,
        flags: u32,
        time: u32,
        extra: usize,
    }

    /// Is one of our own windows the one in front?
    ///
    /// By process rather than by window handle, so the confirmation dialog
    /// counts as ours too, and so this needs nothing from the window library.
    fn ours_is_in_front() -> bool {
        unsafe {
            let front = GetForegroundWindow();
            if front == 0 {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(front, &mut pid);
            pid == GetCurrentProcessId()
        }
    }

    const WH_KEYBOARD_LL: i32 = 13;
    const WM_KEYDOWN: usize = 0x0100;
    const WM_SYSKEYDOWN: usize = 0x0104;
    const HC_ACTION: i32 = 0;

    extern "system" {
        fn SetWindowsHookExW(id: i32, f: usize, module: usize, thread: u32) -> Hook;
        fn CallNextHookEx(hook: Hook, code: i32, w: usize, l: usize) -> isize;
        fn GetMessageW(msg: *mut [u32; 12], wnd: usize, min: u32, max: u32) -> i32;
        fn GetForegroundWindow() -> usize;
        fn GetWindowThreadProcessId(wnd: usize, pid: *mut u32) -> u32;
        fn GetCurrentProcessId() -> u32;
        fn MessageBoxW(wnd: usize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    const MB_YESNO: u32 = 0x0000_0004;
    const MB_ICONQUESTION: u32 = 0x0000_0020;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;
    const IDYES: i32 = 6;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Ask before quitting, in a window a screen reader can read.
    ///
    /// On its own thread, because it does not return until it is answered and
    /// the hook must never block -- a blocked keyboard hook is every key on
    /// the machine stopping, not just this one's. While it is up the hook goes
    /// transparent, or the question could not be answered.
    fn ask_before_quitting(commands: Sender<Command>) {
        std::thread::spawn(move || {
            DIALOG_UP.store(true, Ordering::Relaxed);
            let answer = unsafe {
                MessageBoxW(
                    0,
                    wide("Are you sure you wish to quit?").as_ptr(),
                    wide("VBNote").as_ptr(),
                    MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND,
                )
            };
            DIALOG_UP.store(false, Ordering::Relaxed);
            if answer == IDYES {
                let _ = commands.send(Command::Quit);
            }
        });
    }

    // Where the hook sends what it takes. A hook callback is handed no
    // context of its own, so this has to be reachable from a bare `extern`
    // function; `OnceLock` gives that without `static mut`, which is unsound
    // to take a reference to and a hard error in the 2024 edition.
    static KEYS: std::sync::OnceLock<Sender<Press>> = std::sync::OnceLock::new();
    static COMMANDS: std::sync::OnceLock<Sender<Command>> = std::sync::OnceLock::new();

    unsafe extern "system" fn hook(code: i32, wparam: usize, lparam: usize) -> isize {
        if code != HC_ACTION {
            return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
        }
        let info = &*(lparam as *const KbdLlHookStruct);
        let vk = info.vk_code;
        let down = wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN;
        let fresh = note_down(vk, down);

        // Modifier state is tracked whatever else happens to the key, or a
        // modifier let go of while released would stay down for ever.
        let function = function_key();
        if let Some(flag) = modifier_flag(vk, function) {
            flag.store(down, Ordering::Relaxed);
        }
        if vk == vk::HOST {
            HOST_DOWN.store(down, Ordering::Relaxed);
        }

        let captured = CAPTURED.load(Ordering::Relaxed);
        let host_down = HOST_DOWN.load(Ordering::Relaxed);
        // A question on screen is answered with the keyboard, so while one is
        // up this takes nothing at all.
        let focused = ours_is_in_front() && !DIALOG_UP.load(Ordering::Relaxed);
        match decide(vk, down, captured, host_down, focused, function) {
            Verdict::PassThrough => CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam),
            Verdict::Swallow => 1,
            Verdict::Run(command) => {
                // Capture is applied here, not by whoever is listening. See
                // the note at the top: this is the release valve, and it must
                // not depend on anything else still working.
                if let Command::Capture(on) = command {
                    CAPTURED.store(on, Ordering::Relaxed);
                }
                if let Some(tx) = COMMANDS.get() {
                    if command == Command::Quit {
                        // Quitting throws away whatever is on screen and ends
                        // the session, so it is worth one question.
                        ask_before_quitting(tx.clone());
                    } else {
                        let _ = tx.send(command);
                    }
                }
                1
            }
            Verdict::Send(vk) => {
                // Held keys repeat; the machine only wants the first.
                if fresh {
                    if let Some(tx) = KEYS.get() {
                        let _ = tx.send(Press { vk: vk as u8, mods: mods() });
                    }
                }
                1
            }
        }
    }

    /// Take the keyboard hook and pump it, for ever, on this thread.
    ///
    /// A low-level hook is delivered to the thread that installed it, and only
    /// while that thread is pumping messages, so this owns a thread and never
    /// returns.
    pub fn install(keys: Sender<Press>, commands: Sender<Command>) -> Result<(), String> {
        let _ = KEYS.set(keys);
        let _ = COMMANDS.set(commands);
        std::thread::spawn(move || unsafe {
            let h = SetWindowsHookExW(WH_KEYBOARD_LL, hook as *const () as usize, 0, 0);
            if h.is_null() {
                eprintln!("could not take the keyboard; the host key will not work");
                return;
            }
            let mut msg = [0u32; 12];
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
        });
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn install(_keys: Sender<Press>, _commands: Sender<Command>) -> Result<(), String> {
    Err("taking the keyboard is only implemented on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Focused and released, the host keeps its keyboard. This is the state
    /// the emulator starts in, and getting it wrong means every key in every
    /// application disappears.
    /// `decide` with the default FUNCTION key (right Alt). Most of these
    /// tests are about capture and chords, not about which key is FUNCTION.
    fn d(vk: u32, down: bool, captured: bool, host_down: bool, focused: bool) -> Verdict {
        decide(vk, down, captured, host_down, focused, vk::RMENU)
    }

    /// Focused and released, the host keeps its keyboard. This is the state
    /// the emulator starts in, and getting it wrong means every key in every
    /// application disappears.
    #[test]
    fn released_keys_go_to_the_host() {
        assert_eq!(d(b'A' as u32, true, false, false, true), Verdict::PassThrough);
        assert_eq!(d(vk::LMENU, true, false, false, true), Verdict::PassThrough);
    }

    /// Captured, nothing reaches the host and every key reaches the machine.
    #[test]
    fn captured_keys_go_to_the_machine() {
        assert_eq!(d(b'A' as u32, true, true, false, true), Verdict::Send(b'A' as u32));
        assert_eq!(d(b'A' as u32, false, true, false, true), Verdict::Swallow);
    }

    /// With another window in front this is not here at all: no key taken, no
    /// chord fired, whatever the capture setting says.
    ///
    /// It is also what keeps capture from becoming a trap. The host key needs
    /// focus, so if capture survived losing focus there would be no way to
    /// ask for the keyboard back.
    #[test]
    fn unfocused_nothing_is_taken() {
        for captured in [false, true] {
            for host_down in [false, true] {
                for vk in [b'A' as u32, vk::HOST, vk::G, vk::Q] {
                    assert_eq!(
                        d(vk, true, captured, host_down, false),
                        Verdict::PassThrough,
                        "{vk:#04x} was taken while another window was in front"
                    );
                }
            }
        }
    }

    /// The host key is the emulator's, never the machine's, in either state.
    #[test]
    fn the_host_key_is_never_sent_on() {
        for captured in [false, true] {
            for down in [false, true] {
                assert_eq!(d(vk::HOST, down, captured, false, true), Verdict::Swallow);
            }
        }
    }

    /// The four commands, and the one that has to work while captured because
    /// it is how the keyboard is given back.
    #[test]
    fn the_host_chords_command_the_emulator() {
        assert_eq!(d(vk::G, true, false, true, true), Verdict::Run(Command::Capture(true)));
        assert_eq!(d(vk::G, true, true, true, true), Verdict::Run(Command::Capture(false)));
        assert_eq!(d(vk::R, true, true, true, true), Verdict::Run(Command::Reset));
        assert_eq!(d(vk::Q, true, true, true, true), Verdict::Run(Command::Quit));
    }

    /// A host chord must never also reach the machine or the host. `host`+`Q`
    /// typing a `q` into the document it is quitting would be a poor parting
    /// gesture.
    #[test]
    fn a_host_chord_goes_nowhere_else() {
        for vk in [vk::G, vk::R, vk::Q, b'X' as u32] {
            let v = d(vk, true, true, true, true);
            assert_ne!(v, Verdict::PassThrough, "{vk:#04x} reached the host");
            assert_ne!(v, Verdict::Send(vk), "{vk:#04x} reached the machine");
        }
    }

    /// An unassigned host chord is swallowed rather than passed on, so
    /// finding out a chord does not exist cannot type into another window.
    #[test]
    fn an_unassigned_host_chord_does_nothing_at_all() {
        assert_eq!(d(b'Z' as u32, true, false, true, true), Verdict::Swallow);
    }

    /// The three keys the machine is driven by, mapped as asked: `CONTROL` on
    /// left control, `READ` on left Alt, `FUNCTION` on right Alt. Swapping
    /// two of these would put every chord on the wrong command while still
    /// looking, from the outside, like a working keyboard.
    #[test]
    fn the_modifiers_are_where_they_were_asked_for() {
        assert!(std::ptr::eq(modifier_flag(vk::LCONTROL, vk::RMENU).unwrap(), &CONTROL));
        assert!(std::ptr::eq(modifier_flag(vk::LMENU, vk::RMENU).unwrap(), &READ));
        assert!(std::ptr::eq(modifier_flag(vk::RMENU, vk::RMENU).unwrap(), &FUNCTION));
        assert!(std::ptr::eq(modifier_flag(vk::LSHIFT, vk::RMENU).unwrap(), &SHIFT));
        assert!(std::ptr::eq(modifier_flag(vk::RSHIFT, vk::RMENU).unwrap(), &SHIFT));
        // Right control is `CONTROL` too, the same way both shifts are
        // `SHIFT`. It used to be the host key; now that F11 is, a user who
        // presses it gets the modifier they meant rather than nothing.
        assert!(std::ptr::eq(modifier_flag(vk::RCONTROL, vk::RMENU).unwrap(), &CONTROL));
        // The host key is not a machine modifier.
        assert!(modifier_flag(vk::HOST, vk::RMENU).is_none());
    }

    /// A modifier is held, not sent. This is what stopped `READ` and
    /// `FUNCTION` working: each became a keystroke of its own, Windows
    /// repeated it while it was held, and the letter it modified ended up
    /// queued behind a wall of them.
    #[test]
    fn a_modifier_is_never_a_keystroke_of_its_own() {
        for vk in [vk::LMENU, vk::RMENU, vk::LCONTROL, vk::LSHIFT, vk::RSHIFT] {
            assert_eq!(
                d(vk, true, true, false, true),
                Verdict::Swallow,
                "{vk:#04x} was sent as a keystroke"
            );
        }
    }

    /// It still reaches the machine, as part of the chord.
    #[test]
    fn a_modifier_reaches_the_machine_by_being_held() {
        let kb = gandalf::keyboard::Keyboard::default();
        for vk in [vk::LMENU, vk::RMENU, vk::LCONTROL] {
            let flag = modifier_flag(vk, vk::RMENU).expect("should be a modifier");
            flag.store(true, Ordering::Relaxed);
            let held = mods();
            flag.store(false, Ordering::Relaxed);
            assert!(
                held.keys().iter().any(|k| *k as u32 == vk),
                "{vk:#04x} is not held when it is down"
            );
            assert!(kb.position_of(vk as u8).is_some());
        }
    }

    /// A key already down that reports itself down again is Windows
    /// repeating, and is not another press.
    #[test]
    fn a_repeat_is_not_a_new_press() {
        let vk = b'K' as u32;
        assert!(note_down(vk, true), "the first press is news");
        assert!(!note_down(vk, true), "the repeat is not");
        assert!(!note_down(vk, true));
        assert!(note_down(vk, false), "letting go is news");
        assert!(note_down(vk, true), "and pressing again is news");
        note_down(vk, false);
    }

    /// Every key the machine can receive is a Windows virtual-key code
    /// already, so what the hook reports needs no translating -- but it does
    /// need to be a key the matrix has.
    #[test]
    fn what_the_hook_sends_is_a_key_the_matrix_has() {
        let kb = gandalf::keyboard::Keyboard::default();
        for vk in [b'A' as u32, b'Z' as u32, b'0' as u32, 0x0D, 0x20, 0x1B, 0x2E, 0x26, 0x70] {
            match d(vk, true, true, false, true) {
                Verdict::Send(sent) => assert!(
                    kb.position_of(sent as u8).is_some(),
                    "{sent:#04x} is not on the matrix"
                ),
                other => panic!("{vk:#04x} was not sent: {other:?}"),
            }
        }
    }

    /// The machine's own modifier codes and Windows' are the same numbers, so
    /// what is held can go straight to the matrix.
    #[test]
    fn the_modifier_codes_are_the_machines_own() {
        assert_eq!(vk::LMENU as u8, gandalf::keyboard::named::READ);
        assert_eq!(vk::RMENU as u8, gandalf::keyboard::named::FUNCTION);
        assert_eq!(vk::LCONTROL as u8, gandalf::keyboard::named::CONTROL);
        assert_eq!(vk::LSHIFT as u8, gandalf::keyboard::named::SHIFT);
    }

    /// Every key `FUNCTION` can be moved to works as the modifier: held, not
    /// sent, and exactly one of them is FUNCTION for a given choice.
    #[test]
    fn the_function_key_can_be_moved() {
        const NAMES: &[&str] = &[
            "right_alt", "menu", "application", "caps_lock", "left_windows",
            "right_windows", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f12",
            "left_shift", "right_shift",
        ];
        for name in NAMES {
            let chosen = function_key_named(name).expect(name);
            // Held while captured, it is the modifier and nothing else.
            assert_eq!(
                decide(chosen, true, true, false, true, chosen),
                Verdict::Swallow,
                "{name} was sent as a keystroke"
            );
            assert_eq!(
                decide(chosen, false, true, false, true, chosen),
                Verdict::Swallow,
                "{name} release leaked"
            );
            assert!(
                std::ptr::eq(modifier_flag(chosen, chosen).unwrap(), &FUNCTION),
                "{name} is not FUNCTION"
            );
            // Right Alt is only FUNCTION while it is the choice.
            assert_eq!(
                modifier_flag(vk::RMENU, chosen).is_some(),
                *name == "right_alt",
                "right Alt ({name})"
            );
            // No other candidate becomes FUNCTION by accident. Any shift key
            // that is not the choice is still SHIFT -- a shift stays a shift
            // unless FUNCTION is moved onto it, so choosing one only takes
            // that one key.
            for other in NAMES {
                let other_vk = function_key_named(other).unwrap();
                if other_vk != chosen {
                    if matches!(other_vk, vk::LSHIFT | vk::RSHIFT) {
                        assert!(
                            std::ptr::eq(modifier_flag(other_vk, chosen).unwrap(), &SHIFT),
                            "{other} is not SHIFT alongside {name}"
                        );
                    } else {
                        assert!(
                            modifier_flag(other_vk, chosen).is_none(),
                            "{name} made {other} a modifier"
                        );
                    }
                }
            }
        }
    }

    /// Choosing right shift as FUNCTION takes that key away from SHIFT -- the
    /// chord key has to be where the right thumb can reach it, and the left
    /// shift carries every capital from then on.
    #[test]
    fn a_shift_as_function_leaves_the_other_shift_as_shift() {
        let chosen = function_key_named("right_shift").unwrap();
        assert_eq!(chosen, vk::RSHIFT);
        assert!(std::ptr::eq(modifier_flag(vk::RSHIFT, chosen).unwrap(), &FUNCTION));
        assert!(std::ptr::eq(modifier_flag(vk::LSHIFT, chosen).unwrap(), &SHIFT));
        // The chosen key is held as FUNCTION, so a chord reaches the machine
        // with FUNCTION held, not SHIFT.
        let flag = modifier_flag(vk::RSHIFT, chosen).unwrap();
        flag.store(true, Ordering::Relaxed);
        let held = mods();
        flag.store(false, Ordering::Relaxed);
        assert!(held.function);
        assert!(!held.shift);
        // Left shift still holds SHIFT.
        let flag = modifier_flag(vk::LSHIFT, chosen).unwrap();
        flag.store(true, Ordering::Relaxed);
        let held = mods();
        flag.store(false, Ordering::Relaxed);
        assert!(held.shift);
        assert!(!held.function);
    }

    /// Names that are not a key `FUNCTION` can be are refused, so a typo in
    /// the settings file says so instead of silently keeping right Alt.
    #[test]
    fn an_unknown_function_key_name_is_none() {
        for bad in ["left_alt", "f11", "f1", "shift", "control", "enter", "wibble"] {
            assert_eq!(function_key_named(bad), None, "{bad} should not be allowed");
        }
    }

    /// The chosen key is held as FUNCTION, which is what makes the chord
    /// reach the matrix with the machine's own FUNCTION code held.
    #[test]
    fn the_chosen_key_sets_the_function_flag() {
        let chosen = function_key_named("menu").unwrap();
        let flag = modifier_flag(chosen, chosen).unwrap();
        flag.store(true, Ordering::Relaxed);
        let held = mods();
        flag.store(false, Ordering::Relaxed);
        assert!(held.function, "the chosen key is not held as FUNCTION");
    }

    /// Right Alt is the default, and the machine's code agrees that it is the
    /// FUNCTION key.
    #[test]
    fn the_function_key_defaults_to_right_alt() {
        assert_eq!(function_key(), vk::RMENU);
        assert_eq!(function_key() as u8, gandalf::keyboard::named::FUNCTION);
    }
}
