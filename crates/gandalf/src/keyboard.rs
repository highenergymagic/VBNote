//! The VoiceNote QT keyboard, scanned through the CPLD.
//!
//! `pdikeybd.dll`'s scan routine (`FUN_023025cc` in the decompilation) drives
//! CPLD register index 1 — offset `0x402` — like this:
//!
//! ```text
//! for column in 0..12 {
//!     write(1, 0x0d);          // idle
//!     read(1);                 // discarded
//!     delay(10);
//!     write(1, column);        // select
//!     read(1);                 // discarded
//!     delay(10);
//!     rows = read(1);          // eight row bits for this column
//!     delay(10);
//! }
//! write(1, 0x0c);              // park
//! ```
//!
//! Three reads per two writes, which is exactly the 43327:29275 ratio
//! observed while EBOOT's menu waited for a key. The driver packs the result
//! into six 16-bit words, even columns in the high byte and odd columns in
//! the low byte, giving 12 x 8 = 96 key positions.
//!
//! The virtual-key code for each position comes from a 96-byte table in
//! `pdikeybd.dll`, one 8-byte column at a time. Eight ship; this is the
//! VoiceNote English one at file offset 0x11a0.

/// GPIO the board pulls low while any key is down.
///
/// Windows CE does not poll this matrix. Over a whole boot the scan register
/// sees 76 reads, against 43327 while EBOOT's menu waited for a key, so
/// `pdikeybd.dll` waits on an interrupt and only scans when it fires. Driving
/// this pin alongside the matrix takes those 76 reads to 3480 and makes the
/// driver actually see the key; GPIOs 18, 20 and 101, the other pins the
/// kernel arms for both edges, change nothing.
pub const KEY_DOWN_GPIO: u32 = 11;

/// Column selector meaning "no column driven".
const COLUMN_IDLE: u16 = 0x0D;
const COLUMN_PARK: u16 = 0x0C;
pub const COLUMNS: usize = 12;
pub const ROWS: usize = 8;

/// Windows virtual-key codes by [column][row], from `pdikeybd.dll` at
/// `0x11a0`. Zero means no key at that position.
///
/// The driver carries eight of these and KeySoft chooses one at run time, so
/// this has to be the one it chose or every key comes out as a different key —
/// silently, looking for all the world like a wiring fault. `0x11a0` is the
/// VoiceNote English table, which KeySoft selects when
/// [`crate::patch::KEYSOFT_IS_A_VOICENOTE`] has told it what machine this is.
///
/// Which table for which machine, and how the choice is made, is in
/// `docs/hardware.md`.
pub const VOICENOTE_LAYOUT: [[u8; ROWS]; COLUMNS] = [
    [0x00, 0x00, 0x72, 0x5A, 0x31, 0x51, 0x00, 0x00], // MENU Z 1 Q
    [0x70, 0x00, 0x00, 0x71, 0x1B, 0xA1, 0xA0, 0xA2], // HELP RPT ESC RSHIFT LSHIFT CTRL
    [0x20, 0x00, 0x00, 0x43, 0x33, 0x45, 0x53, 0x00], // SPACE C 3 E S
    [0x00, 0x00, 0x00, 0x58, 0x32, 0x57, 0x41, 0x71], // X 2 W A RPT
    [0x00, 0x00, 0x00, 0x42, 0x35, 0x54, 0x46, 0x00], // B 5 T F
    [0x09, 0x00, 0x00, 0x56, 0x34, 0x52, 0x44, 0x00], // TAB V 4 R D
    [0x2E, 0x25, 0xDB, 0x4D, 0x37, 0x55, 0x48, 0x00], // DEL LEFT [ M 7 U H
    [0xC0, 0x00, 0x00, 0x4E, 0x36, 0x59, 0x47, 0x00], // ` N 6 Y G
    [0x00, 0x27, 0xDC, 0xBE, 0x39, 0x4F, 0x4B, 0x00], // RIGHT \ . 9 O K
    [0x00, 0x28, 0xDD, 0xBC, 0x38, 0x49, 0x4A, 0x00], // DOWN ] , 8 I J
    [0x00, 0x08, 0x0D, 0xA5, 0xBD, 0xBB, 0xBA, 0x00], // BACK ENTER FUNCTION - + ;
    [0xA4, 0x26, 0xDE, 0xBF, 0x30, 0x50, 0x4C, 0x00], // READ UP ' / 0 P L
];

