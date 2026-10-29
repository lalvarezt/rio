use std::io::{Error, ErrorKind, Result};
use std::sync::mpsc::Sender;
use std::time::Instant;

use sctk::reexports::calloop::channel::Channel;
use sctk::reexports::calloop::{EventLoop, channel};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::Connection;
use sctk::reexports::client::globals::registry_queue_init;

use crate::state::{SelectionTarget, State};

/// Spawn a clipboard worker, which dispatches its own `EventQueue` and handles
/// clipboard requests.
pub(crate) fn spawn(
    name: String,
    display: Connection,
    rx_chan: Channel<Message>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            worker_impl(display, rx_chan);
        })
        .ok()
}

/// Clipboard worker thread command.
pub enum Command {
    Types(SelectionTarget, Sender<Result<crate::Offer>>),
    LoadMime(
        SelectionTarget,
        crate::OfferId,
        String,
        usize,
        Sender<Result<Vec<u8>>>,
    ),
    StoreMimes(SelectionTarget, crate::MimeContents, Sender<Result<()>>),
    /// Store data to a clipboard.
    Store(String),
    /// Store data to a primary selection.
    StorePrimary(String),
}

pub(crate) enum Message {
    Request(Queued),
    Exit,
}

pub(crate) struct Queued {
    pub command: Command,
    pub admission: crate::admission::Admission,
    pub deadline: Instant,
}

impl Queued {
    fn ready(self, now: Instant) -> Option<(Command, crate::admission::Admission)> {
        if now >= self.deadline {
            self.command.expire();
            None
        } else {
            Some((self.command, self.admission))
        }
    }
}

impl Command {
    pub(crate) fn retained_bytes(&self) -> Result<usize> {
        let overflow =
            || Error::new(ErrorKind::FileTooLarge, "clipboard command size overflow");
        match self {
            Self::Types(..) => Ok(0),
            Self::LoadMime(_, _, mime, _, _) => Ok(mime.capacity()),
            Self::Store(text) | Self::StorePrimary(text) => Ok(text.capacity()),
            Self::StoreMimes(_, contents, _) => {
                let mut bytes = contents
                    .capacity()
                    .checked_mul(std::mem::size_of::<(String, std::sync::Arc<Vec<u8>>)>())
                    .ok_or_else(overflow)?;
                let mut buffers = std::collections::HashSet::new();
                for (mime, data) in contents {
                    bytes = bytes.checked_add(mime.capacity()).ok_or_else(overflow)?;
                    if buffers.insert(std::sync::Arc::as_ptr(data)) {
                        bytes =
                            bytes.checked_add(data.capacity()).ok_or_else(overflow)?;
                    }
                }
                Ok(bytes)
            }
        }
    }

    fn expire(self) {
        let error = || {
            Error::new(
                ErrorKind::TimedOut,
                "clipboard command expired before dispatch",
            )
        };
        match self {
            Self::Types(_, reply) => {
                let _ = reply.send(Err(error()));
            }
            Self::LoadMime(_, _, _, _, reply) => {
                let _ = reply.send(Err(error()));
            }
            Self::StoreMimes(_, _, reply) => {
                let _ = reply.send(Err(error()));
            }
            Self::Store(_) | Self::StorePrimary(_) => (),
        }
    }
}

