//! PXA27x MMC/SD controller, at physical 0x41100000.
//!
//! EBOOT probes for a bootable SD card before falling back to the ROM image,
//! so this has to answer even when no card is fitted — otherwise the probe
//! spins forever waiting for a command to complete. With no card, every
//! command completes with a response timeout, which is what real hardware
//! reports and what makes the bootloader move on.

use crate::intc::{Intc, IRQ_MMC};
use crate::sdcard::SdCard;

pub const BASE: u32 = 0x4111_0000 & 0xFFF0_0000; // 0x41100000

// STRPCL
const STRPCL_STOP_CLOCK: u32 = 1 << 0;
const STRPCL_START_CLOCK: u32 = 1 << 1;

// STAT
const STAT_READ_TIME_OUT: u32 = 1 << 0;
const STAT_TIME_OUT_RESPONSE: u32 = 1 << 1;
const STAT_XMIT_FIFO_EMPTY: u32 = 1 << 6;
const STAT_RECV_FIFO_FULL: u32 = 1 << 7;
const STAT_CLK_EN: u32 = 1 << 8;
const STAT_DATA_TRAN_DONE: u32 = 1 << 11;
const STAT_PRG_DONE: u32 = 1 << 12;
const STAT_END_CMD_RES: u32 = 1 << 13;

// CMDAT
/// Response format the host is expecting: 0 none, 1 short, 2 long, 3 R3.
const CMDAT_RESPONSE_FORMAT: u32 = 0x3;
/// The command moves data as well as a response.
const CMDAT_DATA_EN: u32 = 1 << 2;
/// Data goes to the card rather than coming from it.
const CMDAT_WR_RD: u32 = 1 << 3;

// I_REG / I_MASK share a bit layout.
const INT_DATA_TRAN_DONE: u32 = 1 << 0;
const INT_PRG_DONE: u32 = 1 << 1;
const INT_END_CMD_RES: u32 = 1 << 2;
const INT_RXFIFO_REQ: u32 = 1 << 5;
const INT_TXFIFO_REQ: u32 = 1 << 6;

pub struct Mmc {
    pub strpcl: u32,
    pub stat: u32,
    pub clkrt: u32,
    pub spi: u32,
    pub cmdat: u32,
    pub resto: u32,
    pub rdto: u32,
    pub blklen: u32,
    pub nob: u32,
    pub prtbuf: u32,
    pub i_mask: u32,
    pub i_reg: u32,
    pub cmd: u32,
    pub argh: u32,
    pub argl: u32,
    clock_on: bool,
    /// The card in the slot, if any.
    pub card: Option<SdCard>,
    /// Response bytes still to be shifted out through `MMC_RES`.
    response: std::collections::VecDeque<u8>,
    /// Bytes of the current data transfer still to move. A transfer is only
    /// done when this reaches zero, and saying so before then is what left
    /// the guest waiting after `ACMD13`.
    bytesleft: u32,
}

impl Default for Mmc {
    fn default() -> Self {
        Mmc {
            strpcl: 0,
            stat: STAT_XMIT_FIFO_EMPTY,
            clkrt: 0,
            spi: 0,
            cmdat: 0,
            resto: 0,
            rdto: 0,
            blklen: 0,
            nob: 0,
            prtbuf: 0,
            // Every source masked at reset.
            i_mask: 0xFFFF_FFFF,
            i_reg: 0,
            cmd: 0,
            argh: 0,
            argl: 0,
            clock_on: false,
            card: None,
            response: std::collections::VecDeque::new(),
            bytesleft: 0,
        }
    }
}

impl Mmc {
    fn update_irq(&self, intc: &mut Intc) {
        intc.set(IRQ_MMC, self.i_reg & !self.i_mask != 0);
    }

