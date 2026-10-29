use std::io::{Error, ErrorKind, Result};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Usage {
    commands: usize,
    bytes: usize,
    closed: bool,
}

#[derive(Clone)]
pub(crate) struct Budget {
    usage: Arc<Mutex<Usage>>,
    commands: usize,
    bytes: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(16, 128 * 1024 * 1024)
    }
}

impl Budget {
    pub(crate) fn new(commands: usize, bytes: usize) -> Self {
        Self {
            usage: Arc::default(),
            commands,
            bytes,
        }
    }

    pub(crate) fn reserve(&self, bytes: usize) -> Result<Admission> {
        let mut usage = self.usage.lock().unwrap();
        if usage.closed {
            return Err(Error::new(ErrorKind::BrokenPipe, "clipboard owner stopped"));
        }
        if usage.commands >= self.commands
            || bytes > self.bytes.saturating_sub(usage.bytes)
        {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "clipboard command budget exhausted",
            ));
        }
        usage.commands += 1;
        usage.bytes += bytes;
        Ok(Admission {
            budget: self.clone(),
            bytes,
        })
    }

    pub(crate) fn close(&self) {
        self.usage.lock().unwrap().closed = true;
    }
}

pub(crate) struct Admission {
    budget: Budget,
    bytes: usize,
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut usage = self.budget.usage.lock().unwrap();
        usage.commands -= 1;
        usage.bytes -= self.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_rejects_cloned_handles_without_revoking_existing_credits() {
        let budget = Budget::new(1, 8);
        let cloned = budget.clone();
        let credit = budget.reserve(8).unwrap();
        budget.close();
        assert_eq!(
            cloned.reserve(0).err().unwrap().kind(),
            ErrorKind::BrokenPipe
        );
        assert_eq!(budget.usage.lock().unwrap().bytes, 8);
        drop(credit);
        assert_eq!(budget.usage.lock().unwrap().bytes, 0);
        assert_eq!(budget.usage.lock().unwrap().commands, 0);
        assert_eq!(
            cloned.reserve(8).err().unwrap().kind(),
            ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn slots_and_bytes_are_released_independently() {
        let budget = Budget::new(2, 8);
        let full = budget.reserve(8).unwrap();
        let empty = budget.reserve(0).unwrap();
        assert_eq!(
            budget.reserve(0).err().unwrap().kind(),
            ErrorKind::WouldBlock
        );
        drop(empty);
        assert_eq!(
            budget.reserve(1).err().unwrap().kind(),
            ErrorKind::WouldBlock
        );
        drop(full);
        budget.reserve(8).unwrap();
    }

    #[test]
    fn concurrent_handles_cannot_oversubscribe_the_budget() {
        let budget = Budget::new(4, 8);
        let ready = Arc::new(std::sync::Barrier::new(9));
        let release = Arc::new(std::sync::Barrier::new(9));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                let ready = ready.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    let admission = budget.reserve(4).ok();
                    ready.wait();
                    release.wait();
                    admission.is_some()
                })
            })
            .collect();
        ready.wait();
        assert_eq!(budget.usage.lock().unwrap().bytes, 8);
        release.wait();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|admitted| *admitted)
                .count(),
            2
        );
        budget.reserve(8).unwrap();
    }
}
