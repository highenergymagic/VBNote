# VBNote — agent context

Rust emulator for the HumanWare **VoiceNote QT / BrailleNote mPower**: Windows
CE 4.2 on Intel PXA270, running real KeySoft firmware. Assistive-technology
preservation — these machines are dying and cannot be replaced.

## Hard constraints

- **Never commit firmware.** `NK.bin`, `EBOOT.bin`, ROM dumps, flash images are
  HumanWare's copyright. `.gitignore` covers `*.bin`, `/roms/`, `*.pe`,
  `*.wav`, `/work/`. Check before `git add -A`.
- **Braille output is out of scope.** Target is the VoiceNote QT, no cells.
- **GPL-2.0-only**, so QEMU source can be drawn on.
- **Zero-configuration, keyboard-only, self-voicing.** The user is blind; the
  window exists only to receive keystrokes. No dialogs, no visual state.
- **Verify claims that can be verified.** Web search, Linux source, the ROM
  itself. Do not guess where a measurement is possible.
- **The DiskOnChip is gone.** Removed deliberately, October 2025 work. Do not
  reintroduce it. `\Flash Disk` comes from the SD card.

## Position

This is **not a HumanWare product** and is not affiliated with them. Two rules,
both binding and both in the README: **never contact HumanWare about this
product**, and **never present it as theirs** -- in the software, the docs, a
bug report or a screenshot.

Do not write anything into this repository about what any company has or has
not said regarding this project. Say what the project is not; say nothing
about anybody's legal position.

## Layout

| path | what |
|---|---|
| `crates/arm` | ARMv5TE interpreter, ARM + Thumb, MMU/CP15, FCSE |
| `crates/pxa270` | SoC: INTC, OS timers, GPIO, UART, AC97, MMC/SD, DMA, I2C |
| `crates/gandalf` | Board: memory map, NOR flash, CPLD, 1-Wire, keyboard, patches, registry, provisioning |
| `crates/ceromfs` | Windows CE `B000FF` ROM parser |
| `app` | Runner: CLI, window, audio, scriptable debugger |
| `tools/*.py` | Firmware analysis: `ceextract.py` (ROM module → PE), `modmap.py` (VA → module), `literalrefs.py`, `cebin.py` |
| `docs/hardware.md` | Everything reverse-engineered, with addresses |
| `work/` | Gitignored scratch: card image, EEPROM dump, logs |

## Installed mode (`app/src/home.rs`, `installer/`)

`vbnote.exe` with **no arguments at all** is how the Start menu starts it, and
it is a supported path rather than an error: it finds `%USERPROFILE%\.VBNote`,
reads `VBNote.ini`, and boots `KeysoftSystemDisk.img` with `FlashDisk.img` and
`onewire.img`, with the window, the keyboard hook and speech all on.

- **It is a windowed program that borrows a console.** Built
  `windows_subsystem = "windows"` so the Start menu does not put a terminal
  beside the machine, then `AttachConsole(ATTACH_PARENT_PROCESS)` at the top of
  `main` so a command line still gets output, and `AllocConsole` when
  `debug = yes`. Only *missing* standard handles are filled in: replacing them
  unconditionally undoes redirection, and `vbnote --help > file` wrote to a
  console nobody could see.
- **Firmware is recognised, never refused.** `provision::tested` has the
  SHA-256 of the KeySoft 8.0 pair, `gandalf::sha256` is fifty lines and checked
  against the published vectors, and a mismatch prints what it got and what was
  expected. The wizard asks the same question in a dialog with a way past it.
- **No machine there means a dialog, not a message.** There is no console when
  a windowed program is started from a menu, so `eprintln!` reaches nobody. It
  says so in a `MessageBoxW`, which a screen reader reads, and stops.
- **The bootloader argument is optional now.** A flash image already contains
  one; requiring `EBOOT.bin` alongside it was an accident of how the CLI grew.
- **`VBNote.ini` is flat `key = value`**, no sections, comments with `#` or
  `;`. A bad line is reported and ignored rather than fatal -- it is a file a
  person edits, and a typo should not stop the machine starting. The wizard
  writes one, and the emulator writes one if it is missing; both come from the
  same text, and a test pins that the file's comments and the defaults agree.
- **The card is never removed by the uninstaller.** It is the user's
  documents.
- **"Successful compile" is not proof a file went in.** 1.0 shipped with no
  NVDA controller client: the workflow wrote `nvdaControllerClient.dll` and
  the `.iss` asked for `nvdaControllerClient64.dll` with
  `skipifsourcedoesntexist`, so Inno left it out and said nothing. Nothing
  failed, the only symptom was speech in the wrong voice, and the installer
  was 2 KB *smaller* than the release before it. So: the client is fetched by
  `installer\build.ps1` rather than by the workflow, so a local build and CI
  package the same file; its source is **required**, not skippable; its
  SHA-256 is checked because it is loaded into this process; and the build
  reads back ISCC's own file list and fails if `vbnote.exe`, the wizard or the
  client is not in it. **Anything optional in a build is something that can be
  missing from a release without a word.**

## Provisioning (`wizard/`)

A wxPython wizard, and the same work headless. Builds `~/.VBNote` with
`KeysoftSystemDisk.img` (NOR flash, read-only afterwards), `FlashDisk.img`
(the card) and `onewire.img`. `python -m wizard --eboot E --nk N --home DIR`
is the testable path and takes about eight minutes.

