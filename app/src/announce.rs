//! Saying what just happened, out loud.
//!
//! Everything the host key does is invisible. The keyboard being captured or
//! released, the power switch flipping, a reset -- none of it changes anything
//! on screen, and the machine itself cannot report them because most of them
//! are done *to* the machine rather than by it. For a user who cannot see the
//! screen, an unannounced mode change is a machine that has silently stopped
//! answering, which is the worst failure this emulator has.
//!
//! So the host side speaks for itself. This is deliberately separate from the
//! guest's own voice: the guest speaks through the AC97 codec, and these come
//! out of the host's speech engine, so a status message cannot be mistaken for
//! something KeySoft said.
//!
//! # NVDA first, the system voice second
//!
//! Announcements go through **NVDA** when it is there. That gets the voice,
//! the rate and the braille display the user has already set up, and it queues
//! against what NVDA is saying rather than talking over it -- which matters,
//! because the alternative is two voices at once and the important one losing.
//!
//! It needs `nvdaControllerClient.dll`, and this project's rule is that a
//! release needs nothing beside it, so the DLL is **loaded at run time and
//! only if it is there**. Nothing links against it, nothing fails to start
//! without it, and a machine with no NVDA falls back to the system speech
//! engine, which is part of Windows and always present. Put the DLL beside the
//! binary (or anywhere on the search path) to get the better one.
//!
//! The fallback is a real fallback, not a courtesy: it is what a user with
//! JAWS, or none, will hear. Both paths are kept working.
//!
//! Messages are short either way, because a status line spoken over the top of
//! KeySoft is only useful if it is over quickly.

use std::sync::mpsc::{self, Sender};

/// A handle for saying things. Cheap to clone and safe to use from any thread.
#[derive(Clone)]
pub struct Voice {
    to_speaker: Option<Sender<String>>,
}

/// Whatever is going to do the talking.
enum Speaker {
    /// NVDA, through its controller client.
    Nvda(nvda::Client),
    /// The system speech engine.
    System(Box<tts::Tts>),
}

impl Speaker {
    /// Find the best one available, and say which it was.
    fn best() -> (Option<Speaker>, String) {
        if let Some(client) = nvda::Client::load() {
            return (Some(Speaker::Nvda(client)), "speech: NVDA".into());
        }
        match tts::Tts::default() {
            Ok(e) => (
                Some(Speaker::System(Box::new(e))),
                "speech: the system voice (no NVDA found)".into(),
            ),
            Err(e) => (None, format!("speech: nothing available ({e})")),
        }
    }

    fn say(&mut self, line: &str) {
        match self {
            // Interrupting is right for both: these are status messages, and
            // the newest is the true one. Queueing "captured" behind
            // "released" would describe the machine as it was.
            Speaker::Nvda(c) => c.speak(line),
            Speaker::System(e) => {
                let _ = e.speak(line, true);
            }
        }
    }
}

/// NVDA's controller client, loaded at run time if it happens to be there.
mod nvda {
    /// The DLL under the names it ships as. The plain one is what current
    /// NVDA installs; the numbered ones are older and are still about.
    const NAMES: &[&str] =
        &["nvdaControllerClient.dll", "nvdaControllerClient64.dll", "nvdaControllerClient32.dll"];

    /// Full paths to try, which are only ever beside this executable.
    ///
    /// Deliberately not bare names. Loading a library by name uses the
    /// operating system's search order, and that order has ended up including
    /// the current working directory often enough to be a well-known way to
    /// get a program to run somebody else's code: leave a file with the right
    /// name in a directory the program is started from, and it is loaded with
    /// the program's privileges. Nothing here needs that flexibility -- the
    /// documented way to get NVDA's voice is to put the DLL beside the
    /// binary -- so only that one place is looked in, by absolute path.
    fn candidates() -> Vec<std::path::PathBuf> {
        let beside = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        match beside {
            Some(dir) => NAMES.iter().map(|n| dir.join(n)).collect(),
            None => Vec::new(),
        }
    }

