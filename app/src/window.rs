//! A window, so the machine can be typed at.
//!
//! KeySoft is driven entirely from the keyboard and answers with speech. That
//! makes the window an odd thing: it has nothing to show and nobody to show it
//! to. What it is for is **keyboard focus** — an operating system will only
//! deliver keystrokes to a window, so there has to be one.
//!
//! It is therefore deliberately plain, and deliberately does not ask anything
//! of a user who cannot see it. There is nothing to read, nothing to click,
//! and no state that matters visually. Close it and the machine stops.
//!
//! # It no longer carries the keys on Windows
//!
//! It cannot. Its key-state table has no entry for either Alt, so `READ` and
//! `FUNCTION` -- most of what KeySoft is driven with -- read as never held
//! however they are bound. A keyboard hook takes the keys instead; see
//! `hostkey`. This still translates, for platforms with no hook, and the
//! window is still wanted on every platform, because closing it is how the
//! machine is stopped.
//!
//! Modifiers are read as modifiers, not sent as keystrokes of their own: what
//! goes to the guest is one keystroke with things held down, which is what a
//! chord is.

use crate::keys::{press_for, Mods, Press};
use gandalf::keyboard::named;
use minifb::{Key, Window, WindowOptions};
use std::sync::mpsc::Sender;

/// Keys that are not characters at all.
///
/// Enter is the important one for a first run: a prompt that says "press enter
/// for English" cannot be answered any other way. The rest are the keys the
/// machine has that a character cannot spell, which is why they were
/// unreachable for so long.
fn special(key: Key) -> Option<u8> {
    Some(match key {
        Key::Enter | Key::NumPadEnter => 0x0D,
        Key::Space => 0x20,
        Key::Backspace => 0x08,
        Key::Tab => 0x09,
        Key::Escape => 0x1B,
        Key::Delete => named::DELETE,
        Key::Left => named::LEFT,
        Key::Right => named::RIGHT,
        Key::Up => named::UP,
        Key::Down => named::DOWN,
        Key::F1 => named::HELP,
        Key::F2 => named::REPEAT,
        Key::F3 => named::MENU,
        _ => return None,
    })
}

/// What the host is holding down, as the machine's own modifiers.
fn held(window: &Window) -> Mods {
    Mods {
        shift: window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift),
        control: window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl),
        read: window.is_key_down(Key::LeftAlt),
        function: window.is_key_down(Key::RightAlt),
    }
}

/// The unshifted character a printable key stands for.
///
/// Shift is not applied here. It is a modifier the guest holds down, and
/// folding it into the character would send `A` as a keystroke with no shift
/// rather than as shift with `a` — the same letter, but not the same chord,
/// and KeySoft has chords on shifted keys.
fn printable(key: Key) -> Option<char> {
    let c = match key {
        Key::A => 'a', Key::B => 'b', Key::C => 'c', Key::D => 'd',
        Key::E => 'e', Key::F => 'f', Key::G => 'g', Key::H => 'h',
        Key::I => 'i', Key::J => 'j', Key::K => 'k', Key::L => 'l',
        Key::M => 'm', Key::N => 'n', Key::O => 'o', Key::P => 'p',
        Key::Q => 'q', Key::R => 'r', Key::S => 's', Key::T => 't',
        Key::U => 'u', Key::V => 'v', Key::W => 'w', Key::X => 'x',
        Key::Y => 'y', Key::Z => 'z',
        Key::Key0 => '0', Key::Key1 => '1', Key::Key2 => '2', Key::Key3 => '3',
        Key::Key4 => '4', Key::Key5 => '5', Key::Key6 => '6', Key::Key7 => '7',
        Key::Key8 => '8', Key::Key9 => '9',
        Key::Comma => ',', Key::Period => '.', Key::Slash => '/',
        Key::Semicolon => ';', Key::Apostrophe => '\'', Key::Minus => '-',
        Key::Equal => '=', Key::LeftBracket => '[', Key::RightBracket => ']',
        Key::Backslash => '\\', Key::Backquote => '`',
        _ => return None,
    };
    Some(c)
}

/// The keystroke a host key becomes, with whatever is held down.
fn translate(key: Key, mods: Mods) -> Option<Press> {
    let vk = match special(key) {
        Some(vk) => vk,
        None => press_for(printable(key)?)?.vk,
    };
    Some(Press { vk, mods })
}

/// Every key this window is willing to translate.
///
/// The modifiers are deliberately not in here: they are read as held, not
/// delivered as keystrokes, so pressing `READ` on its own sends nothing and
/// pressing it with `T` sends one chord.
fn keys_of_interest() -> Vec<Key> {
    let mut v = vec![
        Key::Enter, Key::NumPadEnter, Key::Space, Key::Backspace, Key::Tab, Key::Escape,
        Key::Comma, Key::Period, Key::Slash, Key::Semicolon, Key::Apostrophe,
        Key::Minus, Key::Equal, Key::LeftBracket, Key::RightBracket,
        Key::Backslash, Key::Backquote,
        Key::Delete, Key::Left, Key::Right, Key::Up, Key::Down,
        Key::F1, Key::F2, Key::F3,
    ];
    v.extend([
        Key::A, Key::B, Key::C, Key::D, Key::E, Key::F, Key::G, Key::H, Key::I,
        Key::J, Key::K, Key::L, Key::M, Key::N, Key::O, Key::P, Key::Q, Key::R,
        Key::S, Key::T, Key::U, Key::V, Key::W, Key::X, Key::Y, Key::Z,
    ]);
    v.extend([
        Key::Key0, Key::Key1, Key::Key2, Key::Key3, Key::Key4,
        Key::Key5, Key::Key6, Key::Key7, Key::Key8, Key::Key9,
    ]);
    v
}

