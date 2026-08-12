//! A USB flash drive: descriptors, bulk-only transport, and enough SCSI.
//!
//! What the guest has to recognise this as, and why each choice is forced:
//!
//! - **Interface class 8, subclass 6, protocol 0x50.** Mass storage, SCSI
//!   transparent command set, bulk-only transport. `usbmsc.dll` binds on that
//!   triple; anything else and the device enumerates and is then ignored.
//! - **Two bulk endpoints**, one in, one out, 64 bytes. Bulk-only has no use
//!   for an interrupt endpoint and a driver that finds one may not care, but
//!   one that cannot find both bulk endpoints has nothing to talk over.
//! - **USB 1.1.** This is an OHCI root port; claiming 2.0 invites a driver to
//!   expect high speed from a controller that has none.
//!
//! # Bulk-only transport
//!
//! Every exchange is three steps, always in this order: a 31-byte command
//! block wrapper out, then data in whichever direction the wrapper said, then
//! a 13-byte command status wrapper back. The tag in the wrapper is echoed in
//! the status, and a driver checks it -- getting that wrong looks like the
//! device answering somebody else's question.
//!
//! The residue is the part everyone gets wrong: it is how many bytes of what
//! was *asked for* did not arrive, not how many did.

use std::collections::BTreeMap;

use crate::fat32::Store;
use crate::usb::Device;

/// Sector size, and nothing here changes it.
pub const SECTOR: usize = 512;

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;

const STATUS_GOOD: u8 = 0;
const STATUS_FAILED: u8 = 1;

/// Where the transport has got to.
#[derive(PartialEq, Debug, Clone, Copy)]
enum Phase {
    /// Waiting for a command block wrapper.
    Command,
    /// Handing data to the host, then the status.
    DataIn,
    /// Taking data from the host, then the status.
    DataOut,
    /// The command is done; the host has yet to collect the status.
    Status,
}

pub struct UsbDisk {
    /// The disk itself. A file for a real one, so that a 32 GB drive is 32 GB
    /// of sparse file rather than 32 GB of memory.
    pub store: Store,
    address: u8,
    configured: bool,

    phase: Phase,
    tag: u32,
    /// How much the host said it would move.
    expected: u32,
    /// Bytes handed over or taken so far.
    moved: u32,
    status: u8,
    /// Data waiting to go to the host.
    to_host: Vec<u8>,
    /// Where a write is going, and how much is left of it.
    write_at: u64,
    write_left: usize,
    /// The answer to REQUEST SENSE: key, then additional code and qualifier.
    sense: (u8, u8, u8),
    /// Commands seen, so a class driver that bound can be told from one that
    /// never did. Enumeration and use are separate questions.
    pub commands: u64,
    /// Which commands, and how many of each. A retry loop and a long job
    /// look identical in a total; they do not look alike in this.
    pub opcodes: BTreeMap<u8, u64>,
    /// The last block a read asked for, how often a read asked for the same
    /// one again, and the furthest it has got. A driver making progress walks
    /// forward; a stuck one asks for the same block for ever.
    pub last_read: u32,
    pub repeated_reads: u64,
    pub highest_read: u32,
}

impl UsbDisk {
    pub fn new(store: Store) -> UsbDisk {
        UsbDisk {
            store,
            address: 0,
            configured: false,
            phase: Phase::Command,
            tag: 0,
            expected: 0,
            moved: 0,
            status: STATUS_GOOD,
            to_host: Vec::new(),
            write_at: 0,
            write_left: 0,
            sense: (0, 0, 0),
            commands: 0,
            opcodes: BTreeMap::new(),
            last_read: u32::MAX,
            repeated_reads: 0,
            highest_read: 0,
        }
    }

    pub fn blank(megabytes: usize) -> UsbDisk {
        UsbDisk::new(Store::memory(megabytes * 1024 * 1024))
    }

    pub fn sectors(&self) -> u32 {
        (self.store.len() / SECTOR as u64) as u32
    }

