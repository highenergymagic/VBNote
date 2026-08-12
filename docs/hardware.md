# VoiceNote / BrailleNote mPower hardware notes

Everything here was derived from `EBOOT.bin` and `NK.bin` (KeySoft 7.5 build 31,
image dated 2009-02-18). It is the reference for what the emulator has to model.
Where a fact came from a specific string or structure in the image, that is
noted, so it can be re-checked.

## Platform identity

| Fact | Value | Source |
|---|---|---|
| SoC | Intel PXA270 "Bulverde", XScale, ARMv5TE | EBOOT: `CPU ID = A0 Bulverde`; drivers export `PXA27X::GPIO`, `::INTC`, `::OST`, `::MEMC`, `::DMAController`, `::I2C`, `::SDMMC`, `::LCDController` |
| OS | Windows CE 4.20, retail, ARMV4I | PDB paths `C:\WINCE420\platform\GANDALF\target\ARMV4I\retail\` |
| BSP | "Gandalf", vendor namespace `Prolificx` | OAL string `Gandalf Memory Map Table`, `c:\prolificx\pcdisk\pccard.cpp` |
| Bootloader | HumanWare, built 2007-06-15 | EBOOT: `BrailleNote mPower Bootloader`, `Copyright (c) 2005 HumanWare Ltd` |
| SDRAM | 64 MB at PA `0xA0000000` | OEMAddressTable |
| Boot flash | 64 MB NOR at PA `0x00000000` (nCS0), XIP | OEMAddressTable; `nCS0: BOOT ROM` |

## ROM image layout

```
NK.bin      B000FF record format, image base 0x80041000, 42.5 MB, 460 records
            ROM signature "ECEC" at 0x80041040, pTOC = 0x82AC4434
            245 modules, 262 files, nothing compressed
