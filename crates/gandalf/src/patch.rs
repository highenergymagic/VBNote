//! Code patches applied to the ROM image as it is built.
//!
//! The registry is already rewritten on the way past — `AutoFormat` and
//! `AutoPart` are turned on so a blank medium gets prepared — and this is the
//! same intervention one level down: a few bytes of guest code, changed in
//! the image being built, never in `NK.bin` on disk.
//!
//! A patch is not a fix and is not pretending to be one. Each carries what it
//! bypasses and what would make it unnecessary, so that when the real cause
//! turns up the patch can be deleted rather than archaeologised.
//!
//! Patches themselves are not here: they are `.nkp` files under `patches/`,
//! parsed by [`crate::nkp`]. This is the machinery that applies one. The files
//! are embedded in the binary so a release needs nothing beside it, and
//! `--patches DIR` reads a directory instead.
//!
//! # Finding the site
//!
//! Patches match a **byte signature** rather than an address, and refuse to
//! apply unless the signature appears exactly once in the whole image. An
//! address would be quietly wrong against a different build; a signature that
//! matches twice means the assumption about which code is being changed is
//! already broken, and applying to the first hit would be a guess.

/// One patch: a signature to find, and a word to replace within it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// Short name, used in the message printed when it applies.
    pub name: String,
    /// Bytes that must appear exactly once in the image.
    pub signature: Vec<u8>,
    /// Offset within the signature of the bytes being replaced.
    pub at: usize,
    /// What to put there. Must not run past the end of the signature: a
    /// patch that writes outside what it matched is not a patch, it is a
    /// guess about the bytes next door.
    pub replacement: Vec<u8>,
    /// What the patch buys, for the log line.
    pub because: String,
    /// How many places in the image this is expected to change.
    pub reach: Reach,
}

/// How many places a patch is expected to change.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Reach {
    /// One place. If the image holds the signature more than once the patch
    /// refuses, unless the matches are copies of one another — see `apply`.
    ///
    /// This is the right answer almost always, and the refusal is the point:
    /// `trueffs.dll` ships several near-identical drivers and three separate
    /// addresses in it have turned out to be dead copies.
    Sole,
    /// Every match, because the routine is in more than one module and all of
    /// them are the same code.
    ///
    /// Only for a patch that has been shown to be that, module by module, and
    /// whose documentation says which modules and how it was established.
    /// It is not a way to make an ambiguous patch stop complaining.
    Every,
}

/// Why a patch did not apply.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// The signature is not in the image at all.
    NotFound,
    /// It is there more than once, so which one to patch is a guess.
    Ambiguous(usize),
}

/// ARM's canonical no-op: `mov r0, r0`.
pub const NOP: [u8; 4] = [0x00, 0x00, 0xA0, 0xE1];

/// Every patch has to stay inside the bytes it matched.
pub fn is_well_formed(p: &Patch) -> bool {
    p.at + p.replacement.len() <= p.signature.len()
}

/// How much either side of a signature has to agree before two matches are
/// treated as the same code in two places.
const CONTEXT: usize = 192;

/// Apply a patch, returning every offset it landed on.
///
/// A signature that matches more than once is usually a reason to stop, and
/// this used to stop unconditionally. That was right for `trueffs.dll`, which
/// carries several **near-identical but different** driver copies, where
/// patching the first hit would be a guess about which one runs.
///
/// It is wrong for a module the image genuinely holds twice, where both are
/// the same code and both should be patched.
///
/// So the two cases are told apart by looking wider: if every match has the
/// same `CONTEXT` bytes either side of it, they are copies of one thing and
/// all of them are patched. If the surroundings differ, the matches are
/// different code that happens to share a few instructions, and that is still
/// a reason to stop.
///
/// Note what the two halves of this ROM are, because it is easy to get
/// backwards. `NK.bin` carries the **system disk** — the Windows folder and
/// everything beside it, which is where the operating system lives. The
/// **flash disk** is user storage and holds no operating system code at all:
/// documents, address lists, dictionaries, the things somebody put there. A
/// module found twice is therefore two things inside the ROM, never a ROM
/// copy and a flash-disk copy.
pub fn apply(image: &mut [u8], patch: &Patch) -> Result<Vec<usize>, Failed> {
    let hits: Vec<usize> = image
        .windows(patch.signature.len())
        .enumerate()
        .filter(|(_, w)| *w == patch.signature)
        .map(|(i, _)| i)
        .collect();
    if hits.is_empty() {
        return Err(Failed::NotFound);
    }
    if patch.reach == Reach::Sole
        && hits.len() > 1
        && !all_alike(image, &hits, patch.signature.len())
    {
        return Err(Failed::Ambiguous(hits.len()));
    }
    let mut at = Vec::with_capacity(hits.len());
    for hit in hits {
        let o = hit + patch.at;
        image[o..o + patch.replacement.len()].copy_from_slice(&patch.replacement);
        at.push(o);
    }
    Ok(at)
}

