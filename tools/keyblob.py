"""Build and read the KeySoft licence blob.

The 1-Wire EEPROM carries an encrypted 44-byte payload, and a product key is
the same blob supplied by hand -- KeySoft copies whatever it is given into the
validation context and runs its ordinary validator over it.

The scheme is the Windows CE CryptoAPI, read out of the calls KeySoft makes:

    CryptAcquireContext(PROV_RSA_FULL, CRYPT_VERIFYCONTEXT)  0x0006855c
    CryptCreateHash(CALG_MD5 = 0x8003)                       0x0006861c
    CryptHashData("s#r14^ln5m")                              0x00068670
    CryptDeriveKey(CALG_RC2 = 0x6602, hash, flags = 1)       0x000686b0
    CryptDecrypt(key, Final = TRUE)                          0x000687d0

So **RC2**, keyed by deriving from an MD5 hash of the passphrase at VA
`0x00254a50`, in CBC with a zero IV and PKCS#5 padding -- the CryptoAPI's
defaults for a block cipher. `Final = TRUE` is why a wrong guess is rejected
outright rather than producing rubbish: it validates the padding.

The Blowfish tables that are also in KeySoft.exe belong to some other path.
They are not this one.

Payload fields, from docs/hardware.md:

  +0x04  flags1     non-zero or the validator fails
  +0x08  compared against what 0x00175074 returns
  +0x0c  flags2     bit 1 ignore locale, bit 0 BrailleNote
  +0x10  LCID       0x0809 English
  +0x18  device id  8 bytes, against IOCTL_HAL_GET_DEVICEID
  +0x20  version    entitlement
  +0x24  build      entitlement, compared rounded down to a ten
  +0x28  model      entitlement, 0 accepts any
"""

import argparse
import hashlib
import struct
import sys

PASSPHRASE = b"s#r14^ln5m"

# RFC 2268's PITABLE.
PITABLE = bytes([
    0xd9, 0x78, 0xf9, 0xc4, 0x19, 0xdd, 0xb5, 0xed, 0x28, 0xe9, 0xfd, 0x79,
    0x4a, 0xa0, 0xd8, 0x9d, 0xc6, 0x7e, 0x37, 0x83, 0x2b, 0x76, 0x53, 0x8e,
    0x62, 0x4c, 0x64, 0x88, 0x44, 0x8b, 0xfb, 0xa2, 0x17, 0x9a, 0x59, 0xf5,
    0x87, 0xb3, 0x4f, 0x13, 0x61, 0x45, 0x6d, 0x8d, 0x09, 0x81, 0x7d, 0x32,
    0xbd, 0x8f, 0x40, 0xeb, 0x86, 0xb7, 0x7b, 0x0b, 0xf0, 0x95, 0x21, 0x22,
    0x5c, 0x6b, 0x4e, 0x82, 0x54, 0xd6, 0x65, 0x93, 0xce, 0x60, 0xb2, 0x1c,
    0x73, 0x56, 0xc0, 0x14, 0xa7, 0x8c, 0xf1, 0xdc, 0x12, 0x75, 0xca, 0x1f,
    0x3b, 0xbe, 0xe4, 0xd1, 0x42, 0x3d, 0xd4, 0x30, 0xa3, 0x3c, 0xb6, 0x26,
    0x6f, 0xbf, 0x0e, 0xda, 0x46, 0x69, 0x07, 0x57, 0x27, 0xf2, 0x1d, 0x9b,
    0xbc, 0x94, 0x43, 0x03, 0xf8, 0x11, 0xc7, 0xf6, 0x90, 0xef, 0x3e, 0xe7,
    0x06, 0xc3, 0xd5, 0x2f, 0xc8, 0x66, 0x1e, 0xd7, 0x08, 0xe8, 0xea, 0xde,
    0x80, 0x52, 0xee, 0xf7, 0x84, 0xaa, 0x72, 0xac, 0x35, 0x4d, 0x6a, 0x2a,
    0x96, 0x1a, 0xd2, 0x71, 0x5a, 0x15, 0x49, 0x74, 0x4b, 0x9f, 0xd0, 0x5e,
    0x04, 0x18, 0xa4, 0xec, 0xc2, 0xe0, 0x41, 0x6e, 0x0f, 0x51, 0xcb, 0xcc,
    0x24, 0x91, 0xaf, 0x50, 0xa1, 0xf4, 0x70, 0x39, 0x99, 0x7c, 0x3a, 0x85,
    0x23, 0xb8, 0xb4, 0x7a, 0xfc, 0x02, 0x36, 0x5b, 0x25, 0x55, 0x97, 0x31,
    0x2d, 0x5d, 0xfa, 0x98, 0xe3, 0x8a, 0x92, 0xae, 0x05, 0xdf, 0x29, 0x10,
    0x67, 0x6c, 0xba, 0xc9, 0xd3, 0x00, 0xe6, 0xcf, 0xe1, 0x9e, 0xa8, 0x2c,
    0x63, 0x16, 0x01, 0x3f, 0x58, 0xe2, 0x89, 0xa9, 0x0d, 0x38, 0x34, 0x1b,
    0xab, 0x33, 0xff, 0xb0, 0xbb, 0x48, 0x0c, 0x5f, 0xb9, 0xb1, 0xcd, 0x2e,
    0xc5, 0xf3, 0xdb, 0x47, 0xe5, 0xa5, 0x9c, 0x77, 0x0a, 0xa6, 0x20, 0x68,
    0xfe, 0x7f, 0xc1, 0xad,
])