EBOOT.bin   B000FF, 90 KB, lives at flash offset 0
```

`NK.bin` sits at flash offset `0x41000`, which is where the image base
`0x80041000` lands given that nCS0 maps to VA `0x80000000`.

## OEMAddressTable

Extracted from `nk.exe` at file offset `0x1030`. `VA` is the Windows CE static
kernel mapping; `PA` is what the emulator must decode.

| VA | PA | Size | What |
|---|---|---|---|
| `0x96C00000` | `0xA0000000` | 64 MB | SDRAM |
| `0x80000000` | `0x00000000` | 64 MB | nCS0 boot NOR flash |
| `0x9AD00000` | `0x04000000` | 1 MB | nCS1 — DiskOnChip socket, not modelled |
| `0x9AE00000` | `0x0C000000` | 1 MB | nCS3 — DiskOnChip socket, not modelled |
| `0x9AF00000` | `0x10000000` | 1 MB | nCS4 — **CPLD**, custom glue |
| `0x84500000` | `0x40000000` | 32 MB | PXA27x internal peripherals |
| `0x86500000` | `0x3C000000` | 64 MB | PCMCIA / CF |
| `0x8A500000` | `0x38000000` | 32 MB | PCMCIA / CF |
| `0x8C500000` | `0x30000000` | 32 MB | PCMCIA / CF |
| `0x8E500000` | `0x2C000000` | 64 MB | PCMCIA / CF |
| `0x92500000` | `0x28000000` | 32 MB | PCMCIA / CF |
| `0x94500000` | `0x20000000` | 32 MB | PCMCIA / CF |
| `0x84000000` | `0x5C000000` | 1 MB | Internal SRAM |
| `0x84100000` | `0x58000000` | 1 MB | — |
| `0x84200000` | `0x4C000000` | 1 MB | USB host (OHCI) |
| `0x84300000` | `0x48000000` | 1 MB | Memory controller |
| `0x84400000` | `0x44000000` | 1 MB | LCD controller |
| `0x9AC00000` | `0x50000000` | 1 MB | Camera interface |
| `0x96500000` | `0xE0000000` | 1 MB | XScale coprocessor space |

The ROMHDR reports `RAMStart = 0x96D2C000`, `RAMFree = 0x96D5B000`,
`RAMEnd = 0x9AC00000`, which is consistent with 64 MB of SDRAM based at
`0x96C00000`.

Everything except nCS1, nCS3 and nCS4 is a stock PXA270 map.

## I/O inventory

The mPower's peripherals, from the hardware itself rather than the firmware:

| Device | Notes |
|---|---|
| Keyboard | braille on BT models, QWERTY on QT |
| RS-232 | one DB-9 |
| USB | two host ports, one client port |
| Storage slots | SD, PCMCIA, CompactFlash |
| Modem | V.90 56k, internal |
| Bluetooth | internal module |
| IrDA | internal module |
| Audio | AC97 codec: internal speakers and microphone, headphone and mic jacks |
| FM radio | internal module |
| Controls | power **switch** on GPIO 0, momentary record button, momentary reset button |

This accounts for all three PXA UARTs and settles what each is:

| UART | Wired to | Evidence |
|---|---|---|
| FFUART | the DB-9 and the bootloader console | EBOOT prints its menu here |
| BTUART | **Bluetooth**, as its name says | `bthuart.dll` executes; the traffic is an HCI_Reset |
| STUART | IrDA, or unused | touched but never written to |

BTUART carries `01 03 0c 00`, sent **three times** — a probe and two retries
against something that never answers. That is byte-for-byte an HCI_Reset:
H4 packet type `01` (command), opcode `0x0c03` (OGF 3, OCF 3, Reset),
parameter length `00`. The same four bytes appear as a **literal** in
`bthamb.dll`, the AmbiCom Bluetooth transport driver in this ROM.

This took two wrong turns worth recording.

First it was read as HCI_Reset on the byte pattern alone, which is not
evidence. Then attributing the transmit PC pointed at `bvdmain_serial.dll`,
and that was taken as proof of a braille device. It is not: the registry
registers `bvdmain_serial.dll` as the stream driver for
`Drivers\BuiltIn\Serial` ("Serial Cable on COM1:"), `Serial2` ("Serial Cable
on SP2:") and `IrDA` ("Native IR:"). It is the board's *generic* serial
driver, shared by every client, so its PC names the driver and never the
caller.

What settles it is whether the Bluetooth stack runs at all. Recording every
64 KB region the guest executes, then matching those against the ROM's module
table, shows `bthuart.dll`, `bthuniv.dll`, `btsvc.dll`, `btd.dll`,
`btdrt.dll` and `bthdrv.dll` all executing. The registry agrees:
`SOFTWARE\Microsoft\Bluetooth\Transports\BuiltIn\1` sets `driver =
bthuart.dll` and `Name = COM5:`. So the bytes are an HCI_Reset, sent by the
Bluetooth stack, to a Bluetooth module that is not modelled.

The same region scan shows `brltrn.dll`, the braille translation module,
**never executes**, and no unmodelled peripheral window looks like a display
port. So the braille display is not on BTUART, and where it is remains
unanswered.

The lesson stands, just aimed one level further down: byte patterns are not
evidence of a protocol, and neither is a program counter inside a shared
driver. Whether a module *ran* is evidence.

## Custom hardware to reverse

### CPLD at PA `0x10000000` (nCS4)

The centre of the board glue. `cpld2.dll` maps it with `MmMapIoSpace` and
exports a small C++ class, so the register interface is byte-addressed with
16-bit values:

```
CPLD::CPLD(unsigned long, unsigned long)
CPLD::Read(unsigned char) -> unsigned short
CPLD::Write(unsigned char, unsigned short)
CPLD::BitHigh(unsigned char, unsigned short)
CPLD::BitLow(unsigned char, unsigned short)
GetCPLDWord(unsigned short)
SetCPLDWord(unsigned short, unsigned short)
SetCPLDPin(unsigned short, unsigned short, int)
```

Known consumers, which is where the register semantics will come from:

| Module | Also uses | Purpose |
|---|---|---|
| `pdikeybd.dll` | PXA GPIO, OST, `RequestSysIntr` | keyboard matrix scan |
| `bvdmain_serial.dll` | `SERIAL.dll`, `SetCPLDPin`, INTC, CLK | braille/serial device path |
| `battdrvr.dll` | PXA GPIO | battery and charge status |
| `PwrButton.dll`, `RecordButton.dll` | | front panel buttons |

### CPLD registers observed so far

Gathered by running EBOOT to its boot menu and logging every access. Offsets
are from the CPLD base at PA `0x10000000`. Registers are 16-bit and accessed
with `strh` / `ldrh`.

| Offset | Traffic to the boot menu | First PC | Reading |
|---|---|---|---|
| `0x400` | 2337 writes, 0 reads, last `0x0008` | `0x96c88448` | **the braille shift register.** Data `0x10`, clock `0x20`, strobe `0x40`, enable `0x80`; the far end of the chain is GPIO 103 |
| `0x402` | 29275 writes, 43327 reads, last `0x000c` | `0x96c88430` | **keyboard matrix.** Polled continuously while the menu waits for a keypress, with roughly three reads per two writes — the shape of "select a column, read the rows" |
| `0x404` | 4 writes, 0 reads, last `0x0501` | `0x96c88414` | written once during AC97 setup |

The three are driven by one dispatch function at `0x96c883f0` that takes a
selector of 0, 1 or 2 and writes the same value to `[base]`, `[base+2]` or
`[base+4]` — for **two** base addresses held in a table, so the design has a
pair of these register blocks.

`0x402` is the most valuable of the three: it is the input path, and it is
being exercised by firmware we can already run, which is exactly the
trap-and-log position we wanted to reach.

### The keyboard is interrupt-driven, on GPIO 11

EBOOT polls the matrix; **Windows CE does not**. Over a whole CE boot the scan
register at `0x402` sees 76 reads and 51 writes, against 43327 reads while
EBOOT's boot menu waited for a key. So `pdikeybd.dll` waits on an interrupt
and only scans when it fires, and pressing a key into the matrix on its own
does nothing at all.

Which interrupt: the kernel arms edge detection on GPIOs 0, 11, 13, 18, 20,
69, 101 and 105 — all through one shared OAL helper, so the program counter
says nothing about which driver wanted which pin. Testing them instead does.
Pulling each candidate low alongside the key press:

| Pin | Scan register traffic | Scans that saw a key |
|---|---|---|
| **11** | **3480 reads, 2351 writes** | **6** |
| 18 | 77 reads, 51 writes | 0 |
| 20 | 77 reads, 51 writes | 0 |
| 101 | 76 reads, 51 writes | 0 |

**GPIO 11 is the key-down line.** `pdikeybd.dll`'s init also constructs its
device object with the constants 10 and 11, which fits.

The driver now sees keystrokes. KeySoft still says nothing further, so
whatever it is stuck on, it is not waiting for a key.

### Keyboard matrix (decoded)

`pdikeybd.dll` carries three scan tables, one per keyboard layout, matching
the driver's own `Assigning the ... ScanCode to VKeyTable` messages:

| File offset in `pdikeybd.dll.pe` | Layout |
|---|---|
| `0x1020` | US / VoiceNote |
| `0x1080` | Canadian QWERTY |
| `0x10e0` | French |

Each table is 96 bytes: **12 columns of 8 bytes**, 7 usable entries per column
plus a zero terminator. The values are **Windows virtual-key codes**. Column 1
of the US table decodes as:

```
20 d8 da 43 33 45 53 00     SPACE  ..  ..  C  3  E  S
00 d7 d9 58 32 57 41 00      ..    ..  ..  X  2  W  A
00 00 00 42 35 54 46 00      ..    ..  ..  B  5  T  F
09 00 00 56 34 52 44 00     TAB    ..  ..  V  4  R  D
2e 25 db 4d 37 55 48 00     DEL  LEFT  [   M  7  U  H
c0 00 00 4e 36 59 47 00      `     ..  ..  N  6  Y  G
00 27 dc be 39 4f 4b 00      .. RIGHT  \   .  9  O  K
00 28 dd bc 38 49 4a 00      ..  DOWN  ]   ,  8  I  J
00 08 0d a5 bd bb ba 00      ..  BACK ENTER RALT - + ;
a4 26 de bf 30 50 4c 00    LALT   UP   '   /  0  P  L
```

The QWERTY finger columns are plainly visible — `3/E/D/C`, `2/W/S/X`,
`5/T/G/B`, `4/R/F/V` — confirming this is the QT keyboard rather than the
braille one.

### I2C: a Smart Battery, and nothing else

The OAL drove the 0x40300000 window hard and, unmodelled, got nowhere:

| Register | Traffic | First PC |
|---|---|---|
| `0x40300008` | 270 writes | `0x800848b0` |
| `0x40300010` | 540 reads, 1347 writes | `0x8008469c` |
| `0x40300018` | **5400 reads**, 268 writes | `0x80084828` |
| `0x40300020` | 270 writes | `0x80084770` |

That is a bus transaction waiting on a status bit that never sets — write a
command, poll a status register, give up, repeat. The offsets are exactly the
PXA27x I2C registers: IDBR `0x08`, ICR `0x10`, ISR `0x18`, ISAR `0x20`.

Modelling the controller — enough to record which slave each transaction was
aimed at — answered the question in one boot. The bus has **exactly one
device, at 7-bit address 0x0B**, which is the address the Smart Battery System
specification reserves for a battery, and it is read with standard Smart
Battery Data commands: `0x21` DeviceName, `0x08` Temperature, `0x09` Voltage,
`0x0A` Current. `battdrvr.dll` agrees, carrying *"Failed to get device name"*,
*"Invalid device name length: %d"* and *"Scaling value: %d (v2)"*.

`battdrvr.dll` FUN_022019a0 parses DeviceName strictly: the block length must
be 1 to 7, and it counts trailing ASCII digits, taking a scaling factor from
them only when there are exactly four. Otherwise it falls back to `0x9c4`,
2500. `SmartBattery` reports `PD2500`, which parses cleanly and lands on that
same 2500 rather than inventing a different figure.

With the battery present, the transaction count for a boot falls from 656 to
278 — the retry loop is gone. Nothing else has ever appeared on this bus, so
the earlier guess that a braille display might hang off I2C is ruled out.

### One ROM, four machines

One ROM serves **six** machines. BT and QT name the *keyboard* — Braille
Terminal against QWERTY — not the display, and each BrailleNote comes in an
18-cell and a 32-cell version:

| Model | Display | Keyboard |
|---|---|---|
| BrailleNote BT18 | 18 cells | braille |
| BrailleNote BT32 | 32 cells | braille |
| BrailleNote QT18 | 18 cells | QWERTY |
| BrailleNote QT32 | 32 cells | QWERTY |
| VoiceNote BT | none | braille |
| VoiceNote QT | none | QWERTY |

The two halves are found independently: the display by measuring the shift
register chain, the keyboard from `pdikeybd.dll`'s scan tables. The model is
chosen at run time. Two pieces of evidence for where:

- `pdikeybd.dll` keeps a device-type value it compares against **5, 6, 7 and
  9**, and its layout tables include separate "VN" variants.
- CPLD register **`0x0E` is read once and never written**, which is what a
  hardware strap looks like. Everything else on the CPLD is either written or
  polled.

With `0x0E` reporting zero, KeySoft boots and announces *"The Braille Display
is not operating"*. `--board-id` makes the register settable, but **values 0,
1, 5, 6, 7 and 9 all produce the same message**, so this register is not the
model strap. The lead is dead; the model is selected somewhere else.

### Tracing "The Braille Display is not operating"

Traced end to end, string to function to caller. The chain:

**1. The string.** `strings_en.pll` is an ordinary PE whose big section is a
standard Win32 resource directory, so the message is an `RT_STRING`, not a
bespoke format. `tools/cestrings.py` parses it: **string id 2931 (`0xB73`)**,
resource block 184 index 3, at virtual address `0x027C8870`. An earlier note
here claimed "block 34, index 682" from a hand-rolled chain walk; that was
wrong, and the walk desynchronised because blocks are separate padded
resources rather than one chain.

**2. Who reads it.** Watching reads of `0x027C8870` catches a kernel `memcpy`,
but the call ring at that moment crosses `KeySoft.exe -> strings_en.pll`, and
`FUN_001b19b8` in KeySoft is a load-string-by-id routine (it rejects ids over
100000 and falls back to language 9).

**3. Who asks for id 2931.** Nothing in KeySoft's decompilation mentions
`0xb73`, because the code that builds it was never disassembled. Breaking on
`FUN_001b19b8` with `r1 == 0xb73` finds the caller: a **nine-instruction
function at `0x00023334`** that Ghidra does not know exists.

```
0x00023334  str  lr, [sp, #-4]!
0x00023338  mov  r1, #0x20000
0x0002333c  mov  r0, #0xb70
0x00023340  orr  r1, r1, #0x2      ; r1 = 0x20002
0x00023344  orr  r0, r0, #0x3      ; r0 = 0xb73
0x00023348  bl   0x00189700        ; Announce(id, flags)
0x0002334c  mov  r0, #0x2          ; and return 2
```

`0x00189700` is Announce: it calls `FUN_00189750`, which loads the string by
id and speaks it. So the whole function is "say it, return 2".

**Nothing branches to `0x00023334`.** A scan of every BL in `.text` finds zero
callers, which is exactly why Ghidra never disassembled it and why searching
the decompilation for the id found nothing. Its existence is confirmed by the
`.pdata` exception table, which lists it with a one-instruction prologue and a
length of nine.

**4. How it is reached.** The only pointer to it in `.text` is a literal loaded
at `0x0017bec0`, in a 540-instruction routine at `0x0017bb34` that installs
about fifty handlers:

```
mov r1, #0x1f0                 ; message id 496
ldr r2, =0x00023334            ; handler
bl  0x00012d04                 ; install
```

So it is the handler for **message `0x1F0` (496)**. `tools/ksactions.py`
recovers the whole map this way: **1696 handlers**. Message 496 has two, on
different objects — this one and `0x00124fec`, which belongs to the word
processor — so these are per-class message maps, not a global dispatch table.

**5. What dispatches it.** Every message goes through one indirect call at
`0x0001140c` (`handler = [object+0xc]; mov lr, pc; bx handler`), and the code
that reaches it is a **bytecode interpreter**: a dispatch loop at `0x001773dc`
with a jump table at `0x00177424`. That matches the string data, which is full
of KeySoft's own markup (`%s1080`, `%k111`, `%h246`).

Over an entire boot KeySoft dispatches exactly **two** messages: `0x132`,
whose handler `0x00179638` is the generic prompt runner, and then `0x1F0`.

**6. What raises 496.** `FUN_001772bc` is not an interpreter — it is a Win32
message pump with a state machine, and `Ordinal_865` is SendMessage. Messages
arrive as `WM_USER+8` with the id in `wParam`, which makes the sender
findable, and it is `FUN_00023358`:

```c
if (*DAT_0002343c != 0) {                    // this machine has a display
    if (param_1 == 1) {                      // enable it
        FUN_00023284();                      // probe
        if (*DAT_00023438 == 0) {            // cell count still zero
            SendMessage(hwnd, 0x408, 0x1f0, 1);   // "not operating"
            return 4;
        }
        ...                                  // it works
```

and the probe is:

```c
n = FUN_00026130();                          // cells reported by the hardware
if ((5 < n) && (n < 0x29)) { *cells = n; return 2; }   // 6 to 40 is a display
*cells = 0;
FUN_00023358(0);                             // otherwise switch braille off
```

**So KeySoft's whole test is a cell count between 6 and 40.** It comes from
`KernelIoControl(0x01013FA0, NULL, 0, buf, 12, NULL)` — `FILE_DEVICE_HAL`
function 4072, an OEM code answered by the OAL — whose twelve-byte reply
starts with the count.

### How the braille display is detected

The OAL handler is at `0x8007a494` in `nk.exe`, and it measures the display by
**counting the length of a shift register chain**. See `braille.rs`; the short
version is that CPLD register index 0 (byte offset `0x400`) carries data on
bit `0x10`, clock on `0x20`, strobe on `0x40` and enable on `0x80`, the far end
of the chain is wired to **GPIO 103**, and the OAL flushes the chain with 32
zero bytes and then clocks `0xff` bytes until that pin goes high. Twenty-four
bytes means an 18-cell display, thirty-two means a 32-cell one.

That corrects an earlier guess here: `0x400` is **not** on the audio path. It
was written during EBOOT's codec setup, which is what the guess rested on, but
what actually drives it is the braille bit-bang.

Modelling the chain — a shift register of the right length with its far end on
GPIO 103 — makes the count come out on its own. With it attached the
announcement stops happening: the run reaches its cycle budget instead of
reaching `0x00023334`, writes to `0x400` go from 2342 to 6949 because the OAL
is now sending real cell data, and the speech gets longer.

Two things worth keeping from this. `.pdata` gives the true bounds of every
function in the image, including the ones nothing branches to, so it can be
fed to Ghidra to fix its function list wholesale. And breaking on a
slot-relative address needs the FCSE slot as well: every EXE in this ROM links
at `0x00010000`, so `0x00023334` in slot 5 is a different program entirely —
the first attempt at this stopped there and reported nonsense.

### Keyboard scan protocol (decoded and implemented)

From `pdikeybd.dll`'s scan routine, `FUN_023025cc` in the decompilation. It
drives CPLD register index 1 — byte offset `0x402`:

```c
for (column = 0; column < 12; column++) {
    write(1, 0x0d);        // idle
    read(1);               // discarded
    delay(10);
    write(1, column);      // select the column
    read(1);               // discarded
    delay(10);
    rows = read(1);        // eight row bits
    if (column even) words[column/2]  = rows << 8;
    else             words[column/2] |= rows & 0xff;
    delay(10);
}
write(1, 0x0c);            // park
read(1);
```

Three reads per two writes, which is exactly the 43327:29275 ratio observed
while EBOOT's menu waited for a key — independent confirmation that this is
the same routine EBOOT uses.

The result is six 16-bit words: even columns in the high byte, odd columns in
the low byte, giving 12 x 8 = 96 key positions. `FUN_02301a28` then iterates
`0..0x60` over them, which is the same 96, and looks each up in the layout
table above.

Values `0x0c` and `0x0d` select no column; they bracket the scan.

Implemented in `crates/gandalf/src/keyboard.rs`. One detail is inferred
rather than proven: that row `r` is bit `r` of the byte read back. The driver
only ever treats the byte as eight bits without naming them, so if a key ever
comes out transposed within its column, that is the assumption to revisit.

### CPLD access model

`cpld2.dll` exports `BitHigh` and `BitLow`; EBOOT contains the same pair at
`0x96c88458` and `0x96c88490`. Both are read-modify-write built on
`GetCPLDWord` / `SetCPLDWord`, and `GetCPLDWord` reads a **software shadow**,
not the device. That explains the access pattern exactly:

- `0x400` and `0x404` are **write-only control registers** — zero hardware
  reads, because reads are served from the shadow.
- `0x402` is **read/write** and is the only register that takes real hardware
  reads. It is the keyboard port: written to select, read to sample.

What is still unknown is the bit-level encoding of `0x402` — whether the
column select is one-hot or binary, and which bits carry the rows. That needs
a decompilation pass over `pdikeybd.dll`'s scan routine; its `.text` begins
with the tables and UTF-16 data, so a plain linear disassembly is not enough.

### Image header EBOOT looks for

EBOOT does not boot a bare `NK.bin`. It looks for a HumanWare image header and
reports what it found:

```
ImageID: 0x%x Start: 0x%x Length: 0x%x !!!
```

**Fully decoded and verified against the running bootloader.**

The validator is at `0x96c858f0`:

```
mov r3, #0xc          ; twelve bytes
ldr r1, [r5, #8]      ; from this->offset
add r2, r5, #0x10     ; into this->header
bx  r4                ; device->Read(offset, buf, 12)
ldr r1, [r5, #0x10]   ; word 0 -> ImageID
ldm r0, {r2, r3}      ; words 1, 2 -> Start, Length
cmp r3, r0            ; ImageID == 0x45464748 ?
```

and the object is constructed at `0x96c7cf7c` with `mov r1, #0x40000`, which
is the flash offset it reads from.

| Field | Offset | Value |
|---|---|---|
| ImageID | 0 | `0x45464748` |
| Start | 4 | flash offset of the image, `0x41000` |
| Length | 8 | image size |

Confirmed by provisioning a device with it, at which point EBOOT prints:

```
ImageID: 0x45464748 Start: 0x41000 Length: 0x2A86FD0 !!!
Lauch NK.bin in ROM: 0x41000!!!
Launching image in ROM
```

`Start` is a **flash offset**, not a virtual address — the value above is what
EBOOT accepts.

### Launching the kernel

`Launch2` at `0x96c79ad0` hands control to the image:

```
mcr p15, 0, r1, c1, c0, 0   ; r1 = 0x78, MMU OFF
str r0, [r4]
mov r1, #0
mov pc, r2                  ; r2 = physical address of the tail below
...
mcr p15, 0, r2, c8, c7, 0   ; flush TLB
mov pc, r0                  ; r0 = header.Start, the image entry
```

The entry point is **`header.Start` itself**, the same `0x41000` from the
image header. And the sequence depends on the MMU disable not taking effect
until `mov pc, r2`: that branch target is a physical address, while the three
instructions before it still run virtually.

That is the same pipeline behaviour as the enable path, and it is why the
emulator applies an MMU change at the next pipeline flush rather than after a
fixed instruction count. The two sequences have different lengths, so no
single count satisfies both:

```
enable:  mcr c1 (on)  ; bx r2                     -> r2 virtual
disable: mcr c1 (off) ; str ; mov ; mov pc, r2    -> r2 physical
```

### Recovering the kernel's debug output

The CE kernel writes a boot log, but a retail build discards it. The routine
at `0x80083368` is `OEMWriteDebugByte`:

```
ldr r3, [r2, #4]    ; UART base
add r1, r3, #0x14   ; LSR
ldr r0, [r1]
tst r0, #0x20       ; wait for THRE
beq -8
ldr r0, [r2, #0xc]  ; debug output enabled?
beq write           ; enabled -> store the byte to THR
mov r0, #0x64       ; disabled -> delay 100 us
bl  OEMUdelay       ; ...and drop it
```

The kernel confirms this itself once the log is visible: it prints
`SerDBG port is disabled`. Tapping the routine on entry, where the byte is
still in r1, recovers the whole log without changing what the guest does.
That is what `--debug-byte-hook 80083368` does, and it is the single most
useful diagnostic in the project.

### Where the kernel gets to

With a provisioned device the CE kernel launches, runs, and loads drivers:

```
Windows CE Kernel for ARM (Thumb Enabled) Built on Jan 23 2006 at 13:14:29
ProcessorType=0915  Revision=3
sp_abt=ffff5000 sp_irq=ffff2800 sp_undef=ffffc800 OEMAddressTable = 80042030
OEMInit: RCSR:0x00000001
Sp=ffffc7cc
USBC *** GetSerialObject()
*****UDC Endpoint Memory configured
-- in ac97ctrlconfigure --
SerDBG port is disabled
```

Two emulator bugs stood between the kernel starting and this point, and both
were found from that log.

**ICHP was missing.** Windows CE's OAL resolves interrupts by reading the
interrupt controller's highest-priority register at `0x40D00018` rather than
scanning ICIP. With it unimplemented and reading zero, every interrupt
resolved to source 0 and the kernel logged `In ISRUnknown IRQ:0` forever.
ICHP reports the highest-priority pending, unmasked source, honouring the
IPR priority table when the guest has programmed one; bit 31 flags a pending
IRQ in bits 30:16, bit 15 an FIQ in bits 14:0.

**The TLB granule was too coarse.** ARMv5 small pages carry four independent
AP fields, one per 1 KB subpage. A TLB keyed at 4 KB lets whichever subpage
is touched first dictate its neighbours' permissions. CE depends on the
difference: `PUserKData` sits at `0xFFFFC800`, in a page whose lower subpages
are privileged-only and whose third subpage is user-readable. The kernel
touches `0xFFFFC800 - 0x34` first (it prints `Sp=ffffc7cc`), which cached
privileged-only permissions, and every later user-mode read of PUserKData
took a page permission fault:

```
pc 0x03f77f08  va 0xffffc800  fsr 0x000f (page permission)  mode 0x10
    L2 @0xa0135ff0 = 0xa013525e  (small 4K)
    AP=10 -> priv rw user ro
```

The TLB is now keyed at 1 KB, which is also the natural granule for tiny
pages and makes them cacheable.

Earlier state, before those fixes:

| | |
|---|---|
| PC | inside `nk.exe`, linked at `0x80041000` |
| CP15 control | `0x00003a7f` -- MMU, caches, and high vectors (bit 13) |
| TTBR | `0xa0130000` -- the kernel's own tables, not EBOOT's `0xa0000000` |
| Interrupts | 7767 IRQs taken; INTC mask `0x04000000`, OS timer match 0 |
| Distinct addresses executed | 733 |

The kernel takes over, builds its own address space, installs its vector
table and services the system tick for roughly 1.5 seconds of guest time.

It then stops: `OSMR0` stays at `0x005573a0` while `OSCR` runs on to
`0x03856b35`, `OSSR` bit 0 stays set, and the CPU sits in the OAL's
`OEMUdelay` busy-wait at `0x800834b8` with the CPSR I bit set. It has masked
interrupts and is polling something that never completes. No serial output
appears on any of the three UARTs, which is expected for a retail build.

Since then, three more emulator gaps were found and fixed, and the kernel now
reaches driver load.

**XScale CP14 c7 is the power mode register.** Windows CE idles by writing a
non-zero mode, which stops the core until an interrupt arrives. With the
write ignored, execution ran off the end of the sleep routine into a fallback
that toggles a GPIO forever:

```
mov r4, #3
mcr p14, 0, r4, c7, c0, 0     ; sleep
mov r4, #7
mcr p14, 0, r4, c7, c0, 0     ; deep sleep
...                            ; if we get here, blink a GPIO forever
b   -0x38
```

Waking is independent of the CPSR interrupt mask — the core restarts when the
controller asserts, and the exception is taken separately only if CPSR allows
it. CE idles with interrupts masked and depends on that.

With those in place the kernel loads drivers and reaches, over a long run:

- the USB client controller at `0x40600000` and USB host OHCI at `0x4c000000`
- CPLD registers `0x02`, `0x04`, `0x06`, `0x08`, `0x0e`, driven from a
  user-space driver at `0x03d61c30` rather than from EBOOT — the first traffic
  on those registers, and a new lead on what they do

### AC97 codec register addressing

The PXA spaces the codec's registers **two bytes apart** in its own address
map: AC'97 register N appears at controller offset `0x200 + N * 2`. Register
numbers are themselves even byte offsets (`0x00`, `0x02`, ... `0x7E`), so the
whole 64-register file is exactly the 256-byte window.

Getting this wrong is silent and total — every access lands on a different
register, readbacks make no sense, and the driver concludes the codec is dead
and leaves `ACLINK_OFF` set. The giveaway in a trace is a write of `0xAC44`,
which is 44100 and can only be the PCM front DAC rate at register `0x2C`:

| Controller offset | Register | Value | Meaning |
|---|---|---|---|
| `0x58` | `0x2C` | `0xAC44` | PCM front DAC rate, 44100 Hz |
| `0x54` | `0x2A` | `0x0001` | extended audio control, variable rate on |
| `0x4C` | `0x26` | `0x4000` | powerdown control |
| `0x30` | `0x18` | `0x0303` | PCM out volume |
| `0x04` | `0x02` | `0x0000` | master volume, full and unmuted |

### Sample rate

The AC-link always runs at 48 kHz, but the driver enables **variable-rate
audio** (register `0x2A` bit 0) and sets the PCM front DAC rate (`0x2C`) to
**44100**. With VRA on, the codec resamples internally and the data rate on
the link is the DAC rate, so 44100 is what the guest actually produces.

Treating it as 48 kHz plays everything about nine percent fast, which is
audible immediately as a startup sound that runs sharp. The codec's own rate
register is therefore authoritative: it paces the DMA credit, tags the
captured WAV, and drives the resampler feeding the host device.

### Power switch

`PwrButton.dll` waits on the GPIO 0 interrupt, reads the pin, and acts when it
reads **low**. The mPower's control is a switch, not a momentary button, so
this is a position the machine sits in rather than an event it receives: low
means the user has slid it to off, and powering down is the correct response.

Every emulated GPIO input starts at zero, so before this pin was driven the
guest saw the switch in the off position from the moment the driver loaded,
and shut the machine down a few seconds into boot. Constructing the board is
the equivalent of flicking it on.

### DMA

Audio playback on this SoC is DMA-driven, so `crates/pxa270/src/dma.rs` and
`crates/gandalf/src/dma.rs` implement the controller: 32 channels, the
DCSR/DDADR/DSADR/DTADR/DCMD register file, the DRCMR request map, and
descriptor chains fetched from memory with bit 0 of the next-address field
ending the chain. The register file lives in the SoC and the transfers in the
board, because a descriptor moves data between SDRAM and a peripheral and
only the board reaches both.

It is not what was blocking boot — `wavedev.dll` writes DCSR with zero, which
resets a channel rather than starting one, and never arms a transfer. But it
is what the startup beep will need the moment playback is attempted, and it
is tested against the audio case: a fixed target address feeding the AC97 PCM
register puts samples straight into the emulator's audio path.

### The device powers itself off

With idle handled
correctly, the kernel reaches a routine that writes CP14 power mode 3 and
then 7 — **sleep and deep sleep**. That is not idling; on a PXA it drops most
of the chip and resumes through the reset vector on a configured wake source.

So the machine boots, loads its drivers, and then deliberately powers itself
down. The emulator now says so rather than falling into the blink-forever
fallback that follows the sleep instructions.

#### Battery and charger sense pins

`battdrvr.dll` samples two GPIOs through a helper that is unmistakably the
PXA level register — GPLR0/1/2 at `0x00`/`0x04`/`0x08` and GPLR3 at `0x100`:

```c
bool read_pin(int base, uint pin) {
    uint *reg = (pin >> 5) < 3 ? base + (pin >> 5) * 4 : base + 0x100;
    return (*reg & (1 << (pin & 31))) != 0;
}
```

| Pin | Meaning | Evidence |
|---|---|---|
| **GPIO 14** | AC adapter, **active low** | `FUN_02201cac` is `return !read_pin(14)` |
| **GPIO 108** | charge status, sampled nine times | the charge thread counts highs and lows across nine reads: all high gives state 0, all low state 2, any mixture state 1 |

A line that must be sampled repeatedly to see whether it is *toggling* is a
charge indicator — the same signal that blinks a charge LED. Which steady
level means "charged" and which means "fault" is not proven; the driver only
compares the collapsed state against its previous value.

Modelled in `crates/gandalf/src/power.rs`, defaulting to mains connected.

#### Power state is not why it suspends, and neither is a timeout

Connecting the adapter changed nothing: the machine still suspended, at the
same cycle count to within 0.001%.

A wall-clock timeout looked like the answer for a while, because raising the
notional core clock let boot get much further. It is not the answer. The
numbers rule it out:

| Notional clock | Suspends at | Guest time | Prefetch aborts |
|---|---|---|---|
| 312 MHz | 8.02 G cycles | 25.7 s | 132,815 |
| 1200 MHz | 18.34 G cycles | 15.3 s | 338,552 |

A fixed wall-clock timeout would fire at a fixed *guest time*. Instead the
faster clock suspends at a **shorter** guest time having done **more than
twice the work** — the prefetch-abort count, which tracks how much code has
been demand-paged in, more than doubles. The trigger is tied to boot
progress, not to elapsed time. An earlier run that appeared not to suspend at
1200 MHz had simply exhausted its cycle budget first.

`gandalf::CPU_HZ_EFFECTIVE` is still worth having and is now the default,
because the interpreter really does retire far less work per emulated second
than a real PXA270 and everything the guest times is distorted by it. But it
buys progress, not a fix.

#### What actually calls the power-off

Breaking at the OAL's `OEMPowerOff` entry (`0x80083d2c`) and reading back the
call ring gives the chain:

```
0x02322954 -> 0x02329780      user space, slot 1
...
0x0238a2f8 -> 0x0238a3b4
0x00012358 -> 0x000160f4      slot 0, the running process
0x8004b21c -> 0x80083d2c      kernel -> OEMPowerOff
```

So a **user-mode process asks for the power-off**; the kernel is only obeying.

Resolving the whole trace against the ROM module table (`tools/modmap.py`)
shows what runs immediately before it:

```
battdrvr.dll        cpld2.dll         bvdmain_serial.dll
pcmcia.dll          wavedev.dll       bvd_udc_ser.dll
ohci.dll            coredll.dll       nk.exe
```

That is **every board driver in turn** — the device manager walking its
driver list calling PowerDown on each. In other words the trace is the
suspend itself, not its cause.

The requesting process sits in **FCSE slot 4** (`cp15 pid = 0x08000000`,
slot = pid >> 25). Every CE executable is linked at `0x00010000` and mapped
into its slot, so neither an address nor the ROM module table can name it.

`--scan-processes` closes that from the data instead: it finds the UTF-16
`.exe` strings CE has copied into RAM, which is a strong signal of what has
actually been instantiated. The result lays out the whole startup:

```
Launch10shell.exe        Launch20device.exe      Launch30gwes.exe
Launch50explorer.exe     Launch60services.exe    Launch65Kickoff.exe
\Windows\keysoft.exe      keysoft.exe             voyagershadow.exe
\SD card\pdiboot.exe      \PC card\pdiboot.exe    \CompactFlash\pdiboot.exe
```

Those `LaunchNN` names are the registry's `HKLM\init` launch order, and
**`Launch65` is `Kickoff.exe`** — HumanWare's launcher, which is what starts
KeySoft. The `pdiboot.exe` paths are its removable-media hook: it looks for
that program on the SD card, PC card, CompactFlash and the root, and not
finding one is the ordinary case.

CE assigns slots in creation order, and with `device.exe` launched second the
suspending process is almost certainly it. Two things support that beyond the
slot number: `device.exe` hosts the Power Manager (`pm.dll` is in the image),
and the calls immediately before the power-off are every board driver's
PowerDown in turn, which is exactly what the device manager does when it
suspends. This is a strong inference from the slot ordering and the trace
rather than a direct read of the process table, so it is worth confirming.

**So the machine is being suspended by the Power Manager for want of
activity.** Nothing in the emulator generates user input yet: the keyboard
matrix is modelled but the guest's keyboard driver has to be scanning it
before a keypress can reset the idle timer. That, rather than storage or
power, is what stands between here and a device that stays awake long enough
to beep.

Storage is **not** the reason. Windows CE and KeySoft both live in the NOR
ROM filesystem and execute in place; user storage is mounted separately as
"Flash Disk" and holds only user data. A machine with no Flash Disk should
still boot, beep, and speak.

### Historical note on the launch-address print

EBOOT prints `Launch Address--:0x00000000, ROM` on the way past. That value
comes from a boot-device object field and is only informational; the actual
entry point is `header.Start`, which is why the kernel launches despite the
zero. An earlier reading of this print as the real launch target was wrong.

### EBOOT configuration block

Not needed for booting, but decoded along the way. The loader at `0x96c86e7c`
reads **184 bytes** (`mov r3, #0xb8`) from a device offset held in its object
into `this+4`, then checks the word at block offset `0x2C` against
**`0x11232000`**. A mismatch calls the factory-default routine at
`0x96c86f04`, which is what happens on our blank flash.

### Beeper

EBOOT's startup beep goes through the **AC97 codec**, not a PWM. Confirmed by
running it: EBOOT writes `GCR = COLD_RST` at `0x4050000c`, polls `GSR` at
`0x4050001c` for primary-codec-ready, then writes the primary codec register
window at `0x40500200`. No PWM base (`0x40b00000` / `0x40c00000`) is
referenced anywhere in EBOOT.

The same code region also drives **CPLD registers `0x400` and `0x404`** (first
touched from `0x96c88414` and `0x96c88448`). `0x404` is plausibly on the audio
path. `0x400` is not, whatever its proximity to codec setup suggests: it is
the braille shift register, and the OAL bit-bangs the display through it.

## The removable-storage stacks, and which one is worth driving

Measured from the ROM and from boots, August 2026. All three sockets appear
in the OEMAddressTable above; what follows is which drivers sit behind them
and which of those actually do anything.

### USB, and it is the live one

The whole host stack is in the ROM as XIP modules:

| module | at | |
|---|---|---|
| `ohci.dll` | `0x0233_0000` | host controller |
| `usbd.dll` | `0x0396_0000` | bus driver |
| `usbmsc.dll` | `0x0394_0000` | mass storage class |
| `usbdisk6.dll` | `0x0393_0000` | the block device above it |

It is this board's own rather than a leftover: the OHCI PDD carries the
source path `platform\gandalf\drivers\usb\ohcd\ohcdpdd\ohcdpdd.cpp`.
`Drivers\BuiltIn\OHCI` configures it with `MemBase` and `Irq`, and
`Mass_Storage_Class` and `Drivers\USB\ClientDrivers` are both registered.

The controller is at `0x4C00_0000`: standard OHCI operational registers at
their usual offsets, then three of Intel's own above them -- `UHCHR` at
`0x64`, `UHCHIE` at `0x68`, `UHCHIT` at `0x6C`. **`0x4c000064` is `UHCHR`,
not `HcRhPortStatus[1]`**; reading a boot log against the standard OHCI map
alone gets that wrong.

On a boot the driver resets the controller, writes an HCCA at a real SDRAM
address, sets `HcFmInterval` to `0x48700000`, enables the master interrupt,
powers the root hub, and asks `HcRhDescriptorA` how many ports it has. It
claims **IRQ 3 and IRQ 2** through `RequestSysIntr` at `wce32ddk.dll`
`0x02251070` -- the machine's two host ports.

The client side is live too: `bvd_udc_ser.dll` at `0x0231_0000` touches the
PXA270 UDC at `0x4060_0000` on every boot. That is the Mini-USB port.

### PCMCIA and CompactFlash, and why they are the fallback

`pcmcia.dll` (`0x022c_0000`), `atadisk.dll` (`0x03e1_0000`), `mspart.dll`
(`0x03db_0000`) and `fatfsd.dll` (`0x03f2_0000`) are all present.
`Drivers\BuiltIn\PCMCIA` loads card services;
`System\StorageManager\Profiles\CompactFlash` mounts a card as
`\CompactFlash` with AutoMount and AutoPart; `Drivers\PCMCIA\Detect\50` runs
`DetectATADisk` for any card no named key matches.

`pcmcia.dll`'s own window table at `0x022ce150` gives the same socket
addresses the OEMAddressTable does -- common memory `0x2000_0000`,
attribute `0x2800_0000`, I/O `0x2C00_0000`, and the same again
`0x1000_0000` higher for socket 1. Two independent sources agreeing.

But it does nothing. Its `DllMain` (`0x022cc8d4`) runs, its `Init`
(`0x022c8504`) is called, and the hardware probe at `0x022c7cf8` returns
success -- then nothing follows: no `CardGetStatus`, no CPLD access from its
address range, no read of card space, and **no `RequestSysIntr` at all**.
That points at its socket enumeration finding zero sockets rather than at a
missing detect signal. It reaches hardware through `wce32ddk.dll`
(`g_pGPIORegs`, `g_pMEMCRegs`) and `CPLD2.dll` (`GetCPLDWord`,
`SetCPLDPin`), which is why searching the module for hardware addresses
finds none.

### The empty DiskOnChip sockets

`nCS1` and `nCS3` are unfitted, and `trueffs.dll` never accepts it. It spends
about fifteen percent of the machine's running time polling them:
**30,698,285 reads in one boot**, all but seventy-nine at a single offset.
Reading all ones it waits at `0x081C` for a bit to clear; answer zero there
and it waits at `0x080E` for a bit to set. Opposite polarities, so no fixed
value ends it -- it wants a handshake, and that needs a device model rather
than a constant.

## Why this device is unusually tractable to emulate

- **No display.** The display driver is `ddi_nop.dll`, a null DDI. Output is
  braille cells and speech only.
- **Speech is software.** `kgm32.dll` plus `kgm32eng/frn/spn/grm/itl.dll`
  (Keynote Gold) synthesise on the CPU. Emulate AC97 and you get the real voice.
- **The flash FTL is the guest's problem.** The firmware ships its own
  runs inside CE, so the host only has to present raw CFI NOR.
- **Symbols survived.** Module PDB paths and MSVC-mangled C++ names are intact
  throughout, so reverse engineering works on named functions.
- **One image, both products.** `pdikeybd.dll` contains
  `Assigning the VN Canadian QWERTY ScanCode to VKeyTable`, so BrailleNote and
  VoiceNote QT share this ROM and differ by keyboard table.

## Boot chain

Verified by running it. EBOOT reaches its serial console output under
emulation; the step numbers below are what actually executes.

1. Reset at PA `0x00000000`, MMU off. Flash offset 0 holds a four-byte branch
   to flash offset `0x1000`. **Flash offset 0 corresponds to EBOOT.bin's image
   base `0x96C78000`** — the whole image is stored in flash at
   `record.addr - image.base`.
2. CPU setup: CPSR to SVC with interrupts masked, CP15 c15 CPAR, control
   register set to `0x78` (MMU explicitly *off*), TLB and cache flush,
   DACR = all-manager. Reads PMU `RCSR` at `0x40f00030` for the reset cause
   and the clock manager at `0x41300000`.
3. GPIO init: writes `GPSR0..3` and `GPCR0..3` across all four banks, which is
   why bank 3 at offset `0x100` has to decode correctly.
4. Static memory controller init at `0x48000000`: `MDCNFG`, `MDREFR`, `MSC0-2`,
   `MECR`, `MCMEM/MCATT/MCIO`, `MDMRS`, then reads `BOOT_DEF`.
5. **Copies flash `0x00000000..0x00040000` to SDRAM `0xA0078000`** — that is,
   it copies itself into RAM — then verifies the copy word by word. The
   `**FLASH to SDRAM verification failed...**` string is this check. Loading
   EBOOT anywhere but flash makes this copy overwrite the running code with
   erased-flash bytes.
6. Builds page tables at PA `0xA0000000`, sets DACR = 1, then:

   ```
   mcr p15, 0, r1, c1, c0, 0   ; control = 0x187D, MMU on
   bx  r2                      ; r2 = 0x96C79A68, a virtual address
   ```

   There is **no identity mapping** covering the physical address it is
   executing from. This works on hardware only because the `bx` is already in
   the pipeline when the MCR retires. An emulator that applies the MMU enable
   immediately will fault here. `crates/arm` models the delay
   (`Cp15::mmu_change_delay`).
7. Runs from virtual addresses, prints `Loading configuration data...done` and
   `Resetting factory default configuration ...done` on FFUART.
8. Initialises the AC97 codec (see Beeper above).
9. Probes the NOR flash with CFI commands — writes `0xFF` (read array) then
   `0x98` (CFI query) to the flash base and decodes the geometry fields.
10. On the boot path it launches the CE image at `0x80041000`, which lives at
    flash offset `0x41000`.

### Delay loops

EBOOT busy-waits by polling `OSCR` through CE's uncached static alias
`0xA4F00010` (PA `0x40A00010`). The OS timer rate therefore has to be right:
3.6864 MHz, or every firmware delay is wrong.

## Module inventory highlights

OEM and board-specific modules, i.e. the ones that will hit unimplemented
hardware: `bvdmain_serial.dll`, `bvd_udc_ser.dll`, `pdikeybd.dll`, `cpld2.dll`,
`battdrvr.dll`, `PwrButton.dll`, `RecordButton.dll`, `wavedev.dll`,
`trueffs.dll`, `wce32ddk.dll`, `kbdmouse.dll` (+ `_fr`, `_ca`), `ddi_nop.dll`,
`sdmmc_loader.dll`, `edgeser.dll`, `tl16cser.dll`, `sio950.dll`, `irsir.dll`,
`ohci.dll`, `pcmcia.dll`, `pcx500.dll` (Cisco Aironet 350).

Application layer: `KeySoft.exe` (2.4 MB), `kgm32*.dll` (TTS engines),
`brltrn.dll` (braille translation), `VRPocket.dll`, `KeyAlarm.exe`,
`KickOff.exe`, `strings_en.pll` / `strings_fr.pll`.

## Hearing what the machine says

The emulator can record its audio, and that recording can be read back as text
without anyone having to listen to it:

```bash
vbnote EBOOT.bin --flash --nk NK.bin --wav ks.wav --cycles 22000000000
ffmpeg -i ks.wav -ar 16000 -ac 1 ks16.wav
whisper -nt -m ggml-base.en.bin -otxt ks16.wav
```

Keynote Gold's speech transcribes well enough to tell one announcement from
another, which is what matters — the question is usually *which* message the
machine is giving, not its exact wording. The transcript of a boot with the
flash disk missing comes back as

> The Flash This is unavailable, please press reset with no T's down.

which is "The Flash Disk is unavailable, please press reset with no keys held
down" heard through a small model. Close enough to identify it, and far faster
than asking a person to sit through a three minute boot.

## Two firmware versions, and what moved between them

KeySoft 8.0 is the last version this machine shipped with, so it is the one
worth running. Moving to it from 7.5 was mostly free, which was the first real
test of matching patches by byte signature rather than by address:

| patch | 7.5 | 8.0 |
| --- | --- | --- |
| `DSK_Init` | `0x19018cc` | `0x195a8cc` |
| SD folder | `0x189d0e4` | `0x18f60e4` |
| SD profile | `0x1aad728` | `0x1b3596e` |

Three found their own way. `EBOOT.bin` is byte-identical between the two;
`NK.bin` grew from 43.5 MB to 51.3 MB.

### The licence check is in two modules

The fourth patch matched twice in 8.0 and refused to guess, which was correct.
Reading the ROM's table of contents and searching each module's sections
individually says exactly what the two are:

| module | virtual address |
| --- | --- |
| `KeySoft.exe` | `0x001aef10` |
| `VRPckAud.dll` | `0x02574144` |

One routine the build linked into both, not two routines that happen to share
a prologue. The instruction after the signature is a `bl`, and its offset
differs between them, which is why comparing their surroundings cannot tell
they are the same: a relative branch encodes differently at every address.
Both are the same check, so the patch changes both, and says so in the log.

### `roms/Windows` is not the build in `roms/NK.bin`

Worth knowing before trusting a file dump as a map of the ROM. The system disk
copy of `KeySoft.exe` has the same routine at a virtual address `0x760` higher
than the ROM's, and its branch offset is smaller by exactly `0x760/4` words.
Same callee, same code, different link. They are two builds.

That does not stop the dump being useful -- it is how the 8.0 validator was
found at all, and reading a PE file beats scraping a 51 MB ROM -- but an
address read out of it has to be confirmed against the ROM before anything
relies on it.

### What the ROM holds, and what it does not

Easy to get backwards, and getting it backwards produced a wrong explanation
of the duplicate above that had to be retracted. `NK.bin` carries the **system
disk**: the `Windows` folder and everything beside it, where the operating
system lives. The **flash disk** is user storage and holds no operating system
code at all -- documents, address lists, dictionaries, what somebody put
there. A module found twice is therefore two things inside the ROM. It is
never a ROM copy and a flash-disk copy, because the flash disk has no copy.

## What the licence check publishes, and what it does not

The validator at `0x001aef10` does not only answer yes or no. On its way out it
copies three words from the decoded licence payload into three consecutive
globals, and the patch that makes it return early skips that:

```
0x1aefbc  ldr r0, [r4, #0x120]   ; payload +0x20  ->  0x0031c9b4   (A)
          ldr r0, [r4, #0x124]   ; payload +0x24  ->  0x0031c9b8   (B)
          ldr r0, [r4, #0x128]   ; payload +0x28  ->  0x0031c9bc   (C)
          mov r0, #1
```

Only one function reads them: a getter at `0x001aed54` that hands all three
back through out-parameters and returns true **only if A is non-zero**. So a
patch that leaves them zero does not merely omit some information -- it makes
the getter report that there is no licence data at all.

### They are a version entitlement, not the machine's identity

The getter has exactly one caller, at `0x00175a1c`, and what it does with the
three answers says what they are. All three are compared against fields the
build reports about itself, which three accessors cut out of a single 16-bit
word:

| accessor | expression | field |
| --- | --- | --- |
| `0x0008a5c8` | `word & 0xf` | major version |
| `0x0008a5e4` | `word >> 4` | build |
| `0x0008a644` | `(word >> 14) & 3` | model class |

* **A** is compared with the major version. Greater passes at once, equal goes
  on to B, smaller fails.
* **B** is compared with the build rounded down to a multiple of ten -- the
  `smull` against `0x66666667` is a divide by ten, and the remainder is
  subtracted off. Same three-way outcome.
* **C** must be 3 or less and indexes a jump table of four:

  | C | accepts model |
  | --- | --- |
  | 0 | any |
  | 1 | 1 only |
  | 2 | 1 or 2 |
  | 3 | anything but 0 |

So the triple is *which versions and which models this licence covers*, checked
against what the build says it is. It is an entitlement, and nothing else
reads it.

### This is not what makes `h` press Enter

Worth stating plainly, because the opposite was assumed for a while and it
would have sent the keyboard work in the wrong direction. The theory was that
these globals carry the model, that skipping them made KeySoft come up as a
braille machine, and that restoring them would make it a QWERTY VoiceNote.

The scan says otherwise. Searching all of `.text` for pool slots holding the
three addresses finds six, all in the two functions above, and four loads, all
in those same two. Nothing else in KeySoft.exe reads them. They cannot be
choosing a key table, because the code that chooses a key table never looks at
them.

Which layout `pdikeybd.dll` picks is therefore still open, and has to be
answered where the four tables are selected rather than here.

### The version word is not in the image

`0x002d434c`, which all three accessors read, lies past the initialised part of
`.data` -- section 2 has a virtual size of `0x11f5a8` and only `0x64c00` bytes
behind it. It is filled in at run time. Anything that wants the real major,
build and model has to read it from a running machine, not from the file.

## Which key table the machine uses, and who chooses

`pdikeybd.dll` does not have four key tables. It has **eight**, of 96 bytes
each, from `0x02281020` to `0x022812c0`, and the driver names four of them in
its own debug output:

| code | table | what the driver calls it |
| --- | --- | --- |
| 0 | `0x022812c0` | (the power-on default) |
| 5, 6 | `0x02281020` | |
| 7 | `0x022810e0` | Assigning the French AZERTY ScanCode to VKeyTable |
| 8 | `0x02281260` | Assigning the **VN** French ScanCode to VKeyTable |
| 9 | `0x02281080` | Assigning the Canadian QWERTY ScanCode to VKeyTable |
| 10 | `0x02281200` | Assigning the **VN** Canadian QWERTY ScanCode to VKeyTable |

`VN` is VoiceNote. So the machine this project emulates wants **code 10**, and
codes 9 and 10 are two different tables for what is nominally the same QWERTY
layout -- the VoiceNote's keyboard is not the BrailleNote's with a different
label on it.

### Nothing in the driver decides this

The setter is at `0x02282448`, a one-argument switch that stores a table
pointer into a variable the two lookup functions dereference. It has two
callers and neither of them chooses:

* `PKB_IOControl`, at `0x022817b0`, on a request with a four-byte input
  buffer -- the code is whatever word the caller passed in.
* the init path at `0x022818e4`, which passes a global that is zero in the
  image, so the driver comes up on the default table and waits to be told.

So the layout is KeySoft's decision, sent down as an IOCTL. The reason `h`
presses Enter is that KeySoft is sending something other than 10, and the
place to find out why is in KeySoft, not here.

Note the two lookup functions take the code as a second argument as well:
when its low byte is 10 they bypass the variable entirely and index
`0x02281140` directly. That is a per-call override, separate from the setter.

### What the keys are called, and which code each one is

The scan table says which virtual-key codes the matrix carries. It does not say
which of them is the key labelled `READ`, and three keys on this machine --
`READ`, `FUNCTION` and `CONTROL` -- have no equivalent on a PC keyboard to
guess from. KeySoft answers it directly, in two places.

**A keystroke notation.** `0x000f0998` parses strings like `[READ]t`. It scans
for `[`, compares what follows against four names, sets a flag for each, and
then presses one code per flag:

| name | length compared | pressed as |
| --- | --- | --- |
| `[READ]` | 6 | `0xA4`, left Alt |
| `[SHIFT]` | 7 | `0x10` |
| `[CTRL]` | 6 | `0x11` |
| `[FN]` | 4 | `0xA5`, right Alt |

The matrix carries the side-specific codes -- `0xA0`/`0xA1` for shift, `0xA2`
for control -- and Windows CE treats them as the same key as the generic ones
KeySoft presses.

**A table of the rest**, at `0x00230e70`, running to a null entry after
`[RIGHT]`: pairs of a name pointer and a word of the form `0x0000<code><len>`,
where `len` is the length of that entry's own name. That redundancy is what
makes the reading safe -- a misalignment by one word would put `[APOSTROPHE]`
against a length that is not twelve.

| name | code | | name | code |
| --- | --- | --- | --- | --- |
| `[ESC]` | `0x1B` | | `[HELP]` | `0x70` (F1) |
| `[DASH]` | `0xBD` | | `[RPT]` | `0x71` (F2) |
| `[EQUALS]` | `0xBB` | | `[MENU]` | `0x72` (F3) |
| `[BKS]` | `0x08` | | `[SPC]` | `0x20` |
| `[TAB]` | `0x09` | | `[DEL]` | `0x2E` |
| `[LBRACKET]` | `0xDB` | | `[LEFT]` | `0x25` |
| `[RBRACKET]` | `0xDD` | | `[UP]` | `0x26` |
| `[BACKSLASH]` | `0xDC` | | `[RIGHT]` | `0x27` |
| `[SEMICOLON]` | `0xBA` | | `[DOWN]` | `0x28` |
| `[APOSTROPHE]` | `0xDE` | | `[COMMA]` | `0xBC` |
| `[ENTER]` | `0x0D` | | `[PERIOD]` | `0xBE` |
| `[SLASH]` | `0xBF` | | `[SINGLEQUOTE]` | `0xC0` |

Two of those names read oddly and are not mistakes: `[SINGLEQUOTE]` is the
backtick and `[APOSTROPHE]` is the apostrophe.

`READ` is the QT keyboard's chord key, and it stands exactly where `SPACE`
stands on a braille model. The same message ships twice in KeySoft, once for
each keyboard: *"To view the high score, press SPACE with I"* at `0x0024ae90`
and *"...press READ with I"* at `0x0024aee8`.

All twelve of these are on the VoiceNote table at `0x11a0` and none of them
spells a character, which is why they stayed unreachable so long: the
emulator's input path carried a `char`, so everything that types kept typing
and nothing said the rest of the keyboard was missing.

### This is where the earlier note was wrong

The previous reading -- four tables, with `0x02281140` as a braille table --
was two mistakes. There are eight, and `0x02281140` is what the lookup reaches
for when its argument is 10, which is the *VoiceNote* code, not a braille one.
Nothing in this driver mentions braille at all.

## Why `h` presses Enter: the licence payload carries the locale

This is the whole chain, and it ends somewhere unexpected -- at the patch that
gets KeySoft to start at all.

`0x001758f8` is where KeySoft decides what keyboard it has. It asks
`0x001aecdc` for three things and falls back if the answer is no:

```
0x1758f8  bl  0x1aecdc      ; (out flags1 @sp+8, out lcid @sp+0, out flags2 @sp+4)
0x175918  cmp r0, #0
0x175924  beq 0x175958      ; no answer -> hardcode en-GB, flags 3
```

and `0x001aecdc` is built directly on the licence check:

```
0x1aecf4  bl 0x1aedac       ; make a context on the stack
0x1aecfc  bl 0x1af000       ; load the blob from the 1-Wire part
0x1aed0c  bl 0x1aef10       ; <-- the validator this project patches
0x1aed10  cmp r0, #0 ; beq fail
0x1aed18  ldr r0, [sp, #0x104]   ; payload +0x04  -> flags1
0x1aed1c  ldr r1, [sp, #0x10c]   ; payload +0x0c  -> flags2
0x1aed28  ldrh r0, [sp, #0x110]  ; payload +0x10  -> the LCID
```

So the licence blob does not only say whether the machine is licensed. It
carries the machine's **locale and model flags**, and KeySoft reads them from
the context the validator filled in.

### What the flags mean

`flags2`, the word at payload `+0x0c`, reaches the layout dispatcher at
`0x00175790` and is tested one bit at a time:

* **bit 1** set means "ignore the locale", and the dispatcher answers with code
  0 or 2 depending on bit 0.
* **bit 1** clear sends it down the locale path, where the LCID chooses between
  the French pair and the Canadian pair.
* **bit 0** then chooses within the pair: set gives 7 or 9, the BrailleNote
  tables; clear gives 8 or 10, the **VN** -- VoiceNote -- tables.

A VoiceNote QT therefore wants `flags2` with both bits clear and an
English/Canadian LCID, which produces code 10, `VN Canadian QWERTY`.

### What the current patch does instead

`KEYSOFT_ACCEPT_SERIAL` replaces the validator's first instructions with

```
mov r0, #1
bx  lr
```

which is early enough to skip the memcpy at `0x1aef60` that copies the decoded
payload into the context. The copy happens **before** the device-id
comparison, so the ordering matters:

```
0x1aef60  bl 0x1ff068   ; memcpy payload -> ctx+0x100     <- first
0x1aef68  bl 0x1751ac   ; fetch the device id
0x1aef80  bl 0x1ff208   ; memcmp 8 bytes                  <- the machine binding
```

Returning at the top means the context stays as `0x1aedac` left it, so KeySoft
reads flags of zero and an LCID of zero. It then picks a keyboard for a
machine that has told it nothing, and every key lands in the wrong place.

So `h` pressing Enter is not a separate bug. It is the licence patch, and the
fix is to make the patch leave behind the payload the rest of KeySoft expects
rather than nothing at all.

### Why the blob cannot simply be replayed

Skipping only the device-id comparison and letting everything else run would
be the tidier patch, and it does not work here: it needs a real 44-byte blob
to decode, and there is not one. The 1-Wire dump this project runs with holds
a placeholder identity and the bare serial string, nothing more. A payload has
to be constructed, which means deciding every field:

| offset | what it is |
| --- | --- |
| `+0x04` | flags1, and must be non-zero or the validator fails |
| `+0x08` | compared against whatever `0x175074` returns |
| `+0x0c` | flags2 -- bit 1 locale/override, bit 0 BrailleNote/VoiceNote |
| `+0x10` | the LCID, as a halfword |
| `+0x18` | eight bytes compared with the device id |
| `+0x20` | A, the licensed major version |
| `+0x24` | B, the licensed build, rounded down to a multiple of ten |
| `+0x28` | C, the licensed model class |

Only the length has to be `0x2c`, and only `+0x04` has to be non-zero, for the
copy to happen and the fields to reach KeySoft.

## `\Flash Disk` comes from the card in the SD slot

Nothing else provides it. The ROM registry binds that folder to a DiskOnChip,
which this emulator does not model, so the SDMMC profile is rewritten to claim
it instead -- `SD_FOLDER_IS_FLASH_DISK` and `SD_PROFILE_IS_FLASH_DISK` in
`patch.rs`. KeySoft will not get past its first prompt without it.

Hearing *SD card ready* means the profile patch applied: that is the SDMMC
profile's `Name` after the patch renamed it.

### The first boot on a fresh card is expected to fail

A card that has never been used gets partitioned and formatted during the first
boot, and KeySoft asks for the flash disk before that finishes, saying it is
unavailable. Boot a second time against the same file and it comes up.

Read the card to tell a half-formatted one from a broken one. Both carry the
same table -- an extended partition at LBA 256 holding a logical FAT32 at LBA
288, `MSWIN4.1`, 127.9 MB. What differs is how much has been written: a card
mid-format stops just after the boot sectors, a finished one has its FATs and a
directory.

## What is actually in the 1-Wire EEPROM

Traced from the code that reads it, not guessed. Three layers.

### The record on the wire

`0x00175290` reads 250 bytes from the part in one go and expects:

```
byte 0      length N
byte 1..N   the blob
byte N+1    checksum, the bytes of the blob summed
```

It retries until the checksum agrees. `onewire::record()` already builds
exactly this shape, so the emulator's part is well formed -- what it holds is
not.

### The blob is RC2, through the CryptoAPI

Not Blowfish. `KeySoft.exe` does carry Blowfish's tables, at file offset
`0x1fc300`, and reading that as the answer was wrong -- they belong to some
other path. This one is the Windows CE CryptoAPI, and the calls say so:

```
0x0006855c  CryptAcquireContext("Microsoft Base Cryptographic Provider v1.0",
                                PROV_RSA_FULL, CRYPT_VERIFYCONTEXT)
0x0006861c  CryptCreateHash(CALG_MD5 = 0x8003)
0x00068670  CryptHashData("s#r14^ln5m")     the passphrase at VA 0x00254a50
0x000686b0  CryptDeriveKey(CALG_RC2 = 0x6602, hash, dwFlags = 1)
0x000687d0  CryptDecrypt(key, Final = TRUE)
```

So **RC2 in CBC with a zero IV and PKCS#5 padding**, which are the CryptoAPI's
defaults for a block cipher. `Final = TRUE` validates the padding, which is why
a wrong guess is refused outright instead of producing rubbish -- the transform
returns 0 and the stack still holds the ciphertext.

The key material is the part worth writing down. The Base Provider makes 40-bit
RC2 keys, and with neither `CRYPT_NO_SALT` nor `CRYPT_CREATE_SALT` the key is
given a salt of **zero**. So what RC2 is actually keyed with is

```
md5("s#r14^ln5m")[:5] + 11 zero bytes      effective key length 40 bits
```

a 128-bit key whose effective length is 40. Five bytes on their own do not
work, and neither does the whole hash.

### The 44 bytes

Built up from the code that reads each field, over several sessions:

| offset | size | what | who reads it |
| --- | --- | --- | --- |
| `+0x00` | 4 | not used by the validator; zero is accepted | |
| `+0x04` | 4 | flags1, must be non-zero or the whole thing fails | `0x001aefa0`, `0x001aed18` |
| `+0x08` | 4 | compared against `0x00175074`, which returns **2** | `0x001aef94` |
| `+0x0c` | 4 | flags2: bit 1 ignore the locale, bit 0 BrailleNote | `0x001aed1c` |
| `+0x10` | 2 | LCID: `0x0809` English, `0x040c` French, `0x0c0c` French Canadian | `0x001aed28` |
| `+0x18` | 8 | the device id: the **six-byte 1-Wire serial, zero-padded** | `0x001aef78` |
| `+0x20` | 4 | entitlement: major version | `0x001aed64` |
| `+0x24` | 4 | entitlement: build, compared rounded down to a ten | `0x001aed68` |
| `+0x28` | 4 | entitlement: model class, 0 accepts any | `0x001aed78` |

The validator at `0x001aef10` requires, in order: the blob decodes; the result
is `0x2c` bytes; the payload copies to `ctx+0x100`; the device id matches;
`+0x08` matches `0x00175074`; `+0x04` is non-zero.

### How a typed product key is installed

The install path is `0x001ae6cc`, and it shows a product key **is** a licence
blob, not a code checked against one. It reads bytes, calls `0x001aee58`, which
copies them into the context as the raw blob and zeroes the payload area, then
runs **the ordinary validator** `0x001aef10` over them and only writes the part
if it passes. So a key that KeySoft accepts is exactly a blob its own decryptor
validates -- which means one builder produces both the EEPROM contents and a
product key.

The decrypt interface, from the validator:

```
r0 = sp+0xc     out
r1 = ctx        the blob is at ctx+0xfc, length-prefixed
r2 = blob length
r3 = sp+0x18    out: the decrypted payload
stack: sp+8     out: the decrypted length, checked == 0x2c
       0x254a50 the key
```

So after the decrypt the plaintext is at `sp+0x18` and its length at `sp+8`.
`tools/keyblob.py` builds a candidate and `--selftest` checks the Blowfish
engine against the published vector; confirming the mode and byte order needs
one capture of KeySoft decrypting a candidate, which happens minutes into a
boot, not seconds, so it wants a full run with `--check-serial` and a
breakpoint at `0x001aef40`.

### It is built, and it works

`tools/keyblob.py` builds one, and KeySoft's own validator accepts it: the run
reaches the success tail at `0x001aefbc` and publishes the entitlement with
**every patch turned off**. Driven through its setup with `--check-serial`, the
machine asks for a language, a braille code, a braille grade and the clock, and
never asks for a product key.

The device id being the 1-Wire serial is what makes this possible without
forging anything: the emulator supplies the part, so the machine's identity and
its licence are consistent by construction rather than by pretending.

```bash
python tools/keyblob.py build --field08 2 --device-id 0102030405060000     --version 8 --build 20 --model 0 --out work/licence.blob
python tools/keyblob.py eeprom work/licence.blob --out work/SerialNumber.bin
```

### The product key is a separate question

`Type in a new product key` writes a *new* entitlement into a part that
already has a valid blob -- the messages distinguish "will not improve your
current system options" from "invalid product key", so it is checked against
what is already there rather than replacing it. Nothing above tells us how a
typed key is encoded, and nothing here is an attempt to work that out.

## Where the serial number comes from

KeySoft announces *"Serial number required, please contact your distributor"*
once the flash disk is working. The number is **not** on the flash disk and
**not** in the registry. It comes from the OAL, through three custom
`KernelIoControl` codes in the same `0x01013Fxx` family as the braille
display's:

| code | in KeySoft | in the kernel |
|---|---|---|
| `0x01013FC0` | `FUN_001eb124` | prepare |
| `0x01013FC4` | `FUN_001eb164` | read, handler at `0x8007a75c` |
| `0x01013FC8` | `FUN_001eb198` | write, handler at `0x8007a71c` |

The kernel handler opens a store by id — `0x16`, twenty-two — and reads from
it. That store is the configuration area the boot messages refer to when they
say *"Loading configuration data...done"*: it belongs to the OAL, not to any
file system, and a unit is given its contents when it is built.

KeySoft's side of it, at `0x00162d70`, expects a small record:

```text
[length][length bytes of data][checksum]      checksum = sum of the data & 0xff
```

It asks fifty times before giving up, and the announcement is what giving up
sounds like. Measured on a boot here: the kernel handler runs **200 times**
— fifty retries, four attempts — and the write handler is never called at all,
so KeySoft only ever reads. The store answers with nothing that passes the
checksum, which is what an emulated machine that was never given a
configuration area would do.

### What the store actually is

Following it down: the store is not flash, not a file and not the registry.
It is a **Dallas/Maxim 1-Wire EEPROM, bit-banged on GPIO 22**.

The chain, from the kernel's handler down to the wire:

| routine | what it does |
|---|---|
| `0x8007b734` | send `0xcc`, `0xf0`, then the id as two bytes; clock the reply back |
| `0x8007c7f0` | one byte out, least significant bit first |
| `0x8007c82c` | one byte in, least significant bit first |
| `0x8007c61c` / `0x8007c708` | one **bit** out or in, through the object's vtable |
| `0x8007b31c` `0x8007b398` `0x8007b3bc` | drive the line, release it, sample it |
| `0x80079c7c` / `0x8007a9a4` | `bank = pin >> 5`, `bit = pin & 31` — a GPIO |

Two things name it. `0xcc` then `0xf0` is 1-Wire's **Skip ROM** followed by
**Read Memory**, which is how a host with a single device on the bus reads a
DS2431-style EEPROM without addressing it first. And the "store id" the
handler opens — `0x16` — is not an id at all: it is passed straight to the
GPIO set and clear routines as a pin number. **Twenty-two.**

The timing constants the bus setup writes into the object (`0x100`, `0x18`,
`0x28`, `0xf0`, `0xdc`, `0x24`, `0x780`) are the 1-Wire slot times — reset
low, presence sample, write-zero, write-one, read sample, recovery.

So this is missing data rather than a failed check, and the missing data is
not in the flash image at all: it is in a part this emulator does not model
yet. A real unit has an EEPROM on that pin with its serial number in it, and
an emulated one has an idle line.

### Why KeySoft still exits

With a record in the EEPROM the announcement stops, KeySoft starts, and then
it quits — the sound that follows the startup music is Windows CE's **Close
Program** event, played by the shell from `\Windows\close.wav` when an
application exits. So the record is structurally fine and rejected on content.

`FUN_0019b364` is the whole of the check:

```text
decode(obj, obj[0xfc], key at 0x0023e120) -> buffer, length
if length != 0x2c            fail      ; exactly 44 bytes
copy 44 bytes to obj+0x100
if obj+0x118 (8 bytes) != KernelIoControl(0x01010034)   fail
if obj[0x108] != 2                                      fail
if obj[0x104] == 0                                      fail
```

`0x01010034` is `IOCTL_HAL_GET_DEVICEID`. So the blob is **encoded, and bound
to the device's own identity**: eight bytes inside it have to equal the device
id the OAL reports. `FUN_00162b54` returns a constant `2`, which one field
must match — a model or class code, and plausibly part of how one ROM serves
BrailleNote and VoiceNote from the same image.

That makes this a hardware-bound licence rather than missing configuration.
The distinction matters and it is where this project stops guessing: a real
unit's EEPROM holds a blob issued for that unit's device id, and the honest
way to run a real unit's software is to read that unit's own EEPROM. Minting
a blob for an arbitrary device id is a different act, and not one this
repository does.

What the emulator still owes the problem, and what is straightforwardly
legitimate: presenting a device id the owner's own hardware would report, and
supporting the 1-Wire Read ROM command so a dumped part behaves like the
original. Both are emulation. Neither requires understanding the encoding.
