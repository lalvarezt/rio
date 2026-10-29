// clipboard.rs was retired originally from https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty/src/clipboard.rs
// which is licensed under Apache 2.0 license.

pub mod buffer;
#[cfg(not(target_arch = "wasm32"))]
pub mod jobs;
pub mod kitty;
pub mod typed;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Ask,
    Allow,
    Deny,
}

/// Policy for application clipboard requests. User copy/paste is independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClipboardPolicy {
    pub read: Access,
    pub write: Access,
    pub write_limit_bytes: usize,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            read: Access::Ask,
            write: Access::Allow,
            write_limit_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardType {
    Clipboard,
    Selection,
}

/// Encode pasted text for the terminal's current bracketed-paste mode.
/// Bracketed payloads collapse CRLF to CR, retain lone line breaks, and omit
/// ESC, ETX, and C1 CSI so the payload cannot end its frame.
/// Plain payloads normalize CRLF and LF to CR.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let filtered = text
            .replace(['\x1b', '\x03', '\u{9b}'], "")
            .replace("\r\n", "\r");
        format!("\x1b[200~{filtered}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

// The `Clipboard` helper below backs onto the system clipboard via copypasta,
// which pulls in windowing/AppKit dependencies. It is only used by frontends;
// ClipboardType and paste encoding remain available to headless embedders, so
// only the system clipboard implementation is gated behind this feature.
#[cfg(feature = "clipboard")]
use raw_window_handle::RawDisplayHandle;
#[cfg(feature = "clipboard")]
use tracing::warn;

#[cfg(feature = "clipboard")]
use copypasta::nop_clipboard::NopClipboardContext;
#[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
use copypasta::x11_clipboard::{Primary as X11SelectionClipboard, X11ClipboardContext};
#[cfg(all(
    feature = "clipboard",
    any(feature = "x11", target_os = "macos", windows)
))]
use copypasta::ClipboardContext;
#[cfg(feature = "clipboard")]
use copypasta::ClipboardProvider;

#[cfg(feature = "clipboard")]
pub struct Clipboard {
    provider_jobs: Option<jobs::Jobs>,
    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    wayland: Option<smithay_clipboard::Clipboard>,
    typed_available: bool,
    clipboard: Box<dyn ClipboardProvider>,
    selection: Option<Box<dyn ClipboardProvider>>,
}

#[cfg(feature = "clipboard")]
impl Clipboard {
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn new(display: RawDisplayHandle) -> Self {
        match display {
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            RawDisplayHandle::Wayland(display) => Self {
                wayland: Some(smithay_clipboard::Clipboard::new(
                    display.display.as_ptr(),
                )),
                typed_available: true,
                provider_jobs: None,
                clipboard: Box::new(NopClipboardContext::new().unwrap()),
                selection: None,
            },
            _ => Self::default(),
        }
    }

    /// Used for tests and to handle missing clipboard provider when built without the `x11`
    /// feature.
    pub fn new_nop() -> Self {
        Self {
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            wayland: None,
            typed_available: false,
            provider_jobs: None,
            clipboard: Box::new(NopClipboardContext::new().unwrap()),
            selection: None,
        }
    }
}

#[cfg(feature = "clipboard")]
impl Default for Clipboard {
    fn default() -> Self {
        #[cfg(any(target_os = "macos", windows))]
        return Self {
            typed_available: true,
            provider_jobs: None,
            clipboard: Box::new(ClipboardContext::new().unwrap()),
            selection: None,
        };

        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        return Self {
            #[cfg(feature = "wayland")]
            wayland: None,
            typed_available: true,
            provider_jobs: None,
            clipboard: Box::new(ClipboardContext::new().unwrap()),
            selection: Some(Box::new(
                X11ClipboardContext::<X11SelectionClipboard>::new().unwrap(),
            )),
        };

        #[cfg(not(any(feature = "x11", target_os = "macos", windows)))]
        return Self::new_nop();
    }
}

#[cfg(feature = "clipboard")]
impl Clipboard {
    pub fn set(&mut self, ty: ClipboardType, text: impl Into<String>) {
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(wayland) = &self.wayland {
            match ty {
                ClipboardType::Clipboard => wayland.store(text),
                ClipboardType::Selection => wayland.store_primary(text),
            }
            return;
        }
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            (ClipboardType::Selection, None) => return,
            _ => &mut self.clipboard,
        };

        clipboard.set_contents(text.into()).unwrap_or_else(|err| {
            warn!("Unable to store text in clipboard: {}", err);
        });
    }

    pub fn get(&mut self, ty: ClipboardType) -> String {
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(wayland) = &self.wayland {
            return match match ty {
                ClipboardType::Clipboard => wayland.load(),
                ClipboardType::Selection => wayland.load_primary(),
            } {
                Ok(text) => text,
                Err(error) => {
                    warn!("Unable to load text from clipboard: {error}");
                    String::new()
                }
            };
        }
        let clipboard = match (ty, &mut self.selection) {
            (ClipboardType::Selection, Some(provider)) => provider,
            _ => &mut self.clipboard,
        };

        match clipboard.get_contents() {
            Err(err) => {
                warn!("Unable to load text from clipboard: {}", err);
                String::new()
            }
            Ok(text) => text,
        }
    }
}

#[cfg(test)]
mod paste_tests {
    use super::encode_paste;

    // Cases adapted from Ghostty's src/input/paste.zig while preserving Rio's
    // filtering and newline behavior.
    #[test]
    fn empty_payloads_follow_framing_mode() {
        assert_eq!(encode_paste("", false), b"");
        assert_eq!(encode_paste("", true), b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn plain_paste_normalizes_each_line_break_once() {
        assert_eq!(encode_paste("a\rb\nc\r\nd\n\ne", false), b"a\rb\rc\rd\r\re");
    }

    #[test]
    fn bracketed_paste_collapses_crlf_and_preserves_lone_breaks_and_tabs() {
        assert_eq!(
            encode_paste("a\rb\nc\r\nd\te", true),
            b"\x1b[200~a\rb\nc\rd\te\x1b[201~"
        );
    }

    #[test]
    fn windows_clipboard_does_not_double_bracketed_line_breaks() {
        assert_eq!(
            encode_paste("first\r\nsecond\r\n\r\n", true),
            b"\x1b[200~first\rsecond\r\r\x1b[201~"
        );
    }

    #[test]
    fn bracketed_paste_cannot_close_its_frame_with_csi_or_esc() {
        assert_eq!(
            encode_paste("a\x1b[201~b\u{9b}201~c\x03", true),
            b"\x1b[200~a[201~b201~c\x1b[201~"
        );
    }

    #[test]
    fn filtering_unicode_csi_does_not_strip_utf8_continuation_bytes() {
        let text = "Û👩‍💻中e\u{301}\u{9b}";
        assert_eq!(
            encode_paste(text, true),
            "\x1b[200~Û👩‍💻中e\u{301}\x1b[201~".as_bytes()
        );
        assert_eq!(encode_paste(text, false), text.as_bytes());
    }

    #[test]
    fn repeated_frame_breaking_controls_are_all_filtered() {
        assert_eq!(
            encode_paste(&"\x1b\x03\u{9b}".repeat(1024), true),
            b"\x1b[200~\x1b[201~"
        );
    }
}