    fn descriptor(&self, kind: u8, index: u8) -> Option<Vec<u8>> {
        match kind {
            // Device.
            1 => Some(vec![
                18, 1, 0x10, 0x01, // USB 1.1
                0, 0, 0, 64, // class in the interface, 64-byte control endpoint
                0xC0, 0x16, // vendor
                0x01, 0x00, // product
                0x00, 0x01, // device release 1.00
                1, 2, 3, // manufacturer, product, serial
                1,  // one configuration
            ]),
            // Configuration, with the interface and endpoints behind it.
            2 => {
                let mut d = vec![
                    9, 2, 32, 0, // 32 bytes in total
                    1, 1, 0, 0x80, 50,
                ];
                // Interface: mass storage, SCSI transparent, bulk-only.
                d.extend_from_slice(&[9, 4, 0, 0, 2, 0x08, 0x06, 0x50, 0]);
                // Bulk in, endpoint 1.
                d.extend_from_slice(&[7, 5, 0x81, 0x02, 64, 0, 0]);
                // Bulk out, endpoint 2.
                d.extend_from_slice(&[7, 5, 0x02, 0x02, 64, 0, 0]);
                Some(d)
            }
            // Strings.
            3 => {
                let text = match index {
                    0 => return Some(vec![4, 3, 0x09, 0x04]), // English
                    1 => "Fractal Microsystems",
                    2 => "VBNote Storage Card",
                    3 => "VBNOTE0001",
                    _ => return None,
                };
                let mut d = vec![0, 3];
                for unit in text.encode_utf16() {
                    d.extend_from_slice(&unit.to_le_bytes());
                }
                d[0] = d.len() as u8;
                Some(d)
            }
            _ => None,
        }
    }

    /// A command block wrapper has arrived.
    fn command(&mut self, cbw: &[u8]) {
        if cbw.len() < 31 || u32::from_le_bytes(cbw[0..4].try_into().unwrap()) != CBW_SIGNATURE {
            // Not a wrapper. A real device stalls; pretending it was fine
            // would desynchronise the transport for good.
            self.status = STATUS_FAILED;
            self.phase = Phase::Status;
            return;
        }
        self.tag = u32::from_le_bytes(cbw[4..8].try_into().unwrap());
        self.expected = u32::from_le_bytes(cbw[8..12].try_into().unwrap());
        let to_host = cbw[12] & 0x80 != 0;
        self.moved = 0;
        self.status = STATUS_GOOD;
        self.to_host.clear();
        self.write_left = 0;

        self.commands += 1;
        let cb = &cbw[15..31];
        *self.opcodes.entry(cb[0]).or_insert(0) += 1;
        if cb[0] == 0x28 {
            let lba = u32::from_be_bytes([cb[2], cb[3], cb[4], cb[5]]);
            if lba == self.last_read {
                self.repeated_reads += 1;
            }
            self.last_read = lba;
            self.highest_read = self.highest_read.max(lba);
        }
        self.scsi(cb);

        self.phase = if self.expected == 0 {
            Phase::Status
        } else if to_host {
            Phase::DataIn
        } else {
            Phase::DataOut
        };
    }

    fn fail(&mut self, key: u8, asc: u8, ascq: u8) {
        self.status = STATUS_FAILED;
        self.sense = (key, asc, ascq);
    }