    /// Run the command sitting in CMD/ARG to completion.
    fn execute_command(&mut self) {
        self.stat &= !(STAT_TIME_OUT_RESPONSE | STAT_READ_TIME_OUT);
        self.response.clear();
        let arg = (self.argh << 16) | (self.argl & 0xFFFF);
        // A response timeout means the host asked for an answer and none
        // came. `CMD0` asks for none, and reporting a timeout for it tells
        // the driver the slot is empty — which is exactly what happened:
        // the guest sent `CMD0` twice and never spoke to the card again.
        let wanted_response = self.cmdat & CMDAT_RESPONSE_FORMAT != 0;
        // Only the bottom six bits of CMD are the command index; the guest
        // carries flags above them. Dispatching on the whole register sends
        // CMD55 in as 119 and ACMD41 as 105, so neither is recognised, the
        // card never reports itself ready, and the host asks forever.
        let index = (self.cmd & 0x3F) as u8;
        match self.card.as_mut() {
            Some(card) => {
                if let Some(r) = card.command(index, arg) {
                    self.response.extend(r);
                } else if wanted_response {
                    self.stat |= STAT_TIME_OUT_RESPONSE;
                }
            }
            // Nothing on the bus: the command completes, but no response
            // arrives before the timeout in RESTO expires.
            None => {
                if wanted_response {
                    self.stat |= STAT_TIME_OUT_RESPONSE | STAT_READ_TIME_OUT;
                }
            }
        }
        if std::env::var("VN_MMC").is_ok() {
            let r: Vec<String> = self.response.iter().take(6).map(|b| format!("{b:02x}")).collect();
            eprintln!(
                "[mmc cmd {:2} arg {arg:#010x} cmdat {:#06x} -> {}]",
                index,
                self.cmdat,
                if r.is_empty() { "TIMEOUT".to_string() } else { r.join(" ") }
            );
        }
        self.stat |= STAT_END_CMD_RES;
        self.i_reg |= INT_END_CMD_RES;

        // A command that moves data is not finished when its response
        // arrives. `NOB * BLKLEN` bytes have to cross the FIFO first, and
        // only then is the transfer done. Announcing DATA_DONE up front left
        // the guest with nothing to wait for and nothing to read.
        if self.cmdat & CMDAT_DATA_EN != 0 {
            self.bytesleft = self.nob.max(1) * self.blklen;
        } else {
            self.bytesleft = 0;
            self.stat |= STAT_PRG_DONE | STAT_DATA_TRAN_DONE;
            self.i_reg |= INT_PRG_DONE | INT_DATA_TRAN_DONE;
        }
        self.fifo_update();
    }

    /// Move the data transfer along, and finish it when nothing is left.
    fn fifo_update(&mut self) {
        if self.bytesleft == 0 {
            return;
        }
        if self.cmdat & CMDAT_WR_RD != 0 {
            self.i_reg |= INT_TXFIFO_REQ;
        } else {
            // The host is told there is something to collect; it collects it
            // a byte at a time through RXFIFO.
            self.i_reg |= INT_RXFIFO_REQ;
        }
    }

    /// `n` bytes have crossed the FIFO.
    fn moved_bytes(&mut self, n: u32) {
        if self.bytesleft == 0 {
            return;
        }
        self.bytesleft = self.bytesleft.saturating_sub(n);
        if self.bytesleft == 0 {
            self.stat |= STAT_DATA_TRAN_DONE;
            self.i_reg |= INT_DATA_TRAN_DONE;
            if self.cmdat & CMDAT_WR_RD != 0 {
                self.stat |= STAT_PRG_DONE;
                self.i_reg |= INT_PRG_DONE;
            }
            // And tell the card, which the controller was not doing. The
            // status the driver polls comes from the card, not from here.
            if let Some(card) = self.card.as_mut() {
                card.transfer_finished();
            }
        } else {
            self.fifo_update();
        }
    }

    /// `MMC_RES` hands the response back sixteen bits at a time, the earlier
    /// byte in the **high** half.
    ///
    /// This is the opposite way round from the data FIFO, which is not a
    /// guess: packing it the same way as the data — which is how QEMU's
    /// model of this controller does it — stops the card being identified at
    /// all, and the guest never writes a byte. The two really do differ.
    fn next_response_word(&mut self) -> u32 {
        let hi = self.response.pop_front().unwrap_or(0) as u32;
        let lo = self.response.pop_front().unwrap_or(0) as u32;
        hi << 8 | lo
    }

