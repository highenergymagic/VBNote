//! Reading and writing files in the FAT32 volume the drive holds.
//!
//! This is what makes the drive useful from the host side: files can be put
//! on it and taken off it without the machine running and without asking
//! anybody for administrator rights.
//!
//! # Only the root directory, deliberately
//!
//! The drive is a place to carry things between two machines, not a place to
//! keep them. Documents live on the flash disk. Keeping this to one flat
//! directory removes path handling, directory growth and recursion, and the
//! ways each of those can corrupt a volume -- and a transfer drive with
//! folders on it would be a filing system the user has to maintain in two
//! places.
//!
//! # The parts that go wrong quietly
//!
//! **Both allocation tables.** A volume has two and drivers may read either.
//! Updating one leaves a disk that works until something reads the other, at
//! which point files disappear that were there a moment ago.
//!
//! **The long-name checksum.** A long filename is stored in entries *before*
//! its short one, each carrying a checksum of that short name. If they
//! disagree the long name is ignored and the file appears under its 8.3
//! alias, which looks like the name was mangled rather than like a bug here.
//!
//! **Cluster 0 and 1 are not clusters.** The first real one is 2. A free
//! search that starts at zero hands out an allocation that overwrites the
//! table's own reserved entries.

use crate::fat32::{Store, SECTOR};

/// A file in the volume's root directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub size: u32,
    pub cluster: u32,
    /// Where the 8.3 entry sits, for changing it later.
    pub at: u64,
}

pub struct Volume {
    store: Store,
    part_start: u64,
    spc: u32,
    reserved: u32,
    fats: u32,
    fat_sectors: u32,
    root_cluster: u32,
    data_start: u64,
    total_clusters: u32,
}

impl Volume {
    /// Read the layout out of the volume rather than assuming the one this
    /// project writes: an image may have been formatted elsewhere.
    pub fn open(mut store: Store) -> Result<Volume, String> {
        let mbr = store.read(0, 512);
        if mbr[510] != 0x55 || mbr[511] != 0xAA {
            return Err("no partition table".into());
        }
        let part_start = u32::from_le_bytes(mbr[454..458].try_into().unwrap()) as u64;
        if part_start == 0 {
            return Err("no partition".into());
        }
        let b = store.read(part_start * SECTOR, 512);
        if b[510] != 0x55 || b[511] != 0xAA {
            return Err("no boot record".into());
        }
        let bytes_per_sector = u16::from_le_bytes([b[11], b[12]]) as u64;
        if bytes_per_sector != SECTOR {
            return Err(format!("{bytes_per_sector}-byte sectors are not supported"));
        }
        let spc = b[13] as u32;
        let reserved = u16::from_le_bytes([b[14], b[15]]) as u32;
        let fats = b[16] as u32;
        let fat_sectors = u32::from_le_bytes(b[36..40].try_into().unwrap());
        let root_cluster = u32::from_le_bytes(b[44..48].try_into().unwrap());
        if fat_sectors == 0 || spc == 0 || fats == 0 {
            return Err("this is not a FAT32 volume".into());
        }
        let total = u32::from_le_bytes(b[32..36].try_into().unwrap()) as u64;
        let data_start = reserved as u64 + fats as u64 * fat_sectors as u64;
        let total_clusters = ((total - data_start) / spc as u64) as u32;
        Ok(Volume {
            store,
            part_start,
            spc,
            reserved,
            fats,
            fat_sectors,
            root_cluster,
            data_start,
            total_clusters,
        })
    }

    pub fn into_store(self) -> Store {
        self.store
    }

    fn cluster_bytes(&self) -> usize {
        self.spc as usize * SECTOR as usize
    }

    fn cluster_at(&self, cluster: u32) -> u64 {
        (self.part_start + self.data_start + (cluster as u64 - 2) * self.spc as u64) * SECTOR
    }

    fn fat_entry(&mut self, cluster: u32) -> u32 {
        let at = (self.part_start + self.reserved as u64) * SECTOR + cluster as u64 * 4;
        let v = self.store.read(at, 4);
        u32::from_le_bytes(v.try_into().unwrap()) & 0x0FFF_FFFF
    }

    /// Write one entry into *every* table. A volume has two and a driver may
    /// read either.
    fn set_fat_entry(&mut self, cluster: u32, value: u32) {
        for n in 0..self.fats as u64 {
            let at = (self.part_start + self.reserved as u64 + n * self.fat_sectors as u64) * SECTOR
                + cluster as u64 * 4;
            self.store.write(at, &value.to_le_bytes());
        }
    }

