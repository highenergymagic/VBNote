//! Build the licence the machine keeps in its 1-Wire part.
//!
//! A real VoiceNote carries an encrypted blob on a DS2431 beside the keyboard
//! controller, and KeySoft will not run without one: it decrypts it, checks
//! the machine's own identity against what is inside, and refuses to start
//! otherwise — first asking for a product key, then saying *"Cannot run this
//! version of KeySoft"*.
//!
//! This builds one. Not a forgery of a particular machine's licence: the
//! emulator supplies the 1-Wire part *and* its serial number, so the identity
//! inside the blob and the identity the machine reports are the same by
//! construction. It is issuing a consistent identity to a machine that is what
//! it says it is, which is the whole of what the firmware checks.
//!
//! # The format, from the code that reads it
//!
//! Three layers, all traced in `docs/hardware.md`:
//!
//! ```text
//! the wire     [length][blob][checksum]   checksum = the blob's bytes summed
//! the blob     RC2-CBC, zero IV, PKCS#5, keyed from MD5 of a passphrase
//! the payload  44 bytes of flags, locale, device id and entitlement
//! ```
//!
//! The cipher is the Windows CE CryptoAPI's, and the key material is the part
//! worth knowing: the Base Provider makes 40-bit RC2 keys, and with no salt
//! flags the key is salted with **zero**, so what RC2 is actually keyed with
//! is five bytes of MD5 followed by eleven zero bytes — a 128-bit key whose
//! effective length is 40.
//!
//! **Stop condition.** This exists because the mPower is discontinued and its
//! product keys cannot be obtained. If HumanWare are found to still issue
//! them, it comes out.

/// The passphrase KeySoft hashes, at VA `0x00254a50` in `KeySoft.exe`.
const PASSPHRASE: &[u8] = b"s#r14^ln5m";

/// Bytes of the MD5 hash the derived key uses. The Base Provider's RC2 key is
/// 40 bits.
const KEY_BYTES: usize = 5;
/// What the key material is padded to with zero salt.
const SALTED_TO: usize = 16;
/// RC2's effective key length, in bits.
const EFFECTIVE_BITS: usize = 40;

/// What the machine is licensed to run. Everything here is a statement about
/// this emulator, not about anybody's hardware.
#[derive(Debug, Clone, Copy)]
pub struct Licence {
    /// Six bytes of 1-Wire serial. The device id inside the blob is this,
    /// zero-padded, and the part reports the same, so the two agree.
    pub serial: [u8; 6],
    /// Locale. `0x0809` English, `0x040c` French, `0x0c0c` French Canadian.
    pub lcid: u16,
    /// Bit 1 clear means "use the locale", bit 0 clear means VoiceNote.
    pub flags2: u32,
    /// Compared against what `0x00175074` returns, which is 2 on this ROM.
    pub machine_class: u32,
    /// Entitlement: major version, build (compared rounded down to a ten),
    /// and model class, where 0 accepts any machine.
    pub version: u32,
    pub build: u32,
    pub model: u32,
}

impl Default for Licence {
    fn default() -> Self {
        Licence {
            serial: [1, 2, 3, 4, 5, 6],
            lcid: 0x0809,
            flags2: 0,
            machine_class: 2,
            version: 8,
            build: 20,
            model: 0,
        }
    }
}

impl Licence {
    /// The 44 bytes the validator copies out and reads fields from.
    pub fn payload(&self) -> [u8; 44] {
        let mut p = [0u8; 44];
        // +0x04 has only to be non-zero; the validator refuses a zero.
        p[0x04..0x08].copy_from_slice(&1u32.to_le_bytes());
        p[0x08..0x0c].copy_from_slice(&self.machine_class.to_le_bytes());
        p[0x0c..0x10].copy_from_slice(&self.flags2.to_le_bytes());
        p[0x10..0x12].copy_from_slice(&self.lcid.to_le_bytes());
        p[0x18..0x1e].copy_from_slice(&self.serial);
        p[0x20..0x24].copy_from_slice(&self.version.to_le_bytes());
        p[0x24..0x28].copy_from_slice(&self.build.to_le_bytes());
        p[0x28..0x2c].copy_from_slice(&self.model.to_le_bytes());
        p
    }

