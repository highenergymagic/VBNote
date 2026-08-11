//! Reading patches from `.nkp` files rather than from Rust.
//!
//! A patch is data: some bytes to look for and some bytes to put there. Having
//! that compiled in meant a new one needed a rebuild, and — worse — meant the
//! reasoning behind it lived in a doc comment where nobody editing the bytes
//! would look. A file keeps the two together.
//!
//! # The format
//!
//! ```text
//! # Everything after a hash is a comment, and that is where the case for the
//! # patch goes: what it works around, and what would make it unnecessary.
//! name: SD folder
//! because: so a card mounts where KeySoft looks for the flash disk
//! reach: sole
//! at: 12
//!
//! signature:
//!   53 00 44 00 4d 00 4d 00 43 00 00 00   # "SDMMC" and its terminator
//!   53 00 44 00 4d 00 4d 00 43 00 20 00   # "SDMMC "
//!
//! replacement:
//!   46 00 6c 00 61 00 73 00 68 00 20 00   # "Flash "
//! ```
//!
//! `at` is the offset within the signature that gets replaced, and defaults to
//! zero. `reach` is `sole` or `every`, and defaults to `sole` — see
//! [`crate::patch::Reach`], because `every` is a claim about the firmware that
//! has to be earned.
//!
//! Anything wrong is reported with the line it is on. A patch that quietly
//! does not parse is worse than one that refuses to.

use crate::patch::{Patch, Reach};

/// Parse one `.nkp` file.
pub fn parse(text: &str, source: &str) -> Result<Patch, String> {
    let mut name = None;
    let mut because = None;
    let mut reach = Reach::Sole;
    let mut at = 0usize;
    let mut signature: Vec<u8> = Vec::new();
    let mut replacement: Vec<u8> = Vec::new();
    // Which hex block the bytes on a line belong to, if any.
    let mut block: Option<&'static str> = None;

    for (n, raw) in text.lines().enumerate() {
        let at_line = |what: &str| format!("{source}: line {}: {what}", n + 1);
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "signature" | "replacement" => {
                    block = Some(if key == "signature" { "signature" } else { "replacement" });
                    // Bytes may start on the same line as the label.
                    let bytes = hex(value).map_err(|e| at_line(&e))?;
                    if key == "signature" {
                        signature.extend(bytes);
                    } else {
                        replacement.extend(bytes);
                    }
                    continue;
                }
                _ => block = None,
            }
            match key.as_str() {
                "name" => name = Some(value.to_string()),
                "because" => because = Some(value.to_string()),
                "at" => {
                    at = parse_number(value).ok_or_else(|| at_line("at needs a number"))?
                }
                "reach" => {
                    reach = match value.to_ascii_lowercase().as_str() {
                        "sole" => Reach::Sole,
                        "every" => Reach::Every,
                        other => return Err(at_line(&format!("{other:?} is not sole or every"))),
                    }
                }
                other => return Err(at_line(&format!("{other:?} is not a field"))),
            }
            continue;
        }

        match block {
            Some("signature") => signature.extend(hex(line).map_err(|e| at_line(&e))?),
            Some("replacement") => replacement.extend(hex(line).map_err(|e| at_line(&e))?),
            _ => return Err(at_line("bytes outside a signature or replacement block")),
        }
    }

    let name = name.ok_or_else(|| format!("{source}: no name"))?;
    let because = because.ok_or_else(|| format!("{source}: no because"))?;
    if signature.is_empty() {
        return Err(format!("{source}: no signature, so it would match everywhere"));
    }
    if replacement.is_empty() {
        return Err(format!("{source}: no replacement, so it would change nothing"));
    }
    let patch = Patch { name, signature, at, replacement, because, reach };
    if !crate::patch::is_well_formed(&patch) {
        return Err(format!(
            "{source}: the replacement runs past the signature: {} bytes at offset {} of {}",
            patch.replacement.len(),
            patch.at,
            patch.signature.len()
        ));
    }
    Ok(patch)
}

fn parse_number(text: &str) -> Option<usize> {
    let t = text.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}

/// Hex bytes, whitespace-separated or run together.
fn hex(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        if word.len() % 2 != 0 {
            return Err(format!("{word:?} is not whole bytes"));
        }
        for i in (0..word.len()).step_by(2) {
            let byte = u8::from_str_radix(&word[i..i + 2], 16)
                .map_err(|_| format!("{:?} is not hex", &word[i..i + 2]))?;
            out.push(byte);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# A comment, and a blank line follows.

name: test patch
because: so the tests have something to read
reach: every
at: 4

signature:
  aa bb cc dd
  ee ff   # trailing comment

replacement: 11 22
";

    #[test]
    fn a_file_parses_into_the_patch_it_describes() {
        let p = parse(SAMPLE, "test.nkp").unwrap();
        assert_eq!(p.name, "test patch");
        assert_eq!(p.because, "so the tests have something to read");
        assert_eq!(p.reach, Reach::Every);
        assert_eq!(p.at, 4);
        assert_eq!(p.signature, vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert_eq!(p.replacement, vec![0x11, 0x22]);
    }

    /// The defaults are the cautious ones: a patch that does not say how far
    /// it reaches only changes one place.
    #[test]
    fn reach_and_at_default_to_the_careful_answer() {
        let p = parse("name: n\nbecause: b\nsignature: 01 02\nreplacement: 03\n", "t").unwrap();
        assert_eq!(p.reach, Reach::Sole);
        assert_eq!(p.at, 0);
    }

    /// A patch that writes outside what it matched is a guess about the bytes
    /// next door, and `apply` would panic on it. Catch it at the file.
    #[test]
    fn a_replacement_past_the_signature_is_refused() {
        let e = parse("name: n\nbecause: b\nsignature: 01 02\nreplacement: 03 04 05\n", "t")
            .unwrap_err();
        assert!(e.contains("runs past the signature"), "{e}");
    }

    #[test]
    fn the_things_that_would_match_everything_are_refused() {
        assert!(parse("name: n\nbecause: b\nreplacement: 03\n", "t")
            .unwrap_err()
            .contains("no signature"));
        assert!(parse("name: n\nbecause: b\nsignature: 03\n", "t")
            .unwrap_err()
            .contains("no replacement"));
    }

    /// Every complaint carries the line it is on, because a patch file is
    /// edited by hand and a bare "parse error" is no help.
    #[test]
    fn mistakes_are_reported_with_their_line() {
        let e = parse("name: n\nbecause: b\nsignature:\n  zz\n", "t.nkp").unwrap_err();
        assert!(e.starts_with("t.nkp: line 4:"), "{e}");
        let e = parse("name: n\nwhat: no\n", "t.nkp").unwrap_err();
        assert!(e.starts_with("t.nkp: line 2:"), "{e}");
    }

    #[test]
    fn hex_may_be_spaced_or_run_together() {
        assert_eq!(hex("00 ff").unwrap(), vec![0, 255]);
        assert_eq!(hex("00ff").unwrap(), vec![0, 255]);
        assert!(hex("0").is_err());
        assert!(hex("gg").is_err());
    }
}
