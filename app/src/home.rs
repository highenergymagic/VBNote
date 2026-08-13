//! The machine's own directory, and the settings beside it.
//!
//! An installed VBNote is started from the Start menu with no arguments at
//! all. Everything it needs is in one place -- `%USERPROFILE%\.VBNote` -- put
//! there by the setup wizard:
//!
//! | file | what it is |
//! | --- | --- |
//! | `KeysoftSystemDisk.img` | the NOR flash: bootloader, image header, operating system |
//! | `FlashDisk.img` | the card the machine keeps documents on |
//! | `onewire.img` | the 1-Wire part, holding the machine's identity |
//! | `VBNote.ini` | settings, below |
//!
//! # When it is not there
//!
//! Someone who has installed VBNote and not yet run the wizard will start the
//! machine and get nothing. There is no console to print to -- it is a
//! windowed program launched from a menu -- so it says so in a dialog, which
//! is a thing a screen reader reads, and stops. Anything less is a program
//! that appears to do nothing at all.
//!
//! # The settings file
//!
//! Plain `key = value`, one per line, `#` or `;` for a comment. No sections:
//! a flat file is easier to read a line at a time, and there is not enough
//! here to need grouping. Unknown keys are reported and ignored rather than
//! being fatal, because a settings file is something a person edits and a
//! typo should not stop the machine starting.

use crate::hostkey;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What the directory is called, under the user's home.
pub const DIR: &str = ".VBNote";

pub const SYSTEM_DISK: &str = "KeysoftSystemDisk.img";
pub const FLASH_DISK: &str = "FlashDisk.img";
pub const ONEWIRE: &str = "onewire.img";
/// The removable drive files are moved on and off the machine with.
pub const USB_DISK: &str = "UsbDrive.vhd";
/// The folder on the host that it is kept in step with.
pub const USB_FOLDER: &str = "VBNote USB Drive";
pub const SETTINGS: &str = "VBNote.ini";

/// The settings file as shipped, comments and all.
///
/// Written when there is not one, so that the file a user is told they can
/// edit actually exists and explains itself.
pub const DEFAULT_SETTINGS: &str = "\
# VBNote settings.
#
# One setting per line, as `name = value`. Lines starting with # or ; are
# comments. Delete this file to get it back with the defaults.

# How fast the emulated processor is clocked, in MHz.
#
# This is not a speed dial. It is how much work the emulator promises to do
# per second of the machine's time, and promising more than this computer can
# deliver does not make the machine faster -- it makes it stutter, because the
# machine produces its speech in its own time and the sound card drains it in
# real time. 63 is measured to keep up on an ordinary machine. If speech
# breaks up, lower it. Raising it is unlikely to help.
cpu_mhz = 63

# Longest a key is held down, in milliseconds, if the machine does not look at
# the keyboard in the meantime. Only a backstop; keys are normally released as
# soon as the machine has seen them.
key_hold_ms = 800

# Which key on your keyboard is the machine's FUNCTION key.
#
# right_alt is the default, but some keyboards use right Alt for characters of
# their own (AltGr), and holding it with a letter can type one of those
# instead. If FUNCTION misbehaves on yours, move it to a key the machine would
# never use: menu (the application key), caps_lock, left_windows,
# right_windows, or f4 to f10 and f12. F11 is VBNote's own key and cannot be
# used, and F1-F3 are the machine's HELP, REPEAT and MENU.
#
# left_shift and right_shift are allowed too, at a price: the key you choose
# becomes FUNCTION and stops being SHIFT, so every capital letter and every
# shifted chord then happens on the other shift. It is the price of having the
# chord key under the thumb where the machine's FUNCTION sits.
function_key = right_alt

# How big to make the removable drive, in megabytes, the first time it is
# made. Files put in the VBNote USB Drive folder in Documents are on it when
# the machine starts, and anything it wrote is back in that folder when it stops.
#
# Changing this afterwards does nothing on its own: the drive is laid out when
# it is created, so delete UsbDrive.vhd to have a new one made. Bigger is not
# better -- asking the machine how much space is free means reading the whole
# of the drive's index, and a large drive takes noticeably longer to answer.
usb_disk_mb = 256

# Turn the sound off entirely. yes or no.
mute = no

# Show a terminal window with a running commentary, and write a status file
# beside this one. Useful when reporting a problem; off otherwise, because a
# terminal nobody asked for is one more window to get lost in. yes or no.
debug = no
";

/// Where an installed machine lives.
pub fn directory() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)?;
    Some(home.join(DIR))
}

/// Whether a machine has been set up there.
///
/// The flash disk is deliberately not required: a machine can be started with
/// a blank card, and it is the system disk and the identity that cannot be
/// invented.
pub fn is_set_up(home: &Path) -> bool {
    home.join(SYSTEM_DISK).is_file() && home.join(ONEWIRE).is_file()
}