    fn chain(&mut self, start: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = start;
        while (2..0x0FFF_FFF8).contains(&c) && out.len() < self.total_clusters as usize {
            out.push(c);
            c = self.fat_entry(c);
        }
        out
    }

    /// Somewhere to put `count` clusters. Cluster 2 is the first real one.
    fn allocate(&mut self, count: usize) -> Result<Vec<u32>, String> {
        let mut found = Vec::with_capacity(count);
        for c in 2..self.total_clusters + 2 {
            if self.fat_entry(c) == 0 {
                found.push(c);
                if found.len() == count {
                    return Ok(found);
                }
            }
        }
        Err("the drive is full".into())
    }

    /// Every 32-byte slot of the root directory, with where each one is.
    fn root_slots(&mut self) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::new();
        for cluster in self.chain(self.root_cluster) {
            let at = self.cluster_at(cluster);
            let data = self.store.read(at, self.cluster_bytes());
            for (i, slot) in data.chunks(32).enumerate() {
                out.push((at + i as u64 * 32, slot.to_vec()));
            }
        }
        out
    }

    /// What is on the drive.
    pub fn list(&mut self) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut long = String::new();
        for (at, e) in self.root_slots() {
            if e[0] == 0 {
                break;
            }
            if e[0] == 0xE5 {
                long.clear();
                continue;
            }
            let attr = e[11];
            if attr == 0x0F {
                long = long_name_part(&e) + &long;
                continue;
            }
            // Volume labels are not files, and neither is a directory.
            if attr & 0x08 != 0 || attr & 0x10 != 0 {
                long.clear();
                continue;
            }
            let name = if long.is_empty() {
                short_name_of(&e)
            } else {
                std::mem::take(&mut long)
            };
            out.push(Entry {
                name,
                size: u32::from_le_bytes(e[28..32].try_into().unwrap()),
                cluster: u32::from_le_bytes([e[26], e[27], e[20], e[21]]),
                at,
            });
            long.clear();
        }
        out
    }

    pub fn read_file(&mut self, entry: &Entry) -> Vec<u8> {
        let mut out = Vec::with_capacity(entry.size as usize);
        for c in self.chain(entry.cluster) {
            let at = self.cluster_at(c);
            out.extend_from_slice(&self.store.read(at, self.cluster_bytes()));
            if out.len() >= entry.size as usize {
                break;
            }
        }
        out.truncate(entry.size as usize);
        out
    }

    /// Put a file on the drive, replacing one of the same name.
    pub fn create(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        if let Some(existing) = self.list().into_iter().find(|e| e.name == name) {
            self.remove(&existing);
        }

        let per = self.cluster_bytes();
        let needed = data.len().div_ceil(per).max(1);
        let clusters = self.allocate(needed)?;

        for (i, c) in clusters.iter().enumerate() {
            let at = self.cluster_at(*c);
            let from = i * per;
            let to = ((i + 1) * per).min(data.len());
            let mut block = vec![0u8; per];
            if from < data.len() {
                block[..to - from].copy_from_slice(&data[from..to]);
            }
            self.store.write(at, &block);
        }
        // Chain them, then end it. Written after the data so a half-written
        // file is never reachable through the table.
        for pair in clusters.windows(2) {
            self.set_fat_entry(pair[0], pair[1]);
        }
        self.set_fat_entry(*clusters.last().unwrap(), 0x0FFF_FFFF);

        self.add_directory_entry(name, clusters[0], data.len() as u32)?;
        self.store.sync();
        Ok(())
    }

    /// Free a file's clusters and mark its directory entries deleted.
    pub fn remove(&mut self, entry: &Entry) {
        for c in self.chain(entry.cluster) {
            self.set_fat_entry(c, 0);
        }
        // The long-name entries immediately before the short one belong to
        // it, and leaving them behind attaches the old name to whatever is
        // written into that slot next.
        let mut at = entry.at;
        self.store.write(at, &[0xE5]);
        while at >= 32 {
            at -= 32;
            let slot = self.store.read(at, 32);
            if slot[11] != 0x0F {
                break;
            }
            self.store.write(at, &[0xE5]);
        }
    }

    fn add_directory_entry(&mut self, name: &str, cluster: u32, size: u32) -> Result<(), String> {
        let short = self.unique_short_name(name);
        let checksum = short_checksum(&short);
        let long = long_name_entries(name, checksum);

        let need = long.len() + 1;
        let slots = self.free_slots(need)?;

        for (slot, entry) in slots.iter().zip(long.iter()) {
            self.store.write(*slot, entry);
        }

        let mut e = vec![0u8; 32];
        e[0..11].copy_from_slice(&short);
        e[11] = 0x20; // an ordinary file
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        // A plausible date rather than zero, which some tools show as 1980
        // and some refuse: 1 January 2026.
        e[24..26].copy_from_slice(&0x5C21u16.to_le_bytes());
        e[18..20].copy_from_slice(&0x5C21u16.to_le_bytes());
        self.store.write(slots[long.len()], &e);
        Ok(())
    }

    /// `count` consecutive free slots in the root directory.
    fn free_slots(&mut self, count: usize) -> Result<Vec<u64>, String> {
        let slots = self.root_slots();
        let mut run: Vec<u64> = Vec::new();
        for (at, e) in &slots {
            if e[0] == 0 || e[0] == 0xE5 {
                run.push(*at);
                if run.len() == count {
                    return Ok(run);
                }
            } else {
                run.clear();
            }
        }
        // Out of room, so give the directory another cluster. On a volume
        // with 512-byte clusters the root holds sixteen entries, and a file
        // with a long name takes several -- so without this a drive is full
        // at about seven files, which is not a transfer drive at all.
        //
        // Safe to allocate here because `create` has already claimed and
        // chained the file's own clusters, so this cannot be handed one of
        // them.
        // A zero slot means "no more entries anywhere", so any free slots
        // left at the tail of the current clusters would hide everything in
        // the new one -- from this code and from the guest alike. Mark them
        // deleted instead, which means "nothing here, keep going".
        for (at, e) in self.root_slots() {
            if e[0] == 0 {
                self.store.write(at, &[0xE5]);
            }
        }

        let extra = self.allocate(1)?[0];
        let at = self.cluster_at(extra);
        let blank = vec![0u8; self.cluster_bytes()];
        self.store.write(at, &blank);

        let chain = self.chain(self.root_cluster);
        let last = *chain.last().ok_or("the root directory has no clusters")?;
        self.set_fat_entry(last, extra);
        self.set_fat_entry(extra, 0x0FFF_FFFF);

        let per = (self.cluster_bytes() / 32) as u64;
        if (per as usize) < count {
            return Err("that name is too long for this drive".into());
        }
        Ok((0..count as u64).map(|i| at + i * 32).collect())
    }

    /// An 8.3 name nothing else is using.
    fn unique_short_name(&mut self, name: &str) -> [u8; 11] {
        let taken: Vec<[u8; 11]> = self
            .root_slots()
            .iter()
            .filter(|(_, e)| e[0] != 0 && e[0] != 0xE5 && e[11] != 0x0F)
            .map(|(_, e)| {
                let mut s = [0u8; 11];
                s.copy_from_slice(&e[0..11]);
                s
            })
            .collect();
        for n in 1..1000 {
            let candidate = short_name(name, n);
            if !taken.contains(&candidate) {
                return candidate;
            }
        }
        short_name(name, 999)
    }
}

