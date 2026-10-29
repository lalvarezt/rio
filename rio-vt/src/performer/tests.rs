use super::*;
use crate::ansi::CursorShape;
use crate::crosswords::pos::{Column, Line};
use crate::crosswords::CrosswordsSize;
use crate::event::VoidListener;
use std::sync::mpsc;
use std::time::Duration;
use teletypewriter::{ChildEvent, EventedPty, ProcessReadWrite, WinsizeBuilder};

struct TestPty {
    bytes: io::Cursor<Vec<u8>>,
    ending: Option<ErrorKind>,
    exhausted: Option<mpsc::Sender<()>>,
    writer: TestWriter,
    child: Option<corcovado::Registration>,
    registration_error: bool,
    read_event: bool,
    shutdown_calls: usize,
    deregister_calls: usize,
    raw_error: Option<i32>,
    idle_reads: usize,
    read_calls: usize,
    first_read_delay: Option<Duration>,
    burst_boundary: Option<usize>,
}

#[derive(Default)]
struct TestWriter {
    bytes: Vec<u8>,
    remaining: Option<usize>,
}
impl Write for TestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let len = self.remaining.unwrap_or(bytes.len()).min(bytes.len());
        if len == 0 {
            return Err(ErrorKind::WouldBlock.into());
        }
        self.bytes.extend_from_slice(&bytes[..len]);
        if let Some(remaining) = &mut self.remaining {
            *remaining -= len;
        }
        Ok(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for TestPty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_calls += 1;
        if let Some(delay) = self.first_read_delay.take() {
            std::thread::sleep(delay);
        }
        if self.idle_reads != 0 {
            self.idle_reads -= 1;
            return Err(ErrorKind::WouldBlock.into());
        }
        let limit = if let Some(boundary) = self.burst_boundary {
            let remaining = boundary - self.bytes.position() as usize;
            if remaining == 0 {
                self.burst_boundary = None;
                return Err(ErrorKind::WouldBlock.into());
            }
            remaining.min(buf.len())
        } else {
            buf.len()
        };
        let got = self.bytes.read(&mut buf[..limit])?;
        if got != 0 {
            return Ok(got);
        }
        if let Some(sender) = self.exhausted.take() {
            sender.send(()).unwrap();
        }
        if let Some(code) = self.raw_error {
            return Err(io::Error::from_raw_os_error(code));
        }
        match self.ending {
            Some(kind) => Err(io::Error::from(kind)),
            None => Ok(0),
        }
    }
}

impl ProcessReadWrite for TestPty {
    type Reader = Self;
    type Writer = TestWriter;
    fn reader(&mut self) -> &mut Self {
        self
    }
    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }
    fn read_token(&self) -> corcovado::Token {
        1.into()
    }
    fn write_token(&self) -> corcovado::Token {
        1.into()
    }
    fn set_winsize(&mut self, _: WinsizeBuilder) -> io::Result<()> {
        Ok(())
    }
    fn register(
        &mut self,
        poll: &corcovado::Poll,
        _: &mut dyn Iterator<Item = corcovado::Token>,
        _: Ready,
        _: PollOpt,
    ) -> io::Result<()> {
        if self.registration_error {
            return Err(io::Error::from(ErrorKind::Other));
        }
        if let Some(child) = &self.child {
            #[cfg(unix)]
            let interest = Ready::readable() | Ready::from(UnixReady::hup());
            #[cfg(not(unix))]
            let interest = Ready::readable();
            poll.register(
                child,
                if self.read_event {
                    self.read_token()
                } else {
                    self.child_event_token()
                },
                interest,
                PollOpt::edge(),
            )?;
        }
        Ok(())
    }
    fn reregister(
        &mut self,
        _: &corcovado::Poll,
        _: Ready,
        _: PollOpt,
    ) -> io::Result<()> {
        Ok(())
    }
    fn deregister(&mut self, _: &corcovado::Poll) -> io::Result<()> {
        self.deregister_calls += 1;
        Ok(())
    }
}

impl EventedPty for TestPty {
    fn shutdown(&mut self) -> io::Result<()> {
        self.shutdown_calls += 1;
        Ok(())
    }
    fn child_event_token(&self) -> corcovado::Token {
        2.into()
    }
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        Some(ChildEvent::Exited(Some(0)))
    }
}