    pub fn read(&mut self, offset: u32, intc: &mut Intc) -> u32 {
        let v = match offset {
            0x00 => self.strpcl,
            0x04 => {
                let mut s = self.stat;
                if self.clock_on {
                    s |= STAT_CLK_EN;
                }
                if self.card.as_ref().is_some_and(|c| c.has_data()) {
                    s |= STAT_RECV_FIFO_FULL;
                }
                s | STAT_XMIT_FIFO_EMPTY
            }
            0x08 => self.clkrt,
            0x0C => self.spi,
            0x10 => self.cmdat,
            0x14 => self.resto,
            0x18 => self.rdto,
            0x1C => self.blklen,
            0x20 => self.nob,
            0x24 => self.prtbuf,
            0x28 => self.i_mask,
            0x2C => {
                // I_REG is cleared by reading it.
                let v = self.i_reg;
                self.i_reg = 0;
                v
            }
            0x30 => self.cmd,
            0x34 => self.argh,
            0x38 => self.argl,
            0x3C => self.next_response_word(),
            0x40 => {
                // The FIFO is a byte wide but the bus is not: one 32-bit
                // read takes four bytes, the first of them landing in the
                // *least* significant position. Handing over one byte per
                // access made every transfer a quarter the length it should
                // be; packing them the other way round put a boot sector on
                // the card with every group of four bytes reversed.
                let mut v = 0u32;
                for shift in [0, 8, 16, 24] {
                    let b = self.card.as_mut().map_or(0, |c| c.read_byte()) as u32;
                    v |= b << shift;
                }
                self.i_reg &= !INT_RXFIFO_REQ;
                self.moved_bytes(4);
                v
            }
            _ => 0,
        };
        self.update_irq(intc);
        v
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        if std::env::var("VN_MMC_REGS").is_ok() {
            let n = match offset {
                0x00 => "STRPCL", 0x08 => "CLKRT", 0x0C => "SPI", 0x10 => "CMDAT",
                0x14 => "RESTO", 0x18 => "RDTO", 0x1C => "BLKLEN", 0x20 => "NOB",
                0x24 => "PRTBUF", 0x28 => "I_MASK", 0x30 => "CMD", 0x34 => "ARGH",
                0x38 => "ARGL", 0x44 => "TXFIFO", _ => "?",
            };
            eprintln!("[mmc W {n:7} {offset:#04x} = {val:#010x}]");
        }
        match offset {
            0x00 => {
                self.strpcl = val;
                if val & STRPCL_STOP_CLOCK != 0 {
                    self.clock_on = false;
                }
                if val & STRPCL_START_CLOCK != 0 {
                    self.clock_on = true;
                }
            }
            0x08 => self.clkrt = val,
            0x0C => self.spi = val,
            0x10 => {
                // Writing CMDAT is what issues the command. CMD, ARGH and
                // ARGL are staged first and this is always written last —
                // the guest's own order, and it never touches STRPCL again
                // once the clock is running. Treating the clock as the
                // trigger instead left it staging SEND_OP_COND forever and
                // wondering why no card ever answered.
                self.cmdat = val;
                if self.clock_on {
                    self.execute_command();
                }
            }
            0x14 => self.resto = val,
            0x18 => self.rdto = val,
            0x1C => self.blklen = val,
            0x20 => self.nob = val,
            0x24 => self.prtbuf = val,
            0x28 => self.i_mask = val,
            0x30 => self.cmd = val,
            0x34 => self.argh = val,
            0x38 => self.argl = val,
            0x44 => {
                // Four bytes per access, least significant first, to match
                // the way the receive side hands them back.
                if let Some(card) = self.card.as_mut() {
                    for shift in [0, 8, 16, 24] {
                        card.write_byte((val >> shift) as u8);
                    }
                }
                self.i_reg &= !INT_TXFIFO_REQ;
                self.moved_bytes(4);
            }
            _ => {}
        }
        self.update_irq(intc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_with_no_card_times_out_rather_than_hanging() {
        let mut m = Mmc::default();
        let mut intc = Intc::default();
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        m.write(0x30, 1, &mut intc); // CMD1
        m.write(0x10, 1, &mut intc); // CMDAT last: expecting a short response
        let stat = m.read(0x04, &mut intc);
        assert_ne!(stat & STAT_END_CMD_RES, 0, "command must complete");
        assert_ne!(stat & STAT_TIME_OUT_RESPONSE, 0, "and report no response");
    }

    /// The command is issued by writing CMDAT, not by starting the clock.
    /// The guest stages CMD, ARGH and ARGL and writes CMDAT last, and once
    /// the clock is running it never touches STRPCL again.
    /// CMD carries flags above the command index, so only its bottom six
    /// bits name the command. Dispatching on the whole register turns CMD55
    /// into 119 and the card stops recognising APP_CMD.
    #[test]
    fn only_the_bottom_six_bits_of_cmd_name_the_command() {
        let mut m = Mmc { card: Some(SdCard::new(1 << 20)), ..Default::default() };
        let mut intc = Intc::default();
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        m.write(0x30, 0x177, &mut intc); // CMD55 with flags above it
        m.write(0x10, 1, &mut intc);
        let first = m.read(0x3C, &mut intc);
        assert_eq!(first >> 8, 55, "the card answered APP_CMD");
    }

    #[test]
    fn writing_cmdat_is_what_issues_the_command() {
        let mut m = Mmc { card: Some(SdCard::new(1 << 20)), ..Default::default() };
        let mut intc = Intc::default();
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        m.write(0x30, 1, &mut intc); // CMD1, SEND_OP_COND
        m.write(0x34, 0x0002, &mut intc);
        m.write(0x38, 0x0000, &mut intc);
        assert_eq!(m.read(0x3C, &mut intc), 0, "nothing has been issued yet");
        m.write(0x10, 3, &mut intc); // CMDAT last
        assert_ne!(m.read(0x3C, &mut intc), 0, "and now the card has answered");
    }

    /// GO_IDLE_STATE asks for no response, so getting none is not a
    /// timeout. Reporting one tells the driver the slot is empty.
    #[test]
    fn a_command_that_expects_no_response_does_not_time_out() {
        let mut m = Mmc { card: Some(SdCard::new(1 << 20)), ..Default::default() };
        let mut intc = Intc::default();
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        m.write(0x30, 0, &mut intc); // CMD0
        m.write(0x10, 0, &mut intc); // CMDAT last: no response expected
        assert_eq!(m.read(0x04, &mut intc) & STAT_TIME_OUT_RESPONSE, 0);
    }

    #[test]
    fn interrupt_register_clears_on_read() {
        let mut m = Mmc::default();
        let mut intc = Intc::default();
        m.write(0x28, !INT_END_CMD_RES, &mut intc);
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        m.write(0x10, 0, &mut intc); // CMDAT issues the command
        assert!(intc.irq_line() || m.i_reg & INT_END_CMD_RES != 0);
        assert_ne!(m.read(0x2C, &mut intc) & INT_END_CMD_RES, 0);
        assert_eq!(m.read(0x2C, &mut intc), 0);
    }

    #[test]
    fn clock_enable_is_reported() {
        let mut m = Mmc::default();
        let mut intc = Intc::default();
        assert_eq!(m.read(0x04, &mut intc) & STAT_CLK_EN, 0);
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        assert_ne!(m.read(0x04, &mut intc) & STAT_CLK_EN, 0);
        m.write(0x00, STRPCL_STOP_CLOCK, &mut intc);
        assert_eq!(m.read(0x04, &mut intc) & STAT_CLK_EN, 0);
    }

    /// With a card fitted the host must get a response rather than a
    /// timeout, or it will decide the slot is empty and stop asking.
    #[test]
    fn a_command_to_a_fitted_card_is_answered() {
        let mut m = Mmc { card: Some(SdCard::new(1 << 20)), ..Default::default() };
        let mut intc = Intc::default();
        m.write(0x00, STRPCL_START_CLOCK, &mut intc);
        m.write(0x30, 8, &mut intc); // CMD8, SEND_IF_COND
        m.write(0x34, 0, &mut intc);
        m.write(0x38, 0x1AA, &mut intc);
        m.write(0x10, 1, &mut intc); // CMDAT last
        let stat = m.read(0x04, &mut intc);
        assert_eq!(stat & STAT_TIME_OUT_RESPONSE, 0, "a fitted card answers");
        let first = m.read(0x3C, &mut intc);
        assert_eq!(first & 0xFF00, 0x0800, "the response leads with the command index");
    }

    #[test]
    fn transmit_fifo_always_reports_room() {
        let mut m = Mmc::default();
        let mut intc = Intc::default();
        assert_ne!(m.read(0x04, &mut intc) & STAT_XMIT_FIFO_EMPTY, 0);
        let _ = INT_TXFIFO_REQ;
    }
}