/// The keys the machine is labelled with that are not letters, digits or
/// punctuation, with the codes KeySoft presses them as.
///
/// These are not guesses from the shape of the table. KeySoft carries a
/// keystroke notation of its own -- the strings `[READ]`, `[SHIFT]`, `[CTRL]`,
/// `[FN]` and two dozen more -- and the routine at `0x000f0998` that parses it
/// presses a specific code for each. `[READ]` sets the flag released with
/// `mov r0, #0xa4`, `[FN]` the one released with `mov r0, #0xa5`. The rest come
/// from a table of (name, code) pairs at `0x00230e70`, where the low byte
/// beside each code is the length of its own name, which is how a
/// misalignment would have shown up.
///
/// `docs/hardware.md` has the whole table.
pub mod named {
    /// The QT keyboard's chord key. It is what `SPACE` is on a braille model:
    /// the same message ships twice, "press SPACE with I" and "press READ
    /// with I".
    pub const READ: u8 = 0xA4;
    /// Labelled `FUNCTION` on the machine, `[FN]` in KeySoft's notation.
    pub const FUNCTION: u8 = 0xA5;
    /// The matrix carries the left-hand code; KeySoft presses the
    /// side-agnostic `0x11`, and Windows CE treats them as the same key.
    pub const CONTROL: u8 = 0xA2;
    pub const SHIFT: u8 = 0xA0;
    pub const HELP: u8 = 0x70;
    /// Say it again.
    pub const REPEAT: u8 = 0x71;
    pub const MENU: u8 = 0x72;
    pub const DELETE: u8 = 0x2E;
    pub const LEFT: u8 = 0x25;
    pub const UP: u8 = 0x26;
    pub const RIGHT: u8 = 0x27;
    pub const DOWN: u8 = 0x28;
}

pub struct Keyboard {
    /// Row bits per column; bit `r` set means the key at row `r` is down.
    ///
    /// Which bit corresponds to which row is inferred from the driver packing
    /// eight bits per column and the scan table holding eight entries per
    /// column. If a key ever comes out transposed, this is the assumption to
    /// revisit.
    pub columns: [u8; COLUMNS],
    /// Column most recently selected, or `None` while idle or parked.
    pub selected: Option<usize>,
    pub layout: [[u8; ROWS]; COLUMNS],
    /// Scans that came back with a key down. Pressing a key into the matrix
    /// is only half the job; this says whether the guest's scanner ever
    /// looked at the right column while it was held.
    pub scans_seen: u64,
    /// The same, per hardware column, so a particular key can be asked about
    /// rather than the matrix as a whole.
    pub scans_by_column: [u64; COLUMNS],
    /// Complete sweeps of the matrix.
    ///
    /// The driver parks the selector when it has been round all twelve
    /// columns, so a park is the end of a sweep and this counts them. It is
    /// what says whether the guest has *finished looking*, which neither a
    /// total nor a per-column count can: with a chord held down the totals
    /// climb faster because more keys answer, and a per-column count reaches
    /// its target at a different moment for the modifier than for the key it
    /// modifies. Sweeps are the same question however many keys are down.
    pub sweeps: u64,
    /// Every read of the scan register, whether or not a key was under it.
    /// Without this a key that does nothing cannot be told from a driver that
    /// never looked.
    pub reads: u64,
}

impl Default for Keyboard {
    fn default() -> Self {
        Keyboard {
            columns: [0; COLUMNS],
            selected: None,
            layout: VOICENOTE_LAYOUT,
            scans_seen: 0,
            scans_by_column: [0; COLUMNS],
            sweeps: 0,
            reads: 0,
        }
    }
}