/// The patches this emulator ships, embedded so a release binary needs
/// nothing beside it.
///
/// They are the same `.nkp` files that sit under `patches/`, so editing one
/// and rebuilding is all it takes; `--patches DIR` reads a directory at run
/// time instead, for trying one without a rebuild.
pub fn builtin() -> Result<Vec<Patch>, String> {
    [
        ("sd-folder-is-flash-disk.nkp", include_str!("../../../patches/sd-folder-is-flash-disk.nkp")),
        ("sd-profile-is-flash-disk.nkp", include_str!("../../../patches/sd-profile-is-flash-disk.nkp")),
    ]
    .iter()
    .map(|(name, text)| crate::nkp::parse(text, name))
    .collect()
}

/// Whether every match sits in the same surroundings.
fn all_alike(image: &[u8], hits: &[usize], len: usize) -> bool {
    let window = |h: usize| {
        let lo = h.saturating_sub(CONTEXT);
        let hi = (h + len + CONTEXT).min(image.len());
        &image[lo..hi]
    };
    let first = window(hits[0]);
    // A match too near either end has a short window and cannot be compared,
    // so treat it as unlike rather than as a match.
    hits.iter().all(|h| window(*h) == first)
}








#[cfg(test)]
mod tests {
    use super::*;

    /// A patch to exercise `apply` with, rather than borrowing a real one.
    ///
    /// The generic behaviour under test -- one match, several matches, none,
    /// staying inside the signature -- has nothing to do with any particular
    /// firmware, and a test that names a real patch breaks when that patch is
    /// retired for reasons of its own. Which is what happened to the one that
    /// used to be here.
    fn sample() -> Patch {
        Patch {
            name: "sample".into(),
            signature: vec![
                0x18, 0x20, 0x8d, 0xe2, // add r2, sp, #0x18
                0x06, 0x10, 0x81, 0xe3, // orr r1, r1, #0x6
                0x07, 0x00, 0xa0, 0xe1, // mov r0, r7
                0x00, 0x00, 0x50, 0xe3, // cmp r0, #0x0
                0x0a, 0x00, 0x00, 0x0a, // beq +0x28
            ],
            at: 16,
            replacement: NOP.to_vec(),
            because: "a fixture, so the tests do not depend on a real patch".into(),
            reach: Reach::Sole,
        }
    }

    fn image_with(sig: &[u8], copies: usize) -> Vec<u8> {
        let mut v = vec![0xAB; 16];
        for _ in 0..copies {
            v.extend_from_slice(sig);
            v.extend_from_slice(&[0xCD; 16]);
        }
        v
    }

    #[test]
    fn a_patch_replaces_exactly_its_word_and_nothing_else() {
        let p = &sample();
        let mut img = image_with(&p.signature, 1);
        let before = img.clone();
        let at = apply(&mut img, p).unwrap()[0];
        assert_eq!(img[at..at + 4], NOP, "the branch is gone");
        assert_eq!(img[..at], before[..at], "and nothing before it moved");
        assert_eq!(img[at + 4..], before[at + 4..], "nor after");
    }