    /// The encrypted blob: what a product key is, and what the part holds.
    pub fn blob(&self) -> Vec<u8> {
        let key = derived_key();
        let sched = rc2_expand(&key, EFFECTIVE_BITS);
        cbc_encrypt(&sched, &pkcs5_pad(&self.payload()))
    }

    /// A whole 1-Wire dump: the 64-bit identity, then the memory holding the
    /// record. This is what `--serial-eeprom` reads.
    pub fn eeprom(&self) -> Vec<u8> {
        let blob = self.blob();
        let mut mem = Vec::with_capacity(crate::onewire::SIZE);
        mem.extend(crate::onewire::record(&blob));
        mem.resize(crate::onewire::SIZE, 0xFF);

        let mut out = crate::onewire::rom_id(0x2D, self.serial).to_vec();
        out.extend(mem);
        out
    }
}

/// The key material RC2 is given: five bytes of MD5, then zero salt.
fn derived_key() -> [u8; SALTED_TO] {
    let hash = md5(PASSPHRASE);
    let mut key = [0u8; SALTED_TO];
    key[..KEY_BYTES].copy_from_slice(&hash[..KEY_BYTES]);
    key
}

fn pkcs5_pad(data: &[u8]) -> Vec<u8> {
    let n = 8 - (data.len() % 8);
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(n as u8, n));
    out
}

fn cbc_encrypt(sched: &[u16; 64], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 8];
    for chunk in data.chunks(8) {
        let mut block = [0u8; 8];
        for i in 0..8 {
            block[i] = chunk[i] ^ prev[i];
        }
        prev = rc2_encrypt_block(sched, block);
        out.extend_from_slice(&prev);
    }
    out
}

// ---------------------------------------------------------------- RC2

/// RFC 2268's PITABLE, a fixed permutation of the digits of pi.
const PITABLE: [u8; 256] = [
    0xd9, 0x78, 0xf9, 0xc4, 0x19, 0xdd, 0xb5, 0xed, 0x28, 0xe9, 0xfd, 0x79, 0x4a, 0xa0, 0xd8, 0x9d,
    0xc6, 0x7e, 0x37, 0x83, 0x2b, 0x76, 0x53, 0x8e, 0x62, 0x4c, 0x64, 0x88, 0x44, 0x8b, 0xfb, 0xa2,
    0x17, 0x9a, 0x59, 0xf5, 0x87, 0xb3, 0x4f, 0x13, 0x61, 0x45, 0x6d, 0x8d, 0x09, 0x81, 0x7d, 0x32,
    0xbd, 0x8f, 0x40, 0xeb, 0x86, 0xb7, 0x7b, 0x0b, 0xf0, 0x95, 0x21, 0x22, 0x5c, 0x6b, 0x4e, 0x82,
    0x54, 0xd6, 0x65, 0x93, 0xce, 0x60, 0xb2, 0x1c, 0x73, 0x56, 0xc0, 0x14, 0xa7, 0x8c, 0xf1, 0xdc,
    0x12, 0x75, 0xca, 0x1f, 0x3b, 0xbe, 0xe4, 0xd1, 0x42, 0x3d, 0xd4, 0x30, 0xa3, 0x3c, 0xb6, 0x26,
    0x6f, 0xbf, 0x0e, 0xda, 0x46, 0x69, 0x07, 0x57, 0x27, 0xf2, 0x1d, 0x9b, 0xbc, 0x94, 0x43, 0x03,
    0xf8, 0x11, 0xc7, 0xf6, 0x90, 0xef, 0x3e, 0xe7, 0x06, 0xc3, 0xd5, 0x2f, 0xc8, 0x66, 0x1e, 0xd7,
    0x08, 0xe8, 0xea, 0xde, 0x80, 0x52, 0xee, 0xf7, 0x84, 0xaa, 0x72, 0xac, 0x35, 0x4d, 0x6a, 0x2a,
    0x96, 0x1a, 0xd2, 0x71, 0x5a, 0x15, 0x49, 0x74, 0x4b, 0x9f, 0xd0, 0x5e, 0x04, 0x18, 0xa4, 0xec,
    0xc2, 0xe0, 0x41, 0x6e, 0x0f, 0x51, 0xcb, 0xcc, 0x24, 0x91, 0xaf, 0x50, 0xa1, 0xf4, 0x70, 0x39,
    0x99, 0x7c, 0x3a, 0x85, 0x23, 0xb8, 0xb4, 0x7a, 0xfc, 0x02, 0x36, 0x5b, 0x25, 0x55, 0x97, 0x31,
    0x2d, 0x5d, 0xfa, 0x98, 0xe3, 0x8a, 0x92, 0xae, 0x05, 0xdf, 0x29, 0x10, 0x67, 0x6c, 0xba, 0xc9,
    0xd3, 0x00, 0xe6, 0xcf, 0xe1, 0x9e, 0xa8, 0x2c, 0x63, 0x16, 0x01, 0x3f, 0x58, 0xe2, 0x89, 0xa9,
    0x0d, 0x38, 0x34, 0x1b, 0xab, 0x33, 0xff, 0xb0, 0xbb, 0x48, 0x0c, 0x5f, 0xb9, 0xb1, 0xcd, 0x2e,
    0xc5, 0xf3, 0xdb, 0x47, 0xe5, 0xa5, 0x9c, 0x77, 0x0a, 0xa6, 0x20, 0x68, 0xfe, 0x7f, 0xc1, 0xad,
];

