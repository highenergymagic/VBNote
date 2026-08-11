//! What the host typed, in terms the key matrix understands.
//!
//! The machine is driven entirely from its keyboard, and most of what KeySoft
//! can do is a **chord**: `READ` with `T` speaks the time, `FUNCTION` with a
//! letter reaches a menu. So a keystroke is not a character. It is a key, plus
//! whatever was being held down at the time.
//!
//! This used to be a `char`, which quietly put half the keyboard out of reach:
//! `READ`, `FUNCTION`, `CONTROL`, the arrows, `DELETE`, `HELP`, `MENU` and
//! `REPEAT` are all on the matrix, all scanned, and none of them spell
//! anything. Nothing was broken, so nothing looked broken -- everything that
//! types still typed.
//!
//! # The notation
//!
//! KeySoft has a keystroke notation of its own, and this uses it rather than
//! inventing one: modifiers and named keys in square brackets, ordinary
//! characters as themselves.
//!
//! ```text
//! [READ]t         READ with T
//! [FN][UP]        FUNCTION with the up arrow
//! hello[ENTER]    six keystrokes
//! ```
//!
//! The names are the ones in the ROM, from the table at `0x00230e70` and the
//! parser at `0x000f0998`, so a command written out of a KeySoft manual can be
//! typed at this emulator unchanged. Two of them read oddly and are not
//! mistakes: `[SINGLEQUOTE]` is the backtick and `[APOSTROPHE]` is the
//! apostrophe.

use gandalf::keyboard::named;

/// Modifiers held down while a key is pressed.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub read: bool,
    pub function: bool,
}

impl Mods {
    pub fn is_empty(self) -> bool {
        self == Mods::default()
    }

    /// The matrix keys to hold, in the order they go down.
    pub fn keys(self) -> Vec<u8> {
        let mut v = Vec::new();
        for (on, vk) in [
            (self.shift, named::SHIFT),
            (self.control, named::CONTROL),
            (self.read, named::READ),
            (self.function, named::FUNCTION),
        ] {
            if on {
                v.push(vk);
            }
        }
        v
    }
}

/// One keystroke: a key, and what was held down with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Press {
    pub vk: u8,
    pub mods: Mods,
}

impl Press {
    pub fn plain(vk: u8) -> Self {
        Press { vk, mods: Mods::default() }
    }

    pub fn shifted(vk: u8) -> Self {
        Press { vk, mods: Mods { shift: true, ..Mods::default() } }
    }

    /// The same keystroke with more held down.
    pub fn with(mut self, mods: Mods) -> Self {
        self.mods.shift |= mods.shift;
        self.mods.control |= mods.control;
        self.mods.read |= mods.read;
        self.mods.function |= mods.function;
        self
    }
}

/// The keys KeySoft names in its own notation, with the codes it presses them
/// as. Straight out of the ROM; see the module comment.
const NAMED: &[(&str, u8)] = &[
    ("ESC", 0x1B),
    ("DASH", 0xBD),
    ("EQUALS", 0xBB),
    ("BKS", 0x08),
    ("TAB", 0x09),
    ("LBRACKET", 0xDB),
    ("RBRACKET", 0xDD),
    ("BACKSLASH", 0xDC),
    ("SEMICOLON", 0xBA),
    ("APOSTROPHE", 0xDE),
    ("ENTER", 0x0D),
    ("COMMA", 0xBC),
    ("PERIOD", 0xBE),
    ("UP", named::UP),
    ("SLASH", 0xBF),
    ("HELP", named::HELP),
    ("MENU", named::MENU),
    ("SPC", 0x20),
    ("RPT", named::REPEAT),
    ("SINGLEQUOTE", 0xC0),
    ("DEL", named::DELETE),
    ("LEFT", named::LEFT),
    ("DOWN", named::DOWN),
    ("RIGHT", named::RIGHT),
];

