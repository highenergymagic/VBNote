"""List every function in an extracted Windows CE PE from its .pdata.

ARM PE images carry an exception directory: one 8-byte entry per function,
holding the start address and a packed prologue/length. That is a complete
function list, including the ones nothing ever branches to -- which a
disassembler that follows branches will never find, and which is exactly
where the interesting code hides in an image built around indirect dispatch.

    python tools/cepdata.py tools/extracted/KeySoft.exe.pe            # all
    python tools/cepdata.py tools/extracted/KeySoft.exe.pe 0x001773dc # one
"""
import struct
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from armdis import Image

#: Exception directory index in the PE optional header's data directories.
IMAGE_DIRECTORY_ENTRY_EXCEPTION = 3


def functions(path):
    img = Image(path)
    d = img.d
    pe = struct.unpack_from('<I', d, 0x3C)[0]
    opt = pe + 24
    rva, size = struct.unpack_from('<II', d, opt + 96 + IMAGE_DIRECTORY_ENTRY_EXCEPTION * 8)
    if not rva or not size:
        return []
    off = img.offset(img.base + rva)
    out = []
    for i in range(size // 8):
        start, packed = struct.unpack_from('<2I', d, off + i * 8)
        if not start:
            continue
        out.append({
            'start': start,
            'prolog': packed & 0xFF,
            'instructions': (packed >> 8) & 0x3FFFFF,
            'thumb': not (packed >> 30) & 1,
            'handler': bool((packed >> 31) & 1),
        })
    out.sort(key=lambda f: f['start'])
    return out


def containing(table, va):
    for f in table:
        if f['start'] <= va < f['start'] + f['instructions'] * 4:
            return f
    return None


def main():
    path = sys.argv[1]
    table = functions(path)
    if len(sys.argv) > 2:
        va = int(sys.argv[2], 16)
        f = containing(table, va)
        if not f:
            print(f'{va:#010x} is not inside any function in .pdata')
            return
        end = f['start'] + f['instructions'] * 4
        print(f"{va:#010x} is in {f['start']:#010x}..{end:#010x} "
              f"({f['instructions']} instructions, prologue {f['prolog']})")
        return
    print(f'{len(table)} functions in .pdata')
    for f in table:
        end = f['start'] + f['instructions'] * 4
        print(f"  {f['start']:#010x}..{end:#010x}  {f['instructions']:6} instructions")


if __name__ == '__main__':
    main()