- **The card cannot be built, only grown.** CE partitions and formats it on
  the first boot and KeySoft fills it afterwards, so provisioning is a real
  boot, not a file-writing exercise.
- **It has to answer the machine's own first-run questions**, or nothing is
  ever created. `--keys-from` takes the Enters.
- **CE lays the card out as an *extended* partition** (type `0x05`) with the
  volume inside it. Reading the outer entry as a volume finds a boot signature
  and a boot record of zeroes, which looks exactly like "formatted but empty"
  and is not. `wizard/flashdisk.py` follows the chain.
- **Knowing when it is done took the measuring, and the obvious signals are
  all wrong.** `General` appears after about two answers with questions still
  to come. Silence means nothing either: mid-setup the machine takes **thirty
  seconds** to answer one question and ask the next, so a pause and a finished
  setup are indistinguishable without recognising the words. What settles it
  is that **overanswering is harmless** -- past the last question the extra
  Enters land on the word processor's "document to create" prompt and create
  nothing, and a machine given forty answers still boots straight to the main
  menu. So it presses Enter a fixed ten times, stops, waits a minute for
  quiet, and then checks the card.

## Running

```bash
vbnote roms/EBOOT.bin --flash --nk roms/NK.bin --cpu-mhz 63 \
  --sd-card work/card.img --serial-eeprom work/SerialNumber.bin --keyboard
```

- **`--cpu-mhz` now defaults to 63** (`CPU_HZ_DEFAULT`), which is what this
  interpreter can actually retire. It used to default to 1200 and that is the
  difference between a 145-second boot and a 400-second one, and worse, between
  clean speech and a machine that stutters so badly it sounds broken: at 1200
  the emulator holds **6%** of real time, and the guest makes its audio in
  guest time, so six percent of real time is six percent of the sound with the
  device draining the rest as silence. Higher is not faster. The old default
  was the single easiest way to waste an afternoon — every measurement in a session costs four times what it
  should. The default is 1200, which assumes a core far faster than this
  interpreter, so the firmware's delay loops burn cycles waiting. 63 matches
  what this host retires: speed 73% against 5%, first sound at 4.8 G cycles
  against 11.3 G. The default is high because guest time running fast relative
  to guest progress lets the power manager's idle timeout suspend the machine
  mid-boot; 63 is measured not to.
- `--help` lists every option; all are documented there.
- A **fresh card fails its first boot** ("flash disk is unavailable") because
  formatting is still running. Boot again against the same file.
- `--status PATH` writes progress; `touch PATH.stop` ends a run cleanly and
  saves the card. A detached run has no console, so this is the only way.
- `--debug SCRIPT` sets breakpoints with actions (`regs`, `mem`, `back`,
  `count`, `stop`). Every EXE links at `0x00010000`, so use `slot=N` or a
  breakpoint fires in every process.
- **A boot to the first prompt takes about 90 seconds** with `--cpu-mhz 63`,
  or 7 minutes without it. Run it in the background and poll `status` rather
  than blocking on it.

## Verifying by ear

The machine answers in speech, so audio is the primary output:

```bash
ffmpeg -v error -y -i run.wav -ar 16000 -ac 1 run16.wav
whisper -nt -m ggml-base.en.bin -otxt run16.wav
```

Transcription is rough but enough to tell prompts apart.

## The two patches (`patches/*.nkp`)

Patches are **`.nkp` files under `patches/`**, not Rust: a name, a reason, a
signature and a replacement, with the case for the patch in the comments beside
the bytes. `crates/gandalf/src/nkp.rs` parses them, they are embedded in the
binary with `include_str!` so a release needs nothing beside it, and
`--patches DIR` reads a directory instead.

Applied to the image at provisioning, never to `NK.bin` on disk. Matched by
**byte signature**, not address, so they survive a version change — three of
four relocated themselves from KeySoft 7.5 to 8.0 untouched.

| patch | what it does |
|---|---|
| `SD_FOLDER_IS_FLASH_DISK` | SD block driver mounts as `Flash Disk`, not `SDMMC Disk` |
| `SD_PROFILE_IS_FLASH_DISK` | the storage profile agrees with it |

There used to be three more, all about the licence. They are gone:
`crates/gandalf/src/licence.rs` builds a **real** licence into the 1-Wire part
at provisioning, and KeySoft validates it with its own code. Do not reach for a
patch there again -- see that module and `docs/hardware.md`.

`Reach::Sole` refuses when a signature matches twice — the default, and the
point: `trueffs.dll` ships near-identical driver copies and three addresses in
it turned out to be dead. `Reach::Every` is only for a routine **shown module
by module** to be duplicated (the licence pair is in `KeySoft.exe` and
`VRPckAud.dll`).

## Key firmware facts

KeySoft 8.0 addresses, from `tools/extracted/KeySoft.exe.pe`.

- **Licence validator `0x001aef10`.** Decodes a blob from the 1-Wire part,
  requires 44 bytes, **copies the payload to ctx+0x100 _before_ the device-id
  comparison**, then compares 8 bytes at payload+0x18 against
  `IOCTL_HAL_GET_DEVICEID`. That ordering is why the identity patch is
  possible.
- **The payload is not only a licence.** `0x001aecdc` reads flags at
  payload+0x04 and +0x0c and an **LCID halfword at +0x10**, and hands them to
  KeySoft. This is why a real blob replaces three patches at once: locale,
  model flags and entitlement all live in it.
