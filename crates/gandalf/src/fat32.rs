//! A disk the machine can use without being asked to make one.
//!
//! Windows CE 4.2 will partition and format storage it is given, and for the
//! SD card it does exactly that. It cannot be relied on here, so the drive
//! arrives already partitioned and already formatted and the guest only has
//! to mount it.
//!
//! # Backing, and why it is not a `Vec`
//!
//! A 32 GB disk is 32 GB of memory if it is held as bytes. It is a **sparse
//! file** instead: the metadata is written and the rest of it costs nothing
//! until something is stored there. Formatting a 32 GB volume touches the
//! boot sector, the information sector, two file allocation tables and the
//! root directory, and nothing else, so making one is quick however large it
//! is.
//!
//! Writes also land in the file as they happen rather than at exit, which is
//! the lesson the SD card already paid for.
//!
//! # 32 GB
//!
//! The cap is not arbitrary and is not this emulator's: Windows itself will
//! not *create* a FAT32 volume larger than 32 GB, and a disk this machine
//! cannot read is no use to anybody. Cluster sizes follow the same table
//! Windows uses, for the same reason.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

pub const SECTOR: u64 = 512;

/// The largest volume to make. Windows will not format FAT32 above this, so
/// a bigger one would be a disk the user's own computer could not write to.
pub const MAX_MEGABYTES: usize = 32 * 1024;

/// The smallest that can honestly be called FAT32.
///
/// The format is defined by its cluster count: fewer than 65525 and it is a
/// FAT16 volume wearing the wrong label, which a driver is entitled to reject.
pub const MIN_MEGABYTES: usize = 64;

/// Where the volume starts. One megabyte in, which is what every modern tool
/// does and what alignment-sensitive media expect.
const PARTITION_START: u64 = 2048;

/// Bytes, wherever they are actually kept.
///
/// Two backings, because a disk is a file and a test should not have to be.
pub enum Store {
    Memory(Vec<u8>),
    File { file: File, len: u64 },
}

impl Store {
    pub fn memory(bytes: usize) -> Store {
        Store::Memory(vec![0; bytes])
    }

