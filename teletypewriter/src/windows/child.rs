use corcovado::channel::{channel, Receiver, Sender};
use std::ffi::c_void;
use std::io::Error;
use std::num::NonZeroU32;
use std::os::windows::io::{AsRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{BOOLEAN, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, RegisterWaitForSingleObject, UnregisterWaitEx,
    INFINITE, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
};

use crate::ChildEvent;

struct ChildExitSender {
    sender: Sender<ChildEvent>,
    child: OwnedHandle,
}

/// The watcher owns this context until synchronous unregistration completes.
extern "system" fn child_exit_callback(ctx: *mut c_void, timed_out: BOOLEAN) {
    if timed_out != 0 {
        return;
    }
    let event_tx = unsafe { &*ctx.cast::<ChildExitSender>() };
    let mut exit_code = 0_u32;
    let status =
        unsafe { GetExitCodeProcess(event_tx.child.as_raw_handle(), &mut exit_code) };
    let _ = event_tx.sender.send(ChildEvent::Exited(
        (status != 0).then_some(exit_code as i32),
    ));
}

pub struct ChildExitWatcher {
    wait_handle: HANDLE,
    event_rx: Receiver<ChildEvent>,
    context: Option<Box<ChildExitSender>>,
    pid: Option<NonZeroU32>,
}

// The wait handle can be used from any thread; callback context has a stable
// allocation and is released only after UnregisterWaitEx joins the callback.
unsafe impl Send for ChildExitWatcher {}

impl ChildExitWatcher {
    pub fn new(child: OwnedHandle) -> Result<Self, Error> {
        let (event_tx, event_rx) = channel::<ChildEvent>();
        let pid = unsafe { NonZeroU32::new(GetProcessId(child.as_raw_handle())) };
        let mut context = Box::new(ChildExitSender {
            sender: event_tx,
            child,
        });
        let mut wait_handle = std::ptr::null_mut();
        let success = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                context.child.as_raw_handle(),
                Some(child_exit_callback),
                (&mut *context as *mut ChildExitSender).cast(),
                INFINITE,
                WT_EXECUTEINWAITTHREAD | WT_EXECUTEONLYONCE,
            )
        };
        if success == 0 {
            return Err(Error::last_os_error());
        }
        Ok(Self {
            wait_handle,
            event_rx,
            context: Some(context),
            pid,
        })
    }

    pub fn event_rx(&self) -> &Receiver<ChildEvent> {
        &self.event_rx
    }

    pub fn raw_handle(&self) -> HANDLE {
        self.context.as_ref().unwrap().child.as_raw_handle()
    }

    pub fn pid(&self) -> Option<NonZeroU32> {
        self.pid
    }
}

impl Drop for ChildExitWatcher {
    fn drop(&mut self) {
        // A pending callback borrows both sender and process handle. An
        // asynchronous unregister would free them while it is still running.
        if unsafe { UnregisterWaitEx(self.wait_handle, INVALID_HANDLE_VALUE) } == 0 {
            tracing::error!(
                "Could not unregister child wait: {}",
                Error::last_os_error()
            );
            // If Windows cannot establish completion, retaining the context is
            // safer than allowing a callback to use freed storage.
            Box::leak(self.context.take().unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corcovado::{event::Events, Poll, PollOpt, Ready, Token};
    use std::os::windows::io::AsHandle;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn event_is_emitted_when_child_exits() {
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/c", "exit 7"])
            .spawn()
            .unwrap();
        let watcher =
            ChildExitWatcher::new(child.as_handle().try_clone_to_owned().unwrap())
                .unwrap();
        let mut events = Events::with_capacity(1);
        let poll = Poll::new().unwrap();
        poll.register(
            watcher.event_rx(),
            Token(0),
            Ready::readable(),
            PollOpt::oneshot(),
        )
        .unwrap();
        poll.poll(&mut events, Some(Duration::from_secs(2)))
            .unwrap();
        assert_eq!(events.iter().next().unwrap().token(), Token(0));
        assert_eq!(
            watcher.event_rx().try_recv().unwrap(),
            ChildEvent::Exited(Some(7))
        );
        child.wait().unwrap();
    }

    #[test]
    fn unregister_before_exit_releases_callback_context() {
        for _ in 0..100 {
            let mut child = Command::new("cmd.exe")
                .args(["/d", "/c", "pause"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let watcher =
                ChildExitWatcher::new(child.as_handle().try_clone_to_owned().unwrap())
                    .unwrap();
            drop(watcher);
            let _ = child.kill();
            child.wait().unwrap();
        }
    }
}
