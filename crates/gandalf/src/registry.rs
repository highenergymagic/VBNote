//! Just enough of the Windows CE ROM registry to find one value.
//!
//! The boot registry is compiled into `NK.bin` as a flat run of records.
//! Each value is:
//!
//! ```text
//! u16 size          bytes from here to the next record, less this field and the next
//! u16 unknown       always 2 in this image
//! u16 data_len
//! u16 name_len      characters, including the terminating NUL
//! u16 type          4 is REG_DWORD
//! u16 name[name_len]
//! u8  data[data_len]
//! ```
//!
//! Which is enough to find a named DWORD and read or change it, without
//! implementing the format.
//!
//! # Why this is here
//!
//! `trueffs.dll` will not format a blank medium unless the registry value
//! **`AutoFormat`** under `Drivers\BuiltIn\TrueFFS` reads back non-zero
//! (`0x02226794`: the query has to succeed *and* the value has to be
//! non-zero, or it skips straight past and reports the medium unusable).
//! In this ROM it is zero, because a real unit leaves the factory with its
//! flash already formatted.
//!
//! An emulated unit starts with a blank disk and nothing to format it, so
//! first-boot provisioning sets it. The change is made to the flash image
//! being built, never to `NK.bin` on disk.

/// A DWORD value found in the compiled registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DwordValue {
    /// Offset of the record header within the image.
    pub record: usize,
    /// Offset of the four data bytes within the image.
    pub data: usize,
    pub value: u32,
}

const TYPE_DWORD: u16 = 4;

fn u16_at(image: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*image.get(at)?, *image.get(at + 1)?]))
}

/// Find every REG_DWORD with this name.
///
/// The name is not unique: this image carries an `AutoFormat` under each
/// storage profile, six in all. Which one belongs to TrueFFS would need the
/// key structure, not just the value records — so callers that want to be
/// sure of hitting the right one set all of them, which for an emulator whose
/// disks all start blank is the intended answer anyway.
pub fn find_all_dwords(image: &[u8], name: &str) -> Vec<DwordValue> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(v) = find_dword_from(image, name, from) {
        // Past the data, not past the header: `record + 10` is exactly where
        // this record's name starts, so resuming there finds the same record
        // again, forever.
        from = v.data + 4;
        out.push(v);
    }
    out
}

/// Set every REG_DWORD with this name, returning what each was.
pub fn set_all_dwords(image: &mut [u8], name: &str, value: u32) -> Vec<DwordValue> {
    let found = find_all_dwords(image, name);
    for v in &found {
        image[v.data..v.data + 4].copy_from_slice(&value.to_le_bytes());
    }
    found
}

/// Find a named REG_DWORD.
///
/// The name is matched as a whole record rather than as a loose string, so a
/// mention of the same text elsewhere in the image cannot be mistaken for the
/// value: the header in front of it has to describe a four-byte DWORD whose
/// name is exactly this long.
pub fn find_dword(image: &[u8], name: &str) -> Option<DwordValue> {
    find_dword_from(image, name, 0)
}

fn find_dword_from(image: &[u8], name: &str, start: usize) -> Option<DwordValue> {
    let wide: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    // The header is five halfwords, and the name follows it.
    const HEADER: usize = 10;
    let name_chars = name.chars().count() as u16 + 1;

    let mut from = start;
    while let Some(found) = find_from(image, &wide, from) {
        from = found + 2;
        if found < HEADER {
            continue;
        }
        let record = found - HEADER;
        let data_len = u16_at(image, record + 4)?;
        let chars = u16_at(image, record + 6)?;
        let kind = u16_at(image, record + 8)?;
        if data_len != 4 || chars != name_chars || kind != TYPE_DWORD {
            continue;
        }
        // And the name must be NUL terminated where the header says.
        let data = found + wide.len() + 2;
        if u16_at(image, found + wide.len()) != Some(0) {
            continue;
        }
        let bytes = image.get(data..data + 4)?;
        return Some(DwordValue {
            record,
            data,
            value: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        });
    }
    None
}

fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// Set a named REG_DWORD, returning what it was.
pub fn set_dword(image: &mut [u8], name: &str, value: u32) -> Option<u32> {
    let found = find_dword(image, name)?;
    image[found.data..found.data + 4].copy_from_slice(&value.to_le_bytes());
    Some(found.value)
}

/// The value that decides whether TrueFFS will format a blank medium.
pub const AUTO_FORMAT: &str = "AutoFormat";

