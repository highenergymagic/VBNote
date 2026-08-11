"""Listen to the machine and answer it.

The emulator writes one WAV per burst of speech and reads keystrokes from a
file. This sits between: it waits for an utterance, transcribes it, decides
what to press, writes that back, and goes round again.

    python tools/converse.py --answers answers.txt

Without a script of answers it presses Enter for everything, which is what the
first-run questions want.

Answers are matched against the transcript, one rule per line, first match
wins:

    braille grade   : \\n
    thumb key       : \\n
    *               : \\n

`*` matches anything. `\\n` is Enter, `\\t` Tab, `\\e` Escape; everything else
is typed literally.

Start the emulator alongside it, sharing the same two paths:

    vbnote roms/EBOOT.bin --flash --nk roms/NK.bin --cpu-mhz 63 \\
        --sd-card work/card.img --serial-eeprom work/SerialNumber.bin \\
        --utterances work/utterances --keys-from work/keys.txt \\
        --status work/status
"""

import argparse
import os
import subprocess
import sys
import time

WHISPER = os.environ.get("WHISPER", r"C:/bin/whisper")
MODEL = os.environ.get("WHISPER_MODEL", r"C:/Users/freya/models/ggml-base.en.bin")
FFMPEG = os.environ.get("FFMPEG", "ffmpeg")


def transcribe(wav, scratch):
    """Whisper wants 16 kHz mono, and the guest produces 44.1 kHz stereo.

    The downsampled copy goes in its own directory. Putting it beside the
    utterances made this script transcribe its own scratch file, which it then
    answered, which pressed a key nobody asked for.
    """
    os.makedirs(scratch, exist_ok=True)
    small = os.path.join(scratch, "_16k.wav")
    r = subprocess.run(
        [FFMPEG, "-v", "error", "-y", "-i", wav, "-ar", "16000", "-ac", "1", small],
        capture_output=True,
    )
    if r.returncode != 0:
        return ""
    r = subprocess.run(
        [WHISPER, "-nt", "-m", MODEL, "-otxt", small],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        return ""
    # whisper prints the text and also leaves it in a .txt beside the input.
    txt = small + ".txt"
    if os.path.exists(txt):
        with open(txt, encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    return r.stdout.strip()


def load_answers(path):
    rules = []
    if not path:
        return [("*", "\n")]
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.split("#")[0].strip()
            if not line or ":" not in line:
                continue
            pattern, keys = line.split(":", 1)
            keys = keys.strip().replace("\\n", "\n").replace("\\t", "\t").replace("\\e", "\x1b")
            rules.append((pattern.strip().lower(), keys))
    return rules or [("*", "\n")]


def decide(text, rules):
    low = text.lower()
    for pattern, keys in rules:
        if pattern == "*" or pattern in low:
            return keys
    return None


def say(*a):
    print(*a, flush=True)


def show(keys):
    return "".join({"\n": "<enter>", "\t": "<tab>", "\x1b": "<esc>"}.get(c, c) for c in keys)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--utterances", default="work/utterances")
    ap.add_argument("--keys", default="work/keys.txt")
    ap.add_argument("--answers", help="file of `pattern : keys` rules")
    ap.add_argument("--scratch", default="work/transcribe",
                    help="where to put the downsampled copies")
    ap.add_argument("--settle", type=float, default=1.0,
                    help="seconds to wait after an utterance before answering")
    ap.add_argument("--timeout", type=float, default=900.0,
                    help="give up if nothing is said for this long")
    args = ap.parse_args()

    rules = load_answers(args.answers)
    os.makedirs(args.utterances, exist_ok=True)
    seen = {f for f in os.listdir(args.utterances)
            if f.startswith("utt-") and f.endswith(".wav")}
    say(f"listening in {args.utterances}, answering through {args.keys}")
    say(f"{len(rules)} rule(s); already present and ignored: {len(seen)}")

    last = time.time()
    while True:
        if time.time() - last > args.timeout:
            say("nothing said for a while; stopping")
            return 0
        try:
            now = sorted(
                f for f in set(os.listdir(args.utterances)) - seen
                if f.startswith("utt-") and f.endswith(".wav")
            )
        except FileNotFoundError:
            now = []
        if not now:
            time.sleep(0.3)
            continue

        for name in now:
            seen.add(name)
            path = os.path.join(args.utterances, name)
            # The emulator writes the file after the burst has ended, but give
            # the write itself a moment to land.
            time.sleep(0.2)
            text = transcribe(path, args.scratch)
            print(f"\n[{name}] {text!r}")
            last = time.time()

            keys = decide(text, rules)
            if keys is None:
                say("  no rule matches; waiting")
                continue
            time.sleep(args.settle)
            # Write it somewhere else and rename it into place. The emulator
            # polls for this file and deletes it once read, and a plain open
            # gives it a window to read a half-written one -- which it did,
            # twice per answer, so every keystroke was delivered twice and the
            # machine quietly skipped a question.
            tmp = args.keys + ".part"
            with open(tmp, "w", encoding="utf-8") as f:
                f.write(keys)
            os.replace(tmp, args.keys)
            say(f"  pressing {show(keys)}")


if __name__ == "__main__":
    sys.exit(main())
