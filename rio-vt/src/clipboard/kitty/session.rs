use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

#[derive(Debug)]
pub enum Request {
    Read {
        legacy: Option<u8>,
        generation: u64,
        metadata: Metadata,
        mimes: Vec<String>,
        list: bool,
        terminator: Terminator,
        _admission: Admission,
    },
    Write {
        legacy: bool,
        generation: u64,
        committed: CommittedWrite,
        terminator: Terminator,
    },
}

impl Request {
    pub fn access(
        &self,
        policy: crate::clipboard::ClipboardPolicy,
    ) -> crate::clipboard::Access {
        match self {
            Self::Read { .. } => policy.read,
            Self::Write { .. } => policy.write,
        }
    }

    fn legacy(&self) -> bool {
        match self {
            Self::Read { legacy, .. } => legacy.is_some(),
            Self::Write { legacy, .. } => *legacy,
        }
    }
    pub fn requester_name(&self) -> &str {
        &self.grant_identity().0.name
    }
    pub fn generation(&self) -> u64 {
        match self {
            Self::Read { generation, .. } | Self::Write { generation, .. } => *generation,
        }
    }
    fn grant_identity(&self) -> (&Metadata, Direction) {
        match self {
            Self::Read { metadata, .. } => (metadata, Direction::Read),
            Self::Write { committed, .. } => (&committed.metadata, Direction::Write),
        }
    }
    pub fn prompt_exempt(&self) -> bool {
        matches!(self, Self::Read { mimes, .. } if mimes.is_empty())
    }
    pub fn execute(
        &self,
        provider: &mut (impl crate::clipboard::typed::Provider + ?Sized),
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.execute_into(provider, &mut bytes).unwrap();
        bytes
    }

    pub fn execute_buffered(
        &self,
        provider: &mut (impl crate::clipboard::typed::Provider + ?Sized),
    ) -> std::io::Result<crate::clipboard::buffer::ResponseBuffer> {
        self.execute_buffered_in(provider, crate::clipboard::buffer::Budget::global())
    }

    fn execute_buffered_in(
        &self,
        provider: &mut (impl crate::clipboard::typed::Provider + ?Sized),
        budget: crate::clipboard::buffer::Budget,
    ) -> std::io::Result<crate::clipboard::buffer::ResponseBuffer> {
        use crate::clipboard::buffer::ResponseBuffer;
        let mut bytes = ResponseBuffer::new(budget);
        // Reserve the write acknowledgement before publication, so admission
        // failure cannot publish data and then incorrectly report EBUSY.
        if matches!(self, Self::Write { legacy: false, .. }) {
            bytes.reserve(1024)?;
        }
        self.execute_into(provider, &mut bytes)?;
        Ok(bytes)
    }

    pub fn status_buffered(
        &self,
        status: Status,
    ) -> std::io::Result<crate::clipboard::buffer::ResponseBuffer> {
        use crate::clipboard::buffer::{Budget, ResponseBuffer};
        use std::io::Write;
        let mut bytes = ResponseBuffer::new(Budget::global());
        bytes.write_all(&self.status(status))?;
        Ok(bytes)
    }

    fn execute_into(
        &self,
        provider: &mut (impl crate::clipboard::typed::Provider + ?Sized),
        writer: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        use crate::clipboard::typed::Error;
        let status = |error| match error {
            Error::Unsupported => Status::Unsupported,
            Error::Invalid => Status::Invalid,
            Error::Io => Status::Io,
            Error::TooLarge => Status::TooLarge,
        };
        match self {
            Self::Write { committed, .. } => {
                let result =
                    provider.write(committed.metadata.location, &committed.contents);
                writer
                    .write_all(&self.status(result.map_or_else(status, |()| Status::Ok)))
            }
            Self::Read {
                metadata,
                mimes,
                list,
                terminator,
                legacy,
                ..
            } => match provider.read(metadata.location, mimes) {
                Err(error) => writer.write_all(&self.status(status(error))),
                Ok(snapshot) => {
                    if let Some(kind) = legacy {
                        write!(writer, "\x1b]52;{};", *kind as char)?;
                        let data = snapshot
                            .contents
                            .first()
                            .map_or(&[][..], |content| content.data.as_slice());
                        // Full chunks are divisible by three, preserving one
                        // base64 stream while bounding temporary allocations.
                        for chunk in data.chunks(3 * 1024) {
                            writer.write_all(STANDARD.encode(chunk).as_bytes())?;
                        }
                        writer.write_all(match terminator {
                            Terminator::Bell => b"\x07",
                            Terminator::String => b"\x1b\\",
                        })
                    } else {
                        write_read_response(
                            writer,
                            metadata,
                            *terminator,
                            None,
                            if *list {
                                Some(&snapshot.available)
                            } else {
                                None
                            },
                            &snapshot.contents,
                        )
                    }
                }
            },
        }
    }
    pub fn status(&self, status: Status) -> Vec<u8> {
        // OSC 52 has no status packets; denial and write success stay silent.
        if self.legacy() {
            return Vec::new();
        }
        let (metadata, terminator) = match self {
            Self::Read {
                metadata,
                terminator,
                ..
            } => (metadata, *terminator),
            Self::Write {
                committed,
                terminator,
                ..
            } => (&committed.metadata, *terminator),
        };
        reply(metadata, terminator, status)
    }
}

