//! The internal modem: a 16C550 UART on the CPLD's chip select.
//!
//! The mPower has a V.90 modem, and it is not on any of the PXA's own UARTs —
//! which is why nothing ever appeared on them. The ROM registry says where it
//! is, under `Drivers\BuiltIn\UART1`:
//!
//! ```text
//! FriendlyName  UART1-TL16C
//! IoBase        0x10000000     the CPLD chip select
//! IoStride      0x00000002     registers a halfword apart
//! IRQ           0x0000000a
//! GPIO          0x00000010
//! BaudClock     0x00c65d40     12.96 MHz
//! Tsp           Unimodem.dll
//! ```
//!
//! So a discrete TL16C-series part sharing the CPLD's window, with the stock
//! `serial.dll` driving it and `unimodem.dll` on top. A 16C550's registers are
//! eight bytes; at a stride of two they occupy CPLD offsets `0x00` to `0x0e`,
//! clear of the keyboard at `0x402` and the braille display at `0x400`.
//!
//! # What this models, and what it does not
//!
//! Enough for the machine to find a modem, talk to it, and be answered. It
//! keeps the register file honest — the divisor latch, the line and modem
//! status bits a driver waits on — and answers AT commands with `OK`, which is
//! what a modem that is present but not dialling anything says.
//!
//! It does not dial, and there is no carrier. Giving it a real line means
//! handing the bytes to something on the host, and the shape for that is a
//! channel in and out of [`Modem::feed`] and [`Modem::take_tx`]; nothing here
//! needs to change for it.
//!
//! # The interrupt is not optional
//!
//! The driver does not poll. Measured, on the run that hung: it set the line
//! to 8N1, turned the FIFOs on, raised `DTR`, `RTS` and `OUT2`, enabled every
//! interrupt source in `IER`, wrote one byte — the `A` of `AT` — and stopped.
//! It was waiting to be told the transmitter had drained, and a model that
//! answers `IIR` with "nothing pending" for ever never tells it.
//!
//! The line it waits on is the registry's `IRQ 0x0a` and `GPIO 0x10`: on this
//! SoC IRQ 10 is the shared `GPIO_2_x` source, so the part signals by driving
//! GPIO 16, and the OAL arms that pin for a rising edge. See
//! [`INTERRUPT_GPIO`].

/// Register offsets, in units of the stride the board wires up.
mod reg {
    pub const DATA: u32 = 0; // RBR reading, THR writing, DLL when DLAB is set
    pub const IER: u32 = 1; // DLM when DLAB is set
    pub const IIR: u32 = 2; // FCR writing
    pub const LCR: u32 = 3;
    pub const MCR: u32 = 4;
    pub const LSR: u32 = 5;
    pub const MSR: u32 = 6;
    pub const SCRATCH: u32 = 7;
}

/// Line status bits. A driver will not send until it is told the transmitter
/// is free, and will not read until it is told there is something there.
mod lsr {
    pub const DATA_READY: u16 = 1 << 0;
    pub const THR_EMPTY: u16 = 1 << 5;
    pub const TRANSMITTER_IDLE: u16 = 1 << 6;
}

/// Interrupt-enable bits, as the driver writes them.
mod ier {
    pub const RECEIVED_DATA: u16 = 1 << 0;
    pub const TRANSMITTER_EMPTY: u16 = 1 << 1;
}

/// What `IIR` reports, lowest number being highest priority. Bit 0 clear means
/// there is something to report, which is the wrong way round and always has
/// been.
mod iir {
    pub const NONE: u16 = 0x01;
    pub const TRANSMITTER_EMPTY: u16 = 0x02;
    pub const RECEIVED_DATA: u16 = 0x04;
    /// Set in the top two bits whenever the FIFOs are on, which is how a
    /// driver tells a 16550 from a 16450 that has none.
    pub const FIFOS_ENABLED: u16 = 0xC0;
}

/// The pin the part pulls up to interrupt, from `Drivers\BuiltIn\UART1`.
pub const INTERRUPT_GPIO: u32 = 16;

/// Modem status bits. All asserted: a modem that is plugged in and ready.
mod msr {
    pub const CTS: u16 = 1 << 4;
    pub const DSR: u16 = 1 << 5;
    pub const CARRIER_DETECT: u16 = 1 << 7;
}

/// `LCR` bit 7 swaps the first two registers for the baud divisor.
const LCR_DLAB: u16 = 1 << 7;

/// How many registers the part has, at the board's stride.
pub const REGISTERS: u32 = 8;

