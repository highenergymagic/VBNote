"""Recover KeySoft's message map: which handler answers which message id.

KeySoft dispatches everything through one indirect call, so nothing branches
to a handler and a decompiler never sees most of them. But every handler is
installed the same way -- `mov r1, #id`, `ldr r2, =handler`, `bl register` --
so scanning for the call and reading its operands back rebuilds the whole
table from the raw instructions.

    python tools/ksactions.py                     # every registration
    python tools/ksactions.py 0x1f0               # one message id
"""
import struct
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from armdis import Image, rotated

DEFAULT_PE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          'extracted', 'KeySoft.exe.pe')
#: The registration routine every message map calls.
REGISTER = 0x00012D04


def message_map(path=DEFAULT_PE, register=REGISTER):
    img = Image(path)
    text_rva, text_vs, text_rs, text_off = img.secs[0]
    lo = img.base + text_rva
    out = []
    for off in range(text_off, text_off + min(text_vs, text_rs) - 4, 4):
        w = struct.unpack_from('<I', img.d, off)[0]
        if (w & 0x0F000000) != 0x0B000000:      # BL, any condition
            continue
        o = w & 0xFFFFFF
        if o & 0x800000:
            o -= 0x1000000
        va = lo + (off - text_off)
        if va + 8 + o * 4 != register:
            continue
        ident = handler = None
        for back in range(1, 10):
            p = va - back * 4
            w2 = img.word(p)
            if w2 is None:
                break
            # mov r1, #imm, possibly followed by orr r1, r1, #imm
            if (w2 & 0x0FFFF000) == 0x03A01000 and ident is None:
                ident = rotated(w2)
            if (w2 & 0x0FFF1000) == 0x03811000 and ident is None:
                pass
            if handler is None and (w2 & 0x0F7FF000) in (0x051F2000, 0x059F2000):
                d = w2 & 0xFFF
                handler = img.word(p + 8 + (d if w2 & 0x800000 else -d))
        if ident is None:
            continue
        # An `orr r1, r1, #n` between the mov and the call adjusts the id.
        for fwd in range(1, 6):
            w2 = img.word(va - fwd * 4)
            if w2 is not None and (w2 & 0x0FFF0000) == 0x03811000:
                ident |= rotated(w2)
        out.append((ident, handler, va))
    out.sort(key=lambda t: (t[0], t[2]))
    return out


def main():
    want = int(sys.argv[1], 16) if len(sys.argv) > 1 else None
    table = message_map()
    print(f'{len(table)} handlers registered')
    for ident, handler, site in table:
        if want is not None and ident != want:
            continue
        h = f'{handler:#010x}' if handler else '?'
        print(f'  message {ident:#06x} ({ident:5})  handler {h}   installed at {site:#010x}')


if __name__ == '__main__':
    main()
