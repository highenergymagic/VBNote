# VBNote

An emulator for the HumanWare **VoiceNote QT mPower** — a Windows CE 4.2
notetaker built for blind users on an Intel PXA270 — running the real KeySoft
firmware.

These machines are failing in the field and cannot be repaired or replaced.
The goal is that anyone can download a working VoiceNote instead of hunting for
dying hardware.

**Status: KeySoft boots, asks which language to run in, and can be typed at.** EBOOT provisions, validates its image header, launches NK.bin, the CE kernel
takes over, the flash disk mounts, KeySoft starts and reaches its first-run
prompt, and `--keyboard` opens a window that relays your keystrokes to it.
See [Current state](#current-state).

The window has nothing to display: this machine answers in speech, and the
window exists because an operating system will only deliver keystrokes to one.
Put focus on it and type.

## Running it

You supply `EBOOT.bin` and `NK.bin` yourself. They are HumanWare's, this
project does not carry them, and it never will.

On Windows, double-click **`boot-ks8.bat`**. It builds if it has to, makes
everything else it needs, and tells you what is happening. Or run it yourself:

```bash
vbnote roms/EBOOT.bin --flash --nk roms/NK.bin --cpu-mhz 63     --sd-card work/card.img --serial-eeprom work/SerialNumber.bin --keyboard
```

**`--cpu-mhz` is the flag that decides how long any of this takes.** It sets
the clock the guest's timers run against, so a figure near what this emulator
actually retires makes a guest second last about a real second. The default of
1200 assumes a core far faster than this one, and every delay loop in the
firmware then burns four times the cycles waiting. Measured on this host:

| `--cpu-mhz` | first sound | to the language prompt | speed |
| --- | --- | --- | --- |
| 1200 (default) | 11.3 G cycles | ~400 s | 5% |
| 63 | 4.8 G cycles | ~91 s | 100% |

The default is high for a reason — guest time running fast relative to guest
progress lets the power manager's idle timeout fire mid-boot, and 1200 was
what stopped that. 63 reaches the prompt without suspending, which is measured
rather than assumed, but if a long run suspends on you, raise it.

Measure your own throughput with `--free-run --cycles 4000000000` and a
stopwatch, then pass the millions of cycles a second you get.

`--sd-card` makes the card if the file is not there, partitions it and lets
Windows CE format it. That first boot ends with KeySoft saying the flash disk
is unavailable, because the format is still running; boot it a second time
against the same file and it comes up.

`--serial-eeprom` is the 1-Wire part, eight bytes of identity followed by its
memory. The machine will not start without something answering on that bus, so
if the file is not there one is made and written to that path. Point it at a
dump from a real unit if you have one.

The card is the only thing that provides `\Flash Disk`. The ROM registry
binds that folder to a DiskOnChip, which this emulator does not model, so the
SD profile is rewritten to claim it instead.

Everything the emulator changes in the firmware image is listed by
`--help` under *Getting KeySoft to start*, each with a switch to turn it off,
and each documented in [`patch.rs`](crates/gandalf/src/patch.rs).

```
$ vbnote EBOOT.bin --flash --nk NK.bin

Loading configuration data...done
Resetting factory default configuration ...done
Failed Initializing MMC Controller
Can't Find Boot Device from Priority Configuration

BrailleNote mPower Bootloader
Built 2.7 Jun 15 2007 09:40:40
Copyright (c) 2005 HumanWare Ltd

<<<<< BrailleNote mPower Boot Loader Menu 2.7 >>>>>>

1) Update Windows CE from an SD card
2) Update the Bootloader from an SD card
...
To boot Windows CE, press SPACE

Option?
```


## This is not a HumanWare product

It is an independent project. It is not affiliated with HumanWare, not
endorsed by them, not supported by them, and not derived from anything they
provided. Two things follow from that, and both are conditions of using this
software:

1. **Do not contact HumanWare about this.** Not as a user, not as a developer,
   not now, not later. They did not make it, they cannot support it, and it is
   not their problem. If you have a question about this emulator, raise it
   here. (A question about a real BrailleNote or VoiceNote you own is a
   different matter and is between you and whoever supports it.)
2. **Do not present this as a HumanWare product.** No implication of support,
   endorsement, partnership or origin — in the software, in the
   documentation, in a bug report, or in a screenshot.

HumanWare built these machines and supported them for years. The least this
project owes them is to leave them alone and to be unmistakably not-them.

If you fork this, carry both conditions with it.

## Setting up a machine

The first run builds a machine of your own. There is a wizard for it:

```
setup-vbnote.bat
```

Four pages -- what this is and is not, where your firmware is, a progress bar,
and done. It leaves three files in `%USERPROFILE%\.VBNote`:

| file | what it is |
| --- | --- |
| `KeysoftSystemDisk.img` | the NOR flash: bootloader, image header, operating system. Read-only once made. |
| `FlashDisk.img` | the card the machine keeps your documents on |
| `onewire.img` | the 1-Wire part, holding the machine's identity |

It takes several minutes, and most of that is the machine genuinely starting
for the first time: Windows CE partitions and formats the card, and KeySoft
lays out its folders and asks its first-run questions. The wizard answers
those with their default answers, all of which can be changed afterwards on
the machine itself. When it finishes, the machine boots straight to its main
menu.

The same work without a window, which is also how it is tested:

```
python -m wizard --eboot roms/EBOOT.bin --nk roms/NK.bin
```

The wizard needs wxPython (`pip install -r wizard/requirements.txt`); the
headless path needs nothing beyond the standard library.

## Goals

- Run the unmodified KeySoft 8.0 ROM, the last version these machines shipped
  with. 7.5 works too.
- Speech out, QWERTY keyboard in. Braille display output is explicitly out of
  scope for now — this targets the VoiceNote QT, which has no braille cells.
- Ship as a single Windows `.exe` that a blind user can download and run with
  no configuration, no toolchain, and no sighted assistance.

## Non-goals

- Braille display emulation (see above).
- Cycle accuracy. Fast enough for real-time speech is the bar.
- Emulating hardware KeySoft never touches.

## Current state

| Component | State |
|---|---|
| ARMv5TE CPU core (`crates/arm`) | ARM + Thumb, banked modes, all seven exceptions, v5TE saturating and DSP multiplies. |
| MMU + CP15 | Two-level walk, sections / coarse / fine, large / small / tiny pages, domains, AP checks, FCSE, high vectors, software TLB, pipelined MMU-enable. |
| PXA270 SoC (`crates/pxa270`) | Interrupt controller, OS timers, GPIO (4 banks), 16550 UARTs, clock manager, power manager, RTC, memory controller, AC97, MMC/SD. |
| Gandalf board (`crates/gandalf`) | Memory map, SDRAM, Intel CFI NOR flash, CPLD, 1-Wire serial-number EEPROM, keyboard matrix. |
| CE ROM tooling (`crates/ceromfs`) | `B000FF` parser with checksum verification. |
| Audio | AC97 PCM out to the host speakers via WASAPI, plus WAV capture. |
| Windows app (`app`) | Boots an image, relays host keystrokes through a window, plays and records audio, and carries a scriptable debugger. |

174 tests passing (`cargo test`).

### What EBOOT gets through

Everything up to and including its interactive boot menu: CPU and CP15 setup,
GPIO, the static memory controller, its flash-to-SDRAM self-copy and
verification, page tables, the MMU enable, AC97 codec bring-up, the CFI flash
probe, the SD boot-device probe, and its configuration load. It then waits at
`Option?` for a keypress.

Provisioning builds a faithful flash device — bootloader, HumanWare image
header, kernel — and the whole chain runs:

```
ImageID: 0x45464748 Start: 0x41000 Length: 0x2A86FD0 !!!
Lauch NK.bin in ROM: 0x41000!!!
Launching image in ROM
```

after which the CE kernel executes at `0x800834b8` with CP15 control `0x3a7f`
(MMU, caches, high vectors) and TTBR `0xa0130000` — its own address space,
not the bootloader's.

The kernel boots through interrupt dispatch, demand paging, process creation
and driver load:

```
Windows CE Kernel for ARM (Thumb Enabled) Built on Jan 23 2006 at 13:14:29
ProcessorType=0915  Revision=3
OEMInit: RCSR:0x00000001
USBC *** GetSerialObject()
*****UDC Endpoint Memory configured
-- in ac97ctrlconfigure --
SerDBG port is disabled
```

**The machine makes sound.** Windows CE runs its whole registry launch
sequence — `shell.exe`, `device.exe`, `gwes.exe`, `explorer.exe`,
`services.exe`, then HumanWare's `Kickoff.exe` which starts KeySoft — the
machine stays powered on, and PCM comes out of the AC97 codec:

```bash
cargo run --release -p vbnote -- EBOOT.bin --flash --nk NK.bin --cpu-mhz 63
```

It runs until Ctrl-C and plays through the host's speakers.

The first sound arrives about eighty seconds in. It prints
`audio: first samples after ...` when it does, so a long silence is not a
hang.

`--cpu-mhz` is the flag that matters. It sets the clock the guest's timers
run against, so a figure near the emulator's actual throughput makes a guest
second last about a real second — which both keeps audio smooth, by having
the codec ask for samples at the rate the sound card drains them, and keeps
the OAL's delay loops from burning cycles. Those loops wait on the OS timer
in guest time, so a lower setting reaches the startup sound in a quarter of
the cycles: 5.1 G at 63 MHz against 21 G at the 1200 MHz default.

Measure your own throughput with `--free-run --cycles 4000000000` and a
stopwatch, then pass the cycles-per-second you get, in millions.

KeySoft reaches its first-run prompt — *"operating language, press enter for
English"* — and answers keystrokes typed into the window.

### What is not done

- **The first-run setup, end to end.** It answers questions and writes the
  answers to the card, but nothing has driven the whole sequence through and
  checked the result.
- **Speed.** 65 M cycles/s, 15.3 ns an emulated cycle against 3.2 ns for the
  real PXA270. The cheap wins are taken; what is left is interpretation
  itself, and the remaining 4.6x is a JIT.
- **Braille output.** Out of scope, deliberately. This targets the VoiceNote
  QT, which has no cells.

## Building

```bash
cargo test
```

Provision a flash image and boot it:

```bash
cargo run --release -p vbnote -- EBOOT.bin --flash --nk NK.bin --provision roms/flash.img
```

Requires a recent Rust toolchain with the `x86_64-pc-windows-msvc` target. No
C toolchain, no cmake — the CPU core is written from scratch rather than
binding Unicorn or QEMU's TCG, specifically so the build stays trivial and so
we keep direct control of the IRQ line, which a driver-heavy OS emulation
depends on.

## Firmware

Not included and never will be. `NK.bin` and `EBOOT.bin` are HumanWare's
copyrighted property; the `.gitignore` is set up to keep them out of the repo.
Owners of the hardware can obtain KeySoft from HumanWare's update
distribution. Put the images in `roms/`.

## Analysis tools

`tools/` holds the Python used to take the firmware apart:

- `cebin.py` — parse a Windows CE `B000FF` ROM: records, ROMHDR, module and
  file tables.
- `ceextract.py` — extract ROM modules and rebuild them as valid PE files.
  Ghidra imports and auto-analyses the output, MSVC demangler included.
- `strs.py` — ASCII and UTF-16 string scanner.

```bash
python tools/cebin.py roms/NK.bin
python tools/ceextract.py roms/NK.bin cpld2.dll pdikeybd.dll
```

Findings from that work are written up in [docs/hardware.md](docs/hardware.md).

## Licence

GPL-2.0-only. Device models are derived from QEMU, which is GPLv2.

## Looking inside a run

`--debug SCRIPT` takes a file of breakpoints, each with actions, so one boot
answers many questions instead of one:

```text
# address [conditions] : actions
0x00023334 slot=9            : regs, back 6, stop
0x0001140c slot=9 r1=0x1f0   : regs, mem r0 64
0x02259974                   : count
```

`slot=N` matters: every EXE in this ROM links at `0x00010000`, so without it a
breakpoint fires in every process at once. Actions are `regs`, `mem ADDR LEN`
(where `ADDR` may be a register, so `mem r0 64` follows a pointer), `back N`,
`count` for a site too hot to print at, and `stop`. Without `stop` the run
carries on, so a script can watch a sequence.

A detached run has no terminal, so it writes `vbnote.status` four times a
guest second — cycles, guest and real seconds, speed against real time, audio
underruns and seconds recorded — and ends cleanly when `vbnote.status.stop`
appears, saving the card on the way out.