fn machine(bytes: Vec<u8>, ending: Option<ErrorKind>) -> Machine<TestPty, VoidListener> {
    let terminal = Crosswords::new(
        CrosswordsSize::new(80, 24),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        0,
    );
    Machine::new(
        Arc::new(FairMutex::new(terminal)),
        TestPty {
            bytes: io::Cursor::new(bytes),
            ending,
            exhausted: None,
            writer: TestWriter::default(),
            child: None,
            registration_error: false,
            read_event: false,
            shutdown_calls: 0,
            deregister_calls: 0,
            raw_error: None,
            idle_reads: 0,
            read_calls: 0,
            first_read_delay: None,
            burst_boundary: None,
        },
        VoidListener,
        WindowId::from(0),
        0,
    )
    .unwrap()
}

fn first_line(machine: &Machine<TestPty, VoidListener>, len: usize) -> String {
    let terminal = machine.terminal.lock();
    (0..len)
        .map(|col| terminal.grid[Line(0)][Column(col)].c())
        .collect()
}

#[test]
fn clipboard_credit_survives_channel_queue_and_partial_writes() {
    use crate::clipboard::buffer::{Budget, ResponseBuffer};
    let budget = Budget::new(4096);
    let mut response = ResponseBuffer::new(budget.clone());
    response.write_all(b"response").unwrap();
    let mut machine = machine(Vec::new(), None);
    machine
        .channel()
        .send(Msg::Input(Cow::Borrowed(b"")))
        .unwrap();
    machine
        .channel()
        .send(Msg::ClipboardInput(Arc::new(ResponseBuffer::new(
            budget.clone(),
        ))))
        .unwrap();
    machine
        .channel()
        .send(Msg::ClipboardInput(Arc::new(response)))
        .unwrap();
    assert_eq!(budget.used(), 4096);
    let mut state = State::default();
    assert!(machine.drain_recv_channel(&mut state));
    machine.pty.writer.remaining = Some(3);
    machine.pty_write(&mut state).unwrap();
    assert_eq!(machine.pty.writer.bytes, b"res");
    assert_eq!(budget.used(), 4096);
    machine.pty.writer.remaining = None;
    machine.pty_write(&mut state).unwrap();
    assert_eq!(machine.pty.writer.bytes, b"response");
    assert_eq!(budget.used(), 0);
}

#[test]
fn shutdown_releases_current_and_undrained_response_credits() {
    use crate::clipboard::buffer::{Budget, ResponseBuffer};
    let budget = Budget::new(8192);
    let mut machine = machine(Vec::new(), None);
    let sender = machine.channel();
    let mut first = ResponseBuffer::new(budget.clone());
    first.write_all(b"first").unwrap();
    sender.send(Msg::ClipboardInput(Arc::new(first))).unwrap();
    let mut state = State::default();
    assert!(machine.drain_recv_channel(&mut state));
    machine.pty.writer.remaining = Some(1);
    machine.pty_write(&mut state).unwrap();
    let mut second = ResponseBuffer::new(budget.clone());
    second.write_all(b"second").unwrap();
    sender.send(Msg::ClipboardInput(Arc::new(second))).unwrap();
    assert_eq!(budget.used(), 8192);
    machine.finish(&mut state, &mut [0; 32], Ok(ExitReason::Shutdown));
    assert_eq!(budget.used(), 0);
    assert!(!state.needs_write());
    assert!(sender
        .send(Msg::Input(Cow::Borrowed(b"after shutdown")))
        .is_err());
}

// Holding the terminal until the confirming read guarantees the first batch
// cannot be parsed before EOF/error. No timing assumptions or sleeps are needed.
fn read_with_contended_terminal(ending: Option<ErrorKind>, raw_error: Option<i32>) {
    let mut machine = machine(b"final output".to_vec(), ending);
    machine.pty.raw_error = raw_error;
    let terminal = Arc::clone(&machine.terminal);
    let guard = terminal.lock();
    let (sender, receiver) = mpsc::channel();
    machine.pty.exhausted = Some(sender);
    let worker = std::thread::spawn(move || {
        let result =
            machine.pty_read(&mut State::default(), &mut vec![0; READ_BUFFER_SIZE]);
        (machine, result)
    });
    let exhausted = receiver.recv_timeout(Duration::from_secs(5));
    drop(guard);
    let (machine, result) = worker.join().unwrap();
    exhausted.unwrap();
    if let Some(code) = raw_error {
        assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
    } else {
        match ending {
            Some(kind) => assert_eq!(result.unwrap_err().kind(), kind),
            None => assert_eq!(result.unwrap(), ReadOutcome::Closed),
        }
    }
    assert_eq!(first_line(&machine, 12), "final output");
}

