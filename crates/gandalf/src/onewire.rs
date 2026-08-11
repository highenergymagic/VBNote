//! The 1-Wire EEPROM the machine keeps its serial number in.
//!
//! KeySoft asks the kernel for "serial number data" through
//! `KernelIoControl(0x01013FC4)`, and the kernel bit-bangs a Dallas/Maxim
//! 1-Wire bus on **GPIO 22** to get it. With nothing on that pin the line sits
//! idle, every read comes back failing its checksum, and KeySoft gives up
//! after fifty tries and announces *"Serial number required, please contact
//! your distributor"*.
//!
//! This is a missing part, not a defeated check. A real unit has an EEPROM
//! there with its identity in it.
//!
//! # The wire
//!
//! 1-Wire is open drain: the pull-up holds the line high and either end
//! signals by pulling it low. Everything is timed from the falling edge, so
//! this model watches transitions rather than trying to keep a clock:
//!
//! | the master holds low for | it means |
//! |---|---|
//! | ~480 µs or more | reset — the device answers with a presence pulse |
//! | ~60 µs or more | it is writing a zero |
//! | ~1–15 µs | it is writing a one, or starting a read slot |
//!
//! In a read slot the device answers by holding the line down for the rest of
//! the slot to send a zero, or leaving it alone to send a one.
//!
//! # What the kernel asks for
//!
//! `0xCC` then `0xF0` — Skip ROM, then Read Memory — then a two-byte address,
//! and then it clocks bytes out. Skip ROM is what a host uses when it knows
//! there is only one device on the bus and does not need to address it.

/// The pin. The kernel passes this as a "store id" and then hands it straight
/// to its GPIO set and clear routines, which take a pin number.
pub const PIN: u32 = 22;

/// Bytes in the EEPROM. A DS2431 holds 128; this is the same shape.
pub const SIZE: usize = 128;

/// Read ROM: hand back the 64-bit identity every 1-Wire device carries.
///
/// This is the **first** thing the kernel asks for, before it ever asks for
/// memory, and a device that does not answer it is a device the master
/// decides is not there. It matters beyond the handshake: the eight bytes are
/// where a machine's identity comes from, and KeySoft's licence check
/// compares part of its payload against what `IOCTL_HAL_GET_DEVICEID`
/// reports.
const CMD_READ_ROM: u8 = 0x33;
/// Skip ROM: there is only one device here, so it needs no addressing.
const CMD_SKIP_ROM: u8 = 0xCC;
/// Read Memory, followed by a two-byte address.
const CMD_READ_MEMORY: u8 = 0xF0;

/// How long the master holds the line down, in **OSCR ticks**, for each kind
/// of signal.
///
/// The bus has to be timed by the same clock the guest uses, and the guest
/// uses OSCR: the kernel's delay routine spins reading it, and the constants
/// it waits for land exactly where the 1-Wire specification says they should
/// at 3.6864 MHz — `0x780` is 1920 ticks and 521 µs, a reset; `0xf0` is 240
/// and 65 µs, a written zero; `0x18` is 24 and 6.5 µs, a written one.
///
/// Timing it against emulated cycles instead does not work at all, and fails
/// in a way worth remembering: the emulator ticks in batches of about thirty
/// thousand cycles, so every pull measured the same ~30,318 and every one of
/// them looked like a reset. The device saw 450 resets and not one command
/// byte.
const RESET_LOW: u32 = 1_400;
/// The longest a pull can be and still be a reset.
///
/// The specification puts a reset at 480 to 650 us, and the kernel's own
/// constant asks for 521 -- 1920 ticks, and 1926 is what it measures. So a
/// pull much longer than that is not a reset however long it is: it is a
/// zero bit that took its time, which happens here because the guest can be
/// interrupted in the middle of one. Reading those as resets threw away the
/// address of every memory read.
const RESET_HIGH: u32 = 2_400;
const WRITE_ZERO_LOW: u32 = 100;

/// How long the device holds the line down to say it is there.
///
/// A master samples for this about 70 µs after it lets the line up, and the
/// specification allows the device to hold for up to 240. Holding for 68 was
/// the first guess and it is exactly wrong: the pulse ended as the master
/// looked, so it saw nobody, and the trace was a reset pulse every two
/// thousand ticks for as long as the machine ran.
const PRESENCE_LOW: u32 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for a command byte.
    Command,
    /// Reading the two address bytes of a Read Memory.
    Address(u8),
    /// Streaming bytes out from `cursor`.
    Sending,
}

