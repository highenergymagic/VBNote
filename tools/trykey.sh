#!/usr/bin/env bash
# Try one licence blob against KeySoft's own decryptor.
#
# Builds a blob, wraps it in a 1-Wire dump, boots with the licence check left
# in place, and reports what the decrypt made of it. r0 at 0x001aef40 is
# whether the decrypt succeeded; sp+8 is the length it produced and sp+0x18
# the plaintext.
#
#   tools/trykey.sh [key-bytes] [effective-bits]
#   tools/trykey.sh 5 40      # what the Base CSP gives for RC2
#
# It takes about ninety seconds: the check runs early in KeySoft's start-up,
# not in the first few seconds of the boot.
set -u
cd "$(dirname "$0")/.."

KEYBYTES=${1:-5}
BITS=${2:-40}

echo "=== RC2, ${KEYBYTES}-byte key, T1=${BITS} ==="
python tools/keyblob.py build --key-bytes "$KEYBYTES" --effective-bits "$BITS" \
    --out work/try.blob || exit 1
python tools/keyblob.py eeprom work/try.blob --out work/try-eeprom.bin >/dev/null || exit 1

cat > work/try.dbg <<'DBG'
0x001aef40 : regs, mem sp+0x8 8, mem sp+0x18 48, stop
DBG

rm -f work/try.log work/try.status*
timeout 300 ./target/release/vbnote.exe roms/EBOOT.bin --flash --nk roms/NK.bin \
    --cpu-mhz 63 --check-serial --sd-card work/card.img \
    --serial-eeprom work/try-eeprom.bin --debug work/try.dbg \
    --mute --status work/try.status --free-run > work/try.log 2>&1

if grep -q "break 0x001aef40" work/try.log; then
    echo "--- what the decrypt produced (r0 = 1 means it accepted it) ---"
    grep -A11 "break 0x001aef40" work/try.log | head -13
else
    echo "the validator was never reached"
    tail -3 work/try.log
fi
