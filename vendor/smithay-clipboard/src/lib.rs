//! Smithay Clipboard
//!
//! Provides access to the Wayland clipboard for gui applications. The user
//! should have surface around.

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
use std::ffi::c_void;
use std::io::Result;
use std::sync::mpsc;

use sctk::reexports::calloop::channel::{self, Sender};
use sctk::reexports::client::Connection;
use sctk::reexports::client::backend::Backend;

mod admission;
mod mime;
mod state;
mod transfer;
mod worker;

/// Immutable MIME representations; aliases can share payload ownership.
pub type MimeContents = Vec<(String, std::sync::Arc<Vec<u8>>)>;
/// Identity of the seat, selection, and offer captured during enumeration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferId {
    pub(crate) seat: wayland_backend::client::ObjectId,
    pub(crate) source: OfferSource,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OfferSource {
    Clipboard(Option<wayland_backend::client::ObjectId>),
    Primary(Option<sctk::primary_selection::offer::PrimarySelectionOffer>),
}
pub struct Offer {
    pub id: OfferId,
    pub mimes: Vec<String>,
}

/// Access to a Wayland clipboard.
#[derive(Clone)]
/// Cloneable request sender. The owning Clipboard controls worker lifetime;
/// requests fail once that owner shuts the worker down.
pub struct ClipboardHandle {
    request_sender: Sender<worker::Message>,
    command_budget: admission::Budget,
}

pub struct Clipboard {
    handle: ClipboardHandle,
    clipboard_thread: Option<std::thread::JoinHandle<()>>,
}

impl ClipboardHandle {
    fn submit(&self, command: worker::Command) -> Result<()> {
        let admission = self.command_budget.reserve(command.retained_bytes()?)?;
        self.request_sender
            .send(worker::Message::Request(worker::Queued {
                command,
                admission,
                deadline: std::time::Instant::now() + std::time::Duration::from_secs(6),
            }))
            .map_err(|_| std::io::Error::other("clipboard worker stopped"))
    }

    fn target(primary: bool) -> state::SelectionTarget {
        if primary {
            state::SelectionTarget::Primary
        } else {
            state::SelectionTarget::Clipboard
        }
    }

    /// Enumerate an offer without requesting its data.
    pub fn mime_types(&self, primary: bool) -> Result<Vec<String>> {
        self.offer(primary).map(|offer| offer.mimes)
    }

    pub fn offer(&self, primary: bool) -> Result<Offer> {
        let (tx, rx) = mpsc::channel();
        self.submit(worker::Command::Types(Self::target(primary), tx))?;
        rx.recv_timeout(std::time::Duration::from_secs(6))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "clipboard worker timeout",
                )
            })?
    }

    /// Read one MIME representation from the enumerated offer with a strict byte limit.
    /// Returns `Interrupted` if the active seat or selection offer has changed.
    pub fn load_mime(
        &self,
        primary: bool,
        offer: OfferId,
        mime: String,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        self.submit(worker::Command::LoadMime(
            Self::target(primary),
            offer,
            mime,
            limit,
            tx,
        ))?;
        rx.recv_timeout(std::time::Duration::from_secs(6))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "clipboard transfer timeout",
                )
            })?
    }

    /// Publish all MIME representations with one selection ownership change.
    pub fn store_mimes(&self, primary: bool, contents: MimeContents) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.submit(worker::Command::StoreMimes(
            Self::target(primary),
            contents,
            tx,
        ))?;
        rx.recv_timeout(std::time::Duration::from_secs(6))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "clipboard worker timeout",
                )
            })?
    }
    fn load_text(&self, primary: bool) -> Result<String> {
        let offer = self.offer(primary)?;
        let mime = mime::MimeType::find_allowed(&offer.mimes).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "text format unavailable")
        })?;
        let bytes =
            self.load_mime(primary, offer.id, mime.to_string(), 64 * 1024 * 1024)?;
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
        };
        Ok(match mime {
            mime::MimeType::Utf8String => text,
            _ => mime::normalize_to_lf(text),
        })
    }

    /// Load clipboard data.
    ///
    /// Loads content from a clipboard on a last observed seat.
    pub fn load(&self) -> Result<String> {
        self.load_text(false)
    }

    /// Store to a clipboard.
    ///
    /// Stores to a clipboard on a last observed seat.
    pub fn store<T: Into<String>>(&self, text: T) {
        let request = worker::Command::Store(text.into());
        let _ = self.submit(request);
    }

    /// Load primary clipboard data.
    ///
    /// Loads content from a  primary clipboard on a last observed seat.
    pub fn load_primary(&self) -> Result<String> {
        self.load_text(true)
    }

    /// Store to a primary clipboard.
    ///
    /// Stores to a primary clipboard on a last observed seat.
    pub fn store_primary<T: Into<String>>(&self, text: T) {
        let request = worker::Command::StorePrimary(text.into());
        let _ = self.submit(request);
    }
}

impl Clipboard {
    /// Creates new clipboard which will be running on its own thread with its
    /// own event queue to handle clipboard requests.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `*mut wl_display` pointer, and it must remain
    /// valid for as long as `Clipboard` object is alive.
    pub unsafe fn new(display: *mut c_void) -> Self {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);

        // Create channel to send data to clipboard thread.
        let (request_sender, rx_chan) = channel::channel();

        let name = String::from("smithay-clipboard");
        let clipboard_thread = worker::spawn(name, connection, rx_chan);

        Self {
            handle: ClipboardHandle {
                request_sender,
                command_budget: Default::default(),
            },
            clipboard_thread,
        }
    }

    pub fn handle(&self) -> ClipboardHandle {
        self.handle.clone()
    }
}
impl std::ops::Deref for Clipboard {
    type Target = ClipboardHandle;
    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // Shutdown smithay-clipboard.
        self.command_budget.close();
        let _ = self.request_sender.send(worker::Message::Exit);
        if let Some(clipboard_thread) = self.clipboard_thread.take() {
            let _ = clipboard_thread.join();
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    #[test]
    fn failed_send_releases_command_credit() {
        let (request_sender, receiver) = channel::channel();
        drop(receiver);
        let budget = admission::Budget::new(1, 8);
        let handle = ClipboardHandle {
            request_sender,
            command_budget: budget.clone(),
        };
        assert!(
            handle
                .submit(worker::Command::Store("text".into()))
                .is_err()
        );
        budget.reserve(8).unwrap();
    }
}
