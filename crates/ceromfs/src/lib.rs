//! Reading Windows CE `B000FF` ROM images.
//!
//! The format is a signature, a base address and length, then a list of
//! records, each with a target address, a length and a checksum. A final
//! record with a zero address carries the launch address.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    BadSignature,
    Truncated,
    BadChecksum { addr: u32, expected: u32, actual: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadSignature => write!(f, "not a B000FF Windows CE image"),
            Error::Truncated => write!(f, "image ends in the middle of a record"),
            Error::BadChecksum { addr, expected, actual } => write!(
                f,
                "record at {addr:#010x} has checksum {actual:#x}, expected {expected:#x}"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug)]pub struct Record {
    pub addr: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]pub struct CeImage {
    /// Address the image is linked at.
    pub base: u32,
    /// Span the image covers, which is larger than the sum of the records.
    pub length: u32,
    /// Where to start executing.
    pub launch: u32,
    pub records: Vec<Record>,
}

const SIGNATURE: &[u8] = b"B000FF\n";

fn u32_at(bytes: &[u8], off: usize) -> Result<u32, Error> {
    bytes
        .get(off..off + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or(Error::Truncated)
}

impl CeImage {
    pub fn parse(bytes: &[u8]) -> Result<CeImage, Error> {
        if !bytes.starts_with(SIGNATURE) {
            return Err(Error::BadSignature);
        }
        let base = u32_at(bytes, 7)?;
        let length = u32_at(bytes, 11)?;

        let mut off = 15;
        let mut records = Vec::new();
        let mut launch = base;

        while off + 12 <= bytes.len() {
            let addr = u32_at(bytes, off)?;
            let len = u32_at(bytes, off + 4)?;
            let checksum = u32_at(bytes, off + 8)?;
            off += 12;

            // The terminating record has a zero address. Different builds of
            // the CE tools put the launch address in either remaining field.
            if addr == 0 && (len == 0 || off + len as usize > bytes.len()) {
                launch = if len != 0 { len } else { checksum };
                break;
            }

            let end = off + len as usize;
            let data = bytes.get(off..end).ok_or(Error::Truncated)?.to_vec();
            off = end;

            let actual: u32 = data.iter().map(|b| *b as u32).sum();
            if actual != checksum {
                return Err(Error::BadChecksum { addr, expected: checksum, actual });
            }
            records.push(Record { addr, data });
        }

        Ok(CeImage { base, length, launch, records })
    }

    /// Total bytes across every record.
    pub fn payload_len(&self) -> usize {
        self.records.iter().map(|r| r.data.len()).sum()
    }

    /// Lowest and highest addresses any record touches.
    pub fn extent(&self) -> Option<(u32, u32)> {
        let lo = self.records.iter().map(|r| r.addr).min()?;
        let hi = self.records.iter().map(|r| r.addr + r.data.len() as u32).max()?;
        Some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(records: &[(u32, &[u8])], launch: u32) -> Vec<u8> {
        let mut v = SIGNATURE.to_vec();
        v.extend_from_slice(&0x8000_0000u32.to_le_bytes());
        v.extend_from_slice(&0x1000u32.to_le_bytes());
        for (addr, data) in records {
            v.extend_from_slice(&addr.to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            let sum: u32 = data.iter().map(|b| *b as u32).sum();
            v.extend_from_slice(&sum.to_le_bytes());
            v.extend_from_slice(data);
        }
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&launch.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    }

    #[test]
    fn parses_records_and_launch_address() {
        let img = CeImage::parse(&build(&[(0x8000_1000, &[1, 2, 3, 4])], 0x8000_1000)).unwrap();
        assert_eq!(img.base, 0x8000_0000);
        assert_eq!(img.launch, 0x8000_1000);
        assert_eq!(img.records.len(), 1);
        assert_eq!(img.records[0].data, vec![1, 2, 3, 4]);
        assert_eq!(img.extent(), Some((0x8000_1000, 0x8000_1004)));
    }

    #[test]
    fn rejects_a_bad_signature() {
        assert!(matches!(CeImage::parse(b"nope").unwrap_err(), Error::BadSignature));
    }

    #[test]
    fn rejects_a_corrupted_record() {
        let mut bytes = build(&[(0x8000_1000, &[1, 2, 3, 4])], 0x8000_1000);
        let len = bytes.len();
        bytes[len - 13] ^= 0xFF; // flip a payload byte
        assert!(matches!(CeImage::parse(&bytes).unwrap_err(), Error::BadChecksum { .. }));
    }
}
