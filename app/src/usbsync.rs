//! Keeping a host folder and the drive in step.
//!
//! The drive is a fixed VHD, so it can be mounted on the host -- but that
//! wants administrator rights, and the emulator installs per-user precisely
//! to avoid asking for them. This is the everyday path instead: an ordinary
//! folder, in Explorer, which the user's screen reader already handles.
//!
//! # When, and why not while it is running
//!
//! **In** before the machine starts, **out** after it stops. Never while it
//! runs. Windows CE caches directory and table sectors, so a host write
//! underneath a running guest produces two writers on one filesystem and
//! neither notices -- the same reason a mounted VHD must not be left attached.
//!
//! # What decides which copy wins
//!
//! Contents, not timestamps. The dates written into a FAT directory entry
//! here are fixed, and a drive that has been round a real machine comes back
//! with whatever dates that machine felt like, so comparing them would be
//! guesswork. Comparing bytes is exact.
//!
//! The order does the rest. Out at the end of a run, in at the start of the
//! next, so a document edited on the machine reaches the folder before
//! anything is copied the other way. If a file changed on both sides while
//! the emulator was not running, the host's copy wins -- and that is the case
//! worth stating out loud rather than hiding, so it is said in the summary.

use gandalf::fatfile::Volume;

/// What a sync did, for saying out loud.
#[derive(Default, Debug, PartialEq)]
pub struct Report {
    pub copied: Vec<String>,
    pub failed: Vec<String>,
}

impl Report {
    /// Nothing copied and nothing failed. Only the tests ask, because the
    /// runner decides what to say from `spoken` instead.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.copied.is_empty() && self.failed.is_empty()
    }

    /// A sentence a person can hear, rather than a count they have to hold.
    pub fn spoken(&self, direction: &str) -> Option<String> {
        if self.copied.is_empty() {
            return None;
        }
        Some(match self.copied.len() {
            1 => format!("{} copied {}.", self.copied[0], direction),
            n => format!("{n} files copied {direction}."),
        })
    }
}

/// Files in a host folder, by name, ignoring anything that is not a file.
fn host_files(folder: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            if let Some(name) = entry.file_name().to_str() {
                // Nothing beginning with a dot: those are the host's own
                // bookkeeping and mean nothing to the machine.
                if !name.starts_with('.') {
                    out.push((name.to_string(), entry.path()));
                }
            }
        }
    }
    out
}

/// Copy anything new or changed from the folder onto the drive.
pub fn into_drive(folder: &std::path::Path, volume: &mut Volume) -> Report {
    let mut report = Report::default();
    let on_drive = volume.list();
    for (name, path) in host_files(folder) {
        let Ok(data) = std::fs::read(&path) else {
            report.failed.push(name);
            continue;
        };
        // Same name and same bytes: nothing to do, and copying anyway would
        // churn the drive on every start.
        if let Some(there) = on_drive.iter().find(|e| e.name == name) {
            if there.size as usize == data.len() && volume.read_file(there) == data {
                continue;
            }
        }
        match volume.create(&name, &data) {
            Ok(()) => report.copied.push(name),
            Err(_) => report.failed.push(name),
        }
    }
    report
}

