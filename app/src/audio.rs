//! Host audio output.
//!
//! Audio is not a nicety on this machine — it is the entire user interface.
//! Everything the device says, and every diagnostic beep the bootloader makes,
//! arrives as PCM through the AC97 controller. This takes those samples and
//! puts them on the host's speakers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Fallback when the guest has not told us otherwise.
pub const AC97_RATE: u32 = 48_000;

/// Interleaved stereo samples waiting to be played.
type Shared = Arc<Mutex<VecDeque<i16>>>;

/// Fractional position between input frames, for the resampler.
struct Resample {
    pos: f64,
    last: (i16, i16),
}

pub struct AudioOut {
    queue: Shared,
    resample: Mutex<Resample>,
    stream: Option<cpal::Stream>,
    pub device_rate: u32,
    pub channels: u16,
    /// Frames the device wanted and did not get **while the machine was
    /// talking**.
    ///
    /// The qualifier is the whole value of the number. An empty queue is the
    /// normal state of a machine that is saying nothing, so counting every
    /// empty callback reports thousands of underruns on a completely silent
    /// and completely healthy run -- which is what this used to do, and it
    /// sent a real investigation chasing a fault that was not there. Only a
    /// gap in something the guest was in the middle of producing is a fault.
    pub underruns: Arc<Mutex<u64>>,
    /// Samples handed over by the guest, ever. The callback watches this to
    /// tell "nothing to play" from "something to play and it did not arrive".
    fed: Arc<std::sync::atomic::AtomicU64>,
    /// Set the first time the machine itself makes a sound.
    ///
    /// A boot takes a minute and a half and is completely silent until the
    /// chime, which for a user who cannot see the screen is indistinguishable
    /// from a machine that has not started. Something has to say "still
    /// working" until the machine can say it for itself.
    guest_spoke: std::sync::atomic::AtomicBool,
}