#[derive(Default)]
pub struct Modem {
    ier: u16,
    lcr: u16,
    mcr: u16,
    scratch: u16,
    /// Baud divisor, kept so a driver that writes and reads it back agrees
    /// with itself. Nothing here runs at a baud rate.
    divisor: u16,
    /// Whether the FIFOs have been turned on, which only changes what `IIR`
    /// reports about itself.
    fifos: bool,
    /// The transmitter has drained and the driver has not been told yet.
    ///
    /// It is a latch, not a level: it is set when a byte finishes going out —
    /// here, at once — and cleared when the driver reads `IIR` and sees it.
    /// Without the clear the driver would be interrupted for ever; without the
    /// set it would never send a second byte.
    transmitter_drained: bool,
    /// What the guest has sent and this has not answered yet.
    command: Vec<u8>,
    /// What the guest has yet to read, oldest first.
    rx: std::collections::VecDeque<u8>,
    /// Bytes the guest has sent, for anything that wants to see them — a host
    /// bridge, or a test.
    pub tx: Vec<u8>,
    /// Commands answered, so a run can say whether the modem was talked to.
    pub commands: u64,
}

impl Modem {
    /// Whether an offset in the CPLD's window belongs to this.
    pub fn owns(offset: u32) -> bool {
        offset < REGISTERS * 2
    }

    pub fn read(&mut self, offset: u32) -> u16 {
        match (offset / 2, self.lcr & LCR_DLAB != 0) {
            (reg::DATA, true) => self.divisor & 0xFF,
            (reg::DATA, false) => self.rx.pop_front().unwrap_or(0) as u16,
            (reg::IER, true) => self.divisor >> 8,
            (reg::IER, false) => self.ier,
            // No interrupt pending. Bit 0 set means "nothing to report",
            // which is the answer while this is polled rather than wired.
            (reg::IIR, _) => {
                let pending = self.pending();
                // Reading which interrupt it was is how the driver
                // acknowledges a drained transmitter.
                if pending == iir::TRANSMITTER_EMPTY {
                    self.transmitter_drained = false;
                }
                pending | if self.fifos { iir::FIFOS_ENABLED } else { 0 }
            }
            (reg::LCR, _) => self.lcr,
            (reg::MCR, _) => self.mcr,
            (reg::LSR, _) => {
                let mut v = lsr::THR_EMPTY | lsr::TRANSMITTER_IDLE;
                if !self.rx.is_empty() {
                    v |= lsr::DATA_READY;
                }
                v
            }
            (reg::MSR, _) => msr::CTS | msr::DSR | msr::CARRIER_DETECT,
            (reg::SCRATCH, _) => self.scratch,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, value: u16) {
        let v = value & 0xFF;
        match (offset / 2, self.lcr & LCR_DLAB != 0) {
            (reg::DATA, true) => self.divisor = (self.divisor & 0xFF00) | v,
            (reg::DATA, false) => self.send(v as u8),
            (reg::IER, true) => self.divisor = (self.divisor & 0x00FF) | (v << 8),
            (reg::IER, false) => {
                // Enabling the transmit interrupt on an empty transmitter
                // interrupts straight away; that is what gets the first byte
                // moving, since the driver is waiting rather than writing.
                let newly = v & !self.ier & ier::TRANSMITTER_EMPTY != 0;
                self.ier = v;
                if newly {
                    self.transmitter_drained = true;
                }
            }
            // FCR. The FIFOs themselves are not modelled — nothing here has a
            // baud rate to be ahead of — but whether they are on has to be
            // remembered, because `IIR` reports it and a driver checks.
            (reg::IIR, _) => self.fifos = v & 1 != 0,
            (reg::LCR, _) => self.lcr = v,
            (reg::MCR, _) => self.mcr = v,
            (reg::SCRATCH, _) => self.scratch = v,
            _ => {}
        }
    }

    /// Which interrupt this would report, honouring what is enabled.
    fn pending(&self) -> u16 {
        if self.ier & ier::RECEIVED_DATA != 0 && !self.rx.is_empty() {
            iir::RECEIVED_DATA
        } else if self.ier & ier::TRANSMITTER_EMPTY != 0 && self.transmitter_drained {
            iir::TRANSMITTER_EMPTY
        } else {
            iir::NONE
        }
    }

    /// Whether the part is asserting its interrupt line.
    pub fn interrupting(&self) -> bool {
        self.pending() != iir::NONE
    }

    /// A byte the guest transmitted.
    fn send(&mut self, byte: u8) {
        self.tx.push(byte);
        // It is gone as soon as it is written, so the transmitter is drained
        // the moment the driver hands a byte over.
        self.transmitter_drained = true;
        // A command ends at a carriage return, which is what `<cr>` in the
        // firmware's init strings means.
        if byte == b'\r' || byte == b'\n' {
            if !self.command.is_empty() {
                let line = std::mem::take(&mut self.command);
                self.answer(&line);
            }
            return;
        }
        self.command.push(byte);
    }

    /// Answer one AT command.
    ///
    /// Everything gets `OK`, which is true of a modem that is attached and
    /// idle: the country setting, the flow control and the reset strings the
    /// firmware sends all simply succeed. `ATI` is answered with something
    /// that identifies the part, because a driver that asks who it is talking
    /// to and gets `OK` may decide the answer was rubbish.
    fn answer(&mut self, line: &[u8]) {
        self.commands += 1;
        let text = String::from_utf8_lossy(line).to_ascii_uppercase();
        let text = text.trim();
        if !text.starts_with("AT") {
            return;
        }
        let reply: &str = if text.contains("I3") || text.contains("I0") {
            "TL16C V.90"
        } else {
            "OK"
        };
        self.feed(format!("\r\n{reply}\r\n").as_bytes());
    }

    /// Give the guest bytes to read, as a real modem or a host bridge would.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes.iter().copied());
    }