#[test]
fn eof_preserves_buffered_output_under_lock_contention() {
    read_with_contended_terminal(None, None);
}

#[test]
fn read_error_preserves_buffered_output_under_lock_contention() {
    read_with_contended_terminal(Some(ErrorKind::Other), None);
}

#[test]
fn final_output_can_be_drained_across_multiple_parse_budgets() {
    // NUL padding consumes parse budgets without scrolling the final marker.
    let mut bytes = vec![0; MAX_LOCKED_READ * 2 + 1];
    bytes.extend_from_slice(b"final output");
    let mut machine = machine(bytes, None);
    let mut state = State::default();
    let mut buf = vec![0; READ_BUFFER_SIZE];
    assert_eq!(
        machine.pty_read(&mut state, &mut buf).unwrap(),
        ReadOutcome::Budget
    );
    assert_eq!(
        machine.pty_read(&mut state, &mut buf).unwrap(),
        ReadOutcome::Budget
    );
    assert_eq!(
        machine.pty_read(&mut state, &mut buf).unwrap(),
        ReadOutcome::Closed
    );
    assert_eq!(first_line(&machine, 12), "final output");
}

#[test]
fn final_drain_waits_for_a_delayed_pump() {
    let mut machine = machine(b"final output".to_vec(), Some(ErrorKind::WouldBlock));
    machine.pty.idle_reads = 2;
    machine.drain_after_exit(
        &mut State::default(),
        &mut vec![0; READ_BUFFER_SIZE],
        Duration::from_millis(25),
    );
    assert_eq!(first_line(&machine, 12), "final output");
}

#[test]
fn final_drain_stops_at_real_eof() {
    let mut machine = machine(b"final output".to_vec(), None);
    machine.drain_after_exit(
        &mut State::default(),
        &mut vec![0; READ_BUFFER_SIZE],
        Duration::from_secs(1),
    );
    assert_eq!(machine.pty.read_calls, 2);
}

#[test]
fn final_drain_renews_quiet_window_after_a_short_burst() {
    let mut machine = machine(b"final output".to_vec(), None);
    // Model a pump publication arriving after the original quiet deadline.
    // Its confirming WouldBlock must start a new quiet window for the suffix.
    machine.pty.first_read_delay = Some(Duration::from_millis(10));
    machine.pty.burst_boundary = Some(5);
    machine.drain_after_exit(
        &mut State::default(),
        &mut vec![0; READ_BUFFER_SIZE],
        Duration::from_millis(5),
    );
    assert_eq!(first_line(&machine, 12), "final output");
}

#[cfg(windows)]
#[test]
fn conpty_exit_preserves_final_output() {
    for _ in 0..5 {
        let terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            0,
        );
        let pty = teletypewriter::create_pty(
            Some("cmd.exe"),
            vec!["/d".into(), "/c".into(), "echo FINAL_OUTPUT_MARKER".into()],
            &None,
            None,
            80,
            24,
        )
        .unwrap();
        let machine = Machine::new(
            Arc::new(FairMutex::new(terminal)),
            pty,
            VoidListener,
            WindowId::from(0),
            0,
        )
        .unwrap();
        let (machine, _) = machine.spawn().join().unwrap();
        let terminal = machine.terminal.lock();
        let output: String = (0..24)
            .flat_map(|row| (0..80).map(move |column| (row, column)))
            .map(|(row, column)| terminal.grid[Line(row)][Column(column)].c())
            .collect();
        assert!(
            output.contains("FINAL_OUTPUT_MARKER"),
            "final ConPTY output was lost: {output:?}"
        );
    }
}

#[test]
fn child_exit_drains_multiple_budgets_and_finishes_pending_sync() {
    let mut bytes = vec![0; MAX_LOCKED_READ * 2 + 1];
    bytes.extend_from_slice(b"\x1b[?2026hfinal output");
    // WouldBlock models a descendant retaining the slave after the child exits.
    let mut machine = machine(bytes, Some(ErrorKind::WouldBlock));
    let (registration, readiness) = corcovado::Registration::new2();
    machine.pty.child = Some(registration);
    readiness.set_readiness(Ready::readable()).unwrap();
    let (machine, state) = machine.spawn().join().unwrap();
    assert_eq!(first_line(&machine, 12), "final output");
    assert_eq!(state.parser.sync_bytes_count(), 0);
    assert_eq!(machine.pty.shutdown_calls, 1);
    assert_eq!(machine.pty.deregister_calls, 1);
}