    /// The shipped `.nkp` files have to parse, and every one of them has to
    /// stay inside what it matched -- a patch that writes past its signature
    /// is guessing about the bytes next door.
    ///
    /// This is the test that catches a bad edit to a patch file, which is now
    /// the easiest way to break the emulator without touching any Rust.
    #[test]
    fn every_shipped_patch_parses_and_stays_inside_what_it_matched() {
        let patches = builtin().expect("the shipped patches must parse");
        assert_eq!(patches.len(), 2, "two, both registry records");
        for p in &patches {
            assert!(is_well_formed(p), "{} runs past its signature", p.name);
            assert!(!p.because.is_empty(), "{} does not say why", p.name);
        }
    }

    /// Fetch a shipped patch by the name in its file.
    fn shipped(name: &str) -> Patch {
        builtin()
            .unwrap()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no shipped patch called {name:?}"))
    }







    /// The profile rewrite moves bytes between two records, so the region has
    /// to come out exactly as long as it went in. One byte either way and
    /// every record after it in the registry is at the wrong offset.
    #[test]
    fn the_profile_rewrite_preserves_the_length_of_what_it_replaces() {
        let p = &shipped("SD profile");
        assert_eq!(p.replacement.len(), p.signature.len());
        assert_eq!(p.at, 0, "it replaces the whole region");
    }

    /// Both records still have to describe themselves: the size field counts
    /// everything after it, and the header says how long the name and data
    /// are. A registry that disagrees with itself is worse than one that says
    /// the wrong folder.
    #[test]
    fn the_rewritten_records_still_describe_themselves() {
        let p = shipped("SD profile");
        let bytes = &p.replacement;
        let mut at = 0usize;
        let mut seen = Vec::new();
        while at < bytes.len() {
            let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as usize;
            let (size, two, chars, dlen) = (u16at(at), u16at(at + 2), u16at(at + 6), u16at(at + 8));
            assert_eq!(two, 2, "value marker");
            assert_eq!(size + 4, 10 + chars * 2 + dlen, "the size field must add up");
            let name: Vec<u16> = bytes[at + 10..at + 10 + chars * 2]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let data: Vec<u16> = bytes[at + 10 + chars * 2..at + 10 + chars * 2 + dlen]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            seen.push((
                String::from_utf16_lossy(&name).trim_end_matches('\0').to_string(),
                String::from_utf16_lossy(&data).trim_end_matches('\0').to_string(),
            ));
            at += size + 4;
        }
        assert_eq!(at, bytes.len(), "the records must tile the region exactly");
        assert_eq!(
            seen,
            vec![
                ("Folder".to_string(), "Flash Disk".to_string()),
                ("Name".to_string(), "SD Card".to_string()),
            ]
        );
    }

    /// The folder is renamed in place, so the name that replaces it has to be
    /// exactly as long. One byte longer and the record after it moves.
    #[test]
    fn the_folder_rename_is_the_same_length_as_what_it_replaces() {
        let p = &shipped("SD folder");
        let was = &p.signature[p.at..p.at + p.replacement.len()];
        assert_eq!(
            String::from_utf16_lossy(
                &was.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()
            ),
            "SDMMC Disk"
        );
        assert_eq!(
            String::from_utf16_lossy(
                &p.replacement.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()
            ),
            "Flash Disk"
        );
    }

    /// Patching the first of several matches would be a guess about which
    /// copy of the code is the live one, and this driver ships several
    /// near-identical copies of everything.
    /// Matches whose surroundings differ are different code that happens to
    /// share a few instructions, and patching the first would be a guess.
    /// `trueffs.dll` ships several near-identical drivers and this is what
    /// stops the wrong one being changed.
    #[test]
    fn matches_in_different_surroundings_are_refused() {
        let p = &sample();
        let mut img = vec![0x11u8; 400];
        img.extend_from_slice(&p.signature);
        img.extend_from_slice(&[0x22; 400]);
        img.extend_from_slice(&p.signature);
        img.extend_from_slice(&[0x33; 400]);
        assert_eq!(apply(&mut img, p), Err(Failed::Ambiguous(2)));
        assert!(!img.windows(4).any(|w| w == NOP), "and nothing was touched");
    }