    fn scsi(&mut self, cb: &[u8]) {
        let lba = || u32::from_be_bytes([cb[2], cb[3], cb[4], cb[5]]);
        let count16 = || u16::from_be_bytes([cb[7], cb[8]]) as u32;
        match cb[0] {
            // TEST UNIT READY: no data, and ready is the answer.
            0x00 => self.sense = (0, 0, 0),
            // REQUEST SENSE.
            0x03 => {
                let (key, asc, ascq) = self.sense;
                let mut d = vec![0u8; 18];
                d[0] = 0x70;
                d[2] = key;
                d[7] = 10;
                d[12] = asc;
                d[13] = ascq;
                self.to_host = d;
                self.sense = (0, 0, 0);
            }
            // INQUIRY. The strings are padded to their fixed widths because
            // the fields are fixed-width, not terminated.
            0x12 => {
                let mut d = vec![0u8; 36];
                d[0] = 0x00; // direct access block device
                d[1] = 0x80; // removable
                d[2] = 0x02; // SCSI-2
                d[3] = 0x02;
                d[4] = 31; // additional length
                d[8..16].copy_from_slice(b"Fractal ");
                d[16..32].copy_from_slice(b"VBNote Storage  ");
                d[32..36].copy_from_slice(b"1.00");
                self.to_host = d;
            }
            // MODE SENSE(6): a minimal header, not write protected.
            0x1A => self.to_host = vec![3, 0, 0, 0],
            // PREVENT/ALLOW MEDIUM REMOVAL, START STOP UNIT, SYNCHRONIZE
            // CACHE: nothing to do, and all three must succeed or the driver
            // decides the device is faulty.
            0x1E | 0x1B | 0x35 => {}
            // READ CAPACITY(10): the address of the *last* sector, not the
            // count. One out here is a disk one sector too big, and the error
            // only shows up when something reads the end of it.
            0x25 => {
                let last = self.sectors().saturating_sub(1);
                let mut d = Vec::with_capacity(8);
                d.extend_from_slice(&last.to_be_bytes());
                d.extend_from_slice(&(SECTOR as u32).to_be_bytes());
                self.to_host = d;
            }
            // READ(10).
            0x28 => {
                let (lba, n) = (lba() as u64, count16() as u64);
                let at = lba * SECTOR as u64;
                let end = at + n * SECTOR as u64;
                if end > self.store.len() {
                    // Logical block address out of range.
                    self.fail(5, 0x21, 0);
                } else {
                    self.to_host = self.store.read(at, (n * SECTOR as u64) as usize);
                }
            }
            // WRITE(10).
            0x2A => {
                let (lba, n) = (lba() as u64, count16() as u64);
                let at = lba * SECTOR as u64;
                if at + n * SECTOR as u64 > self.store.len() {
                    self.fail(5, 0x21, 0);
                } else {
                    self.write_at = at;
                    self.write_left = (n * SECTOR as u64) as usize;
                }
            }
            // Anything else: invalid command operation code. Saying so is
            // better than succeeding, which leaves the driver believing
            // something happened.
            _ => self.fail(5, 0x20, 0),
        }
    }

    /// The 13-byte command status wrapper.
    fn csw(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(13);
        d.extend_from_slice(&CSW_SIGNATURE.to_le_bytes());
        d.extend_from_slice(&self.tag.to_le_bytes());
        // Residue is what was asked for and did not arrive.
        d.extend_from_slice(&self.expected.saturating_sub(self.moved).to_le_bytes());
        d.push(self.status);
        d
    }
}

impl Device for UsbDisk {
    fn commands(&self) -> u64 {
        self.commands
    }

