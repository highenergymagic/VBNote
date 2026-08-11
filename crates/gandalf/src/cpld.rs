//! The board CPLD at physical 0x10000000 (nCS4).
//!
//! Undocumented custom glue. `cpld2.dll` exposes it as 16-bit registers
//! addressed by a byte index, and its callers are the keyboard scanner
//! (`pdikeybd.dll`), the serial/braille path (`bvdmain_serial.dll`), the
//! battery driver and the front-panel buttons.
//!
//! Nothing here is known yet, so every register is a plain read/write cell
//! and every access is recorded. The plan is to let the guest drive the log
//! and infer the semantics from what it does, rather than reverse the driver
//! cold.

use crate::braille::BrailleDisplay;
use crate::keyboard::Keyboard;
use std::collections::BTreeMap;

#[derive(Default, Clone, Copy)]
pub struct RegisterLog {
    pub reads: u32,
    pub writes: u32,
    pub first_pc: u32,
    pub last_written: u16,
}

/// Byte offset of the keyboard scan register: CPLD register index 1, which
/// the driver's dispatch reaches as `base + 2`.
pub const KEYBOARD_REG: u32 = 0x402;

/// Byte offset of the braille shift register: CPLD register index 0.
///
/// Earlier notes here guessed this was on the audio path because EBOOT
/// touches it around codec setup. It is not: the OAL bit-bangs the braille
/// display through it, one bit per clock edge. See `braille.rs`.
pub const BRAILLE_REG: u32 = 0x400;

/// Candidate board identification register.
///
/// One ROM serves four machines — BrailleNote and VoiceNote, each in BT and
/// QT — and something has to tell the software which one it is running on.
/// `pdikeybd` keeps a device-type value it compares against 5, 6, 7 and 9,
/// and carries separate "VN" keyboard tables, so the distinction is made at
/// run time rather than at build time.
///
/// This register is the best candidate seen so far: the guest reads it once
/// and never writes it, which is what a hardware strap looks like. Modelled
/// as returning zero unless told otherwise.
pub const BOARD_ID_REG: u32 = 0x00E;

pub struct Cpld {
    /// Backing store, so a write followed by a read returns what was written.
    regs: [u16; 256],
    pub log: BTreeMap<u32, RegisterLog>,
    /// When set, every access is also printed as it happens.
    pub trace: bool,
    pub keyboard: Keyboard,
    pub braille: BrailleDisplay,
    /// The internal modem, a 16C550 sharing this chip select. See
    /// `Drivers\\BuiltIn\\UART1` in the ROM registry: IoBase
    /// 0x10000000, stride 2, which is the bottom of this window.
    pub modem: crate::modem::Modem,
    /// Value reported by [`BOARD_ID_REG`].
    pub board_id: u16,
}

impl Default for Cpld {
    fn default() -> Self {
        Cpld {
            regs: [0; 256],
            log: BTreeMap::new(),
            trace: false,
            keyboard: Keyboard::default(),
            braille: BrailleDisplay::default(),
            modem: crate::modem::Modem::default(),
            board_id: 0,
        }
    }
}

impl Cpld {
    fn entry(&mut self, offset: u32, pc: u32) -> &mut RegisterLog {
        self.log.entry(offset).or_insert(RegisterLog { first_pc: pc, ..Default::default() })
    }

    pub fn read(&mut self, offset: u32, pc: u32) -> u16 {
        let val = if crate::modem::Modem::owns(offset) {
            self.modem.read(offset)
        } else if offset == KEYBOARD_REG {
            self.keyboard.read_rows()
        } else if offset == BOARD_ID_REG {
            self.board_id
        } else {
            self.regs[(offset as usize >> 1) & 0xFF]
        };
        self.entry(offset, pc).reads += 1;
        if self.trace {
            eprintln!("cpld: read  {offset:#06x} -> {val:#06x}   pc={pc:#010x}");
        }
        val
    }

    pub fn write(&mut self, offset: u32, val: u16, pc: u32) {
        self.regs[(offset as usize >> 1) & 0xFF] = val;
        if crate::modem::Modem::owns(offset) {
            self.modem.write(offset, val);
        }
        if offset == KEYBOARD_REG {
            self.keyboard.write_select(val);
        }
        if offset == BRAILLE_REG {
            self.braille.write(val);
        }
        let e = self.entry(offset, pc);
        e.writes += 1;
        e.last_written = val;
        if self.trace {
            eprintln!("cpld: write {offset:#06x} <- {val:#06x}   pc={pc:#010x}");
        }
    }

    /// Did the guest ever sample the board identification register?
    pub fn board_id_was_read(&self) -> bool {
        self.log.get(&BOARD_ID_REG).is_some_and(|l| l.reads > 0)
    }

    /// Summary for the bring-up report.
    pub fn report(&self) -> Vec<(u32, RegisterLog)> {
        self.log.iter().map(|(k, v)| (*k, *v)).collect()
    }
}
