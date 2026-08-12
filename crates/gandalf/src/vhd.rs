//! Making the drive a file Windows can open by itself.
//!
//! A **fixed** VHD is not a container format. It is the raw disk, byte for
//! byte, with a 512-byte footer stuck on the end -- so the image the emulator
//! already writes *is* a VHD as soon as the footer is there, and the guest
//! goes on seeing an ordinary disk because the footer sits past the last
//! sector it knows about.
//!
//! That is worth 512 bytes because of what it buys: the user can mount the
//! drive on their own computer and use Explorer, which is a file manager they
//! already know and their screen reader already handles, instead of anything
//! this project would have to invent.
//!
//! **It needs administrator rights**, which is why it is not the only answer.
//! Attaching a virtual disk is a driver operation and Windows will ask. The
//! emulator installs per-user precisely to avoid that prompt, so mounting is
//! the deliberate path for somebody who wants it and not the everyday one.
//!
//! Nothing may be mounted while the machine is running. Two writers on one
//! filesystem corrupt it, and neither would notice.

use crate::fat32::{Store, SECTOR};

/// Every field is big-endian, which is the thing to remember here: the disk
/// it describes is little-endian throughout and the footer is not.
pub const FOOTER_BYTES: u64 = 512;

/// Write the footer that makes an image a fixed VHD.
///
/// `store`'s length is the disk; the footer goes immediately after it, which
/// is why the file is created 512 bytes longer than the disk it holds.
pub fn write_footer(store: &mut Store) -> Result<(), String> {
    let disk = store.len();
    let footer = footer(disk);
    store.write_past_end(disk, &footer)?;
    store.sync();
    Ok(())
}