/// Copy anything new or changed from the drive into the folder.
pub fn out_of_drive(volume: &mut Volume, folder: &std::path::Path) -> Report {
    let mut report = Report::default();
    if std::fs::create_dir_all(folder).is_err() {
        return report;
    }
    let here = host_files(folder);
    for entry in volume.list() {
        let data = volume.read_file(&entry);
        if let Some((_, path)) = here.iter().find(|(n, _)| *n == entry.name) {
            if std::fs::read(path).is_ok_and(|existing| existing == data) {
                continue;
            }
        }
        // A name from the drive is used as a filename here, so it must not be
        // able to point anywhere but inside the folder.
        if entry.name.contains(['/', '\\', ':']) || entry.name.starts_with("..") {
            report.failed.push(entry.name);
            continue;
        }
        match std::fs::write(folder.join(&entry.name), &data) {
            Ok(()) => report.copied.push(entry.name),
            Err(_) => report.failed.push(entry.name),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use gandalf::fat32::{self, Store};

    fn drive() -> Volume {
        let mut s = Store::memory(64 * 1024 * 1024);
        fat32::format(&mut s, "VBNOTE").unwrap();
        Volume::open(s).unwrap()
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vbnote-sync-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_new_host_file_reaches_the_drive() {
        let dir = folder("in");
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        let mut v = drive();

        let report = into_drive(&dir, &mut v);
        assert_eq!(report.copied, vec!["notes.txt"]);
        let files = v.list();
        assert_eq!(files.len(), 1);
        assert_eq!(v.read_file(&files[0]), b"hello");
    }

    #[test]
    fn a_file_on_the_drive_reaches_the_folder() {
        let dir = folder("out");
        let mut v = drive();
        v.create("From the machine.txt", b"typed on the machine").unwrap();

        let report = out_of_drive(&mut v, &dir);
        assert_eq!(report.copied, vec!["From the machine.txt"]);
        let got = std::fs::read(dir.join("From the machine.txt")).unwrap();
        assert_eq!(got, b"typed on the machine");
    }

    /// Running it twice must do nothing the second time, or every start
    /// rewrites the whole drive.
    #[test]
    fn syncing_twice_copies_nothing_the_second_time() {
        let dir = folder("idempotent");
        std::fs::write(dir.join("a.txt"), b"one").unwrap();
        let mut v = drive();

        assert_eq!(into_drive(&dir, &mut v).copied.len(), 1);
        assert!(into_drive(&dir, &mut v).is_empty(), "it copied again");

        assert!(out_of_drive(&mut v, &dir).is_empty(), "it copied back out");
    }

    /// A changed file is copied even though the name is already there.
    #[test]
    fn a_changed_file_is_copied_again() {
        let dir = folder("changed");
        std::fs::write(dir.join("a.txt"), b"one").unwrap();
        let mut v = drive();
        into_drive(&dir, &mut v);

        std::fs::write(dir.join("a.txt"), b"two").unwrap();
        assert_eq!(into_drive(&dir, &mut v).copied, vec!["a.txt"]);
        let files = v.list();
        assert_eq!(files.len(), 1, "a second entry was made for the same name");
        assert_eq!(v.read_file(&files[0]), b"two");
    }

    /// The round trip: out of the drive, then back in, changes nothing.
    #[test]
    fn a_round_trip_settles() {
        let dir = folder("roundtrip");
        let mut v = drive();
        v.create("Document.txt", b"written on the machine").unwrap();

        assert_eq!(out_of_drive(&mut v, &dir).copied.len(), 1);
        assert!(into_drive(&dir, &mut v).is_empty(), "it copied straight back");
    }

    /// A name from the drive is used as a host filename, so it must not be
    /// able to point outside the folder.
    #[test]
    fn a_name_that_escapes_the_folder_is_refused() {
        let dir = folder("escape");
        let mut v = drive();
        v.create("..\\escaped.txt", b"nope").unwrap();

        let report = out_of_drive(&mut v, &dir);
        assert!(report.copied.is_empty(), "it wrote outside the folder");
        assert_eq!(report.failed.len(), 1);
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
    }

    #[test]
    fn the_summary_reads_as_a_sentence() {
        let one = Report {
            copied: vec!["notes.txt".into()],
            failed: vec![],
        };
        assert_eq!(one.spoken("to the drive").unwrap(), "notes.txt copied to the drive.");
        let many = Report {
            copied: vec!["a".into(), "b".into(), "c".into()],
            failed: vec![],
        };
        assert_eq!(many.spoken("to the drive").unwrap(), "3 files copied to the drive.");
        assert!(Report::default().spoken("anywhere").is_none());
    }
}