/// How far each word rotates in a mixing round.
const ROTATIONS: [u32; 4] = [1, 2, 3, 5];

/// RFC 2268 key expansion.
fn rc2_expand(key: &[u8], t1_bits: usize) -> [u16; 64] {
    let t = key.len();
    let mut l = [0u8; 128];
    l[..t].copy_from_slice(key);
    for i in t..128 {
        l[i] = PITABLE[(l[i - 1] as usize + l[i - t] as usize) & 0xFF];
    }
    let t8 = t1_bits.div_ceil(8);
    let tm = 255u32 % (1u32 << (8 + t1_bits - 8 * t8));
    l[128 - t8] = PITABLE[(l[128 - t8] as u32 & tm) as usize];
    for i in (0..128 - t8).rev() {
        l[i] = PITABLE[(l[i + 1] ^ l[i + t8]) as usize];
    }
    let mut k = [0u16; 64];
    for (i, word) in k.iter_mut().enumerate() {
        *word = l[2 * i] as u16 | ((l[2 * i + 1] as u16) << 8);
    }
    k
}

fn rc2_encrypt_block(k: &[u16; 64], block: [u8; 8]) -> [u8; 8] {
    let mut r = [0u16; 4];
    for i in 0..4 {
        r[i] = u16::from_le_bytes([block[2 * i], block[2 * i + 1]]);
    }
    let mut j = 0usize;
    for round in 0..16 {
        for i in 0..4 {
            let (a, b, c) = ((i + 3) % 4, (i + 2) % 4, (i + 1) % 4);
            r[i] = r[i]
                .wrapping_add(k[j])
                .wrapping_add(r[a] & r[b])
                .wrapping_add(!r[a] & r[c]);
            r[i] = r[i].rotate_left(ROTATIONS[i]);
            j += 1;
        }
        // Two mashing rounds, after the fifth and the eleventh mix.
        if round == 4 || round == 10 {
            for i in 0..4 {
                r[i] = r[i].wrapping_add(k[(r[(i + 3) % 4] & 63) as usize]);
            }
        }
    }
    let mut out = [0u8; 8];
    for i in 0..4 {
        out[2 * i..2 * i + 2].copy_from_slice(&r[i].to_le_bytes());
    }
    out
}