/// A name that only sets a modifier rather than pressing anything.
fn modifier_named(name: &str) -> Option<Mods> {
    let mut m = Mods::default();
    match name {
        "READ" => m.read = true,
        "SHIFT" => m.shift = true,
        "CTRL" => m.control = true,
        "FN" => m.function = true,
        _ => return None,
    }
    Some(m)
}

fn key_named(name: &str) -> Option<u8> {
    NAMED.iter().find(|(n, _)| *n == name).map(|(_, vk)| *vk)
}

/// The keystroke a character stands for, with shift where the US layout needs
/// it.
pub fn press_for(c: char) -> Option<Press> {
    let plain = |v: u8| Some(Press::plain(v));
    let shifted = |v: u8| Some(Press::shifted(v));
    match c {
        'a'..='z' => plain(c.to_ascii_uppercase() as u8),
        'A'..='Z' => shifted(c as u8),
        '0'..='9' => plain(c as u8),
        ' ' => plain(0x20),
        '\r' | '\n' => plain(0x0D),
        '\t' => plain(0x09),
        '\x08' | '\x7f' => plain(0x08),
        '\x1b' => plain(0x1B),
        // OEM keys, by the codes the scan table uses.
        '`' => plain(0xC0),
        '-' => plain(0xBD),
        '=' => plain(0xBB),
        '[' => plain(0xDB),
        ']' => plain(0xDD),
        '\\' => plain(0xDC),
        ';' => plain(0xBA),
        '\'' => plain(0xDE),
        ',' => plain(0xBC),
        '.' => plain(0xBE),
        '/' => plain(0xBF),
        '!' => shifted(b'1'),
        '@' => shifted(b'2'),
        '#' => shifted(b'3'),
        '$' => shifted(b'4'),
        '%' => shifted(b'5'),
        '^' => shifted(b'6'),
        '&' => shifted(b'7'),
        '*' => shifted(b'8'),
        '(' => shifted(b'9'),
        ')' => shifted(b'0'),
        '_' => shifted(0xBD),
        '+' => shifted(0xBB),
        ':' => shifted(0xBA),
        '"' => shifted(0xDE),
        '<' => shifted(0xBC),
        '>' => shifted(0xBE),
        '?' => shifted(0xBF),
        '{' => shifted(0xDB),
        '}' => shifted(0xDD),
        '|' => shifted(0xDC),
        '~' => shifted(0xC0),
        _ => None,
    }
}

/// Turn a script into keystrokes.
///
/// Bracketed names accumulate: a modifier is held for the next real key, and a
/// named key is that key. Anything unrecognised is reported rather than
/// dropped, because a typo in a script that silently types nothing is a long
/// afternoon.
pub fn parse(text: &str) -> (Vec<Press>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let mut pending = Mods::default();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '[' {
            match press_for(c) {
                Some(p) => {
                    out.push(p.with(pending));
                    pending = Mods::default();
                }
                None => problems.push(format!("no key on this keyboard for {c:?}")),
            }
            continue;
        }
        // A bracketed name. An unterminated one is an error, not a literal.
        let mut name = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == ']' {
                closed = true;
                break;
            }
            name.push(c);
        }
        if !closed {
            problems.push(format!("[{name} has no closing bracket"));
            continue;
        }
        let upper = name.to_ascii_uppercase();
        if let Some(m) = modifier_named(&upper) {
            pending = pending.with_mods(m);
        } else if let Some(vk) = key_named(&upper) {
            out.push(Press { vk, mods: pending });
            pending = Mods::default();
        } else {
            problems.push(format!("[{name}] is not a key this machine has"));
        }
    }
    if !pending.is_empty() {
        problems.push("the script ends with a modifier held and nothing to press".into());
    }
    (out, problems)
}

