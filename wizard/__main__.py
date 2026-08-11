"""Provision a machine from the command line.

The wizard is the way most people will do this; this is the same work with the
progress printed instead of drawn, which is what makes it testable.

    python -m wizard --eboot roms/EBOOT.bin --nk roms/NK.bin
"""
from __future__ import annotations

import argparse
import os
import sys

from . import flashdisk, provision
from .provision import default_emulator


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Set up a VBNote machine.")
    p.add_argument("--eboot", required=True, help="the bootloader, EBOOT.bin")
    p.add_argument("--nk", required=True, help="the operating system, NK.bin")
    p.add_argument("--home", default=provision.HOME, help="where to put it")
    p.add_argument("--emulator", default=None, help="the vbnote binary")
    p.add_argument("--force", action="store_true",
                   help="provision even if this home already has a machine")
    args = p.parse_args(argv)

    maker = provision.Provisioner(
        args.emulator or default_emulator(), args.eboot, args.nk, args.home)

    if maker.already_done() and not args.force:
        print(f"{args.home} already has a machine in it; --force to redo it")
        return 0

    last = [-1]

    def report(progress: provision.Progress) -> None:
        percent = int(progress.fraction * 100)
        if percent != last[0]:
            last[0] = percent
            print(f"{percent:3d}%  {progress.message}", flush=True)

    try:
        maker.run(report)
    except provision.Failed as e:
        print(f"provisioning failed: {e}", file=sys.stderr)
        return 1
    print(f"\nready: {args.home}")
    print("  " + ", ".join(flashdisk.RootDirectory(maker.flash_disk).names()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
