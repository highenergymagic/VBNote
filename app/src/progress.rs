//! Saying how far the machine has got, while it is still too early to hear it.
//!
//! A cold start is about ninety seconds and the machine is silent for most of
//! it. Silence is exactly what a machine that has failed to start sounds like,
//! so something has to fill it.
//!
//! It used to be a beep -- 2 kHz, every five seconds, until the guest's first
//! sample. That says "not dead" and nothing else, eighteen times, and a user
//! waiting through it learns nothing from the seventeenth that they did not
//! know at the second.
//!
//! So instead the emulator says what is happening, and it says it when
//! something actually happens rather than on a timer. Each stage below is a
//! thing the runner can genuinely see the guest do, in roughly the order a
//! boot does them.
//!
//! # Why it is a pure function of what was seen
//!
//! The board is not passed in. The runner gathers a [`Sight`] -- a handful of
//! booleans -- and this decides what to say about it. That way the whole
//! sequence can be tested without a machine, and the awkward cases (a stage
//! that never happens, stages arriving out of order, the guest finding its
//! voice halfway through) are ordinary tests rather than ninety-second boots.

/// What the runner can see about how far the machine has got.
#[derive(Default, Clone, Copy, Debug)]
pub struct Sight {
    /// The MMU is on, which on this machine means the kernel is up and
    /// running from virtual addresses rather than the bootloader's physical
    /// ones.
    pub kernel_running: bool,
    /// A process other than the kernel is scheduled -- CE's FCSE slot is no
    /// longer zero -- so the system is running programs.
    pub programs_running: bool,
    /// The USB driver has powered the root hub, which happens as the built-in
    /// drivers are loaded.
    pub drivers_loaded: bool,
    /// The keyboard driver has swept the matrix, so keystrokes would now be
    /// noticed.
    pub keyboard_ready: bool,
    /// KeySoft has asked the modem a question, which it does while working
    /// through its first-run setup.
    pub first_run_setup: bool,
    /// The guest has produced audio. From here it speaks for itself and this
    /// says nothing more.
    pub guest_spoke: bool,
    /// Seconds since the run began.
    pub seconds: u64,
}

/// How long a silence is allowed to last before saying something anyway.
///
/// Stages are not evenly spaced -- there is a long quiet stretch in the middle
/// of a boot -- and going quiet for forty seconds after promising progress is
/// worse than never having promised any.
const PATIENCE: u64 = 15;

/// When to stop saying it is still starting and admit it should have finished.
///
/// A boot is about ninety seconds. Four minutes in, "still starting" is no
/// longer the news -- the news is that something is wrong -- and repeating a
/// reassurance for ever is how a user is left waiting on a machine that is
/// never going to arrive. It also bounds the whole sequence for a run where
/// the guest's audio is not being watched at all, which would otherwise
/// have no ending.
const GIVE_UP: u64 = 240;

/// A stage: how to tell it has been reached, and what to say about it.
type Stage = (fn(&Sight) -> bool, &'static str);

const STAGES: [Stage; 5] = [
    (|s| s.kernel_running, "Windows starting."),
    (|s| s.programs_running, "Loading programs."),
    (|s| s.drivers_loaded, "Loading drivers."),
    (|s| s.keyboard_ready, "Keyboard ready."),
    (
        |s| s.first_run_setup,
        "Setting up for the first time. This part is slow.",
    ),
];

/// Tracks which stages have been announced, and how long it has been quiet.
pub struct Progress {
    said: [bool; STAGES.len()],
    last_spoke_at: u64,
    finished: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Progress::new()
    }
}

impl Progress {
    pub fn new() -> Progress {
        Progress {
            said: [false; STAGES.len()],
            last_spoke_at: 0,
            finished: false,
        }
    }