    type TestIfRunning = unsafe extern "system" fn() -> u32;
    type SpeakText = unsafe extern "system" fn(*const u16) -> u32;
    type CancelSpeech = unsafe extern "system" fn() -> u32;

    pub struct Client {
        // Kept so the function pointers stay valid; never used directly.
        _library: libloading::Library,
        speak: SpeakText,
        cancel: CancelSpeech,
    }

    // The controller client is documented as callable from any thread, and
    // this one is only ever used from the speech thread.
    unsafe impl Send for Client {}

    impl Client {
        pub fn load() -> Option<Client> {
            for name in candidates() {
                // Safety: loading a library runs its initialiser. This one is
                // NVDA's own, and if the name resolves to something else the
                // symbols below will not be found and it is dropped again.
                if !name.exists() {
                    continue;
                }
                let library = match unsafe { libloading::Library::new(&name) } {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                // Every failure here moves on to the next candidate rather
                // than abandoning the search: a file of the right name that
                // turns out not to be this library should not cost the ones
                // after it.
                let found = unsafe {
                    let running: Option<libloading::Symbol<TestIfRunning>> =
                        library.get(b"nvdaController_testIfRunning\0").ok();
                    let speak: Option<libloading::Symbol<SpeakText>> =
                        library.get(b"nvdaController_speakText\0").ok();
                    let cancel: Option<libloading::Symbol<CancelSpeech>> =
                        library.get(b"nvdaController_cancelSpeech\0").ok();
                    match (running, speak, cancel) {
                        // Zero means NVDA is up. A DLL present with no NVDA
                        // behind it would take every message and drop it
                        // silently, which is worse than not using it.
                        (Some(running), Some(speak), Some(cancel)) if running() == 0 => {
                            Some((*speak, *cancel))
                        }
                        _ => None,
                    }
                };
                if let Some((speak, cancel)) = found {
                    return Some(Client { _library: library, speak, cancel });
                }
            }
            None
        }

        pub fn speak(&self, line: &str) {
            let mut wide: Vec<u16> = line.encode_utf16().collect();
            wide.push(0);
            unsafe {
                (self.cancel)();
                (self.speak)(wide.as_ptr());
            }
        }
    }
}

impl Voice {
    /// Start the speech thread.
    ///
    /// Speech runs on its own thread and is never waited on: the emulator's
    /// loop must not stall because an engine took a moment, and a status
    /// message that arrives late is better than a machine that stutters.
    /// If there is no engine, this still succeeds and says nothing -- silence
    /// is a poor experience, but refusing to run is a worse one.
    pub fn start() -> (Self, Option<String>) {
        let (tx, rx) = mpsc::channel::<String>();
        let (ready, ready_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            // Both engines are built on the thread that will use them, which
            // is what the system one requires and what keeps the NVDA client
            // out of everybody else's way.
            let (mut speaker, how) = Speaker::best();
            let _ = ready.send(how);
            while let Ok(line) = rx.recv() {
                if let Some(s) = speaker.as_mut() {
                    s.say(&line);
                }
            }
        });
        let how = ready_rx.recv().unwrap_or_else(|_| "speech: unavailable".into());
        (Voice { to_speaker: Some(tx) }, Some(how))
    }

    /// A voice that says nothing, for runs with no user at the keyboard.
    pub fn silent() -> Self {
        Voice { to_speaker: None }
    }

    pub fn say(&self, what: &str) {
        // Also to the console, so a log of a session says what the user was
        // told and when.
        println!("[{what}]");
        if let Some(tx) = &self.to_speaker {
            let _ = tx.send(what.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A silent voice must still be usable. Every call site would otherwise
    /// need to know whether speech came up, and the one that forgot would
    /// panic on a machine with no engine.
    #[test]
    fn a_silent_voice_still_takes_messages() {
        let v = Voice::silent();
        v.say("keyboard captured");
        let _ = v.clone();
    }
}
