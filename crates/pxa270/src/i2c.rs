//! PXA27x I2C controller, at physical 0x40300000.
//!
//! The OAL drives this hard and, unmodelled, gets nowhere: 270 transactions
//! and 5400 status polls in a single boot, every one timing out. Its code is
//! textbook PXA I2C:
//!
//! ```text
//! str r2, [base+0x20]    ; ISAR = slave address
//! orr r0, r1, #0x40      ; ICR |= IUE, enable the unit
//! lsl r2, r0, #1         ; seven-bit address shifted left for the R/W bit
//! str r2, [base+0x08]    ; IDBR = address byte
//! orr r1, r0, #9         ; ICR |= START | TB
//! ldr r3, [base+0x18]    ; poll ISR
//! and r0, r3, #0x40      ; bit 6, ITE: the byte has gone
//! ```
//!
//! Modelling it costs little and buys a lot: every transaction records the
//! slave address it was aimed at, which is the only way to find out what is
//! actually on this board's I2C bus.
//!
//! What is on the bus turned out to be a **Smart Battery** at 7-bit address
//! 0x0B, the address the SBS specification reserves for one, read with
//! standard Smart Battery Data commands: 0x21 DeviceName, 0x08 Temperature,
//! 0x09 Voltage, 0x0A Current. `battdrvr.dll` agrees, carrying the strings
//! "Failed to get device name", "Invalid device name length: %d" and
//! "Scaling value: %d (v2)".
//!
//! Anything else is answered with a NAK, which is what an empty bus does and
//! lets a probe fail quickly rather than spinning out a timeout.

use crate::intc::{Intc, IRQ_I2C};
use std::collections::BTreeMap;

pub const BASE: u32 = 0x4030_0000;

// ICR
const ICR_START: u32 = 1 << 0;
const ICR_STOP: u32 = 1 << 1;
const ICR_ACKNAK: u32 = 1 << 2;
const ICR_TB: u32 = 1 << 3;
const ICR_MA: u32 = 1 << 4;
const ICR_IUE: u32 = 1 << 6;
const ICR_ITEIE: u32 = 1 << 8;
const ICR_IRFIE: u32 = 1 << 9;
const ICR_UR: u32 = 1 << 14;

// ISR
const ISR_RWM: u32 = 1 << 0;
/// Set when the slave did **not** acknowledge.
const ISR_ACKNAK: u32 = 1 << 1;
const ISR_UB: u32 = 1 << 2;
const ISR_IBB: u32 = 1 << 3;
const ISR_ITE: u32 = 1 << 6;
const ISR_IRF: u32 = 1 << 7;

/// Status bits the guest clears by writing one.
const ISR_W1C: u32 = ISR_ITE | ISR_IRF | ISR_ACKNAK | (1 << 4) | (1 << 5) | (1 << 10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    pub address: u8,
    pub read: bool,
    pub data: u8,
    /// True when this byte carried the address rather than payload.
    pub is_address: bool,
}

/// Smart Battery Data command codes used by this board.
pub mod sbs {
    pub const TEMPERATURE: u8 = 0x08;
    pub const VOLTAGE: u8 = 0x09;
    pub const CURRENT: u8 = 0x0A;
    pub const RELATIVE_CHARGE: u8 = 0x0D;
    pub const ABSOLUTE_CHARGE: u8 = 0x0E;
    pub const REMAINING_CAPACITY: u8 = 0x0F;
    pub const FULL_CHARGE_CAPACITY: u8 = 0x10;
    pub const BATTERY_STATUS: u8 = 0x16;
    pub const CYCLE_COUNT: u8 = 0x17;
    pub const DESIGN_CAPACITY: u8 = 0x18;
    pub const DESIGN_VOLTAGE: u8 = 0x19;
    pub const MANUFACTURER_NAME: u8 = 0x20;
    pub const DEVICE_NAME: u8 = 0x21;
    pub const DEVICE_CHEMISTRY: u8 = 0x22;
}

/// The address the Smart Battery System specification reserves for a battery.
pub const SMART_BATTERY_ADDRESS: u8 = 0x0B;