- **The blob is RC2**, through the CryptoAPI, keyed from MD5 of the passphrase
  `s#r14^ln5m` -- five bytes of hash plus eleven of zero salt, 40-bit
  effective length, CBC with a zero IV and PKCS#5. Not Blowfish; those tables
  are in KeySoft.exe for something else and reading them as the answer cost a
  detour.
- **The device id is the 1-Wire serial**, zero-padded to eight. The emulator
  supplies the part, so the identity inside the licence and the identity the
  machine reports agree by construction.
- **Keyboard**: `flags2` bit 1 clear ⇒ use the locale; bit 0 clear ⇒
  VoiceNote, set ⇒ BrailleNote. LCIDs that mean anything: `0x0809` English,
  `0x040c` French, `0x0c0c` French Canadian. Dispatcher `0x00175790`.
- **`pdikeybd.dll` has eight key tables**, `0x1020`-`0x12c0`, 96 bytes each and
  consecutive, selected by IOCTL `0xb2030` with a 4-byte input. A VoiceNote
  table is its BrailleNote twin **minus the four braille dot keys**
  `0xD7`-`0xDA`, with left control moved from column 1 row 2 to **column 1 row
  7** -- that direction, not the other; an earlier note here had it backwards.
  No letter or digit differs, which is why a wrong table still types and hides
  the error. Code 4 ⇒ table `0x11a0`, what this emulator models.
- **The keys that are not letters have names, and the ROM gives them.**
  `READ` is `0xA4`, `FUNCTION` is `0xA5`, `CONTROL` is `0xA2`; `HELP`, `RPT`
  and `MENU` are F1, F2 and F3. Measured, not inferred: KeySoft parses a
  keystroke notation of its own at `0x000f0998` -- `[READ]`, `[SHIFT]`,
  `[CTRL]`, `[FN]` -- and presses a specific code for each, and a table of
  (name, code) pairs at `0x00230e70` covers the rest. The low byte beside each
  code is the length of its own name, so a misreading of the table would not
  line up. `READ` is the QT's chord key, standing where `SPACE` stands on a
  braille model: the same string ships twice, "press SPACE with I" and "press
  READ with I". `app/src/keys.rs` uses that notation directly, so `[READ]t`
  out of a manual can be typed at `--keys-from`.
- **Entitlement triple** at payload +0x20/+0x24/+0x28 is version, build, model
  class — read by one getter, `0x001aed54`, and nothing else. It does **not**
  choose the keyboard. Left at zero the getter reports "no licence data", and
  KeySoft finishes its setup questions, asks `Product key?`, calls the empty
  answer invalid and says **"Cannot run this version of KeySoft"**. Version 8,
  build 20 (compared rounded down to a ten) and model class 0 get past it.

## Where the modem is

A discrete **TL16C-series UART on the CPLD chip select**, not a PXA UART, not
USB and not I2C. `Drivers\\BuiltIn\\UART1` in the ROM registry says so:

```
FriendlyName  UART1-TL16C
IoBase        0x10000000     the CPLD window
IoStride      0x00000002     registers a halfword apart
IRQ           0x0000000a
BaudClock     0x00c65d40
Tsp           Unimodem.dll
```

Confirmed in behaviour as well as on paper: `serial.dll`, through `ceddk.dll`,
writes 0x83 to LCR for DLAB and 8-N-1, sets a divisor, writes 0x0b to MCR and
polls LSR, which is a textbook 16550 bring-up at CPLD offsets 0x00 to 0x0e.
`crates/gandalf/src/modem.rs` models it and answers AT commands.

`Drivers\\BuiltIn\\SoftModem` is **not** a registry key. It appears once, in
KeySoft.exe's own strings beside `\\windows\\serial.dll` and `Country`, so KeySoft
writes that key itself. Searching the registry for it and not finding it is
what led to the wrong conclusion that there was no modem at all.

**Both interrupts.** It signals on `IRQ 0x0a` with `GPIO 0x10`, and on this
SoC IRQ 10 is the shared `GPIO_2_x` source, so the part interrupts by driving
**GPIO 16**, armed rising by the OAL. The driver does not poll: it enables
every source in `IER`, writes one byte, and waits to be told the transmitter
drained. A model that answers `IIR` with "nothing pending" for ever stops the
machine dead after the `A` of `AT`. `IIR` must latch, and reading it must be
what clears the latch and lowers the pin.

## Getting a key from the host

**A keyboard hook takes the keys; the window library cannot.** minifb's Windows
key-state table has `0x01d`/`0x11d` for the control keys and `0x02a`/`0x036`
for the shifts and **nothing** for `0x038`/`0x138`, so `is_key_down(LeftAlt)`
is permanently false and `READ`/`FUNCTION` can never be held through it.
`app/src/hostkey.rs` installs `WH_KEYBOARD_LL`, which sees every key first,
tells left from right, and can swallow what it takes. Windows' virtual-key
codes for these *are* the machine's codes, so nothing needs translating.

| machine key | host key |
|---|---|
| `CONTROL` | either control key (`0xA2`, `0xA3`) |
| `READ` | left Alt (`0xA4`) |
| `FUNCTION` | right Alt (`0xA5`) |
| host key | **`F11`** (`0x7A`), never sent to the machine |

