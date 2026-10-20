#[allow(dead_code)]
#[path = "../src/windows/pipes.rs"]
mod pipes;
#[allow(dead_code)]
#[path = "../src/windows/spsc.rs"]
mod spsc;

use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

struct Source {
    remaining: usize,
    data: Vec<u8>,
    finished: Arc<AtomicBool>,
}

impl Read for Source {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            // Let the consumer drain the last bytes before reporting EOF.
            // This also permits comparison with transports that signal an
            // error before consuming already-buffered output.
            while !self.finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            return Ok(0);
        }
        let count = output.len().min(self.data.len()).min(self.remaining);
        output[..count].copy_from_slice(&self.data[..count]);
        self.remaining -= count;
        Ok(count)
    }
}

fn transfer(pattern: &'static [u8], chunk: usize, bytes: usize) -> usize {
    let finished = Arc::new(AtomicBool::new(false));
    let data = pattern.iter().copied().cycle().take(chunk).collect();
    let mut reader = pipes::EventedAnonRead::new(Source {
        remaining: bytes,
        data,
        finished: finished.clone(),
    });
    let poll = corcovado::Poll::new().unwrap();
    poll.register(
        &reader,
        corcovado::Token(0),
        corcovado::Ready::readable(),
        corcovado::PollOpt::edge(),
    )
    .unwrap();
    let mut events = corcovado::Events::with_capacity(4);
    let mut output = [0; 8192];
    let mut received = 0;
    let mut wakeups = 0;
    let mut last_progress = Instant::now();
    while received < bytes {
        match reader.read(&mut output) {
            Ok(0) => {}
            Ok(count) => {
                std::hint::black_box(&output[..count]);
                received += count;
                last_progress = Instant::now();
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{error}"),
        }
        poll.poll(&mut events, Some(Duration::from_millis(100)))
            .unwrap();
        // Poll may return early with no user events after registration changes.
        assert!(
            last_progress.elapsed() < Duration::from_secs(5),
            "pipe worker stalled after {received}/{bytes} bytes"
        );
        wakeups += 1;
    }
    finished.store(true, Ordering::Release);
    drop(reader);
    wakeups
}

fn main() {
    let bytes = 64 * 1024 * 1024;
    for (name, pattern) in [
        ("ascii", b"hello world\n".as_slice()),
        ("utf8", "λ日本語🚀\n".as_bytes()),
        ("ansi", b"\x1b[1;32mhello\x1b[0m\r\n".as_slice()),
    ] {
        for sessions in [1, 4] {
            for chunk in [64, 4096, 65536] {
                let mut samples = Vec::new();
                for _ in 0..3 {
                    let start = Instant::now();
                    let threads: Vec<_> = (0..sessions)
                        .map(|_| {
                            std::thread::spawn(move || transfer(pattern, chunk, bytes))
                        })
                        .collect();
                    let wakeups: usize = threads
                        .into_iter()
                        .map(|thread| thread.join().unwrap())
                        .sum();
                    samples.push((start.elapsed().as_secs_f64(), wakeups));
                }
                samples.sort_by(|a, b| a.0.total_cmp(&b.0));
                println!("pattern={name} sessions={sessions} chunk={chunk} median_ms={:.3} mib_s={:.2} poll_wakeups={}", samples[1].0 * 1000.0, 64.0 * sessions as f64 / samples[1].0, samples[1].1);
            }
        }
    }
}
