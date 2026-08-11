//! PXA27x UARTs: 16550-compatible, but with registers spaced four bytes apart.
//!
//! FFUART at 0x40100000 is EBOOT's console, which makes it the first thing
//! that has to work — its output is how we find out whether anything else does.

use crate::intc::Intc;
use std::collections::VecDeque;

pub const FFUART: u32 = 0x4010_0000;
pub const BTUART: u32 = 0x4020_0000;
pub const STUART: u32 = 0x4070_0000;

// LSR
const LSR_DR: u32 = 1 << 0;
const LSR_THRE: u32 = 1 << 5;
const LSR_TEMT: u32 = 1 << 6;
// IER
const IER_RAVIE: u32 = 1 << 0;
const IER_TIE: u32 = 1 << 1;
const IER_UUE: u32 = 1 << 6;

pub struct Uart {
    pub irq: u32,
    ier: u32,
    fcr: u32,
    lcr: u32,
    mcr: u32,
    spr: u32,
    isr: u32,
    divisor: u32,
    /// Bytes the guest has transmitted, waiting for the host to drain.
    pub tx: Vec<u8>,
    /// Bytes the host has queued for the guest to receive.
    pub rx: VecDeque<u8>,
    /// Register accesses, so it is visible which ports a guest actually uses
    /// even when it never sends a byte.
    pub reads: u64,
    pub writes: u64,
    pub bytes_sent: u64,
    /// Program counter of the first byte transmitted, so the sending driver
    /// can be identified rather than assumed.
    pub first_tx_pc: u32,
}

impl Uart {
    pub fn new(irq: u32) -> Self {
        Uart {
            irq,
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            spr: 0,
            isr: 0,
            divisor: 1,
            tx: Vec::new(),
            rx: VecDeque::new(),
            reads: 0,
            writes: 0,
            bytes_sent: 0,
            first_tx_pc: 0,
        }
    }

    #[inline]
    fn dlab(&self) -> bool {
        self.lcr & 0x80 != 0
    }

    fn lsr(&self) -> u32 {
        // Transmission is instantaneous here, so the holding register is
        // always empty.
        let mut v = LSR_THRE | LSR_TEMT;
        if !self.rx.is_empty() {
            v |= LSR_DR;
        }
        v
    }

    /// The 16550 interrupt identification register, highest priority first.
    fn iir(&self) -> u32 {
        if self.ier & IER_RAVIE != 0 && !self.rx.is_empty() {
            0x04 // received data available
        } else if self.ier & IER_TIE != 0 {
            0x02 // transmitter holding register empty
        } else {
            0x01 // no interrupt pending
        }
    }

    pub fn update_irq(&self, intc: &mut Intc) {
        let active = self.ier & IER_UUE != 0 && self.iir() & 1 == 0;
        intc.set(self.irq, active);
    }

    pub fn read(&mut self, offset: u32, intc: &mut Intc) -> u32 {
        self.reads += 1;
        let v = match offset {
            0x00 if self.dlab() => self.divisor & 0xFF,
            0x00 => self.rx.pop_front().unwrap_or(0) as u32,
            0x04 if self.dlab() => (self.divisor >> 8) & 0xFF,
            0x04 => self.ier,
            0x08 => self.iir() | (if self.fcr & 1 != 0 { 0xC0 } else { 0 }),
            0x0C => self.lcr,
            0x10 => self.mcr,
            0x14 => self.lsr(),
            // Modem status: assert CTS, DSR and DCD so flow control never
            // blocks. Nothing on this board wires them up.
            0x18 => 0xB0,
            0x1C => self.spr,
            0x20 => self.isr,
            0x24 => self.rx.len() as u32, // FOR, receive FIFO occupancy
            _ => 0,
        };
        self.update_irq(intc);
        v
    }

    pub fn write_from(&mut self, offset: u32, val: u32, intc: &mut Intc, pc: u32) {
        if offset == 0 && !self.dlab() && self.bytes_sent == 0 {
            self.first_tx_pc = pc;
        }
        self.write(offset, val, intc);
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        self.writes += 1;
        match offset {
            0x00 if self.dlab() => self.divisor = (self.divisor & 0xFF00) | (val & 0xFF),
            0x00 => {
                self.tx.push(val as u8);
                self.bytes_sent += 1;
            }
            0x04 if self.dlab() => self.divisor = (self.divisor & 0xFF) | ((val & 0xFF) << 8),
            0x04 => self.ier = val,
            0x08 => {
                self.fcr = val;
                // Bits 1 and 2 reset the receive and transmit FIFOs.
                if val & 0x02 != 0 {
                    self.rx.clear();
                }
            }
            0x0C => self.lcr = val,
            0x10 => self.mcr = val,
            0x1C => self.spr = val,
            0x20 => self.isr = val,
            _ => {}
        }
        self.update_irq(intc);
    }

    /// Queue a byte from the host for the guest to read.
    pub fn feed(&mut self, byte: u8, intc: &mut Intc) {
        self.rx.push_back(byte);
        self.update_irq(intc);
    }

    /// Take everything the guest has printed.
    pub fn drain_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmitted_bytes_are_captured() {
        let mut u = Uart::new(22);
        let mut intc = Intc::default();
        for b in b"OK\r\n" {
            u.write(0x00, *b as u32, &mut intc);
        }
        assert_eq!(u.drain_tx(), b"OK\r\n");
    }

    #[test]
    fn divisor_latch_switches_the_first_two_registers() {
        let mut u = Uart::new(22);
        let mut intc = Intc::default();
        u.write(0x0C, 0x83, &mut intc); // LCR: DLAB + 8N1
        u.write(0x00, 0x18, &mut intc);
        u.write(0x04, 0x00, &mut intc);
        assert_eq!(u.divisor, 0x18); // 96 -> 9600 baud at 14.7456 MHz
        u.write(0x0C, 0x03, &mut intc); // clear DLAB
        u.write(0x00, b'A' as u32, &mut intc);
        assert_eq!(u.drain_tx(), b"A");
    }

    #[test]
    fn receive_sets_data_ready_and_interrupts_when_enabled() {
        let mut u = Uart::new(22);
        let mut intc = Intc::default();
        intc.mask[0] = 1 << 22;
        u.write(0x04, IER_UUE | IER_RAVIE, &mut intc);
        assert!(!intc.irq_line());
        u.feed(b'x', &mut intc);
        assert!(u.read(0x14, &mut intc) & LSR_DR != 0);
        assert!(intc.irq_line());
        assert_eq!(u.read(0x00, &mut intc), b'x' as u32);
        assert!(!intc.irq_line());
    }
}