/// Dallas's CRC-8, over the polynomial x^8 + x^5 + x^4 + 1.
///
/// The last byte of a ROM id is this over the first seven, and a master that
/// checks it will reject an id that does not carry the right one.
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for b in bytes {
        let mut inbyte = *b;
        for _ in 0..8 {
            let mix = (crc ^ inbyte) & 1;
            crc >>= 1;
            if mix != 0 {
                crc ^= 0x8C;
            }
            inbyte >>= 1;
        }
    }
    crc
}

/// A well-formed ROM id: a family code, six bytes of serial, and the CRC.
pub fn rom_id(family: u8, serial: [u8; 6]) -> [u8; 8] {
    let mut id = [0u8; 8];
    id[0] = family;
    id[1..7].copy_from_slice(&serial);
    id[7] = crc8(&id[..7]);
    id
}

pub struct OneWire {
    /// The 64-bit identity: family code, serial, CRC.
    pub rom: [u8; 8],
    pub data: [u8; SIZE],
    /// Whether the master is currently holding the line down.
    master_low: bool,
    /// When it started doing so, on the guest's clock.
    fell_at: u32,
    /// Whether this device is holding the line down.
    pulling: bool,
    /// When this device should stop holding it down.
    release_at: u32,
    /// Bits of the byte being received, and how many have arrived.
    in_bits: u8,
    in_count: u8,
    /// The byte being sent and how much of it is left.
    out_bits: u8,
    out_count: u8,
    stage: Stage,
    cursor: usize,
    /// Set once a reset has been seen, so a bus with no traffic yet does not
    /// look like one mid-transfer.
    started: bool,
    /// Bytes queued to go out that are not memory — the ROM id.
    out_queue: std::collections::VecDeque<u8>,
}

impl Default for OneWire {
    fn default() -> Self {
        OneWire {
            // A DS2431's family code, and a serial that is plainly not a real
            // machine's. Anything reading this should be reading a dump.
            rom: rom_id(0x2D, [0, 0, 0, 0, 0, 0]),
            data: [0xFF; SIZE],
            master_low: false,
            fell_at: 0,
            pulling: false,
            release_at: 0,
            in_bits: 0,
            in_count: 0,
            out_bits: 0,
            out_count: 0,
            stage: Stage::Command,
            cursor: 0,
            started: false,
            out_queue: std::collections::VecDeque::new(),
        }
    }
}

impl OneWire {
    /// A part holding this memory, with the default identity.
    pub fn with_contents(bytes: &[u8]) -> Self {
        let mut d = OneWire::default();
        let n = bytes.len().min(SIZE);
        d.data[..n].copy_from_slice(&bytes[..n]);
        d
    }

    /// A part read out of a machine.
    ///
    /// The dump is the eight bytes of ROM id followed by the memory, which is
    /// the order the wire hands them over and the order a reader would write
    /// them down. A file shorter than eight bytes is treated as memory alone,
    /// so an experiment that only cares about the record still works.
    pub fn from_dump(bytes: &[u8]) -> Self {
        if bytes.len() <= 8 {
            return OneWire::with_contents(bytes);
        }
        let mut d = OneWire::with_contents(&bytes[8..]);
        d.rom.copy_from_slice(&bytes[..8]);
        d
    }

    /// What the device is driving: `false` means it is holding the line down.
    /// Nothing else on the board pulls it, so anything else reads high.
    pub fn line(&self, now: u32) -> bool {
        // The counter wraps, so "before" is a signed difference rather than
        // a comparison.
        !(self.pulling && (now.wrapping_sub(self.release_at) as i32) < 0)
    }

    /// Tell the device what the master is doing with the line.
    ///
    /// Called on every write that could change the pin, with the cycle count
    /// so durations can be measured. Returns the level the pin should now
    /// read, which is the wired-AND of both ends.
    pub fn update(&mut self, master_low: bool, now: u32) -> bool {
        if master_low && !self.master_low {
            self.fell_at = now;
        } else if !master_low && self.master_low {
            let held = now.wrapping_sub(self.fell_at);
            self.on_release(held, now);
        }
        self.master_low = master_low;
        // Open drain: low if either end is pulling.
        !master_low && self.line(now)
    }

    /// A reset has to look like one. Anything else long is a slow zero.
    fn on_release(&mut self, held: u32, now: u32) {
        if (RESET_LOW..=RESET_HIGH).contains(&held) {
            if std::env::var("VN_1W").is_ok() {
                eprintln!("[1w reset after {held} ticks]");
            }
            self.reset(now);
        } else if held >= WRITE_ZERO_LOW {
            self.bit_in(false, now);
        } else {
            // A short pull is either a written one or the start of a read
            // slot. Which it is depends on whether the device has something
            // to say; if it does, it answers now.
            if self.out_count > 0 {
                self.bit_out(now);
            } else {
                self.bit_in(true, now);
            }
        }
    }

