"""Reading the flash disk image from outside the emulator.

The wizard needs one fact: has the machine finished setting itself up? The
answer is visible in the card image without running anything, because KeySoft
makes a folder called `General` on the flash disk the first time it comes up
properly. So this parses just enough of the partition table and FAT to list
the root directory.

Deliberately read-only and deliberately small. It is not a filesystem driver;
it opens a file the emulator may still be writing to, reads a few kilobytes,
and answers a yes or no question. Anything it does not understand is reported
as "not yet", which is the safe answer for something used to decide whether to
keep waiting.
"""
from __future__ import annotations

import struct

#: Where the machine's own files land once it has set itself up.
READY_FOLDER = "GENERAL"


class NotReady(Exception):
    """The image cannot be read as a formatted disk yet."""


def _u16(b: bytes, at: int) -> int:
    return struct.unpack_from("<H", b, at)[0]


def _u32(b: bytes, at: int) -> int:
    return struct.unpack_from("<I", b, at)[0]


#: Partition types that hold another partition table rather than a volume.
_EXTENDED = (0x05, 0x0F, 0x85)


def _entries(sector: bytes):
    """The four partition entries of a table, skipping empty slots."""
    if len(sector) < 512 or sector[510:512] != b"\x55\xaa":
        raise NotReady("no boot signature")
    for i in range(4):
        entry = sector[446 + i * 16: 446 + (i + 1) * 16]
        if entry[4] != 0:
            yield entry[4], _u32(entry, 8)


def _first_volume_sector(f) -> int:
    """Sector the first real volume starts at.

    Windows CE does not lay this card out the obvious way. It makes a single
    *extended* partition, type `0x05`, whose first sector is another partition
    table, and the volume itself lives inside that. Reading the outer entry as
    a volume finds a boot signature and a boot record full of zeroes, which
    looks exactly like "partitioned but not yet formatted" and is not.
    """
    f.seek(0)
    for kind, start in _entries(f.read(512)):
        if kind not in _EXTENDED:
            return start
        # An extended entry: its own table is at `start`, and the volume
        # inside it sits at an offset relative to there.
        f.seek(start * 512)
        for inner_kind, inner_start in _entries(f.read(512)):
            if inner_kind not in _EXTENDED:
                return start + inner_start
    raise NotReady("no volume in the partition table")


class RootDirectory:
    """The root directory of the first FAT volume in an image."""

    def __init__(self, path: str):
        self.path = path
        with open(path, "rb") as f:
            base = _first_volume_sector(f) * 512
            f.seek(base)
            boot = f.read(512)
            if len(boot) < 512 or boot[510:512] != b"\x55\xaa":
                raise NotReady("no volume where the partition says")

            bytes_per_sector = _u16(boot, 11)
            sectors_per_cluster = boot[13]
            reserved = _u16(boot, 14)
            fats = boot[16]
            root_entries = _u16(boot, 17)
            fat_sectors = _u16(boot, 22) or _u32(boot, 36)
            if not bytes_per_sector or not sectors_per_cluster or not fats:
                raise NotReady("not formatted yet")

            if root_entries:
                # FAT12/16: the root directory is a fixed area after the FATs.
                at = base + (reserved + fats * fat_sectors) * bytes_per_sector
                length = root_entries * 32
            else:
                # FAT32: the root is a normal cluster chain. Reading its first
                # cluster is enough to see whether the folder is there.
                root_cluster = _u32(boot, 44)
                data = base + (reserved + fats * fat_sectors) * bytes_per_sector
                at = data + (root_cluster - 2) * sectors_per_cluster * bytes_per_sector
                length = sectors_per_cluster * bytes_per_sector
            f.seek(at)
            self._raw = f.read(length)

    def names(self) -> list[str]:
        """Short names in the root, directories and files alike."""
        out = []
        for off in range(0, len(self._raw) - 31, 32):
            entry = self._raw[off:off + 32]
            first = entry[0]
            if first == 0x00:
                break          # nothing beyond here has ever been used
            if first == 0xE5:
                continue       # deleted
            attr = entry[11]
            if attr == 0x0F:
                continue       # a long-name fragment, not an entry
            if attr & 0x08:
                continue       # the volume label
            name = entry[0:8].decode("latin-1").rstrip()
            ext = entry[8:11].decode("latin-1").rstrip()
            out.append(f"{name}.{ext}" if ext else name)
        return out


def is_ready(path: str) -> bool:
    """Whether the machine has finished setting the flash disk up.

    False for every kind of "cannot tell yet", because the only thing this is
    used for is deciding whether to go on waiting.
    """
    try:
        return READY_FOLDER in (n.upper() for n in RootDirectory(path).names())
    except (NotReady, OSError, struct.error, IndexError):
        return False


if __name__ == "__main__":
    import sys

    for image in sys.argv[1:]:
        try:
            listing = ", ".join(RootDirectory(image).names()) or "(empty)"
        except (NotReady, OSError, struct.error, IndexError) as e:
            listing = f"unreadable: {e}"
        print(f"{image}: {listing}")
        print(f"  ready: {is_ready(image)}")