/// Handle clipboard requests.
fn worker_impl(connection: Connection, rx_chan: Channel<Message>) {
    let (globals, event_queue) = match registry_queue_init(&connection) {
        Ok(data) => data,
        Err(_) => return,
    };

    let mut event_loop = EventLoop::<State>::try_new().unwrap();
    let loop_handle = event_loop.handle();

    let mut state = match State::new(&globals, &event_queue.handle(), loop_handle.clone())
    {
        Some(state) => state,
        None => return,
    };

    let sync_connection = connection.clone();
    loop_handle
        .insert_source(rx_chan, move |event, _, state| {
            if let channel::Event::Msg(event) = event {
                if state.exit {
                    return;
                }
                let Message::Request(queued) = event else {
                    state.exit = true;
                    return;
                };
                let Some((command, _admission)) = queued.ready(Instant::now()) else {
                    return;
                };
                // Queue ownership ends after dispatch. Reads and retained
                // selection sources have separate bounded state budgets.
                match command {
                    Command::Types(target, reply) => {
                        let result = state.offer(target);
                        let _ = reply.send(result);
                    }
                    Command::LoadMime(target, offer, mime, limit, reply) => {
                        if let Err(error) =
                            state.load_mime(target, offer, mime, limit, reply.clone())
                        {
                            let _ = reply.send(Err(error));
                        }
                    }
                    Command::StoreMimes(target, contents, reply) => {
                        match state.store_mimes(target, contents) {
                            Ok(()) => state.acknowledge_store(&sync_connection, reply),
                            Err(error) => {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                    Command::StorePrimary(contents) => {
                        state.store_selection(SelectionTarget::Primary, contents);
                    }
                    Command::Store(contents) => {
                        state.store_selection(SelectionTarget::Clipboard, contents);
                    }
                }
            } else {
                state.exit = true;
            }
        })
        .unwrap();

    WaylandSource::new(connection, event_queue)
        .insert(loop_handle)
        .unwrap();

    loop {
        if event_loop.dispatch(None, &mut state).is_err() || state.exit {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::Budget;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn retained_bytes_include_allocated_payload_and_metadata_capacity() {
        for primary in [false, true] {
            let mut text = String::with_capacity(2048);
            text.push('x');
            let capacity = text.capacity();
            let command = if primary {
                Command::StorePrimary(text)
            } else {
                Command::Store(text)
            };
            assert_eq!(command.retained_bytes().unwrap(), capacity);
        }

        let payload = Arc::new(Vec::with_capacity(1024));
        let mut contents = Vec::with_capacity(8);
        let mut mime = String::with_capacity(256);
        mime.push_str("text/plain");
        contents.push((mime, payload.clone()));
        contents.push(("UTF8_STRING".to_string(), payload.clone()));
        let expected = contents.capacity()
            * std::mem::size_of::<(String, Arc<Vec<u8>>)>()
            + contents
                .iter()
                .map(|(name, _)| name.capacity())
                .sum::<usize>()
            + payload.capacity();
        let (tx, _) = mpsc::channel();
        assert_eq!(
            Command::StoreMimes(SelectionTarget::Clipboard, contents, tx)
                .retained_bytes()
                .unwrap(),
            expected
        );
    }

    #[test]
    fn dispatched_command_keeps_queue_credit_until_handoff_finishes() {
        let budget = Budget::new(1, 8);
        let now = Instant::now();
        let queued = Queued {
            command: Command::Store("text".into()),
            admission: budget.reserve(4).unwrap(),
            deadline: now + Duration::from_secs(1),
        };
        let dispatched = queued.ready(now).unwrap();
        assert_eq!(
            budget.reserve(0).err().unwrap().kind(),
            ErrorKind::WouldBlock
        );
        drop(dispatched);
        budget.reserve(8).unwrap();
    }

    #[test]
    fn expired_commands_reply_and_release_admission_without_dispatch() {
        let budget = Budget::new(1, 8);
        let now = Instant::now();
        let (tx, rx) = mpsc::channel();
        let queued = Queued {
            command: Command::Types(SelectionTarget::Clipboard, tx),
            admission: budget.reserve(0).unwrap(),
            deadline: now,
        };
        assert!(queued.ready(now).is_none());
        assert_eq!(
            rx.recv().unwrap().err().unwrap().kind(),
            ErrorKind::TimedOut
        );
        let (tx, rx) = mpsc::channel();
        let queued = Queued {
            command: Command::StoreMimes(SelectionTarget::Primary, Vec::new(), tx),
            admission: budget.reserve(0).unwrap(),
            deadline: now,
        };
        assert!(queued.ready(now).is_none());
        assert_eq!(
            rx.recv().unwrap().err().unwrap().kind(),
            ErrorKind::TimedOut
        );
        let (tx, rx) = mpsc::channel();
        let queued = Queued {
            command: Command::LoadMime(
                SelectionTarget::Clipboard,
                crate::OfferId {
                    seat: wayland_backend::client::ObjectId::null(),
                    source: crate::OfferSource::Clipboard(None),
                },
                "text/plain".into(),
                8,
                tx,
            ),
            admission: budget.reserve(0).unwrap(),
            deadline: now,
        };
        assert!(queued.ready(now).is_none());
        assert_eq!(
            rx.recv().unwrap().err().unwrap().kind(),
            ErrorKind::TimedOut
        );
        budget.reserve(8).unwrap();
    }
}
