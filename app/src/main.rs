//! VBNote: the emulator itself.
//!
//! Two programs in one binary. Started from the Start menu with no arguments
//! it is an appliance -- a window, a keyboard and a voice, and no terminal
//! anywhere. Started from a command line it is the tool the hardware models
//! were built with, and prints everything it knows.
//!
//! It is a windowed program that takes a console back when there should be
//! one; see `console`.

// Windowed, so that starting it from a menu does not put a black terminal
// beside the machine. A console is attached or made at run time when there is
// a reason for one, which is a thing that can be decided; the subsystem is
// not.
#![cfg_attr(windows, windows_subsystem = "windows")]

/// Where a keystroke got to, so a key that does nothing says which of the
/// steps lost it rather than leaving all of them suspect.
static KEYS_IN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static KEYS_PRESSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

mod announce;
mod console;
mod audio;
mod cardfile;
mod home;
mod hostclock;
mod hostkey;
mod keys;
mod progress;
mod usbsync;
mod debug;
mod window;

use arm::{Bus, Cpu};
use audio::{AudioOut, WavRecorder};
use ceromfs::CeImage;
use gandalf::provision::{self, StartConvention};

use gandalf::Gandalf;
use std::io::Write;

struct Options {
    image: String,
    max_cycles: u64,
    trace_pc: bool,
    trace_cpld: bool,
    report_limit: usize,
    /// Load into NOR flash and reset at physical 0, the way the board boots.
    flash: bool,
    /// An additional CE image to place in flash at its physical addresses,
    /// normally NK.bin at flash offset 0x41000.
    kernel: Option<String>,
    /// Text to type on the serial console once `trigger` has been printed.
    input: Option<String>,
    /// Output substring that releases the queued input.
    trigger: String,
    /// Write everything the guest played to this WAV file.
    wav: Option<String>,
    /// Suppress opening a host audio device.
    mute: bool,
    /// Build a provisioned flash image and save it here.
    provision_to: Option<String>,
    /// Boot from an already-provisioned flash image.
    flash_image: Option<String>,
    /// Convention for the image header's Start field.
    start_convention: StartConvention,
    /// Address of a one-byte-output routine to tap, with the byte in r1.
    ///
    /// The CE kernel's OEMWriteDebugByte discards every byte when debug
    /// output is disabled, which it is in a retail build. Tapping the routine
    /// recovers the boot log without altering what the guest does.
    debug_byte_hook: Option<u32>,
    /// Clock the guest's timers run against. See `gandalf::CPU_HZ_EFFECTIVE`.
    cpu_hz: u64,
    /// Stop the moment this address is first executed, so the call trace
    /// still holds whatever led there.
    break_at: Option<u32>,
    /// Walk SDRAM for Windows CE process structures and name them.
    scan_processes: bool,
    /// Hold the guest to wall-clock time rather than running flat out.
    realtime: bool,
    /// Value the CPLD's candidate board identification register reports.
    board_id: u16,
    /// Text to press into the guest's key matrix.
    typed: Option<String>,
    /// Guest seconds to wait before typing it.
    type_after: f64,
    /// Guest seconds to wait after the machine first makes a sound before
    /// typing, instead of after the run starts.
    ///
    /// Boot time moves with the host and with `--cpu-mhz`, so a delay measured
    /// from the start is a guess that goes stale. The first sound is the
    /// machine itself saying it has got somewhere.
    type_after_sound: Option<f64>,
    /// A file to take keystrokes from while running. Whatever appears there is
    /// typed and the file is removed, which is how something outside can drive
    /// the machine -- listen to what it said, decide, write the next key.
    keys_from: Option<String>,
    /// Core cycles to let build up before the devices are told about them.
    ///
    /// Exposed because it is a timing knob as well as a speed one: the guest
    /// has delay loops that poll the OS timer, and a batch is how coarsely
    /// they see it move. `--tick-batch 1` is the old behaviour, for telling a
    /// timing bug apart from a batching one.
    tick_batch: u32,
    /// Where to write one WAV per burst of speech.
    utterance_dir: Option<String>,
    /// Seconds of quiet that end an utterance.
    utterance_gap: f64,
    /// Relay the host's own keystrokes into the matrix as they arrive.
    keyboard: bool,
    /// GPIO the board pulls low to say "a key is down".
    ///
    /// Under CE the matrix is not polled: the scan register sees 76 reads for
    /// a whole boot, against 43327 while EBOOT's menu waited for a key. So
    /// `pdikeybd.dll` waits on an interrupt and only then scans, and pressing
    /// a key into the matrix alone does nothing. Defaults to
    /// `keyboard::KEY_DOWN_GPIO`; settable so the finding stays checkable.
    key_gpio: u32,
    /// Virtual address range to report data reads of, as (start, length).
    ///
    /// Reaching a piece of data is often easier than reaching the code that
    /// wants it: a string's address is known from the ROM, while the code
    /// that loads it may build its identifier from immediates that no search
    /// of the binary will find.
    watch: Option<(u32, u32)>,
    /// Stop at the first watched read rather than logging and carrying on.
    stop_on_watch: bool,
    /// Extra condition on `break_at`, as (register, value).
    ///
    /// A shared routine like "load string by id" is reached hundreds of times
    /// a boot; the interesting visit is the one carrying a particular
    /// argument, and that is the only one worth stopping on.
    break_if: Option<(usize, u32)>,
    /// Further condition on `break_at`: only in this FCSE slot.
    ///
    /// Every EXE in this ROM links at 0x00010000, so a slot-relative address
    /// names a different function in every process. Without this, breaking on
    /// one is breaking on all of them.
    break_slot: Option<u32>,
    /// Debugger script: breakpoints with actions. See `debug.rs`.
    debug_script: Option<String>,
    /// Where to write a line of status, and where to look for a stop request.
    ///
    /// A run started detached has no terminal to Ctrl-C and no console to read,
    /// so without these there is no way to tell a working format from a hung
    /// emulator, and no way to end one cleanly so it saves its disk.
    status: Option<String>,
    /// Turn on the ROM registry's AutoFormat, so a blank disk is formatted.
    auto_format: bool,
    /// Whether the SD/MMC card mounts as `\Flash Disk`.
    sd_is_flash_disk: bool,
    /// Read patches from this directory instead of the ones built in.
    patches_dir: Option<String>,
    /// Started with no arguments: everything comes from `~/.VBNote`.
    installed: bool,
    /// Longest a key may be held down, in milliseconds of guest time.
    ///
    /// Only a backstop. A key is normally let go of once the guest has swept
    /// the matrix twice, because how long that takes is the driver's business
    /// and no fixed span is right: too short and the driver never looks at it,
    /// too long and it decides the key is repeating. This releases a key if
    /// the guest is not scanning at all.
    ///
    /// It has to stay well under the driver's auto-repeat delay, because a
    /// key still down when that expires is a key pressed again -- at two
    /// seconds a single tap of an arrow arrived seven or eight times. It also
    /// has to be long enough for the guest to get a scan in. 800 ms sits
    /// between the two and is what the setup questions were driven with, one
    /// keystroke per press, twenty-three for twenty-three.
    key_hold_ms: u64,
    /// Contents of the 1-Wire EEPROM holding this machine's serial number.
    /// Supplied by whoever owns the machine, like EBOOT.bin and NK.bin are.
    serial_eeprom: Option<String>,
    /// Image file backing the card in the SD slot, or None for an empty slot.
    sd_card: Option<String>,
    /// Size of a card created fresh, in megabytes.
    sd_card_mb: usize,
    /// Image file backing the card in the CompactFlash slot, or None for an
    /// empty slot. This is the transfer volume, not the machine's storage:
    /// documents live on the flash disk.
    cf_card: Option<String>,
    /// Size of a CompactFlash card created fresh, in megabytes.
    cf_card_mb: usize,
    /// Image file backing a USB flash drive on the host port.
    usb_disk: Option<String>,
    /// Size of a USB flash drive created fresh, in megabytes.
    usb_disk_mb: usize,
    /// Host folder kept in step with the drive.
    usb_folder: Option<String>,
    /// Sample the program counter, to find where guest time goes.
    sample_pc: bool,
    /// Record `r1` every time this address is executed, keeping the last few.
    ///
    /// Aimed at a dispatcher: the sequence of message ids leading up to a
    /// failure says far more than the failing call on its own.
    trace_at: Option<u32>,
}

