//! PXA27x DMA controller, at physical 0x40000000.
//!
//! Audio playback on this SoC is DMA-driven: `wavedev.dll` builds a chain of
//! descriptors in memory, points a channel at it, sets RUN, and waits for the
//! completion interrupt. Without a DMA engine the driver waits forever, which
//! is why the machine never gets as far as its startup beep.
//!
//! The register file lives here; the transfers themselves are performed by
//! the board, because only it can reach both SDRAM and the peripheral being
//! fed. See `gandalf::dma`.

use crate::intc::{Intc, IRQ_DMA};

pub const BASE: u32 = 0x4000_0000;
pub const CHANNELS: usize = 32;
/// Request sources that can be mapped to a channel.
pub const REQUESTS: usize = 75;

// DCSR
pub const DCSR_RUN: u32 = 1 << 31;
pub const DCSR_NODESC: u32 = 1 << 30;
pub const DCSR_STOPIRQEN: u32 = 1 << 29;
pub const DCSR_EORIRQEN: u32 = 1 << 28;
pub const DCSR_EORJMPEN: u32 = 1 << 27;
pub const DCSR_EORSTOPEN: u32 = 1 << 26;
pub const DCSR_EORINTR: u32 = 1 << 9;
pub const DCSR_REQPEND: u32 = 1 << 8;
pub const DCSR_STOPSTATE: u32 = 1 << 3;
pub const DCSR_ENDINTR: u32 = 1 << 2;
pub const DCSR_STARTINTR: u32 = 1 << 1;
pub const DCSR_BUSERR: u32 = 1 << 0;

/// Bits the guest clears by writing one.
const DCSR_W1C: u32 = DCSR_EORINTR | DCSR_ENDINTR | DCSR_STARTINTR | DCSR_BUSERR;
/// Bits the guest owns outright.
const DCSR_CONTROL: u32 =
    DCSR_RUN | DCSR_NODESC | DCSR_STOPIRQEN | DCSR_EORIRQEN | DCSR_EORJMPEN | DCSR_EORSTOPEN;

// DCMD
pub const DCMD_INCSRCADDR: u32 = 1 << 31;
pub const DCMD_INCTRGADDR: u32 = 1 << 30;
pub const DCMD_FLOWSRC: u32 = 1 << 29;
pub const DCMD_FLOWTRG: u32 = 1 << 28;
pub const DCMD_STARTIRQEN: u32 = 1 << 22;
pub const DCMD_ENDIRQEN: u32 = 1 << 21;
pub const DCMD_LENGTH: u32 = 0x1FFF;

/// A descriptor's next-address field carries a stop flag in bit 0.
pub const DDADR_STOP: u32 = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    pub dcsr: u32,
    pub ddadr: u32,
    pub dsadr: u32,
    pub dtadr: u32,
    pub dcmd: u32,
}

impl Channel {
    /// Transfer width in bytes, from DCMD bits 15:14.
    pub fn width(&self) -> u32 {
        match (self.dcmd >> 14) & 3 {
            1 => 1,
            2 => 2,
            _ => 4,
        }
    }

    pub fn length(&self) -> u32 {
        self.dcmd & DCMD_LENGTH
    }
}

pub struct Dma {
    pub channels: [Channel; CHANNELS],
    /// Request-source to channel map, DRCMR.
    pub drcmr: [u32; REQUESTS],
    /// Bytes moved, for diagnostics.
    pub bytes_moved: u64,
    pub descriptors_run: u64,
}

impl Default for Dma {
    fn default() -> Self {
        Dma {
            channels: [Channel::default(); CHANNELS],
            drcmr: [0; REQUESTS],
            bytes_moved: 0,
            descriptors_run: 0,
        }
    }
}

impl Dma {
    /// DINT: one bit per channel with an interrupt outstanding.
    pub fn dint(&self) -> u32 {
        let mut v = 0;
        for (i, c) in self.channels.iter().enumerate() {
            if c.dcsr & (DCSR_ENDINTR | DCSR_STARTINTR | DCSR_EORINTR | DCSR_BUSERR) != 0 {
                v |= 1 << i;
            }
        }
        v
    }

    pub fn update_irq(&self, intc: &mut Intc) {
        intc.set(IRQ_DMA, self.dint() != 0);
    }