**The host key was right control and is `F11` as of 1.1.0**, because of a bug
report saying some keyboards do not have one -- compact and laptop keyboards
routinely drop it. That is not a small inconvenience here: the host key is how
the keyboard is captured, so a user who cannot press it cannot use the
emulator at all. `F11` is on every keyboard this runs on and the machine has
no use for it, `HELP`/`RPT`/`MENU` being `F1` to `F3`. Right control now means
`CONTROL`, the same way both shifts already meant `SHIFT`, so pressing it does
what somebody pressing a control key meant rather than nothing.

`host`+`G` capture or release, `host`+`R` reset (440 Hz triangle, 0.5 s),
`host`+`Q` quit -- after a `MessageBoxW` asking, because a screen reader can
read a real dialog.

**There is no `host`+`P` any more, and no `PATH.power`.** The power switch was
removed in 1.1.1: it confused people, and it lost their work. Switching off
suspends rather than shuts down, and **the loss is on the guest's side, not
this one** -- CE and KeySoft still hold the open document, its directory entry
and the FAT updates in RAM, and a suspend never asks them to write any of it
out because on a real machine that RAM stays alive. Quitting from suspend
throws it away, so the card is missing metadata the emulator was never given
and no amount of flushing on this side could have helped. The card image
itself was always written correctly. Everything below still stands, and the
guest can still suspend itself on its own idle timeout, so the model stays.

Three rules that are not arbitrary:

- **Capture is applied inside the hook**, not by the emulator's loop. While
  captured this holds every key on the *host*, so if releasing it needed the
  loop, a loop that stalled would take the user's keyboard with it -- no keys
  anywhere, and no way to reach a task manager. The loop is told afterwards and
  only speaks the news.
- **Nothing is taken unless the emulator's window is in front**, by process
  rather than by handle so the dialog counts as ours. That stops it eating keys
  in other applications, and it closes a trap: the host key needs focus, so
  capture surviving a focus change would leave no way to ask for the keyboard
  back. While the dialog is up the hook goes transparent, or the question could
  not be answered.
- **A key is released once every key of the keystroke has had its column
  scanned twice**, `--key-hold-ms` (800) being only a backstop. A fixed hold
  misses keys when short and repeats them when long; counting whole sweeps
  means waiting for the backstop, because the driver does not sweep on a timer.
  At a 2 s backstop one tap of an arrow arrived **seven or eight times**. The
  measurement that settles it is `keybd_event` in coredll, `0x03f72328`: **two
  hits per press**.

**A chord is pressed in two stages, and the key that joins gets its own
edge.** Both halves were bugs, both looked like "READ does nothing", and
neither had anything to do with `READ` itself -- only with where it sits.

- **Modifier first, then the key.** The driver sweeps the matrix in column
  order and posts what it finds, so one sweep that finds a modifier *and* a
  letter posts whichever comes first by column. `READ` is in hardware column
  10 and `FUNCTION` in 11, while a letter is almost always lower, so KeySoft
  saw the letter first with nothing held: `READ`+`D` arrived as a bare `D` and
  opened the database manager. `CONTROL` and `SHIFT` were never affected and
  looked perfectly fine, because both are in **hardware column 0**.
- **The key that joins needs an edge of its own.** The driver scans when the
  key-down line falls, and the modifier is already holding it low, so without
  a fresh edge the new key waits for a sweep that never comes. Measured: with
  `SHIFT` held, the letter's column was scanned twice; with `READ` held, **not
  once**. Same reason again -- `SHIFT` is column 0, so the sweep that has just
  seen it has not yet reached the letter and picks it up in the same pass;
  `READ` is column 10, by which point the letter's column is long gone.
  Pulsing the key-down line when the key joins fixes it.

Verified at coredll's `keybd_event`, `0x03f72328`: `READ`+`D` now delivers
`0xA4` down, `0x44` down, then both up -- the same shape as a `SHIFT` chord
that works, and released because the guest had seen it rather than on the
backstop.

**The keyboard is right all the way into KeySoft's command lookup.** Traced at
KeySoft's window procedure `0x000f1fac`, which handles `WM_KEYDOWN` and
`WM_SYSKEYDOWN` and skips the four modifiers by code. Pressing `READ`+`D`
queues `msg 0x104 wParam 0xa4` then `msg 0x104 wParam 0x44`, both with `lParam`
bit 29, the Alt-context flag.

**KeySoft does not track the modifiers from the messages.** While processing a
key it asks the system: `0x000f2b24` calls `GetKeyState` for `0x11`, `0x10`,
`0xa4` and `0xa5` in turn and builds a flag word at `0x002f7984` -- `CONTROL`
`0x400`, `SHIFT` `0x200`, **`READ` `0x800`**, `FUNCTION` `0x100`. So the
modifier must still be *physically held* when KeySoft gets round to the letter,
which is well behind the driver. The emulator therefore keeps the modifiers
down for `MODIFIER_TAIL_MS` after the key comes up.

Measured, that word reads `0x800` and the value handed to the command lookup at
`0xf2c44` is **`0x0844`** -- `READ` together with `D`, correctly formed.

**The lookup returns nothing for it.** `0x0844` gives 0, as does a bare `d`;
`READ` alone gives `0x2e`. With no match KeySoft falls through and posts the
key on to the application, which is why the machine simply re-announces where
it is. So the chord reaches KeySoft intact and **is not bound in this table**.

The keyboard mode at `0x002d0ea0`, which picks one of three translations, is
about French and not about the machine: `0x0011b9e8` reads the LCID, and
`lcid & 0x3ff == 0x0c` is French, sublanguage `0x0c00` French Canadian, giving
modes 1 and 2. English leaves it 0, which is correct here.