fn parse_args() -> Result<Options, String> {
    let mut opts = Options {
        image: String::new(),
        max_cycles: 0,
        trace_pc: false,
        trace_cpld: false,
        report_limit: 40,
        flash: false,
        kernel: None,
        input: None,
        trigger: "Option?".to_string(),
        wav: None,
        mute: false,
        provision_to: None,
        flash_image: None,
        start_convention: StartConvention::FlashOffset,
        debug_byte_hook: None,
        cpu_hz: gandalf::CPU_HZ_DEFAULT,
        break_at: None,
        scan_processes: false,
        realtime: true,
        board_id: 0,
        typed: None,
        type_after: 120.0,
        type_after_sound: None,
        keys_from: None,
        tick_batch: TICK_BATCH,
        utterance_dir: None,
        utterance_gap: 0.0,
        keyboard: false,
        key_gpio: gandalf::keyboard::KEY_DOWN_GPIO,
        watch: None,
        stop_on_watch: false,
        break_if: None,
        break_slot: None,
        debug_script: None,
        status: Some("vbnote.status".to_string()),
        auto_format: true,
        sd_is_flash_disk: true,
        patches_dir: None,
        installed: false,
        key_hold_ms: 800,
        sd_card: Some("FlashCard.img".to_string()),
        serial_eeprom: Some("SerialNumber.bin".to_string()),
        sd_card_mb: 128,
        cf_card: None,
        cf_card_mb: 64,
        usb_disk: None,
        usb_disk_mb: 64,
        usb_folder: None,
        sample_pc: false,
        trace_at: None,
    };
    // Whether anything was asked for at all. A bare launch is the Start menu.
    let bare = std::env::args().nth(1).is_none();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cycles" => {
                let v = args.next().ok_or("--cycles needs a value")?;
                opts.max_cycles = v.parse().map_err(|_| "--cycles needs a number")?;
            }
            "--free-run" => opts.realtime = false,
            "--board-id" => {
                let v = args.next().ok_or("--board-id needs a value")?;
                let v = v.trim_start_matches("0x");
                opts.board_id =
                    u16::from_str_radix(v, 16).map_err(|_| "expected a hex value")?;
            }
            "--flash" => opts.flash = true,
            "--nk" => {
                opts.kernel = Some(args.next().ok_or("--nk needs a path")?);
            }
            "--wav" => {
                opts.wav = Some(args.next().ok_or("--wav needs a path")?);
            }
            "--mute" => opts.mute = true,
            "--break-at" => {
                let v = args.next().ok_or("--break-at needs an address")?;
                let v = v.trim_start_matches("0x");
                opts.break_at =
                    Some(u32::from_str_radix(v, 16).map_err(|_| "expected a hex address")?);
            }
            "--scan-processes" => opts.scan_processes = true,
            "--cpu-mhz" => {
                let v = args.next().ok_or("--cpu-mhz needs a value")?;
                let mhz: u64 = v.parse().map_err(|_| "--cpu-mhz needs a number")?;
                opts.cpu_hz = mhz * 1_000_000;
            }
            "--provision" => {
                opts.provision_to = Some(args.next().ok_or("--provision needs a path")?);
            }
            "--flash-image" => {
                opts.flash_image = Some(args.next().ok_or("--flash-image needs a path")?);
            }
            "--start-va" => opts.start_convention = StartConvention::VirtualAddress,
            "--debug-byte-hook" => {
                let v = args.next().ok_or("--debug-byte-hook needs an address")?;
                let v = v.trim_start_matches("0x");
                opts.debug_byte_hook =
                    Some(u32::from_str_radix(v, 16).map_err(|_| "expected a hex address")?);
            }
            "--input" => {
                opts.input = Some(args.next().ok_or("--input needs text")?);
            }
            "--input-after" => {
                opts.trigger = args.next().ok_or("--input-after needs text")?;
            }
            "--type" => {
                opts.typed = Some(args.next().ok_or("--type needs text")?);
            }
            "--type-after" => {
                let v = args.next().ok_or("--type-after needs a number")?;
                opts.type_after = v.parse().map_err(|_| "--type-after needs a number of seconds")?;
            }
            "--type-after-sound" => {
                let v = args.next().ok_or("--type-after-sound needs a number")?;
                opts.type_after_sound =
                    Some(v.parse().map_err(|_| "--type-after-sound needs seconds")?);
            }
            "--tick-batch" => {
                let v = args.next().ok_or("--tick-batch needs a number")?;
                opts.tick_batch = v.parse().map_err(|_| "--tick-batch needs a number")?;
            }
            "--keys-from" => {
                opts.keys_from = Some(args.next().ok_or("--keys-from needs a path")?);
            }
            "--utterance-gap" => {
                let v = args.next().ok_or("--utterance-gap needs seconds")?;
                opts.utterance_gap = v.parse().map_err(|_| "--utterance-gap needs a number")?;
            }
            "--utterances" => {
                opts.utterance_dir = Some(args.next().ok_or("--utterances needs a path")?);
            }
            "--keyboard" => opts.keyboard = true,
            "--stop-on-watch" => opts.stop_on_watch = true,
            "--no-auto-format" => opts.auto_format = false,
            "--key-hold-ms" => {
                let v = args.next().ok_or("--key-hold-ms needs a number")?;
                opts.key_hold_ms = v.parse().map_err(|_| "--key-hold-ms needs a number")?;
            }
            "--patches" => {
                opts.patches_dir = Some(args.next().ok_or("--patches needs a directory")?);
            }
            "--no-sd-flash-disk" => opts.sd_is_flash_disk = false,
            "--sd-card" => opts.sd_card = Some(args.next().ok_or("--sd-card needs a path")?),
            "--no-sd-card" => opts.sd_card = None,
            "--serial-eeprom" => {
                opts.serial_eeprom = Some(args.next().ok_or("--serial-eeprom needs a path")?);
            }
            "--no-serial-eeprom" => opts.serial_eeprom = None,
            "--cf-card" => opts.cf_card = Some(args.next().ok_or("--cf-card needs a path")?),
            "--usb-disk" => opts.usb_disk = Some(args.next().ok_or("--usb-disk needs a path")?),
            "--usb-folder" => opts.usb_folder = Some(args.next().ok_or("--usb-folder needs a path")?),
            "--usb-disk-mb" => {
                let v = args.next().ok_or("--usb-disk-mb needs a size")?;
                opts.usb_disk_mb = v.parse().map_err(|_| "--usb-disk-mb needs a number")?;
            }
            "--cf-card-mb" => {
                let v = args.next().ok_or("--cf-card-mb needs a size")?;
                opts.cf_card_mb = v.parse().map_err(|_| "--cf-card-mb needs a number")?;
            }
            "--sd-card-mb" => {
                let v = args.next().ok_or("--sd-card-mb needs a size")?;
                opts.sd_card_mb = v.parse().map_err(|_| "--sd-card-mb needs a number")?;
            }
            "--status" => {
                opts.status = Some(args.next().ok_or("--status needs a path")?);
            }
            "--no-status" => opts.status = None,
            "--debug" => {
                opts.debug_script = Some(args.next().ok_or("--debug needs a script path")?);
            }
            "--trace-at" => {
                let v = args.next().ok_or("--trace-at needs an address")?;
                opts.trace_at = Some(
                    u32::from_str_radix(v.trim_start_matches("0x"), 16)
                        .map_err(|_| "expected a hex address")?,
                );
            }
            "--break-slot" => {
                let v = args.next().ok_or("--break-slot needs a slot number")?;
                opts.break_slot = Some(v.parse().map_err(|_| "--break-slot needs a number")?);
            }
            "--break-if" => {
                let v = args.next().ok_or("--break-if needs rN=VALUE")?;
                let (reg, val) = v.split_once('=').ok_or("--break-if takes rN=VALUE")?;
                let reg: usize = reg
                    .trim_start_matches(['r', 'R'])
                    .parse()
                    .map_err(|_| "--break-if register should be r0 to r15")?;
                let val = u32::from_str_radix(val.trim_start_matches("0x"), 16)
                    .map_err(|_| "--break-if value should be hex")?;
                opts.break_if = Some((reg, val));
            }
            "--watch-read" => {
                let a = args.next().ok_or("--watch-read needs an address")?;
                let a = u32::from_str_radix(a.trim_start_matches("0x"), 16)
                    .map_err(|_| "expected a hex address")?;
                let n = args.next().ok_or("--watch-read needs a length")?;
                let n: u32 = n.parse().map_err(|_| "--watch-read needs a length in bytes")?;
                opts.watch = Some((a, n));
            }
            "--key-gpio" => {
                let v = args.next().ok_or("--key-gpio needs a pin number")?;
                opts.key_gpio = v.parse().map_err(|_| "--key-gpio needs a number")?;
            }
            "--trace-pc" => opts.trace_pc = true,
            "--sample-pc" => opts.sample_pc = true,
            "--trace-cpld" => opts.trace_cpld = true,
            "-h" | "--help" => {
                println!(
                    "usage: vbnote <bootloader.bin> [options]\n\
                     \n\
                     Boots a Windows CE image on the emulated VoiceNote board.\n\
                     \n\
                     Running it:\n\
                     \x20 --flash              provision NOR flash and reset at physical 0\n\
                     \x20 --nk NK.bin          the Windows CE image to provision alongside\n\
                     \x20 --cycles N           stop after N cycles (0, the default, runs until Ctrl-C)\n\
                     \x20 --cpu-mhz N          clock the guest timers run against (default 63).\n\
                     \x20                      Higher is not faster: this one is what the\n\
                     \x20                      interpreter can retire, and going above it\n\
                     \x20                      makes the machine stutter rather than hurry\n\
                     \x20 --free-run           do not hold the guest back to real time\n\
                     \x20 --tick-batch N       cycles to batch before ticking devices\n\
                     \x20                      (default 128; 1 ticks every instruction)\n\
                     \x20 --board-id HEX       what the CPLD reports as the board revision\n\
                     \x20 --start-va           enter at the image virtual address, not physical 0\n\
                     \x20 --provision PATH     write the provisioned flash there and stop\n\
                     \x20 --flash-image PATH   boot a flash image made earlier instead\n\
                     \x20 --status PATH        write status there, and stop when PATH.stop\n\
                     \x20                      appears (default vbnote.status)\n\
                     \x20 --no-status          neither of those\n\
                     \n\
                     Storage:\n\
                     \x20 --no-auto-format     leave the ROM registry AutoFormat off\n\
                     \x20 --sd-card PATH       an SD card image, made if it is not there\n\
                     \x20 --sd-card-mb N       how big to make one (default 128)\n\
                     \x20 --no-sd-card         run with the card slot empty\n\
                     \x20 --cf-card PATH       a CompactFlash card image, made if it is\n\
                     \x20                      not there. This is the transfer volume:\n\
                     \x20                      CE mounts it as \\CompactFlash\n\
                     \x20 --cf-card-mb N       how big to make one (default 64)\n\
                     \n\
                     Getting KeySoft to start:\n\
                     \x20 These undo what the emulator does by default to a firmware image whose\n\
                     \x20 machine no longer exists. Each is documented in crates/gandalf/src/patch.rs.\n\
                     \x20 --no-sd-flash-disk   mount a card as SDMMC Disk, not Flash Disk\n\
                     \x20 --patches DIR       read .nkp patch files from here instead of\n\
                     \x20                      the ones built in\n\
                     \x20 The licence is not among these. It is a real one, built when the\n\
                     \x20 1-Wire part is made, and KeySoft validates it with its own code.\n\
                     \x20 --serial-eeprom PATH the 1-Wire part's contents, identity first\n\
                     \x20 --no-serial-eeprom   leave the 1-Wire bus with nothing on it\n\
                     \n\
                     Typing at it:\n\
                     \x20 --keyboard           take the keyboard and speak what the host key does\n\
                     \n\
                     The host key is F11, and is never sent to the machine:\n\
                     \x20 host + G             capture the keyboard, or give it back\n\
                     \x20 host + R             reset the machine\n\
                     \x20 host + Q             quit, saving the flash disk\n\
                     Captured, every key goes to the machine and none to Windows.\n\
                     Released, the reverse. It starts released, and says which it is.\n\
                     \x20 --type TEXT          press TEXT into the key matrix\n\
                     \x20 --type-after SECS    guest seconds to wait first (default 120)\n\
                     \x20 --type-after-sound S guest seconds after the first sound instead\n\
                     \x20 --keys-from PATH     type whatever appears in this file, then\n\
                     \x20                      delete it, so something else can drive it\n\
                     \x20 --key-gpio N         pin pulled low while a key is held (default 11)\n\
                     \x20 --key-hold-ms N      longest a key is held if unseen (default 800)\n\
                     \x20 --input TEXT         feed TEXT to the bootloader console instead\n\
                     \x20 --input-after TEXT   wait for TEXT in the output before sending it\n\
                     \n\
                     Audio:\n\
                     \x20 --wav OUT.wav        also capture everything played\n\
                     \x20 --utterances DIR     one WAV per burst of speech, written as\n\
                     \x20                      each one ends, for transcribing as it runs\n\
                     \x20 --mute               do not open a host audio device\n\
                     \n\
                     Looking inside:\n\
                     \x20 --debug SCRIPT       breakpoints with actions; see app/src/debug.rs\n\
                     \x20 --debug-byte-hook A  tap a one-byte-output routine, recovering the\n\
                     \x20                      kernel boot log a retail build discards\n\
                     \x20 --scan-processes     name the Windows CE processes in memory\n\
                     \x20 --break-at ADDR      stop when an address is first executed\n\
                     \x20 --break-slot N       only in that process slot\n\
                     \x20 --break-if rN=VALUE  only when a register holds that\n\
                     \x20 --watch-read A N     report accesses to N bytes at virtual address A\n\
                     \x20 --stop-on-watch      stop at the first such access\n\
                     \x20 --trace-at ADDR      trace instructions from there on\n\
                     \x20 --trace-pc           trace every instruction, which is very slow\n\
                     \x20 --sample-pc          count where the guest's time goes, and\n\
                     \x20                      list the worst offenders at the end\n\
                     \x20 --trace-cpld         print every CPLD access\n\
                     \n\
                     Example:\n\
                     \x20 vbnote EBOOT.bin --flash --nk NK.bin --sd-card card.img \\\n\
                     \x20          --serial-eeprom SerialNumber.bin --keyboard"
                );
                // Flush before leaving. Standard output is buffered when it
                // is not a terminal -- a pipe, a file, a capture -- and
                // `exit` does not flush it, so the help would simply vanish
                // into the buffer. As a console program this went unnoticed,
                // because a console is line buffered.
                let _ = std::io::Write::flush(&mut std::io::stdout());
                std::process::exit(0);
            }
            other if opts.image.is_empty() => opts.image = other.to_string(),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    // No bootloader given is not an error any more. Either a flash image was
    // named, which has one inside it, or nothing was named at all and this is
    // an installed machine being started from its menu -- see `installed`.
    // Nothing on the command line at all is not a mistake: it is how the
    // Start menu starts it. Everything then comes from the machine the wizard
    // built.
    if bare {
        opts.installed = true;
    } else if opts.image.is_empty() && opts.flash_image.is_none() {
        return Err("no image given; try --help".into());
    }
    Ok(opts)
}

/// Set up from `~/.VBNote`, for a VBNote started with no arguments.
///
/// This is the path an ordinary user takes: they installed it, they ran the
/// wizard once, and now they pick VBNote off the Start menu. Nothing is typed
/// and nothing is chosen, so everything has to be found.
///
/// Returns the message to show and stop on, if it cannot be done.
fn from_installed_machine(opts: &mut Options) -> Result<(), String> {
    let home = home::directory()
        .ok_or_else(|| "VBNote could not work out where your home folder is.".to_string())?;
    if !home::is_set_up(&home) {
        return Err(format!(
            concat!(
                "VBNote has not been set up yet.\n",
                "\n",
                "It could not find a machine in:\n",
                "{}\n",
                "\n",
                "Run \"VBNote Setup\" from the Start menu first. It builds ",
                "your machine from the firmware files you supply, and only ",
                "needs doing once.",
            ),
            home.display()
        ));
    }

    let settings = home::Settings::load(&home);
    for c in &settings.complaints {
        eprintln!("{}: {c}", home.join(home::SETTINGS).display());
    }

    let at = |name: &str| home.join(name).to_string_lossy().into_owned();
    opts.flash_image = Some(at(home::SYSTEM_DISK));
    opts.sd_card = Some(at(home::FLASH_DISK));
    opts.serial_eeprom = Some(at(home::ONEWIRE));
    opts.cpu_hz = settings.cpu_mhz * 1_000_000;
    opts.key_hold_ms = settings.key_hold_ms;
    opts.mute = settings.mute;
    // The window, the keyboard hook and the speech: this is somebody sitting
    // down to use the machine, not a scripted run.
    opts.keyboard = true;
    // The flash drive, so files can be moved without anybody having to know
    // a command line exists. Made on first use and kept in step with a folder
    // in Documents. 256 MB because the size of this decides how long the
    // machine takes to answer questions about it, not how much can be
    // carried on it.
    opts.usb_disk = Some(at(home::USB_DISK));
    opts.usb_disk_mb = settings.usb_disk_mb as usize;
    opts.status = if settings.debug { Some(at("vbnote.status")) } else { None };
    Ok(())
}

/// Say whether a firmware file is the build VBNote was tested against.
///
/// Never a refusal. Somebody with their own machine and their own firmware is
/// exactly who this software is for, and there is no way for this project to
/// obtain other builds to test with, so "not the one we know" is all that can
/// honestly be said. But when a machine misbehaves the first question is
/// always which firmware it was built from, and this puts the answer in the
/// log without anyone having to think of asking.
fn firmware_note(what: &str, path: Option<&str>, want: &str) {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let got = gandalf::sha256::hex(&bytes);
    if got == want {
        println!("  {what} is the build VBNote is tested against");
    } else {
        println!("  {what} is NOT the build VBNote is tested against");
        println!("    this file  {got}");
        println!("    tested     {want}");
        println!("    It may work perfectly. Nobody has tried it.");
    }
}

/// The patches to apply to this image.
///
/// The shipped ones are `.nkp` files compiled into the binary, so a release
/// needs nothing beside it. `--patches DIR` reads a directory instead, which
/// is how a new one gets tried without a rebuild — every `.nkp` in it, sorted
/// by name so the order is the same every run.
fn load_patches(opts: &Options) -> Result<Vec<gandalf::patch::Patch>, String> {
    if !opts.sd_is_flash_disk && opts.patches_dir.is_none() {
        return Ok(Vec::new());
    }
    let Some(dir) = &opts.patches_dir else {
        return gandalf::patch::builtin();
    };
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {dir}: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "nkp"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no .nkp files in {dir}"));
    }
    let mut out = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        out.push(gandalf::nkp::parse(&text, &path.display().to_string())?);
    }
    println!("patches: {} from {dir}", out.len());
    Ok(out)
}

