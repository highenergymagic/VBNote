"""Pull RT_STRING resources out of a Windows CE PE and print string IDs.

Win32 string tables hold 16 strings per resource block; the resource id is
(stringId >> 4) + 1 and the string is the (stringId & 15)th length-prefixed
UTF-16 entry inside it.
"""
import struct, sys

path = sys.argv[1]
want = sys.argv[2] if len(sys.argv) > 2 else None
d = open(path, 'rb').read()
pe = struct.unpack_from('<I', d, 0x3c)[0]
nsec = struct.unpack_from('<H', d, pe + 6)[0]
optsz = struct.unpack_from('<H', d, pe + 20)[0]
opt = pe + 24
secs = []
for i in range(nsec):
    o = opt + optsz + i * 40
    vs, rva, rs, ro = struct.unpack_from('<IIII', d, o + 8)
    secs.append((rva, vs, rs, ro))

def off(rva):
    for srva, vs, rs, ro in secs:
        if srva <= rva < srva + max(vs, rs):
            return ro + (rva - srva)
    return None

# The resource directory is the section whose head parses as one. The big
# .rsrc here is the last section.
res_rva = max(s[0] for s in secs)
res_off = off(res_rva)

def entries(dir_off):
    named, ids = struct.unpack_from('<HH', d, dir_off + 12)
    out = []
    for i in range(named + ids):
        e = dir_off + 16 + i * 8
        name, offset = struct.unpack_from('<II', d, e)
        out.append((name, offset))
    return out

blocks = {}
for name, offset in entries(res_off):
    if name & 0x80000000 or name != 6:      # RT_STRING
        continue
    for bid, boff in entries(res_off + (offset & 0x7FFFFFFF)):
        for _lang, loff in entries(res_off + (boff & 0x7FFFFFFF)):
            data_rva, size = struct.unpack_from('<II', d, res_off + (loff & 0x7FFFFFFF))
            blocks[bid & 0xFFFF] = (off(data_rva), size)

print(f'{len(blocks)} RT_STRING blocks')
hits = []
for bid in sorted(blocks):
    o, size = blocks[bid]
    p, end = o, o + size
    for idx in range(16):
        if p + 2 > end:
            break
        n = struct.unpack_from('<H', d, p)[0]
        s = d[p + 2:p + 2 + 2 * n].decode('utf-16le', 'replace')
        sid = (bid - 1) * 16 + idx
        if want and want.lower() in s.lower():
            hits.append((sid, bid, idx, p + 2, s))
        p += 2 + 2 * n
for sid, bid, idx, at, s in hits:
    print(f'  string id {sid} (0x{sid:x})  block {bid} index {idx}  at {at:#x}')
    print(f'    {s!r}')
