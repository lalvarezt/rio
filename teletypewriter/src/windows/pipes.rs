use super::spsc::*;
use corcovado::{
    event::Evented, Poll, PollOpt, Ready, Registration, SetReadiness, Token,
};
use std::io::{self, Read, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{spawn, JoinHandle};

const BUFFER_CAPACITY: usize = 65536;

struct PipeState {
    registration: Registration,
    readiness: SetReadiness,
    stop: AtomicBool,
    finished: AtomicBool,
    error: Mutex<Option<io::Error>>,
}

impl PipeState {
    fn new() -> Arc<Self> {
        let (registration, readiness) = Registration::new2();
        Arc::new(Self {
            registration,
            readiness,
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            error: Mutex::new(None),
        })
    }

    fn finish(&self, result: io::Result<()>, ready: Ready) {
        *self.error.lock().unwrap() = result.err();
        self.finished.store(true, Ordering::Release);
        // EOF and failures must wake the event loop even when no bytes remain.
        let _ = self.readiness.set_readiness(ready);
    }

    fn take_error(&self) -> io::Result<()> {
        match self.error.lock().unwrap().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn stop(&self, worker: Option<JoinHandle<()>>) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = worker {
            worker.thread().unpark();
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::System::Threading::WaitForSingleObject;
                use windows_sys::Win32::System::IO::CancelSynchronousIo;

                // A single cancellation can precede the worker's next I/O call.
                // Retry until it observes stop or its in-flight I/O is cancelled.
                while !worker.is_finished() {
                    unsafe {
                        CancelSynchronousIo(worker.as_raw_handle());
                    }
                    // Wake immediately on exit; only an I/O-entry race needs
                    // another cancellation attempt after the short timeout.
                    unsafe {
                        WaitForSingleObject(worker.as_raw_handle(), 1);
                    }
                }
            }
            // Dropping a pipe after an I/O error must not panic or join twice.
            let _ = worker.join();
        }
    }
}

/// Synchronous pipe output exposed as nonblocking, evented reads.
pub struct EventedAnonRead {
    thread: Option<JoinHandle<()>>,
    consumer: SpscBufferReader,
    inner: Arc<PipeState>,
}