/// Settings read from `VBNote.ini`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub cpu_mhz: u64,
    pub key_hold_ms: u64,
    /// The host key that stands in for the machine's `FUNCTION`, as a Windows
    /// virtual-key code. See `hostkey::function_key_named` for what can be
    /// chosen and how a name becomes one.
    pub function_key: u32,
    /// How big to make the removable drive, the first time, in megabytes.
    pub usb_disk_mb: u64,
    pub mute: bool,
    pub debug: bool,
    /// Lines that were not understood, for telling the user about.
    pub complaints: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            cpu_mhz: 63,
            key_hold_ms: 800,
            function_key: hostkey::vk::RMENU,
            usb_disk_mb: 256,
            mute: false,
            debug: false,
            complaints: Vec::new(),
        }
    }
}

impl Settings {
    /// Read the file, or the defaults if there is not one.
    ///
    /// Writes the shipped file when it is missing, so that the thing the user
    /// is told to edit is there to be edited.
    pub fn load(home: &Path) -> Settings {
        let path = home.join(SETTINGS);
        match std::fs::read_to_string(&path) {
            Ok(text) => Settings::parse(&text),
            Err(_) => {
                let _ = std::fs::write(&path, DEFAULT_SETTINGS);
                Settings::default()
            }
        }
    }

    pub fn parse(text: &str) -> Settings {
        let mut found: BTreeMap<String, String> = BTreeMap::new();
        let mut complaints = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            // A section header is tolerated and ignored: somebody will write
            // one out of habit, and refusing the whole file over it would be
            // rude.
            if line.starts_with('[') && line.ends_with(']') {
                continue;
            }
            match line.split_once('=') {
                Some((k, v)) => {
                    found.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
                None => complaints.push(format!("line {}: {line:?} is not `name = value`", n + 1)),
            }
        }

        let mut settings = Settings::default();
        for (key, value) in &found {
            match key.as_str() {
                "cpu_mhz" => number(value, &mut settings.cpu_mhz, key, &mut complaints),
                "key_hold_ms" => number(value, &mut settings.key_hold_ms, key, &mut complaints),
                "function_key" => match hostkey::function_key_named(value) {
                    Some(vk) => settings.function_key = vk,
                    None => complaints.push(format!(
                        "function_key = {value:?} is not a key FUNCTION can be"
                    )),
                },
                "usb_disk_mb" => number(value, &mut settings.usb_disk_mb, key, &mut complaints),
                "mute" => yes_no(value, &mut settings.mute, key, &mut complaints),
                "debug" => yes_no(value, &mut settings.debug, key, &mut complaints),
                _ => complaints.push(format!("{key} is not a setting VBNote knows")),
            }
        }
        // A clock of zero would divide by zero on the first timer tick.
        if settings.cpu_mhz == 0 {
            complaints.push("cpu_mhz cannot be 0; using 63".into());
            settings.cpu_mhz = 63;
        }
        settings.complaints = complaints;
        settings
    }
}

fn number(value: &str, into: &mut u64, key: &str, complaints: &mut Vec<String>) {
    match value.parse::<u64>() {
        Ok(v) => *into = v,
        Err(_) => complaints.push(format!("{key} = {value:?} is not a whole number")),
    }
}

fn yes_no(value: &str, into: &mut bool, key: &str, complaints: &mut Vec<String>) {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => *into = true,
        "no" | "false" | "off" | "0" => *into = false,
        _ => complaints.push(format!("{key} = {value:?} should be yes or no")),
    }
}

/// Tell the user something went wrong, in a way they can read.
///
/// A dialog rather than the console, because an installed VBNote is started
/// from a menu and has no console to print to; a message there is a message
/// nobody sees. A real dialog is also something a screen reader announces
/// without being asked.
pub fn complain(title: &str, message: &str) {
    eprintln!("{title}: {message}");
    platform::complain(title, message);
}

