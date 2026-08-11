"""Map virtual addresses back to the ROM module they belong to.

Windows CE loads XIP ROM DLLs into slot 1 (0x02000000-0x04000000) at fixed
virtual addresses recorded in the image's table of contents. That makes any
address in a call trace attributable to a named module, which is the
difference between "some code in slot 1" and "bvdmain_serial.dll".

    python tools/modmap.py NK.bin                 # list every module
    python tools/modmap.py NK.bin 0x02322954 ...  # resolve addresses
"""

import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ceextract import load, modinfo


def module_table(path):
    m, mods, *_ = load(path)
    table = []
    for mod in mods:
        if not mod["name"]:
            continue
        try:
            info = modinfo(m, mod)
        except Exception:
            continue
        base = info["vbase"]
        size = info["vsize"]
        if base == 0:
            continue
        table.append((base, base + size, mod["name"]))
    table.sort()
    return table


def resolve(table, addr):
    for lo, hi, name in table:
        if lo <= addr < hi:
            return name, addr - lo
    return None, 0


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return
    path = sys.argv[1]
    table = module_table(path)

    if len(sys.argv) == 2:
        print(f"{len(table)} modules with a virtual base:\n")
        for lo, hi, name in table:
            print(f"  {lo:#010x}..{hi:#010x}  {name}")
        return

    for arg in sys.argv[2:]:
        addr = int(arg, 16) if arg.lower().startswith("0x") else int(arg, 16)
        name, off = resolve(table, addr)
        if name:
            print(f"  {addr:#010x}  {name}+{off:#x}")
        else:
            print(f"  {addr:#010x}  (not in any ROM module)")


if __name__ == "__main__":
    main()
