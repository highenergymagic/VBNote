# VBNote

**An emulator of the VoiceNote QT and BrailleNote mPower, for when yours has
died and there is no replacement.**

It runs the real KeySoft firmware on an emulated PXA270 and Windows CE 4.2.
It boots to the main menu, speaks, and takes a QWERTY keyboard. Your documents
live on an emulated flash disk that survives being switched off.

> **This is not a HumanWare product.** It is an independent project by Fractal
> Microsystems. It is not affiliated with HumanWare, not endorsed by them, and
> not supported by them. **Please do not contact HumanWare about it** — they
> did not make it and cannot help with it. Ask here instead.

---

## What you need

1. **Windows**, 64-bit.
2. **The firmware from a machine you own** — two files, `EBOOT.bin` and
   `NK.bin`. VBNote does not include them and cannot obtain them for you. They
   belong to whoever made your machine, and getting them off it is something
   you do yourself.

That is all. Everything else is in the installer.

## Installing

Download `VBNote-1.1.0-setup.exe` from
[Releases](https://github.com/highenergymagic/VBNote/releases) and run it.

It installs for you alone and does not ask for an administrator. Windows may
warn that it does not recognise the program: VBNote is not code-signed, and a
signature costs money this project does not have. Choose **More info**, then
**Run anyway**.

You get three things on the Start menu:

| | |
| --- | --- |
| **Set up VBNote** | builds your machine. Run this once, first. |
| **VBNote** | starts your machine. |
| **VBNote settings** | opens the settings file in Notepad. |

## Setting up, once

Run **Set up VBNote**. It asks you to agree to the two conditions above, then
where your `EBOOT.bin` and `NK.bin` are, and then it builds your machine while
you wait. It takes about ten minutes.

Most of that is not the computer being slow. It is the machine genuinely
starting up for the first time, formatting its flash disk and answering its own
first-run questions — the same questions a new BrailleNote asks out of the box.
The wizard answers them with the standard answers, and you can change any of
them afterwards on the machine itself, in the options menu.

When it finishes, everything lives in `%USERPROFILE%\.VBNote`:

| file | what it is |
| --- | --- |
| `KeysoftSystemDisk.img` | the machine's ROM: bootloader and operating system |
| `FlashDisk.img` | **your flash disk — your documents live here** |
| `onewire.img` | the machine's identity |
| `VBNote.ini` | settings |

Back up `FlashDisk.img` the way you would back up any other documents.
Uninstalling VBNote never touches it.

## Using it

Start **VBNote**. It comes up at the main menu, the same as the real machine.

### The keyboard

Your PC keyboard is the machine's keyboard. The three keys a PC does not have
are mapped to keys near where your thumbs already are:

| the machine's key | your keyboard |
| --- | --- |
| **READ** | left Alt |
| **FUNCTION** | right Alt |
| **CONTROL** | either control key |
| SHIFT, letters, digits, punctuation, arrows, Backspace, Enter, Escape, Tab | themselves |
| HELP, REPEAT, MENU | F1, F2, F3 |

So `FUNCTION`+`O` for the options menu, `READ`+`T` for the time, and so on,
exactly as the manuals describe.

### F11 is VBNote's own key

F11 never reaches the machine. Held with a letter, it talks to the emulator:

> **This was right control before 1.1.0.** It changed because not every
> keyboard has a right control key, and without a host key you cannot take the
> keyboard at all. Right control now works as **CONTROL**, like the left one.

| | |
| --- | --- |
| **F11 + G** | take the keyboard, or give it back |
| **F11 + R** | reset the machine |
| **F11 + P** | the power switch |
| **F11 + Q** | quit, saving your flash disk |

**Start by pressing F11 + G.** Until you do, your keys go to Windows
as usual and the machine hears nothing. While the keyboard is taken, every key
goes to the machine instead — so you can type freely without Windows shortcuts
getting in the way. Press it again to get your keyboard back. VBNote says which
state it is in every time it changes, through NVDA if you have it running.

The emulator only takes keys while its own window is in front, so it never eats
keystrokes in other programs.

### While it starts

A cold start takes about a minute and a half, the same as the real machine, and
it is silent for most of that. VBNote beeps quietly every five seconds until
the machine finds its voice, so you know it is still working.

## Settings

`%USERPROFILE%\.VBNote\VBNote.ini`, a plain text file. Every setting has a
comment above it saying what it does.

The one worth knowing about is **`cpu_mhz`**. It is not a speed dial. It is how
much work the emulator promises to do per second of the machine's time, and
promising more than your computer can deliver does not make the machine faster
— it makes its speech break up, because the machine produces speech in its own
time and your sound card plays it in real time. The default of 63 is measured
to keep up on an ordinary computer. **If speech stutters, lower it.** Raising
it is unlikely to help.

Set `debug = yes` to get a terminal window with a running commentary and a
status file, which is worth including if you report a problem.

## If something goes wrong

**"VBNote has not been set up yet."** Run **Set up VBNote** from the Start
menu first.

**Speech breaks up or stutters.** Lower `cpu_mhz` in the settings file — try
52, then 40.

**The machine does not respond to the keyboard.** Press F11 + G to
take the keyboard, and check VBNote's own window is in front.

**Setup says your firmware is not the tested build.** VBNote is tested against
KeySoft 8.0, because that is the build available to test with. Another build
may work perfectly well — nobody has tried it. If you go on and the machine
behaves oddly, say which build you used when you report it.

Problems go to
[Issues](https://github.com/highenergymagic/VBNote/issues) — not to HumanWare.

## What is not here

- **Braille output.** The target is the VoiceNote QT, which has no braille
  cells. There is no braille display support and none is planned.
- **The modem's line.** The machine finds its modem and talks to it, but it is
  not connected to anything, so there is no dialling out.
- **Resume from sleep.** The power switch suspends the machine and switching it
  back on starts it fresh, where a real one would pick up the document you had
  open.

## For developers

Everything reverse-engineered is written up in
[`docs/hardware.md`](docs/hardware.md), with addresses. The emulator is a Rust
workspace:

| | |
| --- | --- |
| `crates/arm` | ARMv5TE interpreter: ARM and Thumb, MMU, CP15, FCSE |
| `crates/pxa270` | the SoC: interrupts, timers, GPIO, AC97, MMC/SD, DMA, I2C |
| `crates/gandalf` | the board: memory map, NOR flash, CPLD, 1-Wire, keyboard, modem |
| `crates/ceromfs` | the Windows CE `B000FF` ROM format |
| `app` | the runner: CLI, window, audio, keyboard hook, debugger |
| `wizard` | the setup wizard, and the same work headless |

```
cargo test --workspace
vbnote roms/EBOOT.bin --flash --nk roms/NK.bin --sd-card card.img --keyboard
vbnote --help
python -m wizard --eboot roms/EBOOT.bin --nk roms/NK.bin --home somewhere
```

Building the installer needs Rust, PyInstaller and Inno Setup 6:

```
powershell -File installer\build.ps1
```

Tagging a version builds it on GitHub Actions and publishes the release, which
is where the installer people download comes from.

## Licence

GPL-2.0-only. Copyright © 2026 Fractal Microsystems.

VBNote bundles NVDA's controller client so that it can speak in your own
screen reader's voice; that is NVDA's work, also GPL-2.0, and its licence ships
alongside.

The machine's firmware is not part of this project, is not included, and is not
distributed by it.
