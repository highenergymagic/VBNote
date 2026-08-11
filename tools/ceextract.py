"""Extract modules from a Windows CE ROM (.bin B000FF) image and rebuild them as PE files."""
import struct, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cebin import parse_bin, Mem

E32_ROM_FMT = '<HHIIHHIIII' + 'II'*4 + 'H'   # objcnt..subsys
O32_ROM_FMT = '<IIIIII'

def load(path):
    start, length, recs = parse_bin(path)
    m = Mem(recs)
    ptoc = m.u32(start+0x44)
    hdr = m.read(ptoc, 0x54)
    vals = struct.unpack_from('<IIIIIIIIIIIIIIII', hdr, 0)
    nummods, numfiles = vals[4], vals[12]
    physfirst, physlast = vals[2], vals[3]
    toc = ptoc + 0x54
    mods = []
    for i in range(nummods):
        e = m.read(toc + i*0x20, 0x20)
        attr, ftLo, ftHi, size, pname, e32off, o32off, loadoff = struct.unpack('<IIIIIIII', e)
        mods.append(dict(name=m.cstr(pname,128), size=size, attr=attr,
                         e32=e32off, o32=o32off, load=loadoff))
    return m, mods, ptoc, nummods, numfiles, toc

def modinfo(m, mod):
    e = m.read(mod['e32'], 0x100)
    (objcnt, imgflags, entryrva, vbase, ssmaj, ssmin, stackmax, vsize,
     s14rva, s14size, exprva, expsize, imprva, impsize, resrva, ressize,
     excrva, excsize, subsys) = struct.unpack_from(E32_ROM_FMT, e, 0)
    objs = []
    for i in range(objcnt):
        o = m.read(mod['o32'] + i*24, 24)
        vsz, rva, psz, dataptr, realaddr, flags = struct.unpack(O32_ROM_FMT, o)
        objs.append(dict(vsize=vsz, rva=rva, psize=psz, dataptr=dataptr,
                         realaddr=realaddr, flags=flags))
    return dict(objcnt=objcnt, imgflags=imgflags, entry=entryrva, vbase=vbase,
                vsize=vsize, stackmax=stackmax, subsys=subsys,
                dirs=dict(exp=(exprva,expsize), imp=(imprva,impsize),
                          res=(resrva,ressize), exc=(excrva,excsize)),
                objs=objs)

IMAGE_SCN_COMPRESSED = 0x00002000

def build_pe(m, mod, info, outpath):
    objs = info['objs']
    secalign, filealign = 0x1000, 0x1000
    nsec = len(objs)
    sizeofhdrs = (0x80 + 0xF8 + nsec*40 + filealign - 1) // filealign * filealign
    secdata, sections, notes = [], [], []
    real = [(o['rva'], o['rva']+o['vsize']) for o in objs if o['psize']]
    keep = []
    for o in objs:
        if not o['psize'] and any(s < o['rva']+o['vsize'] and o['rva'] < e for s,e in real):
            notes.append(f"dropped uninit section rva 0x{o['rva']:X} (overlaps initialized data)")
            continue
        keep.append(o)
    objs = keep
    nsec = len(objs)
    sizeofhdrs = (0x80 + 0xF8 + nsec*40 + filealign - 1) // filealign * filealign
    fileoff = sizeofhdrs
    for i, o in enumerate(objs):
        raw = b''
        if o['psize']:
            raw = m.read(o['dataptr'], o['psize']) or b''
            if o['flags'] & IMAGE_SCN_COMPRESSED:
                notes.append(f"section {i} COMPRESSED (psize {o['psize']} < vsize {o['vsize']})")
        rawsz = (len(raw) + filealign - 1)//filealign*filealign
        raw = raw.ljust(rawsz, b'\0')
        name = {0x60000020:'.text', 0xC0000040:'.data', 0x40000040:'.rdata',
                0xC0000080:'.bss', 0x40000040:'.rsrc'}.get(o['flags'], f'.sec{i}')
        sections.append((name.encode()[:8].ljust(8,b'\0'), o['vsize'], o['rva'],
                         rawsz if raw.strip(b'\0') else 0, fileoff if raw.strip(b'\0') else 0,
                         o['flags'] & 0xFFFFDFFF))
        secdata.append(raw if raw.strip(b'\0') else b'')
        if raw.strip(b'\0'):
            fileoff += rawsz
    dos = b'MZ' + b'\0'*58 + struct.pack('<I', 0x80) + b'\0'*(0x80-64)
    coff = struct.pack('<IHHIIIHH', 0x00004550, 0x01C2, nsec, 0, 0, 0, 0xE0, 0x0102|0x2000)
    imgsz = max((o['rva']+o['vsize']+secalign-1)//secalign*secalign for o in objs)
    opt = struct.pack('<HBBIIIIIIIIIHHHHHHIIIIHHIIIIII',
        0x10B, 6, 0, sum(s[1] for s in sections), 0, 0,
        info['entry'], objs[0]['rva'] if objs else 0x1000, 0,
        info['vbase'], secalign, filealign,
        4,0, 4,0, 4,0, 0, imgsz, sizeofhdrs, 0, info['subsys'] or 9, 0,
        info['stackmax'] or 0x10000, 0x1000, 0x10000, 0x1000, 0, 16)
    dirs = [(0,0)]*16
    dirs[0] = info['dirs']['exp']; dirs[1] = info['dirs']['imp']
    dirs[2] = info['dirs']['res']; dirs[3] = info['dirs']['exc']
    dirbytes = b''.join(struct.pack('<II', *d) for d in dirs)
    sectbl = b''.join(struct.pack('<8sIIIIIIHHI', n, vs, rva, rs, ro, 0,0,0,0, fl)
                      for n, vs, rva, rs, ro, fl in sections)
    hdr = dos + coff + opt + dirbytes + sectbl
    hdr = hdr.ljust(sizeofhdrs, b'\0')
    with open(outpath,'wb') as f:
        f.write(hdr)
        for d in secdata:
            if d: f.write(d)
    return notes

if __name__ == '__main__':
    img = sys.argv[1]
    m, mods, ptoc, nummods, numfiles, toc = load(img)
    want = sys.argv[2:] or ['nk.exe']
    outdir = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'extracted')
    os.makedirs(outdir, exist_ok=True)
    for mod in mods:
        if not mod['name']: continue
        if want != ['*'] and mod['name'].lower() not in [w.lower() for w in want]: continue
        info = modinfo(m, mod)
        print(f"\n=== {mod['name']}  ({mod['size']} bytes) ===")
        print(f"  vbase 0x{info['vbase']:08X}  entry rva 0x{info['entry']:08X}  vsize 0x{info['vsize']:X}  objs {info['objcnt']}")
        for i,o in enumerate(info['objs']):
            comp = ' COMPRESSED' if o['flags'] & IMAGE_SCN_COMPRESSED else ''
            print(f"   obj{i}: rva 0x{o['rva']:08X} vsize 0x{o['vsize']:07X} psize 0x{o['psize']:07X} "
                  f"data@0x{o['dataptr']:08X} flags 0x{o['flags']:08X}{comp}")
        out = os.path.join(outdir, mod['name'] + '.pe')
        notes = build_pe(m, mod, info, out)
        print(f"  -> {out}  ({os.path.getsize(out)} bytes)")
        for n in notes: print('   !', n)
