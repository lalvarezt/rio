#[allow(dead_code)]
#[path = "../src/windows/spsc.rs"]
mod spsc;

use std::hint::black_box;
use std::time::Instant;

fn cpu_seconds() -> f64 {
    #[cfg(unix)]
    {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        assert_eq!(
            unsafe {
                libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, time.as_mut_ptr())
            },
            0
        );
        let time = unsafe { time.assume_init() };
        time.tv_sec as f64 + time.tv_nsec as f64 / 1e9
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [creation, exit, kernel, user] = &mut times;
        assert_ne!(
            unsafe { GetProcessTimes(GetCurrentProcess(), creation, exit, kernel, user) },
            0
        );
        let ticks = |time: &FILETIME| {
            ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
        };
        (ticks(&times[2]) + ticks(&times[3])) as f64 / 1e7
    }
}

fn latency() {
    let (mut request, mut receive) = spsc::spsc_buffer(65536);
    let (mut reply, mut response) = spsc::spsc_buffer(65536);
    let peer = std::thread::spawn(move || {
        for _ in 0..10000 {
            let mut data = [0; 16];
            while receive.read_to_slice(&mut data) == 0 {
                std::hint::spin_loop();
            }
            while reply.write_from_slice(&data) == 0 {
                std::hint::spin_loop();
            }
        }
    });
    let mut samples = Vec::with_capacity(10000);
    for sequence in 0..10000_u64 {
        let mut data = [0; 16];
        data[..8].copy_from_slice(&sequence.to_ne_bytes());
        let start = Instant::now();
        assert_eq!(request.write_from_slice(&data), data.len());
        let mut output = [0; 16];
        while response.read_to_slice(&mut output) == 0 {
            std::hint::spin_loop();
        }
        samples.push(start.elapsed().as_nanos());
        assert_eq!(output, data);
    }
    peer.join().unwrap();
    samples.sort_unstable();
    println!(
        "queue_roundtrip_ns p50={} p95={} p99={}",
        samples[5000], samples[9500], samples[9900]
    );
}

fn transfer(capacity: usize, chunk: usize, bytes: usize) {
    let (mut writer, mut reader) = spsc::spsc_buffer(capacity);
    let producer = std::thread::spawn(move || {
        let data = vec![0xa5; chunk];
        let mut written = 0;
        while written < bytes {
            let count = writer.write_from_slice(&data[..chunk.min(bytes - written)]);
            written += count;
            if count == 0 {
                std::hint::spin_loop();
            }
        }
    });
    let mut output = vec![0; chunk];
    let mut read = 0;
    while read < bytes {
        let count = reader.read_to_slice(&mut output);
        if count > 0 {
            assert_eq!(black_box(output[count - 1]), 0xa5);
        } else {
            std::hint::spin_loop();
        }
        read += count;
    }
    producer.join().unwrap();
}

fn main() {
    let mib = std::env::var("RIO_TRANSPORT_BENCH_MIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(64);
    let bytes = mib * 1024 * 1024;
    let capacity_filter = std::env::var("RIO_TRANSPORT_BENCH_CAPACITY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    for capacity in [16 * 1024, 64 * 1024, 128 * 1024, 256 * 1024] {
        if capacity_filter.is_some_and(|requested| requested != capacity) {
            continue;
        }
        for chunk in [16, 256, 4096, 65536] {
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let cpu = cpu_seconds();
                transfer(capacity, chunk, bytes);
                samples.push((start.elapsed().as_secs_f64(), cpu_seconds() - cpu));
            }
            samples.sort_by(|a, b| a.0.total_cmp(&b.0));
            println!(
                "capacity={capacity} chunk={chunk} median_ms={:.3} mib_s={:.2} cpu_us_mib={:.2}",
                samples[1].0 * 1000.0,
                bytes as f64 / 1048576.0 / samples[1].0,
                samples[1].1 * 1e6 / mib as f64
            );
        }
    }
    latency();
}