pub enum Effect {
    Request(Request),
    Reply(Vec<u8>),
}

#[derive(Default)]
pub struct Session {
    pool: Pool,
    pending: Option<WriteTransaction>,
    generation: u64,
    grants: Grants,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("write_pending", &self.pending.is_some())
            .finish()
    }
}

fn reply(metadata: &Metadata, terminator: Terminator, status: Status) -> Vec<u8> {
    let mut bytes = Vec::new();
    Response {
        metadata,
        status,
        primary: metadata.location == ClipboardType::Selection,
        mime: None,
        password: None,
        payload: &[],
        terminator,
    }
    .encode(&mut bytes)
    .unwrap();
    bytes
}

impl Session {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn abort(&mut self, terminator: Terminator) -> Option<Vec<u8>> {
        let pending = self.pending.take()?;
        Some(reply(pending.metadata(), terminator, Status::TooLarge))
    }
    pub fn legacy_request(
        &self,
        location: ClipboardType,
        kind: u8,
        text: Option<String>,
        terminator: Terminator,
    ) -> Result<Request, WriteError> {
        let mut admission = self.pool.admit()?;
        let metadata = Metadata {
            operation: if text.is_some() {
                Operation::Write
            } else {
                Operation::Read
            },
            location,
            id: String::new(),
            mime: String::new(),
            password: String::new(),
            name: String::new(),
        };
        Ok(match text {
            Some(text) => {
                admission.reserve(text.capacity())?;
                Request::Write {
                    generation: self.generation,
                    legacy: true,
                    terminator,
                    committed: CommittedWrite {
                        _admission: admission,
                        metadata,
                        contents: vec![Content {
                            mime: "text/plain".to_owned(),
                            data: std::sync::Arc::new(text.into_bytes()),
                        }],
                    },
                }
            }
            None => Request::Read {
                _admission: admission,
                generation: self.generation,
                legacy: Some(kind),
                metadata,
                mimes: vec!["text/plain".to_owned()],
                list: false,
                terminator,
            },
        })
    }
    pub fn paste_event(
        &mut self,
        location: ClipboardType,
        available: &[String],
    ) -> std::io::Result<crate::clipboard::buffer::ResponseBuffer> {
        // Never fall back to a predictable token if secure entropy is absent.
        let mut entropy = [0u8; 24];
        #[cfg(not(target_arch = "wasm32"))]
        getrandom::fill(&mut entropy)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        #[cfg(target_arch = "wasm32")]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "clipboard paste events require secure entropy",
        ));
        #[allow(unreachable_code)]
        {
            let password = STANDARD.encode(entropy);
            let metadata = Metadata {
                operation: Operation::Read,
                location,
                id: String::new(),
                mime: String::new(),
                password: String::new(),
                name: String::new(),
            };
            let mut bytes = crate::clipboard::buffer::ResponseBuffer::new(
                crate::clipboard::buffer::Budget::global(),
            );
            write_read_response(
                &mut bytes,
                &metadata,
                Terminator::String,
                Some(&password),
                Some(available),
                &[],
            )?;
            self.grants.grant(&password, Direction::Read, true);
            Ok(bytes)
        }
    }
    pub fn current(&self, request: &Request) -> bool {
        self.generation == request.generation()
    }
    pub fn reset(&mut self) {
        self.pending = None;
        self.grants = Grants::default();
        self.generation = self
            .generation
            .checked_add(1)
            .expect("clipboard session generation exhausted");
    }
    pub fn cancel_write(&mut self) {
        self.pending = None;
    }
    pub fn granted(&mut self, request: &Request) -> bool {
        if !self.current(request) {
            return false;
        }
        if request.prompt_exempt() {
            return true;
        }
        let (metadata, direction) = request.grant_identity();
        !metadata.name.is_empty() && self.grants.consume(&metadata.password, direction)
    }
    pub fn approve(&mut self, request: &Request) -> bool {
        if !self.current(request) {
            return false;
        }
        let (metadata, direction) = request.grant_identity();
        if !metadata.name.is_empty() {
            self.grants.grant(&metadata.password, direction, false);
        }
        true
    }
    /// No OS access occurs here. Completed requests carry their own identity
    /// across the event queue; the terminal route owns this session.
    pub fn handle(
        &mut self,
        raw: &[u8],
        payload: &[u8],
        terminator: Terminator,
        policy: crate::clipboard::ClipboardPolicy,
    ) -> Option<Effect> {
        let metadata = match Metadata::parse(raw) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => return None,
            Err(_) => {
                if matches!(
                    Metadata::operation(raw),
                    Some(Operation::Data | Operation::Alias)
                ) {
                    let pending = self.pending.take()?;
                    return Some(Effect::Reply(reply(
                        pending.metadata(),
                        terminator,
                        Status::Invalid,
                    )));
                }
                return None;
            }
        };
        let denied = match metadata.operation {
            Operation::Read => policy.read == crate::clipboard::Access::Deny,
            Operation::Write => policy.write == crate::clipboard::Access::Deny,
            _ => false,
        };
        if denied {
            if metadata.operation == Operation::Write {
                self.pending = None;
            }
            return Some(Effect::Reply(reply(
                &metadata,
                terminator,
                Status::Permission,
            )));
        }
        match metadata.operation {
            Operation::Read => {
                // Keep decode scratch bounded independently of response size.
                if payload.len() > 64 * 1024 {
                    return None;
                }
                let decoded = STANDARD.decode(payload).ok()?;
                let text = std::str::from_utf8(&decoded).ok()?;
                let admission = match self.pool.admit() {
                    Ok(admission) => admission,
                    Err(_) => {
                        return Some(Effect::Reply(reply(
                            &metadata,
                            terminator,
                            Status::Busy,
                        )))
                    }
                };
                let mut list = false;
                let mut mimes = Vec::new();
                for mime in text.split_ascii_whitespace() {
                    if mime == "." {
                        list = true;
                    } else if mime.len() <= 256 && mimes.len() < 4 {
                        mimes.push(mime.to_owned());
                    }
                }
                Some(Effect::Request(Request::Read {
                    _admission: admission,
                    legacy: None,
                    generation: self.generation,
                    metadata,
                    mimes,
                    list,
                    terminator,
                }))
            }
            Operation::Write => {
                self.pending = None;
                match WriteTransaction::new_in(
                    metadata.clone(),
                    policy.write_limit_bytes,
                    &self.pool,
                ) {
                    Ok(pending) => {
                        self.pending = Some(pending);
                        None
                    }
                    Err(_) => {
                        Some(Effect::Reply(reply(&metadata, terminator, Status::Busy)))
                    }
                }
            }
            Operation::Data | Operation::Alias => {
                let mut pending = self.pending.take()?;
                let original = pending.metadata().clone();
                if metadata.operation == Operation::Data && metadata.mime.is_empty() {
                    return Some(match pending.commit() {
                        Ok(committed) => Effect::Request(Request::Write {
                            legacy: false,
                            generation: self.generation,
                            committed,
                            terminator,
                        }),
                        Err(_) => {
                            Effect::Reply(reply(&original, terminator, Status::Invalid))
                        }
                    });
                }
                let result = if metadata.operation == Operation::Data {
                    pending.data(&metadata.mime, payload)
                } else {
                    pending.alias(&metadata.mime, payload)
                };
                match result {
                    Ok(()) => {
                        self.pending = Some(pending);
                        None
                    }
                    Err(error) => Some(Effect::Reply(reply(
                        &original,
                        terminator,
                        match error {
                            WriteError::Invalid => Status::Invalid,
                            WriteError::TooLarge => Status::TooLarge,
                            WriteError::Busy => Status::Busy,
                        },
                    ))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{Access, ClipboardPolicy};
    #[test]
    fn read_request_clones_keep_admission_until_the_last_owner_drops() {
        let mut session = Session {
            pool: Pool::new(1, 8),
            ..Default::default()
        };
        let first = read(&mut session, b"Lg==");
        let shared = std::sync::Arc::new(first);
        let queued = shared.clone();
        drop(shared);
        let Some(Effect::Reply(bytes)) = session.handle(
            b"type=read:id=busy",
            b"",
            Terminator::Bell,
            ClipboardPolicy::default(),
        ) else {
            panic!("expected admission failure")
        };
        assert_eq!(bytes, b"\x1b]5522;type=read:status=EBUSY:id=busy\x07");
        drop(queued);
        assert!(matches!(
            session.handle(
                b"type=read",
                b"",
                Terminator::Bell,
                ClipboardPolicy::default()
            ),
            Some(Effect::Request(_))
        ));
    }

    #[test]
    fn replacement_releases_old_slot_before_admitting_the_new_write() {
        let mut session = Session {
            pool: Pool::new(1, 8),
            ..Default::default()
        };
        let policy = ClipboardPolicy::default();
        assert!(session
            .handle(b"type=write:id=old", b"", Terminator::Bell, policy)
            .is_none());
        assert!(session
            .handle(b"type=write:id=new", b"", Terminator::Bell, policy)
            .is_none());
        let Some(Effect::Request(request)) =
            session.handle(b"type=wdata", b"", Terminator::Bell, policy)
        else {
            panic!("expected replacement commit")
        };
        let Some(Effect::Reply(bytes)) =
            session.handle(b"type=write:id=blocked", b"", Terminator::Bell, policy)
        else {
            panic!("expected admission failure")
        };
        assert_eq!(bytes, b"\x1b]5522;type=write:status=EBUSY:id=blocked\x07");
        drop(request);
        assert!(session
            .handle(b"type=write:id=recovered", b"", Terminator::Bell, policy)
            .is_none());
    }

    #[test]
    fn byte_pressure_replies_busy_and_never_emits_a_partial_commit() {
        let mut session = Session {
            pool: Pool::new(1, 4),
            ..Default::default()
        };
        let policy = ClipboardPolicy::default();
        session.handle(b"type=write:id=full", b"", Terminator::Bell, policy);
        let Some(Effect::Reply(bytes)) = session.handle(
            b"type=wdata:mime=YQ==",
            b"SGVsbG8=",
            Terminator::Bell,
            policy,
        ) else {
            panic!("expected byte admission failure")
        };
        assert_eq!(bytes, b"\x1b]5522;type=write:status=EBUSY:id=full\x07");
        assert!(session
            .handle(b"type=wdata", b"", Terminator::Bell, policy)
            .is_none());
        assert!(session
            .handle(b"type=write:id=recovered", b"", Terminator::Bell, policy)
            .is_none());
    }

    struct DataProvider {
        data: Vec<u8>,
        writes: usize,
    }
    impl crate::clipboard::typed::Provider for DataProvider {
        fn read(
            &mut self,
            _: ClipboardType,
            _: &[String],
        ) -> Result<crate::clipboard::typed::Snapshot, crate::clipboard::typed::Error>
        {
            Ok(crate::clipboard::typed::Snapshot {
                available: vec!["text/plain".into()],
                contents: vec![Content {
                    mime: "text/plain".into(),
                    data: std::sync::Arc::new(self.data.clone()),
                }],
            })
        }
        fn write(
            &mut self,
            _: ClipboardType,
            _: &[Content],
        ) -> Result<(), crate::clipboard::typed::Error> {
            self.writes += 1;
            Ok(())
        }
    }

    #[test]
    fn osc52_streaming_encoding_preserves_base64_across_chunk_boundaries() {
        let session = Session::default();
        for len in [3071, 3072, 3073, 8193] {
            let mut provider = DataProvider {
                data: vec![b'x'; len],
                writes: 0,
            };
            let request = session
                .legacy_request(ClipboardType::Clipboard, b'c', None, Terminator::Bell)
                .unwrap();
            assert_eq!(
                request.execute(&mut provider),
                format!("\x1b]52;c;{}\x07", STANDARD.encode(&provider.data)).as_bytes()
            );
        }
    }

    #[test]
    fn response_reservation_failure_precedes_clipboard_publication() {
        use crate::clipboard::buffer::{Budget, ResponseBuffer};
        use std::io::Write;
        let budget = Budget::new(4096);
        let mut occupied = ResponseBuffer::new(budget.clone());
        occupied.write_all(b"reserved").unwrap();
        let mut session = Session::default();
        session.handle(
            b"type=write:id=w",
            b"",
            Terminator::Bell,
            ClipboardPolicy::default(),
        );
        let Some(Effect::Request(request)) = session.handle(
            b"type=wdata",
            b"",
            Terminator::Bell,
            ClipboardPolicy::default(),
        ) else {
            panic!("expected commit")
        };
        let mut provider = DataProvider {
            data: Vec::new(),
            writes: 0,
        };
        assert_eq!(
            request
                .execute_buffered_in(&mut provider, budget.clone())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(provider.writes, 0);
        drop(occupied);
        let reply = request
            .execute_buffered_in(&mut provider, budget.clone())
            .unwrap();
        assert_eq!(provider.writes, 1);
        assert_eq!(reply.as_slice(), b"\x1b]5522;type=write:status=OK:id=w\x07");
        drop(reply);
        assert_eq!(budget.used(), 0);
    }
    fn read(session: &mut Session, payload: &[u8]) -> Request {
        let Some(Effect::Request(request)) = session.handle(
            b"type=read:name=YXBw:pw=c2VjcmV0",
            payload,
            Terminator::Bell,
            ClipboardPolicy::default(),
        ) else {
            panic!("expected read")
        };
        request
    }

    #[test]
    fn approval_reuses_grant_and_reset_invalidates_queued_requests() {
        let mut session = Session::default();
        let request = read(&mut session, b"dGV4dC9wbGFpbg==");
        assert!(!session.granted(&request));
        assert!(session.approve(&request));
        let followup = read(&mut session, b"dGV4dC9wbGFpbg==");
        assert!(session.granted(&followup));
        session.reset();
        assert!(!session.current(&request));
        assert!(!session.approve(&request));
        let fresh = read(&mut session, b"dGV4dC9wbGFpbg==");
        assert!(!session.granted(&fresh));
    }

    #[test]
    fn listing_never_consumes_one_time_grant() {
        let mut session = Session::default();
        session.grants.grant("secret", Direction::Read, true);
        let listing = read(&mut session, b"Lg==");
        assert!(session.granted(&listing));
        let data = read(&mut session, b"dGV4dC9wbGFpbg==");
        assert!(session.granted(&data));
        assert!(!session.granted(&data));
    }
    #[test]
    fn denied_write_replaces_pending_and_ignores_later_data() {
        let mut session = Session::default();
        let policy = ClipboardPolicy::default();
        session.handle(b"type=write:id=old", b"", Terminator::Bell, policy);
        let denied = ClipboardPolicy {
            write: Access::Deny,
            ..policy
        };
        let Some(Effect::Reply(bytes)) =
            session.handle(b"type=write:id=new", b"", Terminator::Bell, denied)
        else {
            panic!("expected denial")
        };
        assert_eq!(bytes, b"\x1b]5522;type=write:status=EPERM:id=new\x07");
        assert!(session
            .handle(b"type=wdata", b"", Terminator::Bell, policy)
            .is_none());
    }

    #[test]
    fn zero_limit_rejects_data_and_limit_is_captured_at_begin() {
        let mut session = Session::default();
        let zero = ClipboardPolicy {
            write_limit_bytes: 0,
            ..Default::default()
        };
        session.handle(b"type=write:id=limit", b"", Terminator::String, zero);
        let Some(Effect::Reply(bytes)) = session.handle(
            b"type=wdata:mime=YQ==",
            b"YQ==",
            Terminator::String,
            ClipboardPolicy::default(),
        ) else {
            panic!("expected limit failure")
        };
        assert_eq!(bytes, b"\x1b]5522;type=write:status=EFBIG:id=limit\x1b\\");
    }

    #[test]
    fn denied_read_returns_permission_error() {
        let mut session = Session::default();
        let policy = ClipboardPolicy {
            read: Access::Deny,
            ..Default::default()
        };
        let Some(Effect::Reply(bytes)) =
            session.handle(b"type=read:id=r", b"", Terminator::Bell, policy)
        else {
            panic!("expected denial")
        };
        assert_eq!(bytes, b"\x1b]5522;type=read:status=EPERM:id=r\x07");
    }
}
