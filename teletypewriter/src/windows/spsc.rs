// Bounded byte queue with one producer and one consumer.

use std::cell::UnsafeCell;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// The two threads publish independently; sharing a cache line makes both stores
// contend even though neither cursor needs an atomic read-modify-write.
#[repr(align(128))]
struct Cursor(AtomicUsize);

struct SpscBuffer {
    buf: Box<[UnsafeCell<u8>]>,
    head: Cursor,
    tail: Cursor,
}

// Only the producer accesses unpublished bytes, and only the consumer accesses
// published bytes. Release/Acquire on tail publishes initialized bytes; the same
// ordering on head prevents reuse until the consumer has finished reading them.
unsafe impl Sync for SpscBuffer {}

/// Consumer of the ring buffer. It cannot be cloned.
pub struct SpscBufferReader {
    cursor: usize,
    offset: usize,
    buffer: Arc<SpscBuffer>,
}

impl SpscBufferReader {
    pub fn len(&self) -> usize {
        self.buffer
            .tail
            .0
            .load(Ordering::Acquire)
            .wrapping_sub(self.cursor)
    }

    pub fn capacity(&self) -> usize {
        self.buffer.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read_to_slice(&mut self, output: &mut [u8]) -> usize {
        self.write_into(&mut &mut output[..]).unwrap()
    }

    /// Keep bytes reserved until the synchronous write finishes. Short writes
    /// consume only their prefix, so no staging copy or retry buffer is needed.
    pub fn write_into(&mut self, writer: &mut impl Write) -> io::Result<usize> {
        let size = self.len().min(self.capacity() - self.offset);
        if size == 0 {
            return Ok(0);
        }
        // No mutable reference to the entire allocation is formed: the producer
        // may simultaneously be filling a different, unpublished region.
        let bytes = unsafe {
            std::slice::from_raw_parts(self.buffer.buf[self.offset].get(), size)
        };
        let count = writer.write(bytes)?;
        assert!(count <= size);
        if count != 0 {
            self.offset = (self.offset + count) % self.capacity();
            self.cursor = self.cursor.wrapping_add(count);
            self.buffer.head.0.store(self.cursor, Ordering::Release);
        }
        Ok(count)
    }
}

impl Read for SpscBufferReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.read_to_slice(buf))
    }
}

/// Producer of the ring buffer. It cannot be cloned.
pub struct SpscBufferWriter {
    cursor: usize,
    offset: usize,
    buffer: Arc<SpscBuffer>,
}

impl SpscBufferWriter {
    pub fn len(&self) -> usize {
        self.cursor
            .wrapping_sub(self.buffer.head.0.load(Ordering::Acquire))
    }

    pub fn capacity(&self) -> usize {
        self.buffer.buf.len()
    }

    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    pub fn write_from_slice(&mut self, input: &[u8]) -> usize {
        self.read_from(&mut &input[..]).unwrap()
    }

    /// Read directly into free storage, publishing only after I/O completes.
    pub fn read_from(&mut self, reader: &mut impl Read) -> io::Result<usize> {
        let size = (self.capacity() - self.len()).min(self.capacity() - self.offset);
        if size == 0 {
            return Ok(0);
        }
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(self.buffer.buf[self.offset].get(), size)
        };
        let count = reader.read(bytes)?;
        assert!(count <= size);
        if count != 0 {
            self.offset = (self.offset + count) % self.capacity();
            self.cursor = self.cursor.wrapping_add(count);
            self.buffer.tail.0.store(self.cursor, Ordering::Release);
        }
        Ok(count)
    }
}

impl Write for SpscBufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(self.write_from_slice(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn spsc_buffer(size: usize) -> (SpscBufferWriter, SpscBufferReader) {
    assert!(size > 0 && size <= isize::MAX as usize);
    let buffer = Arc::new(SpscBuffer {
        buf: (0..size).map(|_| UnsafeCell::new(0)).collect(),
        head: Cursor(AtomicUsize::new(0)),
        tail: Cursor(AtomicUsize::new(0)),
    });
    let producer = SpscBufferWriter {
        cursor: 0,
        offset: 0,
        buffer: buffer.clone(),
    };
    let consumer = SpscBufferReader {
        cursor: 0,
        offset: 0,
        buffer,
    };
    (producer, consumer)
}

#[cfg(test)]
mod test {
    #[test]
    fn cursor_overflow_preserves_non_power_of_two_wraparound() {
        use super::*;
        let (mut writer, mut reader) = spsc_buffer(3);
        writer.cursor = usize::MAX - 1;
        reader.cursor = usize::MAX - 1;
        writer.buffer.head.0.store(reader.cursor, Ordering::Relaxed);
        writer.buffer.tail.0.store(writer.cursor, Ordering::Relaxed);
        for _ in 0..4 {
            assert_eq!(writer.write_from_slice(b"abc"), 3);
            assert!(writer.is_full());
            let mut output = [0; 3];
            assert_eq!(reader.read_to_slice(&mut output), 3);
            assert_eq!(&output, b"abc");
            assert!(reader.is_empty());
        }
    }
}
