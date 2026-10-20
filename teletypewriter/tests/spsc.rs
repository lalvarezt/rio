// Exercise the actual Windows queue on Unix too, without a Windows build.
#[allow(dead_code)]
#[path = "../src/windows/spsc.rs"]
mod spsc;

#[test]
fn short_io_and_errors_publish_only_completed_bytes() {
    use std::io::{self, Read, Write};
    struct Failed;
    impl Read for Failed {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            output[0] = 42;
            Err(io::ErrorKind::Interrupted.into())
        }
    }
    impl Write for Failed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Interrupted.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (mut writer, mut reader) = spsc::spsc_buffer(3);
    assert!(writer.read_from(&mut Failed).is_err());
    assert!(reader.is_empty());
    assert_eq!(writer.read_from(&mut &b"ab"[..]).unwrap(), 2);
    assert!(reader.write_into(&mut Failed).is_err());
    assert_eq!(reader.len(), 2);
    let mut first = [0];
    assert_eq!(reader.write_into(&mut &mut first[..]).unwrap(), 1);
    assert_eq!(first, [b'a']);
    assert_eq!(reader.len(), 1);
    let mut second = [0; 3];
    assert_eq!(reader.read_to_slice(&mut second), 1);
    assert_eq!(second[0], b'b');
}

#[test]
fn randomized_operations_match_a_bounded_fifo() {
    use std::collections::VecDeque;
    for capacity in [1, 3, 60, 65536] {
        let (mut writer, mut reader) = spsc::spsc_buffer(capacity);
        let mut reference = VecDeque::new();
        let mut seed = 123456789_u64;
        for _ in 0..10000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let length = (seed as usize >> 2) % 257;
            if seed & 1 == 0 {
                let input = vec![seed as u8; length];
                let count = writer.write_from_slice(&input);
                assert!(count <= length.min(capacity - reference.len()));
                reference.extend(&input[..count]);
            } else {
                let mut output = vec![0; length];
                let count = reader.read_to_slice(&mut output);
                assert!(count <= length.min(reference.len()));
                for byte in &output[..count] {
                    assert_eq!(Some(*byte), reference.pop_front());
                }
            }
            assert_eq!(reader.len(), reference.len());
            assert_eq!(writer.len(), reference.len());
        }
    }
}

#[test]
fn concurrent_wraparound_preserves_bytes() {
    for capacity in [1, 3, 60, 65536] {
        let (mut writer, mut reader) = spsc::spsc_buffer(capacity);
        let producer = std::thread::spawn(move || {
            let data: Vec<u8> = (0..257).map(|i| i as u8).collect();
            for _ in 0..1000 {
                let mut offset = 0;
                while offset < data.len() {
                    let count = writer.write_from_slice(&data[offset..]);
                    offset += count;
                    if count == 0 {
                        std::thread::yield_now();
                    }
                }
            }
        });
        let mut buffer = [0; 113];
        let mut received = 0;
        while received < 257_000 {
            let count = reader.read_to_slice(&mut buffer);
            for (i, byte) in buffer[..count].iter().enumerate() {
                assert_eq!(*byte, ((received + i) % 257) as u8);
            }
            received += count;
            if count == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert!(reader.is_empty());
    }
}
