// Kernel-pipe measurements, independent of the renderer and terminal parser.
#[allow(dead_code)]
#[path = "../src/windows/pipes.rs"]
mod pipes;
#[allow(dead_code)]
#[path = "../src/windows/spsc.rs"]
mod spsc;

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

fn anonymous_pipe() -> (impl Read + Send, impl Write + Send) {
    #[cfg(windows)]
    {
        // Match ConPTY's current hint; use zero for the old system baseline.
        let size = std::env::var("RIO_TRANSPORT_BENCH_PIPE_SIZE")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(65536);
        miow::pipe::anonymous(size).unwrap()
    }
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        let mut descriptors = [-1; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let pair = unsafe {
            (
                std::fs::File::from_raw_fd(descriptors[0]),
                std::fs::File::from_raw_fd(descriptors[1]),
            )
        };
        #[cfg(target_os = "linux")]
        if let Some(size) = std::env::var("RIO_TRANSPORT_BENCH_PIPE_SIZE")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|size| *size > 0)
        {
            use std::os::fd::AsRawFd;
            assert!(
                unsafe { libc::fcntl(pair.0.as_raw_fd(), libc::F_SETPIPE_SZ, size) }
                    >= size,
                "{}",
                io::Error::last_os_error()
            );
        }
        pair
    }
}

fn poll_until(
    poll: &corcovado::Poll,
    events: &mut corcovado::Events,
    progress: &mut Instant,
    moved: &mut bool,
) {
    if *moved {
        *progress = Instant::now();
        *moved = false;
    }
    assert!(
        progress.elapsed() < Duration::from_secs(10),
        "transport stalled"
    );
    poll.poll(events, Some(Duration::from_millis(100))).unwrap();
}

fn output(bytes: usize, chunk: usize, slow: bool, cpu: bool) {
    let (read, mut write) = anonymous_pipe();
    let writer = std::thread::spawn(move || {
        let input = vec![0xa5; chunk];
        for _ in 0..bytes / chunk {
            write.write_all(&input).unwrap();
        }
    });
    let mut reader = pipes::EventedAnonRead::new(read);
    let poll = corcovado::Poll::new().unwrap();
    poll.register(
        &reader,
        corcovado::Token(0),
        corcovado::Ready::readable(),
        corcovado::PollOpt::edge(),
    )
    .unwrap();
    let mut events = corcovado::Events::with_capacity(4);
    let mut buffer = [0; 8192];
    let mut received = 0;
    let mut progress = Instant::now();
    let mut moved = false;
    let mut reads = 0;
    let mut slept = Duration::ZERO;
    let mut digest = 1_u64;
    while received < bytes {
        match reader.read(&mut buffer) {
            Ok(0) => panic!("premature EOF after {received}/{bytes} bytes"),
            Ok(count) => {
                assert_eq!(buffer[count - 1], 0xa5);
                received += count;
                moved = true;
                reads += 1;
                if slow {
                    let start = Instant::now();
                    std::thread::sleep(Duration::from_micros(50));
                    slept += start.elapsed();
                }
                if cpu {
                    // Fixed work per byte keeps parser pressure independent of
                    // read fragmentation and the OS sleep-timer resolution.
                    for byte in &buffer[..count] {
                        for _ in 0..8 {
                            digest =
                                digest.rotate_left(7).wrapping_mul(0x9e37_79b1_85eb_ca87)
                                    ^ u64::from(*byte);
                        }
                    }
                    std::hint::black_box(digest);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                poll_until(&poll, &mut events, &mut progress, &mut moved)
            }
            Err(error) => panic!("{error}"),
        }
    }
    writer.join().unwrap();
    if slow || cpu {
        println!(
            "consumer={} reads={reads} avg_bytes={:.1} sleep_ms={:.3}",
            if slow { "sleep" } else { "cpu" },
            bytes as f64 / reads as f64,
            slept.as_secs_f64() * 1000.0
        );
    }
}

fn input(bytes: usize, chunk: usize) {
    let (mut read, write) = anonymous_pipe();
    let reader = std::thread::spawn(move || {
        let mut received = 0;
        let mut buffer = [0; 8192];
        while received < bytes {
            let count = read.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            assert_eq!(buffer[count - 1], 0xa5);
            received += count;
        }
    });
    let mut writer = pipes::EventedAnonWrite::new(write);
    let poll = corcovado::Poll::new().unwrap();
    poll.register(
        &writer,
        corcovado::Token(0),
        corcovado::Ready::writable(),
        corcovado::PollOpt::edge(),
    )
    .unwrap();
    let mut events = corcovado::Events::with_capacity(4);
    let buffer = vec![0xa5; chunk];
    let mut written = 0;
    let mut progress = Instant::now();
    let mut moved = false;
    while written < bytes {
        match writer.write(&buffer[..chunk.min(bytes - written)]) {
            Ok(0) => panic!("zero-progress input"),
            Ok(count) => {
                written += count;
                moved = true;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                poll_until(&poll, &mut events, &mut progress, &mut moved)
            }
            Err(error) => panic!("{error}"),
        }
    }
    reader.join().unwrap();
}

fn roundtrip() {
    let (read, mut reply) = anonymous_pipe();
    let (mut receive, write) = anonymous_pipe();
    let peer = std::thread::spawn(move || {
        for _ in 0..2000 {
            let mut bytes = [0; 16];
            receive.read_exact(&mut bytes).unwrap();
            reply.write_all(&bytes).unwrap();
        }
    });
    let mut reader = pipes::EventedAnonRead::new(read);
    let mut writer = pipes::EventedAnonWrite::new(write);
    let poll = corcovado::Poll::new().unwrap();
    poll.register(
        &reader,
        corcovado::Token(0),
        corcovado::Ready::readable(),
        corcovado::PollOpt::edge(),
    )
    .unwrap();
    let mut events = corcovado::Events::with_capacity(4);
    let mut samples = Vec::with_capacity(2000);
    for sequence in 0..2000_u64 {
        let mut message = [0; 16];
        message[..8].copy_from_slice(&sequence.to_ne_bytes());
        let start = Instant::now();
        let mut progress = start;
        let mut moved = false;
        assert_eq!(writer.write(&message).unwrap(), message.len());
        let mut received = 0;
        let mut output = [0; 16];
        while received < output.len() {
            match reader.read(&mut output[received..]) {
                Ok(0) => panic!("premature echo EOF"),
                Ok(count) => received += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    poll_until(&poll, &mut events, &mut progress, &mut moved)
                }
                Err(error) => panic!("{error}"),
            }
        }
        samples.push(start.elapsed().as_nanos());
        assert_eq!(message, output);
    }
    peer.join().unwrap();
    samples.sort_unstable();
    println!(
        "native_roundtrip_ns p50={} p95={} p99={}",
        samples[1000], samples[1900], samples[1980]
    );
}

