use std::cell::Cell;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::{Arc, mpsc::Sender};
use std::time::Duration;

use sctk::data_device_manager::{ReadPipe, WritePipe};
use sctk::reexports::calloop::timer::{TimeoutAction, Timer};
use sctk::reexports::calloop::{LoopHandle, PostAction, RegistrationToken};

pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRANSFERS: usize = 16;

#[derive(Clone, Default)]
pub(crate) struct Budget {
    active: Rc<Cell<usize>>,
    reads: Rc<Cell<usize>>,
    writes: Rc<Cell<usize>>,
}

struct Transfer {
    budget: Budget,
    bytes: Rc<Cell<usize>>,
    charged: usize,
    timer: Rc<Cell<Option<RegistrationToken>>>,
    remove: Box<dyn Fn(RegistrationToken)>,
}

impl Transfer {
    fn new<D: 'static>(
        handle: &LoopHandle<'static, D>,
        budget: &Budget,
        read: bool,
    ) -> io::Result<Self> {
        if budget.active.get() >= MAX_TRANSFERS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many clipboard transfers",
            ));
        }
        budget.active.set(budget.active.get() + 1);
        let weak = handle.downgrade();
        Ok(Self {
            budget: budget.clone(),
            bytes: if read {
                budget.reads.clone()
            } else {
                budget.writes.clone()
            },
            charged: 0,
            timer: Rc::new(Cell::new(None)),
            remove: Box::new(move |token| {
                if let Some(handle) = weak.upgrade() {
                    handle.remove(token);
                }
            }),
        })
    }

    fn reserve(&mut self, bytes: usize) -> io::Result<()> {
        if bytes > MAX_BYTES.saturating_sub(self.bytes.get()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "clipboard transfer byte budget exhausted",
            ));
        }
        self.bytes.set(self.bytes.get() + bytes);
        self.charged += bytes;
        Ok(())
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.bytes.set(self.bytes.get() - self.charged);
        self.budget.active.set(self.budget.active.get() - 1);
        if let Some(timer) = self.timer.take() {
            (self.remove)(timer);
        }
    }
}

fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
            < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn deadline<D: 'static>(
    handle: &LoopHandle<'static, D>,
    source: RegistrationToken,
    timer: Rc<Cell<Option<RegistrationToken>>>,
    timeout: Duration,
    expired: impl FnOnce() + 'static,
) -> io::Result<()> {
    let weak = handle.downgrade();
    let timer_cell = timer.clone();
    let mut expired = Some(expired);
    match handle.insert_source(Timer::from_duration(timeout), move |_, _, _| {
        timer_cell.set(None);
        if let Some(handle) = weak.upgrade() {
            handle.remove(source);
        }
        if let Some(expired) = expired.take() {
            expired();
        }
        TimeoutAction::Drop
    }) {
        Ok(token) => {
            timer.set(Some(token));
            Ok(())
        }
        Err(error) => {
            handle.remove(source);
            Err(io::Error::other(error.to_string()))
        }
    }
}

pub(crate) fn read<D: 'static>(
    handle: &LoopHandle<'static, D>,
    pipe: ReadPipe,
    limit: usize,
    budget: &Budget,
    timeout: Duration,
    reply: Sender<io::Result<Vec<u8>>>,
) -> io::Result<()> {
    let mut transfer = Transfer::new(handle, budget, true)?;
    nonblocking(&pipe)?;
    let timer = transfer.timer.clone();
    let read_reply = reply.clone();
    let mut buffer = Vec::new();
    let limit = limit.min(MAX_BYTES);
    let token = handle
        .insert_source(pipe, move |_, file, _| {
            let file = unsafe { file.get_mut() };
            let mut scratch = [0; 4096];
            loop {
                let result = match file.read(&mut scratch) {
                    Ok(0) => Ok(std::mem::take(&mut buffer)),
                    Ok(n) if n > limit.saturating_sub(buffer.len()) => {
                        Err(io::Error::new(
                            io::ErrorKind::FileTooLarge,
                            "clipboard read limit exceeded",
                        ))
                    }
                    Ok(n) => {
                        let needed = buffer.len() + n;
                        if needed > buffer.capacity() {
                            let capacity = needed.next_multiple_of(64 * 1024).min(limit);
                            if let Err(error) =
                                transfer.reserve(capacity - buffer.capacity())
                            {
                                let _ = read_reply.send(Err(error));
                                break PostAction::Remove;
                            }
                            buffer.reserve_exact(capacity - buffer.len());
                        }
                        buffer.extend_from_slice(&scratch[..n]);
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        break PostAction::Continue;
                    }
                    Err(error) => Err(error),
                };
                let _ = read_reply.send(result);
                break PostAction::Remove;
            }
        })
        .map_err(|error| io::Error::other(error.to_string()))?;
    deadline(handle, token, timer, timeout, move || {
        let _ = reply.send(Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "clipboard source stalled",
        )));
    })
}

