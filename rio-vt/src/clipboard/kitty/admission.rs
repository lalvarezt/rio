use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};

#[derive(Clone)]
pub struct Pool {
    inner: Arc<Counts>,
    slots: usize,
    bytes: usize,
}
struct Counts {
    slots: AtomicUsize,
    bytes: AtomicUsize,
}

impl Default for Pool {
    fn default() -> Self {
        static GLOBAL: OnceLock<Pool> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Self::new(64, 64 * 1024 * 1024))
            .clone()
    }
}
impl Pool {
    pub fn new(slots: usize, bytes: usize) -> Self {
        Self {
            inner: Arc::new(Counts {
                slots: AtomicUsize::new(0),
                bytes: AtomicUsize::new(0),
            }),
            slots,
            bytes,
        }
    }
    pub fn admit(&self) -> Result<Admission, super::WriteError> {
        self.inner
            .slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |slots| {
                slots.checked_add(1).filter(|total| *total <= self.slots)
            })
            .map_err(|_| super::WriteError::Busy)?;
        Ok(Admission {
            pool: self.clone(),
            bytes: 0,
        })
    }
}

/// Unique ownership of a request slot and its retained write capacity.
/// Event/permission queues keep this inside the shared request object.
pub struct Admission {
    pool: Pool,
    bytes: usize,
}
impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardAdmission")
            .field("bytes", &self.bytes)
            .finish()
    }
}
impl Admission {
    pub fn available(&self) -> usize {
        self.pool
            .bytes
            .saturating_sub(self.pool.inner.bytes.load(Ordering::Acquire))
    }
    pub fn reserve(&mut self, bytes: usize) -> Result<(), super::WriteError> {
        self.pool
            .inner
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= self.pool.bytes)
            })
            .map_err(|_| super::WriteError::Busy)?;
        self.bytes += bytes;
        Ok(())
    }
    pub fn release(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
        self.pool.inner.bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        self.pool
            .inner
            .bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
        self.pool.inner.slots.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_admission_preserves_both_limits() {
        let pool = Pool::new(4, 8);
        let ready = Arc::new(std::sync::Barrier::new(9));
        let release = Arc::new(std::sync::Barrier::new(9));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let pool = pool.clone();
                let ready = ready.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    let mut admission = pool.admit();
                    let reserved = admission
                        .as_mut()
                        .is_ok_and(|admission| admission.reserve(4).is_ok());
                    ready.wait();
                    release.wait();
                    reserved
                })
            })
            .collect();
        ready.wait();
        assert_eq!(pool.inner.slots.load(Ordering::Acquire), 4);
        assert_eq!(pool.inner.bytes.load(Ordering::Acquire), 8);
        release.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|reserved| *reserved)
                .count(),
            2
        );
        assert_eq!(pool.inner.slots.load(Ordering::Acquire), 0);
        assert_eq!(pool.inner.bytes.load(Ordering::Acquire), 0);
    }
    #[test]
    fn slots_and_bytes_are_shared_and_released_independently() {
        let pool = Pool::new(2, 10);
        let mut first = pool.admit().unwrap();
        first.reserve(8).unwrap();
        let mut second = pool.admit().unwrap();
        assert!(pool.admit().is_err());
        assert!(second.reserve(3).is_err());
        first.release(2);
        second.reserve(3).unwrap();
        drop(first);
        second.reserve(7).unwrap();
        drop(second);
        let mut recovered = pool.admit().unwrap();
        recovered.reserve(10).unwrap();
    }
}
