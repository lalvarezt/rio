//! Response memory reservations survive event and PTY queue ownership changes.
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};

const RESPONSE_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Clone)]
pub struct Budget {
    used: Arc<AtomicUsize>,
    limit: usize,
}

impl Budget {
    pub fn new(limit: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }
    pub fn global() -> Self {
        static GLOBAL: OnceLock<Budget> = OnceLock::new();
        GLOBAL.get_or_init(|| Self::new(RESPONSE_LIMIT)).clone()
    }
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
    fn acquire(&self, bytes: usize) -> io::Result<()> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= self.limit)
            })
            .map(|_| ())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "clipboard response memory budget exhausted",
                )
            })
    }
}

struct Reservation {
    budget: Budget,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

pub struct ResponseBuffer {
    bytes: Vec<u8>,
    reservation: Reservation,
}
impl std::fmt::Debug for ResponseBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardResponse")
            .field("len", &self.bytes.len())
            .finish()
    }
}
impl ResponseBuffer {
    pub fn new(budget: Budget) -> Self {
        Self {
            bytes: Vec::new(),
            reservation: Reservation { budget, bytes: 0 },
        }
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    pub fn reserve(&mut self, additional: usize) -> io::Result<()> {
        let needed = self.bytes.len().checked_add(additional).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::FileTooLarge,
                "clipboard response length overflow",
            )
        })?;
        if needed > self.bytes.capacity() {
            let target = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .max(4096)
                .min(self.reservation.budget.limit);
            if target < needed {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "clipboard response exceeds byte limit",
                ));
            }
            let available = self
                .reservation
                .budget
                .limit
                .saturating_sub(self.reservation.budget.used());
            let target = target.min(self.bytes.capacity().saturating_add(available));
            if target < needed {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "clipboard response memory budget exhausted",
                ));
            }
            let growth = target - self.bytes.capacity();
            self.reservation.budget.acquire(growth)?;
            self.reservation.bytes += growth;
            self.bytes
                .try_reserve_exact(target - self.bytes.len())
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}
impl Write for ResponseBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.reserve(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_encoders_cannot_overcommit_the_shared_budget() {
        let budget = Budget::new(4096);
        let ready = Arc::new(std::sync::Barrier::new(9));
        let release = Arc::new(std::sync::Barrier::new(9));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                let ready = ready.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    let mut response = ResponseBuffer::new(budget);
                    let admitted = response.write_all(b"data").is_ok();
                    ready.wait();
                    release.wait();
                    admitted
                })
            })
            .collect();
        ready.wait();
        assert_eq!(budget.used(), 4096);
        release.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|admitted| *admitted)
                .count(),
            1
        );
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn shared_budget_rejects_overcommit_and_releases_failed_encodings() {
        let budget = Budget::new(4096);
        let mut first = ResponseBuffer::new(budget.clone());
        first.write_all(b"one").unwrap();
        let mut second = ResponseBuffer::new(budget.clone());
        assert_eq!(
            second.write_all(b"two").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(second);
        assert_eq!(budget.used(), 4096);
        drop(first);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn reservation_stays_with_bytes_across_shared_ownership() {
        let budget = Budget::new(4096);
        let mut response = ResponseBuffer::new(budget.clone());
        response.write_all(b"response").unwrap();
        let first = Arc::new(response);
        let queued = first.clone();
        drop(first);
        assert_eq!(queued.as_slice(), b"response");
        assert_eq!(budget.used(), 4096);
        drop(queued);
        assert_eq!(budget.used(), 0);
    }
}
