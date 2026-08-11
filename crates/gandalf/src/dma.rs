//! Driving the PXA27x DMA engine.
//!
//! The register file lives in `pxa270::dma`; the transfers happen here,
//! because a descriptor moves data between SDRAM and a peripheral and only
//! the board can reach both.
//!
//! A descriptor is four words in memory: next address, source, target,
//! command. Bit 0 of the next address stops the chain.
//!
//! Real hardware paces a transfer against whichever end has flow control set.
//! For audio that is the AC97 FIFO draining at 48 kHz, and it matters: an
//! unthrottled chain into the codec produces hours of audio per second of
//! guest time, because the driver keeps a circular chain running over a
//! silence buffer whenever the device is idle. Transfers into the codec are
//! therefore metered against its sample-rate credit, and a descriptor that
//! runs out of credit resumes on a later call rather than completing.

use crate::Gandalf;
use arm::Bus;
use pxa270::dma::{DCMD_INCSRCADDR, DCMD_INCTRGADDR, DDADR_STOP, DCSR_NODESC};

/// Cap on bytes moved per service call, so a runaway descriptor cannot stall
/// the emulator.
const MAX_BYTES: u32 = 1 << 16;

/// Run at most one descriptor on one channel. Called from the board's tick.
pub fn service(board: &mut Gandalf) -> bool {
    let Some(ch) = board.soc.dma.next_runnable() else {
        return false;
    };

    let no_desc = board.soc.dma.channels[ch].dcsr & DCSR_NODESC != 0;
    // Only fetch a new descriptor once the current one is exhausted, so a
    // transfer paced by its destination can resume where it left off.
    let needs_fetch = !no_desc && board.soc.dma.channels[ch].length() == 0;
    if needs_fetch {
        // Fetch the descriptor the channel points at.
        let addr = board.soc.dma.channels[ch].ddadr & !0xF;
        let next = board.read32(addr);
        let src = board.read32(addr + 4);
        let dst = board.read32(addr + 8);
        let cmd = board.read32(addr + 12);
        let c = &mut board.soc.dma.channels[ch];
        c.ddadr = next;
        c.dsadr = src;
        c.dtadr = dst;
        c.dcmd = cmd;
    }

    let c = board.soc.dma.channels[ch];
    let width = c.width();
    let mut len = c.length().min(MAX_BYTES);
    let inc_src = c.dcmd & DCMD_INCSRCADDR != 0;
    let inc_dst = c.dcmd & DCMD_INCTRGADDR != 0;

    // Feeding the codec is paced by the codec. Without this the chain runs
    // at emulator speed and produces hours of audio per second of guest time.
    if !inc_dst && c.dtadr == pxa270::ac97::BASE + 0x40 && width > 0 {
        let frames = board.soc.ac97.take_credit(len / width);
        if frames == 0 {
            return false;
        }
        len = len.min(frames * width);
    }

    let mut src = c.dsadr;
    let mut dst = c.dtadr;
    let mut moved = 0;
    while moved + width <= len {
        match width {
            1 => {
                let v = board.read8(src);
                board.write8(dst, v);
            }
            2 => {
                let v = board.read16(src);
                board.write16(dst, v);
            }
            _ => {
                let v = board.read32(src);
                board.write32(dst, v);
            }
        }
        if inc_src {
            src += width;
        }
        if inc_dst {
            dst += width;
        }
        moved += width;
    }

    board.soc.dma.bytes_moved += moved as u64;
    let c = &mut board.soc.dma.channels[ch];
    c.dsadr = src;
    c.dtadr = dst;
    let remaining = c.length().saturating_sub(moved);
    c.dcmd = (c.dcmd & !pxa270::dma::DCMD_LENGTH) | remaining;

    // A descriptor that is only partly done raises nothing yet.
    if remaining > 0 {
        return true;
    }

    // Without descriptor fetch there is no chain, so one transfer is the
    // whole job. With it, bit 0 of the next address ends the chain.
    let stop = no_desc || board.soc.dma.channels[ch].ddadr & DDADR_STOP != 0;
    let mut intc = std::mem::take(&mut board.soc.intc);
    board.soc.dma.complete(ch, stop, &mut intc);
    board.soc.intc = intc;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use pxa270::dma::{DCMD_ENDIRQEN, DCSR_ENDINTR, DCSR_RUN};

    const SDRAM: u32 = crate::SDRAM_BASE;

    /// Build a descriptor in SDRAM and point channel `ch` at it.
    fn arm_channel(board: &mut Gandalf, ch: usize, desc: u32, next: u32, src: u32, dst: u32, cmd: u32) {
        board.write32(desc, next);
        board.write32(desc + 4, src);
        board.write32(desc + 8, dst);
        board.write32(desc + 12, cmd);
        let mut intc = std::mem::take(&mut board.soc.intc);
        board.soc.dma.write(0x200 + (ch as u32) * 16, desc, &mut intc);
        board.soc.dma.write((ch * 4) as u32, DCSR_RUN, &mut intc);
        board.soc.intc = intc;
    }

    #[test]
    fn feeding_the_codec_is_paced_by_its_sample_rate() {
        let mut board = Gandalf::new();
        let (desc, src) = (SDRAM + 0x1000, SDRAM + 0x2000);
        let pcdr = pxa270::ac97::BASE + 0x40;
        // A large transfer with no credit available makes no progress.
        arm_channel(&mut board, 3, desc, DDADR_STOP, src, pcdr,
            DCMD_INCSRCADDR | (3 << 14) | 0x1000);
        assert!(!service(&mut board), "no credit, no transfer");

        // Two milliseconds of guest time buys about a hundred frames, far
        // short of the 1024 this descriptor wants.
        board.soc.ac97.tick((crate::CPU_HZ_EFFECTIVE / 500) as u32, crate::CPU_HZ_EFFECTIVE);
        assert!(service(&mut board));
        let after = board.soc.dma.channels[3].length();
        assert!(after > 0, "a paced descriptor resumes rather than completing");
        assert!(board.soc.dma.bytes_moved > 0, "but it does make progress");
    }

    #[test]
    fn a_descriptor_moves_memory_to_memory() {
        let mut board = Gandalf::new();
        let (desc, src, dst) = (SDRAM + 0x1000, SDRAM + 0x2000, SDRAM + 0x3000);
        for i in 0..8u32 {
            board.write32(src + i * 4, 0x1000 + i);
        }
        // Increment both ends, 4-byte width, 32 bytes, stop after one.
        arm_channel(&mut board, 1, desc, DDADR_STOP, src, dst,
            DCMD_INCSRCADDR | DCMD_INCTRGADDR | (3 << 14) | DCMD_ENDIRQEN | 32);

        assert!(service(&mut board));
        for i in 0..8u32 {
            assert_eq!(board.read32(dst + i * 4), 0x1000 + i, "word {i}");
        }
        assert_eq!(board.soc.dma.bytes_moved, 32);
    }

    #[test]
    fn a_completed_chain_stops_and_interrupts() {
        let mut board = Gandalf::new();
        board.soc.intc.mask[0] = 1 << pxa270::intc::IRQ_DMA;
        let (desc, src, dst) = (SDRAM + 0x1000, SDRAM + 0x2000, SDRAM + 0x3000);
        arm_channel(&mut board, 2, desc, DDADR_STOP, src, dst,
            DCMD_INCSRCADDR | DCMD_INCTRGADDR | (3 << 14) | DCMD_ENDIRQEN | 16);

        assert!(service(&mut board));
        assert!(board.soc.intc.irq_line(), "completion raises the DMA interrupt");
        assert_ne!(board.soc.dma.read(2 * 4) & DCSR_ENDINTR, 0);
        assert!(!service(&mut board), "a stopped channel has no more work");
    }

    #[test]
    fn a_chain_of_two_descriptors_runs_both() {
        let mut board = Gandalf::new();
        let (d0, d1) = (SDRAM + 0x1000, SDRAM + 0x1010);
        let (src, dst) = (SDRAM + 0x2000, SDRAM + 0x3000);
        board.write32(src, 0xAAAA_AAAA);
        board.write32(src + 4, 0xBBBB_BBBB);
        // First descriptor points at the second; the second stops.
        board.write32(d1, DDADR_STOP);
        board.write32(d1 + 4, src + 4);
        board.write32(d1 + 8, dst + 4);
        board.write32(d1 + 12, DCMD_INCSRCADDR | DCMD_INCTRGADDR | (3 << 14) | 4);
        arm_channel(&mut board, 0, d0, d1, src, dst,
            DCMD_INCSRCADDR | DCMD_INCTRGADDR | (3 << 14) | 4);

        assert!(service(&mut board));
        assert!(service(&mut board), "the chain continues to the second descriptor");
        assert!(!service(&mut board));
        assert_eq!(board.read32(dst), 0xAAAA_AAAA);
        assert_eq!(board.read32(dst + 4), 0xBBBB_BBBB);
    }

    #[test]
    fn a_fixed_target_feeds_a_peripheral_fifo() {
        // This is the audio case: source increments through a buffer, target
        // stays on the AC97 PCM data register.
        let mut board = Gandalf::new();
        let (desc, src) = (SDRAM + 0x1000, SDRAM + 0x2000);
        let pcdr = pxa270::ac97::BASE + 0x40;
        for i in 0..4u32 {
            board.write32(src + i * 4, 0x0100_0200 + i);
        }
        arm_channel(&mut board, 3, desc, DDADR_STOP, src, pcdr,
            DCMD_INCSRCADDR | (3 << 14) | DCMD_ENDIRQEN | 16);

        // The codec only accepts samples as fast as it plays them, so let
        // enough guest time pass for four frames of credit.
        board.soc.ac97.tick(1_000_000, crate::CPU_HZ_EFFECTIVE);
        assert!(service(&mut board));
        let pcm = board.soc.ac97.drain_pcm();
        assert_eq!(pcm.len(), 4, "four words reached the codec");
        assert_eq!(pcm[0], 0x0100_0200);
        assert_eq!(pcm[3], 0x0100_0203);
    }
}