impl Keyboard {
    /// Find the matrix position of a Windows virtual-key code.
    pub fn position_of(&self, vk: u8) -> Option<(usize, usize)> {
        if vk == 0 {
            return None;
        }
        for (c, col) in self.layout.iter().enumerate() {
            for (r, k) in col.iter().enumerate() {
                if *k == vk {
                    return Some((c, r));
                }
            }
        }
        None
    }

    pub fn set_key(&mut self, vk: u8, down: bool) -> bool {
        match self.position_of(vk) {
            Some((c, r)) => {
                if down {
                    self.columns[c] |= 1 << r;
                } else {
                    self.columns[c] &= !(1 << r);
                }
                true
            }
            None => false,
        }
    }

    /// How many scans have looked at this key's column and found something
    /// down. `None` if the key is not on this keyboard at all.
    ///
    /// The column the hardware drives is the table column with its low bit
    /// flipped; see [`Keyboard::read_rows`].
    pub fn scans_of(&self, vk: u8) -> Option<u64> {
        self.position_of(vk).map(|(c, _)| self.scans_by_column[c ^ 1])
    }

    pub fn release_all(&mut self) {
        self.columns = [0; COLUMNS];
    }

    pub fn any_down(&self) -> bool {
        self.columns.iter().any(|c| *c != 0)
    }

    /// A write to the scan register selects a column.
    pub fn write_select(&mut self, value: u16) {
        let v = value & 0xFF;
        if v == COLUMN_PARK {
            self.sweeps += 1;
        }
        self.selected = if v == COLUMN_IDLE || v == COLUMN_PARK || v as usize >= COLUMNS {
            None
        } else {
            Some(v as usize)
        };
    }

