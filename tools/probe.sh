#!/usr/bin/env bash
# After setup, is the machine at a menu with nothing to say, or is it stuck?
#
# Drives the first-run questions, then presses several different keys with a
# pause after each and reports whether any of them made it speak. A machine at
# a menu answers something to at least one of them; a stuck one answers none.
set -u
cd "$(dirname "$0")/.."

STATUS=work/probe.status
rm -rf work/utterances work/keys.txt "$STATUS"* work/probe.log

./target/release/vbnote.exe roms/EBOOT.bin --flash --nk roms/NK.bin \
    --cpu-mhz 63 --sd-card work/card.img --serial-eeprom work/SerialNumber.bin \
    --utterances work/utterances --utterance-gap 2.5 --keys-from work/keys.txt \
    --mute --status "$STATUS" > work/probe.log 2>&1 &
EMU=$!
sleep 20

python tools/converse.py --answers work/answers.txt --settle 2.0 --timeout 280 \
    2>&1 | tail -4

echo
echo "=== setup answered; probing for a menu ==="
send() {
    printf '%s' "$1" > work/keys.txt.part
    mv work/keys.txt.part work/keys.txt
}
for key in "h" $'\x1b' $'\n' " " "m"; do
    before=$(ls work/utterances 2>/dev/null | wc -l)
    case "$key" in
        $'\x1b') label="escape" ;;
        $'\n')   label="enter" ;;
        " ")     label="space" ;;
        *)       label="'$key'" ;;
    esac
    send "$key"
    sleep 40
    after=$(ls work/utterances 2>/dev/null | wc -l)
    if [ "$after" -gt "$before" ]; then
        echo "  $label -> it spoke"
    else
        echo "  $label -> silence"
    fi
done

touch "$STATUS.stop"
wait $EMU 2>/dev/null
echo
echo "=== anything new it said ==="
mkdir -p work/tr
for f in $(ls work/utterances/utt-*.wav 2>/dev/null | tail -3); do
    b=$(basename "$f" .wav)
    ffmpeg -v error -y -i "$f" -ar 16000 -ac 1 "work/tr/$b.wav" 2>/dev/null &&
        printf "  %s: " "$b" &&
        /c/bin/whisper -nt -m "C:/Users/freya/models/ggml-base.en.bin" \
            "work/tr/$b.wav" 2>/dev/null | tr '\n' ' '
    echo
done
