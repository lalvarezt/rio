//! Bounded provider jobs using cloneable platform request handles.
use super::typed::Provider;
use std::sync::mpsc::{self, SyncSender, TrySendError};

type Job = Box<dyn FnOnce(&mut dyn Provider) + Send>;
const QUEUE_CAPACITY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Full,
    Stopped,
}

pub struct Jobs {
    sender: SyncSender<Job>,
}
impl Jobs {
    pub fn new(mut provider: Box<dyn Provider + Send>) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("rio-clipboard-jobs".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job(provider.as_mut());
                }
            })?;
        Ok(Self { sender })
    }
    pub fn submit(
        &self,
        job: impl FnOnce(&mut dyn Provider) + Send + 'static,
    ) -> Result<(), Error> {
        self.sender
            .try_send(Box::new(job))
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::Full,
                TrySendError::Disconnected(_) => Error::Stopped,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{
        kitty::Content,
        typed::{Error as ProviderError, Snapshot},
        ClipboardType,
    };
    use std::time::Duration;
    struct Fake;
    impl Provider for Fake {
        fn read(
            &mut self,
            _: ClipboardType,
            _: &[String],
        ) -> Result<Snapshot, ProviderError> {
            unreachable!()
        }
        fn write(
            &mut self,
            _: ClipboardType,
            _: &[Content],
        ) -> Result<(), ProviderError> {
            unreachable!()
        }
    }
    #[test]
    fn queue_is_bounded_while_provider_is_stalled() {
        let jobs = Jobs::new(Box::new(Fake)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        jobs.submit(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
        .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        for _ in 0..QUEUE_CAPACITY {
            let done = done_tx.clone();
            jobs.submit(move |_| {
                done.send(()).unwrap();
            })
            .unwrap();
        }
        assert_eq!(jobs.submit(|_| {}), Err(Error::Full));
        release_tx.send(()).unwrap();
        for _ in 0..QUEUE_CAPACITY {
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }
    #[test]
    fn job_runs_off_the_calling_thread() {
        let jobs = Jobs::new(Box::new(Fake)).unwrap();
        let caller = std::thread::current().id();
        let (tx, rx) = mpsc::channel();
        jobs.submit(move |_| {
            tx.send(std::thread::current().id()).unwrap();
        })
        .unwrap();
        assert_ne!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), caller);
    }
}