    fn reset(&mut self, now: u32) {
        self.in_bits = 0;
        self.in_count = 0;
        self.out_bits = 0;
        self.out_count = 0;
        self.stage = Stage::Command;
        self.cursor = 0;
        self.started = true;
        self.out_queue.clear();
        // The presence pulse: hold the line down so the master sees somebody
        // is there.
        self.pulling = true;
        self.release_at = now.wrapping_add(PRESENCE_LOW);
    }

    fn bit_in(&mut self, one: bool, now: u32) {
        if !self.started {
            return;
        }
        // Least significant bit first.
        self.in_bits >>= 1;
        if one {
            self.in_bits |= 0x80;
        }
        self.in_count += 1;
        if self.in_count == 8 {
            let byte = self.in_bits;
            self.in_bits = 0;
            self.in_count = 0;
            self.byte_in(byte, now);
        }
    }

    fn byte_in(&mut self, byte: u8, _now: u32) {
        if std::env::var("VN_1W").is_ok() {
            eprintln!("[1w byte {byte:#04x} stage {:?}]", self.stage);
        }
        match self.stage {
            Stage::Command => match byte {
                CMD_READ_ROM => {
                    // The identity goes out immediately; there is no address.
                    self.out_queue.extend(self.rom);
                    self.load_next();
                }
                CMD_SKIP_ROM => {}
                CMD_READ_MEMORY => self.stage = Stage::Address(0),
                // Anything else is a command this part does not implement;
                // staying quiet is what an unprogrammed device would do.
                _ => {}
            },
            Stage::Address(0) => {
                self.cursor = byte as usize;
                self.stage = Stage::Address(1);
            }
            Stage::Address(_) => {
                self.cursor |= (byte as usize) << 8;
                self.stage = Stage::Sending;
                self.load_next();
            }
            Stage::Sending => {}
        }
    }

    /// Load the next byte to shift out: whatever is queued from a Read ROM,
    /// otherwise the next byte of memory.
    fn load_next(&mut self) {
        self.out_bits = match self.out_queue.pop_front() {
            Some(b) => b,
            None => {
                let b = self.data.get(self.cursor).copied().unwrap_or(0xFF);
                self.cursor += 1;
                b
            }
        };
        self.out_count = 8;
    }

    fn bit_out(&mut self, now: u32) {
        let one = self.out_bits & 1 != 0;
        self.out_bits >>= 1;
        self.out_count -= 1;
        if !one {
            // A zero is sent by holding the line down through the slot.
            self.pulling = true;
            self.release_at = now.wrapping_add(WRITE_ZERO_LOW);
        }
        if self.out_count == 0 && (self.stage == Stage::Sending || !self.out_queue.is_empty()) {
            self.load_next();
        }
    }
}

