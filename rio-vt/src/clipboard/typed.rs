//! Typed clipboard boundary. Providers publish all representations atomically
//! and report unsupported formats before altering clipboard ownership.
use super::{kitty::Content, ClipboardType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Invalid,
    Io,
    TooLarge,
}

pub struct Snapshot {
    pub available: Vec<String>,
    pub contents: Vec<Content>,
}

pub trait Provider {
    fn text(&mut self, location: ClipboardType) -> Result<String, Error> {
        let snapshot = self.read(location, &["text/plain".to_owned()])?;
        Ok(snapshot
            .contents
            .first()
            .map_or_else(String::new, |content| {
                String::from_utf8_lossy(&content.data).into_owned()
            }))
    }
    fn formats(&mut self, location: ClipboardType) -> Result<Vec<String>, Error> {
        self.read(location, &[]).map(|snapshot| snapshot.available)
    }
    fn read(
        &mut self,
        location: ClipboardType,
        mimes: &[String],
    ) -> Result<Snapshot, Error>;
    fn write(
        &mut self,
        location: ClipboardType,
        contents: &[Content],
    ) -> Result<(), Error>;
}

#[cfg(any(feature = "clipboard", test))]
const TEXT_MIMES: [&str; 2] = ["text/plain", "text/plain;charset=utf-8"];

/// Validate the complete transaction before calling a string-only adapter.
/// Multiple text aliases are representable only when their bytes agree.
#[cfg(any(feature = "clipboard", test))]
fn text_write(contents: &[Content]) -> Result<&str, Error> {
    let mut text = None;
    for content in contents {
        if !TEXT_MIMES.contains(&content.mime.as_str()) {
            return Err(Error::Unsupported);
        }
        let value = std::str::from_utf8(&content.data).map_err(|_| Error::Invalid)?;
        if text.is_some_and(|old| old != value) {
            return Err(Error::Unsupported);
        }
        text = Some(value);
    }
    Ok(text.unwrap_or_default())
}

#[cfg(feature = "clipboard")]
impl Provider for super::Clipboard {
    fn formats(&mut self, location: ClipboardType) -> Result<Vec<String>, Error> {
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(wayland) = &self.wayland {
            return WaylandProvider(wayland.handle()).formats(location);
        }
        self.read(location, &[]).map(|snapshot| snapshot.available)
    }
    fn read(
        &mut self,
        location: ClipboardType,
        mimes: &[String],
    ) -> Result<Snapshot, Error> {
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(wayland) = &self.wayland {
            return WaylandProvider(wayland.handle()).read(location, mimes);
        }
        if !self.typed_available {
            return Err(Error::Unsupported);
        }
        let provider = match location {
            ClipboardType::Clipboard => &mut self.clipboard,
            ClipboardType::Selection => {
                self.selection.as_mut().ok_or(Error::Unsupported)?
            }
        };
        let text = provider.get_contents().map_err(|_| Error::Io)?;
        if text.len() > 64 * 1024 * 1024 {
            return Err(Error::TooLarge);
        }
        let data = std::sync::Arc::new(text.into_bytes());
        Ok(Snapshot {
            available: TEXT_MIMES.iter().map(|mime| (*mime).to_owned()).collect(),
            contents: mimes
                .iter()
                .filter(|mime| TEXT_MIMES.contains(&mime.as_str()))
                .map(|mime| Content {
                    mime: mime.clone(),
                    data: data.clone(),
                })
                .collect(),
        })
    }

    fn write(
        &mut self,
        location: ClipboardType,
        contents: &[Content],
    ) -> Result<(), Error> {
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(wayland) = &self.wayland {
            return WaylandProvider(wayland.handle()).write(location, contents);
        }
        if !self.typed_available {
            return Err(Error::Unsupported);
        }
        let text = text_write(contents)?;
        let provider = match location {
            ClipboardType::Clipboard => &mut self.clipboard,
            ClipboardType::Selection => {
                self.selection.as_mut().ok_or(Error::Unsupported)?
            }
        };
        provider
            .set_contents(text.to_owned())
            .map_err(|_| Error::Io)
    }
}