#[cfg(unix)]
#[test]
fn eio_preserves_buffered_output_under_lock_contention() {
    read_with_contended_terminal(None, Some(libc::EIO));
}

#[test]
fn registration_failure_shuts_down_pty() {
    let mut machine = machine(Vec::new(), None);
    machine.pty.registration_error = true;
    let (machine, _) = machine.spawn().join().unwrap();
    assert_eq!(machine.pty.shutdown_calls, 1);
    assert_eq!(machine.pty.deregister_calls, 1);
}

#[test]
fn read_failure_shuts_down_pty_and_finishes_pending_sync() {
    let mut machine =
        machine(b"\x1b[?2026hfinal output".to_vec(), Some(ErrorKind::Other));
    let (registration, readiness) = corcovado::Registration::new2();
    machine.pty.child = Some(registration);
    machine.pty.read_event = true;
    readiness.set_readiness(Ready::readable()).unwrap();
    let (machine, state) = machine.spawn().join().unwrap();
    assert_eq!(machine.pty.shutdown_calls, 1);
    assert_eq!(machine.pty.deregister_calls, 1);
    assert_eq!(first_line(&machine, 12), "final output");
    assert_eq!(state.parser.sync_bytes_count(), 0);
}

#[derive(Clone)]
struct ExitObserver {
    terminal: Arc<std::sync::Mutex<std::sync::Weak<FairMutex<Crosswords<Self>>>>>,
    observed: mpsc::Sender<String>,
}

impl EventListener for ExitObserver {
    fn send_event(&self, event: RioEvent, _: WindowId) {
        if matches!(event, RioEvent::ChildExited(..)) {
            let terminal = self.terminal.lock().unwrap().upgrade().unwrap();
            let terminal = terminal.lock();
            let text = (0..12)
                .map(|col| terminal.grid[Line(0)][Column(col)].c())
                .collect();
            self.observed.send(text).unwrap();
        }
    }
}

#[test]
fn pending_sync_is_visible_before_child_exited_notification() {
    let (observed, receiver) = mpsc::channel();
    let observer = ExitObserver {
        terminal: Arc::new(std::sync::Mutex::new(std::sync::Weak::new())),
        observed,
    };
    let terminal = Arc::new(FairMutex::new(Crosswords::new(
        CrosswordsSize::new(80, 24),
        CursorShape::Block,
        observer.clone(),
        WindowId::from(0),
        0,
        0,
    )));
    *observer.terminal.lock().unwrap() = Arc::downgrade(&terminal);
    let mut pty = machine(
        b"\x1b[?2026hfinal output".to_vec(),
        Some(ErrorKind::WouldBlock),
    )
    .pty;
    let (registration, readiness) = corcovado::Registration::new2();
    pty.child = Some(registration);
    readiness.set_readiness(Ready::readable()).unwrap();
    let machine = Machine::new(terminal, pty, observer, WindowId::from(0), 0).unwrap();
    let worker = machine.spawn();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        "final output"
    );
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn hangup_without_readable_readiness_drains_residual_output() {
    let mut machine = machine(b"final output".to_vec(), Some(ErrorKind::Other));
    let (registration, readiness) = corcovado::Registration::new2();
    machine.pty.child = Some(registration);
    machine.pty.read_event = true;
    readiness.set_readiness(UnixReady::hup().into()).unwrap();
    let shutdown = machine.channel();
    let (completed, receiver) = mpsc::channel();
    let worker = machine.spawn();
    std::thread::spawn(move || completed.send(worker.join().unwrap()).unwrap());
    let result = receiver.recv_timeout(Duration::from_secs(5));
    if result.is_err() {
        // Unblock the worker even if HUP handling regresses.
        shutdown.send(Msg::Shutdown).unwrap();
    }
    let (machine, _) = result.expect("HUP must be handled without readable readiness");
    assert_eq!(first_line(&machine, 12), "final output");
    assert_eq!(machine.pty.shutdown_calls, 1);
    assert_eq!(machine.pty.deregister_calls, 1);
}

#[test]
fn explicit_shutdown_finalizes_once() {
    let machine = machine(Vec::new(), None);
    machine.channel().send(Msg::Shutdown).unwrap();
    let (machine, _) = machine.spawn().join().unwrap();
    assert_eq!(machine.pty.shutdown_calls, 1);
    assert_eq!(machine.pty.deregister_calls, 1);
}