ROTATIONS = (1, 2, 3, 5)


def rc2_expand(key, t1_bits):
    """RFC 2268 key expansion. `t1_bits` is RC2's effective key length."""
    t = len(key)
    L = list(key) + [0] * (128 - t)
    for i in range(t, 128):
        L[i] = PITABLE[(L[i - 1] + L[i - t]) & 0xFF]
    t8 = (t1_bits + 7) // 8
    tm = 255 % (1 << (8 + t1_bits - 8 * t8))
    L[128 - t8] = PITABLE[L[128 - t8] & tm]
    for i in range(127 - t8, -1, -1):
        L[i] = PITABLE[L[i + 1] ^ L[i + t8]]
    return [L[2 * i] | (L[2 * i + 1] << 8) for i in range(64)]


def _rol(x, n):
    return ((x << n) | (x >> (16 - n))) & 0xFFFF


def _ror(x, n):
    return ((x >> n) | (x << (16 - n))) & 0xFFFF


def rc2_encrypt_block(K, block):
    R = list(struct.unpack("<4H", block))
    j = 0
    for rnd in range(16):
        for i in range(4):
            a, b, c = (i + 3) % 4, (i + 2) % 4, (i + 1) % 4
            R[i] = (R[i] + K[j] + (R[a] & R[b]) + ((~R[a] & 0xFFFF) & R[c])) & 0xFFFF
            R[i] = _rol(R[i], ROTATIONS[i])
            j += 1
        if rnd in (4, 10):
            for i in range(4):
                R[i] = (R[i] + K[R[(i + 3) % 4] & 63]) & 0xFFFF
    return struct.pack("<4H", *R)


def rc2_decrypt_block(K, block):
    R = list(struct.unpack("<4H", block))
    j = 63
    for rnd in range(15, -1, -1):
        for i in range(3, -1, -1):
            a, b, c = (i + 3) % 4, (i + 2) % 4, (i + 1) % 4
            R[i] = _ror(R[i], ROTATIONS[i])
            R[i] = (R[i] - K[j] - (R[a] & R[b]) - ((~R[a] & 0xFFFF) & R[c])) & 0xFFFF
            j -= 1
        # The mash sits between mix 4 and mix 5, and between 10 and 11, so
        # undoing it comes after undoing the mix that follows it.
        if rnd in (5, 11):
            for i in range(3, -1, -1):
                R[i] = (R[i] - K[R[(i + 3) % 4] & 63]) & 0xFFFF
    return struct.pack("<4H", *R)


def derive_key(passphrase, key_bytes, salt_to=0):
    """The key material CryptDeriveKey hands the cipher.

    With an MD5 hash it takes the leading bytes. The wrinkle is salt: unless
    `CRYPT_NO_SALT` is asked for, a 40-bit key is given a salt value, and with
    neither `CRYPT_CREATE_SALT` nor a salt set that value is **zero**. So the
    material RC2 actually keys with is five derived bytes followed by eleven
    zero bytes -- a 128-bit key whose effective length is still 40.

    `salt_to` is the length to pad to; 0 leaves the derived bytes alone.
    """
    key = hashlib.md5(passphrase).digest()[:key_bytes]
    if salt_to > len(key):
        key = key + bytes(salt_to - len(key))
    return key


def cbc_encrypt(K, data, iv=bytes(8)):
    out, prev = b"", iv
    for i in range(0, len(data), 8):
        prev = rc2_encrypt_block(K, bytes(x ^ y for x, y in zip(data[i:i + 8], prev)))
        out += prev
    return out


def cbc_decrypt(K, data, iv=bytes(8)):
    out, prev = b"", iv
    for i in range(0, len(data), 8):
        blk = data[i:i + 8]
        out += bytes(x ^ y for x, y in zip(rc2_decrypt_block(K, blk), prev))
        prev = blk
    return out


def pkcs5_pad(data):
    n = 8 - (len(data) % 8)
    return data + bytes([n]) * n


def pkcs5_unpad(data):
    if not data:
        return data
    n = data[-1]
    return data[:-n] if 1 <= n <= 8 else data


def build_payload(a):
    p = bytearray(44)
    struct.pack_into("<I", p, 0x04, a.flags1)
    struct.pack_into("<I", p, 0x08, a.field08)
    struct.pack_into("<I", p, 0x0c, a.flags2)
    struct.pack_into("<H", p, 0x10, a.lcid)
    d = bytes.fromhex(a.device_id)
    p[0x18:0x18 + min(8, len(d))] = d[:8]
    struct.pack_into("<I", p, 0x20, a.version)
    struct.pack_into("<I", p, 0x24, a.build)
    struct.pack_into("<I", p, 0x28, a.model)
    return bytes(p)