**Next**: find where `READ`+`D` *is* bound. The table consulted at `0xf2c44`
is not it. The help strings ("press READ with T", "press READ with K") sit
beside `KeyWord` and `KeyPlan` in `strings_en.pll`, which suggests the
bindings are per-application rather than global, so the thing to find is what
`0xf2c44` searches and what else searches it.

**Everything the host key does is invisible, so it is spoken** --
`app/src/announce.rs`. NVDA first, through `nvdaControllerClient.dll` loaded at
run time with `libloading`, so nothing links against it and nothing fails to
start without it; the system speech engine otherwise. The DLL is **not** in
NVDA's own install directory -- it ships bundled with applications that use it
-- so it has to be put beside `vbnote.exe` to get the good voice.

## Reset, the power switch, and the erased boot block

`host`+`R` starts the machine over, and getting there found a real fault
underneath. The power switch this describes is **no longer reachable by the
user** -- see above for why -- but the guest still suspends itself on its own
idle timeout, so all of this is still live.

- **The guest erases the block the bootloader lives in**, a few seconds into
  every boot. The erase asks for `0x20000`; `bus_block_size()` is 256 KB
  (128 KB per device times an interleave of two), so that is the same block as
  the reset vector at zero. Nothing noticed while nothing ever read the vector
  twice. A reset reads it again, finds `0xFFFFFFFF`, which is an undefined
  instruction, which vectors to `0x4`, where the next fetch is also
  `0xFFFFFFFF` -- **a machine that resets into a tight loop on the
  undefined-instruction vector**, all registers zero, mode `0x1b`. It looks
  exactly like a crash and is nothing of the kind. Whether the real part has
  smaller parameter blocks at the bottom, or the boot block is locked, is
  **still open**; a reset works round it by putting the provisioned image back.
- **The power control is a switch, and it is wired to sleep rather than to
  power.** It stays where it is put, so the pin is a level; what the machine
  acts on is the *transition*. Down is a falling edge, which the OAL has armed
  while running, and the guest suspends. Up is a rising edge, and `PRER` and
  `PFER` both read `0x00004001` when it sleeps -- GPIO 0 wakes it on either
  edge -- so flipping it back up is a wake event. The machine is never off in
  the sense of having no power, which is why alarms and recordings go on
  working while the switch says off. Modelling it as a momentary button was
  tried and is wrong in a way that looks right: the guest suspends, because a
  press is what it saw, and then nothing holds the switch down.
- **A true resume is not implemented, and the pieces for one are known.** At
  the moment it sleeps the guest leaves `PSPR = 0xa00c2000`, a real SDRAM
  address to resume at, and `PWER = 0x80004001`, arming the RTC alarm and
  GPIO 0 among its wake sources -- which is exactly how an alarm still goes
  off while the machine is "off". Setting `RCSR` bit 2 (`SMR`, sleep-mode
  reset -- **not** "software", which is what this constant used to be called)
  does make the firmware take its resume path, and that path goes somewhere
  this emulator cannot follow: the machine comes back running and completely
  silent, for as long as it is left. Feeding it `PEDR` and `PSSR` as well does
  not change that. So waking is a cold boot for now, which loses the open
  document; the real machine says **"Resuming edit of %s"** (`strings_en.pll`
  at `0x1ea1a`, sitting directly under `KeyWord`), and that string is the
  target to aim at. A plain reset from the sleeping state boots normally,
  which says the sleep state itself is fine and it is the resume path that is
  unsatisfied.
- A reset is a **cold start**, and there is now one description of what that
  means (`cold_start`). Setting only the program counter is not enough, and
  fails in an unrelated-looking way: with no stack pointer the first call
  writes through zero.
- **The flash chip has to be reset too.** The guest leaves it in a status or
  query mode, and a fetch from a part in that mode is not the array.
- **A suspend is not the end of the run** when anything can still switch the
  machine back on -- somebody at the keyboard, or a script poking files. It
  used to end it, which is why the power switch looked like a crash. While
  asleep nothing is stepped, so **guest time stands still**: anything polled on
  a cycle count stops being polled, which briefly made the one command that
  wakes the machine the one command it could not receive.
- `touch PATH.reset` and `touch PATH.power` do what the host chords do, for a
  run with nobody at the keyboard. This is how the above was tested.

## Audio underruns, and a number that was lying

Most of the "underruns" were **silence**. An empty queue is the normal state of
a machine that is not saying anything, and the counter was incrementing for
every empty callback -- thousands of them on a completely healthy run, which
sent a real investigation chasing a fault that was not there. It now only
counts a gap in something the guest was **in the middle of producing**, which
is the only kind that can be heard.

What was real, and is fixed:

- **The cushion was a tenth of a second and the shortfall is a stall.** The
  interpreter drops to 92-95% of real time while the machine is busy, and a
  machine that is busy is usually a machine that is talking. During a stall
  there is no audio to stretch, so what rides it out is having more already in
  hand: the cushion is now a quarter of a second. It is the machine's own
  voice, not a key click, so the latency is not felt.
- **Recovering from a dropout no longer waits for the whole cushion**, which
  would have turned every gap into a quarter-second silence -- worse to hear
  than the gap that caused it. The first fill waits for the cushion; a recovery
  waits for a quarter of it.