/// An SBS-compliant Smart Battery, reporting a healthy pack.
#[derive(Debug, Clone)]
pub struct SmartBattery {
    pub present: bool,
    /// Reported by DeviceName.
    ///
    /// battdrvr requires a length of 1 to 7 and takes a scaling factor from
    /// the last four characters when they are all digits, falling back to
    /// 2500 otherwise. The name a real pack reports is unknown, so this one
    /// is chosen to land on the driver's own default rather than invent a
    /// different figure.
    pub device_name: &'static str,
    pub manufacturer: &'static str,
    /// Tenths of a kelvin, as the specification defines temperature.
    pub temperature_dk: u16,
    pub voltage_mv: u16,
    /// Milliamps, positive charging and negative discharging.
    pub current_ma: i16,
    pub charge_percent: u16,
    pub capacity_mah: u16,
}

impl Default for SmartBattery {
    fn default() -> Self {
        SmartBattery {
            present: true,
            device_name: "PD2500",
            manufacturer: "PulseData",
            temperature_dk: 2982,
            voltage_mv: 8200,
            current_ma: 0,
            charge_percent: 100,
            capacity_mah: 2500,
        }
    }
}

impl SmartBattery {
    /// The bytes a command reads back. Words go low byte first; the block
    /// commands are length-prefixed, which is how SMBus block reads work and
    /// what battdrvr parses.
    pub fn response(&self, command: u8) -> Vec<u8> {
        let word = |v: u16| vec![v as u8, (v >> 8) as u8];
        let block = |t: &str| {
            let mut v = vec![t.len() as u8];
            v.extend_from_slice(t.as_bytes());
            v
        };
        match command {
            sbs::TEMPERATURE => word(self.temperature_dk),
            sbs::VOLTAGE => word(self.voltage_mv),
            sbs::CURRENT => word(self.current_ma as u16),
            sbs::RELATIVE_CHARGE | sbs::ABSOLUTE_CHARGE => word(self.charge_percent),
            sbs::REMAINING_CAPACITY => {
                word((self.capacity_mah as u32 * self.charge_percent as u32 / 100) as u16)
            }
            sbs::FULL_CHARGE_CAPACITY | sbs::DESIGN_CAPACITY => word(self.capacity_mah),
            sbs::DESIGN_VOLTAGE => word(8400),
            sbs::BATTERY_STATUS => word(0x0020),
            sbs::CYCLE_COUNT => word(12),
            sbs::MANUFACTURER_NAME => block(self.manufacturer),
            sbs::DEVICE_NAME => block(self.device_name),
            sbs::DEVICE_CHEMISTRY => block("LION"),
            _ => word(0),
        }
    }
}

pub struct I2c {
    pub battery: SmartBattery,
    /// Command byte of the transaction in progress.
    command: Option<u8>,
    /// Bytes still to be handed back on reads, in reverse order.
    read_buf: Vec<u8>,
    pub ibmr: u32,
    pub idbr: u32,
    pub icr: u32,
    pub isr: u32,
    pub isar: u32,
    /// Every slave address the guest has addressed, and how often.
    pub addresses: BTreeMap<u8, u32>,
    /// Bounded record of the opening traffic.
    pub log: Vec<Transfer>,
    /// Address of the transaction in progress.
    current: Option<u8>,
    reading: bool,
}

impl Default for I2c {
    fn default() -> Self {
        I2c {
            battery: SmartBattery::default(),
            command: None,
            read_buf: Vec::new(),
            // Both bus lines idle high.
            ibmr: 0x3,
            idbr: 0,
            icr: 0,
            isr: 0,
            isar: 0,
            addresses: BTreeMap::new(),
            log: Vec::new(),
            current: None,
            reading: false,
        }
    }
}

impl I2c {
    fn update_irq(&self, intc: &mut Intc) {
        let mut active = false;
        if self.icr & ICR_ITEIE != 0 && self.isr & ISR_ITE != 0 {
            active = true;
        }
        if self.icr & ICR_IRFIE != 0 && self.isr & ISR_IRF != 0 {
            active = true;
        }
        intc.set(IRQ_I2C, active && self.icr & ICR_IUE != 0);
    }