    /// The lowest-numbered channel that is running and has work to do.
    pub fn next_runnable(&self) -> Option<usize> {
        self.channels
            .iter()
            .position(|c| c.dcsr & DCSR_RUN != 0 && c.dcsr & DCSR_STOPSTATE == 0)
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x00..=0x7F => self.channels[(offset / 4) as usize].dcsr,
            0xF0 => self.dint(),
            0x100..=0x1FF => {
                let i = ((offset - 0x100) / 4) as usize;
                self.drcmr.get(i).copied().unwrap_or(0)
            }
            0x200..=0x3FF => {
                let ch = ((offset - 0x200) / 16) as usize;
                let c = &self.channels[ch.min(CHANNELS - 1)];
                match (offset - 0x200) % 16 {
                    0 => c.ddadr,
                    4 => c.dsadr,
                    8 => c.dtadr,
                    _ => c.dcmd,
                }
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        match offset {
            0x00..=0x7F => {
                let ch = (offset / 4) as usize;
                let c = &mut self.channels[ch];
                // Status bits are write-one-to-clear; control bits are taken
                // from the written value.
                let kept = c.dcsr & !(val & DCSR_W1C);
                c.dcsr = (kept & !DCSR_CONTROL) | (val & DCSR_CONTROL);
                if val & DCSR_RUN != 0 {
                    c.dcsr &= !DCSR_STOPSTATE;
                } else {
                    c.dcsr |= DCSR_STOPSTATE;
                }
            }
            0x100..=0x1FF => {
                let i = ((offset - 0x100) / 4) as usize;
                if let Some(r) = self.drcmr.get_mut(i) {
                    *r = val;
                }
            }
            0x200..=0x3FF => {
                let ch = ((offset - 0x200) / 16) as usize;
                if ch >= CHANNELS {
                    return;
                }
                let c = &mut self.channels[ch];
                match (offset - 0x200) % 16 {
                    0 => c.ddadr = val,
                    4 => c.dsadr = val,
                    8 => c.dtadr = val,
                    _ => c.dcmd = val,
                }
            }
            _ => {}
        }
        self.update_irq(intc);
    }

    /// Record that a descriptor finished, raising whatever it asked for.
    pub fn complete(&mut self, ch: usize, stop: bool, intc: &mut Intc) {
        let c = &mut self.channels[ch];
        if c.dcmd & DCMD_ENDIRQEN != 0 {
            c.dcsr |= DCSR_ENDINTR;
        }
        if c.dcmd & DCMD_STARTIRQEN != 0 {
            c.dcsr |= DCSR_STARTINTR;
        }
        if stop {
            c.dcsr |= DCSR_STOPSTATE;
            c.dcsr &= !DCSR_RUN;
        }
        self.descriptors_run += 1;
        self.update_irq(intc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dcsr_off(ch: usize) -> u32 {
        (ch * 4) as u32
    }
    fn desc_off(ch: usize, field: u32) -> u32 {
        0x200 + (ch as u32) * 16 + field
    }

    #[test]
    fn starting_a_channel_clears_its_stop_state() {
        let mut d = Dma::default();
        let mut intc = Intc::default();
        assert!(d.next_runnable().is_none());
        d.write(dcsr_off(1), DCSR_RUN, &mut intc);
        assert_eq!(d.next_runnable(), Some(1));
        assert_eq!(d.read(dcsr_off(1)) & DCSR_STOPSTATE, 0);
    }

    #[test]
    fn descriptor_registers_round_trip_per_channel() {
        let mut d = Dma::default();
        let mut intc = Intc::default();
        d.write(desc_off(3, 0), 0xA000_1000, &mut intc);
        d.write(desc_off(3, 4), 0xA000_2000, &mut intc);
        d.write(desc_off(3, 8), 0x4050_0040, &mut intc);
        d.write(desc_off(3, 12), DCMD_ENDIRQEN | 0x40, &mut intc);
        assert_eq!(d.read(desc_off(3, 0)), 0xA000_1000);
        assert_eq!(d.read(desc_off(3, 4)), 0xA000_2000);
        assert_eq!(d.read(desc_off(3, 8)), 0x4050_0040);
        assert_eq!(d.channels[3].length(), 0x40);
        // A different channel is untouched.
        assert_eq!(d.read(desc_off(4, 0)), 0);
    }

    #[test]
    fn completion_raises_the_dma_interrupt_and_the_guest_clears_it() {
        let mut d = Dma::default();
        let mut intc = Intc::default();
        intc.mask[0] = 1 << IRQ_DMA;
        d.write(desc_off(2, 12), DCMD_ENDIRQEN | 0x20, &mut intc);
        d.write(dcsr_off(2), DCSR_RUN, &mut intc);
        assert!(!intc.irq_line());

        d.complete(2, true, &mut intc);
        assert_ne!(d.dint() & (1 << 2), 0, "DINT names the channel");
        assert!(intc.irq_line());

        d.write(dcsr_off(2), DCSR_ENDINTR, &mut intc);
        assert!(!intc.irq_line(), "write-one-to-clear releases it");
        assert_eq!(d.dint(), 0);
    }

    #[test]
    fn a_stopping_descriptor_takes_the_channel_out_of_run() {
        let mut d = Dma::default();
        let mut intc = Intc::default();
        d.write(dcsr_off(0), DCSR_RUN, &mut intc);
        d.complete(0, true, &mut intc);
        assert_eq!(d.read(dcsr_off(0)) & DCSR_RUN, 0);
        assert_ne!(d.read(dcsr_off(0)) & DCSR_STOPSTATE, 0);
        assert!(d.next_runnable().is_none());
    }

    #[test]
    fn transfer_width_decodes_from_dcmd() {
        let mut c = Channel { dcmd: 1 << 14, ..Default::default() };
        assert_eq!(c.width(), 1);
        c.dcmd = 2 << 14;
        assert_eq!(c.width(), 2);
        c.dcmd = 3 << 14;
        assert_eq!(c.width(), 4);
    }

    #[test]
    fn request_map_registers_are_independent() {
        let mut d = Dma::default();
        let mut intc = Intc::default();
        // DRCMR12 is the AC97 PCM-out request on this SoC.
        d.write(0x100 + 12 * 4, 0x80 | 1, &mut intc);
        assert_eq!(d.read(0x100 + 12 * 4), 0x81);
        assert_eq!(d.read(0x100 + 13 * 4), 0);
    }
}
