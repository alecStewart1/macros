//! macOS Secure Event Input for password reads.
//!
//! While enabled, other processes can't observe keystrokes through event taps
//! (keyloggers, accessibility snoopers), and input methods fall back to plain
//! Roman entry, so an IME candidate window never shows the secret. It's what
//! `NSSecureTextField` turns on for the duration of a password field's focus.
//!
//! `EnableSecureEventInput` is reference-counted system-wide and must be
//! balanced exactly, so it's held as an RAII guard: the guard lives inside the
//! secret `Minibuffer`, and every way that read ends (submit, abort, being
//! replaced by another read) drops it. Other platforms: a no-op.

pub struct SecureInputGuard(());

#[cfg(all(target_os = "macos", not(test)))]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
}

impl SecureInputGuard {
    pub fn new() -> Self {
        // Tests drive password reads headlessly; flipping a real system-wide
        // input mode there would only interfere with the developer's session.
        #[cfg(all(target_os = "macos", not(test)))]
        // SAFETY: plain Carbon calls with no arguments; balanced by Drop.
        unsafe {
            EnableSecureEventInput();
        }
        SecureInputGuard(())
    }
}

impl Drop for SecureInputGuard {
    fn drop(&mut self) {
        #[cfg(all(target_os = "macos", not(test)))]
        // SAFETY: pairs with the Enable in `new`.
        unsafe {
            DisableSecureEventInput();
        }
    }
}