    /// Move one byte, which is what setting TB asks for.
    fn transfer_byte(&mut self) {
        let byte = self.idbr as u8;

        let mut acked = false;

        if self.icr & ICR_START != 0 {
            // The first byte of a transaction carries the address in bits 7:1
            // and the direction in bit 0.
            let address = byte >> 1;
            self.reading = byte & 1 != 0;
            self.current = Some(address);
            *self.addresses.entry(address).or_insert(0) += 1;
            if self.log.len() < 256 {
                self.log.push(Transfer {
                    address,
                    read: self.reading,
                    data: byte,
                    is_address: true,
                });
            }
            if address == SMART_BATTERY_ADDRESS && self.battery.present {
                acked = true;
                if self.reading {
                    // A repeated start into a read: stage the answer to the
                    // command the write phase left behind.
                    let mut r = match self.command {
                        Some(c) => self.battery.response(c),
                        None => Vec::new(),
                    };
                    r.reverse();
                    self.read_buf = r;
                } else {
                    self.command = None;
                }
            }
            self.isr |= ISR_ITE;
        } else if self.reading {
            match self.read_buf.pop() {
                Some(b) => {
                    self.idbr = b as u32;
                    acked = true;
                }
                None => self.idbr = 0xFF,
            }
            if let Some(address) = self.current {
                if self.log.len() < 256 {
                    let d = self.idbr as u8;
                    self.log.push(Transfer { address, read: true, data: d, is_address: false });
                }
            }
            self.isr |= ISR_IRF;
        } else {
            if let Some(address) = self.current {
                if self.log.len() < 256 {
                    self.log.push(Transfer { address, read: false, data: byte, is_address: false });
                }
                if address == SMART_BATTERY_ADDRESS && self.battery.present {
                    acked = true;
                    if self.command.is_none() {
                        self.command = Some(byte);
                    }
                }
            }
            self.isr |= ISR_ITE;
        }

        if acked {
            self.isr &= !ISR_ACKNAK;
        } else {
            self.isr |= ISR_ACKNAK;
        }
        if self.reading {
            self.isr |= ISR_RWM;
        } else {
            self.isr &= !ISR_RWM;
        }

        if self.icr & ICR_STOP != 0 {
            self.current = None;
            self.isr &= !(ISR_UB | ISR_IBB);
        } else {
            self.isr |= ISR_UB;
        }

        // The controller clears TB and the START/STOP requests once the byte
        // has moved; firmware that does not clear them itself relies on it.
        self.icr &= !(ICR_TB | ICR_START | ICR_STOP);
    }

    pub fn read(&mut self, offset: u32) -> u32 {
        match offset {
            0x00 => self.ibmr,
            0x08 => self.idbr,
            0x10 => self.icr,
            0x18 => self.isr,
            0x20 => self.isar,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        match offset {
            0x08 => self.idbr = val,
            0x10 => {
                if val & ICR_UR != 0 {
                    let addresses = std::mem::take(&mut self.addresses);
                    let log = std::mem::take(&mut self.log);
                    *self = I2c::default();
                    self.addresses = addresses;
                    self.log = log;
                    return;
                }
                self.icr = val;
                if val & ICR_MA != 0 {
                    self.current = None;
                    self.isr &= !(ISR_UB | ISR_IBB);
                    self.icr &= !ICR_MA;
                } else if val & ICR_TB != 0 && val & ICR_IUE != 0 {
                    self.transfer_byte();
                }
                let _ = ICR_ACKNAK;
            }
            0x18 => self.isr &= !(val & ISR_W1C),
            0x20 => self.isar = val,
            _ => {}
        }
        self.update_irq(intc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addressed(i2c: &mut I2c, intc: &mut Intc, address: u8, read: bool) {
        i2c.write(0x20, 0x32, intc); // our own slave address, as the OAL sets
        i2c.write(0x08, ((address << 1) | read as u8) as u32, intc);
        i2c.write(0x10, ICR_IUE | ICR_START | ICR_TB, intc);
    }

    #[test]
    fn a_transfer_reports_transmit_empty_so_the_poll_ends() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        assert_eq!(i2c.read(0x18) & ISR_ITE, 0);
        addressed(&mut i2c, &mut intc, 0x50, false);
        assert_ne!(i2c.read(0x18) & ISR_ITE, 0, "the OAL polls this bit");
    }

    #[test]
    fn an_empty_bus_answers_with_a_nak() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        addressed(&mut i2c, &mut intc, 0x50, false);
        assert_ne!(i2c.read(0x18) & ISR_ACKNAK, 0, "nothing at 0x50 acknowledged");
        addressed(&mut i2c, &mut intc, SMART_BATTERY_ADDRESS, false);
        assert_eq!(i2c.read(0x18) & ISR_ACKNAK, 0, "the battery does");
    }

    #[test]
    fn the_addressed_slave_is_recorded() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        addressed(&mut i2c, &mut intc, 0x50, false);
        addressed(&mut i2c, &mut intc, 0x50, true);
        addressed(&mut i2c, &mut intc, 0x1A, false);
        assert_eq!(i2c.addresses.get(&0x50), Some(&2));
        assert_eq!(i2c.addresses.get(&0x1A), Some(&1));
        assert_eq!(i2c.log[0], Transfer { address: 0x50, read: false, data: 0xA0, is_address: true });
        assert!(i2c.log[1].read, "bit 0 of the address byte selects a read");
    }

