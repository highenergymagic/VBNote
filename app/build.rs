//! Windows version information for `vbnote.exe`.
//!
//! Without this the file's Details tab is blank: no company, no product, no
//! copyright. That matters more than it looks. An unsigned executable already
//! makes SmartScreen suspicious, and one with no identifying information at
//! all makes it more so -- and a user who right-clicks to check what they have
//! been given deserves an answer.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("CompanyName", "Fractal Microsystems");
        res.set("ProductName", "VBNote");
        res.set("FileDescription", "VBNote - VoiceNote QT mPower emulator");
        res.set("LegalCopyright", "Copyright (C) 2026 Fractal Microsystems. GPL-2.0-only.");
        res.set("OriginalFilename", "vbnote.exe");
        // From Cargo.toml rather than written out again here, so a release is
        // one place to change instead of two that can disagree.
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        if let Err(e) = res.compile() {
            // Not fatal: the emulator is the same program without it, and a
            // build host with no resource compiler should still get one.
            println!("cargo:warning=no version information embedded: {e}");
        }
    }
}
