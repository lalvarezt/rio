// Compile the same workers on Linux; only synchronous-I/O cancellation is native.
#[path = "../src/windows/pipes.rs"]
mod pipes;
#[allow(dead_code)]
#[path = "../src/windows/spsc.rs"]
mod spsc;

use pipes::{EventedAnonRead, EventedAnonWrite};
use std::io::{self, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "worker failed to make progress");
        std::thread::yield_now();
    }
}

struct EndOfFile(Arc<AtomicUsize>);
impl Read for EndOfFile {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(0)
    }
}

#[test]
fn eof_is_not_retried_and_notifies_poll() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pipe = EventedAnonRead::new(EndOfFile(calls.clone()));
    let poll = corcovado::Poll::new().unwrap();
    poll.register(
        &pipe,
        corcovado::Token(0),
        corcovado::Ready::readable(),
        corcovado::PollOpt::edge(),
    )
    .unwrap();
    let mut events = corcovado::Events::with_capacity(4);
    until(|| {
        poll.poll(&mut events, Some(Duration::from_millis(100)))
            .unwrap();
        events
            .iter()
            .any(|event| event.token() == corcovado::Token(0))
    });
    assert_eq!(pipe.read(&mut [0; 16]).unwrap(), 0);
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

struct ZeroWriter(Arc<AtomicUsize>);
impl Write for ZeroWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(0)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn zero_write_is_an_error_and_drop_is_safe() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pipe = EventedAnonWrite::new(ZeroWriter(calls.clone()));
    assert_eq!(pipe.write(b"x").unwrap(), 1);
    until(|| calls.load(Ordering::Relaxed) != 0);
    let mut failure = None;
    until(|| match pipe.write(b"x") {
        Err(error) => {
            failure = Some(error.kind());
            true
        }
        Ok(_) => false,
    });
    assert_eq!(failure, Some(io::ErrorKind::WriteZero));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    drop(pipe);
}

#[test]
fn full_queue_wakes_and_drains_before_eof() {
    // Repeatedly cross the full/empty boundaries and wrap with short reads.
    let expected: Vec<u8> = (0..2_000_000).map(|n| (n % 251) as u8).collect();
    let mut pipe = EventedAnonRead::new(io::Cursor::new(expected.clone()));
    let mut actual = Vec::with_capacity(expected.len());
    let mut buffer = [0; 137];
    until(|| match pipe.read(&mut buffer) {
        Ok(0) => true,
        Ok(count) => {
            actual.extend_from_slice(&buffer[..count]);
            false
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("{error}"),
    });
    assert_eq!(actual, expected);
}

struct ShortWriter(Arc<Mutex<Vec<u8>>>);
impl Write for ShortWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let size = input.len().min(3);
        self.0.lock().unwrap().extend_from_slice(&input[..size]);
        Ok(size)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn short_writes_and_repeated_empty_wakeups_preserve_bytes() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut pipe = EventedAnonWrite::new(ShortWriter(output.clone()));
    let expected: Vec<u8> = (0..200_000).map(|n| (n % 251) as u8).collect();
    let mut offset = 0;
    until(|| {
        if offset < expected.len() {
            match pipe.write(&expected[offset..(offset + 113).min(expected.len())]) {
                Ok(count) => offset += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
        }
        output.lock().unwrap().len() == expected.len()
    });
    assert_eq!(*output.lock().unwrap(), expected);
}

#[test]
fn drop_wakes_workers_parked_on_full_and_empty_queues() {
    let read = EventedAnonRead::new(io::repeat(1));
    let write = EventedAnonWrite::new(io::sink());
    std::thread::sleep(Duration::from_millis(10));
    drop(read);
    drop(write);
}

#[test]
fn buffered_output_is_delivered_before_a_worker_error() {
    struct FailsAfterPrefix(Arc<AtomicUsize>);
    impl Read for FailsAfterPrefix {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                output[..3].copy_from_slice(b"abc");
                Ok(3)
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pipe = EventedAnonRead::new(FailsAfterPrefix(calls.clone()));
    until(|| calls.load(Ordering::Relaxed) == 2);
    let mut output = [0; 16];
    assert_eq!(pipe.read(&mut output).unwrap(), 3);
    assert_eq!(&output[..3], b"abc");
    let mut failure = None;
    until(|| match pipe.read(&mut output) {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => {
            failure = Some(error.kind());
            true
        }
        other => panic!("expected worker failure, got {other:?}"),
    });
    assert_eq!(failure, Some(io::ErrorKind::BrokenPipe));
    drop(pipe);
}

#[test]
fn empty_active_pipe_is_not_eof() {
    struct Waiting(Arc<AtomicBool>);
    impl Read for Waiting {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            while !self.0.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Ok(0)
        }
    }
    let release = Arc::new(AtomicBool::new(false));
    let mut pipe = EventedAnonRead::new(Waiting(release.clone()));
    let result = pipe.read(&mut [0; 1]);
    release.store(true, Ordering::Release);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

#[cfg(windows)]
#[test]
fn drop_cancels_blocking_native_reads_and_writes() {
    let (finished, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (read, _write) = miow::pipe::anonymous(4096).unwrap();
        let pipe = EventedAnonRead::new(read);
        std::thread::sleep(Duration::from_millis(20));
        drop(pipe);

        let (_read, write) = miow::pipe::anonymous(4096).unwrap();
        let mut pipe = EventedAnonWrite::new(write);
        // The peer remains open but never reads, so the worker blocks in WriteFile.
        for _ in 0..100 {
            let _ = pipe.write(&[1; 65536]);
        }
        std::thread::sleep(Duration::from_millis(20));
        drop(pipe);
        // Race shutdown against worker startup and entry into synchronous I/O.
        for _ in 0..100 {
            let (read, _write) = miow::pipe::anonymous(4096).unwrap();
            drop(EventedAnonRead::new(read));
            let (_read, write) = miow::pipe::anonymous(4096).unwrap();
            let mut pipe = EventedAnonWrite::new(write);
            let _ = pipe.write(&[1; 65536]);
            drop(pipe);
        }
        finished.send(()).unwrap();
    });
    result.recv_timeout(Duration::from_secs(10)).unwrap();
}