/// A 1-Wire part for a machine that does not have one.
///
/// The old version of this held the serial number as digits, which is what a
/// reader would see on a real part but not what KeySoft wants: it wants a
/// licence, and without one it asks for a product key and then refuses to
/// start. This builds a real licence, so the firmware validates it with its
/// own code and nothing has to be patched out.
///
/// See `gandalf::licence` for the format and for why issuing one here is not
/// forging anybody's.
fn default_serial_eeprom() -> Vec<u8> {
    gandalf::licence::Licence::default().eeprom()
}

fn main() {
    // Before anything is printed: if a terminal started this, print there.
    console::attach_to_parent();

    let mut opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("vbnote: {e}");
            std::process::exit(2);
        }
    };

    // Started from the Start menu with nothing typed: find the machine the
    // wizard built and use it. Anything wrong here is shown in a dialog,
    // because there is no console to print to and nobody would see it.
    if opts.installed {
        if let Err(trouble) = from_installed_machine(&mut opts) {
            home::complain("VBNote", &trouble);
            std::process::exit(1);
        }
        // `debug = yes` wants somewhere to print. Started from a menu there is
        // nowhere, so make one.
        if opts.status.is_some() {
            console::open_new();
        }
    }
    let opts = opts;

    // A bootloader on the command line is one way in. The other is a flash
    // image that already contains one, which is what an installed machine
    // boots from, and then there is nothing to read here.
    let image: Option<CeImage> = if opts.image.is_empty() {
        None
    } else {
        let bytes = match std::fs::read(&opts.image) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("vbnote: cannot read {}: {e}", opts.image);
                std::process::exit(1);
            }
        };
        match CeImage::parse(&bytes) {
            Ok(i) => Some(i),
            Err(e) => {
                eprintln!("vbnote: {}: {e}", opts.image);
                std::process::exit(1);
            }
        }
    };

    if let Some(image) = &image {
        let (lo, hi) = image.extent().unwrap_or((0, 0));
        println!(
            "image  {}\n  base {:#010x}  launch {:#010x}  {} records, {} KB payload, spans {:#010x}..{:#010x}",
            opts.image,
            image.base,
            image.launch,
            image.records.len(),
            image.payload_len() / 1024,
            lo,
            hi
        );
    }

    let mut board = Gandalf::with_clock(opts.cpu_hz);
    // Start the clock where the host's is, the way a backup cell would have
    // kept it. Without this the machine powers on believing it is midnight on
    // 1 January 2010 and asks a blind user to set it, every boot.
    if let Some(now) = hostclock::now() {
        board.soc.rtc.set_count(now);
    }
    board.cpld.trace = opts.trace_cpld;
    board.cpld.board_id = opts.board_id;

    // The card in the SD slot. Windows CE's own storage stack partitions and
    // formats this, which is what puts a mountable folder in front of KeySoft.
    if let Some(path) = &opts.sd_card {
        use pxa270::sdcard::SdCard;
        let card = match std::fs::read(path) {
            Ok(raw) => {
                println!("sd card: {} MB, from {path}", raw.len() / (1024 * 1024));
                SdCard::from_image(raw)
            }
            Err(_) => {
                let card = SdCard::new(opts.sd_card_mb * 1024 * 1024);
                println!(
                    "sd card: {} MB, blank, will be saved to {path}",
                    card.data.len() / (1024 * 1024)
                );
                card
            }
        };
        board.soc.mmc.card = Some(card);
    }

    // The card in the CompactFlash slot. Nothing in the guest needs adding
    // for this: pcmcia.dll is already loaded, and the registry sends an
    // unrecognised fixed-disk card to DetectATADisk and then to the
    // CompactFlash storage profile, which mounts it as \CompactFlash.
    if let Some(path) = &opts.cf_card {
        use gandalf::pcmcia::Card;
        let card = match std::fs::read(path) {
            Ok(raw) => {
                println!("cf card: {} MB, from {path}", raw.len() / (1024 * 1024));
                Card::with_data(raw)
            }
            Err(_) => {
                let card = Card::blank((opts.cf_card_mb * 1024 * 1024 / 512) as u32);
                println!("cf card: {} MB, blank", opts.cf_card_mb);
                card
            }
        };
        board.pcmcia.insert(card);
    }

    // A flash drive on the USB host port. Plugged in before the guest runs,
    // so the driver finds it during its own enumeration rather than having to
    // be told about it later.
    if let Some(path) = &opts.usb_disk {
        use gandalf::fat32;
        use gandalf::usbdisk::UsbDisk;
        // The machine will not partition or format this for itself, so it
        // arrives ready to mount. Made sparse, because the size the user
        // asked for is a ceiling rather than an amount of disk to hand over.
        let store = if std::path::Path::new(path).exists() {
            let store = match fat32::open(path) {
                Ok(s) => s,
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            };
            println!("usb disk: {} MB, from {path}", store.len() / (1024 * 1024));
            store
        } else {
            let mb = opts
                .usb_disk_mb
                .clamp(fat32::MIN_MEGABYTES, fat32::MAX_MEGABYTES);
            if mb != opts.usb_disk_mb {
                println!(
                    "usb disk: {} MB is outside {}-{} MB, using {mb}",
                    opts.usb_disk_mb,
                    fat32::MIN_MEGABYTES,
                    fat32::MAX_MEGABYTES
                );
            }
            let mut store = match fat32::create_sparse(path, mb as u64 * 1024 * 1024) {
                Ok(s) => s,
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            };
            if let Err(e) = fat32::format(&mut store, "VBNOTE") {
                eprintln!("{e}");
                std::process::exit(1);
            }
            if let Err(e) = gandalf::vhd::write_footer(&mut store) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            println!("usb disk: {mb} MB, formatted FAT32, {path}");
            println!("  it is a fixed VHD, so Windows can mount it (needs administrator)");
            store
        };
        let folder = usb_folder(&opts);
        if let Err(e) = std::fs::create_dir_all(&folder) {
            eprintln!("usb disk: cannot make {}: {e}", folder.display());
        } else {
            println!("usb disk: files go in {}", folder.display());
        }

        // Anything new in the folder goes on the drive before the machine
        // starts. Never while it runs: CE caches directory sectors, and two
        // writers on one filesystem corrupt it without either noticing.
        let store = match gandalf::fatfile::Volume::open(store) {
            Ok(mut volume) => {
                let report = usbsync::into_drive(&usb_folder(&opts), &mut volume);
                if let Some(said) = report.spoken("to the drive") {
                    println!("usb disk: {said}");
                }
                for name in &report.failed {
                    eprintln!("usb disk: could not copy {name} to the drive");
                }
                volume.into_store()
            }
            Err(e) => {
                eprintln!("usb disk: {e}; leaving it alone");
                gandalf::fat32::open(path).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                })
            }
        };
        let disk = UsbDisk::new(store);
        board.usb = Some(Box::new(disk));
        let soc = &mut board.soc;
        soc.ohci.set_connected(0, true, &mut soc.intc);
    }

    // The serial number lives in a 1-Wire EEPROM on GPIO 22, and KeySoft will
    // not start without something answering there: it asks for the record
    // before it asks anything else, and a part that stays quiet is a part it
    // decides is broken.
    //
    // A real machine's contents belong to that machine, so a dump is used when
    // there is one. Otherwise a part is made, because requiring somebody to
    // hand-build 136 bytes before the emulator will start is exactly the
    // configuration this project is supposed not to have. Nothing about the
    // number matters here — the validator that would check it against a device
    // id is patched — so it is written down and reused rather than being
    // different on every run.
    if let Some(path) = &opts.serial_eeprom {
        match std::fs::read(path) {
            Ok(raw) => {
                println!("serial eeprom: {} bytes from {path}", raw.len());
                board.onewire = gandalf::onewire::OneWire::from_dump(&raw);
            }
            Err(_) => {
                let made = default_serial_eeprom();
                match std::fs::write(path, &made) {
                    Ok(()) => println!("serial eeprom: no {path}, so one was made there"),
                    Err(e) => println!("serial eeprom: no {path} and cannot write one ({e})"),
                }
                board.onewire = gandalf::onewire::OneWire::from_dump(&made);
            }
        }
    }

    // Filled in by whichever boot path is taken, and kept for as long as the
    // machine runs so a reset can start it the same way.
    // Filled in by whichever way the machine is started, and kept for as long
    // as it runs so that a reset can start it the same way. Every path below
    // sets it, so there is nothing sensible to start it as.
    let bootable;
    let entry = if let Some(path) = &opts.flash_image {
        // An already-provisioned device: the guest's own writes persist.
        match std::fs::read(path) {
            Ok(raw) => {
                match provision::ImageHeader::from_bytes(
                    raw.get(provision::HEADER_OFFSET..provision::HEADER_OFFSET + 12)
                        .unwrap_or(&[]),
                ) {
                    Some(h) if h.is_valid() => {
                        println!("  image header: start {:#010x} length {:#010x}", h.start, h.length)
                    }
                    _ => println!("  no valid image header; the bootloader will find no kernel"),
                }
                if let Err(e) = board.load_raw_flash(&raw) {
                    eprintln!("vbnote: {e}");
                    std::process::exit(1);
                }
                // A reset puts this back, because the guest erases the block
                // its bootloader lives in a few seconds into every boot.
                bootable = Bootable::Flash(raw);
                0
            }
            Err(e) => {
                eprintln!("vbnote: cannot read {path}: {e}");
                std::process::exit(1);
            }
        }
    } else if opts.flash {
        let image = match &image {
            Some(i) => i,
            None => {
                eprintln!("vbnote: --flash needs a bootloader; try --help");
                std::process::exit(2);
            }
        };
        // Provision the whole device the way a factory-flashed machine has
        // it — bootloader, image header, kernel — then boot from physical
        // zero exactly as the hardware does.
        let kernel = match &opts.kernel {
            Some(path) => match std::fs::read(path)
                .map_err(|e| e.to_string())
                .and_then(|b| CeImage::parse(&b).map_err(|e| e.to_string()))
            {
                Ok(k) => Some(k),
                Err(e) => {
                    eprintln!("vbnote: {path}: {e}");
                    std::process::exit(1);
                }
            },
            None => None,
        };

        println!("\nprovisioning flash...");
        {
            use gandalf::provision::tested;
            firmware_note("EBOOT.bin", Some(&opts.image), tested::EBOOT_SHA256);
            firmware_note("NK.bin", opts.kernel.as_deref(), tested::KERNEL_SHA256);
        }
        let built = match provision::build_flash_image(
            gandalf::FLASH_SIZE,
            image,
            kernel.as_ref(),
            opts.start_convention,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("vbnote: provisioning failed: {e}");
                std::process::exit(1);
            }
        };
        println!("  bootloader   {:>9} bytes at flash 0x00000000", built.eboot_bytes);
        if built.kernel_bytes > 0 {
            println!(
                "  image header        12 bytes at flash {:#010x}   id {:#010x} start {:#010x} length {:#010x}",
                provision::HEADER_OFFSET,
                built.header.id,
                built.header.start,
                built.header.length
            );
            println!(
                "  kernel       {:>9} bytes at flash {:#010x}",
                built.kernel_bytes,
                provision::KERNEL_OFFSET
            );
        } else {
            println!("  no kernel given, so the image header is left erased");
        }

        // A factory unit leaves with its flash already formatted, so the ROM
        // registry has AutoFormat off. An emulated one starts blank and
        // nothing else will format it: trueffs.dll skips the format unless
        // this reads back non-zero, and then reports the medium unusable.
        // The change is made to the image being built, never to NK.bin.
        let mut built = built;
        if opts.auto_format {
            use gandalf::registry::{set_all_dwords, AUTO_FORMAT, AUTO_PART};
            // AutoFormat lays a filesystem on a partition; AutoPart creates
            // the partition for it to go on. A disk that leaves HumanWare has
            // both done already, so the shipped registry has AutoPart off and
            // nothing in the image will ever partition a blank medium —
            // `mspart.dll` finds no partition, `fatfsd.dll` is never handed a
            // volume, and \Flash Disk never appears however well the flash
            // underneath it works.
            for (name, what) in [(AUTO_PART, "partitioned"), (AUTO_FORMAT, "formatted")] {
                let was = set_all_dwords(&mut built.image, name, 1);
                if was.is_empty() {
                    eprintln!("  {name} not found in the registry; leaving it alone");
                } else {
                    let already = was.iter().filter(|v| v.value != 0).count();
                    println!(
                        "  {name} set on {} storage profiles ({already} already on), so a blank disk gets {what}",
                        was.len()
                    );
                }
            }
        }
        {
            use gandalf::patch::{apply, Failed};
            let wanted = match load_patches(&opts) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("vbnote: {e}");
                    std::process::exit(2);
                }
            };
            for p in &wanted {
                match apply(&mut built.image, p) {
                    Ok(at) => {
                        let places: Vec<String> = at.iter().map(|o| format!("{o:#x}")).collect();
                        println!("  patched {} at {}, {}", p.name, places.join(" and "), p.because);
                    }
                    Err(Failed::NotFound) => {
                        eprintln!("  {} patch site not found; leaving it alone", p.name)
                    }
                    Err(Failed::Ambiguous(n)) => {
                        eprintln!("  {} patch site matches {n} places; refusing to guess", p.name)
                    }
                }
            }
        }
        let built = built;

        if let Some(out) = &opts.provision_to {
            match std::fs::write(out, &built.image) {
                Ok(()) => println!("  saved to {out}"),
                Err(e) => eprintln!("vbnote: could not write {out}: {e}"),
            }
        }
        if let Err(e) = board.load_raw_flash(&built.image) {
            eprintln!("vbnote: {e}");
            std::process::exit(1);
        }
        bootable = Bootable::Flash(built.image);
        0
    } else {
        let image = match &image {
            Some(i) => i,
            None => {
                eprintln!("vbnote: nothing to boot; try --help");
                std::process::exit(2);
            }
        };
        match board.load_image(image) {
            Ok(e) => {
                bootable = Bootable::Memory(image.clone());
                e
            }
            Err(e) => {
                eprintln!("vbnote: {e}");
                std::process::exit(1);
            }
        }
    };
    println!("entering at physical {entry:#010x}\n");
    let start = ColdStart { from: bootable, entry };

    let mut cpu = Cpu::new();
    if let Some((a, n)) = opts.watch {
        cpu.watch = Some((a, a.wrapping_add(n)));
    }
    cold_start(&mut cpu, entry);

    let (out, warning) = if opts.mute {
        (None, None)
    } else {
        let (a, w) = AudioOut::new();
        (Some(a), w)
    };
    if let Some(w) = warning {
        eprintln!("vbnote: audio: {w}");
    }
    if let Some(a) = &out {
        if a.is_live() {
            println!("audio: {} Hz, {} channels", a.device_rate, a.channels);
        }
    }
    let mut recorder = WavRecorder::default();

    if let Some(path) = &opts.status {
        println!("status in {path}; create {path}.stop to end the run cleanly and save the disk");
    }
    if opts.max_cycles == 0 {
        println!(
            "running until Ctrl-C. Guest timers are clocked at {} MHz{}.",
            opts.cpu_hz / 1_000_000,
            if opts.realtime { ", held to real time" } else { ", free-running" }
        );
    }
    let mut card_file = match (&opts.sd_card, board.soc.mmc.card.as_ref()) {
        (Some(path), Some(card)) => match cardfile::CardFile::open(path, card) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("sd card: cannot open {path} for writing: {e}");
                None
            }
        },
        _ => None,
    };
    let outcome = run(
        &mut cpu,
        &mut board,
        &opts,
        out.as_ref(),
        &mut recorder,
        &start,
        &mut card_file,
    );

    if let Some(path) = &opts.wav {
        if recorder.is_empty() {
            println!("audio: the guest played nothing, so {path} was not written");
        } else if let Err(e) = recorder.write(path) {
            eprintln!("vbnote: could not write {path}: {e}");
        } else {
            let rate = if recorder.rate == 0 { audio::AC97_RATE } else { recorder.rate };
            let seconds = recorder.samples.len() as f64 / 2.0 / rate as f64;
            println!("audio: wrote {path}, {seconds:.2} s at {rate} Hz");
        }
    }
    if opts.scan_processes {
        scan_processes(&mut board, cpu.cp15.pid);
    }
    report(&cpu, &mut board, outcome, opts.report_limit);

    // The drive's own writes land in its file as they happen, so this only
    // has to read them back out into the folder. Re-opened by path rather
    // than taken back off the board, which keeps the device behind its trait.
    if let Some(path) = &opts.usb_disk {
        let folder = usb_folder(&opts);
        match gandalf::fat32::open(path).and_then(gandalf::fatfile::Volume::open) {
            Ok(mut volume) => {
                let report = usbsync::out_of_drive(&mut volume, &folder);
                if let Some(said) = report.spoken("from the drive") {
                    // The folder goes on its own line rather than into the
                    // sentence: "They are in" is wrong for one file, and a
                    // sentence that has to agree with a count is a sentence
                    // that will get it wrong again later.
                    println!("usb disk: {said}");
                    println!("  in {}", folder.display());
                }
                for name in &report.failed {
                    eprintln!("usb disk: could not copy {name} out");
                }
            }
            Err(e) => eprintln!("usb disk: cannot read it back: {e}"),
        }
    }

    if let (Some(f), Some(card)) = (card_file.as_mut(), board.soc.mmc.card.as_mut()) {
        match f.flush(card) {
            Ok(_) => {
                if card.dirty {
                    println!(
                        "sd card: {} wrote {} blocks in {} flushes",
                        f.path(),
                        f.blocks_written,
                        f.flushes
                    );
                } else {
                    println!("sd card: nothing written to it, leaving {} alone", f.path());
                }
            }
            Err(e) => eprintln!("sd card: cannot write {}: {e}", f.path()),
        }
    }
}