fn footer(disk_bytes: u64) -> Vec<u8> {
    let mut f = vec![0u8; FOOTER_BYTES as usize];
    f[0..8].copy_from_slice(b"conectix");
    // Features: bit 1 is reserved and must be set.
    f[8..12].copy_from_slice(&2u32.to_be_bytes());
    f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // A fixed disk has no header elsewhere, and says so with all ones.
    f[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    // Seconds since the start of 2000, not 1970. A wrong timestamp is
    // harmless, so this does not reach for a clock it may not have.
    f[24..28].copy_from_slice(&0u32.to_be_bytes());
    f[28..32].copy_from_slice(b"vbno");
    f[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // Created on Windows.
    f[36..40].copy_from_slice(b"Wi2k");
    f[40..48].copy_from_slice(&disk_bytes.to_be_bytes());
    f[48..56].copy_from_slice(&disk_bytes.to_be_bytes());

    let (cylinders, heads, sectors) = geometry(disk_bytes / SECTOR);
    f[56..58].copy_from_slice(&cylinders.to_be_bytes());
    f[58] = heads;
    f[59] = sectors;
    // 2 is a fixed disk: the data is all there, in order, with no map.
    f[60..64].copy_from_slice(&2u32.to_be_bytes());

    // A stable identity derived from the size, rather than a random one that
    // would change every time the drive was made.
    let mut id = [0u8; 16];
    id[0..8].copy_from_slice(b"VBNoteHD");
    id[8..16].copy_from_slice(&disk_bytes.to_be_bytes());
    f[68..84].copy_from_slice(&id);

    // The checksum covers the whole footer with its own field zeroed, and is
    // the ones' complement of the sum. Windows refuses a footer whose
    // checksum disagrees, so this is not decoration.
    let sum: u32 = f.iter().map(|b| *b as u32).sum();
    f[64..68].copy_from_slice(&(!sum).to_be_bytes());
    f
}

/// The cylinder/head/sector numbers the VHD specification asks for.
///
/// Nothing reads a real geometry off a disk any more, but the format insists
/// on one and it has a defined way of inventing it. This is that way.
pub fn geometry(total_sectors: u64) -> (u16, u8, u8) {
    // The largest the format can describe.
    let total = total_sectors.min(65535 * 16 * 255);
    let (mut sectors_per_track, mut heads, mut cylinder_times_heads);

    if total >= 65535 * 16 * 63 {
        sectors_per_track = 255;
        heads = 16;
        cylinder_times_heads = total / sectors_per_track;
    } else {
        sectors_per_track = 17;
        cylinder_times_heads = total / sectors_per_track;
        heads = cylinder_times_heads.div_ceil(1024);
        if heads < 4 {
            heads = 4;
        }
        if cylinder_times_heads >= heads * 1024 || heads > 16 {
            sectors_per_track = 31;
            heads = 16;
            cylinder_times_heads = total / sectors_per_track;
        }
        if cylinder_times_heads >= heads * 1024 {
            sectors_per_track = 63;
            heads = 16;
            cylinder_times_heads = total / sectors_per_track;
        }
    }
    let cylinders = cylinder_times_heads / heads;
    (cylinders as u16, heads as u8, sectors_per_track as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer_of(megabytes: u64) -> Vec<u8> {
        footer(megabytes * 1024 * 1024)
    }

    #[test]
    fn it_is_recognisably_a_fixed_vhd() {
        let f = footer_of(64);
        assert_eq!(&f[0..8], b"conectix");
        assert_eq!(u32::from_be_bytes(f[60..64].try_into().unwrap()), 2);
        // A fixed disk points nowhere for its header.
        assert_eq!(u64::from_be_bytes(f[16..24].try_into().unwrap()), u64::MAX);
        assert_eq!(f.len() as u64, FOOTER_BYTES);
    }

    /// Windows rejects a footer whose checksum disagrees, so this is the
    /// difference between a disk that mounts and one that does not.
    #[test]
    fn the_checksum_is_what_the_format_asks_for() {
        let f = footer_of(64);
        let stated = u32::from_be_bytes(f[64..68].try_into().unwrap());
        let mut zeroed = f.clone();
        zeroed[64..68].fill(0);
        let sum: u32 = zeroed.iter().map(|b| *b as u32).sum();
        assert_eq!(stated, !sum);
    }

    /// The size is recorded twice and both are the disk, not the file. A
    /// footer that counted itself would describe a disk 512 bytes too long.
    #[test]
    fn the_size_is_the_disk_not_the_file() {
        let bytes = 64 * 1024 * 1024;
        let f = footer(bytes);
        assert_eq!(u64::from_be_bytes(f[40..48].try_into().unwrap()), bytes);
        assert_eq!(u64::from_be_bytes(f[48..56].try_into().unwrap()), bytes);
    }

    /// Every field is big-endian. Reading one back little-endian is the
    /// mistake this catches: 64 MB would come out as an absurd number.
    #[test]
    fn the_footer_is_big_endian_throughout() {
        let f = footer_of(64);
        let wrong = u64::from_le_bytes(f[40..48].try_into().unwrap());
        assert_ne!(wrong, 64 * 1024 * 1024);
    }

    /// The geometry has to multiply out to no more than the disk, or a tool
    /// believes in sectors that are not there.
    #[test]
    fn the_geometry_fits_inside_the_disk() {
        for mb in [64u64, 512, 2048, 8192, 32 * 1024] {
            let sectors = mb * 1024 * 1024 / SECTOR;
            let (c, h, s) = geometry(sectors);
            let described = c as u64 * h as u64 * s as u64;
            assert!(
                described <= sectors,
                "{mb} MB: geometry describes {described} of {sectors} sectors"
            );
            assert!(c > 0 && h > 0 && s > 0, "{mb} MB gave a zero dimension");
        }
    }

    /// The largest disk this project will make still has a describable
    /// geometry, which is not obvious: the format runs out at 127 GB.
    #[test]
    fn the_largest_disk_still_has_a_geometry() {
        let sectors = 32u64 * 1024 * 1024 * 1024 / SECTOR;
        let (c, h, s) = geometry(sectors);
        assert!(c > 0 && h == 16 && s > 0);
    }
}