- **The resampler does the two rates and nothing else.** A controller that
  nudged the ratio to hold the queue at the cushion was tried and **removed**:
  queue depth is not a rate deficit. A machine saying nothing has an empty
  queue -- the normal, correct state -- and the controller read that as
  starvation and stretched for as long as it lasted. Measured over a boot it
  ran between **-3% and -6%**, pinned at its own clamp much of the time, and
  swung about three percent within a burst, which is heard as a phrase
  climbing in pitch as it goes on. A dropout is a fault; a voice that wanders
  is also a fault, and that cure was worse than the disease. There is a test
  named for it.

Measured with the cushion and the honest counter, at both 63 and 52 MHz and
again after the resampler was taken back out: **0 underruns** across a boot
with 12.1 seconds of speech, and the recording transcribes cleanly.

The remaining truth underneath is the known one: the interpreter cannot always
hold real time, and that is the JIT in "Open problems". None of this makes it
faster; it makes the shortfall inaudible.

## While the machine is still quiet

A boot is ninety seconds of silence and a reset starts another one, which is
indistinguishable from a machine that has failed to start. Until the guest's
first sample the emulator beeps for it: 2 kHz, 50 ms, every 5 s, at -20 dB.
It stops by itself, at the moment the first guest samples arrive.

## The card is written as the machine runs

`app/src/cardfile.rs`. The image used to be written **once, at exit**, so any
other ending -- Ctrl-C, a crash, a hard kill -- lost every document typed since
the run began, while KeySoft cheerfully reported them saved. Rewriting 128 MB
on a timer is not the fix either.

So `SdCard` remembers which blocks were written, hands them over as runs of
consecutive blocks, and the runner seeks and writes just those every two guest
seconds, then `sync_data`. A boot costs about **206 blocks in 2 flushes**.
Verified the only way that means anything: `taskkill /F` mid-run, and the card
still boots to the Main Menu.

## What is on the flash disk

Measured from a provisioned card, and it decides what any file-transfer code
has to speak:

- CE's layout is an extended partition holding a volume at **LBA 288** whose
  partition byte says `0x06` while **the BPB says FAT32**. Trust the BPB.
- **512-byte clusters** (one sector), 259,778 of them, and **one** FAT of 2046
  sectors -- no second copy to fall back on if a write goes wrong.
- **Long filenames are in use**, with 8.3 aliases beside them: `Read Me for
  KeySoft 8_00.kwt` is `README~1.KWT`.
- KeySoft's folders are `General` (the user's documents), `Keylist`,
  `Keybase`, `Dictionaries` and the rest.
- A `.kwt` is a structured format -- it opens with the document's name in
  UTF-16LE, then a zeroed header -- but **plain `.txt` sits on the card and
  KeyWord opens it directly** (`General\xbase.txt`). So moving files is the
  whole job; no format converter is needed to make transfer useful.

## The CompactFlash slot

The route for getting files in and out, and the go/no-go is **measured**:

- The ROM carries the whole stack as XIP modules: `pcmcia.dll` at
  `0x022c0000`, `atadisk.dll` at `0x03e10000`, `mspart.dll` at `0x03db0000`,
  `fatfsd.dll` at `0x03f20000`.
- `Drivers\BuiltIn\PCMCIA` loads `PCMCIA.dll` with IClass
  `{6BEAB08A-8914-42fd-B33F-61968B9AAB32}` -- PCMCIA Card Services.
- `System\StorageManager\Profiles\CompactFlash` says `Name = PCMCIA/Compact
  Flash Device`, **`Folder = CompactFlash`** so it mounts as `\CompactFlash`,
  `PartitionDriver = mspart.dll`, with AutoMount, AutoPart and AutoFormat.
- KeySoft already probes `\CompactFlash\pdiboot.exe`, `\PC card\pdiboot.exe`
  and `\SD card\pdiboot.exe`, so it knows the slot exists.
- **`pcmcia.dll`'s DllMain `0x022cc8d4` is hit once on a normal boot;
  `atadisk.dll`'s `0x03e117bc` is not, and card space `0x2000_0000` is never
  read.** That is a live socket driver with an empty slot: card services
  initialises, and the ATA driver is loaded only when a card is detected. The
  work is presenting a card, not making CE care about one.
- **The board has two slots**, a PC Card slot and a CF slot, which is what
  `pcmcia.dll`'s table describes as socket 0 and socket 1. Only socket 0 is
  wired in the emulator so far; which slot is which is not yet established.
- Not modelled yet: card space is **not in the memory map at all**, and the
  static memory controller at `0x4800_0000` is a plain register file, so
  `MECR`/`MCMEM0`/`MCATT0`/`MCIO0` are absorbed silently. Socket registers
  being *mapped* is why an unmapped-access report cannot see them.

## USB mass storage, the other way in

**This ROM has the whole USB host stack, and an earlier note here saying it
did not was wrong.** That claim came from grepping a list of guessed
filenames -- `usbdisk.dll`, `MassStorage`, `UHCI` -- and concluding from
their absence. The module table settles it instead:

| module | at | |
|---|---|---|
| `ohci.dll` | `0x0233_0000` | USB host controller |
| `usbd.dll` | `0x0396_0000` | USB bus driver |
| `usbmsc.dll` | `0x0394_0000` | mass storage class |
| `usbdisk6.dll` | `0x0393_0000` | the block device above it |