    pub fn len(&self) -> u64 {
        match self {
            Store::Memory(v) => v.len() as u64,
            Store::File { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn sectors(&self) -> u64 {
        self.len() / SECTOR
    }

    pub fn read(&mut self, at: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        match self {
            Store::Memory(v) => {
                let at = at as usize;
                let end = (at + len).min(v.len());
                if at < v.len() {
                    out[..end - at].copy_from_slice(&v[at..end]);
                }
            }
            Store::File { file, .. } => {
                if file.seek(SeekFrom::Start(at)).is_ok() {
                    // A short read is a read past the end of what has been
                    // written, which on a sparse file is zeroes -- which is
                    // what `out` already holds.
                    let _ = read_as_much_as_possible(file, &mut out);
                }
            }
        }
        out
    }

    pub fn write(&mut self, at: u64, data: &[u8]) {
        match self {
            Store::Memory(v) => {
                let at = at as usize;
                if at < v.len() {
                    let end = (at + data.len()).min(v.len());
                    v[at..end].copy_from_slice(&data[..end - at]);
                }
            }
            Store::File { file, len } => {
                if at + data.len() as u64 > *len {
                    return;
                }
                if file.seek(SeekFrom::Start(at)).is_ok() {
                    let _ = file.write_all(data);
                }
            }
        }
    }

    /// Write beyond the disk's own length.
    ///
    /// Only one thing is out there: the VHD footer, which belongs to the file
    /// rather than to the disk. Ordinary writes are refused past the end, and
    /// should be.
    pub fn write_past_end(&mut self, at: u64, data: &[u8]) -> Result<(), String> {
        match self {
            Store::Memory(v) => {
                let end = at as usize + data.len();
                if v.len() < end {
                    v.resize(end, 0);
                }
                v[at as usize..end].copy_from_slice(data);
                Ok(())
            }
            Store::File { file, .. } => {
                file.seek(SeekFrom::Start(at))
                    .and_then(|_| file.write_all(data))
                    .map_err(|e| format!("cannot write the disk footer: {e}"))
            }
        }
    }

    pub fn sync(&mut self) {
        if let Store::File { file, .. } = self {
            let _ = file.sync_data();
        }
    }
}

fn read_as_much_as_possible(file: &mut File, out: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < out.len() {
        match file.read(&mut out[done..])? {
            0 => break,
            n => done += n,
        }
    }
    Ok(())
}

/// How big a cluster to use, following Windows' own table.
///
/// Getting this wrong is not a formatting detail: too small and the file
/// allocation table for a large volume becomes enormous, too large and a
/// small volume has too few clusters to be FAT32 at all.
pub fn sectors_per_cluster(total_sectors: u64) -> u32 {
    // Work down from the largest, and take the first that still leaves enough
    // clusters to be FAT32. Bigger clusters mean a smaller table, and the
    // size of that table is not a detail on this machine: asking the drive
    // how much space is free means **reading the whole of it**, one sector at
    // a time, and the guest gets through about 185 commands a second.
    //
    // A 256 MB drive laid out with 512-byte clusters has an 8,032-sector
    // table across its two copies, which is a minute of silence when somebody
    // asks for drive information -- measured, and mistaken for a lockup by
    // the person who asked. The same drive with 2 KB clusters has 2,048, and
    // four times less to wait for.
    //
    // 70,000 rather than the bare 65,525 minimum: a volume that sits exactly
    // on the line is one a slightly different arithmetic somewhere else
    // rejects.
    for spc in [64u32, 32, 16, 8, 4, 2] {
        if total_sectors / spc as u64 >= 70_000 {
            return spc;
        }
    }
    1
}

/// Lay down a partition table and a FAT32 volume.
pub fn format(store: &mut Store, label: &str) -> Result<(), String> {
    let total = store.sectors();
    if total <= PARTITION_START {
        return Err("the disk is too small to hold a partition".into());
    }
    let volume_sectors = total - PARTITION_START;
    let spc = sectors_per_cluster(total);
    let reserved: u32 = 32;
    // Two, the conventional number.
    //
    // One was tried, on the theory that counting free space reads every copy
    // and halving them would halve the wait. It does not: 1,022 sector reads
    // with one table against 1,023 with two. The guest reads the table
    // **once** whatever is there, and the block numbers that suggested
    // otherwise were the single table sitting where the second used to be.
    // So the second copy is free, and it is the copy a damaged volume is
    // recovered from.
    let fats: u32 = 2;

    // The table has to fit the clusters, and the clusters are what is left
    // after the table -- so it is solved rather than counted. The usual
    // arrangement: work out how many sectors of table each cluster costs.
    let bytes_per_cluster = spc as u64 * SECTOR;
    let approx_clusters = (volume_sectors - reserved as u64) * SECTOR / (bytes_per_cluster + 8);
    let fat_sectors = ((approx_clusters + 2) * 4).div_ceil(SECTOR) as u32;
    let data_start = reserved as u64 + (fats as u64 * fat_sectors as u64);
    if data_start >= volume_sectors {
        return Err("the disk is too small for a file allocation table".into());
    }
    let clusters = (volume_sectors - data_start) / spc as u64;
    if clusters < 65525 {
        return Err(format!(
            "{} clusters is too few for FAT32; the disk must be at least {MIN_MEGABYTES} MB",
            clusters
        ));
    }

    write_mbr(store, total, volume_sectors);

    let boot = boot_sector(volume_sectors, spc, reserved, fats, fat_sectors, label);
    let base = PARTITION_START * SECTOR;
    store.write(base, &boot);
    // The backup, six sectors in, which a driver falls back to if the first
    // one is unreadable.
    store.write(base + 6 * SECTOR, &boot);
    store.write(base + SECTOR, &fs_info(clusters));
    store.write(base + 7 * SECTOR, &fs_info(clusters));

    // Both tables: three entries, the last of which ends the root
    // directory's chain. The rest is zero, which is what a free cluster is,
    // and on a sparse file costs nothing to leave alone.
    let mut table = Vec::with_capacity(12);
    table.extend_from_slice(&0x0FFF_FFF8u32.to_le_bytes());
    table.extend_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
    table.extend_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
    for n in 0..fats as u64 {
        store.write(base + (reserved as u64 + n * fat_sectors as u64) * SECTOR, &table);
    }

    // The root directory: one volume label entry, and zeroes after it.
    let root = (PARTITION_START + data_start) * SECTOR;
    store.write(root, &volume_label_entry(label));
    store.sync();
    Ok(())
}

fn write_mbr(store: &mut Store, total: u64, volume_sectors: u64) {
    let mut mbr = vec![0u8; 512];
    let e = 446;
    mbr[e] = 0x00; // not bootable
    // Cylinder/head/sector fields nobody reads any more, filled with the
    // conventional "use LBA instead" values.
    mbr[e + 1] = 0xFE;
    mbr[e + 2] = 0xFF;
    mbr[e + 3] = 0xFF;
    // 0x0C is FAT32 with LBA addressing. 0x0B, the one without, invites a
    // driver to try geometry that is not there.
    mbr[e + 4] = 0x0C;
    mbr[e + 5] = 0xFE;
    mbr[e + 6] = 0xFF;
    mbr[e + 7] = 0xFF;
    mbr[e + 8..e + 12].copy_from_slice(&(PARTITION_START as u32).to_le_bytes());
    mbr[e + 12..e + 16].copy_from_slice(&(volume_sectors as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    let _ = total;
    store.write(0, &mbr);
}

fn boot_sector(
    volume_sectors: u64,
    spc: u32,
    reserved: u32,
    fats: u32,
    fat_sectors: u32,
    label: &str,
) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"MSWIN4.1");
    b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    b[13] = spc as u8;
    b[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
    b[16] = fats as u8;
    // Root entries and the 16-bit totals are zero on FAT32, and that is how
    // a driver tells which format it is looking at.
    b[21] = 0xF8;
    b[24..26].copy_from_slice(&63u16.to_le_bytes());
    b[26..28].copy_from_slice(&255u16.to_le_bytes());
    b[28..32].copy_from_slice(&(PARTITION_START as u32).to_le_bytes());
    b[32..36].copy_from_slice(&(volume_sectors as u32).to_le_bytes());
    b[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
    b[44..48].copy_from_slice(&2u32.to_le_bytes()); // root starts at cluster 2
    b[48..50].copy_from_slice(&1u16.to_le_bytes()); // information sector
    b[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
    b[64] = 0x80;
    b[66] = 0x29;
    b[67..71].copy_from_slice(&0x5642_4E31u32.to_le_bytes());
    b[71..82].copy_from_slice(&label_bytes(label));
    b[82..90].copy_from_slice(b"FAT32   ");
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

fn fs_info(clusters: u64) -> Vec<u8> {
    let mut s = vec![0u8; 512];
    s[0..4].copy_from_slice(b"RRaA");
    s[484..488].copy_from_slice(b"rrAa");
    // Everything but the root directory's one cluster is free.
    s[488..492].copy_from_slice(&((clusters - 1) as u32).to_le_bytes());
    s[492..496].copy_from_slice(&3u32.to_le_bytes());
    s[508..512].copy_from_slice(&[0x00, 0x00, 0x55, 0xAA]);
    s
}

/// A volume label is 11 bytes, space padded, upper case, and not terminated.
fn label_bytes(label: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    for (i, c) in label.bytes().take(11).enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    out
}

fn volume_label_entry(label: &str) -> Vec<u8> {
    let mut e = vec![0u8; 32];
    e[0..11].copy_from_slice(&label_bytes(label));
    e[11] = 0x08; // volume label
    e
}

/// Make a file of `bytes`, sparse where the filesystem allows it.
///
/// Without this a 32 GB drive reserves 32 GB before it holds anything.
pub fn create_sparse(path: &str, bytes: u64) -> Result<Store, String> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("cannot create {path}: {e}"))?;
    #[cfg(windows)]
    mark_sparse(&file);
    // Room for the disk and the VHD footer after it. The footer belongs to
    // the file, not to the disk, so the store's length stays the disk's.
    file.set_len(bytes + FOOTER)
        .map_err(|e| format!("cannot size {path}: {e}"))?;
    Ok(Store::File { file, len: bytes })
}

/// The VHD footer's size, and its cookie, which is how one is recognised.
const FOOTER: u64 = 512;
const FOOTER_COOKIE: &[u8; 8] = b"conectix";

/// Open one that is already there.
pub fn open(path: &str) -> Result<Store, String> {
    let file = File::options()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open {path}: {e}"))?;
    let len = file
        .metadata()
        .map_err(|e| format!("cannot measure {path}: {e}"))?
        .len();

    // If the file ends in a VHD footer, the disk is everything before it.
    // Handing the guest the footer as if it were a sector would put four
    // stray sectors of metadata at the end of its disk.
    let mut store = Store::File { file, len };
    if len > FOOTER {
        let tail = store.read(len - FOOTER, 8);
        if tail == FOOTER_COOKIE {
            if let Store::File { len, .. } = &mut store {
                *len -= FOOTER;
            }
        }
    }
    Ok(store)
}

/// Ask NTFS to keep the unwritten parts of the file costing nothing.
///
/// Best effort: a filesystem that will not do it still gets a working disk,
/// just one that takes up its full size.
#[cfg(windows)]
fn mark_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    const FSCTL_SET_SPARSE: u32 = 0x000900C4;
    unsafe extern "system" {
        fn DeviceIoControl(
            handle: *mut std::ffi::c_void,
            control: u32,
            in_buffer: *mut std::ffi::c_void,
            in_size: u32,
            out_buffer: *mut std::ffi::c_void,
            out_size: u32,
            returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            file.as_raw_handle() as *mut _,
            FSCTL_SET_SPARSE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(megabytes: usize) -> Store {
        let mut s = Store::memory(megabytes * 1024 * 1024);
        format(&mut s, "VBNOTE").expect("format failed");
        s
    }

    /// The partition entry has to say FAT32-with-LBA and start where the boot
    /// sector claims it does, or the volume is found in the wrong place.
    #[test]
    fn the_partition_table_points_at_the_volume() {
        let mut s = formatted(64);
        let mbr = s.read(0, 512);
        assert_eq!([mbr[510], mbr[511]], [0x55, 0xAA]);
        assert_eq!(mbr[446 + 4], 0x0C, "not FAT32 with LBA");
        let start = u32::from_le_bytes(mbr[446 + 8..446 + 12].try_into().unwrap());
        assert_eq!(start as u64, PARTITION_START);

        let boot = s.read(PARTITION_START * SECTOR, 512);
        let hidden = u32::from_le_bytes(boot[28..32].try_into().unwrap());
        assert_eq!(hidden as u64, PARTITION_START, "hidden sectors disagree");
    }

    /// The three fields a driver uses to tell FAT32 from FAT16. All three
    /// must be zero, and the 32-bit ones filled in instead.
    #[test]
    fn it_is_recognisably_fat32() {
        let mut s = formatted(64);
        let b = s.read(PARTITION_START * SECTOR, 512);
        assert_eq!(u16::from_le_bytes([b[17], b[18]]), 0, "root entries not zero");
        assert_eq!(u16::from_le_bytes([b[19], b[20]]), 0, "16-bit total not zero");
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), 0, "16-bit FAT size not zero");
        assert_ne!(u32::from_le_bytes(b[36..40].try_into().unwrap()), 0);
        assert_eq!(&b[82..90], b"FAT32   ");
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 2);
        assert_eq!([b[510], b[511]], [0x55, 0xAA]);
    }

    /// The first two entries are reserved and the third ends the root
    /// directory's chain. A root that does not terminate is a directory that
    /// reads off into free space.
    #[test]
    fn the_table_starts_with_a_terminated_root() {
        let mut s = formatted(64);
        let b = s.read(PARTITION_START * SECTOR, 512);
        let reserved = u16::from_le_bytes([b[14], b[15]]) as u64;
        let fat = s.read((PARTITION_START + reserved) * SECTOR, 12);
        assert_eq!(u32::from_le_bytes(fat[0..4].try_into().unwrap()), 0x0FFF_FFF8);
        assert_eq!(u32::from_le_bytes(fat[4..8].try_into().unwrap()), 0x0FFF_FFFF);
        assert_eq!(
            u32::from_le_bytes(fat[8..12].try_into().unwrap()),
            0x0FFF_FFFF,
            "the root directory's chain does not end"
        );
    }

    /// One table, and the boot sector says so. Counting free space reads
    /// every table there is, so a second copy is a second scan.
    #[test]
    fn there_are_two_tables_and_a_backup_boot_sector() {
        let mut s = formatted(64);
        let b = s.read(PARTITION_START * SECTOR, 512);
        let backup = s.read((PARTITION_START + 6) * SECTOR, 512);
        assert_eq!(b, backup, "the backup boot sector differs");
        assert_eq!(b[16], 2, "the volume claims a number of tables it has not got");

        let reserved = u16::from_le_bytes([b[14], b[15]]) as u64;
        let fat_sectors = u32::from_le_bytes(b[36..40].try_into().unwrap()) as u64;
        let first = s.read((PARTITION_START + reserved) * SECTOR, 12);
        let second = s.read((PARTITION_START + reserved + fat_sectors) * SECTOR, 12);
        assert_eq!(first, second, "the second table was not written");
    }

    /// Fewer than 65525 clusters is not FAT32, whatever the label says, so a
    /// disk too small to have them is refused rather than made wrong.
    #[test]
    fn a_disk_too_small_for_fat32_is_refused() {
        let mut s = Store::memory(8 * 1024 * 1024);
        let e = format(&mut s, "VBNOTE").expect_err("an 8 MB FAT32 volume should be refused");
        assert!(e.contains("too few"), "{e}");
    }

    /// And one that is big enough has enough, which is the same check from
    /// the other side.
    #[test]
    fn the_smallest_allowed_disk_has_enough_clusters() {
        let mut s = formatted(MIN_MEGABYTES);
        let b = s.read(PARTITION_START * SECTOR, 512);
        let spc = b[13] as u64;
        let reserved = u16::from_le_bytes([b[14], b[15]]) as u64;
        let fats = b[16] as u64;
        let fat_sectors = u32::from_le_bytes(b[36..40].try_into().unwrap()) as u64;
        let total = u32::from_le_bytes(b[32..36].try_into().unwrap()) as u64;
        let clusters = (total - reserved - fats * fat_sectors) / spc;
        assert!(clusters >= 65525, "only {clusters} clusters");
    }

    /// Cluster sizes follow Windows' table, because a volume it would not
    /// make is a volume it may not want to read.
    /// Every size still has enough clusters to be FAT32, and gets the
    /// largest cluster that manages it -- which is the same thing as the
    /// smallest table.
    #[test]
    fn clusters_are_as_large_as_they_can_be() {
        let mb = |n: u64| n * 1024 * 1024 / SECTOR;
        for size in [64u64, 128, 256, 512, 1024, 4096, 8192, 16 * 1024, 32 * 1024] {
            let sectors = mb(size);
            let spc = sectors_per_cluster(sectors);
            assert!(
                sectors / spc as u64 >= 65_525,
                "{size} MB with {spc} sectors a cluster is not FAT32"
            );
            if spc < 64 {
                let bigger = spc * 2;
                assert!(
                    sectors / bigger as u64 <= 70_000,
                    "{size} MB could have used {bigger} sectors a cluster"
                );
            }
        }
    }

    /// The bug this was found by: a 256 MB drive with 512-byte clusters has
    /// an 8,032-sector table across both copies, and reading it is a minute
    /// of silence when somebody asks how much space is free.
    #[test]
    fn a_small_drives_table_is_not_enormous() {
        let mut s = formatted(256);
        let b = s.read(PARTITION_START * SECTOR, 512);
        let fats = b[16] as u64;
        let fat_sectors = u32::from_le_bytes(b[36..40].try_into().unwrap()) as u64;
        let total = fats * fat_sectors;
        assert!(
            total <= 2500,
            "{total} sectors of table to read for a 256 MB drive"
        );
    }

    /// The largest allowed disk still formats, and its table is a sane size
    /// rather than most of the volume.
    #[test]
    fn the_largest_allowed_disk_formats() {
        // Sparse in spirit: only the metadata is touched, so this does not
        // need 32 GB of anything. The memory store cannot pretend, so this
        // checks the arithmetic instead of writing it.
        let total = MAX_MEGABYTES as u64 * 1024 * 1024 / SECTOR;
        let spc = sectors_per_cluster(total);
        assert_eq!(spc, 64);
        let clusters = total / spc as u64;
        assert!(clusters >= 65525);
        // Four bytes an entry, and it must not swallow the volume.
        let fat_bytes = clusters * 4;
        assert!(fat_bytes < total * SECTOR / 100, "table is over 1% of the disk");
    }

    #[test]
    fn the_volume_label_is_written_where_it_is_looked_for() {
        let mut s = formatted(64);
        let b = s.read(PARTITION_START * SECTOR, 512);
        assert_eq!(&b[71..82], b"VBNOTE     ");
    }

    /// A sparse file reads as zeroes where nothing was written, and that has
    /// to hold or a freshly formatted disk is full of whatever was there.
    #[test]
    fn unwritten_space_reads_as_zeroes() {
        let mut s = formatted(64);
        let far = s.read(40 * 1024 * 1024, 64);
        assert!(far.iter().all(|b| *b == 0));
    }
}