impl EventedAnonRead {
    pub fn new(mut pipe: impl Read + Send + 'static) -> Self {
        let (mut producer, consumer) = spsc_buffer(BUFFER_CAPACITY);
        let inner = PipeState::new();
        let state = inner.clone();
        let thread = spawn(move || {
            let result = (|| {
                while !state.stop.load(Ordering::Acquire) {
                    if producer.is_full() {
                        // unpark retains a token if space becomes available
                        // between this predicate and park, so no wake is lost.
                        std::thread::park();
                        continue;
                    }
                    match producer.read_from(&mut pipe) {
                        Ok(0) => return Ok(()),
                        Ok(_) => {
                            if !state.readiness.readiness().is_readable() {
                                state.readiness.set_readiness(Ready::readable())?;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            })();
            state.finish(result, Ready::readable());
        });
        Self {
            thread: Some(thread),
            consumer,
            inner,
        }
    }
}

impl Read for EventedAnonRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let count = self.consumer.read_to_slice(buf);
        if self.consumer.is_empty() {
            self.inner.readiness.set_readiness(Ready::empty())?;
            // Publishing and clearing readiness can race. Recheck both data
            // and completion after clearing so neither notification is lost.
            if !self.consumer.is_empty() || self.inner.finished.load(Ordering::Acquire) {
                self.inner.readiness.set_readiness(Ready::readable())?;
            }
        }
        if count != 0 {
            self.thread.as_ref().unwrap().thread().unpark();
            return Ok(count);
        }
        if self.inner.finished.load(Ordering::Acquire) {
            // Observe completion before rechecking the queue, so the last
            // publication cannot be mistaken for EOF.
            if !self.consumer.is_empty() {
                return self.read(buf);
            }
            self.inner.take_error()?;
            Ok(0)
        } else {
            // Rio treats Ok(0) as terminal EOF, not temporary queue exhaustion.
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

/// Nonblocking, evented writes drained by a synchronous pipe worker.
pub struct EventedAnonWrite {
    thread: Option<JoinHandle<()>>,
    producer: SpscBufferWriter,
    inner: Arc<PipeState>,
}

impl EventedAnonWrite {
    pub fn new(mut pipe: impl Write + Send + 'static) -> Self {
        let (producer, mut consumer) = spsc_buffer(BUFFER_CAPACITY);
        let inner = PipeState::new();
        let state = inner.clone();
        let thread = spawn(move || {
            let result = (|| {
                state.readiness.set_readiness(Ready::writable())?;
                while !state.stop.load(Ordering::Acquire) {
                    if consumer.is_empty() {
                        std::thread::park();
                        continue;
                    }
                    match consumer.write_into(&mut pipe) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(_) => {
                            if !state.readiness.readiness().is_writable() {
                                state.readiness.set_readiness(Ready::writable())?;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            })();
            state.finish(result, Ready::writable());
        });
        Self {
            thread: Some(thread),
            producer,
            inner,
        }
    }
}

impl Write for EventedAnonWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.inner.finished.load(Ordering::Acquire) {
            self.inner.take_error()?;
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let count = self.producer.write_from_slice(buf);
        if self.producer.is_full() {
            self.inner.readiness.set_readiness(Ready::empty())?;
            if !self.producer.is_full() || self.inner.finished.load(Ordering::Acquire) {
                self.inner.readiness.set_readiness(Ready::writable())?;
            }
        }
        if count != 0 {
            self.thread.as_ref().unwrap().thread().unpark();
            Ok(count)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

macro_rules! evented_pipe {
    ($pipe:ty) => {
        impl Evented for $pipe {
            fn register(
                &self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt,
            ) -> io::Result<()> {
                poll.register(&self.inner.registration, token, interest, opts)
            }
            fn reregister(
                &self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt,
            ) -> io::Result<()> {
                poll.reregister(&self.inner.registration, token, interest, opts)
            }
            fn deregister(&self, poll: &Poll) -> io::Result<()> {
                poll.deregister(&self.inner.registration)
            }
        }
        impl Drop for $pipe {
            fn drop(&mut self) {
                self.inner.stop(self.thread.take());
            }
        }
    };
}

evented_pipe!(EventedAnonRead);
evented_pipe!(EventedAnonWrite);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::SyncSender;

    struct Source(SyncSender<usize>);
    impl Read for Source {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            output.fill(1);
            self.0.send(output.len()).unwrap();
            Ok(output.len())
        }
    }

    #[test]
    fn stopped_consumer_bounds_native_reads_to_ring_capacity() {
        use std::sync::mpsc::{sync_channel, RecvTimeoutError};
        use std::time::Duration;
        let (sender, received) = sync_channel(1);
        let mut pipe = EventedAnonRead::new(Source(sender));
        let timeout = Duration::from_secs(2);
        assert_eq!(received.recv_timeout(timeout).unwrap(), BUFFER_CAPACITY);
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(20)),
            Err(RecvTimeoutError::Timeout)
        ));

        let mut bytes = [0; 8192];
        assert_eq!(pipe.read(&mut bytes).unwrap(), bytes.len());
        assert!(bytes.iter().all(|byte| *byte == 1));
        // Only newly freed bytes can be read from the native pipe. No staging
        // buffer may consume another burst while the queue remains full.
        assert_eq!(received.recv_timeout(timeout).unwrap(), bytes.len());
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(20)),
            Err(RecvTimeoutError::Timeout)
        ));
        drop(pipe);
    }

    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    struct Sink {
        started: SyncSender<usize>,
        release: Arc<AtomicBool>,
    }

    impl Write for Sink {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.started.send(input.len()).unwrap();
            while !self.release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            if input.len() == BUFFER_CAPACITY {
                assert!(input.iter().all(|byte| *byte == 1));
            }
            Ok(input.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_native_write_keeps_reserved_bytes_and_applies_backpressure() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;
        let (started, received) = sync_channel(1);
        let release = Arc::new(AtomicBool::new(false));
        let mut pipe = EventedAnonWrite::new(Sink {
            started,
            release: release.clone(),
        });
        // Release a blocked test writer even if an assertion fails, before
        // the pipe's destructor joins it.
        let _release = Release(release.clone());
        assert_eq!(
            pipe.write(&vec![1; BUFFER_CAPACITY]).unwrap(),
            BUFFER_CAPACITY
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            BUFFER_CAPACITY
        );
        for _ in 0..100 {
            assert_eq!(
                pipe.write(b"x").unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        release.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match pipe.write(b"y") {
                Ok(count) => {
                    assert_eq!(count, 1);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
    }
}
