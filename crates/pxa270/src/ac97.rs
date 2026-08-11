//! PXA27x AC97 controller, at physical 0x40500000.
//!
//! This is the audio path, and on this machine audio is the entire user
//! interface: KeySoft's speech is software-synthesised PCM pushed through
//! here. EBOOT also uses it long before that, to make its startup beep, and
//! it will not proceed past codec initialisation until the link reports the
//! primary codec ready.
//!
//! Register layout and bit names follow the PXA27x developer's manual, the
//! same definitions Linux uses in `regs-ac97.h`.

use crate::intc::{Intc, IRQ_AC97};

pub const BASE: u32 = 0x4050_0000;

// Global control register.
const GCR_GIE: u32 = 1 << 0;
const GCR_COLD_RST: u32 = 1 << 1;
const GCR_WARM_RST: u32 = 1 << 2;
const GCR_ACLINK_OFF: u32 = 1 << 3;
const GCR_SDONE_IE: u32 = 1 << 18;
const GCR_CDONE_IE: u32 = 1 << 19;

// Global status register.
const GSR_ACOFFD: u32 = 1 << 3;
const GSR_PCR: u32 = 1 << 8; // primary codec ready
const GSR_SDONE: u32 = 1 << 18;
const GSR_CDONE: u32 = 1 << 19;

// Codec register file within the controller's window.
//
// The PXA spaces the codec's registers two bytes apart in its own address
// map, so AC'97 register N appears at `0x200 + N * 2`. Register numbers are
// themselves even byte offsets (0x00, 0x02, ... 0x7E), which makes the whole
// 64-register file exactly the 256-byte window.
//
// Getting this wrong is silent and total: every access lands on a different
// register, readbacks make no sense, and the driver concludes the codec is
// dead and shuts the AC-link off. The giveaway in a trace is a write of
// 0xAC44 — 44100 — which can only be the PCM DAC rate at register 0x2C, and
// which appears at controller offset 0x58.
const PRIMARY_CODEC: u32 = 0x0200;
const CODEC_WINDOW: u32 = 0x0100;

/// Controller offset to AC'97 register number.
#[inline]
fn codec_reg(offset: u32) -> u32 {
    (offset - PRIMARY_CODEC) / 2
}

/// AC'97 register number to an index in the 64-entry file.
#[inline]
fn codec_index(reg: u32) -> usize {
    (reg / 2) as usize
}

/// Standard AC'97 codec register numbers we give meaningful answers for.
mod codec {
    pub const RESET: u32 = 0x00;
    pub const PC_BEEP: u32 = 0x0A;
    pub const POWERDOWN: u32 = 0x26;
    pub const EXT_AUDIO_ID: u32 = 0x28;
    /// Extended audio control; bit 0 enables variable-rate audio.
    pub const EXT_AUDIO_CTRL: u32 = 0x2A;
    /// PCM front DAC rate, in Hz.
    pub const PCM_DAC_RATE: u32 = 0x2C;
    pub const VENDOR_ID1: u32 = 0x7C;
    pub const VENDOR_ID2: u32 = 0x7E;
}

/// Variable-rate audio enable, in the extended audio control register.
const EXT_CTRL_VRA: u16 = 1 << 0;

pub struct Ac97 {
    pub gcr: u32,
    pub gsr: u32,
    pub car: u32,
    pub pocr: u32,
    pub picr: u32,
    pub mccr: u32,
    pub posr: u32,
    pub pisr: u32,
    pub mcsr: u32,
    /// Primary codec register file, 64 sixteen-bit registers.
    codec_regs: [u16; 64],
    /// PCM samples the guest has written, waiting to be played.
    pub pcm_out: Vec<u32>,
    /// Set once the guest has taken the link out of cold reset.
    pub link_up: bool,
    /// Stereo frames the codec can still accept. Refilled at the sample
    /// rate, so DMA into the FIFO is paced the way flow control paces it on
    /// hardware instead of running as fast as the emulator loops.
    credit: u64,
    frac: u64,
    /// Bounded log of control-register writes, for bring-up.
    pub gcr_log: Vec<(u32, u32)>,
    /// Codec register writes, as (register, value).
    pub codec_log: Vec<(u32, u16)>,
}

impl Default for Ac97 {
    fn default() -> Self {
        let mut codec_regs = [0u16; 64];
        // Reset register: reports a codec with no special capabilities.
        codec_regs[codec_index(codec::RESET)] = 0x0400;
        // Powerdown status: ADC, DAC, analogue and reference all ready. The
        // low four bits are read-only status, and drivers spin until all are
        // set.
        codec_regs[codec_index(codec::POWERDOWN)] = 0x000F;
        // Extended audio: variable-rate PCM supported.
        codec_regs[codec_index(codec::EXT_AUDIO_ID)] = 0x0001;
        // Beep is muted at reset, like real silicon.
        codec_regs[codec_index(codec::PC_BEEP)] = 0x8000;

        Ac97 {
            gcr: 0,
            gsr: 0,
            car: 0,
            pocr: 0,
            picr: 0,
            mccr: 0,
            posr: 0,
            pisr: 0,
            mcsr: 0,
            codec_regs,
            pcm_out: Vec::new(),
            link_up: false,
            credit: 0,
            frac: 0,
            gcr_log: Vec::new(),
            codec_log: Vec::new(),
        }
    }
}

