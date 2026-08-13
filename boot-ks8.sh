#!/bin/bash
# Boot KeySoft 8.0 and let you type at it.
#
# The Linux counterpart of boot-ks8.bat. Everything except the firmware is made
# on the first run, so there is nothing to set up.
#
#     ./boot-ks8.sh                     firmware in roms/ beside this script
#     ROMS=/path/to/firmware ./boot-ks8.sh    firmware somewhere else
#
# This file must keep Unix line endings. The rest of the repository is CRLF,
# and a shell script with a CR on the end of its shebang fails to run at all,
# with an error naming an interpreter that does not exist.

set -u
cd "$(dirname "$0")"

ROMS=${ROMS:-roms}
EBOOT=$ROMS/EBOOT.bin
NK=$ROMS/NK.bin
CARD=work/card.img
EEPROM=work/SerialNumber.bin
EXE=target/release/vbnote

echo "VBNote - VoiceNote QT mPower, KeySoft 8.0"
echo

# ------------------------------------------------------------------- firmware
if [ ! -f "$EBOOT" ] || [ ! -f "$NK" ]; then
    echo "I cannot find the firmware."
    echo
    echo "This emulator does not ship it. HumanWare own it, and you supply your"
    echo "own copy. Put both of these in the roms folder beside this script:"
    echo
    echo "    $EBOOT"
    echo "    $NK"
    echo
    echo "Then run this again. If you keep them elsewhere, say where:"
    echo
    echo "    ROMS=/path/to/firmware $0"
    echo
    exit 1
fi

# ------------------------------------------------------------------ the build
if [ ! -x "$EXE" ]; then
    # rustup installs to ~/.cargo/bin, which is not on PATH unless its env file
    # has been sourced. A shell that has never done so has no cargo at all.
    if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
        . "$HOME/.cargo/env"
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        echo "There is no build yet and no cargo to make one with."
        echo "It needs a recent Rust toolchain; see the README."
        exit 1
    fi
    echo "Building. This takes a minute or two the first time, and prints a lot."
    echo
    if ! cargo build --release; then
        echo
        echo "The build failed. On Debian and Ubuntu it needs:"
        echo
        echo "    sudo apt install libasound2-dev libclang-dev libx11-dev \\"
        echo "        libxkbcommon-dev libspeechd-dev libwayland-dev"
        echo
        echo "libclang is the one that is not obvious: speech-dispatcher-sys"
        echo "runs bindgen, and without clang's own headers the error is"
        echo "'stddef.h' file not found, which reads like a broken system."
        exit 1
    fi
    echo
fi

mkdir -p work

# -------------------------------------------------------------- the SD card
# This is the Flash Disk. The emulator makes the image if it is not there, and
# Windows CE partitions and formats it during the first boot -- which takes
# longer than KeySoft waits, so the first boot always ends with it saying the
# flash disk is unavailable. Expected, and only once.
if [ ! -f "$CARD" ]; then
    echo "There is no SD card image yet, so this run will make one and format it."
    echo
    echo "KeySoft will say the flash disk is unavailable and stop. That is normal"
    echo "and it only happens once: formatting is still running when it asks. Let"
    echo "it get that far, quit, and run this again."
    echo
else
    echo "The card is ready, so this should come up properly."
    echo
fi

echo "A window will open. Keep it focused - on this platform the keyboard only"
echo "works while it is in front, and the window is there to catch keystrokes"
echo "rather than to be looked at. There is nothing to see."
echo
echo "  F11 with G    capture the keyboard, or give it back"
echo "  F11 with R    reset the machine"
echo "  F11 with Q    quit, saving the flash disk"
echo
echo "It starts released, so nothing you type reaches the machine until you"
echo "press F11 with G. It says which it is, out loud."
echo
echo "Expect about a minute and a half of near-silence, with a beep every five"
echo "seconds so you know it is alive, then music, then it asks which language"
echo "to use. Press Enter for English."
echo
echo "Quit with F11 and Q. Closing the window does not stop the machine: the"
echo "window is a separate thread, and the emulator carries on without it."
echo
echo "If READ chords seem to do nothing, suspect the window manager before the"
echo "emulator. READ is left Alt, and most window managers bind Alt themselves."
echo

# --cpu-mhz is the flag that decides how long any of this takes. It sets the
# clock the guest's timers run against, so a figure near what this emulator
# actually retires makes a guest second last about a real second. Leave it out
# and the default assumes a core far faster than this one, so every delay loop
# in the firmware burns four times the cycles waiting.
#
# 63 was measured on the Windows machine, where it held 73% of real time. This
# host free-runs at about 130%, so there may be headroom here -- but higher is
# only worth trying where the interpreter can actually retire it, and nobody
# has measured what the right figure is on Linux. To find out, run it by hand
# with --free-run --cycles 4000000000, time it, and pass the millions of cycles
# a second you get.
"$EXE" "$EBOOT" --flash --nk "$NK" \
    --cpu-mhz 63 \
    --sd-card "$CARD" \
    --serial-eeprom "$EEPROM" \
    --keyboard \
    --status work/status

echo
echo "Stopped."