    fn summary(&self) -> String {
        let mut out = String::new();
        for (op, n) in &self.opcodes {
            let name = match op {
                0x00 => "TEST UNIT READY",
                0x03 => "REQUEST SENSE",
                0x12 => "INQUIRY",
                0x1A => "MODE SENSE(6)",
                0x1B => "START STOP UNIT",
                0x1E => "PREVENT REMOVAL",
                0x23 => "READ FORMAT CAPACITIES",
                0x25 => "READ CAPACITY",
                0x28 => "READ(10)",
                0x2A => "WRITE(10)",
                0x35 => "SYNCHRONIZE CACHE",
                0x5A => "MODE SENSE(10)",
                _ => "unknown",
            };
            out.push_str(&format!("
    {op:#04x} {name:<22} x{n}"));
        }
        out.push_str(&format!(
            "
    reads: highest block {}, {} asked for twice running",
            self.highest_read, self.repeated_reads
        ));
        out
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn control(&mut self, setup: &[u8; 8], data_out: &[u8]) -> Option<Vec<u8>> {
        let request_type = setup[0];
        let request = setup[1];
        let value = u16::from_le_bytes([setup[2], setup[3]]);
        let length = u16::from_le_bytes([setup[6], setup[7]]) as usize;
        let _ = data_out;

        // Class requests, to the interface.
        if request_type & 0x60 == 0x20 {
            return match request {
                // GET MAX LUN: one drive, so zero.
                0xFE => Some(vec![0]),
                // Bulk-only mass storage reset: back to waiting for a command.
                0xFF => {
                    self.phase = Phase::Command;
                    self.to_host.clear();
                    self.write_left = 0;
                    Some(Vec::new())
                }
                _ => None,
            };
        }

        match request {
            // GET_STATUS: self-powered, not remote-wakeup.
            0x00 => Some(vec![1, 0]),
            // CLEAR_FEATURE, on an endpoint: clearing a halt.
            0x01 => Some(Vec::new()),
            // SET_ADDRESS.
            0x05 => {
                self.address = value as u8;
                Some(Vec::new())
            }
            // GET_DESCRIPTOR. Truncated to what was asked for, because the
            // host reads the first eight bytes to find out how long the real
            // answer is before asking again.
            0x06 => {
                let mut d = self.descriptor((value >> 8) as u8, value as u8)?;
                d.truncate(length);
                Some(d)
            }
            // GET_CONFIGURATION / SET_CONFIGURATION.
            0x08 => Some(vec![u8::from(self.configured)]),
            0x09 => {
                self.configured = value != 0;
                Some(Vec::new())
            }
            // SET_INTERFACE, and its query.
            0x0A => Some(vec![0]),
            0x0B => Some(Vec::new()),
            _ => None,
        }
    }

    fn bulk_in(&mut self, _endpoint: u8, len: usize) -> Vec<u8> {
        match self.phase {
            Phase::DataIn => {
                let n = len.min(self.to_host.len());
                let out: Vec<u8> = self.to_host.drain(..n).collect();
                self.moved += out.len() as u32;
                if self.to_host.is_empty() {
                    self.phase = Phase::Status;
                }
                out
            }
            // The host is collecting the status.
            Phase::Status | Phase::Command => {
                let csw = self.csw();
                self.phase = Phase::Command;
                csw
            }
            Phase::DataOut => Vec::new(),
        }
    }

    fn bulk_out(&mut self, _endpoint: u8, data: &[u8]) {
        match self.phase {
            Phase::Command => self.command(data),
            Phase::DataOut => {
                let n = data.len().min(self.write_left);
                if n > 0 {
                    let at = self.write_at;
                    self.store.write(at, &data[..n]);
                    self.write_at += n as u64;
                    self.write_left -= n;
                }
                self.moved += data.len() as u32;
                if self.write_left == 0 || self.moved >= self.expected {
                    self.phase = Phase::Status;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbw(tag: u32, len: u32, to_host: bool, cb: &[u8]) -> Vec<u8> {
        let mut d = Vec::with_capacity(31);
        d.extend_from_slice(&CBW_SIGNATURE.to_le_bytes());
        d.extend_from_slice(&tag.to_le_bytes());
        d.extend_from_slice(&len.to_le_bytes());
        d.push(if to_host { 0x80 } else { 0 });
        d.push(0);
        d.push(cb.len() as u8);
        let mut block = cb.to_vec();
        block.resize(16, 0);
        d.extend_from_slice(&block);
        d
    }

    /// The triple usbmsc.dll binds on. Get any of the three wrong and the
    /// device enumerates and is then ignored, which looks like nothing
    /// happening at all.
    #[test]
    fn it_declares_itself_mass_storage_bulk_only() {
        let disk = UsbDisk::blank(1);
        let config = disk.descriptor(2, 0).unwrap();
        let interface = &config[9..18];
        assert_eq!(interface[5], 0x08, "class is not mass storage");
        assert_eq!(interface[6], 0x06, "subclass is not SCSI transparent");
        assert_eq!(interface[7], 0x50, "protocol is not bulk-only");
        assert_eq!(interface[4], 2, "wrong number of endpoints");
    }

    /// The total length must count everything behind it, or the host reads a
    /// truncated configuration and never finds the endpoints.
    #[test]
    fn the_configuration_length_covers_what_follows() {
        let disk = UsbDisk::blank(1);
        let config = disk.descriptor(2, 0).unwrap();
        let total = u16::from_le_bytes([config[2], config[3]]) as usize;
        assert_eq!(total, config.len());
        assert_eq!(total, 9 + 9 + 7 + 7);
    }

    /// One bulk endpoint each way, which is what the transport needs.
    #[test]
    fn there_is_a_bulk_endpoint_each_way() {
        let disk = UsbDisk::blank(1);
        let config = disk.descriptor(2, 0).unwrap();
        let (a, b) = (&config[18..25], &config[25..32]);
        assert_eq!(a[2] & 0x80, 0x80, "first endpoint is not an IN");
        assert_eq!(b[2] & 0x80, 0, "second endpoint is not an OUT");
        assert_eq!(a[3], 2, "not a bulk endpoint");
        assert_eq!(b[3], 2, "not a bulk endpoint");
    }

    /// A descriptor request is answered short, because the host asks for
    /// eight bytes first to find out how long the answer really is.
    #[test]
    fn a_short_descriptor_request_is_answered_short() {
        let mut disk = UsbDisk::blank(1);
        let setup = [0x80, 0x06, 0x00, 0x01, 0, 0, 8, 0];
        let d = disk.control(&setup, &[]).unwrap();
        assert_eq!(d.len(), 8);
        assert_eq!(d[0], 18, "the length byte should still say 18");
    }

    #[test]
    fn set_address_is_remembered() {
        let mut disk = UsbDisk::blank(1);
        assert_eq!(disk.address(), 0);
        disk.control(&[0x00, 0x05, 7, 0, 0, 0, 0, 0], &[]);
        assert_eq!(disk.address(), 7);
    }

    /// One drive, so the maximum logical unit number is zero. A driver that
    /// gets no answer at all may give up on the device.
    #[test]
    fn it_reports_one_logical_unit() {
        let mut disk = UsbDisk::blank(1);
        let d = disk.control(&[0xA1, 0xFE, 0, 0, 0, 0, 1, 0], &[]).unwrap();
        assert_eq!(d, vec![0]);
    }

    /// The whole exchange, in the order a driver does it.
    #[test]
    fn inquiry_comes_back_with_a_status() {
        let mut disk = UsbDisk::blank(1);
        disk.bulk_out(2, &cbw(0x1234, 36, true, &[0x12, 0, 0, 0, 36, 0]));
        let data = disk.bulk_in(1, 64);
        assert_eq!(data.len(), 36);
        assert_eq!(&data[8..16], b"Fractal ");

        let csw = disk.bulk_in(1, 13);
        assert_eq!(&csw[0..4], &CSW_SIGNATURE.to_le_bytes());
        assert_eq!(u32::from_le_bytes(csw[4..8].try_into().unwrap()), 0x1234);
        assert_eq!(csw[12], STATUS_GOOD);
    }

    /// READ CAPACITY reports the last sector's address, not the count. One
    /// out here is a disk a sector too large and a fault that only appears
    /// when something reads the very end.
    #[test]
    fn read_capacity_reports_the_last_sector_not_the_count() {
        let mut disk = UsbDisk::blank(1);
        let sectors = disk.sectors();
        disk.bulk_out(2, &cbw(1, 8, true, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        let d = disk.bulk_in(1, 8);
        assert_eq!(u32::from_be_bytes(d[0..4].try_into().unwrap()), sectors - 1);
        assert_eq!(u32::from_be_bytes(d[4..8].try_into().unwrap()), SECTOR as u32);
    }

    #[test]
    fn a_sector_written_reads_back() {
        let mut disk = UsbDisk::blank(1);
        let payload: Vec<u8> = (0..SECTOR).map(|i| (i % 251) as u8).collect();

        let write = [0x2A, 0, 0, 0, 0, 9, 0, 0, 1, 0];
        disk.bulk_out(2, &cbw(2, SECTOR as u32, false, &write));
        disk.bulk_out(2, &payload);
        assert_eq!(disk.bulk_in(1, 13)[12], STATUS_GOOD);

        let read = [0x28, 0, 0, 0, 0, 9, 0, 0, 1, 0];
        disk.bulk_out(2, &cbw(3, SECTOR as u32, true, &read));
        let got = disk.bulk_in(1, SECTOR);
        assert_eq!(got, payload);
        assert_eq!(disk.store.read(9 * SECTOR as u64, SECTOR), payload);
    }

    /// Reading past the end fails rather than returning zeroes, and the
    /// failure is legible: sense key 5, "logical block address out of range".
    #[test]
    fn reading_off_the_end_fails_and_says_why() {
        let mut disk = UsbDisk::blank(1);
        let far = disk.sectors() + 10;
        let mut cb = [0u8; 10];
        cb[0] = 0x28;
        cb[2..6].copy_from_slice(&far.to_be_bytes());
        cb[8] = 1;
        disk.bulk_out(2, &cbw(4, SECTOR as u32, true, &cb));
        // The host still asks for its data, and gets a zero-length packet
        // because there is none, before collecting the status.
        assert!(disk.bulk_in(1, SECTOR).is_empty());
        assert_eq!(disk.bulk_in(1, 13)[12], STATUS_FAILED);

        disk.bulk_out(2, &cbw(5, 18, true, &[0x03, 0, 0, 0, 18, 0]));
        let sense = disk.bulk_in(1, 18);
        assert_eq!(sense[2], 5, "not an illegal-request sense key");
        assert_eq!(sense[12], 0x21, "not 'address out of range'");
    }

    /// The residue is what was asked for and did not arrive. Reporting the
    /// bytes that *did* is the classic way round to get this wrong.
    #[test]
    fn the_residue_counts_what_did_not_arrive() {
        let mut disk = UsbDisk::blank(1);
        // Ask for 64 bytes of an inquiry that is only 36 long.
        disk.bulk_out(2, &cbw(6, 64, true, &[0x12, 0, 0, 0, 36, 0]));
        let data = disk.bulk_in(1, 64);
        assert_eq!(data.len(), 36);
        let csw = disk.bulk_in(1, 13);
        // Bytes 4 to 8 are the tag; the residue is the word after it.
        let residue = u32::from_le_bytes(csw[8..12].try_into().unwrap());
        assert_eq!(residue, 64 - 36);
    }

    /// An unknown command is refused. Succeeding would leave the driver
    /// believing something happened.
    #[test]
    fn an_unknown_command_is_refused() {
        let mut disk = UsbDisk::blank(1);
        disk.bulk_out(2, &cbw(7, 0, false, &[0xFF, 0, 0, 0, 0, 0]));
        assert_eq!(disk.bulk_in(1, 13)[12], STATUS_FAILED);
    }

    /// Rubbish where a wrapper should be must not be taken as a command.
    #[test]
    fn a_broken_wrapper_is_not_obeyed() {
        let mut disk = UsbDisk::blank(1);
        disk.bulk_out(2, &[0u8; 31]);
        assert_eq!(disk.bulk_in(1, 13)[12], STATUS_FAILED);
    }

    /// Test unit ready is the first thing a driver asks, and it must succeed
    /// or the disk is treated as having no medium.
    #[test]
    fn test_unit_ready_succeeds() {
        let mut disk = UsbDisk::blank(1);
        disk.bulk_out(2, &cbw(8, 0, false, &[0x00, 0, 0, 0, 0, 0]));
        assert_eq!(disk.bulk_in(1, 13)[12], STATUS_GOOD);
    }
}