/// The AC-link itself always runs at 48 kHz.
pub const LINK_RATE: u32 = 48_000;
/// Frames of slack allowed, so a burst of DMA is not chopped into single
/// samples but playback still cannot run away.
const MAX_CREDIT: u64 = 4096;

impl Ac97 {
    /// The rate PCM is actually delivered at.
    ///
    /// With variable-rate audio enabled the codec resamples internally and
    /// the data rate is whatever the driver put in the DAC rate register —
    /// 44100 on this machine. Treating it as the 48 kHz link rate plays
    /// everything about nine percent fast.
    pub fn pcm_rate(&self) -> u32 {
        let ctrl = self.codec_regs[codec_index(codec::EXT_AUDIO_CTRL)];
        let rate = self.codec_regs[codec_index(codec::PCM_DAC_RATE)] as u32;
        if ctrl & EXT_CTRL_VRA != 0 && (4000..=48_000).contains(&rate) {
            rate
        } else {
            LINK_RATE
        }
    }

    /// Advance the codec's appetite for samples.
    pub fn tick(&mut self, cycles: u32, cpu_hz: u64) {
        self.frac += cycles as u64 * self.pcm_rate() as u64;
        let frames = self.frac / cpu_hz;
        if frames > 0 {
            self.frac -= frames * cpu_hz;
            self.credit = (self.credit + frames).min(MAX_CREDIT);
        }
    }

    /// Claim up to `want` frames of the codec's capacity.
    pub fn take_credit(&mut self, want: u32) -> u32 {
        let got = (want as u64).min(self.credit);
        self.credit -= got;
        got as u32
    }

    fn update_irq(&self, intc: &mut Intc) {
        let mut active = false;
        if self.gcr & GCR_CDONE_IE != 0 && self.gsr & GSR_CDONE != 0 {
            active = true;
        }
        if self.gcr & GCR_SDONE_IE != 0 && self.gsr & GSR_SDONE != 0 {
            active = true;
        }
        intc.set(IRQ_AC97, active && self.gcr & GCR_GIE != 0);
    }

    /// Bring the AC-link up or take it down in response to a GCR write.
    fn apply_gcr(&mut self) {
        let reset_requested = self.gcr & (GCR_COLD_RST | GCR_WARM_RST) != 0;
        let link_off = self.gcr & GCR_ACLINK_OFF != 0;

        if link_off {
            self.link_up = false;
            self.gsr &= !GSR_PCR;
            self.gsr |= GSR_ACOFFD;
        } else if reset_requested {
            // A real codec takes a few frames to come up. Nothing here can
            // observe the delay, and firmware only ever polls for the ready
            // bit, so report it immediately.
            self.link_up = true;
            self.gsr |= GSR_PCR;
            self.gsr &= !GSR_ACOFFD;
        }
    }

    pub fn read(&mut self, offset: u32, intc: &mut Intc) -> u32 {
        let v = match offset {
            0x00 => self.pocr,
            0x04 => self.picr,
            0x08 => self.mccr,
            0x0C => self.gcr,
            0x10 => self.posr,
            0x14 => self.pisr,
            0x18 => self.mcsr,
            0x1C => self.gsr,
            // Codec access in progress: never, we answer instantly.
            0x20 => self.car & !1,
            // PCM data in. Nothing is recording.
            0x40 => 0,
            off if (PRIMARY_CODEC..PRIMARY_CODEC + CODEC_WINDOW).contains(&off) => {
                let val = self.read_codec(codec_reg(off));
                self.gsr |= GSR_SDONE;
                val
            }
            _ => 0,
        };
        self.update_irq(intc);
        v
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        match offset {
            0x00 => self.pocr = val,
            0x04 => self.picr = val,
            0x08 => self.mccr = val,
            0x0C => {
                if self.gcr_log.len() < 64 {
                    self.gcr_log.push((val, self.gsr));
                }
                self.gcr = val;
                self.apply_gcr();
            }
            0x10 => self.posr = val,
            0x14 => self.pisr = val,
            0x18 => self.mcsr = val,
            // GSR status bits are write-one-to-clear. PCR is a level, not an
            // event, so it survives.
            0x1C => self.gsr &= !(val & !GSR_PCR),
            0x20 => self.car = val,
            // PCM out data: one word carries a left and a right sample.
            0x40 => self.pcm_out.push(val),
            off if (PRIMARY_CODEC..PRIMARY_CODEC + CODEC_WINDOW).contains(&off) => {
                let reg = codec_reg(off);
                if self.codec_log.len() < 96 {
                    self.codec_log.push((reg, val as u16));
                }
                self.write_codec(reg, val as u16);
                self.gsr |= GSR_CDONE;
            }
            _ => {}
        }
        self.update_irq(intc);
    }

