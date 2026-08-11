"""Find the PC-relative loads that reference a literal pool slot.

ARM builds any constant it cannot encode as an immediate by loading it from a
pool near the code. Going the other way -- from the pool slot back to the
instruction that reads it -- is how you find the comparison or the call that
uses a value, when searching for the value itself only finds the pool.

    python tools/literalrefs.py tools/extracted/nk.exe.pe 0x8007a908
"""
import struct
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from armdis import Image, dis


def refs(img, target):
    out = []
    for rva, vs, rs, ro in img.secs:
        lo = img.base + rva
        for off in range(ro, ro + min(vs, rs) - 4, 4):
            w = struct.unpack_from('<I', img.d, off)[0]
            # ldr rD, [pc, #imm12], up or down
            if (w & 0x0F7F0000) not in (0x051F0000, 0x059F0000):
                continue
            va = lo + (off - ro)
            d = w & 0xFFF
            if va + 8 + (d if w & 0x800000 else -d) == target:
                out.append((va, w))
    return out


def main():
    img = Image(sys.argv[1])
    target = int(sys.argv[2], 16)
    found = refs(img, target)
    print(f'{len(found)} PC-relative loads reference {target:#010x}')
    for va, w in found:
        print(f'  {va:#010x}  {w:08x}  {dis(img, va, w)}')


if __name__ == '__main__':
    main()
