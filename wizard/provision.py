"""Setting up a machine, without a user having to see any of it.

This is the part of the wizard that does the work, kept free of any user
interface so that it can be run and tested on its own. The wizard on top of it
draws a progress bar and nothing more.

# What provisioning actually involves

Three files end up in the home directory, and only one of them is built
directly:

* `KeysoftSystemDisk.img` -- the NOR flash, with the bootloader, the image
  header and the operating system laid out the way a factory-flashed machine
  has them. Written once and never written again; the emulator loads it and
  does not save it back.
* `onewire.img` -- the 1-Wire part, holding the machine's identity.
* `FlashDisk.img` -- the card. This one cannot be built, only *grown*: it
  starts as an empty file, and Windows CE partitions and formats it during the
  first boot, and KeySoft lays out its folders during the second.

That last point is the whole reason this takes minutes rather than seconds.
There is no way to hand the machine a finished flash disk; it has to make one,
and it will not make one until it has been through its own first-run
questions -- preferred braille grade, thumb keys, the clock, the modem's
country. A brand new card that is left alone sits on the first question for
ever and the folders never appear.

So this answers them, by pressing Enter, which takes every default. They are
all changeable afterwards on the machine itself, and a user who cannot see the
screen should not have to answer a dozen questions before the machine will
start at all.

# Knowing when it is finished

This is the part that took the measuring, because the obvious answers are all
wrong.

`General` appearing on the flash disk is not it. It turns up *early* --
after about two answers, with questions still to come -- so a machine stopped
there comes back up asking for its operating language. The folder says the
card is working, not that the machine is set up.

Silence is not it either. The emulator writes one sound file per burst of
speech, so the machine can be listened to without hearing it, but a gap in
speech means nothing on its own: mid-setup, the machine takes **thirty
seconds** to answer one question and ask the next. A pause and a finished
setup look exactly alike from outside, and telling them apart would mean
recognising the words.

What settles it is that **overanswering is harmless**. Past the last question
the machine is at its main menu, and further Enters walk into the word
processor and sit on its "document to create" prompt, which creates nothing.
Measured: a machine given forty answers, most of them into that prompt, boots
afterwards straight to the main menu with no questions.

So this does not try to detect the end. It answers a generous fixed number of
times, which is several more than the machine has ever needed, stops, lets it
go quiet, and then checks that the flash disk really was set up. Predictable,
and wrong only in the direction that does no harm.
"""
from __future__ import annotations

import os
import subprocess
import time
from dataclasses import dataclass

from . import flashdisk

#: The settings file, as the emulator ships it. Kept here as well so that a
#: freshly provisioned machine has one before it is ever started.
SETTINGS = """# VBNote settings.
#
# One setting per line, as `name = value`. Lines starting with # or ; are
# comments. Delete this file to get it back with the defaults.

# How fast the emulated processor is clocked, in MHz.
#
# This is not a speed dial. It is how much work the emulator promises to do
# per second of the machine's time, and promising more than this computer can
# deliver does not make the machine faster -- it makes it stutter, because the
# machine produces its speech in its own time and the sound card drains it in
# real time. 63 is measured to keep up on an ordinary machine. If speech
# breaks up, lower it. Raising it is unlikely to help.
cpu_mhz = 63

# Longest a key is held down, in milliseconds, if the machine does not look at
# the keyboard in the meantime. Only a backstop; keys are normally released as
# soon as the machine has seen them.
key_hold_ms = 800

# Turn the sound off entirely. yes or no.
mute = no

# Show a terminal window with a running commentary, and write a status file
# beside this one. Useful when reporting a problem; off otherwise, because a
# terminal nobody asked for is one more window to get lost in. yes or no.
debug = no
"""

#: Where a provisioned machine lives.
HOME = os.path.join(os.path.expanduser("~"), ".VBNote")

SYSTEM_DISK = "KeysoftSystemDisk.img"
FLASH_DISK = "FlashDisk.img"
ONEWIRE = "onewire.img"
STATUS = "provision.status"

#: The guest clock, in MHz. Not the real machine's 312: this one is chosen so
#: that a second of the machine's time takes about a second of ours, which is
#: what keeps its speech from breaking up. `CLAUDE.md` has the measurements.
CPU_MHZ = 63

#: How long to allow in total. A provision is two boots and a dozen answers,
#: and takes a few minutes; this is only here so that a machine that has
#: genuinely stopped does not wait for ever.
PATIENCE_SECONDS = 30 * 60

#: How long to let a burst of speech finish before answering it. The sound
#: file appears as the machine starts talking, not as it stops.
SETTLE_SECONDS = 2.5

#: How long the machine must be quiet, after the last answer, before its flash
#: disk is inspected. Long enough to be past the thirty-second pauses it takes
#: while it is still working.
QUIET_SECONDS = 60.0

#: How many times to press Enter. A fresh machine has needed three; this is
#: several times that, and the extra presses land harmlessly on a prompt that
#: creates nothing.
ANSWERS = 10