fn main() {
    #[cfg(windows)]
    println!(
        "native_pipe_hint={}",
        std::env::var("RIO_TRANSPORT_BENCH_PIPE_SIZE").unwrap_or_else(|_| "65536".into())
    );
    let mib = std::env::var("RIO_TRANSPORT_BENCH_MIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(16);
    let selected_chunk = std::env::var("RIO_TRANSPORT_BENCH_CHUNK")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    let selected_direction = std::env::var("RIO_TRANSPORT_BENCH_DIRECTION").ok();
    let sessions = std::env::var("RIO_TRANSPORT_BENCH_SESSIONS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);
    assert!(sessions > 0);
    for direction in ["output", "input", "slow-output", "cpu-output"] {
        if selected_direction
            .as_deref()
            .is_some_and(|requested| requested != direction)
        {
            continue;
        }
        for chunk in [64, 4096, 65536] {
            if selected_chunk.is_some_and(|requested| requested != chunk) {
                continue;
            }
            if (direction == "slow-output" || direction == "cpu-output") && chunk != 65536
            {
                continue;
            }
            let bytes = if direction == "slow-output" || direction == "cpu-output" {
                1024 * 1024
            } else {
                mib * 1024 * 1024
            };
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let transfer = || {
                    if direction == "input" {
                        input(bytes, chunk);
                    } else {
                        output(
                            bytes,
                            chunk,
                            direction == "slow-output",
                            direction == "cpu-output",
                        );
                    }
                };
                if sessions == 1 {
                    transfer();
                } else {
                    std::thread::scope(|scope| {
                        let threads: Vec<_> =
                            (0..sessions).map(|_| scope.spawn(transfer)).collect();
                        for thread in threads {
                            thread.join().unwrap();
                        }
                    });
                }
                samples.push(start.elapsed().as_secs_f64());
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "direction={direction} sessions={sessions} chunk={chunk} median_ms={:.3} mib_s={:.2}",
                samples[1] * 1000.0,
                bytes as f64 * sessions as f64 / 1048576.0 / samples[1]
            );
        }
    }
    if selected_direction
        .as_deref()
        .is_none_or(|requested| requested == "roundtrip")
    {
        roundtrip();
    }
}