/// Put the processor where it is at power-on.
///
/// The one description of how this machine starts, so that a reset is the same
/// thing as a first start rather than an approximation of it. Setting only the
/// program counter is not enough and fails in a way that looks unrelated: with
/// no stack pointer the bootloader's first call writes through zero, and the
/// machine ends up spinning on the undefined-instruction vector at `0x4`.
/// Start the machine over: the bootloader back in memory, the processor back
/// at power-on.
///
/// The image has to go back because it is loaded into SDRAM, and the operating
/// system that has been running since has written all over it. A reset that
/// only moved the program counter jumped into whatever CE had left there.
fn restart(cpu: &mut Cpu, board: &mut Gandalf, start: &ColdStart) {
    board.reset();
    match &start.from {
        // Booting from flash. The bootloader has to be put back, because the
        // guest erases the block it lives in a few seconds into every boot --
        // the erase asks for `0x20000`, and with 256 KB blocks on this bus
        // that is the same block as the reset vector at zero. Nothing noticed
        // while nothing ever read the vector twice; a reset reads it again and
        // finds an erased chip, which is `0xFFFFFFFF`, which is an undefined
        // instruction, which vectors to `0x4`, where the next fetch is also
        // `0xFFFFFFFF`. A machine that resets into a tight loop on the
        // undefined-instruction vector and looks like it has died.
        Bootable::Flash(image) => {
            if let Err(e) = board.load_raw_flash(image) {
                eprintln!("reset: cannot put the flash back: {e}");
            }
        }
        // Handed an image in memory instead; the operating system has been
        // writing over it ever since, so it goes back too.
        Bootable::Memory(image) => {
            if let Err(e) = board.load_image(image) {
                eprintln!("reset: cannot put the bootloader back: {e}");
            }
        }
    }
    cold_start(cpu, start.entry);
}

/// How this machine starts: what it boots from, and where that lands.
///
/// The two travel together everywhere -- a reset needs both, and either alone
/// cannot put the machine back where it began -- so they are one thing.
struct ColdStart {
    from: Bootable,
    /// The address the processor begins at.
    entry: u32,
}

/// What this machine was started from, kept so that it can be started again.
enum Bootable {
    /// A whole provisioned flash image, as a factory-flashed machine has.
    Flash(Vec<u8>),
    /// A bootloader loaded straight into memory.
    Memory(ceromfs::CeImage),
}

fn cold_start(cpu: &mut Cpu, entry: u32) {
    let watch = cpu.watch;
    *cpu = Cpu::new();
    cpu.watch = watch;
    cpu.r[15] = entry;
    // EBOOT reads r1 as a flags word from the stage before it. Zero selects
    // the ordinary cold-boot path.
    cpu.r[1] = 0;
    // Give it a stack in the top of SDRAM in case anything runs before the
    // firmware sets its own up.
    cpu.r[13] = gandalf::SDRAM_BASE + gandalf::SDRAM_SIZE as u32 - 0x1000;
}