#: Guest seconds a boot takes before the machine says anything. Used only to
#: turn "still booting" into a moving progress bar.
BOOT_SECONDS = 95.0

#: Longer than this, and what the machine just said was an announcement rather
#: than a question. Measured across provisioning runs: the startup sound is
#: about five seconds and the version announcement about the same, while the
#: questions are one and a half to two and a half.
#:
#: This is the only thing available. Nothing here can hear *words* -- there is
#: no speech recognition in the wizard and there is not going to be -- but the
#: emulator writes one sound file per burst of speech, and how long each one is
#: is a fact that can be read off the file. It is enough to say something
#: truthful about what is happening.
LONG_UTTERANCE = 3.5


def default_emulator() -> str:
    """The emulator beside this checkout, or whatever is on the path."""
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    for candidate in (
        os.path.join(here, "target", "release", "vbnote.exe"),
        os.path.join(here, "target", "release", "vbnote"),
        os.path.join(here, "vbnote.exe"),
        os.path.join(here, "vbnote"),
    ):
        if os.path.exists(candidate):
            return candidate
    return "vbnote"


class Failed(Exception):
    """Provisioning did not finish."""


@dataclass
class Progress:
    """Where provisioning has got to."""

    fraction: float
    message: str


class Provisioner:
    """Builds a machine in `home` from the firmware the user supplied."""

    def __init__(
        self,
        emulator: str,
        eboot: str,
        kernel: str,
        home: str = HOME,
        usb_disk_mb: int = 256,
    ):
        self.emulator = emulator
        self.eboot = eboot
        self.kernel = kernel
        self.home = home
        self.usb_disk_mb = usb_disk_mb

    # -- the three files ------------------------------------------------
    @property
    def system_disk(self) -> str:
        return os.path.join(self.home, SYSTEM_DISK)

    @property
    def flash_disk(self) -> str:
        return os.path.join(self.home, FLASH_DISK)

    @property
    def onewire(self) -> str:
        return os.path.join(self.home, ONEWIRE)

    @property
    def status(self) -> str:
        return os.path.join(self.home, STATUS)

    @property
    def speech(self) -> str:
        """Where the emulator drops one sound file per burst of speech."""
        return os.path.join(self.home, "speech")

    def already_done(self) -> bool:
        """Whether a finished machine is already here."""
        return (
            os.path.exists(self.system_disk)
            and os.path.exists(self.onewire)
            and flashdisk.is_ready(self.flash_disk)
        )

    # -- doing it -------------------------------------------------------
    def run(self, report) -> None:
        """Provision, calling `report(Progress)` as it goes.

        Raises `Failed` if it does not finish. Everything it makes is inside
        `home`, so a failed attempt can simply be deleted and tried again.
        """
        os.makedirs(self.home, exist_ok=True)
        self._clear_leftovers()
        os.makedirs(self.speech, exist_ok=True)

        report(Progress(0.02, "Preparing"))
        # One run does both halves: it writes the system disk out and then
        # boots from what it just built, which is what gets the card
        # partitioned, formatted and filled.
        keys = os.path.join(self.home, "keys.txt")
        command = [
            self.emulator, self.eboot,
            "--flash", "--nk", self.kernel,
            "--provision", self.system_disk,
            "--sd-card", self.flash_disk,
            "--serial-eeprom", self.onewire,
            "--cpu-mhz", str(CPU_MHZ),
            "--mute",
            "--keys-from", keys,
            "--utterances", self.speech,
            "--utterance-gap", "2.5",
            "--status", self.status,
        ]
        machine = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
        )
        try:
            self._answer_until_it_stops_asking(machine, keys, report)
        finally:
            self._shut_down(machine)
        if not flashdisk.is_ready(self.flash_disk):
            raise Failed("the machine did not finish setting up its flash disk")
        self._write_settings()
        report(Progress(1.0, "Ready"))

    def _write_settings(self) -> None:
        """Leave a settings file, so there is one to find and edit.

        The emulator writes its own if there is none, but a user told "the
        settings are in VBNote.ini" should find VBNote.ini, not be told to
        start the machine once to make it appear.
        """
        path = os.path.join(self.home, "VBNote.ini")
        if os.path.exists(path):
            return
        try:
            with open(path, "w", newline="") as f:
                text = SETTINGS
                if "usb_disk_mb" not in text:
                    # This copy of the defaults predates the setting. Adding it
                    # here rather than editing the block above keeps the two
                    # from drifting further apart: the emulator's own copy in
                    # home.rs is the one with the explanation.
                    text += "\n".join(
                        [
                            "",
                            "# How big to make the removable drive, in megabytes, the",
                            "# first time it is made. Delete UsbDrive.vhd to have a new",
                            "# one made at a new size.",
                            f"usb_disk_mb = {self.usb_disk_mb}",
                            "",
                        ]
                    )
                else:
                    text = text.replace(
                        "usb_disk_mb = 256", f"usb_disk_mb = {self.usb_disk_mb}"
                    )
                f.write(text)
        except OSError:
            pass
        self._make_transfer_folder()

    def _make_transfer_folder(self) -> None:
        """Put the transfer folder there before anybody looks for it.

        The emulator makes it too, on its first run -- but somebody who has
        just been told where to put their files should be able to go and put
        them there, rather than being told to start the machine once first so
        that the folder appears.
        """
        home = os.environ.get("USERPROFILE") or os.path.expanduser("~")
        try:
            os.makedirs(os.path.join(home, "Documents", "VBNote USB Drive"), exist_ok=True)
        except OSError:
            pass

    # -- the waiting ----------------------------------------------------
    def _answer_until_it_stops_asking(self, machine, keys: str, report) -> None:
        started = time.monotonic()
        spoken = 0
        answered = 0
        last_spoke = None
        pending = None
        said = ""
        while True:
            if machine.poll() is not None:
                raise Failed("the machine stopped before it had finished")
            now = time.monotonic()
            if now - started > PATIENCE_SECONDS:
                raise Failed("the machine took too long")

            heard = self._utterances()
            if len(heard) > spoken:
                # Let the file finish being written before measuring it.
                time.sleep(SETTLE_SECONDS)
                said = self._describe(heard[-1], first=(spoken == 0))
                spoken = len(heard)
                last_spoke = time.monotonic()
                now = last_spoke
                if answered < ANSWERS:
                    pending = now

            if last_spoke is None:
                # Still booting: nothing has been said yet. Guest time is a
                # fair guide to this part and to nothing after it.
                guest = self._guest_seconds()
                report(Progress(
                    0.05 + 0.30 * min(1.0, guest / BOOT_SECONDS),
                    f"Starting the machine for the first time "
                    f"({int(guest)} seconds of machine time so far)",
                ))
            elif answered < ANSWERS:
                report(Progress(
                    0.35 + 0.50 * (answered / ANSWERS),
                    f"{said}. Answering it, {answered + 1} of {ANSWERS}",
                ))
                if pending is not None and now >= pending:
                    pending = None
                    answered += 1
                    self._press_enter(keys)
            else:
                # Answered enough. Stop poking it and let it come to rest.
                quiet = now - last_spoke
                report(Progress(
                    0.85 + 0.10 * min(1.0, quiet / QUIET_SECONDS),
                    f"Finished answering. Waiting for the machine to go quiet "
                    f"({int(quiet)} of {int(QUIET_SECONDS)} seconds)",
                ))
                if quiet >= QUIET_SECONDS:
                    return
            time.sleep(0.5)

    def _utterances(self) -> list[str]:
        """The sound files, oldest first, one per burst of speech."""
        try:
            return sorted(os.listdir(self.speech))
        except OSError:
            return []

    def _seconds(self, name: str) -> float:
        """How long one burst of speech lasted.

        From the WAV header rather than the file size, because the header
        carries the rate and guessing it would make every duration wrong on a
        machine that changed it.
        """
        try:
            with open(os.path.join(self.speech, name), "rb") as f:
                head = f.read(44)
            if len(head) < 44 or head[:4] != b"RIFF":
                return 0.0
            rate_bytes = int.from_bytes(head[28:32], "little")
            data = int.from_bytes(head[40:44], "little")
            return data / rate_bytes if rate_bytes else 0.0
        except OSError:
            return 0.0

    def _describe(self, name: str, first: bool) -> str:
        """What the machine most likely just did, from how long it took.

        Deliberately hedged. Length tells announcements from questions and
        nothing finer, so this does not pretend to know which question.
        """
        seconds = self._seconds(name)
        if first:
            return f"The machine started up and played its chime ({seconds:.1f} seconds)"
        if seconds >= LONG_UTTERANCE:
            return f"The machine made an announcement ({seconds:.1f} seconds)"
        return f"The machine asked a question ({seconds:.1f} seconds)"

    def _press_enter(self, keys: str) -> None:
        """Take the default answer to whatever the machine just asked.

        Written to a temporary name and moved into place, because the emulator
        deletes the file as it reads it and a half-written one would be read
        as half a keystroke.
        """
        part = keys + ".part"
        with open(part, "w") as f:
            f.write("\n")
        os.replace(part, keys)

    def _guest_seconds(self) -> float:
        try:
            with open(self.status) as f:
                for line in f:
                    if line.startswith("guest_seconds"):
                        return float(line.split()[1])
        except (OSError, ValueError, IndexError):
            pass
        return 0.0

    def _shut_down(self, machine) -> None:
        """Ask for a clean stop, which is what flushes the card."""
        if machine.poll() is None:
            try:
                open(self.status + ".stop", "w").close()
            except OSError:
                pass
            try:
                machine.wait(timeout=60)
            except subprocess.TimeoutExpired:
                machine.kill()

    def _clear_leftovers(self) -> None:
        for name in (self.status, self.status + ".stop",
                     os.path.join(self.home, "keys.txt")):
            try:
                os.remove(name)
            except OSError:
                pass
        # The speech is only ever used to count questions, and counting starts
        # from nothing.
        try:
            for f in os.listdir(self.speech):
                os.remove(os.path.join(self.speech, f))
        except OSError:
            pass