impl Mods {
    fn with_mods(mut self, other: Mods) -> Self {
        self.shift |= other.shift;
        self.control |= other.control;
        self.read |= other.read;
        self.function |= other.function;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letter_is_one_plain_keystroke() {
        let (keys, bad) = parse("a");
        assert!(bad.is_empty());
        assert_eq!(keys, vec![Press::plain(b'A')]);
    }

    #[test]
    fn a_capital_holds_shift() {
        let (keys, _) = parse("A");
        assert_eq!(keys, vec![Press::shifted(b'A')]);
    }

    /// The chord the whole change is for. KeySoft's own manuals write it this
    /// way, so it should be typeable this way.
    #[test]
    fn read_with_a_letter_is_one_chord() {
        let (keys, bad) = parse("[READ]t");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(keys.len(), 1, "a chord is one keystroke, not two");
        assert_eq!(keys[0].vk, b'T');
        assert!(keys[0].mods.read);
        assert_eq!(keys[0].mods.keys(), vec![named::READ]);
    }

    #[test]
    fn function_and_control_reach_their_own_keys() {
        let (keys, _) = parse("[FN]m");
        assert_eq!(keys[0].mods.keys(), vec![named::FUNCTION]);
        let (keys, _) = parse("[CTRL]c");
        assert_eq!(keys[0].mods.keys(), vec![named::CONTROL]);
    }

    /// Read and Function must not collapse into one another; they are
    /// different keys in different columns of the matrix.
    #[test]
    fn read_and_function_stay_apart() {
        let (read, _) = parse("[READ]a");
        let (func, _) = parse("[FN]a");
        assert_ne!(read[0].mods, func[0].mods);
        assert_ne!(read[0].mods.keys(), func[0].mods.keys());
    }

    #[test]
    fn modifiers_stack() {
        let (keys, bad) = parse("[READ][CTRL]x");
        assert!(bad.is_empty());
        assert_eq!(keys[0].mods.keys(), vec![named::CONTROL, named::READ]);
    }

    /// A named key can be the key of a chord, not only a modifier.
    #[test]
    fn a_named_key_can_be_modified() {
        let (keys, bad) = parse("[FN][UP]");
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(keys, vec![Press { vk: named::UP, mods: Mods { function: true, ..Default::default() } }]);
    }

    #[test]
    fn every_rom_name_is_a_key_the_matrix_has() {
        let kb = gandalf::keyboard::Keyboard::default();
        for (name, vk) in NAMED {
            assert!(kb.position_of(*vk).is_some(), "[{name}] ({vk:#04x}) is not on the matrix");
        }
        for name in ["READ", "SHIFT", "CTRL", "FN"] {
            let m = modifier_named(name).unwrap();
            for vk in m.keys() {
                assert!(kb.position_of(vk).is_some(), "[{name}] holds a key that is not there");
            }
        }
    }

    /// A mistyped script says so. Dropping it silently is what made the
    /// missing keys invisible in the first place.
    #[test]
    fn unknown_names_are_reported() {
        let (keys, bad) = parse("[WIBBLE]a");
        assert_eq!(keys.len(), 1, "the rest of the script still runs");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("WIBBLE"));

        let (_, bad) = parse("[UP");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("closing bracket"));

        let (_, bad) = parse("a[READ]");
        assert!(bad[0].contains("modifier held"));
    }

    /// Square brackets are also keys on this keyboard, and a script that
    /// wants one asks for it by name.
    #[test]
    fn the_bracket_keys_are_still_reachable() {
        let (keys, bad) = parse("[LBRACKET][RBRACKET]");
        assert!(bad.is_empty());
        assert_eq!(keys, vec![Press::plain(0xDB), Press::plain(0xDD)]);
    }

    #[test]
    fn a_whole_line_becomes_a_keystroke_each() {
        let (keys, bad) = parse("hi[ENTER]");
        assert!(bad.is_empty());
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[2].vk, 0x0D);
    }

    /// Names are matched without regard to case, because a manual will write
    /// them one way and a script another.
    #[test]
    fn names_are_case_insensitive() {
        let (upper, _) = parse("[READ]t");
        let (lower, _) = parse("[read]t");
        assert_eq!(upper, lower);
    }
}