    /// A module the image genuinely holds twice is the same code in two
    /// places, and both want patching.
    #[test]
    fn identical_copies_of_one_module_are_all_patched() {
        let p = &sample();
        let mut img = Vec::new();
        for _ in 0..2 {
            img.extend_from_slice(&[0xAB; 400]);
            img.extend_from_slice(&p.signature);
            img.extend_from_slice(&[0xCD; 400]);
        }
        let at = apply(&mut img, p).unwrap();
        assert_eq!(at.len(), 2, "both copies");
        for o in at {
            assert_eq!(&img[o..o + 4], NOP);
        }
    }

    #[test]
    fn a_missing_signature_is_reported_rather_than_ignored() {
        let mut img = vec![0u8; 4096];
        assert_eq!(apply(&mut img, &sample()), Err(Failed::NotFound));
    }

    /// `at` has to land on a whole instruction, not straddle two of them.
    #[test]
    fn a_patch_replaces_whole_instructions() {
        // Only for a patch that changes ARM code. Both of the patches left in
        // this file rewrite registry records, which are bytes rather than
        // instructions and are not word-aligned; the last code patch went
        // when the licence stopped needing one.
        let p = &sample();
        assert_eq!(p.at % 4, 0, "{} starts mid-instruction", p.name);
        assert_eq!(p.replacement.len() % 4, 0, "{} is not whole words", p.name);
        assert_eq!(p.signature.len() % 4, 0, "{} is not whole words", p.name);
    }

    /// An image with the signature in two modules, each surrounded by more
    /// than `CONTEXT` bytes of its own code, so the two are genuinely
    /// distinguishable. Padding matters: an image shorter than the context
    /// window makes every match look alike, which is a property of the test
    /// rather than of the code.
    fn two_modules_sharing(sig: &[u8]) -> Vec<u8> {
        let mut image = Vec::new();
        image.extend(std::iter::repeat_n(b'a', CONTEXT * 2));
        image.extend(sig);
        image.extend(std::iter::repeat_n(b'b', CONTEXT * 2));
        image.extend(sig);
        image.extend(std::iter::repeat_n(b'c', CONTEXT * 2));
        image
    }

    /// A routine a build linked into two modules sits in different
    /// surroundings in each, so the sameness test cannot recognise it and
    /// must not be asked to. `Every` is how a patch says it already knows,
    /// having been shown which modules those are.
    #[test]
    fn a_patch_that_names_every_match_changes_all_of_them() {
        let sig: &[u8] = b"ROUTINE";
        let mut image = two_modules_sharing(sig);
        let every = Patch {
            name: "t".into(), signature: sig.to_vec(), at: 0,
            replacement: b"PATCHED".to_vec(), because: "t".into(),
            reach: Reach::Every,
        };
        let at = apply(&mut image, &every).expect("Every patches regardless of surroundings");
        assert_eq!(at.len(), 2);
        assert_eq!(image.windows(7).filter(|w| *w == b"PATCHED").count(), 2);
        assert!(!image.windows(7).any(|w| w == sig), "no match left behind");
    }

    /// The same image, the same signature, with `Sole` -- because the default
    /// has to keep refusing. Several near-identical drivers ship in
    /// trueffs.dll and picking the wrong one is silent.
    #[test]
    fn the_same_matches_are_refused_when_a_patch_expects_one() {
        let sig: &[u8] = b"ROUTINE";
        let mut image = two_modules_sharing(sig);
        let sole = Patch {
            name: "t".into(), signature: sig.to_vec(), at: 0,
            replacement: b"PATCHED".to_vec(), because: "t".into(),
            reach: Reach::Sole,
        };
        assert_eq!(apply(&mut image, &sole), Err(Failed::Ambiguous(2)));
        assert!(!image.windows(7).any(|w| w == b"PATCHED"), "a refusal changes nothing");
    }

}
