//! The braille display, as a shift register on the CPLD.
//!
//! Not braille output — that is out of scope. This exists because the
//! machine will not finish starting without a display that answers, and
//! answering is a handful of lines.
//!
//! The OAL bit-bangs the display through CPLD register index 0, byte offset
//! `0x400`, one bit at a time (`FUN_8007c044` in `nk.exe`):
//!
//! ```text
//! for bit in 0..8 {
//!     if byte & mask[bit] { set(DATA) } else { clear(DATA) }
//!     set(CLOCK); clear(CLOCK);
//! }
//! ```
//!
//! and detects the display by **measuring how long the chain is**
//! (`FUN_8007c128`):
//!
//! ```text
//! clear DATA, CLOCK and STROBE
//! shift out 32 zero bytes            ; flush the chain
//! count = 0
//! while count < 33 {
//!     if gpio(103) { break }         ; a one has reached the far end
//!     shift out 0xff
//!     count += 1
//!     Sleep(1)
//! }
//! cells = 0
//! if count == 24 { cells = 18 }      ; an 18-cell display
//! if count == 32 { cells = 32 }      ; a 32-cell display
//! ```
//!
//! The count is then handed to KeySoft through
//! `KernelIoControl(0x01013FA0, ...)`, which fills a twelve-byte block whose
//! first halfword is the cell count. KeySoft accepts 6 to 40 cells; anything
//! else and it announces *"The Braille Display is not operating"*.
//!
//! So a display is a chain of shift-register stages with its far end wired
//! to GPIO 103, and its length is what identifies the model. Model exactly
//! that and the count falls out on its own.

/// Bits of CPLD register 0 the OAL drives, from the primitives at
/// `0x8007be88` (clear) and `0x8007bea8` (set) and their neighbours, which
/// each carry one of these masks.
pub const DATA: u16 = 0x10;
pub const CLOCK: u16 = 0x20;
pub const STROBE: u16 = 0x40;
pub const ENABLE: u16 = 0x80;

/// GPIO the far end of the chain is wired to, read by `FUN_8007bfe8` as
/// `gpio(0x67)`.
pub const END_OF_CHAIN_GPIO: u32 = 103;

/// Chain length, in bytes, of the two displays this ROM knows.
///
/// Cell count is not the model. BT and QT name the **keyboard** — Braille
/// Terminal against QWERTY — and each comes in an 18-cell and a 32-cell
/// version, so this ROM serves six machines:
///
/// | Model | Display | Keyboard |
/// |---|---|---|
/// | BrailleNote BT18 | 18 cells | braille |
/// | BrailleNote BT32 | 32 cells | braille |
/// | BrailleNote QT18 | 18 cells | QWERTY |
/// | BrailleNote QT32 | 32 cells | QWERTY |
/// | VoiceNote BT | none | braille |
/// | VoiceNote QT | none | QWERTY |
///
/// The chain length gives the display half only. The keyboard half comes
/// from `pdikeybd.dll`'s scan tables, and a VoiceNote is a machine with no
/// chain at all.
pub const CHAIN_BYTES_32_CELL: usize = 32;
pub const CHAIN_BYTES_18_CELL: usize = 24;

pub struct BrailleDisplay {
    /// Whether a display is attached at all. With none, the chain never
    /// returns anything and KeySoft reports it is not operating — which is
    /// what a bare VoiceNote does.
    pub present: bool,
    /// One entry per stage, oldest first. `stages[0]` is the stage nearest
    /// the far end, so it is what GPIO 103 reads.
    stages: Vec<bool>,
    /// Previous value written, to find the clock edges.
    last: u16,
    /// Bits clocked in since reset, for diagnostics.
    pub bits_shifted: u64,
}

impl Default for BrailleDisplay {
    fn default() -> Self {
        BrailleDisplay::with_chain(CHAIN_BYTES_32_CELL)
    }
}

impl BrailleDisplay {
    pub fn with_chain(bytes: usize) -> Self {
        BrailleDisplay {
            present: true,
            stages: vec![false; bytes * 8],
            last: 0,
            bits_shifted: 0,
        }
    }

    /// A write to CPLD register 0. Shifts on the rising edge of the clock.
    pub fn write(&mut self, value: u16) {
        let rising = value & CLOCK != 0 && self.last & CLOCK == 0;
        self.last = value;
        if !rising || !self.present || self.stages.is_empty() {
            return;
        }
        self.stages.remove(0);
        self.stages.push(value & DATA != 0);
        self.bits_shifted += 1;
    }

    /// What GPIO 103 reads: the stage at the far end of the chain.
    ///
    /// Combinational, not latched. That is what makes the OAL's count come
    /// out right: it checks the line *before* shifting each byte, so a
    /// latched output would leave the chain looking one byte longer than it
    /// is.
    pub fn end_of_chain(&self) -> bool {
        self.present && *self.stages.first().unwrap_or(&false)
    }

    /// Cell count this chain length reports, by the OAL's own table.
    pub fn cells(&self) -> u16 {
        match self.stages.len() / 8 {
            CHAIN_BYTES_18_CELL => 18,
            CHAIN_BYTES_32_CELL => 32,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clock one byte through, the way `FUN_8007c044` does.
    fn shift_byte(d: &mut BrailleDisplay, byte: u8) {
        for bit in 0..8 {
            let data = if byte >> bit & 1 != 0 { DATA } else { 0 };
            d.write(data);
            d.write(data | CLOCK);
            d.write(data);
        }
    }

    /// The OAL's detection loop, returning the count it would arrive at.
    fn probe(d: &mut BrailleDisplay) -> u16 {
        for _ in 0..32 {
            shift_byte(d, 0x00);
        }
        let mut count = 0u16;
        while count < 0x21 {
            if d.end_of_chain() {
                break;
            }
            shift_byte(d, 0xFF);
            count += 1;
        }
        count
    }

    #[test]
    fn a_thirty_two_cell_display_is_counted_correctly() {
        let mut d = BrailleDisplay::with_chain(CHAIN_BYTES_32_CELL);
        assert_eq!(probe(&mut d), 0x20, "the OAL wants exactly 32 here");
        assert_eq!(d.cells(), 32);
    }

    #[test]
    fn an_eighteen_cell_display_is_counted_correctly() {
        let mut d = BrailleDisplay::with_chain(CHAIN_BYTES_18_CELL);
        assert_eq!(probe(&mut d), 0x18, "the OAL wants exactly 24 here");
        assert_eq!(d.cells(), 18);
    }

    #[test]
    fn both_counts_land_in_the_range_keysoft_accepts() {
        // FUN_00023284 in KeySoft: 5 < cells < 0x29.
        for chain in [CHAIN_BYTES_32_CELL, CHAIN_BYTES_18_CELL] {
            let cells = BrailleDisplay::with_chain(chain).cells();
            assert!((6..=40).contains(&cells), "{cells} cells would be rejected");
        }
    }

    #[test]
    fn no_display_never_answers_and_the_count_runs_out() {
        let mut d = BrailleDisplay { present: false, ..Default::default() };
        assert_eq!(probe(&mut d), 0x21, "the loop should give up, not break");
        assert!(!d.end_of_chain());
    }

    #[test]
    fn only_the_rising_clock_edge_shifts() {
        let mut d = BrailleDisplay::with_chain(1);
        for _ in 0..8 {
            d.write(DATA);
            d.write(DATA | CLOCK);
            d.write(DATA | CLOCK); // held high, must not shift again
            d.write(DATA);
        }
        assert_eq!(d.bits_shifted, 8);
    }
}