    /// Take what the guest has sent, for a host bridge to deal with.
    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set the part up the way the measured driver does, so the tests are
    /// about the same machine the firmware met.
    fn as_the_driver_opens_it(m: &mut Modem) {
        m.write(reg::LCR * 2, 0x03); // 8N1
        m.write(reg::IIR * 2, 0x83); // FIFOs on, trigger at 14
        m.write(reg::MCR * 2, 0x0b); // DTR, RTS, OUT2
        m.write(reg::IER * 2, 0x0f); // every source enabled
    }

    fn send(m: &mut Modem, text: &str) {
        for b in text.bytes() {
            m.write(reg::DATA * 2, b as u16);
        }
    }

    fn read_reply(m: &mut Modem) -> String {
        let mut out = String::new();
        while m.read(reg::LSR * 2) & lsr::DATA_READY != 0 {
            out.push(m.read(reg::DATA * 2) as u8 as char);
        }
        out
    }

    /// A driver will not transmit until the line status says the transmitter
    /// is free. Saying so always is what makes it possible to talk at all.
    #[test]
    fn the_transmitter_is_always_ready() {
        let mut m = Modem::default();
        let s = m.read(reg::LSR * 2);
        assert_ne!(s & lsr::THR_EMPTY, 0);
        assert_ne!(s & lsr::TRANSMITTER_IDLE, 0);
        assert_eq!(s & lsr::DATA_READY, 0, "and nothing to read yet");
    }

    #[test]
    fn an_at_command_is_answered_with_ok() {
        let mut m = Modem::default();
        send(&mut m, "ATZ\r");
        assert_eq!(read_reply(&mut m), "\r\nOK\r\n");
        assert_eq!(m.commands, 1);
    }

    /// The country setting is the command this was built for: it is what
    /// KeySoft asks about during its first-run setup.
    #[test]
    fn the_country_command_is_answered() {
        let mut m = Modem::default();
        send(&mut m, "AT+GCI=B5\r");
        assert_eq!(read_reply(&mut m), "\r\nOK\r\n");
    }

    /// Asking who it is talking to should get a name rather than `OK`.
    #[test]
    fn asking_what_it_is_gets_an_identity() {
        let mut m = Modem::default();
        send(&mut m, "ATI3\r");
        assert!(read_reply(&mut m).contains("TL16C"));
    }

    /// The divisor latch swaps the first two registers, and a driver that
    /// writes a baud rate and reads it back has to get the same number.
    #[test]
    fn the_baud_divisor_reads_back_as_written() {
        let mut m = Modem::default();
        m.write(reg::LCR * 2, LCR_DLAB);
        m.write(reg::DATA * 2, 0x34);
        m.write(reg::IER * 2, 0x12);
        assert_eq!(m.read(reg::DATA * 2), 0x34);
        assert_eq!(m.read(reg::IER * 2), 0x12);
        // With the latch closed the same offsets are data and the interrupt
        // enable again, not the divisor.
        m.write(reg::LCR * 2, 0);
        m.write(reg::IER * 2, 0x0F);
        assert_eq!(m.read(reg::IER * 2), 0x0F);
        assert_eq!(m.read(reg::DATA * 2), 0, "no data waiting");
    }