    #[test]
    fn transfer_bit_clears_itself() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        addressed(&mut i2c, &mut intc, 0x50, false);
        assert_eq!(i2c.read(0x10) & ICR_TB, 0, "the controller clears TB");
    }

    #[test]
    fn status_bits_are_write_one_to_clear() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        addressed(&mut i2c, &mut intc, 0x50, false);
        i2c.write(0x18, ISR_ITE | ISR_ACKNAK, &mut intc);
        assert_eq!(i2c.read(0x18) & (ISR_ITE | ISR_ACKNAK), 0);
    }

    /// Read a Smart Battery command the way the OAL does: address for write,
    /// send the command, repeated start into a read, then clock bytes out.
    fn sbs_read(i2c: &mut I2c, intc: &mut Intc, command: u8, count: usize) -> Vec<u8> {
        addressed(i2c, intc, SMART_BATTERY_ADDRESS, false);
        i2c.write(0x08, command as u32, intc);
        i2c.write(0x10, ICR_IUE | ICR_TB, intc);
        addressed(i2c, intc, SMART_BATTERY_ADDRESS, true);
        let mut out = Vec::new();
        for _ in 0..count {
            i2c.write(0x10, ICR_IUE | ICR_TB, intc);
            out.push(i2c.read(0x08) as u8);
        }
        out
    }

    #[test]
    fn the_battery_reports_its_voltage_low_byte_first() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        let got = sbs_read(&mut i2c, &mut intc, sbs::VOLTAGE, 2);
        let v = u16::from_le_bytes([got[0], got[1]]);
        assert_eq!(v, i2c.battery.voltage_mv);
    }

    #[test]
    fn device_name_is_length_prefixed_and_battdrvr_will_accept_it() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        let got = sbs_read(&mut i2c, &mut intc, sbs::DEVICE_NAME, 8);
        let len = got[0] as usize;
        // battdrvr rejects a length outside one to seven.
        assert!((1..=7).contains(&len), "length {len} is outside what battdrvr accepts");
        let name = &got[1..1 + len];
        // It then takes a scaling factor from four trailing digits.
        let digits = name.iter().rev().take_while(|c| c.is_ascii_digit()).count();
        assert_eq!(digits, 4, "battdrvr wants exactly four trailing digits");
    }

    #[test]
    fn a_removed_battery_does_not_acknowledge() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        i2c.battery.present = false;
        addressed(&mut i2c, &mut intc, SMART_BATTERY_ADDRESS, false);
        assert_ne!(i2c.read(0x18) & ISR_ACKNAK, 0);
    }

    #[test]
    fn a_read_after_addressing_returns_an_idle_bus() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        addressed(&mut i2c, &mut intc, 0x50, true);
        i2c.write(0x18, ISR_W1C, &mut intc);
        i2c.write(0x10, ICR_IUE | ICR_TB, &mut intc); // read the data byte
        assert_eq!(i2c.read(0x08), 0xFF, "an undriven bus reads as ones");
        assert_ne!(i2c.read(0x18) & ISR_IRF, 0);
    }

    #[test]
    fn a_unit_reset_clears_state_but_keeps_the_record() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        addressed(&mut i2c, &mut intc, 0x50, false);
        i2c.write(0x10, ICR_UR, &mut intc);
        assert_eq!(i2c.read(0x18), 0);
        assert_eq!(i2c.addresses.get(&0x50), Some(&1), "diagnostics survive");
    }

    #[test]
    fn interrupts_follow_the_enable_bits() {
        let mut i2c = I2c::default();
        let mut intc = Intc::default();
        intc.mask[0] = 1 << IRQ_I2C;
        i2c.write(0x08, 0xA0, &mut intc);
        i2c.write(0x10, ICR_IUE | ICR_START | ICR_TB, &mut intc);
        assert!(!intc.irq_line(), "not enabled yet");
        i2c.write(0x10, ICR_IUE | ICR_ITEIE, &mut intc);
        assert!(intc.irq_line(), "transmit-empty interrupt enabled");
    }
}