/// The 13 characters a long-name entry carries, in their three runs.
fn long_name_part(e: &[u8]) -> String {
    let mut units = Vec::with_capacity(13);
    for range in [1..11usize, 14..26, 28..32] {
        for pair in e[range].chunks(2) {
            let u = u16::from_le_bytes([pair[0], pair[1]]);
            if u == 0 || u == 0xFFFF {
                break;
            }
            units.push(u);
        }
    }
    String::from_utf16_lossy(&units)
}

fn short_name_of(e: &[u8]) -> String {
    let base = String::from_utf8_lossy(&e[0..8]).trim_end().to_string();
    let ext = String::from_utf8_lossy(&e[8..11]).trim_end().to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// The 8.3 alias: upper case, padded, with `~n` to tell similar names apart.
fn short_name(name: &str, n: u32) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let keep = |s: &str| -> Vec<u8> {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || "!#$%&'()-@^_`{}~".contains(*c))
            .map(|c| c.to_ascii_uppercase() as u8)
            .collect()
    };
    let stem = keep(stem);
    let ext = keep(ext);

    let tail = format!("~{n}");
    let head = 8usize.saturating_sub(tail.len()).min(stem.len());
    out[..head].copy_from_slice(&stem[..head]);
    out[head..head + tail.len()].copy_from_slice(tail.as_bytes());
    for (i, c) in ext.iter().take(3).enumerate() {
        out[8 + i] = *c;
    }
    out
}

/// The checksum every long-name entry carries, of the short name it belongs
/// to. A rotate and add, one byte at a time, and it must match or the long
/// name is ignored.
fn short_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for c in short {
        sum = sum.rotate_right(1).wrapping_add(*c);
    }
    sum
}