#[cfg(windows)]
mod platform {
    extern "system" {
        fn MessageBoxW(wnd: usize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn complain(title: &str, message: &str) {
        unsafe {
            MessageBoxW(
                0,
                wide(message).as_ptr(),
                wide(title).as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }
}

#[cfg(unix)]
mod platform {
    /// The helpers that can put a dialog on a desktop, best first.
    ///
    /// There is no `MessageBoxW` here -- no dialog is part of the system, so
    /// this asks the desktop for one. `zenity` is GTK and `kdialog` is Qt, and
    /// **both are read by Orca**, which is the whole point: a message a
    /// screen reader does not announce has not been delivered. `notify-send`
    /// is a notification rather than a dialog and is usually announced too.
    ///
    /// `xmessage` is last and deliberately so. It is raw Xlib with no AT-SPI
    /// at all, so a blind user is told nothing by it -- it is here only
    /// because a sighted user debugging a headless-ish box may still see it,
    /// and because something is better than nothing.
    const DIALOGS: &[(&str, &[&str])] = &[
        ("zenity", &["--error", "--no-markup", "--title", "{title}", "--text", "{message}"]),
        ("kdialog", &["--error", "{message}", "--title", "{title}"]),
        ("notify-send", &["-u", "critical", "{title}", "{message}"]),
        ("xmessage", &["-center", "{message}"]),
    ];

    pub fn complain(title: &str, message: &str) {
        // Nothing to put a dialog on. The `eprintln!` the caller has already
        // done is the whole of the report, which is right for a terminal.
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return;
        }
        for (program, template) in DIALOGS {
            let args = template
                .iter()
                .map(|a| a.replace("{title}", title).replace("{message}", message));
            // Waiting matters: this is said just before stopping, and a dialog
            // the process outlives is one that vanishes before it is read.
            // A helper that is not installed fails here and the next is tried.
            if let Ok(status) = std::process::Command::new(program).args(args).status() {
                if status.success() {
                    return;
                }
            }
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn complain(_title: &str, _message: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_what_the_shipped_file_says() {
        // The file a user reads and the behaviour they get have to agree, or
        // the comments in it are a lie.
        let from_file = Settings::parse(DEFAULT_SETTINGS);
        let plain = Settings::default();
        assert_eq!(from_file.cpu_mhz, plain.cpu_mhz);
        assert_eq!(from_file.key_hold_ms, plain.key_hold_ms);
        assert_eq!(from_file.function_key, plain.function_key);
        assert_eq!(from_file.mute, plain.mute);
        assert_eq!(from_file.debug, plain.debug);
        assert!(from_file.complaints.is_empty(), "{:?}", from_file.complaints);
    }

    #[test]
    fn settings_are_read() {
        let s = Settings::parse("cpu_mhz = 52\nkey_hold_ms=1200\nmute = yes\n");
        assert_eq!(s.cpu_mhz, 52);
        assert_eq!(s.key_hold_ms, 1200);
        assert!(s.mute);
        assert!(s.complaints.is_empty());
    }

    /// FUNCTION can be moved off right Alt, for keyboards that use it for
    /// characters of their own. A name it cannot be is reported, and the
    /// default is kept rather than anything half-understood.
    #[test]
    fn function_key_is_read_and_validated() {
        let s = Settings::parse("function_key = menu\n");
        assert_eq!(s.function_key, hostkey::vk::MENU);
        assert!(s.complaints.is_empty(), "{:?}", s.complaints);

        let s = Settings::parse("function_key = caps_lock\n");
        assert_eq!(s.function_key, hostkey::vk::CAPS_LOCK);

        let s = Settings::parse("function_key = right_alt\n");
        assert_eq!(s.function_key, hostkey::vk::RMENU);

        let s = Settings::parse("function_key = right_shift\n");
        assert_eq!(s.function_key, hostkey::vk::RSHIFT);

        let s = Settings::parse("function_key = f11\n");
        assert_eq!(s.function_key, hostkey::vk::RMENU, "kept the default");
        assert_eq!(s.complaints.len(), 1, "{:?}", s.complaints);
        assert!(s.complaints[0].contains("function_key"));
    }

    #[test]
    fn comments_and_blank_lines_and_sections_are_skipped() {
        let s = Settings::parse("# a comment\n; another\n\n[machine]\ncpu_mhz = 40\n");
        assert_eq!(s.cpu_mhz, 40);
        assert!(s.complaints.is_empty(), "{:?}", s.complaints);
    }

    /// A settings file is something a person edits, so a mistake in it says so
    /// and everything else still works. Refusing to start would leave them
    /// with a machine that will not run and no way to see why.
    #[test]
    fn a_bad_line_is_reported_and_the_rest_still_applies() {
        let s = Settings::parse("cpu_mhz = fast\nkey_hold_ms = 500\nwibble = 3\nnonsense\n");
        assert_eq!(s.cpu_mhz, 63, "kept the default");
        assert_eq!(s.key_hold_ms, 500, "and read the good line");
        assert_eq!(s.complaints.len(), 3, "{:?}", s.complaints);
    }

    /// Zero would divide by zero the first time a timer ticked.
    #[test]
    fn a_clock_of_zero_is_refused() {
        let s = Settings::parse("cpu_mhz = 0\n");
        assert_eq!(s.cpu_mhz, 63);
        assert_eq!(s.complaints.len(), 1);
    }

    #[test]
    fn yes_and_no_are_generous() {
        for (text, want) in [("yes", true), ("YES", true), ("on", true), ("1", true),
                             ("no", false), ("Off", false), ("0", false)] {
            let s = Settings::parse(&format!("mute = {text}\n"));
            assert_eq!(s.mute, want, "{text}");
            assert!(s.complaints.is_empty(), "{text}: {:?}", s.complaints);
        }
    }

    /// A machine is set up when the two things that cannot be invented are
    /// there. The card is not one of them.
    #[test]
    fn set_up_means_the_system_disk_and_the_identity() {
        let dir = std::env::temp_dir().join(format!("vbnote-home-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(!is_set_up(&dir));
        std::fs::write(dir.join(SYSTEM_DISK), b"x").unwrap();
        assert!(!is_set_up(&dir), "the identity is missing");
        std::fs::write(dir.join(ONEWIRE), b"x").unwrap();
        assert!(is_set_up(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