def selftest():
    """RFC 2268's own vectors, so the engine is known good before anything
    leans on it."""
    cases = [
        ("0000000000000000", "0000000000000000", 63, "ebb773f993278eff"),
        ("ffffffffffffffff", "ffffffffffffffff", 64, "278b27e42e2f0d49"),
        ("88bca90e90875a7f0f79c384627bafb2", "0000000000000000", 128,
         "2269552ab0f85ca6"),
    ]
    ok = True
    for key, pt, bits, want in cases:
        K = rc2_expand(bytes.fromhex(key), bits)
        got = rc2_encrypt_block(K, bytes.fromhex(pt)).hex()
        back = rc2_decrypt_block(K, bytes.fromhex(got)).hex()
        good = got == want and back == pt
        ok = ok and good
        print(f"  RC2 T1={bits:<4} {got}  want {want}  {'OK' if good else 'FAIL'}")
    print(f"self-test: {'OK' if ok else 'FAIL'}")
    return ok


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("selftest", help="check the RC2 engine against RFC 2268")

    b = sub.add_parser("build", help="build an encrypted blob")
    b.add_argument("--flags1", type=lambda x: int(x, 0), default=1)
    b.add_argument("--field08", type=lambda x: int(x, 0), default=0)
    b.add_argument("--flags2", type=lambda x: int(x, 0), default=0)
    b.add_argument("--lcid", type=lambda x: int(x, 0), default=0x0809)
    b.add_argument("--device-id", default="0102030405060708")
    b.add_argument("--version", type=lambda x: int(x, 0), default=8)
    b.add_argument("--build", type=lambda x: int(x, 0), default=20)
    b.add_argument("--model", type=lambda x: int(x, 0), default=0)
    b.add_argument("--key-bytes", type=int, default=5,
                   help="what CryptDeriveKey produced; 5 is 40-bit RC2")
    b.add_argument("--effective-bits", type=int, default=40)
    b.add_argument("--salt-to", type=int, default=16,
                   help="pad the key material to this many bytes with zero "
                        "salt; 0 to leave it alone")
    b.add_argument("--out")

    e = sub.add_parser("eeprom", help="write a full 1-Wire dump around a blob")
    e.add_argument("blob")
    e.add_argument("--out", default="work/SerialNumber.bin")
    e.add_argument("--family", type=lambda x: int(x, 0), default=0x2D)
    e.add_argument("--serial", default="010203040506")

    s = sub.add_parser("show", help="decrypt and print a blob")
    s.add_argument("blob")
    s.add_argument("--key-bytes", type=int, default=5)
    s.add_argument("--effective-bits", type=int, default=40)
    s.add_argument("--salt-to", type=int, default=16)

    a = ap.parse_args()

    if a.cmd == "selftest":
        return 0 if selftest() else 1

    if a.cmd == "build":
        K = rc2_expand(derive_key(PASSPHRASE, a.key_bytes, a.salt_to),
                       a.effective_bits)
        payload = build_payload(a)
        blob = cbc_encrypt(K, pkcs5_pad(payload))
        print(f"payload  {payload.hex()}")
        print(f"blob     {blob.hex()}")
        print(f"         {len(blob)} bytes, RC2-CBC, {a.key_bytes * 8}-bit key "
              f"salted to {a.salt_to} bytes, T1={a.effective_bits}")
        if a.out:
            open(a.out, "wb").write(blob)
            print(f"written to {a.out}")
        return 0

    if a.cmd == "eeprom":
        blob = open(a.blob, "rb").read()
        rec = bytes([len(blob)]) + blob + bytes([sum(blob) & 0xFF])
        mem = rec + bytes([0xFF]) * (128 - len(rec))
        rom = bytes([a.family]) + bytes.fromhex(a.serial)
        crc = 0
        for byte in rom:
            v = byte
            for _ in range(8):
                mix = (crc ^ v) & 1
                crc >>= 1
                if mix:
                    crc ^= 0x8C
                v >>= 1
        open(a.out, "wb").write(rom + bytes([crc]) + mem)
        print(f"wrote {a.out}: record len={len(blob)} checksum={sum(blob) & 0xFF:#04x}")
        return 0

    if a.cmd == "show":
        K = rc2_expand(derive_key(PASSPHRASE, a.key_bytes, a.salt_to),
                       a.effective_bits)
        payload = pkcs5_unpad(cbc_decrypt(K, open(a.blob, "rb").read()))
        print(f"payload  {payload.hex()}  ({len(payload)} bytes)")
        for name, off, size in [("flags1", 0x04, 4), ("field08", 0x08, 4),
                                ("flags2", 0x0c, 4), ("lcid", 0x10, 2),
                                ("device", 0x18, 8), ("version", 0x20, 4),
                                ("build", 0x24, 4), ("model", 0x28, 4)]:
            print(f"  +{off:#04x} {name:8} {payload[off:off + size].hex()}")
        return 0


if __name__ == "__main__":
    sys.exit(main())