#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
fn wayland_error(error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::Unsupported => Error::Unsupported,
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidData => {
            Error::Invalid
        }
        std::io::ErrorKind::FileTooLarge => Error::TooLarge,
        _ => Error::Io,
    }
}

#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
struct WaylandProvider(smithay_clipboard::ClipboardHandle);
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
impl Provider for WaylandProvider {
    fn text(&mut self, location: ClipboardType) -> Result<String, Error> {
        match location {
            ClipboardType::Clipboard => self.0.load(),
            ClipboardType::Selection => self.0.load_primary(),
        }
        .map_err(wayland_error)
    }
    fn formats(&mut self, location: ClipboardType) -> Result<Vec<String>, Error> {
        let wayland = &self.0;
        wayland
            .mime_types(location == ClipboardType::Selection)
            .map_err(wayland_error)
    }
    fn read(
        &mut self,
        location: ClipboardType,
        mimes: &[String],
    ) -> Result<Snapshot, Error> {
        let wayland = &self.0;
        let primary = location == ClipboardType::Selection;
        let offer = wayland.offer(primary).map_err(wayland_error)?;
        let available = offer.mimes;
        let mut remaining = 64 * 1024 * 1024;
        let mut contents = Vec::new();
        for mime in mimes.iter().take(4).filter(|mime| available.contains(mime)) {
            let data = wayland
                .load_mime(primary, offer.id.clone(), mime.clone(), remaining)
                .map_err(wayland_error)?;
            remaining -= data.len();
            contents.push(Content {
                mime: mime.clone(),
                data: std::sync::Arc::new(data),
            });
        }
        Ok(Snapshot {
            available,
            contents,
        })
    }
    fn write(
        &mut self,
        location: ClipboardType,
        contents: &[Content],
    ) -> Result<(), Error> {
        let wayland = &self.0;
        wayland
            .store_mimes(
                location == ClipboardType::Selection,
                contents
                    .iter()
                    .map(|content| (content.mime.clone(), content.data.clone()))
                    .collect(),
            )
            .map_err(wayland_error)
    }
}
#[cfg(feature = "clipboard")]
impl super::Clipboard {
    fn background_provider(&self) -> Option<Box<dyn Provider + Send>> {
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(wayland) = &self.wayland {
            return Some(Box::new(WaylandProvider(wayland.handle())));
        }
        None
    }
    pub fn queue_job(
        &mut self,
        job: impl FnOnce(&mut dyn Provider) + Send + 'static,
    ) -> Result<(), super::jobs::Error> {
        if self.provider_jobs.is_none() {
            let provider = self
                .background_provider()
                .ok_or(super::jobs::Error::Unsupported)?;
            self.provider_jobs = Some(
                super::jobs::Jobs::new(provider)
                    .map_err(|_| super::jobs::Error::Stopped)?,
            );
        }
        self.provider_jobs.as_ref().unwrap().submit(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn text_bridge_rejects_lossy_or_partial_publication() {
        let text = Content {
            mime: "text/plain".into(),
            data: Arc::new(b"hello".to_vec()),
        };
        let png = Content {
            mime: "image/png".into(),
            data: Arc::new(vec![0]),
        };
        assert_eq!(text_write(&[text, png]), Err(Error::Unsupported));
        assert_eq!(
            text_write(&[Content {
                mime: "text/plain".into(),
                data: Arc::new(vec![255])
            }]),
            Err(Error::Invalid)
        );
    }
    #[test]
    fn identical_aliases_can_be_published_together() {
        let data = Arc::new(b"hello".to_vec());
        let contents: Vec<_> = TEXT_MIMES
            .iter()
            .map(|mime| Content {
                mime: (*mime).into(),
                data: data.clone(),
            })
            .collect();
        assert_eq!(text_write(&contents), Ok("hello"));
    }
}