pub(crate) fn write<D: 'static>(
    handle: &LoopHandle<'static, D>,
    pipe: WritePipe,
    contents: Arc<Vec<u8>>,
    budget: &Budget,
    timeout: Duration,
) -> io::Result<()> {
    let mut transfer = Transfer::new(handle, budget, false)?;
    transfer.reserve(contents.len())?;
    nonblocking(&pipe)?;
    let timer = transfer.timer.clone();
    let mut written = 0;
    let token = handle
        .insert_source(pipe, move |_, file, _| {
            let _keep_budget = &transfer;
            let file = unsafe { file.get_mut() };
            loop {
                match file.write(&contents[written..]) {
                    Ok(0) => break PostAction::Remove,
                    Ok(n) => {
                        written += n;
                        if written == contents.len() {
                            break PostAction::Remove;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        break PostAction::Continue;
                    }
                    Err(_) => break PostAction::Remove,
                }
            }
        })
        .map_err(|error| io::Error::other(error.to_string()))?;
    deadline(handle, token, timer, timeout, || {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use sctk::reexports::calloop::EventLoop;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    #[test]
    fn stalled_read_times_out_and_releases_every_reservation() {
        let mut event_loop = EventLoop::<()>::try_new().unwrap();
        let budget = Budget::default();
        let (pipe, _peer) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::channel();
        read(
            &event_loop.handle(),
            OwnedFd::from(pipe).into(),
            100,
            &budget,
            Duration::from_millis(10),
            tx,
        )
        .unwrap();
        event_loop
            .dispatch(Duration::from_millis(100), &mut ())
            .unwrap();
        assert_eq!(
            rx.recv().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(budget.active.get(), 0);
        assert_eq!(budget.reads.get(), 0);
    }

    #[test]
    fn successful_and_oversized_reads_release_their_timers() {
        for limit in [0, 3] {
            let mut event_loop = EventLoop::<()>::try_new().unwrap();
            let budget = Budget::default();
            let (pipe, mut peer) = UnixStream::pair().unwrap();
            peer.write_all(b"abc").unwrap();
            peer.shutdown(std::net::Shutdown::Write).unwrap();
            let (tx, rx) = mpsc::channel();
            read(
                &event_loop.handle(),
                OwnedFd::from(pipe).into(),
                limit,
                &budget,
                Duration::from_secs(1),
                tx,
            )
            .unwrap();
            event_loop
                .dispatch(Duration::from_millis(100), &mut ())
                .unwrap();
            let result = rx.recv().unwrap();
            if limit == 0 {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::FileTooLarge);
            } else {
                assert_eq!(result.unwrap(), b"abc");
            }
            assert_eq!(budget.active.get(), 0);
            assert_eq!(budget.reads.get(), 0);
        }
    }

    #[test]
    fn stalled_writer_releases_payload_and_write_budget() {
        let mut event_loop = EventLoop::<()>::try_new().unwrap();
        let budget = Budget::default();
        let payload = Arc::new(vec![1; 1024 * 1024]);
        let (pipe, _peer) = UnixStream::pair().unwrap();
        write(
            &event_loop.handle(),
            OwnedFd::from(pipe).into(),
            payload.clone(),
            &budget,
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(budget.writes.get(), payload.len());
        for _ in 0..5 {
            event_loop
                .dispatch(Duration::from_millis(20), &mut ())
                .unwrap();
        }
        assert_eq!(budget.active.get(), 0);
        assert_eq!(budget.writes.get(), 0);
        assert_eq!(Arc::strong_count(&payload), 1);
    }

    #[test]
    fn transfer_count_and_retained_write_bytes_are_bounded() {
        let event_loop = EventLoop::<()>::try_new().unwrap();
        let budget = Budget::default();
        let leases: Vec<_> = (0..MAX_TRANSFERS)
            .map(|_| Transfer::new(&event_loop.handle(), &budget, true).unwrap())
            .collect();
        assert!(
            matches!(Transfer::new(&event_loop.handle(), &budget, false), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        drop(leases);
        let mut first = Transfer::new(&event_loop.handle(), &budget, false).unwrap();
        let mut second = Transfer::new(&event_loop.handle(), &budget, false).unwrap();
        first.reserve(MAX_BYTES).unwrap();
        assert_eq!(
            second.reserve(1).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        second.reserve(1).unwrap();
    }

    #[test]
    fn dropping_worker_loop_releases_active_transfer_payloads() {
        let event_loop = EventLoop::<()>::try_new().unwrap();
        let budget = Budget::default();
        let payload = Arc::new(vec![1; 1024]);
        let (pipe, _peer) = UnixStream::pair().unwrap();
        write(
            &event_loop.handle(),
            OwnedFd::from(pipe).into(),
            payload.clone(),
            &budget,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(budget.active.get(), 1);
        drop(event_loop);
        assert_eq!(budget.active.get(), 0);
        assert_eq!(budget.writes.get(), 0);
        assert_eq!(Arc::strong_count(&payload), 1);
    }
}
