import struct, sys, os

def parse_bin(path):
    d = open(path,'rb').read()
    assert d[:7] == b'B000FF\n', d[:8]
    start, length = struct.unpack_from('<II', d, 7)
    off = 15
    recs = []
    while off + 12 <= len(d):
        addr, ln, chk = struct.unpack_from('<III', d, off)
        off += 12
        if addr == 0 and (ln == 0 or off + ln > len(d)):
            # final record: addr=0, one of the remaining fields is the launch address
            recs.append(('EOF', ln or chk, 0, 0))
            break
        data = d[off:off+ln]
        off += ln
        recs.append((data, addr, ln, chk))
    return start, length, recs

def build_mem(recs):
    mem = {}
    for r in recs:
        if r[0] == 'EOF': continue
        data, addr, ln, chk = r
        mem[addr] = data
    return mem

class Mem:
    def __init__(self, recs):
        self.segs = []
        for r in recs:
            if r[0]=='EOF': continue
            data, addr, ln, chk = r
            self.segs.append((addr, addr+ln, data))
        self.segs.sort()
    def read(self, va, n):
        """Read n bytes. None if the address is not in the image at all.

        A section's psize is rounded up to a file alignment the ROM does not
        bother to store, so the last record of a module can stop a couple of
        hundred bytes short of what the header asks for. Zero-fill that tail
        rather than failing: dropping the whole section over its padding is
        how KeySoft.exe came out of here as a 480 KB stub of a 2.4 MB module.
        """
        out = b''
        while n > 0:
            for s,e,data in self.segs:
                if s <= va < e:
                    take = min(n, e-va)
                    out += data[va-s:va-s+take]
                    va += take; n -= take
                    break
            else:
                if not out:
                    return None
                return out + b'\0' * n
        return out
    def u32(self, va):
        b = self.read(va,4)
        return None if b is None else struct.unpack('<I', b)[0]
    def cstr(self, va, maxn=256):
        b = self.read(va, maxn)
        if b is None: return None
        i = b.find(b'\0')
        return b[:i if i>=0 else maxn].decode('ascii','replace')
    def wstr(self, va, maxn=256):
        b = self.read(va, maxn*2)
        if b is None: return None
        s = b.decode('utf-16-le','replace')
        i = s.find('\0')
        return s[:i if i>=0 else maxn]

def main(path):
    start, length, recs = parse_bin(path)
    print(f'== {os.path.basename(path)} ==')
    print(f'image start 0x{start:08X}  length 0x{length:08X} ({length/1048576:.1f} MB)  records={len(recs)}')
    for r in recs[:8]:
        if r[0]=='EOF':
            print('  EOF record  start-addr=0x%08X'%r[1]); continue
        print(f'  rec addr=0x{r[1]:08X} len=0x{r[2]:08X}')
    if len(recs)>8: print(f'  ... {len(recs)-8} more')
    m = Mem(recs)
    # ROM signature at image start + 0x40
    sig = m.u32(start+0x40)
    ptoc = m.u32(start+0x44)
    print(f'ROM sig @0x{start+0x40:08X} = 0x{sig:08X} ({struct.pack("<I",sig)})  pTOC=0x{ptoc:08X}')
    if sig != 0x43454345:
        print('  (no CECE/ECEC signature here)'); return
    # ROMHDR
    f = '<IIIIIIIIIIIIIIIIIIHHIIIII'
    hdr = m.read(ptoc, 0x100)
    if hdr is None:
        print('  pTOC not in image'); return
    (dllfirst, dlllast, physfirst, physlast, nummods, ulRAMStart, ulRAMFree,
     ulRAMEnd, ulCopyEntries, ulCopyOffset, ulProfileLen, ulProfileOffset,
     numfiles, ulKernelFlags, ulFSRamPercent, ulDrivglobRef, usCPUType,
     usMiscFlags, pExtensions, ulTrackingStart, ulTrackingLen) = struct.unpack_from(
        '<IIIIIIIIIIIIIIIIHHIII', hdr, 0)
    cpu = {0x01c0:'ARM (0x1C0)',0x01c2:'ARM/Thumb (0x1C2)',0x014c:'x86 i386',
           0x0166:'MIPS R4000',0x01a2:'SH3',0x01a6:'SH4'}.get(usCPUType, hex(usCPUType))
    print(f'''ROMHDR:
  dllfirst      0x{dllfirst:08X}   dlllast   0x{dlllast:08X}
  physfirst     0x{physfirst:08X}   physlast  0x{physlast:08X}  ({(physlast-physfirst)/1048576:.1f} MB)
  nummods       {nummods}          numfiles  {numfiles}
  RAMStart      0x{ulRAMStart:08X}   RAMFree   0x{ulRAMFree:08X}   RAMEnd 0x{ulRAMEnd:08X}
  copyentries   {ulCopyEntries} @0x{ulCopyOffset:08X}
  kernelflags   0x{ulKernelFlags:08X}   FSRamPercent 0x{ulFSRamPercent:08X}
  CPU           {cpu}   miscflags 0x{usMiscFlags:04X}
  pExtensions   0x{pExtensions:08X}''')
    # TOCentry array follows ROMHDR (size 0x54)
    toc = ptoc + 0x54
    mods = []
    for i in range(nummods):
        e = m.read(toc + i*0x20, 0x20)
        if e is None: break
        dwFileAttributes, ftLo, ftHi, nFileSize, lpszFileName, ulE32Off, ulO32Off, ulLoadOff = struct.unpack('<IIIIIIII', e)
        name = m.cstr(lpszFileName, 128)
        mods.append((name, nFileSize, dwFileAttributes))
    print(f'\n-- {len(mods)} modules --')
    for n,s,a in mods:
        print(f'  {str(n):<32} {s:>9}')
    # FILES entries follow modules (FILESentry = 0x18 too)
    ftoc = toc + nummods*0x20
    files = []
    for i in range(numfiles):
        e = m.read(ftoc + i*0x18, 0x18)
        if e is None: break
        attr, ftLo, ftHi, nRealFileSize, nCompFileSize, lpszFileName = struct.unpack('<IIIIII', e)
        files.append((m.cstr(lpszFileName,128), nRealFileSize, nCompFileSize))
    print(f'\n-- {len(files)} files --')
    for n,r,c in files:
        print(f'  {str(n):<40} {r:>9} (comp {c})')

if __name__ == '__main__':
    for p in sys.argv[1:]:
        main(p); print()