    /// The scratch register is a driver's usual way of asking whether a
    /// 16550 is there at all: write a byte, read it back.
    #[test]
    fn the_scratch_register_holds_what_it_is_given() {
        let mut m = Modem::default();
        m.write(reg::SCRATCH * 2, 0xA5);
        assert_eq!(m.read(reg::SCRATCH * 2), 0xA5);
    }

    /// The modem reports itself plugged in and ready. A driver that sees no
    /// CTS will not send.
    #[test]
    fn the_line_looks_connected() {
        let mut m = Modem::default();
        let s = m.read(reg::MSR * 2);
        for bit in [msr::CTS, msr::DSR, msr::CARRIER_DETECT] {
            assert_ne!(s & bit, 0);
        }
    }

    /// The hang this was written for: the driver enables the transmit
    /// interrupt and waits to be told the line is free, rather than polling.
    /// A part that never interrupts leaves it waiting after one byte.
    #[test]
    fn enabling_the_transmit_interrupt_interrupts() {
        let mut m = Modem::default();
        assert!(!m.interrupting(), "quiet until it is asked to speak");
        as_the_driver_opens_it(&mut m);
        assert!(m.interrupting(), "the transmitter is empty and that is news");
        assert_eq!(m.read(reg::IIR * 2) & 0x0f, iir::TRANSMITTER_EMPTY);
    }

    /// Reading which interrupt it was is the acknowledgement. Without it the
    /// driver would be interrupted for ever and get nothing else done.
    #[test]
    fn reading_why_it_interrupted_stops_it() {
        let mut m = Modem::default();
        as_the_driver_opens_it(&mut m);
        m.read(reg::IIR * 2);
        assert!(!m.interrupting());
        assert_eq!(m.read(reg::IIR * 2) & 0x0f, iir::NONE);
    }

    /// And every byte handed over drains again, which is what carries a whole
    /// command out one interrupt at a time.
    #[test]
    fn each_byte_sent_asks_for_the_next() {
        let mut m = Modem::default();
        as_the_driver_opens_it(&mut m);
        m.read(reg::IIR * 2);
        for byte in b"AT" {
            m.write(reg::DATA * 2, *byte as u16);
            assert!(m.interrupting(), "wants the next byte");
            assert_eq!(m.read(reg::IIR * 2) & 0x0f, iir::TRANSMITTER_EMPTY);
        }
        assert_eq!(m.tx, b"AT");
    }

    /// An answer waiting to be read interrupts too, and outranks the
    /// transmitter: a driver told only that it may send would never collect
    /// the `OK`.
    #[test]
    fn an_answer_waiting_interrupts_first() {
        let mut m = Modem::default();
        as_the_driver_opens_it(&mut m);
        send(&mut m, "ATZ\r");
        assert_eq!(m.read(reg::IIR * 2) & 0x0f, iir::RECEIVED_DATA);
        assert_eq!(read_reply(&mut m), "\r\nOK\r\n");
        // Drained, so it falls back to the transmitter having room.
        assert_eq!(m.read(reg::IIR * 2) & 0x0f, iir::TRANSMITTER_EMPTY);
    }

    /// A driver that has enabled nothing is not interrupted, whatever the
    /// part has to say.
    #[test]
    fn nothing_enabled_means_nothing_reported() {
        let mut m = Modem::default();
        m.feed(b"RING\r\n");
        assert!(!m.interrupting());
        assert_eq!(m.read(reg::IIR * 2) & 0x0f, iir::NONE);
    }

    /// Turning the FIFOs on shows up in `IIR`'s top bits, which is how a
    /// driver tells this part from a 16450 that has none.
    #[test]
    fn the_fifos_report_themselves() {
        let mut m = Modem::default();
        assert_eq!(m.read(reg::IIR * 2) & iir::FIFOS_ENABLED, 0);
        m.write(reg::IIR * 2, 0x83);
        assert_eq!(m.read(reg::IIR * 2) & iir::FIFOS_ENABLED, iir::FIFOS_ENABLED);
    }

    /// Only the eight registers belong to it; the keyboard and the braille
    /// display live further up the same window and must not be swallowed.
    #[test]
    fn it_claims_only_its_own_registers() {
        assert!(Modem::owns(0));
        assert!(Modem::owns(0x0e));
        assert!(!Modem::owns(0x10));
        assert!(!Modem::owns(crate::cpld::BRAILLE_REG));
        assert!(!Modem::owns(crate::cpld::KEYBOARD_REG));
    }
}