And it is *this board's* driver, not a generic one left in the image: the
OHCI PDD carries its own source path,
`c:\wince420\platform\gandalf\drivers\usb\ohcd\ohcdpdd\ohcdpdd.cpp` --
`platform\gandalf`, which is where this crate's name comes from. It is a
built-in driver, `Drivers\BuiltIn\OHCI`, configured with `MemBase` and `Irq`,
and `Mass_Storage_Class` and `Drivers\USB\ClientDrivers` are both in the
registry. The machine has two USB host ports.

**The OHCI driver is not merely loaded, it has brought the controller all the
way up**, measured on a normal boot with the registers still unimplemented:

| register | | what the driver did |
|---|---|---|
| `0x4c000008` | HcCommandStatus | wrote 1, host controller reset |
| `0x4c000018` | HcHCCA | wrote `0xa0105000`, a real SDRAM address |
| `0x4c000034` | HcFmInterval | `0x48700000`, standard 1 ms framing |
| `0x4c000010` | HcInterruptEnable | `0x80000000`, master interrupt enable |
| `0x4c000050` | HcRhStatus | `0x00010000`, root hub power on |
| `0x4c000048` | HcRhDescriptorA | 11 reads, asking how many ports it has |
| `0x4c000064` | `UHCHR` | 9 reads and 8 writes, the reset and power sequence |

**`0x4c000064` is `UHCHR`, not port status.** The PXA270 puts three of its own
registers above the OHCI block -- `UHCHR` at `0x64`, `UHCHIE` at `0x68`,
`UHCHIT` at `0x6C` -- and a first reading of this log against the standard
OHCI map alone called it `HcRhPortStatus[1]` and concluded the driver was
polling for a device. It was not. `HcRhPortStatus` at `0x54` was **never
touched at all**, which is the stronger version of the same conclusion: told
it had no ports, the driver never looked at one.