    /// A read of the scan register returns the selected column's row bits.
    ///
    /// The column the hardware drives and the column the key table is indexed
    /// by are not the same number: they differ by one in the low bit. The
    /// driver packs its twelve columns into six 16-bit words, one pair per
    /// word, and reads the pair back out in the opposite order to the one it
    /// put them in — so within each pair the two columns trade places.
    ///
    /// It is invisible in the letters, which is why it survived so long. Every
    /// letter and digit lands on a key that still types *something*, so the
    /// keyboard looks like it works. It shows up on the punctuation, where a
    /// pair happens to straddle two keys somebody presses on purpose: Enter at
    /// table column 10 comes back as the apostrophe at column 11, and
    /// backslash at 8 comes back as the right bracket at 9.
    pub fn read_rows(&mut self) -> u16 {
        self.reads += 1;
        let rows = match self.selected {
            Some(c) => self.columns[c ^ 1] as u16,
            None => 0,
        };
        if rows != 0 {
            self.scans_seen += 1;
            if let Some(c) = self.selected {
                self.scans_by_column[c] += 1;
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VK_SPACE: u8 = 0x20;
    const VK_A: u8 = 0x41;
    const VK_RETURN: u8 = 0x0D;

    #[test]
    fn every_layout_column_holds_eight_entries() {
        assert_eq!(VOICENOTE_LAYOUT.len(), COLUMNS);
        assert_eq!(VOICENOTE_LAYOUT.iter().flatten().count(), COLUMNS * ROWS);
    }

    #[test]
    fn the_table_covers_a_qwerty_keyboard() {
        let kb = Keyboard::default();
        for vk in b"QWERTYUIOPASDFGHJKL1234567890" {
            assert!(kb.position_of(*vk).is_some(), "missing key {}", *vk as char);
        }
        for vk in [VK_SPACE, VK_RETURN, 0x08, 0x09, 0x1B, 0xA0, 0xA1] {
            assert!(kb.position_of(vk).is_some(), "missing control key {vk:#04x}");
        }
    }

    #[test]
    fn a_pressed_key_appears_only_in_its_own_column() {
        let mut kb = Keyboard::default();
        let (col, row) = kb.position_of(VK_A).unwrap();
        assert!(kb.set_key(VK_A, true));

        // The hardware column is the table column with the low bit flipped.
        kb.write_select((col ^ 1) as u16);
        assert_eq!(kb.read_rows(), 1 << row);

        for other in 0..COLUMNS {
            if other == col ^ 1 {
                continue;
            }
            kb.write_select(other as u16);
            assert_eq!(kb.read_rows(), 0, "column {other} should be clear");
        }
    }

    /// Press every key and read it back the way the guest does, which is the
    /// test that would have caught the pairs being swapped.
    ///
    /// The driver scans a hardware column, finds a row bit, and looks the key
    /// up in its table — and the column it indexes the table by is not the one
    /// it drove. Anything that gets that wrong still types: every letter lands
    /// on some other key, so the keyboard looks like it works right up until
    /// somebody presses Enter and gets an apostrophe.
    #[test]
    fn every_key_reads_back_as_itself() {
        let mut seen_vks: Vec<u8> = Vec::new();
        for col in VOICENOTE_LAYOUT.iter() {
            for &vk in col.iter() {
                if vk == 0 || seen_vks.contains(&vk) {
                    continue;
                }
                seen_vks.push(vk);
                let mut kb = Keyboard::default();
                assert!(kb.set_key(vk, true), "{vk:#04x} is not on the matrix");
                // Where it actually went; F2 appears twice and `set_key` takes
                // the first.
                let (c, r) = kb.position_of(vk).unwrap();

                // Scan as the driver does, and see which column reports it.
                let mut seen = None;
                for hw in 0..COLUMNS {
                    kb.write_select(hw as u16);
                    let rows = kb.read_rows();
                    if rows != 0 {
                        assert!(seen.is_none(), "{vk:#04x} answered on two columns");
                        assert_eq!(rows, 1 << r, "{vk:#04x} reported the wrong row");
                        seen = Some(hw);
                    }
                }
                let hw = seen.unwrap_or_else(|| panic!("{vk:#04x} answered on no column"));

                // The driver then indexes its table by the paired column.
                let got = VOICENOTE_LAYOUT[hw ^ 1][r];
                assert_eq!(
                    got, vk,
                    "key at table ({c}, {r}) drove column {hw} and read back as {got:#04x}"
                );
            }
        }
    }

    /// The three keys that were unreachable until the ROM said what they
    /// were, plus the ones beside them. A layout that loses one of these
    /// loses a whole class of KeySoft commands, silently, because everything
    /// that types still types.
    #[test]
    fn the_named_keys_are_all_on_the_matrix() {
        let kb = Keyboard::default();
        for (what, vk) in [
            ("READ", named::READ),
            ("FUNCTION", named::FUNCTION),
            ("CONTROL", named::CONTROL),
            ("SHIFT", named::SHIFT),
            ("HELP", named::HELP),
            ("REPEAT", named::REPEAT),
            ("MENU", named::MENU),
            ("DELETE", named::DELETE),
            ("LEFT", named::LEFT),
            ("UP", named::UP),
            ("RIGHT", named::RIGHT),
            ("DOWN", named::DOWN),
        ] {
            assert!(kb.position_of(vk).is_some(), "{what} ({vk:#04x}) is not on the matrix");
        }
    }

    /// READ and FUNCTION are distinct keys in distinct places. Having them
    /// the same, or the same as Alt, would make every chord land on the
    /// wrong command.
    #[test]
    fn read_and_function_are_not_the_same_key() {
        let kb = Keyboard::default();
        assert_ne!(named::READ, named::FUNCTION);
        assert_ne!(kb.position_of(named::READ), kb.position_of(named::FUNCTION));
    }

    /// The driver parks the selector at the end of a sweep, and that is what
    /// says it has finished looking. Counting them is how a chord and a
    /// single key can be waited on with the same rule.
    #[test]
    fn parking_the_selector_ends_a_sweep() {
        let mut kb = Keyboard::default();
        assert_eq!(kb.sweeps, 0);
        for c in 0..COLUMNS {
            kb.write_select(COLUMN_IDLE);
            let _ = kb.read_rows();
            kb.write_select(c as u16);
            let _ = kb.read_rows();
        }
        assert_eq!(kb.sweeps, 0, "not until it parks");
        kb.write_select(COLUMN_PARK);
        assert_eq!(kb.sweeps, 1);
    }

    /// However many keys are down, a sweep is one sweep. This is the property
    /// that makes the rule work for a chord: holding READ with a letter must
    /// not look like twice as much looking as the letter alone.
    #[test]
    fn a_chord_does_not_inflate_the_sweep_count() {
        let mut alone = Keyboard::default();
        alone.set_key(VK_A, true);
        let mut chord = Keyboard::default();
        chord.set_key(VK_A, true);
        chord.set_key(named::READ, true);

        for kb in [&mut alone, &mut chord] {
            for c in 0..COLUMNS {
                kb.write_select(c as u16);
                let _ = kb.read_rows();
            }
            kb.write_select(COLUMN_PARK);
        }
        assert_eq!(alone.sweeps, chord.sweeps);
        assert!(chord.scans_seen > alone.scans_seen, "but more keys did answer");
    }

    /// A modifier held down in another column must not make the key it
    /// modifies look as though the guest has seen it. This is the whole
    /// reason the count is per column.
    #[test]
    fn a_held_modifier_does_not_count_as_seeing_the_key() {
        let mut kb = Keyboard::default();
        kb.set_key(named::READ, true);
        kb.set_key(VK_A, true);
        let (read_col, _) = kb.position_of(named::READ).unwrap();
        let before = kb.scans_of(VK_A).unwrap();

        // Scan only the modifier's column, over and over.
        for _ in 0..10 {
            kb.write_select((read_col ^ 1) as u16);
            assert_ne!(kb.read_rows(), 0, "the modifier is down and should answer");
        }
        assert!(kb.scans_seen >= 10, "the total climbs");
        assert_eq!(kb.scans_of(VK_A).unwrap(), before, "but not for A");

        // Looking at A's column is what counts for A.
        let (a_col, _) = kb.position_of(VK_A).unwrap();
        kb.write_select((a_col ^ 1) as u16);
        assert_ne!(kb.read_rows(), 0);
        assert_eq!(kb.scans_of(VK_A).unwrap(), before + 1);
    }

    #[test]
    fn idle_and_park_selections_read_as_nothing_pressed() {
        let mut kb = Keyboard::default();
        kb.set_key(VK_SPACE, true);
        kb.write_select(COLUMN_IDLE);
        assert_eq!(kb.read_rows(), 0);
        kb.write_select(COLUMN_PARK);
        assert_eq!(kb.read_rows(), 0);
    }

    #[test]
    fn releasing_clears_the_bit() {
        let mut kb = Keyboard::default();
        let (col, _) = kb.position_of(VK_SPACE).unwrap();
        kb.set_key(VK_SPACE, true);
        kb.write_select((col ^ 1) as u16);
        assert_ne!(kb.read_rows(), 0);
        kb.set_key(VK_SPACE, false);
        assert_eq!(kb.read_rows(), 0);
        assert!(!kb.any_down());
    }

    #[test]
    fn an_unmapped_key_is_rejected_rather_than_silently_lost() {
        let mut kb = Keyboard::default();
        assert!(!kb.set_key(0xF5, false), "F5 is not on this keyboard");
        assert!(!kb.any_down());
    }

    /// Walk the matrix exactly as the driver does and reassemble its six
    /// words, checking a key lands in the bit position the driver will read.
    #[test]
    fn a_full_scan_reproduces_the_drivers_packing() {
        let mut kb = Keyboard::default();
        kb.set_key(VK_A, true);
        let (col, row) = kb.position_of(VK_A).unwrap();

        let mut words = [0u16; 6];
        for c in 0..COLUMNS {
            kb.write_select(COLUMN_IDLE);
            let _ = kb.read_rows();
            kb.write_select(c as u16);
            let _ = kb.read_rows();
            let rows = kb.read_rows();
            if c % 2 == 0 {
                words[c / 2] = rows << 8;
            } else {
                words[c / 2] |= rows & 0xFF;
            }
        }

        // A key at an even *table* column answers on an odd *hardware*
        // column, which is the half the driver reads back first. Having this
        // the other way round is precisely the bug that made Enter type an
        // apostrophe.
        let expected_bit = if col % 2 == 0 { row } else { 8 + row };
        assert_eq!(words[col / 2], 1 << expected_bit);
        assert_eq!(words.iter().filter(|w| **w != 0).count(), 1);
    }
}