    /// What to say now, if anything.
    ///
    /// At most one thing per call: two announcements arriving together would
    /// be read as one run-on sentence, and stages do sometimes land in the
    /// same instant.
    pub fn update(&mut self, sight: &Sight) -> Option<String> {
        // Once the machine has its own voice, anything said over it is in the
        // way. This is the only thing that ends the sequence, and it ends it
        // for good -- a machine that falls silent again is not starting up.
        if self.finished {
            return None;
        }
        if sight.guest_spoke {
            self.finished = true;
            return None;
        }

        for (i, (reached, phrase)) in STAGES.iter().enumerate() {
            if !self.said[i] && reached(sight) {
                self.said[i] = true;
                self.last_spoke_at = sight.seconds;
                return Some((*phrase).to_string());
            }
        }

        if sight.seconds >= self.last_spoke_at + PATIENCE {
            self.last_spoke_at = sight.seconds;
            if sight.seconds >= GIVE_UP {
                self.finished = true;
                return Some(
                    "Still starting after four minutes. Something is probably wrong.".to_string(),
                );
            }
            return Some(format!("Still starting. {} seconds.", sight.seconds));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing_yet() -> Sight {
        Sight::default()
    }

    /// Each stage is announced once and only once, however long it stays true.
    #[test]
    fn a_stage_is_announced_once() {
        let mut p = Progress::new();
        let mut s = nothing_yet();
        s.kernel_running = true;
        assert_eq!(p.update(&s).as_deref(), Some("Windows starting."));
        s.seconds = 1;
        assert_eq!(p.update(&s), None);
        s.seconds = 2;
        assert_eq!(p.update(&s), None);
    }

    /// One thing at a time. Stages can become true together, and two phrases
    /// returned at once would be spoken as one sentence.
    #[test]
    fn only_one_thing_is_said_at_a_time() {
        let mut p = Progress::new();
        let s = Sight {
            kernel_running: true,
            programs_running: true,
            drivers_loaded: true,
            keyboard_ready: true,
            ..Default::default()
        };
        assert_eq!(p.update(&s).as_deref(), Some("Windows starting."));
        assert_eq!(p.update(&s).as_deref(), Some("Loading programs."));
        assert_eq!(p.update(&s).as_deref(), Some("Loading drivers."));
        assert_eq!(p.update(&s).as_deref(), Some("Keyboard ready."));
        assert_eq!(p.update(&s), None);
    }

    /// A stage that never happens must not block the ones after it. The
    /// modem question only comes up on a first-run boot, so on every later
    /// boot the last stage never arrives at all.
    #[test]
    fn a_stage_that_never_happens_blocks_nothing() {
        let mut p = Progress::new();
        let s = Sight {
            keyboard_ready: true,
            ..Default::default()
        };
        assert_eq!(p.update(&s).as_deref(), Some("Keyboard ready."));
    }

    /// The whole point: a long quiet stretch still says something.
    #[test]
    fn silence_is_broken_eventually() {
        let mut p = Progress::new();
        let mut s = nothing_yet();
        s.seconds = PATIENCE - 1;
        assert_eq!(p.update(&s), None);
        s.seconds = PATIENCE;
        assert_eq!(
            p.update(&s).as_deref(),
            Some("Still starting. 15 seconds.")
        );
        // And not again straight away.
        s.seconds = PATIENCE + 1;
        assert_eq!(p.update(&s), None);
    }

    /// A stage resets the patience: reaching one is news, and the heartbeat
    /// exists only because news is sometimes a long way off.
    #[test]
    fn reaching_a_stage_resets_the_patience() {
        let mut p = Progress::new();
        let mut s = nothing_yet();
        s.seconds = 10;
        s.kernel_running = true;
        assert_eq!(p.update(&s).as_deref(), Some("Windows starting."));
        s.seconds = 10 + PATIENCE - 1;
        assert_eq!(p.update(&s), None, "heartbeat came too soon after a stage");
        s.seconds = 10 + PATIENCE;
        assert!(p.update(&s).is_some());
    }

    /// Once the machine speaks, this stops for good -- including the
    /// heartbeat, and including stages that had not been reached yet.
    #[test]
    fn the_machine_finding_its_voice_ends_it() {
        let mut p = Progress::new();
        let mut s = nothing_yet();
        s.guest_spoke = true;
        assert_eq!(p.update(&s), None);

        s.guest_spoke = false;
        s.seconds = 100;
        s.kernel_running = true;
        assert_eq!(p.update(&s), None, "it started talking over the machine again");
    }

    /// Nothing is said before there is anything to say. A boot that gets
    /// going promptly should not be narrated from zero.
    #[test]
    fn nothing_is_said_at_the_very_start() {
        let mut p = Progress::new();
        assert_eq!(p.update(&nothing_yet()), None);
    }

    /// It gives up rather than reassuring for ever. A run with no audio to
    /// watch never sees the guest speak, and without this it would say "still
    /// starting" until the machine was switched off -- which is exactly what
    /// a `--mute` boot did.
    #[test]
    fn it_stops_reassuring_eventually() {
        let mut p = Progress::new();
        let mut s = nothing_yet();
        let mut last = None;
        for second in 0..=GIVE_UP + PATIENCE * 3 {
            s.seconds = second;
            if let Some(said) = p.update(&s) {
                last = Some(said);
            }
        }
        assert_eq!(
            last.as_deref(),
            Some("Still starting after four minutes. Something is probably wrong.")
        );
        // And nothing after that, for ever.
        s.seconds = GIVE_UP * 10;
        assert_eq!(p.update(&s), None);
    }
}
