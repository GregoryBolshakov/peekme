//! The terminal's own selection on a Mac, read through Accessibility.
//!
//! An agent that leaves the mouse to the terminal (Kiro inline, Claude classic)
//! leaves the selection there too, and Terminal.app never copies it to the
//! clipboard. Its text view tells Accessibility clients (VoiceOver) what is
//! selected, so peekme asks for it the same way. macOS allows that only once
//! the user lets the terminal app use Accessibility, in System Settings.

/// What one look at the focused element found, for the event log and `doctor`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Probe {
    pub trusted: bool,
    /// The step that failed and its AXError, e.g. `focused -25204`.
    pub error: Option<String>,
    /// AXRole of the focused element.
    pub role: Option<String>,
    pub text: Option<String>,
}

impl Probe {
    pub fn fields(&self) -> serde_json::Value {
        serde_json::json!({
            "trusted": self.trusted,
            "error": self.error,
            "role": self.role,
            "chars": self.text.as_ref().map(|t| t.chars().count()),
        })
    }
}

/// Whether macOS lets this process (in fact its terminal app) use Accessibility.
pub fn trusted() -> bool {
    imp::trusted()
}

/// Ask macOS to show its dialog for letting the terminal app use
/// Accessibility. Returns whether it is allowed already.
pub fn request() -> bool {
    imp::request()
}

/// The text selected in the focused element of the frontmost app: the
/// terminal window the user is typing in.
pub fn probe() -> Probe {
    imp::probe()
}

/// The selected text, if Accessibility is allowed and something is selected.
pub fn selected_text() -> Option<String> {
    if !trusted() {
        return None;
    }
    probe().text.filter(|t| !t.trim().is_empty())
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn trusted() -> bool {
        false
    }
    pub fn request() -> bool {
        false
    }
    pub fn probe() -> super::Probe {
        super::Probe::default()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type AXError = i32;

    const UTF8: u32 = 0x0800_0100;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> u8;
        fn AXUIElementCreateSystemWide() -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(
            element: CFTypeRef,
            attribute: CFTypeRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> AXError;
        static kAXTrustedCheckOptionPrompt: CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringCreateWithBytes(
            alloc: CFTypeRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CFTypeRef;
        fn CFStringGetLength(s: CFTypeRef) -> isize;
        fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut u8, size: isize, encoding: u32) -> u8;
        fn CFDictionaryCreate(
            alloc: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
    }

    /// A CF object this code owns, released on drop.
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn cfstr(s: &str) -> Owned {
        Owned(unsafe {
            CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0)
        })
    }

    fn string(cf: CFTypeRef) -> Option<String> {
        if cf.is_null() || unsafe { CFGetTypeID(cf) != CFStringGetTypeID() } {
            return None;
        }
        let size = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(cf), UTF8) } + 1;
        let mut buf = vec![0u8; size.max(1) as usize];
        if unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), size, UTF8) } == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(end);
        String::from_utf8(buf).ok()
    }

    fn attribute(element: CFTypeRef, name: &str) -> Result<Owned, AXError> {
        let name = cfstr(name);
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut value) };
        if err != 0 {
            return Err(err);
        }
        Ok(Owned(value))
    }

    pub fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    pub fn request() -> bool {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = Owned(CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            ));
            AXIsProcessTrustedWithOptions(options.0) != 0
        }
    }

    pub fn probe() -> super::Probe {
        let mut p = super::Probe {
            trusted: trusted(),
            ..Default::default()
        };
        if !p.trusted {
            return p;
        }
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        // A busy terminal must not stall the keyboard: give up after half a second.
        unsafe { AXUIElementSetMessagingTimeout(system.0, 0.5) };
        let focused = match attribute(system.0, "AXFocusedUIElement") {
            Ok(f) => f,
            Err(e) => {
                p.error = Some(format!("focused {e}"));
                return p;
            }
        };
        p.role = attribute(focused.0, "AXRole")
            .ok()
            .and_then(|r| string(r.0));
        match attribute(focused.0, "AXSelectedText") {
            Ok(t) => p.text = string(t.0),
            Err(e) => p.error = Some(format!("selected {e}")),
        }
        p
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// The frameworks link and a look never fails loudly, allowed or not.
    #[test]
    fn probe_answers() {
        let p = super::probe();
        assert_eq!(p.trusted, super::trusted());
        if !p.trusted {
            assert_eq!(p, super::Probe::default());
        }
    }
}
