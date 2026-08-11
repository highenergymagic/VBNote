"""Disassemble a handful of ARM forms in an extracted Windows CE PE.

Not a real disassembler: enough to read a call site, follow a PC-relative
literal, and recover a constant that a decompiler missed. Ghidra only finds
functions something branches to, so code reached solely through a pointer --
which is most of KeySoft's message handlers -- never appears in its output at
all, and the constants inside are invisible to any search of the source.

    python tools/armdis.py tools/extracted/KeySoft.exe.pe 0x00023334 12
"""
import struct
import sys

REG = [f'r{i}' for i in range(13)] + ['sp', 'lr', 'pc']
COND = ['eq', 'ne', 'cs', 'cc', 'mi', 'pl', 'vs', 'vc',
        'hi', 'ls', 'ge', 'lt', 'gt', 'le', '', 'nv']


class Image:
    """An extracted PE, addressed by the virtual addresses it runs at."""

    def __init__(self, path):
        self.d = d = open(path, 'rb').read()
        pe = struct.unpack_from('<I', d, 0x3C)[0]
        nsec = struct.unpack_from('<H', d, pe + 6)[0]
        optsz = struct.unpack_from('<H', d, pe + 20)[0]
        opt = pe + 24
        self.base = struct.unpack_from('<I', d, opt + 28)[0]
        self.secs = []
        for i in range(nsec):
            o = opt + optsz + i * 40
            vs, rva, rs, ro = struct.unpack_from('<IIII', d, o + 8)
            self.secs.append((rva, vs, rs, ro))

    def offset(self, va):
        rva = va - self.base
        for srva, vs, rs, ro in self.secs:
            if srva <= rva < srva + max(vs, rs):
                off = ro + (rva - srva)
                if off + 4 <= len(self.d):
                    return off
        return None

    def word(self, va):
        o = self.offset(va)
        return struct.unpack_from('<I', self.d, o)[0] if o is not None else None


def rotated(w):
    rot = (w >> 8 & 0xF) * 2
    v = w & 0xFF
    return ((v >> rot) | (v << (32 - rot))) & 0xFFFFFFFF if rot else v


DP = ['and', 'eor', 'sub', 'rsb', 'add', 'adc', 'sbc', 'rsc',
      'tst', 'teq', 'cmp', 'cmn', 'orr', 'mov', 'bic', 'mvn']
#: Opcodes that only set flags, so they have no destination register.
DP_NO_RD = {8, 9, 10, 11}
#: Opcodes that take no first operand register.
DP_NO_RN = {13, 15}


def shifted(w):
    """The register operand of a data-processing instruction."""
    rm = REG[w & 0xF]
    if w & 0x10:                       # shift amount in a register
        return f'{rm}, {["lsl", "lsr", "asr", "ror"][w >> 5 & 3]} {REG[w >> 8 & 0xF]}'
    amount = w >> 7 & 0x1F
    kind = w >> 5 & 3
    if amount == 0:
        if kind == 0:
            return rm
        if kind == 3:
            return f'{rm}, rrx'
        amount = 32                    # lsr #0 and asr #0 mean 32
    return f'{rm}, {["lsl", "lsr", "asr", "ror"][kind]} #{amount}'


def reglist(w):
    names = [REG[i] for i in range(16) if w >> i & 1]
    return '{' + ', '.join(names) + '}'


def dis(img, va, w):
    c = COND[w >> 28 & 0xF]
    if (w & 0x0E000000) == 0x0A000000:
        off = w & 0xFFFFFF
        if off & 0x800000:
            off -= 0x1000000
        return f"b{'l' if w & 0x1000000 else ''}{c} {va + 8 + off * 4:#010x}"
    # Block transfer, which is how ARM does push and pop.
    if (w & 0x0E000000) == 0x08000000:
        load = w & 0x100000
        rn = REG[w >> 16 & 0xF]
        mode = {(1, 1): 'ib', (1, 0): 'ia', (0, 1): 'db', (0, 0): 'da'}[
            (bool(w & 0x1000000), bool(w & 0x800000))]
        bang = '!' if w & 0x200000 else ''
        hat = '^' if w & 0x400000 else ''
        return f"{'ldm' if load else 'stm'}{c}{mode} {rn}{bang}, {reglist(w & 0xFFFF)}{hat}"
    # Halfword and signed byte transfer.
    if (w & 0x0E000090) == 0x00000090 and (w & 0x60) != 0:
        load = w & 0x100000
        rd, rn = REG[w >> 12 & 0xF], REG[w >> 16 & 0xF]
        kind = {1: 'h', 2: 'sb', 3: 'sh'}[w >> 5 & 3]
        if w & 0x400000:
            o = ((w >> 4) & 0xF0) | (w & 0xF)
            operand = f'#{o:#x}' if o else ''
        else:
            operand = REG[w & 0xF]
        sign = '' if w & 0x800000 else '-'
        tail = f', {sign}{operand}' if operand else ''
        return f"{'ldr' if load else 'str'}{c}{kind} {rd}, [{rn}{tail}]"
    if (w & 0x0FFFFFF0) == 0x012FFF10:
        return f"bx{c} {REG[w & 0xF]}"
    if (w & 0x0FFFFFF0) == 0x012FFF30:
        return f"blx{c} {REG[w & 0xF]}"
    # Single data transfer.
    if (w & 0x0C000000) == 0x04000000:
        load = w & 0x100000
        rn, rd, o = REG[w >> 16 & 0xF], REG[w >> 12 & 0xF], w & 0xFFF
        b = 'b' if w & 0x400000 else ''
        name = f"{'ldr' if load else 'str'}{c}{b}"
        if w & 0x2000000:
            return f"{name} {rd}, [{rn}, {'' if w & 0x800000 else '-'}{shifted(w)}]"
        if rn == 'pc' and load:
            lit = va + 8 + (o if w & 0x800000 else -o)
            val = img.word(lit)
            shown = f'{val:#010x}' if val is not None else '?'
            return f"{name} {rd}, [pc, #{o:#x}]   ; {lit:#010x} = {shown}"
        pre = bool(w & 0x1000000)
        bang = '!' if w & 0x200000 else ''
        sign = '' if w & 0x800000 else '-'
        if pre:
            inner = f"[{rn}, #{sign}{o:#x}]" if o else f"[{rn}]"
            return f"{name} {rd}, {inner}{bang}"
        return f"{name} {rd}, [{rn}], #{sign}{o:#x}"
    # Data processing, immediate or register operand.
    if (w & 0x0C000000) == 0x00000000:
        op = w >> 21 & 0xF
        name = DP[op] + c + ('s' if w & 0x100000 and op not in DP_NO_RD else '')
        operand = f'#{rotated(w):#x}' if w & 0x2000000 else shifted(w)
        rd, rn = REG[w >> 12 & 0xF], REG[w >> 16 & 0xF]
        if op in DP_NO_RD:
            return f'{name} {rn}, {operand}'
        if op in DP_NO_RN:
            return f'{name} {rd}, {operand}'
        return f'{name} {rd}, {rn}, {operand}'
    return f".word {w:#010x}"


def main():
    img = Image(sys.argv[1])
    start = int(sys.argv[2], 16)
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 24
    for i in range(count):
        va = start + i * 4
        w = img.word(va)
        if w is None:
            print(f'{va:#010x}  (not mapped)')
            continue
        print(f'{va:#010x}  {w:08x}  {dis(img, va, w)}')


if __name__ == '__main__':
    main()