/// The long-name entries, in the order they go on the disk: last part first,
/// with the final one flagged, so that reading them backwards rebuilds the
/// name.
fn long_name_entries(name: &str, checksum: u8) -> Vec<Vec<u8>> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let parts = units.len().div_ceil(13).max(1);
    let mut out = Vec::with_capacity(parts);
    for part in (0..parts).rev() {
        let mut e = vec![0u8; 32];
        let sequence = (part + 1) as u8;
        e[0] = if part == parts - 1 { sequence | 0x40 } else { sequence };
        e[11] = 0x0F;
        e[13] = checksum;
        let mut at = 0usize;
        for range in [1..11usize, 14..26, 28..32] {
            for slot in e[range].chunks_mut(2) {
                let index = part * 13 + at;
                let value = match index.cmp(&units.len()) {
                    std::cmp::Ordering::Less => units[index],
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 0xFFFF,
                };
                slot.copy_from_slice(&value.to_le_bytes());
                at += 1;
            }
        }
        out.push(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fat32;

    fn drive() -> Volume {
        let mut s = Store::memory(64 * 1024 * 1024);
        fat32::format(&mut s, "VBNOTE").unwrap();
        Volume::open(s).unwrap()
    }

    #[test]
    fn a_fresh_drive_is_empty() {
        let mut v = drive();
        assert!(v.list().is_empty());
    }

    #[test]
    fn a_file_written_reads_back() {
        let mut v = drive();
        let text = b"the quick brown fox".to_vec();
        v.create("notes.txt", &text).unwrap();

        let files = v.list();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "notes.txt");
        assert_eq!(files[0].size, text.len() as u32);
        assert_eq!(v.read_file(&files[0]), text);
    }

    /// Longer than a cluster, so the chain has to be followed rather than
    /// the first cluster being mistaken for the whole file.
    #[test]
    fn a_file_longer_than_a_cluster_reads_back() {
        let mut v = drive();
        let big: Vec<u8> = (0..40_000).map(|i| (i % 251) as u8).collect();
        v.create("big.bin", &big).unwrap();
        let files = v.list();
        assert_eq!(files[0].size, 40_000);
        assert_eq!(v.read_file(&files[0]), big);
    }

    /// The name a user typed, not the 8.3 alias. If the checksum is wrong
    /// this comes back as BRAILL~1.TXT and looks like mangling.
    #[test]
    fn a_long_name_survives() {
        let mut v = drive();
        let name = "My braille notes from Tuesday.txt";
        v.create(name, b"hello").unwrap();
        assert_eq!(v.list()[0].name, name);
    }

    #[test]
    fn the_checksum_is_the_rotate_and_add_the_format_asks_for() {
        // Worked by hand from the algorithm: rotate right, add, one byte at
        // a time.
        let short = *b"NOTES   TXT";
        let mut expect: u8 = 0;
        for c in short.iter() {
            expect = expect.rotate_right(1).wrapping_add(*c);
        }
        assert_eq!(short_checksum(&short), expect);
    }

    /// Every long-name entry must carry the checksum of the short name it
    /// belongs to, or the long name is silently discarded.
    #[test]
    fn every_long_entry_carries_the_short_names_checksum() {
        let short = short_name("a rather long file name.txt", 1);
        let sum = short_checksum(&short);
        let entries = long_name_entries("a rather long file name.txt", sum);
        assert!(entries.len() > 1, "this name should need several entries");
        for e in &entries {
            assert_eq!(e[13], sum);
            assert_eq!(e[11], 0x0F);
        }
        // The first on disk is the last part, and it is the flagged one.
        assert_eq!(entries[0][0] & 0x40, 0x40);
        assert_eq!(entries.last().unwrap()[0], 1);
    }

    /// Both tables, because a driver may read either and disagreeing copies
    /// lose files that were there a moment ago.
    #[test]
    fn both_allocation_tables_are_written() {
        let mut v = drive();
        v.create("a.txt", b"x").unwrap();
        let first = v.list()[0].cluster;

        let base = (v.part_start + v.reserved as u64) * SECTOR;
        let second = base + v.fat_sectors as u64 * SECTOR;
        let a = v.store.read(base + first as u64 * 4, 4);
        let b = v.store.read(second + first as u64 * 4, 4);
        assert_eq!(a, b, "the second table was not updated");
        assert_ne!(u32::from_le_bytes(a.try_into().unwrap()), 0);
    }

    /// Cluster 2 is the first real one; 0 and 1 are the table's own.
    #[test]
    fn allocation_never_hands_out_the_reserved_clusters() {
        let mut v = drive();
        for i in 0..8 {
            v.create(&format!("f{i}.txt"), b"data").unwrap();
        }
        for f in v.list() {
            assert!(f.cluster >= 2, "{} got cluster {}", f.name, f.cluster);
        }
    }

    #[test]
    fn several_files_do_not_share_clusters() {
        let mut v = drive();
        for i in 0..6 {
            v.create(&format!("file{i}.txt"), format!("contents {i}").as_bytes()).unwrap();
        }
        let files = v.list();
        assert_eq!(files.len(), 6);
        let mut clusters: Vec<u32> = files.iter().map(|f| f.cluster).collect();
        clusters.sort_unstable();
        clusters.dedup();
        assert_eq!(clusters.len(), 6, "two files were given the same cluster");
        for (i, f) in files.iter().enumerate() {
            assert_eq!(v.read_file(f), format!("contents {i}").as_bytes());
        }
    }

    /// Writing the same name again replaces it rather than leaving two.
    #[test]
    fn writing_a_name_twice_replaces_it() {
        let mut v = drive();
        v.create("notes.txt", b"first").unwrap();
        v.create("notes.txt", b"second").unwrap();
        let files = v.list();
        assert_eq!(files.len(), 1, "the old entry was left behind");
        assert_eq!(v.read_file(&files[0]), b"second");
    }

    /// And the clusters of the replaced file come back, or a drive written
    /// to repeatedly fills up with files that are not there.
    #[test]
    fn replacing_a_file_frees_what_it_had() {
        let mut v = drive();
        let big: Vec<u8> = vec![7; 100_000];
        v.create("big.bin", &big).unwrap();
        let first = v.list()[0].cluster;
        v.create("big.bin", b"small").unwrap();

        let mut freed = 0;
        for c in first..first + 100 {
            if v.fat_entry(c) == 0 {
                freed += 1;
            }
        }
        assert!(freed > 50, "only {freed} clusters came back");
    }

    #[test]
    fn removing_a_file_takes_its_long_name_with_it() {
        let mut v = drive();
        v.create("A long name that needs several entries.txt", b"x").unwrap();
        let f = v.list()[0].clone();
        v.remove(&f);
        assert!(v.list().is_empty(), "something was left in the directory");
    }

    /// Similar long names must not collide in their 8.3 aliases.
    #[test]
    fn similar_names_get_different_aliases() {
        let mut v = drive();
        v.create("Meeting notes January.txt", b"a").unwrap();
        v.create("Meeting notes February.txt", b"b").unwrap();
        let files = v.list();
        assert_eq!(files.len(), 2);
        assert_eq!(v.read_file(&files[0]), b"a");
        assert_eq!(v.read_file(&files[1]), b"b");
    }

    /// More files than one cluster of directory can hold. At 512-byte
    /// clusters that is sixteen entries, and a long name takes several, so
    /// without a directory that grows a drive is full at about seven files.
    #[test]
    fn the_directory_grows_past_one_cluster() {
        let mut v = drive();
        for i in 0..40 {
            let name = format!("Document number {i} from the machine.txt");
            v.create(&name, format!("contents of {i}").as_bytes()).unwrap();
        }
        let files = v.list();
        assert_eq!(files.len(), 40, "files were lost as the directory grew");
        for (i, f) in files.iter().enumerate() {
            assert_eq!(f.name, format!("Document number {i} from the machine.txt"));
            assert_eq!(v.read_file(f), format!("contents of {i}").as_bytes());
        }
    }

    #[test]
    fn an_alias_is_padded_and_upper_case() {
        assert_eq!(&short_name("notes.txt", 1), b"NOTES~1 TXT");
        assert_eq!(&short_name("a.b", 1), b"A~1     B  ");
    }

    /// The volume label is not a file and must not be listed as one.
    #[test]
    fn the_volume_label_is_not_a_file() {
        let mut v = drive();
        assert!(v.list().is_empty());
        v.create("x.txt", b"x").unwrap();
        assert_eq!(v.list().len(), 1);
    }

    /// An empty file is legal and must not be given a zero-length chain.
    #[test]
    fn an_empty_file_is_written_without_a_broken_chain() {
        let mut v = drive();
        v.create("empty.txt", b"").unwrap();
        let f = &v.list()[0];
        assert_eq!(f.size, 0);
        assert!(f.cluster >= 2);
        assert_eq!(v.read_file(f), Vec::<u8>::new());
    }
}