// ---------------------------------------------------------------- MD5

/// MD5, because `CryptDeriveKey` derives from an MD5 hash and the key comes
/// out of the first five bytes of it.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> =
        (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();

    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 1321's own vectors. Everything else here depends on this being
    /// right, because the key is five bytes of it.
    #[test]
    fn md5_matches_its_specification() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    /// RFC 2268's own vectors, including the short-key case this uses.
    #[test]
    fn rc2_matches_its_specification() {
        for (key, plain, bits, want) in [
            ("0000000000000000", "0000000000000000", 63, "ebb773f993278eff"),
            ("ffffffffffffffff", "ffffffffffffffff", 64, "278b27e42e2f0d49"),
            ("88bca90e90875a7f0f79c384627bafb2", "0000000000000000", 128, "2269552ab0f85ca6"),
        ] {
            let sched = rc2_expand(&unhex(key), bits);
            let mut block = [0u8; 8];
            block.copy_from_slice(&unhex(plain));
            assert_eq!(hex(&rc2_encrypt_block(&sched, block)), want, "T1={bits}");
        }
    }

    /// The key material is five bytes of MD5 and eleven of zero salt. Getting
    /// this wrong is why several earlier attempts were refused: the Base
    /// Provider salts a 40-bit key, and the salt is zero unless asked
    /// otherwise, so the cipher sees sixteen bytes rather than five.
    #[test]
    fn the_key_is_salted_to_sixteen_bytes() {
        let key = derived_key();
        assert_eq!(key.len(), 16);
        assert_eq!(&key[..5], &md5(PASSPHRASE)[..5]);
        assert!(key[5..].iter().all(|b| *b == 0), "the salt is zero");
    }

    /// The device id inside the blob has to be the serial the part reports,
    /// or the validator refuses it. They come from one place so they cannot
    /// drift apart.
    #[test]
    fn the_identity_inside_matches_the_part_outside() {
        let l = Licence { serial: [9, 8, 7, 6, 5, 4], ..Default::default() };
        let payload = l.payload();
        assert_eq!(&payload[0x18..0x1e], &[9, 8, 7, 6, 5, 4]);
        assert_eq!(&payload[0x1e..0x20], &[0, 0], "and zero-padded to eight");
        let dump = l.eeprom();
        assert_eq!(&dump[1..7], &[9, 8, 7, 6, 5, 4], "the same serial in the ROM id");
    }

    /// The validator reads the length before anything else and requires
    /// exactly 0x2c.
    #[test]
    fn the_payload_is_the_length_the_validator_demands() {
        assert_eq!(Licence::default().payload().len(), 0x2c);
    }

    /// The blob is whole blocks, and the record around it carries the length
    /// and checksum the reader retries until it agrees with.
    #[test]
    fn the_record_frames_the_blob_the_way_the_reader_expects() {
        let l = Licence::default();
        let blob = l.blob();
        assert_eq!(blob.len() % 8, 0, "whole RC2 blocks");
        assert_eq!(blob.len(), 48, "44 bytes padded to a block boundary");
        let dump = l.eeprom();
        let mem = &dump[8..];
        assert_eq!(mem[0] as usize, blob.len(), "the length byte");
        assert_eq!(&mem[1..1 + blob.len()], &blob[..]);
        let sum = blob.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(mem[1 + blob.len()], sum, "the checksum");
    }

    /// Pinned against the blob KeySoft's own validator accepted, so a change
    /// to any part of the chain that would stop the machine starting fails
    /// here first.
    #[test]
    fn the_default_licence_is_the_one_that_was_accepted() {
        let want = "ad75c4a606b4b024a71207570f051595be5fa05b9e58553521cf6b07e1bb692a\
                    fd9ca858b9df1ff6d0d18c8bb9ca20e9";
        assert_eq!(hex(&Licence::default().blob()), want);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }
}