/// Identify Windows CE processes by walking SDRAM.
///
/// Every CE executable is linked at 0x00010000 and mapped into a slot, so an
/// address cannot name a process and neither can the ROM module table. The
/// kernel does know: each process has a structure holding a pointer to its
/// name and the base of the slot it occupies. Rather than hard-code struct
/// offsets for this CE version, find them from the data — locate the UTF-16
/// names, find the pointers to them, and look for a slot base alongside.
fn scan_processes(board: &mut Gandalf, current_pid: u32) {
    use std::collections::{HashMap, HashSet};

    const SDRAM_VA: u32 = 0x96C0_0000;
    const FLASH_VA: u32 = 0x8000_0000;

    let sdram = std::mem::take(&mut board.sdram);
    let flash = std::mem::take(&mut board.flash.data);

    /// Collect UTF-16 strings ending in ".exe" and the address each starts at.
    fn collect_names(region: &[u8], base: u32, out: &mut HashMap<u32, String>) {
        let suffix: Vec<u16> = ".exe".encode_utf16().collect();
        let mut i = 0usize;
        while i + 2 < region.len() {
            let mut units: Vec<u16> = Vec::new();
            let mut j = i;
            while j + 1 < region.len() && units.len() < 64 {
                let u = u16::from_le_bytes([region[j], region[j + 1]]);
                if u == 0 {
                    break;
                }
                if !(0x20..0x7F).contains(&u) {
                    units.clear();
                    break;
                }
                units.push(u);
                j += 2;
            }
            if units.len() > suffix.len() {
                let tail: Vec<u16> = units[units.len() - suffix.len()..]
                    .iter()
                    .map(|c| if (0x41..=0x5A).contains(c) { c + 0x20 } else { *c })
                    .collect();
                if tail == suffix {
                    out.insert(base + i as u32, String::from_utf16_lossy(&units));
                    i = j + 2;
                    continue;
                }
            }
            i += 2;
        }
    }

    let mut names: HashMap<u32, String> = HashMap::new();
    let mut ram_names: HashMap<u32, String> = HashMap::new();
    collect_names(&sdram, SDRAM_VA, &mut ram_names);
    names.extend(ram_names.iter().map(|(k, v)| (*k, v.clone())));
    collect_names(&flash, FLASH_VA, &mut names);

    // Process structures live in RAM. Find words pointing at one of those
    // names, then look nearby for the base of the slot the process occupies;
    // slot bases are a multiple of 32 MB.
    let mut found: Vec<(u32, String, u32)> = Vec::new();
    let mut k = 0usize;
    while k + 4 <= sdram.len() {
        let w = u32::from_le_bytes([sdram[k], sdram[k + 1], sdram[k + 2], sdram[k + 3]]);
        if let Some(name) = names.get(&w) {
            let lo = (k.saturating_sub(0x40)) & !3;
            let hi = (k + 0x40).min(sdram.len().saturating_sub(4));
            let mut slot_base = 0;
            let mut m = lo;
            while m + 4 <= hi {
                let v = u32::from_le_bytes([sdram[m], sdram[m + 1], sdram[m + 2], sdram[m + 3]]);
                if v != 0 && v & 0x01FF_FFFF == 0 && (v >> 25) < 34 {
                    slot_base = v;
                    break;
                }
                m += 4;
            }
            found.push((SDRAM_VA + k as u32, name.clone(), slot_base));
        }
        k += 4;
    }

    board.sdram = sdram;
    board.flash.data = flash;

    // Every process name CE has copied into RAM. A name in RAM rather than
    // only in the ROM's table of contents means something instantiated it.
    let mut ram: Vec<(&u32, &String)> = ram_names.iter().collect();
    ram.sort_by_key(|(a, _)| **a);
    println!("
executable names resident in RAM ({}):", ram.len());
    for (at, name) in &ram {
        println!("  {at:#010x}  {name}");
    }

    println!("
Windows CE processes found in memory ({} name strings known):", names.len());
    if found.is_empty() {
        println!("  no process structure referenced any of them");
    }
    let mut seen = HashSet::new();
    for (at, name, slot_base) in &found {
        if !seen.insert((name.clone(), *slot_base)) {
            continue;
        }
        let mark =
            if *slot_base == current_pid && current_pid != 0 { "   <== current process" } else { "" };
        println!(
            "  {name:<30} struct field {at:#010x}  slot {} (base {slot_base:#010x}){mark}",
            slot_base >> 25
        );
    }
    println!("  current FCSE pid {current_pid:#010x} -> slot {}", current_pid >> 25);

    // Reverse search: find structures holding the current slot base, and look
    // beside them for a name pointer. This is what actually names the running
    // process, without needing CE's struct layout.
    if current_pid != 0 {
        println!("
structures containing the current slot base {current_pid:#010x}:");
        let sdram = std::mem::take(&mut board.sdram);
        let mut hits = 0;
        let mut k = 0usize;
        while k + 4 <= sdram.len() {
            let w = u32::from_le_bytes([sdram[k], sdram[k + 1], sdram[k + 2], sdram[k + 3]]);
            if w == current_pid {
                let lo = k.saturating_sub(0x60) & !3;
                let hi = (k + 0x60).min(sdram.len().saturating_sub(4));
                let mut m = lo;
                while m + 4 <= hi {
                    let v =
                        u32::from_le_bytes([sdram[m], sdram[m + 1], sdram[m + 2], sdram[m + 3]]);
                    if let Some(name) = names.get(&v) {
                        println!(
                            "  {:#010x}  slot base at +{:#x}, name {:?} via {:#010x}",
                            SDRAM_VA + k as u32,
                            m as i64 - k as i64,
                            name,
                            v
                        );
                        hits += 1;
                        break;
                    }
                    m += 4;
                }
            }
            k += 4;
        }
        board.sdram = sdram;
        if hits == 0 {
            println!("  none found with a name pointer nearby");
        }
    }
}

enum Outcome {
    CycleLimitWithLoop(Vec<u32>),
    Stuck { pc: u32, count: u64 },
    /// The guest asked the core to sleep, which on this board is a power-off.
    Suspended { mode: u8 },
    /// `--break-at` fired.
    Breakpoint { pc: u32 },
    /// The user pressed Ctrl-C.
    Interrupted,
}
/// Core cycles to let build up before the devices are told about them.
///
/// Free-running over three billion cycles: 43.0 M cycles/s ticking every
/// instruction, 61.8 at 32, 65.2 at 128, 66.3 at 512. The divisions this
/// avoids are amortised away long before the end of that, and what keeps
/// growing is interrupt latency and 1-Wire jitter, so the last 2% is not worth
/// buying. At 128 a batch is about seven OSCR ticks, against the hundred-tick
/// threshold that tells a written zero from a one.
///
/// This was off for a while because it made the machine hang saving a setting,
/// which turned out not to be batching's fault: the card model never left
/// `rcv` after a write, and batching only changed how quickly the driver got
/// round to noticing. `--tick-batch 1` restores the old behaviour if a timing
/// question ever needs telling apart from a batching one.
const TICK_BATCH: u32 = 128;

/// How long the modifiers of a chord stay down after the key has come up, in
/// milliseconds of guest time.
///
/// Long enough that KeySoft, which asks the system what is held while it
/// processes the key, is certain to still find them; short enough that the
/// keyboard driver does not decide the modifier is repeating, which it starts
/// to do somewhere beyond half a second.
const MODIFIER_TAIL_MS: u64 = 250;

/// Instructions between profile samples. Prime, so a loop whose period
/// divides evenly into it cannot hide from the sampler.
const SAMPLE_EVERY: u32 = 997;


fn run(
    cpu: &mut Cpu,
    board: &mut Gandalf,
    opts: &Options,
    audio: Option<&AudioOut>,
    recorder: &mut WavRecorder,
    // How the machine started, so a reset can start it the same way.
    start: &ColdStart,
    // The image behind the card, kept up to date as the machine runs.
    card_file: &mut Option<cardfile::CardFile>,
) -> Outcome {
    let mut spent: u64 = 0;
    let mut last_pc = u32::MAX;
    let mut same_pc = 0u64;
    let mut stdout = std::io::stdout();

    // Queued console input is released once the firmware prints its prompt,
    // which is more reliable than guessing a cycle count.
    // Ring of recently executed addresses, so a stall can be explained by
    // showing the loop rather than a single sampled PC.
    const HISTORY: usize = 512;
    let mut history = [0u32; HISTORY];
    let mut history_at = 0usize;
    // Sampling profile, to tell "progressing through delays" apart from
    // "looping forever in one place".
    let mut profile: std::collections::HashMap<(u8, u32), u64> =
        std::collections::HashMap::new();
    // When each FCSE slot was last caught running, and how often. A profile
    // adds up over the whole run and so cannot say whether something is still
    // going; this can. A slot last seen a hundred guest seconds before the end
    // stopped a hundred guest seconds before the end.
    let mut slot_last = [0u64; 32];
    let mut slot_samples = [0u64; 32];
    // Counted down rather than tested with a modulo: 997 is prime, so
    // `% 997` is a real division, and it was running once per instruction.
    let mut sample_in = SAMPLE_EVERY;
    // Every 64 KB region ever executed. A sampled profile answers "where is
    // the time going"; this answers "did this module run at all", which is
    // the question when a module is expected to do one short thing once.
    let mut executed = vec![false; 1 << 16];
    let mut last_region = usize::MAX;

    let mut pending_input: Vec<u8> =
        opts.input.as_deref().map(|s| s.as_bytes().to_vec()).unwrap_or_default();
    let mut output_tail = String::new();
    let mut debug_log: Vec<u8> = Vec::new();
    let mut announced_audio = false;
    // Distinct FCSE process IDs seen. Each is a slot, so the count is a
    // lower bound on how many processes Windows CE actually started.
    let mut slots: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let mut last_pid = u32::MAX;
    let mut bt_log: Vec<u8> = Vec::new();
    // FCSE slot and call chain at the first byte out of BTUART.
    let mut bt_first: Option<(u32, Vec<(u32, u32)>)> = None;
    let mut st_log: Vec<u8> = Vec::new();
    let mut dispatched: std::collections::VecDeque<u32> = Default::default();
    let mut breakpoints = match &opts.debug_script {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => match debug::parse(&text) {
                Ok(bps) => {
                    println!("debugger: {} breakpoints from {path}", bps.len());
                    bps
                }
                Err(e) => {
                    eprintln!("vbnote: {path}: {e}");
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("vbnote: cannot read {path}: {e}");
                std::process::exit(1);
            }
        },
        None => Vec::new(),
    };
    let status_path = opts.status.clone();
    let stop_path = status_path.as_ref().map(|p| format!("{p}.stop"));
    // The host key's commands, as files, for a run with nobody at the
    // keyboard: `touch PATH.reset`. A detached run has no keyboard to press
    // the host key on, and this is the only way to exercise a reset from a
    // script.
    let poke_paths: Vec<(String, hostkey::Command)> = status_path
        .as_ref()
        .map(|p| {
            vec![
                (format!("{p}.reset"), hostkey::Command::Reset),
            ]
        })
        .unwrap_or_default();
    let mut next_status = 0u64;

    // Keystrokes waiting to go into the matrix, and the host thread that
    // supplies them. Reading stdin from the run loop would stall the guest,
    // so a thread does it and hands bytes over a channel.
    let (typed, complaints) = keys::parse(opts.typed.as_deref().unwrap_or(""));
    for c in &complaints {
        eprintln!("--type: {c}");
    }
    let mut typing: std::collections::VecDeque<keys::Press> = typed.into();
    // Keystrokes from the window, which are never held back.
    let mut live: std::collections::VecDeque<keys::Press> = std::collections::VecDeque::new();

    // When typing is measured from the first sound this is not known yet.
    let mut typing_at = match opts.type_after_sound {
        Some(_) => u64::MAX,
        None => (opts.type_after * opts.cpu_hz as f64) as u64,
    };
    let mut last_pcm_at = 0u64;
    let gap_secs = if opts.utterance_gap > 0.0 { opts.utterance_gap } else { 1.5 };
    let gap_cycles = (gap_secs * opts.cpu_hz as f64) as u64;
    let mut utterances = match &opts.utterance_dir {
        Some(dir) => match audio::Utterances::new(dir, opts.utterance_gap) {
            Ok(u) => Some(u),
            Err(e) => {
                eprintln!("utterances: cannot use {dir}: {e}");
                None
            }
        },
        None => None,
    };
    let (key_tx, key_rx) = std::sync::mpsc::channel::<keys::Press>();
    let (command_tx, command_rx) = std::sync::mpsc::channel::<hostkey::Command>();
    // Everything the host key does is invisible, so it is spoken. A run with
    // no window has nobody at the keyboard and stays quiet.
    let (voice, trouble) = if opts.keyboard {
        announce::Voice::start()
    } else {
        (announce::Voice::silent(), None)
    };
    if let Some(t) = trouble {
        eprintln!("{t}");
    }
    if opts.keyboard {
        // Not straight away. The window has only just been asked for, and a
        // screen reader that is still working out what appeared will talk
        // over this or drop it entirely -- and it is the one message that
        // says how to work everything else.
        let voice = voice.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            voice.say(
                "VBNote ready. Keyboard released. \
                 F 11 with G to capture it, R to reset, Q to quit.",
            );
        });
    }
    if opts.keyboard {
        // A window, because an operating system will only deliver keystrokes
        // to one. It has nothing to display — the machine answers in speech —
        // so it exists purely to be typed at. Reading stdin instead meant
        // nothing arrived until Enter, which a menu answering single keys
        // never sees.
        // The window is for focus and for closing; the keys come from the
        // hook, which sees keys the window library cannot. Handing it the
        // sender as well would deliver everything twice.
        std::thread::spawn(window::run_without_keys);
        if let Err(e) = hostkey::install(key_tx, command_tx.clone()) {
            eprintln!("keyboard: {e}");
        }
    }
    // The key-down line idles high. It is active low -- the board pulls it
    // down while a key is held -- and every GPIO input starts at zero here,
    // so without this it reads "key down" from the moment the machine powers
    // on. Pressing a key then drives it low a second time, which is no edge
    // at all, so no interrupt fires and the driver never scans. That is
    // exactly what it did: 113 scan reads before the press and 113 after.
    board.soc.gpio.set_input(opts.key_gpio, true, &mut board.soc.intc);

    // The key currently held, and the cycle at which to let it go. A real
    // press has to survive at least one full 12-column scan; a tenth of a
    // second of guest time is comfortably longer than that and still faster
    // than anyone types.
    // The keystroke currently down, if any: a key plus whatever is being held
    // with it.
    let mut held: Option<keys::Press> = None;
    let mut release_at = 0u64;
    // For each key of the keystroke currently down, how many times its column
    // had been scanned when it went down, so the release can wait for the
    // guest to look rather than for a clock.
    let mut baselines: Vec<(u8, u64)> = Vec::new();
    // Where the keystroke currently down has got to. A chord is played the
    // way a hand plays one: the modifier goes down first, the key joins it,
    // the key comes up, and only then does the modifier follow. Every stage
    // waits for the guest to have looked, because a stage the guest never saw
    // did not happen.
    #[derive(PartialEq, Eq, Clone, Copy)]
    enum Stage {
        /// Modifiers down, waiting for the guest to see them.
        Mods,
        /// Everything down.
        All,
        /// The key is up and the modifiers are still down.
        LetGo,
    }
    let mut stage = Stage::Mods;
    let key_hold = opts.cpu_hz * opts.key_hold_ms / 1000;

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = std::sync::Arc::clone(&stop);
        let _ = ctrlc::set_handler(move || stop.store(true, std::sync::atomic::Ordering::Relaxed));
    }

    // Holding the guest to wall-clock time keeps audio from arriving in
    // bursts. It only ever slows the emulator down; when the host cannot keep
    // up, which is the usual case today, it does nothing.
    let started = std::time::Instant::now();
    let mut next_check = 0u64;

    // A boot is a minute and a half of silence, and a reset starts another
    // one. Silence is exactly what a machine that has failed to start sounds
    // like, so until the machine makes its own first sound this makes one on
    // its behalf. It stops of its own accord: the moment the guest pushes a
    // sample, there is something better to listen to.
    // Checked once a guest second: often enough that a stage is announced
    // when it happens, rarely enough to cost nothing.
    let progress_every = opts.cpu_hz;
    let mut next_progress = progress_every;

    // Where the guest's time actually goes. Sampling the program counter
    // every few thousand cycles and counting what comes up is enough to tell
    // work from waiting -- and telling those apart is the whole question when
    // something is slow but correct.
    let mut samples: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let sample_every: u64 = 4096;
    let mut next_sample = sample_every;
    let mut progress = progress::Progress::new();

    // How often the card is written back. Two seconds of the guest's time,
    // which at the clock this runs at is about two seconds of the user's. It
    // is the amount of typing an abrupt ending is allowed to cost.
    let flush_every = opts.cpu_hz * 2;
    let mut next_flush = flush_every;

    // The machine has switched itself off and is waiting for the switch.
    let mut asleep = false;
    let mut next_poke = 0u64;
    // Whether a suspend should be waited through rather than treated as the
    // end of the run. It is, whenever there is any way left to switch the
    // machine back on: somebody at the keyboard, or a script poking files.
    let stays_up = opts.keyboard || status_path.is_some();

    let mut early: Option<Outcome> = None;
    let mut batch: u32 = 0;
    while opts.max_cycles == 0 || spent < opts.max_cycles {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            early = Some(Outcome::Interrupted);
            break;
        }

        if opts.sample_pc && spent >= next_sample {
            next_sample = spent + sample_every;
            *samples.entry(cpu.r[15]).or_insert(0) += 1;
        }

        if spent >= next_progress {
            next_progress = spent + progress_every;
            let sight = progress::Sight {
                // The MMU being on is the kernel running from virtual
                // addresses, which the bootloader never does.
                kernel_running: cpu.cp15.control & 1 != 0,
                // CE puts each process in its own FCSE slot, so a non-zero
                // process id is something other than the kernel scheduled.
                programs_running: cpu.cp15.pid >> 25 != 0,
                // The USB driver powers the root hub as the built-in drivers
                // are loaded.
                drivers_loaded: board.soc.ohci.ports.iter().any(|p| p.status & 0x100 != 0),
                // `reads`, not `scans_seen`: the latter counts scans that
                // found a key held, and nobody is typing during a boot, so it
                // stays at zero and the stage never arrives. This one counts
                // the driver looking at all.
                keyboard_ready: board.cpld.keyboard.reads > 0,
                first_run_setup: board.cpld.modem.commands > 0,
                guest_spoke: audio.as_ref().is_some_and(|a| a.guest_has_spoken()),
                seconds: spent / opts.cpu_hz,
            };
            if let Some(said) = progress.update(&sight) {
                // Printed as well as spoken: a bug report that says which
                // stage it stopped at is worth a great deal more than one
                // that says it went quiet.
                println!("starting up: {said}");
                voice.say(&said);
            }
        }

        if spent >= next_flush {
            next_flush = spent + flush_every;
            if let (Some(f), Some(card)) = (card_file.as_mut(), board.soc.mmc.card.as_mut()) {
                if let Err(e) = f.flush(card) {
                    eprintln!("sd card: cannot write {}: {e}", f.path());
                }
            }
        }

        // The same commands as the host key, but as files, so a run with
        // nobody at the keyboard can still be driven. Checked here rather
        // than with the status file, because that is written further down and
        // a sleeping machine never gets that far -- which would make the one
        // command that wakes it the one command it could not receive.
        // Or whenever the machine is asleep: nothing is being stepped then,
        // so guest time stands still and a check gated on it would never come
        // round again -- leaving the one command that wakes the machine as
        // the one command it cannot receive.
        if asleep || spent >= next_poke {
            next_poke = spent + opts.cpu_hz / 4;
            for (path, command) in &poke_paths {
                if std::path::Path::new(path).exists() {
                    let _ = std::fs::remove_file(path);
                    let _ = command_tx.send(*command);
                }
            }
        }

        // What the user asked of the emulator rather than of the machine.
        // Everything here is invisible, so everything here is said out loud.
        while let Ok(command) = command_rx.try_recv() {
            match command {
                hostkey::Command::Capture(on) => {
                    // Already applied, in the hook. This only reports it.
                    if !on {
                        // Whatever was down stays down for ever otherwise:
                        // the release of a key is a key event too, and while
                        // released this stops seeing them.
                        board.cpld.keyboard.release_all();
                        held = None;
                    }
                    voice.say(if on { "keyboard captured" } else { "keyboard released" });
                }
                hostkey::Command::Reset => {
                    voice.say("reset");
                    if let Some(a) = audio {
                        a.push_tone(440.0, 0.5, 0.25);
                    }
                    // Starting again means starting again: a machine left
                    // asleep or halted has to be woken, or the reset lands on
                    // a core that is not running and looks like nothing
                    // happened at all.
                    asleep = false;
                    last_pc = u32::MAX;
                    same_pc = 0;
                    next_progress = spent + progress_every;
                    restart(cpu, board, start);
                    board.cpld.keyboard.release_all();
                    held = None;
                }
                hostkey::Command::Quit => {
                    voice.say("quitting, saving the flash disk");
                    early = Some(Outcome::Interrupted);
                }
            }
        }
        if early.is_some() {
            break;
        }
        if opts.realtime && spent >= next_check {
            next_check = spent + opts.cpu_hz / 1000;
            let guest = std::time::Duration::from_secs_f64(spent as f64 / opts.cpu_hz as f64);
            let real = started.elapsed();
            if guest > real {
                std::thread::sleep(guest - real);
            }
        }
        board.pc = cpu.r[15];
        board.soc.pc = cpu.r[15];

        if opts.trace_pc {
            eprintln!("{:#010x}  cpsr={:08x}", cpu.r[15], cpu.cpsr);
        }

        // A tight branch-to-self is how ARM firmware signals "I have given
        // up". Detect it rather than burning the whole cycle budget.
        // A halted core keeps the same PC by design, so exclude it from the
        // stuck detector.
        if Some(cpu.r[15]) == opts.trace_at
            && opts.break_slot.is_none_or(|slot| cpu.cp15.pid >> 25 == slot)
        {
            dispatched.push_back(cpu.r[1]);
            if dispatched.len() > 64 {
                dispatched.pop_front();
            }
        }
        if Some(cpu.r[15]) == opts.break_at
            && opts.break_if.is_none_or(|(reg, val)| cpu.r[reg] == val)
            && opts.break_slot.is_none_or(|slot| cpu.cp15.pid >> 25 == slot)
        {
            early = Some(Outcome::Breakpoint { pc: cpu.r[15] });
            break;
        }
        // Stopping on the watched read leaves the call ring holding whoever
        // asked for the data, which is the point of watching it.
        if opts.stop_on_watch && !cpu.watch_hits.is_empty() {
            early = Some(Outcome::Breakpoint { pc: cpu.r[15] });
            break;
        }
        if cpu.suspended && stays_up {
            // Switched off rather than finished. With somebody at the
            // keyboard the emulator stays up so the switch can be flipped
            // back; ending the run here is what made the power switch look
            // like a crash. Nothing is stepped while it sleeps.
            if !asleep {
                asleep = true;
                // The switch stays where it was put. It is not a button, and
                // putting it back would be flipping it on again.
                voice.say("asleep");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        if cpu.suspended {
            early = Some(Outcome::Suspended { mode: cpu.pwrmode });
            break;
        }
        if cpu.r[15] == last_pc && !cpu.halted {
            same_pc += 1;
            if same_pc > 2_000_000 {
                early = Some(Outcome::Stuck { pc: cpu.r[15], count: same_pc });
                break;
            }
        } else {
            same_pc = 0;
            last_pc = cpu.r[15];
        }

        // Tap the guest's debug-byte routine on entry, where the byte to be
        // emitted is still in r1.
        if Some(cpu.r[15]) == opts.debug_byte_hook {
            let b = (cpu.r[1] & 0xFF) as u8;
            debug_log.push(b);
            stdout.write_all(&[b]).ok();
            if b == 0x0A {
                stdout.flush().ok();
            }
        }

        if cpu.cp15.pid != last_pid {
            last_pid = cpu.cp15.pid;
            *slots.entry(last_pid >> 25).or_insert(0) += 1;
        }

        // Drive the key matrix: one key at a time, held until the guest's
        // scanner has actually looked at it, then released before the next
        // goes down.
        //
        // A fixed hold cannot be right, because what it has to outlast is the
        // driver's scan interval, and that is the guest's business and varies
        // with what the machine is doing. Measured: at 100 ms of guest time
        // the driver saw four keys in a whole run and missed the rest, and at
        // 600 ms it saw one press as seventy-six, because holding a key that
        // long starts auto-repeat. Neither number is right because no number
        // is.
        //
        // So watch, for every key that is down, how many times the guest has
        // scanned the column it is in. Two apiece means the driver has both
        // seen it and had a scan to debounce against; holding longer only
        // invites a repeat.
        //
        // Counting whole sweeps instead was tried and is worse -- the driver
        // does not sweep on a timer, so waiting for two sweeps usually meant
        // waiting for the backstop, and a key held that long is a key the
        // driver decides is repeating. That is what turned one press of an
        // arrow into seven or eight.
        // One scan while the modifiers are going down, two once the whole
        // chord is. The difference matters: the modifier only has to have been
        // *seen*, so that it is posted before the key it modifies, and every
        // extra scan it is held for is a scan closer to the driver deciding it
        // is repeating. It does not take many -- holding READ for two scans
        // was enough for the driver to start repeating it and then ignore the
        // letter that joined it, which looked exactly like the letter never
        // arriving.
        // Two scans once the whole chord is down, so the guest has seen it
        // and had one to debounce against; one for the stages either side,
        // because they only have to be noticed and every extra scan a
        // modifier is held for is a scan nearer the driver deciding it is
        // repeating.
        let want = if stage == Stage::All { 2 } else { 1 };
        let seen_enough = !baselines.is_empty()
            && baselines.iter().all(|(vk, at_press)| {
                board.cpld.keyboard.scans_of(*vk).is_some_and(|n| n >= at_press + want)
            });
        match held {
            // The modifiers are down and the guest has seen them. Now the key
            // they modify can join them.
            //
            // Not at the same moment, which is what this used to do and why
            // `READ` and `FUNCTION` did nothing at all. The driver sweeps the
            // matrix in column order and posts keys as it finds them, so one
            // sweep that finds a modifier *and* a letter posts whichever comes
            // first -- and `READ` is in hardware column 10, `FUNCTION` in 11,
            // while a letter is almost always lower. KeySoft therefore saw the
            // letter arrive first, with nothing held, and acted on it: `READ`
            // with `D` opened the database manager, exactly as a bare `D`
            // does. `CONTROL` and `SHIFT` were unaffected and looked fine,
            // because both sit in hardware column 0 and are found before
            // anything else.
            //
            // A hand presses the chord key first and holds it. So does this.
            Some(press) if stage == Stage::Mods && (seen_enough || spent >= release_at) => {
                if board.cpld.keyboard.set_key(press.vk, true) {
                    // Tell the driver a key has arrived. It scans when the
                    // key-down line falls, and that line is already low --
                    // the modifier is holding it there -- so without an edge
                    // of its own the new key waits for a sweep that never
                    // comes.
                    //
                    // This is what stopped `READ` working while `SHIFT`
                    // looked fine, and the difference was only ever where
                    // they sit. `SHIFT` is in hardware column 0, so the sweep
                    // that has just seen it has not yet reached the letter's
                    // column and picks the letter up in the same pass.
                    // `READ` is in column 10: by the time it has been seen
                    // the sweep is all but over, the letter's column went by
                    // long ago, and nothing starts another one.
                    board.soc.gpio.set_input(opts.key_gpio, true, &mut board.soc.intc);
                    board.soc.gpio.set_input(opts.key_gpio, false, &mut board.soc.intc);
                    baselines.clear();
                    let seen = board.cpld.keyboard.scans_of(press.vk).unwrap_or(0);
                    baselines.push((press.vk, seen));
                    stage = Stage::All;
                    release_at = spent + key_hold;
                } else {
                    eprintln!("no key on this keyboard for {:#04x}", press.vk);
                    for vk in press.mods.keys() {
                        board.cpld.keyboard.set_key(vk, false);
                    }
                    board.soc.gpio.set_input(opts.key_gpio, true, &mut board.soc.intc);
                    held = None;
                }
            }
            // The key comes up while the modifiers are still held. Letting
            // them go together left the guest seeing the modifier released
            // first, which is not a chord being finished but a chord being
            // abandoned.
            Some(press) if stage == Stage::All && (seen_enough || spent >= release_at) => {
                board.cpld.keyboard.set_key(press.vk, false);
                let mods = press.mods.keys();
                if mods.is_empty() {
                    board.soc.gpio.set_input(opts.key_gpio, true, &mut board.soc.intc);
                    held = None;
                    stage = Stage::Mods;
                } else {
                    // The line is still low, because the modifiers are still
                    // down, so the driver needs telling that something
                    // changed here too.
                    board.soc.gpio.set_input(opts.key_gpio, true, &mut board.soc.intc);
                    board.soc.gpio.set_input(opts.key_gpio, false, &mut board.soc.intc);
                    baselines.clear();
                    for vk in &mods {
                        let seen = board.cpld.keyboard.scans_of(*vk).unwrap_or(0);
                        baselines.push((*vk, seen));
                    }
                    // And now hold them, for a good long moment.
                    //
                    // KeySoft does not track the modifiers from the key
                    // messages at all. While it is processing the letter it
                    // asks the system, calling `GetKeyState` for `0x11`,
                    // `0x10`, `0xa4` and `0xa5` in turn and building a flag
                    // word from whatever is down *at that instant*
                    // (`0x000f2b24`). So the modifier has to still be held
                    // when KeySoft gets round to the letter, which is some
                    // way behind the driver -- and letting go after a scan or
                    // two meant it never was. The flag word read zero and the
                    // chord arrived as a bare letter.
                    //
                    // A hand holds a chord key for a good fraction of a
                    // second. There is nothing to wait *for* here, no scan
                    // that means "KeySoft has looked", so this is a time.
                    baselines.clear();
                    stage = Stage::LetGo;
                    release_at = spent + opts.cpu_hz * MODIFIER_TAIL_MS / 1000;
                }
            }
            Some(press) if seen_enough || spent >= release_at => {
                for vk in press.mods.keys() {
                    board.cpld.keyboard.set_key(vk, false);
                }
                board.soc.gpio.set_input(opts.key_gpio, true, &mut board.soc.intc);
                held = None;
                stage = Stage::Mods;
            }
            Some(_) => {}
            None => {
                // Keys somebody is actually pressing go in their own queue.
                // `--type-after` exists so a scripted string can wait for the
                // machine to finish starting, and applying that delay to a
                // live keystroke means the window swallows everything typed
                // in the first two minutes -- which is all of it, for anyone
                // who opens the window and tries a key.
                while let Ok(press) = key_rx.try_recv() {
                    KEYS_IN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    live.push_back(press);
                }
                let next = live.pop_front().or_else(|| {
                    if spent >= typing_at { typing.pop_front() } else { None }
                });
                if let Some(press) = next {
                    // The modifiers go down first and alone, and the key that
                    // they modify waits until the guest has seen them. See the
                    // release arm above for why the two cannot go down
                    // together.
                    let mods = press.mods.keys();
                    baselines.clear();
                    for vk in &mods {
                        board.cpld.keyboard.set_key(*vk, true);
                        let seen = board.cpld.keyboard.scans_of(*vk).unwrap_or(0);
                        baselines.push((*vk, seen));
                    }
                    stage = if mods.is_empty() { Stage::All } else { Stage::Mods };
                    let started = if stage == Stage::All {
                        let down = board.cpld.keyboard.set_key(press.vk, true);
                        if down {
                            let seen = board.cpld.keyboard.scans_of(press.vk).unwrap_or(0);
                            baselines.push((press.vk, seen));
                        }
                        down
                    } else {
                        true
                    };
                    if started {
                        KEYS_PRESSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        board.soc.gpio.set_input(opts.key_gpio, false, &mut board.soc.intc);
                        held = Some(press);
                        release_at = spent + key_hold;
                    } else {
                        eprintln!("no key on this keyboard for {:#04x}", press.vk);
                    }
                }
            }
        }

        if !breakpoints.is_empty() {
            let mut hit = None;
            for (i, bp) in breakpoints.iter().enumerate() {
                if debug::matches(bp, cpu) {
                    hit = Some(i);
                    break;
                }
            }
            if let Some(i) = hit {
                if debug::fire(&mut breakpoints[i], cpu, board) {
                    early = Some(Outcome::Breakpoint { pc: cpu.r[15] });
                    break;
                }
            }
        }

        // Status, and the only way to stop a detached run cleanly. Both are
        // files because a run started detached has no console: nothing can
        // signal it, and nothing can read what it prints.
        if spent >= next_status {
            next_status = spent + opts.cpu_hz / 4;
            if let Some(path) = &status_path {
                let line = format!(
                    "cycles {spent}\nguest_seconds {:.1}\nreal_seconds {:.1}\n\
                     speed {:.0}%\naudio_underruns {}\naudio_seconds {:.1}\n",
                    spent as f64 / opts.cpu_hz as f64,
                    started.elapsed().as_secs_f64(),
                    // Under 100 the guest cannot make audio as fast as the
                    // device drains it, and no amount of buffering fixes that:
                    // the answer is a lower --cpu-mhz or a faster core.
                    100.0 * (spent as f64 / opts.cpu_hz as f64)
                        / started.elapsed().as_secs_f64().max(1e-9),
                    audio.map_or(0, |a| a.underruns()),
                    recorder.samples.len() as f64 / 2.0 / 44100.0,
                );
                let _ = std::fs::write(path, line);
            }
            if let Some(path) = &opts.keys_from {
                if let Ok(text) = std::fs::read_to_string(path) {
                    let _ = std::fs::remove_file(path);
                    let (pressed, complaints) = keys::parse(&text);
                    for c in &complaints {
                        eprintln!("--keys-from: {c}");
                    }
                    for press in pressed {
                        KEYS_IN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        live.push_back(press);
                    }
                }
            }
            if let Some(path) = &stop_path {
                if std::path::Path::new(path).exists() {
                    let _ = std::fs::remove_file(path);
                    early = Some(Outcome::Interrupted);
                    break;
                }
            }
        }

        // Only write when the region changes. Straight-line code stays in
        // one 64 KB region for a long time, so this is a register compare
        // almost always, against a scattered write into 64 KB of memory.
        let region = (cpu.r[15] >> 16) as usize;
        if region != last_region {
            last_region = region;
            executed[region] = true;
        }
        history[history_at % HISTORY] = cpu.r[15];
        history_at += 1;
        sample_in -= 1;
        if sample_in == 0 {
            sample_in = SAMPLE_EVERY;
            let slot = (cpu.cp15.pid >> 25) as usize & 31;
            *profile.entry((slot as u8, cpu.r[15])).or_insert(0) += 1;
            slot_last[slot] = spent;
            slot_samples[slot] += 1;
        }

        let c = cpu.step(board);
        spent += c as u64;

        // Hand the devices a batch of cycles rather than one instruction's
        // worth. `tick` takes a count and does its arithmetic once, so this is
        // the same sum reaching the same accumulators — but the OS timer, the
        // RTC and the AC97 pacing each divide a 64-bit fraction by the core
        // clock, and paying for three divisions per instruction was over half
        // of everything this emulator did.
        //
        // What it costs is latency: a timer interrupt can be up to a batch
        // late, and the 1-Wire bus sees time in steps rather than smoothly.
        // A batch is small against both. The bus measures pulses in tens to
        // thousands of OSCR ticks and one OSCR tick is about seventeen core
        // cycles here, so a batch is a couple of ticks of jitter on a
        // hundred-tick threshold.
        batch += c;
        if batch < opts.tick_batch {
            continue;
        }
        board.tick(batch);
        batch = 0;

        // A burst can end by going quiet or by the guest simply stopping, and
        // the second leaves no samples to notice it in. Watch the clock too.
        if let Some(u) = utterances.as_mut() {
            if u.pending() && spent.saturating_sub(last_pcm_at) > gap_cycles {
                if let Some(path) = u.flush() {
                    println!("utterance: {}", path.display());
                }
                last_pcm_at = spent;
            }
        }

        // Everything below here is draining what the devices have produced,
        // and a device only produces anything when the guest writes to it.
        // Asking four of them once per instruction meant four cache lines
        // touched to be told "nothing", which is the same waste the tick
        // batching removed. Once a batch is soon enough: a batch is a few
        // microseconds of guest time and these are audio and serial queues.

        // Move any PCM the guest produced towards the speakers.
        if !board.soc.ac97.pcm_out.is_empty() {
            if !announced_audio {
                announced_audio = true;
                if let Some(secs) = opts.type_after_sound {
                    typing_at = spent + (secs * opts.cpu_hz as f64) as u64;
                    println!(
                        "typing: {secs:.0} guest seconds after this",
                    );
                }
                println!(
                    "
audio: first samples after {:.1} G cycles ({:.0} s of guest time)",
                    spent as f64 / 1e9,
                    spent as f64 / opts.cpu_hz as f64
                );
            }
            let rate = board.soc.ac97.pcm_rate();
            let pcm = board.soc.ac97.drain_pcm();
            if let Some(u) = utterances.as_mut() {
                last_pcm_at = spent;
                if let Some(path) = u.push(&pcm, rate) {
                    println!("utterance: {}", path.display());
                }
            }
            if let Some(a) = audio {
                a.push_ac97(&pcm, rate);
            }
            if opts.wav.is_some() {
                recorder.push_ac97(&pcm, rate);
            }
        }

        // Stream the serial console as it appears; it is the whole point.
        // The CE kernel's OAL may use a different UART than the bootloader,
        // so surface all three rather than assuming FFUART.
        if !board.soc.btuart.tx.is_empty() {
            // The driver PC only ever names the serial driver, which every
            // client shares. The call chain and the FCSE slot at the moment
            // the first byte goes out name the client.
            if bt_first.is_none() {
                bt_first = Some((cpu.cp15.pid >> 25, cpu.call_trace()));
            }
            bt_log.extend_from_slice(&board.soc.btuart.drain_tx());
        }
        if !board.soc.stuart.tx.is_empty() {
            st_log.extend_from_slice(&board.soc.stuart.drain_tx());
        }
        if !board.soc.ffuart.tx.is_empty() {
            let out = board.soc.ffuart.drain_tx();
            stdout.write_all(&out).ok();
            stdout.flush().ok();

            if !pending_input.is_empty() {
                output_tail.push_str(&String::from_utf8_lossy(&out));
                if output_tail.contains(&opts.trigger) {
                    for b in pending_input.drain(..) {
                        board.soc.ffuart.feed(b, &mut board.soc.intc);
                    }
                }
                // Only the recent tail can match, so keep it bounded.
                if output_tail.len() > 4096 {
                    let cut = output_tail.len() - 1024;
                    output_tail = output_tail.split_off(cut);
                }
            }
        }
    }
    // A run usually ends mid-phrase, so write what was being said.
    if let Some(u) = utterances.as_mut() {
        if let Some(path) = u.flush() {
            println!("utterance: {}", path.display());
        }
    }

    let mut recent: Vec<u32> = Vec::new();
    let start = history_at.saturating_sub(HISTORY);
    for i in start..history_at {
        let pc = history[i % HISTORY];
        if !recent.contains(&pc) {
            recent.push(pc);
        }
    }
    for (name, log) in [("BTUART", &bt_log), ("STUART", &st_log)] {
        if log.is_empty() {
            continue;
        }
        println!("
[{name}: {} bytes]", log.len());
        let printable: String = log
            .iter()
            .map(|b| match b {
                0x20..=0x7E | 0x0A | 0x0D => *b as char,
                _ => '.',
            })
            .collect();
        println!("{printable}");
        let hex: Vec<String> = log.iter().take(64).map(|b| format!("{b:02x}")).collect();
        println!("hex: {}", hex.join(" "));
        if name == "BTUART" {
            if let Some((slot, trace)) = &bt_first {
                println!("first byte sent from FCSE slot {slot}, called via:");
                for (from, to) in trace.iter().rev().take(16) {
                    println!("  {from:#010x} -> {to:#010x}");
                }
            }
        }
    }
    if !debug_log.is_empty() {
        println!("

[tapped {} bytes of kernel debug output]", debug_log.len());
    }
    println!("
FCSE slots scheduled ({} distinct):", slots.len());
    for (slot, switches) in &slots {
        println!("  slot {slot:<3} entered {switches} times");
    }
    recent.sort_unstable();

    // Which slots were still running when the run ended. Anything whose last
    // sample is well before the end has stopped, which is the difference
    // between a process waiting and a process gone.
    println!("
slots by when they last ran (end of run: {spent} cycles):");
    let mut alive: Vec<(usize, u64, u64)> = (0..32)
        .filter(|s| slot_samples[*s] > 0)
        .map(|s| (s, slot_last[s], slot_samples[s]))
        .collect();
    alive.sort_by_key(|(_, last, _)| std::cmp::Reverse(*last));
    for (slot, last, samples) in &alive {
        let behind = spent.saturating_sub(*last);
        let seconds = behind as f64 / opts.cpu_hz as f64;
        let note = if behind < opts.cpu_hz { "still running".to_string() }
                   else { format!("last seen {seconds:.1} guest seconds before the end") };
        println!("  slot {slot:<3} {samples:>9} samples   {note}");
    }

    let mut hot: Vec<((u8, u32), u64)> = profile.into_iter().collect();
    hot.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("
sampled profile: {} distinct addresses", hot.len());
    for ((slot, pc), n) in hot.iter().take(12) {
        println!("  slot {slot:<3} {pc:#010x}  {n} samples");
    }
    // What each slot's own code is doing. The kernel's addresses are left out
    // on purpose: every slot's busiest address is the idle loop it sleeps in,
    // which says only that it is waiting, not what for.
    println!("
busiest addresses outside the kernel, per slot:");
    for (slot, _, _) in &alive {
        let mut own: Vec<((u8, u32), u64)> = hot
            .iter()
            .filter(|((s, pc), _)| *s as usize == *slot && *pc < 0x8000_0000)
            .map(|(k, v)| (*k, *v))
            .collect();
        if own.is_empty() {
            println!("  slot {slot:<3} nothing outside the kernel");
            continue;
        }
        own.truncate(5);
        let places: Vec<String> =
            own.iter().map(|((_, pc), n)| format!("{pc:#010x} ({n})")).collect();
        println!("  slot {slot:<3} {}", places.join("  "));
    }

    // Coalesce the executed 64 KB regions into runs. Matching these against
    // the ROM's module table says exactly which modules ran.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (i, hit) in executed.iter().enumerate() {
        let at = (i as u32) << 16;
        match runs.last_mut() {
            Some(last) if *hit && last.1 == at => last.1 = at + 0x10000,
            _ if *hit => runs.push((at, at + 0x10000)),
            _ => {}
        }
    }
    println!("
code executed, by 64 KB region ({} runs):", runs.len());
    for (lo, hi) in &runs {
        println!("  {lo:#010x}..{hi:#010x}");
    }
    // On an early stop the path that led there is the whole point, and
    // unlike the deduplicated set it has to stay in order to be readable.
    if !breakpoints.is_empty() {
        println!("
breakpoints, and how often each was reached:");
        for bp in &breakpoints {
            let slot = bp.slot.map_or(String::new(), |s| format!(" slot={s}"));
            println!("  {:#010x}{slot}  {} hits", bp.pc, bp.hits);
        }
    }
    if !dispatched.is_empty() {
        println!("
last {} values of r1 at the traced address:", dispatched.len());
        for chunk in dispatched.iter().collect::<Vec<_>>().chunks(8) {
            let line: Vec<String> = chunk.iter().map(|v| format!("{v:#06x}")).collect();
            println!("  {}", line.join("  "));
        }
    }
    if early.is_some() {
        let show = 96.min(history_at);
        println!("\nthe last {show} instructions, in order:");
        for chunk in ((history_at - show)..history_at)
            .map(|i| history[i % HISTORY])
            .collect::<Vec<_>>()
            .chunks(6)
        {
            let line: Vec<String> = chunk.iter().map(|p| format!("{p:#010x}")).collect();
            println!("  {}", line.join("  "));
        }
    }
    if !samples.is_empty() {
        let mut top: Vec<(u32, u64)> = samples.into_iter().collect();
        top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let total: u64 = top.iter().map(|(_, n)| *n).sum();
        println!("\nwhere the guest spent its time ({total} samples):");
        for (pc, n) in top.iter().take(20) {
            println!("  {pc:#010x}  {:>5.1}%  {n}", *n as f64 * 100.0 / total as f64);
        }
    }

    early.unwrap_or(Outcome::CycleLimitWithLoop(recent))
}

/// Where the drive's files live on the host.
///
/// Documents rather than beside the machine's own files: this is the folder
/// a user is meant to open, and it belongs where they keep their things.
fn usb_folder(opts: &Options) -> std::path::PathBuf {
    if let Some(p) = &opts.usb_folder {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    std::path::Path::new(&home)
        .join("Documents")
        .join(home::USB_FOLDER)
}

fn report(cpu: &Cpu, board: &mut Gandalf, outcome: Outcome, limit: usize) {
    println!("\n\n---- bring-up report ----");
    match outcome {
        Outcome::CycleLimitWithLoop(ref pcs) => {
            println!("stopped: cycle budget exhausted");
            println!(
                "the last {} instructions covered {} distinct addresses:",
                512.min(pcs.len() * 4),
                pcs.len()
            );
            for chunk in pcs.chunks(6) {
                let line: Vec<String> = chunk.iter().map(|p| format!("{p:#010x}")).collect();
                println!("  {}", line.join("  "));
            }
        }
        Outcome::Stuck { pc, count } => {
            println!("stopped: spinning at {pc:#010x} for {count} instructions")
        }
        Outcome::Breakpoint { pc } => println!("stopped: reached {pc:#010x}"),
        Outcome::Interrupted => println!("stopped: interrupted"),
        Outcome::Suspended { mode } => {
            println!(
                "stopped: the guest entered CP14 power mode {mode} (sleep).
                 The board powered itself down; nothing we model is a wake source."
            )
        }
    }
    println!(
        "pc {:#010x}  cpsr {:#010x}  mode {:#04x}  {} cycles retired",
        cpu.r[15],
        cpu.cpsr,
        cpu.mode(),
        cpu.cycles
    );
    print!("registers");
    for (i, v) in cpu.r.iter().enumerate() {
        if i % 4 == 0 {
            print!("\n  ");
        }
        print!("r{i:<2} {v:#010x}   ");
    }
    println!("\n  cp15 control {:#010x}  ttbr {:#010x}  pid {:#010x}",
        cpu.cp15.control, cpu.cp15.ttbr, cpu.cp15.pid);

    // Collapse the call trace: consecutive identical calls become a count,
    // so a polling loop shows as one line instead of 256.
    let trace = cpu.call_trace();
    if !trace.is_empty() {
        println!("
call trace (most recent last):");
        let mut runs: Vec<((u32, u32), u32)> = Vec::new();
        for c in trace {
            match runs.last_mut() {
                Some((prev, n)) if *prev == c => *n += 1,
                _ => runs.push((c, 1)),
            }
        }
        for ((from, to), n) in runs.iter().rev().take(30).rev() {
            let times = if *n > 1 { format!("   x{n}") } else { String::new() };
            println!("  {from:#010x} -> {to:#010x}{times}");
        }

        // Slot 0 is the running process's own code. Filtering to it strips
        // out the kernel and the ROM DLLs in slot 1, leaving what the
        // process itself did.
        let own: Vec<((u32, u32), u32)> = runs
            .iter()
            .filter(|((f, t), _)| *f < 0x0200_0000 || *t < 0x0200_0000)
            .cloned()
            .collect();
        if !own.is_empty() {
            println!("
calls involving the running process's own code (slot 0):");
            for ((from, to), n) in own.iter().rev().take(40).rev() {
                let times = if *n > 1 { format!("   x{n}") } else { String::new() };
                println!("  {from:#010x} -> {to:#010x}{times}");
            }
        }
    }

    let names = ["reset", "undefined", "swi", "prefetch abort", "data abort", "irq", "fiq"];
    let taken: Vec<String> = cpu
        .exception_counts
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(i, n)| format!("{} x{n}", names[i]))
        .collect();
    println!(
        "
exceptions taken: {}",
        if taken.is_empty() { "none".to_string() } else { taken.join(", ") }
    );
    println!(
        "INTC: mask {:#010x}/{:#010x}  pending {:#010x}/{:#010x}  irq line {}",
        board.soc.intc.mask[0],
        board.soc.intc.mask[1],
        board.soc.intc.pending[0],
        board.soc.intc.pending[1],
        board.soc.intc.irq_line()
    );
    println!(
        "OST: OSCR {:#010x}  OSMR0 {:#010x}  OIER {:#010x}  OSSR {:#010x}",
        board.soc.ost.oscr, board.soc.ost.osmr[0], board.soc.ost.oier, board.soc.ost.ossr
    );
    println!(
        "
AC97: link {}, beep register {:#06x}, {} PCM words queued",
        if board.soc.ac97.link_up { "up" } else { "down" },
        board.soc.ac97.beep_register(),
        board.soc.ac97.pcm_out.len()
    );
    if !board.soc.ac97.gcr_log.is_empty() {
        println!("AC97 control writes (value, GSR at the time):");
        for (v, gsr) in board.soc.ac97.gcr_log.iter().take(24) {
            let mut what = Vec::new();
            if v & (1 << 1) != 0 { what.push("COLD_RST"); }
            if v & (1 << 2) != 0 { what.push("WARM_RST"); }
            if v & (1 << 3) != 0 { what.push("ACLINK_OFF"); }
            if v & 1 != 0 { what.push("GIE"); }
            println!("  {v:#010x}  gsr {gsr:#010x}   {}", what.join(" "));
        }
    }
    if !board.soc.ac97.codec_log.is_empty() {
        println!("AC97 codec register writes:");
        for (reg, v) in board.soc.ac97.codec_log.iter().take(24) {
            let name = match reg {
                0x00 => "reset", 0x02 => "master vol", 0x0A => "PC beep",
                0x18 => "PCM out vol", 0x26 => "powerdown", 0x2A => "ext audio ctrl",
                0x2C => "DAC rate", _ => "",
            };
            println!("  reg {reg:#04x} <- {v:#06x}   {name}");
        }
    }
    if !board.soc.i2c.addresses.is_empty() {
        println!("
I2C slave addresses the guest addressed:");
        for (addr, n) in &board.soc.i2c.addresses {
            println!("  {addr:#04x} (7-bit)  {n} transactions");
        }
        println!("  first {} transfers:", board.soc.i2c.log.len().min(24));
        for t in board.soc.i2c.log.iter().take(24) {
            let kind = if t.is_address {
                format!("ADDRESS {:#04x} {}", t.address, if t.read { "read" } else { "write" })
            } else {
                format!("data {:#04x}", t.data)
            };
            println!("    {kind}");
        }
    }

    println!("
serial ports (which the braille display would hang off):");
    for (name, u, wired) in [
        ("FFUART", &board.soc.ffuart, "DB-9 RS-232 and the bootloader console"),
        ("BTUART", &board.soc.btuart, "Bluetooth module"),
        ("STUART", &board.soc.stuart, "IrDA"),
    ] {
        println!(
            "  {name:<7} {:>8} reads {:>8} writes {:>7} bytes sent   first tx pc {:#010x}   {wired}",
            u.reads, u.writes, u.bytes_sent, u.first_tx_pc
        );
    }

    if !cpu.data_abort_log.is_empty() {
        println!("
data aborts (distinct):");
        for (pc, va, fsr, mode) in cpu.data_abort_log.iter().take(32) {
            let cause = match fsr & 0xF {
                0x1 | 0x3 => "alignment",
                0x5 => "section translation",
                0x7 => "page translation",
                0x9 => "section domain",
                0xB => "page domain",
                0xD => "section permission",
                0xF => "page permission",
                _ => "other",
            };
            println!(
                "  pc {pc:#010x}  va {va:#010x}  fsr {fsr:#06x} ({cause})  mode {mode:#04x}"
            );
            let text = arm::mmu::explain(&cpu.cp15, board, *va);
            for line in text.lines() {
                println!("    {line}");
            }
        }
    }

    // The USB host controller. The root hub's port status is the interesting
    // part: a driver that never reads it has been told it has no ports, which
    // is exactly how this started.
    {
        let hc = &board.soc.ohci;
        print!(
            "\nUSB host: HCCA {:#010x}, {} interrupts raised, {} acknowledged",
            hc.hcca, hc.raises, hc.status_clears
        );
        for (n, port) in hc.ports.iter().enumerate() {
            print!("   port {n} {:#010x}", port.status);
        }
        println!();
        if let Some(dev) = board.usb.as_ref() {
            // A non-zero address is the proof that matters: the host only
            // assigns one after a successful control transfer, so it cannot
            // be there unless descriptors were read and answered.
            println!(
                "  device attached, address {}, {} storage commands ({}){}",
                dev.address(),
                dev.commands(),
                if dev.address() == 0 {
                    "not enumerated"
                } else {
                    "enumerated"
                },
                dev.summary()
            );
            let guest = board.elapsed as f64 / 63_000_000.0;
            println!(
                "  engine: {} calls, {} found work, {} transfers ({:.0} transfers/guest-second, {:.1} per busy call)",
                board.usb_calls,
                board.usb_busy_calls,
                board.usb_tds,
                board.usb_tds as f64 / guest.max(0.001),
                board.usb_tds as f64 / board.usb_busy_calls.max(1) as f64
            );
        }
        if !hc.unexpected.is_empty() {
            println!("  registers outside the map:");
            for (off, val) in hc.unexpected.iter().take(limit) {
                println!("    {off:#06x}  last {val:#010x}");
            }
        }
    }

    // What the PCMCIA socket was asked for. An empty list means card services
    // never looked, which is a different fault from a card it looked at and
    // rejected -- and telling those two apart is most of the work.
    if !board.pcmcia.log.is_empty() {
        println!(
            "\nCompactFlash socket: {} distinct addresses touched, card {}",
            board.pcmcia.log.len(),
            if board.pcmcia.occupied() { "in" } else { "absent" }
        );
        for ((space, off), s) in board.pcmcia.log.iter().take(limit) {
            println!(
                "  {space:?} {off:#07x}  {:>5} reads {:>5} writes  last {:#06x}  first pc {:#010x}",
                s.reads, s.writes, s.last_value, s.first_pc
            );
        }
    }

    let mut any = false;
    if !cpu.undefined_log.is_empty() {
        any = true;
        println!("\nundefined instructions executed:");
        for (addr, insn) in cpu.undefined_log.iter().take(limit) {
            println!("  {addr:#010x}  {insn:#010x}");
        }
    }
    if !board.soc.unimplemented.is_empty() {
        any = true;
        println!("\nunimplemented PXA registers touched:");
        for (addr, s) in board.soc.unimplemented.iter().take(limit) {
            println!(
                "  {addr:#010x}  {:>5} reads {:>5} writes  last {:#010x}  first pc {:#010x}",
                s.reads, s.writes, s.last_value, s.first_pc
            );
        }
    }
    if !board.unmapped.is_empty() {
        any = true;
        println!("\nunmapped physical addresses touched:");
        for (addr, s) in board.unmapped.iter().take(limit) {
            println!(
                "  {addr:#010x}  {:>5} reads {:>5} writes  last {:#010x}  first pc {:#010x}",
                s.reads, s.writes, s.last_value, s.first_pc
            );
        }
    }
    if board.cpld.board_id_was_read() {
        println!(
            "
the guest sampled the CPLD board identification register (reported {:#06x})",
            board.cpld.board_id
        );
    }
    {
        let rtc = &board.soc.rtc;
        println!("\nreal-time clock: counter now {}", rtc.rcnr);
        for (i, (reads, writes)) in rtc.accesses.iter().enumerate() {
            if *reads == 0 && *writes == 0 {
                continue;
            }
            println!("  {:#06x}  {reads:>6} reads {writes:>4} writes", i * 4);
        }
    }
    let cpld = board.cpld.report();
    if !cpld.is_empty() {
        any = true;
        println!("\nCPLD registers touched:");
        for (off, l) in cpld.iter().take(limit) {
            println!(
                "  {off:#06x}  {:>5} reads {:>5} writes  last written {:#06x}  first pc {:#010x}",
                l.reads, l.writes, l.last_written, l.first_pc
            );
        }
        println!(
            "  keyboard: {} arrived from the window, {} pressed into the matrix",
            KEYS_IN.load(std::sync::atomic::Ordering::Relaxed),
            KEYS_PRESSED.load(std::sync::atomic::Ordering::Relaxed)
        );
        println!(
            "  keyboard: {} scans came back with a key down",
            board.cpld.keyboard.scans_seen
        );
        println!(
            "  keyboard: {} reads of the scan register in all",
            board.cpld.keyboard.reads
        );
    }

    // Which GPIOs the guest armed for edge detection. A driver that waits on
    // a pin rather than polling is invisible in access counts, but it has to
    // arm the edge first, and that shows up here.
    let gpio = &board.soc.gpio;
    let mut armed: Vec<String> = Vec::new();
    for bank in 0..gpio.rising.len() {
        for bit in 0..32 {
            let r = gpio.rising[bank] >> bit & 1 != 0;
            let f = gpio.falling[bank] >> bit & 1 != 0;
            if r || f {
                let edge = match (r, f) {
                    (true, true) => "both",
                    (true, false) => "rising",
                    _ => "falling",
                };
                armed.push(format!(
                    "{} ({edge}, armed from {:#010x})",
                    bank * 32 + bit,
                    gpio.armed_by[bank][bit]
                ));
            }
        }
    }
    if !cpu.watch_hits.is_empty() {
        any = true;
        println!("\naccesses to the watched range:");
        for (va, pc, pid, write) in &cpu.watch_hits {
            let kind = if *write { "written by" } else { "read from" };
            println!("  {va:#010x} {kind} pc {pc:#010x}  slot {}", pid >> 25);
        }
    }
    if !armed.is_empty() {
        any = true;
        println!("\nGPIOs armed for edge detection:");
        for a in &armed {
            println!("  {a}");
        }
    }
    if !any {
        println!("\nno unimplemented hardware was touched");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gandalf::keyboard::Keyboard;

    #[test]
    fn every_key_a_script_can_ask_for_exists_on_the_keyboard() {
        let kb = Keyboard::default();
        // Square brackets are the notation's own, so a script asks for those
        // two keys by name; see `keys`.
        let (pressed, complaints) = keys::parse(
            "abcdefghijklmnopqrstuvwxyz0123456789 \r\t\x08\x1b`-=[LBRACKET][RBRACKET];',./",
        );
        assert!(complaints.is_empty(), "{complaints:?}");
        for press in &pressed {
            assert!(press.mods.is_empty(), "{:#04x} should not need a modifier", press.vk);
            assert!(kb.position_of(press.vk).is_some(), "{:#04x} is not there", press.vk);
        }
        let (pressed, complaints) = keys::parse("ABCXYZ!@#$%^&*()_+:\"<>?");
        assert!(complaints.is_empty(), "{complaints:?}");
        for press in &pressed {
            assert!(press.mods.shift, "{:#04x} should need shift", press.vk);
            assert!(kb.position_of(press.vk).is_some(), "{:#04x} is not there", press.vk);
        }
    }

    #[test]
    fn newline_and_carriage_return_both_press_enter() {
        assert_eq!(keys::press_for('\n'), Some(keys::Press::plain(0x0D)));
        assert_eq!(keys::press_for('\r'), Some(keys::Press::plain(0x0D)));
    }

    /// The chord keys reach the matrix through the whole path a script takes,
    /// which is the thing that was missing: READ, FUNCTION and CONTROL are on
    /// the keyboard and no character spells any of them.
    #[test]
    fn the_chord_keys_reach_the_matrix() {
        let mut kb = Keyboard::default();
        let (pressed, complaints) = keys::parse("[READ]t");
        assert!(complaints.is_empty(), "{complaints:?}");
        let press = pressed[0];
        for vk in press.mods.keys() {
            assert!(kb.set_key(vk, true), "{vk:#04x} is not on the matrix");
        }
        assert!(kb.set_key(press.vk, true));
        assert!(kb.any_down());
    }
}