    fn read_codec(&self, reg: u32) -> u32 {
        match reg {
            // Vendor identification. No driver in this image keys off a
            // specific codec, so report a generic one.
            codec::VENDOR_ID1 => 0x4144, // "AD"
            codec::VENDOR_ID2 => 0x5300, // "S"
            _ => self.codec_regs.get(codec_index(reg)).copied().unwrap_or(0) as u32,
        }
    }

    fn write_codec(&mut self, reg: u32, val: u16) {
        // The low four bits of the powerdown register are read-only status.
        if reg == codec::POWERDOWN {
            if let Some(r) = self.codec_regs.get_mut(codec_index(reg)) {
                *r = (val & 0xFFF0) | 0x000F;
            }
            return;
        }
        if let Some(r) = self.codec_regs.get_mut(codec_index(reg)) {
            *r = val;
        }
    }

    /// Current state of the codec's PC Beep register, which is how EBOOT and
    /// KeySoft make the startup and alert tones.
    pub fn beep_register(&self) -> u16 {
        self.codec_regs[codec_index(codec::PC_BEEP)]
    }

    /// Take the queued PCM samples for playback.
    pub fn drain_pcm(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pcm_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_reports_ready_after_cold_reset() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        assert_eq!(a.read(0x1C, &mut intc) & GSR_PCR, 0);
        a.write(0x0C, GCR_COLD_RST, &mut intc);
        assert_eq!(a.read(0x1C, &mut intc) & GSR_PCR, GSR_PCR, "firmware polls for this");
    }

    #[test]
    fn primary_codec_ready_survives_status_clears() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        a.write(0x0C, GCR_COLD_RST, &mut intc);
        // Firmware clears CDONE by writing it back; PCR must not be lost.
        a.write(0x1C, 0xFFFF_FFFF, &mut intc);
        assert_eq!(a.read(0x1C, &mut intc) & GSR_PCR, GSR_PCR);
    }

    #[test]
    fn codec_register_write_signals_command_done() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        a.write(PRIMARY_CODEC + codec::PC_BEEP * 2, 0x0800, &mut intc);
        assert_eq!(a.read(0x1C, &mut intc) & GSR_CDONE, GSR_CDONE);
        assert_eq!(a.beep_register(), 0x0800);
    }

    #[test]
    fn powerdown_status_bits_stay_ready() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        a.write(PRIMARY_CODEC + codec::POWERDOWN * 2, 0x0000, &mut intc);
        assert_eq!(a.read(PRIMARY_CODEC + codec::POWERDOWN * 2, &mut intc) & 0xF, 0xF);
    }

    #[test]
    fn codec_registers_are_two_bytes_apart_in_the_controller_map() {
        // AC'97 register 0x2C is the PCM front DAC rate, and the driver
        // writes 44100 to it. It must appear at controller offset 0x58.
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        a.write(PRIMARY_CODEC + 0x58, 0xAC44, &mut intc);
        assert_eq!(a.read(PRIMARY_CODEC + 0x58, &mut intc), 0xAC44);
        assert_eq!(codec_reg(PRIMARY_CODEC + 0x58), 0x2C);
        // And the whole 64-register file fits the 256-byte window.
        assert_eq!(codec_reg(PRIMARY_CODEC + CODEC_WINDOW - 4), 0x7E);
        assert_eq!(codec_index(0x7E), 63);
    }

    #[test]
    fn the_dac_rate_register_sets_the_pcm_rate() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        // Without variable-rate audio the link rate stands.
        assert_eq!(a.pcm_rate(), LINK_RATE);
        // The driver enables VRA and asks for 44100.
        a.write(PRIMARY_CODEC + codec::EXT_AUDIO_CTRL * 2, 0x0001, &mut intc);
        a.write(PRIMARY_CODEC + codec::PCM_DAC_RATE * 2, 44_100, &mut intc);
        assert_eq!(a.pcm_rate(), 44_100);
        // A nonsense rate falls back rather than dividing by something silly.
        a.write(PRIMARY_CODEC + codec::PCM_DAC_RATE * 2, 0, &mut intc);
        assert_eq!(a.pcm_rate(), LINK_RATE);
    }

    #[test]
    fn credit_accrues_at_the_codec_rate() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        a.write(PRIMARY_CODEC + codec::EXT_AUDIO_CTRL * 2, 0x0001, &mut intc);
        a.write(PRIMARY_CODEC + codec::PCM_DAC_RATE * 2, 44_100, &mut intc);
        let cpu_hz = 1_200_000_000u64;
        // Ten milliseconds should buy about 441 frames, not 480.
        a.tick((cpu_hz / 100) as u32, cpu_hz);
        let got = a.take_credit(100_000);
        assert!((430..=450).contains(&got), "got {got} frames, expected about 441");
    }

    #[test]
    fn pcm_writes_are_captured_for_playback() {
        let mut a = Ac97::default();
        let mut intc = Intc::default();
        a.write(0x40, 0x1234_5678, &mut intc);
        assert_eq!(a.drain_pcm(), vec![0x1234_5678]);
    }
}