/// Whether the storage manager creates a partition table on a medium that
/// has none. Off in this image, because a disk that leaves the factory is
/// already partitioned — which leaves a blank emulated one with nowhere for a
/// filesystem to go.
pub const AUTO_PART: &str = "AutoPart";

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a record the way the image does, so the tests exercise the
    /// layout rather than a fixture copied out of it.
    fn record(name: &str, kind: u16, data: &[u8]) -> Vec<u8> {
        let wide: Vec<u8> = name.encode_utf16().chain(Some(0)).flat_map(|c| c.to_le_bytes()).collect();
        let mut out = Vec::new();
        let size = (10 + wide.len() + data.len() - 4) as u16;
        for h in [size, 2, data.len() as u16, name.chars().count() as u16 + 1, kind] {
            out.extend_from_slice(&h.to_le_bytes());
        }
        out.extend_from_slice(&wide);
        out.extend_from_slice(data);
        out
    }

    fn image() -> Vec<u8> {
        let mut v = vec![0xAB; 32];
        v.extend(record("MountFlags", TYPE_DWORD, &[0, 0, 0, 0]));
        v.extend(record("AutoFormat", TYPE_DWORD, &[0, 0, 0, 0]));
        v.extend(record("AutoPart", TYPE_DWORD, &[1, 0, 0, 0]));
        v
    }

    #[test]
    fn a_named_dword_is_found_with_its_value() {
        let img = image();
        let found = find_dword(&img, AUTO_FORMAT).expect("AutoFormat should be there");
        assert_eq!(found.value, 0);
        assert_eq!(find_dword(&img, "AutoPart").unwrap().value, 1);
    }

    #[test]
    fn setting_it_reports_what_it_was_and_leaves_the_neighbours_alone() {
        let mut img = image();
        assert_eq!(set_dword(&mut img, AUTO_FORMAT, 1), Some(0));
        assert_eq!(find_dword(&img, AUTO_FORMAT).unwrap().value, 1);
        assert_eq!(find_dword(&img, "MountFlags").unwrap().value, 0);
        assert_eq!(find_dword(&img, "AutoPart").unwrap().value, 1);
    }

    #[test]
    fn a_loose_mention_of_the_name_is_not_mistaken_for_the_value() {
        // The same text with no record header in front of it, placed first.
        let mut img: Vec<u8> = "AutoFormat".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        img.extend(image());
        let found = find_dword(&img, AUTO_FORMAT).expect("the real record should win");
        assert_eq!(found.value, 0);
        assert!(found.record > 10, "it should not have matched the bare string");
    }

    #[test]
    fn a_record_of_the_wrong_type_or_width_is_skipped() {
        let mut img = vec![0u8; 8];
        img.extend(record(AUTO_FORMAT, 1, &[0, 0, 0, 0])); // REG_SZ, not DWORD
        assert_eq!(find_dword(&img, AUTO_FORMAT), None);
    }

    #[test]
    fn every_copy_of_a_name_is_found_and_can_be_set() {
        let mut img = image();
        img.extend(record(AUTO_FORMAT, TYPE_DWORD, &[0, 0, 0, 0]));
        assert_eq!(find_all_dwords(&img, AUTO_FORMAT).len(), 2, "one per profile");
        let was = set_all_dwords(&mut img, AUTO_FORMAT, 1);
        assert_eq!(was.len(), 2);
        assert!(find_all_dwords(&img, AUTO_FORMAT).iter().all(|v| v.value == 1));
    }

    /// The scan must move past each record it returns.
    ///
    /// Resuming at `record + 10` lands on the same record's name and finds it
    /// again, which does not fail — it loops until memory runs out, and takes
    /// the emulator with it, since provisioning calls this on every boot.
    #[test]
    fn scanning_terminates_and_each_record_is_returned_once() {
        let mut img = image();
        img.extend(record(AUTO_FORMAT, TYPE_DWORD, &[0, 0, 0, 0]));
        img.extend(record(AUTO_FORMAT, TYPE_DWORD, &[2, 0, 0, 0]));
        let found = find_all_dwords(&img, AUTO_FORMAT);
        assert_eq!(found.len(), 3);
        let mut records: Vec<usize> = found.iter().map(|v| v.record).collect();
        records.dedup();
        assert_eq!(records.len(), 3, "each record should appear once");
        assert!(found.windows(2).all(|w| w[0].record < w[1].record), "and in order");
    }

    #[test]
    fn a_name_that_is_not_there_is_not_invented() {
        assert_eq!(find_dword(&image(), "NoSuchValue"), None);
    }
}