/// Build the record KeySoft expects on top of the wire.
///
/// `FUN_00162d70` reads a byte of length, that many bytes of data, and then a
/// checksum byte which is the sum of the data. It tries fifty times and gives
/// up if none of them check out.
pub fn record(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 2);
    v.push(payload.len() as u8);
    v.extend_from_slice(payload);
    v.push(payload.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the bus the way the kernel does and collect what comes back.
    struct Master {
        dev: OneWire,
        now: u32,
    }

    impl Master {
        fn new(dev: OneWire) -> Self {
            Master { dev, now: 1_000_000 }
        }

        fn low_for(&mut self, cycles: u32) {
            self.dev.update(true, self.now);
            self.now += cycles;
            self.dev.update(false, self.now);
            self.now += 100;
        }

        fn reset(&mut self) {
            self.low_for(RESET_LOW + 500);
            // Let the presence pulse finish.
            self.now += PRESENCE_LOW + 1;
        }

        fn write_byte(&mut self, b: u8) {
            for i in 0..8 {
                if b >> i & 1 == 0 {
                    self.low_for(WRITE_ZERO_LOW + 50);
                } else {
                    self.low_for(10);
                }
            }
        }

        fn read_byte(&mut self) -> u8 {
            let mut v = 0u8;
            for i in 0..8 {
                // A read slot starts with a short pull, then the master looks
                // at the line before the slot ends.
                self.dev.update(true, self.now);
                self.now += 10;
                let level = self.dev.update(false, self.now);
                if level {
                    v |= 1 << i;
                }
                self.now += WRITE_ZERO_LOW + 100;
            }
            v
        }
    }

    fn read_from(dev: OneWire, at: u16, n: usize) -> Vec<u8> {
        let mut m = Master::new(dev);
        m.reset();
        m.write_byte(CMD_SKIP_ROM);
        m.write_byte(CMD_READ_MEMORY);
        m.write_byte(at as u8);
        m.write_byte((at >> 8) as u8);
        (0..n).map(|_| m.read_byte()).collect()
    }


    /// Read ROM is the first thing the kernel asks for, and a device that
    /// does not answer it is one the master decides is absent. The trace of
    /// that failing is a reset pulse every two thousand ticks, forever.
    #[test]
    fn the_device_answers_read_rom_with_its_identity() {
        let want = rom_id(0x2D, [1, 2, 3, 4, 5, 6]);
        let dev = OneWire { rom: want, ..Default::default() };
        let mut m = Master::new(dev);
        m.reset();
        m.write_byte(CMD_READ_ROM);
        let got: Vec<u8> = (0..8).map(|_| m.read_byte()).collect();
        assert_eq!(got, want.to_vec());
    }

    /// The last byte of an identity is Dallas's CRC-8 over the other seven,
    /// and a master that checks it rejects one that is wrong.
    #[test]
    fn an_identity_carries_a_correct_checksum() {
        let id = rom_id(0x2D, [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]);
        assert_eq!(id[7], crc8(&id[..7]));
        // The whole thing, checksum included, sums to zero. That is how a
        // master usually tests it.
        assert_eq!(crc8(&id), 0);
    }

    /// A dump is the identity followed by the memory, in the order the wire
    /// hands them over.
    #[test]
    fn a_dump_is_read_as_identity_then_memory() {
        let mut dump = rom_id(0x2D, [9, 8, 7, 6, 5, 4]).to_vec();
        dump.extend_from_slice(b"payload");
        let dev = OneWire::from_dump(&dump);
        assert_eq!(dev.rom[1..7], [9, 8, 7, 6, 5, 4]);
        assert_eq!(&dev.data[..7], b"payload");
    }

    #[test]
    fn a_host_can_read_back_what_the_device_holds() {
        let want = b"hello 1-wire";
        let got = read_from(OneWire::with_contents(want), 0, want.len());
        assert_eq!(got, want);
    }

    /// The address is two bytes, low first, and reading is sequential from
    /// there. Getting the order wrong reads from the wrong end of the part.
    #[test]
    fn reading_starts_at_the_address_it_was_given() {
        let mut contents = [0u8; SIZE];
        for (i, c) in contents.iter_mut().enumerate() {
            *c = i as u8;
        }
        let got = read_from(OneWire::with_contents(&contents), 4, 4);
        assert_eq!(got, vec![4, 5, 6, 7]);
    }

    /// Every read starts with a reset, and the master takes the presence
    /// pulse as proof something is there. Without one it decides the bus is
    /// empty, which is what an unmodelled pin looks like.
    #[test]
    fn a_reset_is_answered_with_a_presence_pulse() {
        let mut dev = OneWire::default();
        let now = 1_000_000u32;
        dev.update(true, now);
        let after = now + RESET_LOW + 500;
        let level = dev.update(false, after);
        assert!(!level, "the device should be holding the line down");
        assert!(dev.line(after + PRESENCE_LOW + 1), "and let go again afterwards");
        assert!(!dev.line(after + PRESENCE_LOW / 2), "held long enough to be seen");
    }

    /// A record is a length, the data, and the sum of the data. KeySoft
    /// checks the sum and rejects fifty times before it complains.
    #[test]
    fn a_record_carries_its_length_and_checksum() {
        let r = record(b"ABC");
        assert_eq!(r[0], 3, "length first");
        assert_eq!(&r[1..4], b"ABC");
        assert_eq!(r[4], b'A'.wrapping_add(b'B').wrapping_add(b'C'), "then the sum");
    }

    /// The whole path, as the machine walks it: a record written into the
    /// part comes back out through the wire intact.
    #[test]
    fn a_record_survives_the_round_trip_over_the_wire() {
        let payload = b"50235 Australia BNB";
        let blob = record(payload);
        let got = read_from(OneWire::with_contents(&blob), 0, blob.len());
        assert_eq!(got, blob);
        let len = got[0] as usize;
        let sum = got[1..1 + len].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, got[1 + len], "the checksum KeySoft computes must match");
    }
}