It also claims its interrupts: `RequestSysIntr` at `wce32ddk.dll` `0x02251070`
is called with **IRQ 3 and IRQ 2**, both from `ohci.dll`, which is the two USB
host ports. (The other claims on a boot are IRQ 22 from `bvdmain_serial.dll`
and IRQ 10, the modem's shared GPIO source.)

**The client controller is live too.** `bvd_udc_ser.dll` at `0x0231_0000`
touches the PXA270 UDC at `0x4060_0000` on every boot -- that is the Mini-USB
port, in device mode. Not modelled, and the obvious use for it later is
carrying USB over IP.

`crates/pxa270/src/ohci.rs` models the controller now. With `HcRhDescriptorA`
reporting **two** ports, a boot ends with both root hub ports **powered**
(`0x00000100`) instead of the driver never looking: it found them and turned
them on. What is still missing is a device to plug in, which needs the
endpoint and transfer descriptor lists walked so control and bulk transfers
actually happen.

**It sees nothing because the emulator answers `HcRhDescriptorA` with zero,
which says the root hub has no ports.** The driver believed it, correctly.

By contrast `pcmcia.dll` **never calls `RequestSysIntr` at all**, and touches
neither the CPLD nor GPIO nor card space on a boot with a card in the slot.
Its init succeeds -- `0x022c7cf8` returns 1 -- but nothing follows, which
points at its socket enumeration finding **zero sockets** rather than at a
missing detect signal. That is a deeper problem than a wrong interrupt.

The trade against CompactFlash, honestly:

- **CF is one unknown from working** and the unknown is board-specific and
  undocumented: which interrupt announces card detect.
- **USB has no unknown of that kind.** OHCI is a published specification and
  a device appearing is a root-hub port status change *inside the controller
  being emulated* -- there is no board secret to reverse. But it is far more
  code: endpoint and transfer descriptor lists, enumeration over control
  transfers, bulk transport, then SCSI on top (`INQUIRY`, `READ CAPACITY`,
  `READ(10)`, `WRITE(10)`, `TEST UNIT READY`, `REQUEST SENSE`).

**USB is now the route, and CompactFlash is the fallback** -- the reverse of
what the first version of this section said. The evidence moved: the USB
driver is running and asking for a device, and the PCMCIA driver is running
and asking for nothing. A live driver polling a port beats an inert one, and
what it polls is specified rather than reverse-engineered.

None of the CompactFlash work is wasted. The image-as-device with a host
directory as interchange, the volume builder, the export, the host key, the
size question in the wizard -- all of that is the same whichever transport
carries the sectors. What changes is the bottom layer: OHCI and bulk-only
transport with a SCSI subset (`INQUIRY`, `READ CAPACITY`, `READ(10)`,
`WRITE(10)`, `TEST UNIT READY`, `REQUEST SENSE`) in place of a CIS and an ATA
task file. `crates/gandalf/src/pcmcia.rs` stays; it is finished and tested,
and it is what to come back to if USB disappoints.

**Do not record "X is not in this ROM" again from a filename search that came
up empty** -- check the module table.

The intended shape, so it does not get redesigned by accident:

- **The image is the device; a host directory is the interchange.** Building a
  volume from scratch needs only a sequential writer -- format, lay files down
  contiguously, append directory entries -- and reading one back needs only a
  reader. **A general-purpose FAT writer is never required.**
- **Not vvfat.** Synthesising FAT live over a directory and mapping guest
  sector writes back to files is what QEMU's read-write vvfat does, and it is
  experimental for good reason. This is GPL-2.0 so that source is available,
  but not for the write path, and not under a blind user's documents.
- **The CF is a transfer volume, not the store.** Documents live on the Flash
  Disk. A bug here loses a copy, not the work.
- **Export is idempotent and resumable, never exit-only work.** Eject, quit,
  and *next start if the last run did not get there*, found by a marker. The
  card image itself is flushed as it runs, the same as the SD card, because
  `taskkill /F` is an ending too -- see the section above for why that is not
  a hypothetical.

## Asking the drive how much space is free

Reported as a lockup, and it is not one: the machine is reading the whole
allocation table, because that is what free space means on FAT32. Measured by
counting SCSI opcodes rather than guessed -- `READ+I` on the drive
information screen gave **2,783 `READ(10)`s and 5,669 `TEST UNIT READY`s**,
with **zero blocks asked for twice**, and the highest block read was the last
sector of the second table. A driver walking forward through 8,032 sectors at
about **185 commands a second**, not a driver stuck.

So the fix is fewer sectors of table, and the lever is the cluster size:

- `sectors_per_cluster` takes the **largest cluster that still leaves 70,000
  clusters**, rather than following Windows' size bands. A 256 MB drive was
  landing on 512-byte clusters and an 8,032-sector table; it now gets 2 KB
  clusters and 2,048. Same repro afterwards: **3,183 commands against 8,462,
  1,023 reads against 2,783**, and it finishes comfortably inside the window
  where it used to still be going.
- 70,000 rather than the bare 65,525 minimum, because a volume sitting
  exactly on the line is one that some other arithmetic rejects.
- **A bigger drive is slower to answer this question**, and there is no
  cluster size that fixes it: 32 KB is FAT32's largest, so 32 GB means a
  million clusters and an 8 MB table however it is laid out. A reason to keep
  the default modest.

The counting itself is worth keeping: `UsbDisk` records opcodes, repeated
reads and the highest block, and the bring-up report prints them. A retry
loop and a long job look identical in a total and nothing alike in that
breakdown.

## The clock

`RCNR` counts seconds from **midnight on 1 January 2010**, not from 1970: left
at zero the machine announces exactly that date and asks the user to set the
clock. The emulator seeds it from the host's **local** time at startup
(`app/src/hostclock.rs`), which is what the backup cell does on a real one --
CE's OAL hands the kernel local time and `GetSystemTime` takes the bias back
off, so UTC would be wrong by the offset. Without this the machine asks three
clock questions out loud at every boot, to somebody who cannot see the screen.

## Open problems

1. **Speed.** 65 M cycles/s, 15.3 ns an emulated cycle, against 3.2 ns for a
   real PXA270. The cheap wins are taken: device ticking was over half the
   time and is batched, the runner's per-instruction bookkeeping is gone, and
   the real workload now runs within 25% of a microbenchmark executing nothing
   but `add`. The remaining **4.6x is a JIT**. Cranelift is
   `Apache-2.0 WITH LLVM-exception`, which the exception makes GPLv2
   compatible, so it is available. Do not re-measure the MMU: it costs
   nothing, the TLB hit path is a tag compare.

2. **Nothing is driving the modem's line.** It answers AT commands with `OK`
   and identifies itself, which is all first-run setup needs. Real internet
   means handing bytes to the host; the seams are `Modem::feed` and
   `Modem::take_tx` and nothing else has to change.

**Any card populated before the SD fix is damaged.** Files written while the
card model was stuck in `rcv` got broken cluster chains: the directory entry
has the right size and the data is not there. `error reading uni code table
file` was that, and a rebuilt card has the file byte-identical to
`roms/Windows/unicode tables.uct`. Rebuild rather than debug.

## How setup got past its last question

Recorded because both causes were mis-diagnosed for a long time, and the
symptom -- **the machine answers every question, then goes quiet** -- pointed
at neither.

- **The keyboard.** Keys were held for a fixed span of guest time. What that
  has to outlast is the driver's scan interval, which is the guest's business.
  At 100 ms the driver saw four keys in a whole run; at 600 ms it saw one
  press as **seventy-six**, because holding that long starts auto-repeat. The
  fix is to release once `keyboard.scans_seen` has risen by two -- the guest
  has looked, and has had a scan to debounce against. Two `keybd_event` calls
  per press, which is one down and one up, is the number that means right.
- **The modem.** See above. This is what the machine was actually waiting on:
  it asks the modem its country, gets no interrupt, and never asks anything
  else.

Neither shows up as a hang. `WaitForSingleObject` looping on a 1000 ms
timeout, 871 timeouts against 1 signal, is an idle housekeeping thread and
says nothing about the fault; the AC97 link being switched off is what a
notetaker does when it has nothing to say, not a symptom. **A process that is
scheduled and waiting correctly can still be waiting for something that will
never come.** Find the device that was asked a question, not the thread that
looks stuck.


## Working practices that have paid off

- **Measure, do not infer.** Several wrong conclusions came from reading a
  disassembly and assuming a path ran. `--debug` with `count` settles it.
  A conditional instruction (`movne`, `moveq`) is not proof of execution.
- **A breakpoint that is hit is not a value that was produced.** Check the
  register.
- Read a ROM module as a **PE file** (`tools/ceextract.py`) rather than
  scraping the 51 MB image.
- `roms/Windows` is a **different build** from `roms/NK.bin` — same routine at
  a different address. Confirm any address from it against the ROM.
- Prefer writing analysis scripts to `work/` over shell heredocs; quoting
  failures have cost real time. Beware a scratch `dis.py` shadowing stdlib.
- The repo is **CRLF**. Do not convert files to LF.
