"""Recognising the firmware, and saying so when it is not recognised.

VBNote has been developed and tested against one build of the machine's
firmware. Anything else may work perfectly, may work oddly, or may not boot --
there is no way to know without trying it, and this project has no way to
obtain other builds to try.

So the files are checked, and the answer is used to *inform* rather than to
refuse. Somebody with a different machine and their own firmware is exactly
the person this software is for, and telling them "no" would be absurd. What
they get is an honest warning: this is not the build we know, here is what to
expect, do you want to go on.
"""
from __future__ import annotations

import hashlib
import os

#: The build VBNote is developed and tested against: KeySoft 8.0, as it ships
#: on a BrailleNote mPower.
KNOWN = {
    "eboot": "6d76224e533e0e23ad3d00f7f7b1adb507c9e85660b92b01153280c45e36f08b",
    "kernel": "2eee7ba72800ec36049ed0722e33ae6c21fca67c342af2115ab9429fc593bfc4",
}

#: What each one is called when talking to a person.
NAMES = {"eboot": "bootloader (EBOOT.bin)", "kernel": "operating system (NK.bin)"}


def sha256(path: str) -> str:
    """The hash of a file, read in pieces because NK.bin is fifty megabytes."""
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def check(which: str, path: str) -> tuple[bool, str]:
    """Whether this is the known build, and its hash.

    A file that cannot be read is not the known build, and the reason comes
    back in place of the hash.
    """
    try:
        got = sha256(path)
    except OSError as e:
        return False, f"could not be read ({e})"
    return got == KNOWN[which], got


def describe(unknown: list[tuple[str, str]]) -> str:
    """The warning to show, given what did not match.

    Names the files, gives the hashes, and says plainly what is and is not
    known -- so that somebody reporting a problem afterwards has the number to
    quote.
    """
    lines = [
        "These firmware files are not the build VBNote was tested against.",
        "",
    ]
    for which, got in unknown:
        lines.append(f"{NAMES[which]}:")
        lines.append(f"    this file:  {got}")
        lines.append(f"    tested:     {KNOWN[which]}")
        lines.append("")
    lines += [
        "That does not mean they are wrong. VBNote is tested against one "
        "build, KeySoft 8.0, because that is the one available to test with. "
        "Another build may work perfectly well.",
        "",
        "It does mean nobody has tried this one, so if the machine behaves "
        "oddly, this is worth mentioning when you report it.",
        "",
        "Do you want to go on?",
    ]
    return "\n".join(lines)


def unknown_files(eboot: str, kernel: str) -> list[tuple[str, str]]:
    """Which of the two are not the known build, with their hashes."""
    out = []
    for which, path in (("eboot", eboot), ("kernel", kernel)):
        if not path or not os.path.exists(path):
            continue
        matched, got = check(which, path)
        if not matched:
            out.append((which, got))
    return out