impl AudioOut {
    /// Open the default output device. Returns an inert sink if no device is
    /// available, so a machine with no sound card still runs.
    pub fn new() -> (Self, Option<String>) {
        let queue: Shared = Arc::new(Mutex::new(VecDeque::new()));
        let underruns = Arc::new(Mutex::new(0u64));
        let fed = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let host = cpal::default_host();
        let device = match host.default_output_device() {
            Some(d) => d,
            None => {
                return (
                    AudioOut {
                        queue,
                        resample: Mutex::new(Resample { pos: 0.0, last: (0, 0) }),
                        stream: None,
                        device_rate: AC97_RATE,
                        channels: 2,
                        underruns,
                        fed,
                        guest_spoke: std::sync::atomic::AtomicBool::new(false),
                    },
                    Some("no output device; audio will be discarded".into()),
                )
            }
        };

        let config = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                return (
                    AudioOut {
                        queue,
                        resample: Mutex::new(Resample { pos: 0.0, last: (0, 0) }),
                        stream: None,
                        device_rate: AC97_RATE,
                        channels: 2,
                        underruns,
                        fed,
                        guest_spoke: std::sync::atomic::AtomicBool::new(false),
                    },
                    Some(format!("no usable output config ({e}); audio will be discarded")),
                )
            }
        };

        let device_rate = config.sample_rate().0;
        let channels = config.channels();
        let q = Arc::clone(&queue);
        let u = Arc::clone(&underruns);
        let f = Arc::clone(&fed);
        // What the guest had produced when this callback last ran. Unchanged
        // and nothing queued means the machine is simply quiet.
        let seen_fed = Arc::new(Mutex::new(0u64));
        // The cushion the callback fills before it starts, in samples rather
        // than frames.
        let prime_samples = Self::cushion_samples(device_rate);
        // `Some(n)` means "not playing until n samples are in hand".
        let prime_flag: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(Some(prime_samples)));
        // What to gather before starting again after a dropout: enough not to
        // fall straight back in, little enough not to be a silence of its own.
        let resume_samples = prime_samples / 4;
        let last: Arc<Mutex<(i16, i16)>> = Arc::new(Mutex::new((0, 0)));

        // The guest always produces stereo i16 at 48 kHz. The callback maps
        // that onto whatever the device wants.
        let stream = device.build_output_stream(
            &config.config(),
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut q = q.lock().unwrap();
                let mut starved = 0u64;
                let now_fed = f.load(std::sync::atomic::Ordering::Relaxed);
                let talking = {
                    let mut seen = seen_fed.lock().unwrap();
                    let moved = now_fed != *seen;
                    *seen = now_fed;
                    moved
                };
                let mut held = last.lock().unwrap();

                // Do not start on an empty queue, and do not restart on a
                // nearly empty one. The guest produces in bursts and, on a
                // host that cannot quite keep up, slightly slower than the
                // device drains; without a cushion the callback runs dry
                // every few milliseconds, which is what the harshness is.
                // Filling for the first time is worth waiting for; recovering
                // from a dropout is not. Waiting for the whole cushion again
                // would turn every dropout into a quarter-second of silence,
                // which is a far worse thing to hear than the gap that caused
                // it.
                let mut priming = prime_flag.lock().unwrap();
                if let Some(wanted) = *priming {
                    if q.len() < wanted {
                        out.fill(0.0);
                        return;
                    }
                    *priming = None;
                }

                for frame in out.chunks_mut(channels as usize) {
                    match (q.pop_front(), q.pop_front()) {
                        (Some(l), Some(r)) => *held = (l, r),
                        _ => {
                            // Hold the last sample rather than dropping to
                            // zero. A gap of silence is a click; a held level
                            // is a much softer artefact, and going back to
                            // priming means it happens once per dropout
                            // instead of once per frame.
                            starved += 1;
                            *priming = Some(resume_samples);
                        }
                    }
                    let (l, r) = (held.0 as f32 / 32768.0, held.1 as f32 / 32768.0);
                    for (i, sample) in frame.iter_mut().enumerate() {
                        *sample = if i % 2 == 0 { l } else { r };
                    }
                }
                if starved > 0 && talking {
                    *u.lock().unwrap() += starved;
                }
            },
            move |err| eprintln!("audio: {err}"),
            None,
        );

        match stream {
            Ok(s) => {
                let started = s.play().is_ok();
                (
                    AudioOut {
                        queue,
                        resample: Mutex::new(Resample { pos: 0.0, last: (0, 0) }),
                        stream: Some(s),
                        device_rate,
                        channels,
                        underruns,
                        fed,
                        guest_spoke: std::sync::atomic::AtomicBool::new(false),
                    },
                    (!started).then(|| "output stream would not start".to_string()),
                )
            }
            Err(e) => (
                AudioOut {
                    queue,
                    resample: Mutex::new(Resample { pos: 0.0, last: (0, 0) }),
                    stream: None,
                    device_rate,
                    channels,
                    underruns,
                    fed,
                    guest_spoke: std::sync::atomic::AtomicBool::new(false),
                },
                Some(format!("could not open output stream ({e}); audio will be discarded")),
            ),
        }
    }

    pub fn is_live(&self) -> bool {
        self.stream.is_some()
    }

    /// Push AC97 PCM words, each carrying a left sample in the low half and a
    /// right sample in the high half, produced at `src_rate`.
    pub fn push_ac97(&self, words: &[u32], src_rate: u32) {
        if self.stream.is_none() || words.is_empty() {
            return;
        }
        self.guest_spoke.store(true, std::sync::atomic::Ordering::Relaxed);
        self.fed
            .fetch_add(words.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let src_rate = if src_rate == 0 { AC97_RATE } else { src_rate };
        let mut q = self.queue.lock().unwrap();
        let mut rs = self.resample.lock().unwrap();

        // Linear interpolation from the guest's rate to the device's. The
        // guest asks for 44100 while most host devices run at 48000, so
        // without this everything plays about nine percent fast.
        //
        // Exactly that ratio, and nothing clever on top.
        //
        // There was, briefly, a controller here that nudged the ratio to keep
        // the queue at the callback's cushion depth, meaning to make up for
        // the emulator falling behind real time. It was measuring the wrong
        // thing. Queue depth is not a rate deficit: a machine that is saying
        // nothing has an empty queue, which is the normal and correct state,
        // and the controller read that as starvation and stretched the audio
        // for as long as it lasted. Measured over a boot it sat between -3%
        // and -6%, pinned at its own clamp much of the time, and swung about
        // three percent within a burst -- audible, and heard, as speech that
        // climbs in pitch as each phrase goes on.
        //
        // A dropout is a fault worth fixing; a voice that wanders is a fault
        // too, and this cure was worse than the disease. What actually carries
        // the machine through a stall is the cushion below, which is a
        // quarter of a second, and the shortfall it was built for does not
        // arise at the clock the emulator now defaults to.
        let step = src_rate as f64 / self.device_rate as f64;
        for w in words {
            let left = (*w & 0xFFFF) as u16 as i16;
            let right = ((*w >> 16) & 0xFFFF) as u16 as i16;
            while rs.pos < 1.0 {
                let t = rs.pos as f32;
                let l = rs.last.0 as f32 + (left as f32 - rs.last.0 as f32) * t;
                let r = rs.last.1 as f32 + (right as f32 - rs.last.1 as f32) * t;
                q.push_back(l as i16);
                q.push_back(r as i16);
                rs.pos += step;
            }
            rs.pos -= 1.0;
            rs.last = (left, right);
        }
        // Never let the backlog grow without bound: better to drop old audio
        // than to drift further and further behind.
        let cap = (self.device_rate as usize) * 2; // one second of stereo
        while q.len() > cap {
            q.pop_front();
        }
    }

    /// How much audio the callback keeps in hand before it starts playing.
    ///
    /// A quarter of a second. It used to be a tenth, and a tenth is not enough
    /// on this emulator: the shortfall is not a steady one that a resampler
    /// can stretch away but a *stall* -- the interpreter drops to 92% of real
    /// time while the machine is busy, which is exactly while it is talking,
    /// and during a stall there is no audio to stretch. What rides that out is
    /// having more of it already in hand.
    ///
    /// The cost is latency, and this is the machine's own voice rather than a
    /// key click, so a quarter of a second of it is not felt.
    pub fn cushion_samples(device_rate: u32) -> usize {
        (device_rate as usize / 4) * 2
    }

    /// How many samples are waiting to be played.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn queued(&self) -> usize {
        self.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Whether the machine itself has made a sound yet.
    pub fn guest_has_spoken(&self) -> bool {
        self.guest_spoke.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Play a tone, on the host's own output rather than the machine's.
    ///
    /// A reset is silent: the machine simply stops being where it was, and a
    /// user who cannot see the screen has nothing to tell them it happened
    /// until the boot chime arrives seconds later. A triangle wave is used
    /// rather than a sine because it carries through small speakers, and it
    /// is not a sound the guest can make, so it cannot be mistaken for the
    /// machine speaking.
    pub fn push_tone(&self, hz: f32, seconds: f32, level: f32) {
        let rate = self.device_rate.max(1) as f32;
        let frames = (rate * seconds) as usize;
        let mut q = match self.queue.lock() {
            Ok(q) => q,
            Err(_) => return,
        };
        for n in 0..frames {
            let phase = (n as f32 * hz / rate).fract();
            // A triangle: up for the first half of the cycle, down for the
            // second.
            let tri = if phase < 0.5 { 4.0 * phase - 1.0 } else { 3.0 - 4.0 * phase };
            // Fade in and out, because a square-edged start clicks.
            let edge = (frames as f32 * 0.05).max(1.0);
            let fade = (n as f32 / edge).min(1.0).min((frames - n) as f32 / edge);
            let v = (tri * fade * level * i16::MAX as f32) as i16;
            q.push_back(v);
            q.push_back(v);
        }
    }

    pub fn underruns(&self) -> u64 {
        *self.underruns.lock().unwrap()
    }
}

/// Accumulates everything the guest played, for writing a WAV afterwards.
#[derive(Default)]
pub struct WavRecorder {
    pub samples: Vec<i16>,
    /// Set once the cap was hit and audio was dropped.
    pub truncated: bool,
    /// Rate the guest produced these samples at, written into the header.
    pub rate: u32,
}

/// Cap on captured audio, so a runaway DMA chain cannot fill the disk.
const MAX_RECORDED_SECONDS: usize = 300;

impl WavRecorder {
    pub fn push_ac97(&mut self, words: &[u32], rate: u32) {
        if rate != 0 {
            self.rate = rate;
        }
        if self.samples.len() >= MAX_RECORDED_SECONDS * AC97_RATE as usize * 2 {
            self.truncated = true;
            return;
        }
        for w in words {
            self.samples.push((*w & 0xFFFF) as u16 as i16);
            self.samples.push(((*w >> 16) & 0xFFFF) as u16 as i16);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn write(&self, path: &str) -> std::io::Result<()> {
        let data_len = (self.samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // stereo
        let rate = if self.rate == 0 { AC97_RATE } else { self.rate };
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 4).to_le_bytes()); // byte rate
        out.extend_from_slice(&4u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, out)
    }
}

/// Cuts the guest's speech into utterances, one file each.
///
/// The machine talks in bursts with silence between them, and a burst is one
/// thing it wanted to say: a prompt, an answer, a beep. Waiting for the run to
/// finish and transcribing one long WAV loses which key each burst was
/// answering, so this writes each burst out as it ends and something else can
/// pick them up while the machine is still running.
///
/// A gap is judged two ways, because the guest uses both. Sometimes it keeps
/// the codec fed with near-zero samples between phrases, which shows up as
/// quiet inside a block; sometimes it stops feeding it at all, and then no
/// block arrives and the only evidence is guest time passing. Watching only
/// the samples means every burst runs into the next one and the whole run
/// comes out as a single utterance.
pub struct Utterances {
    dir: std::path::PathBuf,
    current: WavRecorder,
    /// Samples of quiet since the last sound above the floor.
    quiet: usize,
    written: u32,
    gap: f64,
}

/// Below this a sample counts as silence. The codec's idle output is not
/// exactly zero, so a floor of nothing would never fire.
const SILENCE_FLOOR: i16 = 96;
/// Quiet needed to call an utterance finished, in seconds. Default only:
/// `Utterances::new` takes the figure to use.
///
/// Too short and a prompt is cut into pieces at its own commas, which is no
/// use to something trying to read it; too long and an answer arrives after
/// the machine has moved on.
const GAP_SECONDS: f64 = 1.5;
/// Sound needed before a burst is worth writing at all, in seconds. Below this
/// it is a click or the tail of the last one.
const MIN_SPEECH_SECONDS: f64 = 0.25;

impl Utterances {
    pub fn new(dir: &str, gap: f64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Utterances {
            dir: std::path::PathBuf::from(dir),
            current: WavRecorder::default(),
            quiet: 0,
            written: 0,
            gap: if gap > 0.0 { gap } else { GAP_SECONDS },
        })
    }

    /// Feed it what the guest just played. Returns the path of an utterance
    /// when one has just ended.
    pub fn push(&mut self, words: &[u32], rate: u32) -> Option<std::path::PathBuf> {
        let loud = words.iter().any(|w| {
            let l = (*w & 0xFFFF) as u16 as i16;
            let r = ((*w >> 16) & 0xFFFF) as u16 as i16;
            l.saturating_abs() > SILENCE_FLOOR || r.saturating_abs() > SILENCE_FLOOR
        });
        self.current.push_ac97(words, rate);
        if loud {
            self.quiet = 0;
            return None;
        }
        self.quiet += words.len();
        let rate = if self.current.rate == 0 { AC97_RATE } else { self.current.rate } as f64;
        if (self.quiet as f64) < self.gap * rate {
            return None;
        }
        self.finish(rate)
    }

    /// Write whatever has been collected, if it is long enough to be speech.
    fn finish(&mut self, rate: f64) -> Option<std::path::PathBuf> {
        let frames = self.current.samples.len() / 2;
        let speech = frames.saturating_sub(self.quiet);
        self.quiet = 0;
        if (speech as f64) < MIN_SPEECH_SECONDS * rate {
            self.current.samples.clear();
            return None;
        }
        self.written += 1;
        let path = self.dir.join(format!("utt-{:03}.wav", self.written));
        let r = self.current.write(&path.to_string_lossy());
        self.current.samples.clear();
        match r {
            Ok(()) => Some(path),
            Err(e) => {
                eprintln!("utterances: cannot write {}: {e}", path.display());
                None
            }
        }
    }

    /// Whether there is a burst in progress worth ending.
    pub fn pending(&self) -> bool {
        !self.current.is_empty()
    }

    /// Flush a burst the run ended in the middle of.
    pub fn flush(&mut self) -> Option<std::path::PathBuf> {
        if self.current.is_empty() {
            return None;
        }
        let rate = if self.current.rate == 0 { AC97_RATE } else { self.current.rate } as f64;
        self.finish(rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guest's rate and the device's, and nothing else in between.
    ///
    /// A regression test for a cure that was worse than the disease. A
    /// controller used to sit in `push_ac97` adjusting the ratio by up to six
    /// percent to hold the queue at the callback's cushion, which on a machine
    /// that is quiet most of the time meant stretching the audio nearly
    /// always: measured over a boot it ran between -3% and -6% and swung about
    /// three percent within a single burst, which is heard as a phrase
    /// climbing in pitch as it goes on.
    ///
    /// So: the same input must give the same amount of output whatever is
    /// already waiting to be played.
    #[test]
    fn the_rate_does_not_depend_on_the_backlog() {
        let (a, _) = AudioOut::new();
        if !a.is_live() {
            return;                 // no output device here to resample for
        }
        let words = vec![0u32; 512];

        let before = a.queued();
        a.push_ac97(&words, 44_100);
        let with_nothing_waiting = a.queued() - before;

        // Again, with a backlog this time.
        let midway = a.queued();
        a.push_ac97(&words, 44_100);
        let with_a_backlog = a.queued() - midway;

        let drift = with_nothing_waiting as i64 - with_a_backlog as i64;
        assert!(
            drift.abs() <= 2,
            "{with_nothing_waiting} samples on an empty queue against              {with_a_backlog} on a full one: the rate is following the backlog"
        );
    }
}
