//! Keeping the card image on disk up to date while the machine runs.
//!
//! The card is the user's data: the documents they have typed. It used to be
//! written exactly once, when the emulator finished tidily, which meant any
//! other ending -- a Ctrl-C, a crash, a machine switched off at the wall --
//! lost everything written since the run started. That is the worst kind of
//! bug this project can have, because the machine cheerfully says the file is
//! saved and it is, on an emulated card that never reaches the host.
//!
//! Writing the whole image on a timer is not the fix. It is well over a
//! hundred megabytes and almost none of it changes, so it would cost a
//! multi-second stall every time. Instead the card model remembers which
//! blocks were written, and this writes back only those, as runs of
//! consecutive blocks. A few seconds of typing is a handful of kilobytes.
//!
//! What is left unprotected is the few seconds since the last flush. That is a
//! choice: flushing on every block would turn each of the guest's writes into
//! a host write, and the guest writes a block at a time.

use pxa270::sdcard::SdCard;
use std::io::{Seek, SeekFrom, Write};

pub struct CardFile {
    file: std::fs::File,
    path: String,
    /// Blocks written back over the life of the run, for the closing report.
    pub blocks_written: u64,
    pub flushes: u64,
}

impl CardFile {
    /// Open the image for writing, creating it at the card's size if it is not
    /// there yet.
    pub fn open(path: &str, card: &SdCard) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // A card made from nothing has no file behind it yet, and a short file
        // would make every seek past its end silently extend it with zeros in
        // the wrong places.
        let want = (card.blocks() * card.block_size()) as u64;
        if file.metadata()?.len() < want {
            file.set_len(want)?;
        }
        Ok(CardFile { file, path: path.to_string(), blocks_written: 0, flushes: 0 })
    }

    /// Write back everything the card has changed since this was last called.
    ///
    /// Returns how many blocks went out, which is zero when the guest has been
    /// idle -- the common case, and the one that has to be cheap.
    pub fn flush(&mut self, card: &mut SdCard) -> std::io::Result<usize> {
        let runs = card.take_dirty_runs();
        if runs.is_empty() {
            return Ok(0);
        }
        let block = card.block_size() as u64;
        let mut blocks = 0;
        for (first, count) in runs {
            let bytes = card.block_bytes(first, count);
            if bytes.is_empty() {
                continue;
            }
            self.file.seek(SeekFrom::Start(first as u64 * block))?;
            self.file.write_all(bytes)?;
            blocks += count;
        }
        // Ask the operating system to put it on the medium rather than in its
        // own cache, because the ending this protects against is the one where
        // the process does not get to tidy up.
        self.file.sync_data()?;
        self.blocks_written += blocks as u64;
        self.flushes += 1;
        Ok(blocks)
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("vbnote-card-test-{name}-{}.img", std::process::id()));
        p.to_string_lossy().into_owned()
    }

    /// The whole point: what the guest wrote is on the host's disk without
    /// the emulator having exited.
    #[test]
    fn a_write_reaches_the_file_without_shutting_down() {
        let path = scratch("basic");
        let _ = std::fs::remove_file(&path);
        let mut card = SdCard::new(64 * 1024);
        let mut f = CardFile::open(&path, &card).unwrap();

        card.command(24, 0);
        for b in b"hello" {
            card.write_byte(*b);
        }
        assert_eq!(f.flush(&mut card).unwrap(), 1);

        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(&on_disk[..5], b"hello");
        std::fs::remove_file(&path).unwrap();
    }

    /// An idle machine must cost nothing. This runs every couple of seconds
    /// for the whole life of a session.
    #[test]
    fn an_idle_card_writes_nothing() {
        let path = scratch("idle");
        let _ = std::fs::remove_file(&path);
        let card_ro = SdCard::new(64 * 1024);
        let mut f = CardFile::open(&path, &card_ro).unwrap();
        let mut card = SdCard::new(64 * 1024);
        assert_eq!(f.flush(&mut card).unwrap(), 0);
        assert_eq!(f.flushes, 0, "an empty flush is not a flush");
        std::fs::remove_file(&path).unwrap();
    }

    /// Writes land where they are addressed, not where they happen to fall in
    /// order. Getting this wrong corrupts a card rather than failing to save
    /// one, which is worse than the bug it fixes.
    #[test]
    fn blocks_are_written_at_their_own_offsets() {
        let path = scratch("offsets");
        let _ = std::fs::remove_file(&path);
        let mut card = SdCard::new(64 * 1024);
        let block = card.block_size();
        let mut f = CardFile::open(&path, &card).unwrap();

        card.command(24, 0);
        card.position = block * 3;
        card.write_byte(0x5A);
        card.position = block * 7;
        card.write_byte(0xA5);
        f.flush(&mut card).unwrap();

        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(on_disk[block * 3], 0x5A);
        assert_eq!(on_disk[block * 7], 0xA5);
        assert_eq!(on_disk[block * 5], 0x00, "and nothing in between");
        std::fs::remove_file(&path).unwrap();
    }

    /// A card with no file behind it gets one of the right size, or later
    /// seeks land in a file that is too short.
    #[test]
    fn a_new_image_is_made_the_size_of_the_card() {
        let path = scratch("size");
        let _ = std::fs::remove_file(&path);
        let card = SdCard::new(64 * 1024);
        let _ = CardFile::open(&path, &card).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, (card.blocks() * card.block_size()) as u64);
        std::fs::remove_file(&path).unwrap();
    }
}
