import sys, re, io
path = sys.argv[1]
minlen = int(sys.argv[2]) if len(sys.argv) > 2 else 6
pat = sys.argv[3] if len(sys.argv) > 3 else None
d = open(path,'rb').read()
rx = re.compile(rb'[ -~]{%d,}' % minlen)
seen = set()
out = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')
cre = re.compile(pat, re.I) if pat else None
for m in rx.finditer(d):
    s = m.group().decode('ascii')
    if cre and not cre.search(s): continue
    if s in seen: continue
    seen.add(s)
    out.write(f'{m.start():08x}  {s}\n')
# also UTF-16LE
rx2 = re.compile(rb'(?:[ -~]\x00){%d,}' % minlen)
for m in rx2.finditer(d):
    s = m.group().decode('utf-16-le')
    if cre and not cre.search(s): continue
    if ('W:'+s) in seen: continue
    seen.add('W:'+s)
    out.write(f'{m.start():08x} W {s}\n')
out.flush()