/// Open the window and relay keystrokes until it is closed.
///
/// Runs on its own thread: the window is created and pumped there, which is
/// what the host requires, and it keeps the emulator's loop free of any
/// obligation to service a message queue on time.
pub fn run_without_keys() {
    run(None)
}

/// Open the window and relay keystrokes until it is closed.
///
/// `keys` is `None` when something else is taking the keyboard -- on Windows a
/// hook does, because this library cannot see either Alt and so cannot carry
/// `READ` or `FUNCTION` at all. The window is still wanted: it is what the
/// user closes to stop the machine.
pub fn run(keys: Option<Sender<Press>>) {
    let mut window = match Window::new(
        "VBNote — VoiceNote QT mPower (type here; the machine speaks)",
        480,
        160,
        WindowOptions::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("keyboard: no window ({e}); falling back to nothing");
            return;
        }
    };
    // Repeat is the host's business, not ours: a menu that answers single
    // keys should not receive four of them because a key was held.
    window.set_target_fps(60);

    // Nothing is drawn, but the buffer has to exist for the window to appear.
    let blank = vec![0u32; 480 * 160];
    let watch = keys_of_interest();
    let mut down = vec![false; watch.len()];

    while window.is_open() {
        let mods = held(&window);
        for (i, key) in watch.iter().enumerate() {
            let now = window.is_key_down(*key);
            // On the edge only. Holding a key sends one keystroke, the way a
            // menu expects, and the guest decides how long to hold the matrix
            // line down for its scanner.
            if now && !down[i] {
                if let (Some(keys), Some(press)) = (keys.as_ref(), translate(*key, mods)) {
                    if keys.send(press).is_err() {
                        return;
                    }
                }
            }
            down[i] = now;
        }
        if window.update_with_buffer(&blank, 480, 160).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A first-run prompt that says "press enter for English" can only be
    /// answered with Enter, so it has to reach the guest as Enter.
    #[test]
    fn enter_reaches_the_guest_as_enter() {
        assert_eq!(special(Key::Enter), Some(0x0D));
        assert_eq!(special(Key::NumPadEnter), Some(0x0D));
    }

    /// The three keys this change is for. Getting left and right Alt the
    /// wrong way round would put every chord on the wrong command while
    /// looking, from the outside, as though the keyboard worked.
    #[test]
    fn alt_and_ctrl_are_the_machines_own_modifiers() {
        assert_eq!(Mods { read: true, ..Default::default() }.keys(), vec![named::READ]);
        assert_eq!(
            Mods { function: true, ..Default::default() }.keys(),
            vec![named::FUNCTION]
        );
        assert_eq!(
            Mods { control: true, ..Default::default() }.keys(),
            vec![named::CONTROL]
        );
        // READ is 0xA4, which is left Alt; FUNCTION is 0xA5, which is right.
        assert_eq!(named::READ, 0xA4);
        assert_eq!(named::FUNCTION, 0xA5);
    }

    /// A chord is one keystroke with something held, not two keystrokes.
    #[test]
    fn a_modifier_rides_along_with_the_key() {
        let mods = Mods { read: true, ..Default::default() };
        let press = translate(Key::T, mods).unwrap();
        assert_eq!(press.vk, b'T');
        assert!(press.mods.read);
    }

    /// Shift stays a modifier rather than becoming a different character, so
    /// a chord on a shifted key is still a chord.
    #[test]
    fn shift_is_held_not_folded_into_the_letter() {
        let press = translate(Key::A, Mods { shift: true, ..Default::default() }).unwrap();
        assert_eq!(press.vk, b'A');
        assert!(press.mods.shift);
        let plain = translate(Key::A, Mods::default()).unwrap();
        assert_eq!(plain.vk, press.vk);
        assert!(!plain.mods.shift);
    }

    #[test]
    fn letters_and_digits_translate() {
        assert_eq!(printable(Key::A), Some('a'));
        assert_eq!(printable(Key::Key1), Some('1'));
        assert_eq!(translate(Key::Key1, Mods::default()).unwrap().vk, b'1');
    }

    /// Every key the window watches has to translate to a key the matrix
    /// actually has, or it is watched for nothing and the omission is
    /// invisible. This is the test that would have caught the arrows, Delete
    /// and the function keys being unreachable.
    #[test]
    fn every_watched_key_reaches_the_matrix() {
        let kb = gandalf::keyboard::Keyboard::default();
        for key in keys_of_interest() {
            let press = translate(key, Mods::default())
                .unwrap_or_else(|| panic!("{key:?} is watched but translates to nothing"));
            assert!(
                kb.position_of(press.vk).is_some(),
                "{key:?} maps to {:#04x}, which is not on the matrix",
                press.vk
            );
        }
    }

    /// The keys that have no character at all are watched, which is the gap
    /// this closed.
    #[test]
    fn the_keys_a_character_cannot_spell_are_watched() {
        let watched = keys_of_interest();
        for key in [
            Key::Delete, Key::Left, Key::Right, Key::Up, Key::Down,
            Key::F1, Key::F2, Key::F3,
        ] {
            assert!(watched.contains(&key), "{key:?} is not watched");
        }
    }

    /// The modifiers must not also be delivered as keystrokes: pressing READ
    /// on its own is not a command, and sending it as one would type
    /// something every time a chord was started.
    #[test]
    fn modifiers_are_not_watched_as_keys() {
        for key in [Key::LeftAlt, Key::RightAlt, Key::LeftCtrl, Key::RightCtrl,
                    Key::LeftShift, Key::RightShift] {
            assert!(!keys_of_interest().contains(&key), "{key:?} should be held, not sent");
        }
    }
}
